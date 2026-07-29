use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Query, State, WebSocketUpgrade};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::select;
use tokio::sync::OwnedSemaphorePermit;
use tokio::time::sleep;
use tracing::{info, warn};

use crate::gamepad::types::{GamepadState, RumbleEvent};
use crate::gamepad::ControllerId;
use crate::protocol::{ClientMessage, ServerMessage};

use super::server::AppState;

/// WebSocket 升级查询参数
#[derive(Deserialize, Default)]
pub struct WsQuery {
    pub token: Option<String>,
}

/// 页面请求查询参数
#[derive(Deserialize)]
#[allow(dead_code)]
pub struct IndexQuery {
    token: Option<String>,
}

/// 首页 - 提供虚拟手柄页面
pub async fn index(
    Query(_query): Query<IndexQuery>,
    State(_state): State<Arc<AppState>>,
) -> impl IntoResponse {
    let html = index_html();
    (StatusCode::OK, [("Content-Type", "text/html; charset=utf-8")], html)
}

/// 最大 WebSocket 消息大小（64 KB）
///
/// 游戏手柄状态消息通常只有几十字节，认证消息也很小。
/// 限制消息大小可以防止恶意用户发送超大消息占用内存。
const MAX_WS_MESSAGE_SIZE: usize = 64 * 1024;

/// WebSocket 升级端点
///
/// 安全防护：
/// 1. **Origin 验证**：防止跨站 WebSocket 劫持（CSWSH）
/// 2. **总连接上限**：所有 WebSocket 连接共享一个 `Semaphore`，超过 `max_connections` 拒绝
/// 3. **未认证连接上限**：未通过预认证的连接共享另一个 `Semaphore`，超过 `max_unauth_connections` 拒绝
///
/// 任一限制触发时返回 403 或 503，不进入 `handle_socket`，不消耗服务器资源。
pub async fn ws_upgrade(
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    Query(query): Query<WsQuery>,
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    // Origin 头验证（防止跨站 WebSocket 劫持）
    if let Err(status) = validate_origin(&headers, &state.config) {
        warn!("Rejected WebSocket upgrade: invalid Origin header");
        return (status, "").into_response();
    }

    // 预认证检查：如果提供了有效 token，保留 token 以便会话清理
    let pre_auth_token = match &query.token {
        Some(token) if state.auth.validate_token(token).await => Some(token.clone()),
        _ => None,
    };

    // IP 级别的退避检查（仅对未认证连接）
    // 如果 IP 已超过最大失败次数，直接拒绝连接，防止占用未认证槽位
    if pre_auth_token.is_none() && state.ip_backoff.should_disconnect(addr.ip()) {
        warn!(
            "Rejected WebSocket upgrade: IP {} has too many auth failures",
            addr.ip()
        );
        return (StatusCode::FORBIDDEN, "").into_response();
    }

    match acquire_connection_permits(&state, &query, pre_auth_token).await {
        Ok((total_permit, unauth_permit, pre_auth_token)) => {
            ws.max_message_size(MAX_WS_MESSAGE_SIZE)
                .on_upgrade(move |socket| {
                    handle_socket(socket, state, pre_auth_token, total_permit, unauth_permit, addr)
                })
                .into_response()
        }
        Err(status) => (status, "").into_response(),
    }
}

/// 验证 WebSocket 升级请求的 Origin 头
///
/// 验证规则：
/// - 无 Origin 头：允许（非浏览器客户端，如原生应用）
/// - Origin 与 Host 匹配：允许（同源请求）
/// - 其他情况：拒绝（防止跨站 WebSocket 劫持）
fn validate_origin(headers: &HeaderMap, _config: &crate::config::Config) -> Result<(), StatusCode> {
    let origin = match headers.get("origin") {
        Some(o) => o,
        None => return Ok(()), // 无 Origin 头，允许（非浏览器客户端）
    };

    let origin_str = match origin.to_str() {
        Ok(s) => s,
        Err(_) => return Err(StatusCode::FORBIDDEN),
    };

    // 从 Origin 中提取 host 部分
    let origin_host = origin_str
        .strip_prefix("http://")
        .or_else(|| origin_str.strip_prefix("https://"))
        .unwrap_or(origin_str);

    // 获取 Host 头
    let host_header = match headers.get("host") {
        Some(h) => match h.to_str() {
            Ok(s) => s,
            Err(_) => return Err(StatusCode::FORBIDDEN),
        },
        None => return Err(StatusCode::FORBIDDEN),
    };

    // Origin host 应与 Host 头匹配（同源）
    if origin_host == host_header {
        Ok(())
    } else {
        // 也允许 localhost 和 127.0.0.1 之间的等价
        let origin_hostname = origin_host.split(':').next().unwrap_or("");
        let host_hostname = host_header.split(':').next().unwrap_or("");
        let is_local_origin = matches!(origin_hostname, "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]");
        let is_local_host = matches!(host_hostname, "localhost" | "127.0.0.1" | "0.0.0.0" | "[::1]");
        if is_local_origin && is_local_host {
            Ok(())
        } else {
            Err(StatusCode::FORBIDDEN)
        }
    }
}

/// 获取连接所需的 permit
///
/// 返回 `(total_permit, unauth_permit, pre_auth_token)`：
/// - `total_permit`：总连接槽位，必有
/// - `unauth_permit`：未认证槽位，仅未通过预认证时存在（认证成功后释放）
/// - `pre_auth_token`：预认证 token，仅当 query.token 有效时存在
///
/// 失败时返回 503 状态码。提取成独立函数以便单元测试覆盖限制逻辑，
/// 不依赖 WebSocket 握手。
async fn acquire_connection_permits(
    state: &Arc<AppState>,
    _query: &WsQuery,
    pre_auth_token: Option<String>,
) -> Result<(OwnedSemaphorePermit, Option<OwnedSemaphorePermit>, Option<String>), StatusCode> {
    // 门槛 1：总连接数限制
    let total_permit = match state.connection_limiter.try_acquire_total() {
        Some(p) => p,
        None => {
            warn!(
                "Rejected WebSocket upgrade: total connection limit reached ({})",
                state.connection_limiter.total_used()
            );
            return Err(StatusCode::SERVICE_UNAVAILABLE);
        }
    };

    // 门槛 2：未认证连接数限制（仅未通过预认证的连接需要此 permit）
    let unauth_permit = if pre_auth_token.is_none() {
        match state.connection_limiter.try_acquire_unauth() {
            Some(p) => Some(p),
            None => {
                drop(total_permit); // 释放已占用的总连接 permit
                warn!(
                    "Rejected WebSocket upgrade: unauth connection limit reached ({})",
                    state.connection_limiter.unauth_used()
                );
                return Err(StatusCode::SERVICE_UNAVAILABLE);
            }
        }
    } else {
        None
    };

    Ok((total_permit, unauth_permit, pre_auth_token))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::{AuthManager, IpBackoffManager};
    use crate::config::Config;
    use crate::upnp::PortMapper;
    use crate::web::ConnectionLimiter;
    use crate::web::GamepadHandle;

    /// 构建一个 AppState，使用自定义 ConnectionLimiter
    async fn build_state(max_total: usize, max_unauth: usize) -> Arc<AppState> {
        let config = Arc::new(Config::default());
        let auth = Arc::new(AuthManager::new(crate::password::hash_password("pwd")));
        let upnp = Arc::new(PortMapper::new(0).await);
        let gamepad = GamepadHandle::new(None);
        let connection_limiter = Arc::new(ConnectionLimiter::new(max_total, max_unauth));
        let ip_backoff = Arc::new(IpBackoffManager::new(500, 10000, 5));
        Arc::new(AppState {
            auth,
            gamepad,
            upnp,
            config,
            connection_limiter,
            ip_backoff,
        })
    }

    #[tokio::test]
    async fn permits_granted_when_under_limits() {
        let state = build_state(2, 2).await;
        let query = WsQuery::default();
        let result = acquire_connection_permits(&state, &query, None).await;
        assert!(result.is_ok());
        let (total, unauth, token) = result.unwrap();
        assert!(token.is_none()); // 无 token
        assert!(unauth.is_some()); // 占用未认证槽位
        assert_eq!(state.connection_limiter.total_used(), 1);
        assert_eq!(state.connection_limiter.unauth_used(), 1);
        drop(total);
        drop(unauth);
        assert_eq!(state.connection_limiter.total_used(), 0);
        assert_eq!(state.connection_limiter.unauth_used(), 0);
    }

    #[tokio::test]
    async fn total_limit_reached_returns_503() {
        let state = build_state(1, 5).await;
        // 占用唯一的 total permit
        let occupied = state.connection_limiter.try_acquire_total().unwrap();
        let query = WsQuery::default();
        let result = acquire_connection_permits(&state, &query, None).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), StatusCode::SERVICE_UNAVAILABLE);
        drop(occupied);
    }

    #[tokio::test]
    async fn unauth_limit_reached_returns_503() {
        let state = build_state(5, 1).await;
        // 占用唯一的 unauth permit
        let occupied_unauth = state.connection_limiter.try_acquire_unauth().unwrap();
        let query = WsQuery::default(); // 无 token -> 走未认证路径
        let result = acquire_connection_permits(&state, &query, None).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), StatusCode::SERVICE_UNAVAILABLE);
        // 503 时 total permit 应已被释放，不影响后续连接
        assert_eq!(state.connection_limiter.total_used(), 0);
        drop(occupied_unauth);
    }

    #[tokio::test]
    async fn valid_token_bypasses_unauth_limit() {
        let state = build_state(5, 1).await;
        // 先创建有效 token
        let token = state.auth.create_session().await;
        // 占满 unauth
        let _occupied_unauth = state.connection_limiter.try_acquire_unauth().unwrap();
        // 带 token 的请求应成功（走预认证路径，不消耗 unauth permit）
        let query = WsQuery { token: Some(token.clone()) };
        let result = acquire_connection_permits(&state, &query, Some(token)).await;
        assert!(result.is_ok());
        let (_total, unauth, token) = result.unwrap();
        assert!(token.is_some()); // 预认证成功，保留 token
        assert!(unauth.is_none()); // 不占用未认证槽位
        assert_eq!(state.connection_limiter.unauth_used(), 1); // 仍是手动占用的那个
    }

    #[tokio::test]
    async fn invalid_token_falls_back_to_unauth_path() {
        let state = build_state(5, 1).await;
        // 占满 unauth
        let _occupied_unauth = state.connection_limiter.try_acquire_unauth().unwrap();
        // 无效 token 应回退到未认证路径，触发 503
        let query = WsQuery { token: Some("invalid".to_string()) };
        let result = acquire_connection_permits(&state, &query, None).await;
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), StatusCode::SERVICE_UNAVAILABLE);
    }
}

/// WebSocket 消息处理循环
///
/// `_total_permit` 持有到函数结束（连接断开），代表一个总连接槽位。
/// `unauth_permit` 在认证成功时通过 `take()` 释放，未认证断开时随函数结束释放。
async fn handle_socket(
    socket: axum::extract::ws::WebSocket,
    state: Arc<AppState>,
    pre_auth_token: Option<String>,
    _total_permit: OwnedSemaphorePermit,
    mut unauth_permit: Option<OwnedSemaphorePermit>,
    client_addr: SocketAddr,
) {
    let (mut sender, mut receiver) = socket.split();

    let heartbeat_timeout = state.config.heartbeat_timeout();
    let client_ip = client_addr.ip();

    // 认证状态：预认证客户端直接标记为已认证
    let mut authenticated = pre_auth_token.is_some();

    // 当前会话 token（用于断开时从 AuthManager 清理，避免 sessions 集合无限增长）
    let mut session_token: Option<String> = pre_auth_token;

    // 认证成功后才创建控制器
    let mut controller_id: Option<ControllerId> = None;
    let mut rumble_rx: Option<tokio::sync::mpsc::Receiver<RumbleEvent>> = None;
    if authenticated {
        // 预认证连接已直接占槽，无需 unauth permit
        unauth_permit.take();
        if let Some((cid, rx)) = create_controller(&state).await {
            controller_id = Some(cid);
            rumble_rx = Some(rx);
        }
    }

    // GamepadState 速率限制相关
    let mut last_gamepad_update = tokio::time::Instant::now();
    let gamepad_min_interval = std::time::Duration::from_millis(8); // 约 125 Hz 上限

    // 发送连接成功消息
    let connected_msg = ServerMessage::Connected {
        controller_name: "WebPad Virtual Controller".to_string(),
    };
    if let Ok(text) = serde_json::to_string(&connected_msg) {
        let _ = sender.send(axum::extract::ws::Message::Text(text.into())).await;
    }

    // 预认证客户端（query token 有效）直接发送 AuthSuccess
    if authenticated {
        let success_msg = ServerMessage::AuthSuccess { token: String::new() };
        if let Ok(text) = serde_json::to_string(&success_msg) {
            let _ = sender.send(axum::extract::ws::Message::Text(text.into())).await;
        }
    }

    // 消息处理循环
    loop {
        select! {
            msg = receiver.next() => {
                match msg {
                    Some(Ok(axum::extract::ws::Message::Text(text))) => {
                        let client_msg: ClientMessage = match serde_json::from_str(&text) {
                            Ok(msg) => msg,
                            Err(e) => {
                                warn!("Failed to parse message: {}", e);
                                continue;
                            }
                        };

                        match client_msg {
                            ClientMessage::AuthRequest { password } => {
                                let was_authenticated = authenticated;
                                let should_disconnect = handle_auth_request(
                                    &mut sender,
                                    &state,
                                    &password,
                                    &mut authenticated,
                                    &mut session_token,
                                    client_ip,
                                ).await;
                                if should_disconnect {
                                    warn!("Too many auth failures from {}, disconnecting", client_ip);
                                    let _ = sender.send(axum::extract::ws::Message::Close(None)).await;
                                    break;
                                }
                                // 认证成功后创建控制器，并释放未认证连接 permit
                                if !was_authenticated && authenticated && controller_id.is_none() {
                                    unauth_permit.take();
                                    if let Some((cid, rx)) = create_controller(&state).await {
                                        controller_id = Some(cid);
                                        rumble_rx = Some(rx);
                                    }
                                }
                            }
                            ClientMessage::GamepadState { .. } => {
                                if !authenticated {
                                    warn!("GamepadState from unauthenticated client");
                                    continue;
                                }
                                // 速率限制：避免过高频率更新导致 CPU 过高
                                let now = tokio::time::Instant::now();
                                if now.duration_since(last_gamepad_update) < gamepad_min_interval {
                                    continue;
                                }
                                last_gamepad_update = now;
                                if let Some(cid) = controller_id {
                                    update_gamepad_state(&state, cid, &client_msg).await;
                                }
                            }
                            ClientMessage::Heartbeat => {
                                if !authenticated {
                                    continue;
                                }
                                let pong = ServerMessage::Pong;
                                if let Ok(text) = serde_json::to_string(&pong) {
                                    let _ = sender.send(axum::extract::ws::Message::Text(text.into())).await;
                                }
                            }
                            ClientMessage::Disconnect { reason } => {
                                info!("Client disconnected: {}", reason);
                                break;
                            }
                        }
                    }
                    Some(Ok(axum::extract::ws::Message::Close(_))) => {
                        info!("WebSocket closed by client");
                        break;
                    }
                    Some(Err(e)) => {
                        warn!("WebSocket error: {}", e);
                        break;
                    }
                    None => break,
                    _ => {}
                }
            }
            rumble = async {
                match rumble_rx.as_mut() {
                    Some(rx) => rx.recv().await,
                    None => std::future::pending().await,
                }
            } => {
                match rumble {
                    Some(event) => {
                        // large_motor -> left_motor, small_motor -> right_motor（XInput 约定）
                        let msg = ServerMessage::Rumble {
                            left_motor: event.large_motor,
                            right_motor: event.small_motor,
                        };
                        if let Ok(text) = serde_json::to_string(&msg) {
                            let _ = sender
                                .send(axum::extract::ws::Message::Text(text.into()))
                                .await;
                        }
                    }
                    None => {
                        // 通道关闭（控制器已销毁），停止轮询避免忙等
                        rumble_rx = None;
                    }
                }
            }
            _ = sleep(heartbeat_timeout) => {
                warn!("Heartbeat timeout, disconnecting");
                let _ = sender.send(axum::extract::ws::Message::Close(None)).await;
                break;
            }
        }
    }

    // 销毁控制器
    if let Some(id) = controller_id {
        destroy_controller(&state, Some(id)).await;
    }

    // 清理会话 token，避免 sessions 集合无限增长
    if let Some(token) = session_token {
        state.auth.remove_session(&token).await;
    }
}

/// 创建虚拟手柄控制器
///
/// 返回 (ControllerId, 震动事件接收端)。震动事件由 GamepadManager 从 ViGEmBus
/// 驱动通知捕获，通过 mpsc 通道传递给此处的调用方。
async fn create_controller(
    state: &Arc<AppState>,
) -> Option<(ControllerId, tokio::sync::mpsc::Receiver<RumbleEvent>)> {
    #[cfg(windows)]
    {
        if let Some(ref manager) = state.gamepad.inner {
            match manager.create_controller().await {
                Ok((id, rumble_rx)) => {
                    info!("Created controller {}", id);
                    return Some((id, rumble_rx));
                }
                Err(e) => {
                    warn!("Failed to create controller: {}", e);
                    return None;
                }
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = state;
    }
    None
}

/// 更新手柄状态
async fn update_gamepad_state(state: &Arc<AppState>, controller_id: ControllerId, msg: &ClientMessage) {
    let gamepad_state = match GamepadState::from_client_message(msg) {
        Some(s) => s,
        None => return,
    };

    #[cfg(windows)]
    {
        if let Some(ref manager) = state.gamepad.inner {
            if let Err(e) = manager.update_state(controller_id, &gamepad_state).await {
                warn!("Failed to update controller {}: {}", controller_id, e);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (controller_id, gamepad_state);
    }
}

/// 销毁虚拟手柄控制器
async fn destroy_controller(state: &Arc<AppState>, controller_id: Option<ControllerId>) {
    if let Some(id) = controller_id {
        #[cfg(windows)]
        {
            if let Some(ref manager) = state.gamepad.inner {
                info!("Destroying controller {}", id);
                manager.destroy_controller(id).await;
            }
        }
        #[cfg(not(windows))]
        {
            let _ = id;
        }
    }
}

/// 处理认证请求
///
/// 使用 IP 级别的退避机制，而不是连接级别。这样可以防止攻击者通过建立大量连接
/// 来绕过退避，占用所有未认证连接槽位。
///
/// 返回 true 表示应该断开连接（失败次数超限）
async fn handle_auth_request(
    sender: &mut futures_util::stream::SplitSink<
        axum::extract::ws::WebSocket,
        axum::extract::ws::Message,
    >,
    state: &Arc<AppState>,
    password: &str,
    authenticated: &mut bool,
    session_token: &mut Option<String>,
    client_ip: std::net::IpAddr,
) -> bool {
    if state.auth.verify_password(password) {
        // 认证成功，重置该 IP 的失败计数
        state.ip_backoff.reset_ip(client_ip);

        // 客户端重新认证时，先清理旧 session 避免泄漏
        if let Some(old) = session_token.take() {
            state.auth.remove_session(&old).await;
        }
        let token = state.auth.create_session().await;
        *authenticated = true;
        *session_token = Some(token.clone());
        let msg = ServerMessage::AuthSuccess { token };
        if let Ok(text) = serde_json::to_string(&msg) {
            let _ = sender.send(axum::extract::ws::Message::Text(text.into())).await;
        }
        false
    } else {
        // 记录 IP 级别的失败并获取退避时间
        let backoff = state.ip_backoff.record_failure(client_ip);
        let backoff_ms = backoff.as_millis() as u64;

        // 检查是否超过最大失败次数
        if state.ip_backoff.should_disconnect(client_ip) {
            let msg = ServerMessage::AuthFailure {
                reason: format!("Too many failed attempts. Connection closed."),
            };
            if let Ok(text) = serde_json::to_string(&msg) {
                let _ = sender.send(axum::extract::ws::Message::Text(text.into())).await;
            }
            // 退避等待在断开连接前不执行（直接断开，释放槽位）
            return true;
        }

        let reason = format!("Wrong password. Wait {}ms before retry.", backoff_ms);

        let msg = ServerMessage::AuthFailure { reason };
        if let Ok(text) = serde_json::to_string(&msg) {
            let _ = sender.send(axum::extract::ws::Message::Text(text.into())).await;
        }

        // 应用退避延迟
        // 注意：退避在连接内部执行，但由于是 IP 级别，同一 IP 的其他连接也会被阻止
        // 这里不释放未认证槽位，因为连接本身仍然存在
        // 但攻击者无法通过新建更多连接来绕过退避（新连接会立即被 IP 退避阻止）
        if backoff_ms > 0 {
            sleep(backoff).await;
        }

        false
    }
}

/// 生成首页 HTML
fn index_html() -> String {
    r##"<!DOCTYPE html>
<html lang="zh-CN">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0, maximum-scale=1.0, user-scalable=no, viewport-fit=cover">
    <title>WebPad - Virtual Gamepad</title>
    <style>
        * { margin: 0; padding: 0; box-sizing: border-box; -webkit-tap-highlight-color: transparent; }
        html, body {
            width: 100%; height: 100%;
            font-family: -apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif;
            background: #1a1a2e;
            color: #fff;
            overflow: hidden;
            touch-action: none;
            user-select: none;
            -webkit-user-select: none;
        }
        .container {
            width: 100vw; height: 100vh;
            display: flex;
            flex-direction: column;
            position: fixed;
            top: 0; left: 0;
            transform-origin: top left;
            overflow: hidden;
        }
        #authOverlay {
            position: fixed;
            top: 0; left: 0;
            width: 100vw; height: 100vh;
            z-index: 1000;
            transform-origin: top left;
            background: rgba(0,0,0,0.9);
            display: flex;
            align-items: center;
            justify-content: center;
        }
        .status-bar {
            height: 22px;
            display: flex;
            justify-content: space-between;
            align-items: center;
            padding: 0 8px;
            font-size: 10px;
            color: rgba(255,255,255,0.5);
            flex-shrink: 0;
        }
        .status-dot {
            display: inline-block;
            width: 6px; height: 6px;
            border-radius: 50%;
            margin-right: 4px;
        }
        .status-dot.connected { background: #4ade80; box-shadow: 0 0 4px #4ade80; }
        .status-dot.disconnected { background: #f87171; }

        .gamepad {
            width: 100%;
            flex: 1;
            min-height: 0;
            position: relative;
        }
        .controls { display: none; position: absolute; }
        .controls.visible { display: block; }

        /* === 肩键 === */
        .shoulder-btn {
            position: absolute;
            border-radius: 6px;
            border: 2px solid rgba(255,255,255,0.15);
            background: rgba(255,255,255,0.06);
            color: rgba(255,255,255,0.7);
            font-size: 11px;
            font-weight: bold;
            display: flex;
            align-items: center;
            justify-content: center;
            touch-action: none;
        }
        .shoulder-btn:active, .shoulder-btn.pressed { background: rgba(255,255,255,0.2); }
        .shoulder-btn.lb-btn {
            left: 11.7%; top: 37px;
            width: 100px; height: 35px;
        }
        .shoulder-btn.rb-btn {
            right: 11.7%; top: 35px;
            width: 100px; height: 35px;
        }

        /* === 扳机 === */
        .trigger-bar {
            position: absolute;
            border-radius: 6px;
            border: 2px solid rgba(255,255,255,0.12);
            overflow: hidden;
            background: rgba(255,255,255,0.03);
            touch-action: none;
        }
        .trigger-bar.left-trigger {
            left: 32.9%; top: 100px;
            width: 45px; height: 180px;
        }
        .trigger-bar.right-trigger {
            right: 32.9%; top: 100px;
            width: 45px; height: 180px;
        }
        .trigger-bar .trigger-fill {
            position: absolute; top: 0; width: 100%;
            background: rgba(96,165,250,0.5);
            transition: height 0.05s;
        }
        .trigger-bar .trigger-label {
            position: absolute; top: 50%; left: 50%;
            transform: translate(-50%, -50%);
            font-size: 11px;
            font-weight: bold;
            color: rgba(255,255,255,0.5);
            pointer-events: none;
            text-shadow: 0 1px 2px rgba(0,0,0,0.5);
        }

        /* === 摇杆 === */
        .thumbstick {
            position: absolute;
            border-radius: 50%;
            border: 2px solid rgba(255,255,255,0.15);
            background: rgba(255,255,255,0.04);
            touch-action: none;
        }
        .thumbstick.left-stick {
            left: 10.6%; top: 104px;
            width: 117px; height: 117px;
        }
        .thumbstick.right-stick {
            right: 10.6%; bottom: 30px;
            width: 117px; height: 117px;
        }
        .thumbstick-inner {
            width: 30px; height: 30px;
            border-radius: 50%;
            background: rgba(255,255,255,0.12);
            position: absolute;
            top: 50%; left: 50%;
            transform: translate(-50%, -50%);
            transition: transform 0.05s;
            pointer-events: none;
        }

        /* === 十字键 === */
        .dpad {
            position: absolute;
            left: 8.6%; bottom: 25px;
            width: 145px; height: 135px;
            touch-action: none;
        }
        .dpad-center {
            position: absolute; top: 50%; left: 50%;
            transform: translate(-50%, -50%);
            width: 48px; height: 45px;
            background: rgba(255,255,255,0.04);
            border-radius: 4px;
        }
        .dpad-btn {
            position: absolute;
            background: rgba(255,255,255,0.06);
            border: 2px solid rgba(255,255,255,0.12);
            color: rgba(255,255,255,0.5);
            display: flex; align-items: center; justify-content: center;
            font-size: 14px;
            touch-action: none;
        }
        .dpad-btn:active, .dpad-btn.pressed { background: rgba(255,255,255,0.2); }
        .dpad-btn.up    { top: 0; left: 50%; transform: translateX(-50%); width: 48px; height: 51px; border-radius: 8px 8px 0 0; }
        .dpad-btn.down  { bottom: 0; left: 50%; transform: translateX(-50%); width: 48px; height: 51px; border-radius: 0 0 8px 8px; }
        .dpad-btn.left  { left: 0; top: 50%; transform: translateY(-50%); width: 55px; height: 45px; border-radius: 8px 0 0 8px; }
        .dpad-btn.right { right: 0; top: 50%; transform: translateY(-50%); width: 55px; height: 45px; border-radius: 0 8px 8px 0; }

        /* === ABXY 按钮 === */
        .abxy-btn {
            position: absolute;
            width: 40px; height: 40px;
            border-radius: 50%;
            border: 2px solid rgba(255,255,255,0.15);
            font-size: 14px; font-weight: bold;
            display: flex; align-items: center; justify-content: center;
            touch-action: none;
        }
        .abxy-btn:active, .abxy-btn.pressed { opacity: 0.6; }
        .abxy-btn.y { right: 17.1%; top: 95px; background: rgba(251,191,36,0.25); border-color: #fbbf24; color: #fbbf24; }
        .abxy-btn.a { right: 17.1%; top: 180px; background: rgba(74,222,128,0.25); border-color: #4ade80; color: #4ade80; }
        .abxy-btn.x { right: 22.9%; top: 140px; background: rgba(96,165,250,0.25); border-color: #60a5fa; color: #60a5fa; }
        .abxy-btn.b { right: 11.4%; top: 140px; background: rgba(248,113,113,0.25); border-color: #f87171; color: #f87171; }

        /* === Back / Start === */
        .ss-btn {
            position: absolute;
            padding: 4px 12px;
            border-radius: 4px;
            border: 1px solid rgba(255,255,255,0.15);
            font-size: 9px;
            color: rgba(255,255,255,0.6);
            background: transparent;
            touch-action: none;
            width: 60px;
            height: 22px;
            display: flex;
            align-items: center;
            justify-content: center;
        }
        .ss-btn:active, .ss-btn.pressed { background: rgba(255,255,255,0.12); }
        .ss-btn.back-btn {
            left: 37.1%; top: 300px;
        }
        .ss-btn.start-btn {
            right: 37.1%; top: 300px;
        }

        /* === 响应式缩放 === */
        .controls {
            transform-origin: top left;
        }

        /* === 设置面板 === */
        #settingsPanel {
            display: none;
            position: fixed; top: 0; left: 0;
            width: 100vw; height: 100vh;
            z-index: 600;
            background: rgba(0,0,0,0.85);
            align-items: center; justify-content: center;
        }
        #settingsPanel.active { display: flex; }
        .layout-item {
            display: flex; align-items: center; justify-content: space-between;
            padding: 6px 8px; border-radius: 6px; margin-bottom: 4px;
            background: rgba(255,255,255,0.04); font-size: 13px;
        }
        .layout-item.active {
            background: rgba(96,165,250,0.2);
            border: 1px solid rgba(96,165,250,0.4);
        }
        .layout-item button {
            background: none; border: none; color: rgba(255,255,255,0.6);
            cursor: pointer; font-size: 12px; padding: 2px 6px;
        }
        .layout-item button:hover { color: #fff; }
        body.edit-mode .shoulder-btn,
        body.edit-mode .trigger-bar,
        body.edit-mode .thumbstick,
        body.edit-mode .dpad,
        body.edit-mode .abxy-btn,
        body.edit-mode .ss-btn {
            cursor: move !important;
        }
        .edit-selected {
            outline: 2px solid #fbbf24 !important;
            z-index: 100 !important;
        }
        .edit-resize-handle {
            position: absolute;
            width: 18px; height: 18px;
            background: #fbbf24;
            border: 2px solid #fff;
            border-radius: 3px;
            z-index: 200;
            touch-action: none;
        }
        .edit-resize-handle.nw { top: -9px; left: -9px; cursor: nw-resize; }
        .edit-resize-handle.ne { top: -9px; right: -9px; cursor: ne-resize; }
        .edit-resize-handle.sw { bottom: -9px; left: -9px; cursor: sw-resize; }
        .edit-resize-handle.se { bottom: -9px; right: -9px; cursor: se-resize; }
        .color-swatch {
            width: 100%; aspect-ratio: 1;
            border-radius: 4px; cursor: pointer;
            border: 2px solid transparent;
            transition: border-color 0.15s;
        }
        .color-swatch.active { border-color: #fff; }
        .color-swatch:hover { border-color: rgba(255,255,255,0.5); }
    </style>
</head>
<body>
    <div class="container">
        <div class="status-bar">
            <span><span class="status-dot disconnected" id="statusDot"></span><span id="statusText">Connecting...</span></span>
            <span id="fps"></span>
        </div>
        <div class="gamepad" id="gamepad">
            <div class="controls" id="controls">
                <!-- LB 肩键 -->
                <div class="shoulder-btn lb-btn" data-btn="lb" data-layout-id="lb">LB</div>
                <!-- RB 肩键 -->
                <div class="shoulder-btn rb-btn" data-btn="rb" data-layout-id="rb">RB</div>
                <!-- LT 扳机 -->
                <div class="trigger-bar left-trigger" id="leftTrigger" data-layout-id="lt">
                    <span class="trigger-label">LT</span>
                    <div class="trigger-fill" style="height:0%"></div>
                </div>
                <!-- RT 扳机 -->
                <div class="trigger-bar right-trigger" id="rightTrigger" data-layout-id="rt">
                    <span class="trigger-label">RT</span>
                    <div class="trigger-fill" style="height:0%"></div>
                </div>
                <!-- 左摇杆 -->
                <div class="thumbstick left-stick" id="leftStick" data-layout-id="leftStick">
                    <div class="thumbstick-inner" id="leftStickInner"></div>
                </div>
                <!-- 右摇杆 -->
                <div class="thumbstick right-stick" id="rightStick" data-layout-id="rightStick">
                    <div class="thumbstick-inner" id="rightStickInner"></div>
                </div>
                <!-- 十字键 -->
                <div class="dpad" id="dpad" data-layout-id="dpad">
                    <div class="dpad-center"></div>
                    <div class="dpad-btn up" data-btn="up">&#9650;</div>
                    <div class="dpad-btn down" data-btn="down">&#9660;</div>
                    <div class="dpad-btn left" data-btn="left">&#9664;</div>
                    <div class="dpad-btn right" data-btn="right">&#9654;</div>
                </div>
                <!-- ABXY 按钮 -->
                <div class="abxy-btn y" data-btn="y" data-layout-id="y">Y</div>
                <div class="abxy-btn a" data-btn="a" data-layout-id="a">A</div>
                <div class="abxy-btn x" data-btn="x" data-layout-id="x">X</div>
                <div class="abxy-btn b" data-btn="b" data-layout-id="b">B</div>
                <!-- Back / Start -->
                <div class="ss-btn back-btn" data-btn="select" data-layout-id="back">BACK</div>
                <div class="ss-btn start-btn" data-btn="start" data-layout-id="start">START</div>
            </div>
        </div>
    </div>
    <button id="settingsBtn" style="position:fixed;top:28px;right:8px;z-index:500;width:32px;height:32px;border:none;border-radius:50%;background:rgba(255,255,255,0.1);color:#fff;font-size:16px;cursor:pointer;display:flex;align-items:center;justify-content:center;">&#9881;</button>
    <div id="settingsPanel">
      <div style="background:#1a1a2e;border:1px solid rgba(255,255,255,0.15);border-radius:16px;padding:20px;width:300px;max-height:80vh;overflow-y:auto;">
        <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:12px;">
          <h2 style="font-size:16px;">设置</h2>
          <button id="closeSettings" style="background:none;border:none;color:#fff;font-size:18px;cursor:pointer;">&times;</button>
        </div>
        <div style="margin-bottom:12px;">
          <label style="display:block;font-size:12px;color:rgba(255,255,255,0.6);margin-bottom:4px;">背景颜色</label>
          <div id="bgColorPicker" style="display:flex;flex-direction:column;gap:6px;">
            <div id="colorPalette" style="display:grid;grid-template-columns:repeat(8,1fr);gap:3px;"></div>
            <div style="display:flex;align-items:center;gap:4px;">
              <span style="font-size:11px;color:rgba(255,255,255,0.5);">#</span>
              <input type="text" id="bgColorHex" value="1a1a2e" maxlength="6" style="flex:1;padding:4px 8px;border:1px solid rgba(255,255,255,0.15);border-radius:4px;background:rgba(255,255,255,0.05);color:#fff;font-size:12px;outline:none;font-family:monospace;">
              <div id="bgColorPreview" style="width:24px;height:24px;border-radius:4px;border:1px solid rgba(255,255,255,0.2);background:#1a1a2e;"></div>
            </div>
          </div>
        </div>
        <div style="margin-bottom:12px;">
          <div style="display:flex;justify-content:space-between;align-items:center;margin-bottom:6px;">
            <label style="font-size:12px;color:rgba(255,255,255,0.6);">布局</label>
            <button id="newLayoutBtn" style="padding:2px 8px;border:none;border-radius:4px;background:#4ade80;color:#000;font-size:11px;cursor:pointer;">+ 新建</button>
          </div>
          <div id="layoutList" style="max-height:150px;overflow-y:auto;"></div>
        </div>
        <div style="display:flex;gap:6px;flex-wrap:wrap;">
          <button id="enterEditBtn" style="flex:1;padding:8px;border:none;border-radius:6px;background:#60a5fa;color:#fff;font-size:12px;cursor:pointer;">编辑布局</button>
          <button id="exportLayoutBtn" style="flex:1;padding:8px;border:none;border-radius:6px;background:rgba(255,255,255,0.1);color:#fff;font-size:12px;cursor:pointer;">导出</button>
          <button id="importLayoutBtn" style="flex:1;padding:8px;border:none;border-radius:6px;background:rgba(255,255,255,0.1);color:#fff;font-size:12px;cursor:pointer;">导入</button>
        </div>
        <input type="file" id="importFile" accept=".json" style="display:none;">
      </div>
    </div>
    <div id="editToolbar" style="display:none;position:fixed;bottom:10px;left:50%;transform:translateX(-50%);z-index:700;background:rgba(0,0,0,0.8);padding:8px 16px;border-radius:8px;gap:8px;align-items:center;">
      <span style="font-size:12px;color:rgba(255,255,255,0.7);">编辑模式</span>
      <button id="saveEditBtn" style="padding:4px 10px;border:none;border-radius:4px;background:#4ade80;color:#000;font-size:12px;cursor:pointer;">保存</button>
      <button id="cancelEditBtn" style="padding:4px 10px;border:none;border-radius:4px;background:#f87171;color:#fff;font-size:12px;cursor:pointer;">取消</button>
    </div>
    <script>
(function() {
    'use strict';

    var state = {
        ws: null,
        connected: false,
        authenticated: false,
        password: null,
        token: null,
        reconnectTimer: null,
        reconnectAttempts: 0,
        maxReconnectDelay: 30000,
        buttons: 0,
        leftTrigger: 0,
        rightTrigger: 0,
        thumbLX: 0, thumbLY: 0,
        thumbRX: 0, thumbRY: 0,
        animFrameId: null,
        lastSendTime: 0,
        sendInterval: 16,
        lastPong: Date.now(),
        heartbeatInterval: null,
        heartbeatTimeout: 12000,
        framesSent: 0,
        fpsStartTime: Date.now(),
        rotated: false,
        editMode: false
    };

    var BTN = {
        UP: 1<<0, DOWN: 1<<1, LEFT: 1<<2, RIGHT: 1<<3,
        START: 1<<4, BACK: 1<<5,
        LTHUMB: 1<<6, RTHUMB: 1<<7,
        LB: 1<<8, RB: 1<<9,
        GUIDE: 1<<10,
        A: 1<<12, B: 1<<13, X: 1<<14, Y: 1<<15
    };

    var $ = function(id) { return document.getElementById(id); };
    var dom = null;

    function setStatus(connected, auth) {
        state.connected = connected;
        dom.statusDot.className = 'status-dot ' + (connected ? 'connected' : 'disconnected');
        if (connected && auth) dom.statusText.textContent = 'Connected';
        else if (connected) dom.statusText.textContent = 'Authenticating...';
        else dom.statusText.textContent = 'Connecting...';
    }

    function showControls() { dom.controls.className = 'controls visible'; }

    function updateFps() {
        var elapsed = (Date.now() - state.fpsStartTime) / 1000;
        if (elapsed > 1) dom.fps.textContent = Math.round(state.framesSent / elapsed) + ' FPS';
    }

    // ========== Orientation / Force Landscape ==========
    // 顺时针 90° 旋转时，元素内部坐标 (dx, dy) 与视口偏移 (vx, vy) 的关系：
    //   dx = vy,  dy = -vx
    // 即内部 x 轴朝向视口下方，内部 y 轴朝向视口左方。
    function applyRotation(el, w, h, rotate) {
        if (rotate) {
            // 宽高互换，旋转 90° 后向上平移自身原高度，正好填满视口
            el.style.width = h + 'px';
            el.style.height = w + 'px';
            el.style.transform = 'rotate(90deg) translateY(-' + w + 'px)';
        } else {
            el.style.width = w + 'px';
            el.style.height = h + 'px';
            el.style.transform = '';
        }
    }
    function updateOrientation() {
        var w = window.innerWidth;
        var h = window.innerHeight;
        var rotate = h > w;
        state.rotated = rotate;
        var container = document.querySelector('.container');
        if (container) applyRotation(container, w, h, rotate);
        var authOverlay = document.getElementById('authOverlay');
        if (authOverlay) applyRotation(authOverlay, w, h, rotate);

        var viewW = rotate ? h : w;
        var viewH = rotate ? w : h;
        var statusHeight = 22;
        var designW = 700;
        var designH = 400 - statusHeight;

        var controls = $('controls');
        var gamepad = $('gamepad');
        if (controls && gamepad) {
            controls.style.width = designW + 'px';
            controls.style.height = designH + 'px';
            var availH = viewH - statusHeight;
            var scaleX = viewW / designW;
            var scaleY = availH / designH;
            var scale = Math.min(scaleX, scaleY);
            controls.style.transform = 'scale(' + scale + ')';
            var offsetX = (viewW - designW * scale) / 2;
            var offsetY = (availH - designH * scale) / 2;
            controls.style.left = offsetX + 'px';
            controls.style.top = offsetY + 'px';
        }
    }

    // ========== Password Dialog ==========
    function showPasswordDialog() {
        if (document.getElementById('authOverlay')) return;
        var overlay = document.createElement('div');
        overlay.id = 'authOverlay';
        overlay.innerHTML = '<div style="background:#1a1a2e;border:1px solid rgba(255,255,255,0.15);border-radius:16px;padding:24px;width:260px;text-align:center;">'
            + '<h2 style="margin-bottom:6px;font-size:18px;">WebPad</h2>'
            + '<p style="color:rgba(255,255,255,0.5);margin-bottom:14px;font-size:12px;">Enter password from your PC</p>'
            + '<input type="password" id="passwordInput" placeholder="Password" autocomplete="off" '
            + 'style="width:100%;padding:8px 12px;border:2px solid rgba(255,255,255,0.15);border-radius:8px;background:rgba(255,255,255,0.05);color:#fff;font-size:15px;outline:none;box-sizing:border-box;">'
            + '<button id="authBtn" style="width:100%;margin-top:8px;padding:8px;border:none;border-radius:8px;background:#4ade80;color:#000;font-size:14px;font-weight:bold;cursor:pointer;">Connect</button>'
            + '<p id="authError" style="color:#f87171;margin-top:8px;font-size:11px;display:none;"></p>'
            + '</div>';
        document.body.appendChild(overlay);

        var input = $('passwordInput');
        var btn = $('authBtn');
        var error = $('authError');

        function doAuth() {
            var pwd = input.value.trim();
            if (!pwd) return;
            error.style.display = 'none';
            btn.disabled = true;
            btn.textContent = '...';
            sendWs({ type: 'auth_request', password: pwd });
        }
        btn.addEventListener('click', doAuth);
        input.addEventListener('keydown', function(e) { if (e.key === 'Enter') doAuth(); });
        setTimeout(function() { input.focus(); }, 200);
    }

    function removePasswordDialog() {
        var el = document.getElementById('authOverlay');
        if (el) el.remove();
    }

    function showAuthError(msg) {
        var error = $('authError');
        var btn = $('authBtn');
        if (error) { error.textContent = msg; error.style.display = 'block'; }
        if (btn) { btn.disabled = false; btn.textContent = 'Retry'; }
    }

    // ========== WebSocket ==========
    function connect() {
        if (state.ws && (state.ws.readyState === WebSocket.OPEN || state.ws.readyState === WebSocket.CONNECTING)) return;
        var protocol = window.location.protocol === 'https:' ? 'wss:' : 'ws:';
        var url = protocol + '//' + window.location.host + '/ws';
        setStatus(false, false);
        try {
            var ws = new WebSocket(url);
            state.ws = ws;
            ws.onopen = function() { state.reconnectAttempts = 0; setStatus(true, false); };
            ws.onmessage = function(event) { try { handleServerMessage(JSON.parse(event.data)); } catch (e) {} };
            ws.onclose = function() { state.ws = null; state.authenticated = false; state.token = null; setStatus(false, false); stopHeartbeat(); scheduleReconnect(); };
            ws.onerror = function() {};
        } catch (e) { scheduleReconnect(); }
    }

    function scheduleReconnect() {
        if (state.reconnectTimer) return;
        var delay = Math.min(1000 * Math.pow(2, state.reconnectAttempts), state.maxReconnectDelay);
        state.reconnectAttempts++;
        state.reconnectTimer = setTimeout(function() { state.reconnectTimer = null; connect(); }, delay);
    }

    function sendWs(msg) {
        if (state.ws && state.ws.readyState === WebSocket.OPEN) state.ws.send(JSON.stringify(msg));
    }

    function handleServerMessage(msg) {
        switch (msg.type) {
            case 'connected': break;
            case 'auth_success':
                state.authenticated = true;
                state.token = msg.token || '';
                removePasswordDialog();
                setStatus(true, true);
                showControls();
                startHeartbeat();
                break;
            case 'auth_failure':
                state.authenticated = false;
                showAuthError(msg.reason || 'Wrong password');
                break;
            case 'pong':
                state.lastPong = Date.now();
                break;
            case 'rumble':
                if (navigator.vibrate) {
                    var intensity = Math.max(msg.left_motor || 0, msg.right_motor || 0);
                    if (intensity > 0) navigator.vibrate(intensity * 2);
                }
                break;
        }
    }

    function startHeartbeat() {
        stopHeartbeat();
        state.lastPong = Date.now();
        state.heartbeatInterval = setInterval(function() {
            sendWs({ type: 'heartbeat' });
            if (Date.now() - state.lastPong > state.heartbeatTimeout) disconnect();
        }, 5000);
    }
    function stopHeartbeat() {
        if (state.heartbeatInterval) { clearInterval(state.heartbeatInterval); state.heartbeatInterval = null; }
    }
    function disconnect() {
        stopHeartbeat();
        if (state.ws) { sendWs({ type: 'disconnect', reason: 'client_timeout' }); state.ws.close(); state.ws = null; }
        state.authenticated = false;
        setStatus(false, false);
        scheduleReconnect();
    }

    function sendGamepadState() {
        if (!state.connected || !state.authenticated) return;
        sendWs({
            type: 'gamepad_state',
            buttons: state.buttons,
            left_trigger: state.leftTrigger,
            right_trigger: state.rightTrigger,
            thumb_lx: state.thumbLX, thumb_ly: state.thumbLY,
            thumb_rx: state.thumbRX, thumb_ry: state.thumbRY
        });
        state.framesSent++;
    }

    // ========== Buttons (ABXY, LB, RB, Back, Start) ==========
    function setupButtons() {
        var btnMap = {
            start: BTN.START, back: BTN.BACK, select: BTN.BACK,
            lb: BTN.LB, rb: BTN.RB,
            a: BTN.A, b: BTN.B, x: BTN.X, y: BTN.Y
        };
        document.querySelectorAll('[data-btn]').forEach(function(el) {
            if (el.id === 'dpad') return;
            var bit = btnMap[el.dataset.btn];
            if (bit === undefined) return;
            function press(e) { if (state.editMode) return; e.preventDefault(); e.stopPropagation(); state.buttons |= bit; el.classList.add('pressed'); }
            function release(e) { if (state.editMode) return; e.preventDefault(); e.stopPropagation(); state.buttons &= ~bit; el.classList.remove('pressed'); }
            el.addEventListener('touchstart', press, {passive:false});
            el.addEventListener('touchend', release, {passive:false});
            el.addEventListener('touchcancel', release, {passive:false});
            el.addEventListener('mousedown', press);
            el.addEventListener('mouseup', release);
            el.addEventListener('mouseleave', release);
        });
    }

    // ========== Triggers (multi-touch safe via touch.identifier) ==========
    function setupTriggers() {
        function setupTrigger(el, setter) {
            if (!el) return;
            var activeTouchId = null;
            el.addEventListener('touchstart', function(e) {
                if (state.editMode) return;
                e.preventDefault(); e.stopPropagation();
                if (activeTouchId !== null) return; // already tracking a finger
                var t = e.changedTouches[0];
                activeTouchId = t.identifier;
                doUpdate(t);
            }, {passive:false});
            el.addEventListener('touchmove', function(e) {
                if (state.editMode) return;
                e.preventDefault(); e.stopPropagation();
                if (activeTouchId === null) return;
                var t = findChangedTouch(e, activeTouchId);
                if (t) doUpdate(t);
            }, {passive:false});
            function onTouchEnd(e) {
                if (state.editMode) return;
                e.preventDefault(); e.stopPropagation();
                if (activeTouchId === null) return;
                var t = findChangedTouch(e, activeTouchId);
                if (t) { activeTouchId = null; doReset(); }
            }
            el.addEventListener('touchend', onTouchEnd, {passive:false});
            el.addEventListener('touchcancel', onTouchEnd, {passive:false});
            // mouse fallback
            el.addEventListener('mousedown', function(e) {
                if (state.editMode) return;
                e.preventDefault(); e.stopPropagation();
                doUpdate(e);
                function onMove(ev) { doUpdate(ev); }
                function onUp(ev) { doReset(); document.removeEventListener('mousemove',onMove); document.removeEventListener('mouseup',onUp); }
                document.addEventListener('mousemove',onMove);
                document.addEventListener('mouseup',onUp);
            });
            function doUpdate(touchOrMouse) {
                var rect = el.getBoundingClientRect();
                var cx = rect.left + rect.width/2, cy = rect.top + rect.height/2;
                var h = el.offsetHeight;
                var localDy;
                if (state.rotated) {
                    localDy = cx - touchOrMouse.clientX;
                } else {
                    localDy = touchOrMouse.clientY - cy;
                }
                var y = localDy + h/2;
                var v = Math.round(Math.max(0, Math.min(1, y / h)) * 255);
                setter(v);
                el.querySelector('.trigger-fill').style.height = (v/255*100)+'%';
            }
            function doReset() {
                setter(0);
                el.querySelector('.trigger-fill').style.height = '0%';
            }
        }
        function findChangedTouch(e, id) {
            for (var i = 0; i < e.changedTouches.length; i++) {
                if (e.changedTouches[i].identifier === id) return e.changedTouches[i];
            }
            return null;
        }
        setupTrigger(dom.leftTrigger, function(v) { state.leftTrigger = v; });
        setupTrigger(dom.rightTrigger, function(v) { state.rightTrigger = v; });
    }

    // ========== D-Pad (8-direction, multi-touch safe via touch.identifier) ==========
    function setupDpad() {
        var el = $('dpad');
        if (!el) return;
        var activeTouchId = null;
        var deadzone = 8;

        function getDirection(touchOrMouse) {
            var rect = el.getBoundingClientRect();
            var cx = rect.left + rect.width / 2;
            var cy = rect.top + rect.height / 2;
            var vx = touchOrMouse.clientX - cx;
            var vy = touchOrMouse.clientY - cy;
            var dx, dy;
            if (state.rotated) { dx = vy; dy = -vx; }
            else { dx = vx; dy = vy; }
            state.buttons &= ~(BTN.UP | BTN.DOWN | BTN.LEFT | BTN.RIGHT);
            el.querySelectorAll('.dpad-btn').forEach(function(b) { b.classList.remove('pressed'); });
            if (Math.abs(dx) < deadzone && Math.abs(dy) < deadzone) return;
            var angle = Math.atan2(dy, dx) * 180 / Math.PI;
            if (angle < 0) angle += 360;
            if ((angle >= 337.5) || (angle < 22.5)) {
                state.buttons |= BTN.RIGHT;
                el.querySelector('.dpad-btn.right').classList.add('pressed');
            } else if (angle < 67.5) {
                state.buttons |= BTN.DOWN | BTN.RIGHT;
                el.querySelector('.dpad-btn.down').classList.add('pressed');
                el.querySelector('.dpad-btn.right').classList.add('pressed');
            } else if (angle < 112.5) {
                state.buttons |= BTN.DOWN;
                el.querySelector('.dpad-btn.down').classList.add('pressed');
            } else if (angle < 157.5) {
                state.buttons |= BTN.DOWN | BTN.LEFT;
                el.querySelector('.dpad-btn.down').classList.add('pressed');
                el.querySelector('.dpad-btn.left').classList.add('pressed');
            } else if (angle < 202.5) {
                state.buttons |= BTN.LEFT;
                el.querySelector('.dpad-btn.left').classList.add('pressed');
            } else if (angle < 247.5) {
                state.buttons |= BTN.UP | BTN.LEFT;
                el.querySelector('.dpad-btn.up').classList.add('pressed');
                el.querySelector('.dpad-btn.left').classList.add('pressed');
            } else if (angle < 292.5) {
                state.buttons |= BTN.UP;
                el.querySelector('.dpad-btn.up').classList.add('pressed');
            } else {
                state.buttons |= BTN.UP | BTN.RIGHT;
                el.querySelector('.dpad-btn.up').classList.add('pressed');
                el.querySelector('.dpad-btn.right').classList.add('pressed');
            }
        }

        function resetDpad() {
            state.buttons &= ~(BTN.UP | BTN.DOWN | BTN.LEFT | BTN.RIGHT);
            el.querySelectorAll('.dpad-btn').forEach(function(b) { b.classList.remove('pressed'); });
        }

        el.addEventListener('touchstart', function(e) {
            if (state.editMode) return;
            e.preventDefault(); e.stopPropagation();
            if (activeTouchId !== null) return;
            var t = e.changedTouches[0];
            activeTouchId = t.identifier;
            getDirection(t);
        }, {passive:false});
        el.addEventListener('touchmove', function(e) {
            if (state.editMode) return;
            e.preventDefault(); e.stopPropagation();
            if (activeTouchId === null) return;
            var t = findTouch(e, activeTouchId);
            if (t) getDirection(t);
        }, {passive:false});
        function onTouchEnd(e) {
            if (state.editMode) return;
            e.preventDefault(); e.stopPropagation();
            if (activeTouchId === null) return;
            var t = findTouch(e, activeTouchId);
            if (t) { activeTouchId = null; resetDpad(); }
        }
        el.addEventListener('touchend', onTouchEnd, {passive:false});
        el.addEventListener('touchcancel', onTouchEnd, {passive:false});
        // mouse fallback
        el.addEventListener('mousedown', function(e) {
            if (state.editMode) return;
            e.preventDefault(); e.stopPropagation();
            getDirection(e);
            function onMove(ev) { getDirection(ev); }
            function onUp(ev) { resetDpad(); document.removeEventListener('mousemove',onMove); document.removeEventListener('mouseup',onUp); }
            document.addEventListener('mousemove',onMove);
            document.addEventListener('mouseup',onUp);
        });

        function findTouch(e, id) {
            for (var i = 0; i < e.changedTouches.length; i++) {
                if (e.changedTouches[i].identifier === id) return e.changedTouches[i];
            }
            return null;
        }
    }

    // ========== Thumbsticks (origin at first touch, no direction until drag) ==========
    function setupThumbsticks() {
        function setupStick(el, innerEl, setX, setY) {
            if (!el || !innerEl) return;
            var activeTouchId = null;
            var radius = 36;
            var maxVal = 32767;
            var originX = 0, originY = 0;

            function doStart(clientX, clientY) {
                originX = clientX;
                originY = clientY;
                setX(0); setY(0);
                innerEl.style.transform = 'translate(-50%, -50%)';
            }
            function doUpdate(clientX, clientY) {
                var vx = clientX - originX, vy = clientY - originY;
                var dx, dy;
                if (state.rotated) { dx = vy; dy = -vx; }
                else { dx = vx; dy = vy; }
                var dist = Math.sqrt(dx*dx + dy*dy);
                if (dist > radius) { dx = dx/dist*radius; dy = dy/dist*radius; }
                setX(Math.round(dx/radius*maxVal));
                setY(Math.round(-dy/radius*maxVal));
                innerEl.style.transform = 'translate(calc(-50% + '+dx+'px), calc(-50% + '+dy+'px))';
            }
            function doReset() {
                setX(0); setY(0);
                innerEl.style.transform = 'translate(-50%, -50%)';
            }

            el.addEventListener('touchstart', function(e) {
                if (state.editMode) return;
                e.preventDefault(); e.stopPropagation();
                if (activeTouchId !== null) return;
                var t = e.changedTouches[0];
                activeTouchId = t.identifier;
                doStart(t.clientX, t.clientY);
            }, {passive:false});
            el.addEventListener('touchmove', function(e) {
                if (state.editMode) return;
                e.preventDefault(); e.stopPropagation();
                if (activeTouchId === null) return;
                var t = findTouch(e, activeTouchId);
                if (t) doUpdate(t.clientX, t.clientY);
            }, {passive:false});
            function onTouchEnd(e) {
                if (state.editMode) return;
                e.preventDefault(); e.stopPropagation();
                if (activeTouchId === null) return;
                var t = findTouch(e, activeTouchId);
                if (t) { activeTouchId = null; doReset(); }
            }
            el.addEventListener('touchend', onTouchEnd, {passive:false});
            el.addEventListener('touchcancel', onTouchEnd, {passive:false});
            // mouse fallback
            el.addEventListener('mousedown', function(e) {
                if (state.editMode) return;
                e.preventDefault(); e.stopPropagation();
                doStart(e.clientX, e.clientY);
                function onMove(ev) { doUpdate(ev.clientX, ev.clientY); }
                function onUp(ev) { doReset(); document.removeEventListener('mousemove',onMove); document.removeEventListener('mouseup',onUp); }
                document.addEventListener('mousemove',onMove);
                document.addEventListener('mouseup',onUp);
            });

            function findTouch(e, id) {
                for (var i = 0; i < e.changedTouches.length; i++) {
                    if (e.changedTouches[i].identifier === id) return e.changedTouches[i];
                }
                return null;
            }
        }
        setupStick(dom.leftStick, dom.leftStickInner,
            function(v) { state.thumbLX = v; }, function(v) { state.thumbLY = v; });
        setupStick(dom.rightStick, dom.rightStickInner,
            function(v) { state.thumbRX = v; }, function(v) { state.thumbRY = v; });
    }

    var LS_KEY = 'webpad_layouts_v2';
    var LAYOUT_ELEMENTS = ['lb','rb','lt','rt','leftStick','rightStick','dpad','y','a','x','b','back','start'];

    function getDefaultLayout() {
        return {
            id: 'default',
            name: '默认布局',
            bgColor: '#1a1a2e',
            elements: {
                lb: { left: '11.7%', top: '37px', width: '100px', height: '35px' },
                rb: { right: '11.7%', top: '35px', width: '100px', height: '35px' },
                lt: { left: '32.9%', top: '100px', width: '45px', height: '180px' },
                rt: { right: '32.9%', top: '100px', width: '45px', height: '180px' },
                leftStick: { left: '10.6%', top: '104px', width: '117px', height: '117px' },
                rightStick: { right: '10.6%', bottom: '30px', width: '117px', height: '117px' },
                dpad: { left: '8.6%', bottom: '25px', width: '145px', height: '135px' },
                y: { right: '17.1%', top: '95px', width: '40px', height: '40px' },
                a: { right: '17.1%', top: '180px', width: '40px', height: '40px' },
                x: { right: '22.9%', top: '140px', width: '40px', height: '40px' },
                b: { right: '11.4%', top: '140px', width: '40px', height: '40px' },
                back: { left: '37.1%', top: '300px', width: '60px', height: '22px' },
                start: { right: '37.1%', top: '300px', width: '60px', height: '22px' }
            }
        };
    }

    var LayoutManager = {
        data: null,
        init: function() {
            this.load();
            if (!this.data) {
                this.data = { version: 2, activeLayoutId: 'default', layouts: [getDefaultLayout()] };
                this.save();
            }
            var defaultIdx = -1;
            for (var i = 0; i < this.data.layouts.length; i++) {
                if (this.data.layouts[i].id === 'default') { defaultIdx = i; break; }
            }
            if (defaultIdx === -1) {
                this.data.layouts.unshift(getDefaultLayout());
            }
            this.applyLayout(this.data.activeLayoutId);
        },
        load: function() {
            try {
                var raw = localStorage.getItem(LS_KEY);
                if (raw) this.data = JSON.parse(raw);
            } catch(e) {}
        },
        save: function() {
            try { localStorage.setItem(LS_KEY, JSON.stringify(this.data)); } catch(e) {}
        },
        getLayout: function(id) {
            for (var i = 0; i < this.data.layouts.length; i++) {
                if (this.data.layouts[i].id === id) return this.data.layouts[i];
            }
            return null;
        },
        getActiveLayout: function() {
            return this.getLayout(this.data.activeLayoutId);
        },
        setActiveLayout: function(id) {
            this.data.activeLayoutId = id;
            this.save();
            this.applyLayout(id);
        },
        applyLayout: function(id) {
            var layout = this.getLayout(id) || this.getLayout('default');
            if (!layout) return;
            if (layout.bgColor) document.body.style.background = layout.bgColor;
            for (var key in layout.elements) {
                var el = document.querySelector('[data-layout-id="' + key + '"]');
                if (!el) continue;
                var s = layout.elements[key];
                el.style.left = s.left !== undefined ? s.left : '';
                el.style.top = s.top !== undefined ? s.top : '';
                el.style.right = s.right !== undefined ? s.right : '';
                el.style.bottom = s.bottom !== undefined ? s.bottom : '';
                el.style.width = s.width !== undefined ? s.width : '';
                el.style.height = s.height !== undefined ? s.height : '';
            }
        },
        createLayout: function(name, baseId) {
            var base = this.getLayout(baseId) || this.getLayout('default');
            var newLayout = {
                id: 'layout_' + Date.now(),
                name: name || '新布局',
                bgColor: base.bgColor,
                elements: JSON.parse(JSON.stringify(base.elements))
            };
            this.data.layouts.push(newLayout);
            this.save();
            return newLayout;
        },
        deleteLayout: function(id) {
            if (id === 'default') return false;
            var idx = -1;
            for (var i = 0; i < this.data.layouts.length; i++) {
                if (this.data.layouts[i].id === id) { idx = i; break; }
            }
            if (idx === -1) return false;
            this.data.layouts.splice(idx, 1);
            if (this.data.activeLayoutId === id) {
                this.data.activeLayoutId = 'default';
                this.applyLayout('default');
            }
            this.save();
            return true;
        },
        renameLayout: function(id, newName) {
            var layout = this.getLayout(id);
            if (layout) { layout.name = newName; this.save(); }
        },
        updateBgColor: function(layoutId, color) {
            var layout = this.getLayout(layoutId);
            if (layout) { layout.bgColor = color; this.save(); }
        },
        exportActive: function() {
            return JSON.stringify(this.getActiveLayout(), null, 2);
        },
        importLayout: function(json) {
            var obj = JSON.parse(json);
            if (!obj || !obj.elements) throw new Error('Invalid layout');
            obj.id = 'layout_' + Date.now();
            if (!obj.name) obj.name = '导入布局';
            this.data.layouts.push(obj);
            this.save();
            return obj;
        }
    };

    function setupSettingsPanel() {
        var settingsBtn = $('settingsBtn');
        var settingsPanel = $('settingsPanel');
        var closeSettings = $('closeSettings');
        var colorPalette = $('colorPalette');
        var bgColorHex = $('bgColorHex');
        var bgColorPreview = $('bgColorPreview');
        var layoutList = $('layoutList');
        var newLayoutBtn = $('newLayoutBtn');
        var enterEditBtn = $('enterEditBtn');
        var exportLayoutBtn = $('exportLayoutBtn');
        var importLayoutBtn = $('importLayoutBtn');
        var importFile = $('importFile');
        var editToolbar = $('editToolbar');
        var saveEditBtn = $('saveEditBtn');
        var cancelEditBtn = $('cancelEditBtn');

        var PRESET_COLORS = [
            '#1a1a2e','#16213e','#0f3460','#533483',
            '#1e1e1e','#2d2d2d','#1a3a1a','#3a1a1a',
            '#0d1117','#161b22','#21262d','#30363d',
            '#1a1a1a','#2c3e50','#34495e','#0b0e14'
        ];

        function setBgColor(color) {
            var hex = color.replace('#', '');
            document.body.style.background = '#' + hex;
            bgColorPreview.style.background = '#' + hex;
            bgColorHex.value = hex;
            LayoutManager.updateBgColor(LayoutManager.data.activeLayoutId, '#' + hex);
            colorPalette.querySelectorAll('.color-swatch').forEach(function(s) {
                s.classList.toggle('active', s.dataset.color === '#' + hex);
            });
        }

        function buildPalette() {
            colorPalette.innerHTML = '';
            PRESET_COLORS.forEach(function(color) {
                var swatch = document.createElement('div');
                swatch.className = 'color-swatch';
                swatch.style.background = color;
                swatch.dataset.color = color;
                swatch.addEventListener('click', function() { setBgColor(color); });
                colorPalette.appendChild(swatch);
            });
        }
        buildPalette();

        function renderLayoutList() {
            layoutList.innerHTML = '';
            LayoutManager.data.layouts.forEach(function(layout) {
                var item = document.createElement('div');
                item.className = 'layout-item' + (layout.id === LayoutManager.data.activeLayoutId ? ' active' : '');
                var nameSpan = document.createElement('span');
                nameSpan.textContent = layout.name;
                var actions = document.createElement('div');
                if (layout.id !== 'default') {
                    var renameBtn = document.createElement('button');
                    renameBtn.textContent = '重命名';
                    renameBtn.onclick = function(e) {
                        e.stopPropagation();
                        var newName = prompt('新名称', layout.name);
                        if (newName) { LayoutManager.renameLayout(layout.id, newName); renderLayoutList(); }
                    };
                    var delBtn = document.createElement('button');
                    delBtn.textContent = '删除';
                    delBtn.onclick = function(e) {
                        e.stopPropagation();
                        if (confirm('删除布局 "' + layout.name + '"?')) {
                            LayoutManager.deleteLayout(layout.id);
                            renderLayoutList();
                        }
                    };
                    actions.appendChild(renameBtn);
                    actions.appendChild(delBtn);
                }
                var switchBtn = document.createElement('button');
                switchBtn.textContent = layout.id === LayoutManager.data.activeLayoutId ? '使用中' : '切换';
                switchBtn.style.color = layout.id === LayoutManager.data.activeLayoutId ? '#4ade80' : '';
                switchBtn.onclick = function(e) {
                    e.stopPropagation();
                    LayoutManager.setActiveLayout(layout.id);
                    renderLayoutList();
                    var active = LayoutManager.getActiveLayout();
                    setBgColor(active.bgColor || '#1a1a2e');
                };
                actions.appendChild(switchBtn);
                item.appendChild(nameSpan);
                item.appendChild(actions);
                layoutList.appendChild(item);
            });
        }

        settingsBtn.addEventListener('click', function() {
            settingsPanel.classList.add('active');
            var active = LayoutManager.getActiveLayout();
            setBgColor(active.bgColor || '#1a1a2e');
            renderLayoutList();
        });
        closeSettings.addEventListener('click', function() { settingsPanel.classList.remove('active'); });
        settingsPanel.addEventListener('click', function(e) { if (e.target === settingsPanel) settingsPanel.classList.remove('active'); });

        bgColorHex.addEventListener('input', function() {
            var hex = bgColorHex.value.replace(/[^0-9a-fA-F]/g, '').substring(0, 6);
            if (hex.length === 6) setBgColor('#' + hex);
        });

        newLayoutBtn.addEventListener('click', function() {
            var name = prompt('新布局名称', '新布局');
            if (name) {
                LayoutManager.createLayout(name, LayoutManager.data.activeLayoutId);
                renderLayoutList();
            }
        });

        exportLayoutBtn.addEventListener('click', function() {
            var data = LayoutManager.exportActive();
            var blob = new Blob([data], {type: 'application/json'});
            var url = URL.createObjectURL(blob);
            var a = document.createElement('a');
            a.href = url;
            a.download = (LayoutManager.getActiveLayout().name || 'layout') + '.json';
            a.click();
            URL.revokeObjectURL(url);
        });

        importLayoutBtn.addEventListener('click', function() { importFile.click(); });
        importFile.addEventListener('change', function(e) {
            var file = e.target.files[0];
            if (!file) return;
            var reader = new FileReader();
            reader.onload = function(ev) {
                try {
                    var imported = LayoutManager.importLayout(ev.target.result);
                    LayoutManager.setActiveLayout(imported.id);
                    renderLayoutList();
                    setBgColor(imported.bgColor || '#1a1a2e');
                    alert('导入成功');
                } catch(err) { alert('导入失败: ' + err.message); }
            };
            reader.readAsText(file);
            importFile.value = '';
        });

        var editState = {
            active: false,
            selectedId: null,
            dragMode: null,
            dragStartX: 0, dragStartY: 0,
            elStartLeft: 0, elStartTop: 0,
            elStartW: 0, elStartH: 0
        };

        function getEditScale() {
            var controls = dom.controls;
            var scaleMatch = controls.style.transform.match(/scale\(([^)]+)\)/);
            return scaleMatch ? parseFloat(scaleMatch[1]) : 1;
        }

        function clientToInner(dx, dy) {
            var scale = getEditScale();
            if (state.rotated) {
                return { dx: dy / scale, dy: -dx / scale };
            }
            return { dx: dx / scale, dy: dy / scale };
        }

        function removeResizeHandles(el) {
            el.querySelectorAll('.edit-resize-handle').forEach(function(h) { h.remove(); });
        }

        function addResizeHandles(el) {
            removeResizeHandles(el);
            ['nw','ne','sw','se'].forEach(function(corner) {
                var handle = document.createElement('div');
                handle.className = 'edit-resize-handle ' + corner;
                handle.dataset.corner = corner;
                handle._resizeDown = function(e) {
                    if (!editState.active) return;
                    if (e.cancelable) e.preventDefault();
                    e.stopPropagation();
                    editState.dragMode = 'resize-' + corner;
                    editState.dragStartX = e.touches ? e.touches[0].clientX : e.clientX;
                    editState.dragStartY = e.touches ? e.touches[0].clientY : e.clientY;
                    var computed = window.getComputedStyle(el);
                    editState.elStartLeft = parseFloat(computed.left) || 0;
                    editState.elStartTop = parseFloat(computed.top) || 0;
                    editState.elStartW = parseFloat(computed.width) || el.offsetWidth;
                    editState.elStartH = parseFloat(computed.height) || el.offsetHeight;
                };
                handle.addEventListener('touchstart', handle._resizeDown, {passive:false});
                handle.addEventListener('mousedown', handle._resizeDown);
                el.appendChild(handle);
            });
        }

        function onEditMove(e) {
            if (!editState.active || !editState.dragMode) return;
            if (e.cancelable) e.preventDefault();
            var clientX = e.touches ? e.touches[0].clientX : e.clientX;
            var clientY = e.touches ? e.touches[0].clientY : e.clientY;
            var cdx = clientX - editState.dragStartX;
            var cdy = clientY - editState.dragStartY;
            var inner = clientToInner(cdx, cdy);
            var el = document.querySelector('[data-layout-id="' + editState.selectedId + '"]');
            if (!el) return;
            var designW = 700, designH = 378;
            var minSize = 20;

            if (editState.dragMode === 'move') {
                var newLeft = editState.elStartLeft + inner.dx;
                var newTop = editState.elStartTop + inner.dy;
                var w = editState.elStartW;
                var h = editState.elStartH;
                newLeft = Math.max(0, Math.min(designW - w, newLeft));
                newTop = Math.max(0, Math.min(designH - h, newTop));
                el.style.left = newLeft + 'px';
                el.style.top = newTop + 'px';
                el.style.right = 'auto';
                el.style.bottom = 'auto';
            } else if (editState.dragMode.indexOf('resize-') === 0) {
                var corner = editState.dragMode.substring(7);
                var newW = editState.elStartW;
                var newH = editState.elStartH;
                var newL = editState.elStartLeft;
                var newT = editState.elStartTop;
                if (corner === 'se') {
                    newW = Math.max(minSize, editState.elStartW + inner.dx);
                    newH = Math.max(minSize, editState.elStartH + inner.dy);
                } else if (corner === 'sw') {
                    newW = Math.max(minSize, editState.elStartW - inner.dx);
                    newH = Math.max(minSize, editState.elStartH + inner.dy);
                    newL = editState.elStartLeft + (editState.elStartW - newW);
                } else if (corner === 'ne') {
                    newW = Math.max(minSize, editState.elStartW + inner.dx);
                    newH = Math.max(minSize, editState.elStartH - inner.dy);
                    newT = editState.elStartTop + (editState.elStartH - newH);
                } else if (corner === 'nw') {
                    newW = Math.max(minSize, editState.elStartW - inner.dx);
                    newH = Math.max(minSize, editState.elStartH - inner.dy);
                    newL = editState.elStartLeft + (editState.elStartW - newW);
                    newT = editState.elStartTop + (editState.elStartH - newH);
                }
                newL = Math.max(0, Math.min(designW - newW, newL));
                newT = Math.max(0, Math.min(designH - newH, newT));
                el.style.width = newW + 'px';
                el.style.height = newH + 'px';
                el.style.left = newL + 'px';
                el.style.top = newT + 'px';
                el.style.right = 'auto';
                el.style.bottom = 'auto';
            }
        }

        function onEditEnd(e) {
            editState.dragMode = null;
        }

        function startEditMode() {
            editState.active = true;
            state.editMode = true;
            document.body.classList.add('edit-mode');
            editToolbar.style.display = 'flex';
            settingsPanel.classList.remove('active');
            LAYOUT_ELEMENTS.forEach(function(id) {
                var el = document.querySelector('[data-layout-id="' + id + '"]');
                if (!el) return;
                el._editDown = function(e) {
                    if (!editState.active) return;
                    if (e.cancelable) e.preventDefault();
                    e.stopPropagation();
                    editState.selectedId = id;
                    editState.dragMode = 'move';
                    document.querySelectorAll('.edit-selected').forEach(function(s) {
                        s.classList.remove('edit-selected');
                        removeResizeHandles(s);
                    });
                    el.classList.add('edit-selected');
                    addResizeHandles(el);
                    var clientX = e.touches ? e.touches[0].clientX : e.clientX;
                    var clientY = e.touches ? e.touches[0].clientY : e.clientY;
                    editState.dragStartX = clientX;
                    editState.dragStartY = clientY;
                    var computed = window.getComputedStyle(el);
                    editState.elStartLeft = parseFloat(computed.left) || 0;
                    editState.elStartTop = parseFloat(computed.top) || 0;
                    editState.elStartW = parseFloat(computed.width) || el.offsetWidth;
                    editState.elStartH = parseFloat(computed.height) || el.offsetHeight;
                };
                el.addEventListener('touchstart', el._editDown, {passive:false});
                el.addEventListener('mousedown', el._editDown);
            });
            document.addEventListener('touchmove', onEditMove, {passive:false});
            document.addEventListener('mousemove', onEditMove);
            document.addEventListener('touchend', onEditEnd, {passive:false});
            document.addEventListener('mouseup', onEditEnd);
        }

        function stopEditMode(save) {
            editState.active = false;
            state.editMode = false;
            document.body.classList.remove('edit-mode');
            editToolbar.style.display = 'none';
            document.querySelectorAll('.edit-selected').forEach(function(s) {
                s.classList.remove('edit-selected');
                removeResizeHandles(s);
            });
            LAYOUT_ELEMENTS.forEach(function(id) {
                var el = document.querySelector('[data-layout-id="' + id + '"]');
                if (!el) return;
                if (el._editDown) {
                    el.removeEventListener('touchstart', el._editDown);
                    el.removeEventListener('mousedown', el._editDown);
                    el._editDown = null;
                }
            });
            document.removeEventListener('touchmove', onEditMove);
            document.removeEventListener('mousemove', onEditMove);
            document.removeEventListener('touchend', onEditEnd);
            document.removeEventListener('mouseup', onEditEnd);
            if (save) {
                var layout = LayoutManager.getActiveLayout();
                LAYOUT_ELEMENTS.forEach(function(id) {
                    var el = document.querySelector('[data-layout-id="' + id + '"]');
                    if (!el) return;
                    var computed = window.getComputedStyle(el);
                    var s = {};
                    if (computed.left && computed.left !== 'auto') s.left = computed.left;
                    if (computed.top && computed.top !== 'auto') s.top = computed.top;
                    if (computed.right && computed.right !== 'auto') s.right = computed.right;
                    if (computed.bottom && computed.bottom !== 'auto') s.bottom = computed.bottom;
                    if (computed.width) s.width = computed.width;
                    if (computed.height) s.height = computed.height;
                    layout.elements[id] = s;
                });
                LayoutManager.save();
            } else {
                LayoutManager.applyLayout(LayoutManager.data.activeLayoutId);
            }
        }

        enterEditBtn.addEventListener('click', startEditMode);
        saveEditBtn.addEventListener('click', function() { stopEditMode(true); });
        cancelEditBtn.addEventListener('click', function() { stopEditMode(false); });
    }

    // ========== Game Loop ==========
    function gameLoop(ts) {
        if (state.connected && state.authenticated) {
            if (ts - state.lastSendTime >= state.sendInterval) {
                sendGamepadState();
                state.lastSendTime = ts;
                updateFps();
            }
        }
        state.animFrameId = requestAnimationFrame(gameLoop);
    }

    // ========== Init ==========
    function init() {
        dom = {
            statusDot: $('statusDot'), statusText: $('statusText'), fps: $('fps'),
            controls: $('controls'),
            leftTrigger: $('leftTrigger'), rightTrigger: $('rightTrigger'),
            leftStick: $('leftStick'), rightStick: $('rightStick'),
            leftStickInner: $('leftStickInner'), rightStickInner: $('rightStickInner')
        };
        if (!dom.leftStick || !dom.rightStick) { console.error('DOM elements not found!'); return; }

        updateOrientation();
        window.addEventListener('resize', updateOrientation);
        window.addEventListener('orientationchange', function() {
            setTimeout(updateOrientation, 100);
        });

        LayoutManager.init();
        setupSettingsPanel();
        setupButtons();
        setupTriggers();
        setupDpad();
        setupThumbsticks();
        state.lastSendTime = performance.now();
        state.fpsStartTime = Date.now();
        state.animFrameId = requestAnimationFrame(gameLoop);
        connect();

        var waitForWs = setInterval(function() {
            if (state.ws && state.connected && !state.authenticated) {
                clearInterval(waitForWs);
                showPasswordDialog();
                updateOrientation();
            }
        }, 200);
    }

    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', init);
    } else {
        init();
    }
})();
    </script>
</body>
</html>"##.to_string()
}
