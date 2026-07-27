use std::sync::Arc;
use axum::{
    Router,
    body::Body,
    http::{HeaderName, HeaderValue, Request},
    response::Response,
    middleware::Next,
};
use axum::routing::get;
use tokio::sync::{Semaphore, OwnedSemaphorePermit};

use crate::auth::AuthManager;
use crate::auth::IpBackoffManager;
use crate::config::Config;
use crate::upnp::PortMapper;

use super::handler;

/// 游戏手柄句柄
///
/// 在 Windows 上包装 GamepadManager，在其他平台上为空实现。
pub struct GamepadHandle {
    #[cfg(windows)]
    pub inner: Option<Arc<crate::gamepad::GamepadManager>>,
}

#[cfg(windows)]
impl GamepadHandle {
    pub fn new(manager: Option<Arc<crate::gamepad::GamepadManager>>) -> Self {
        Self { inner: manager }
    }
}

#[cfg(not(windows))]
impl GamepadHandle {
    pub fn new(_manager: Option<Arc<()>>) -> Self {
        Self {}
    }
}

/// 连接数限制器
///
/// 通过两个 `Semaphore` 控制：
/// - `total`：总并发 WebSocket 连接数上限（含已认证和未认证），防资源耗尽
/// - `unauth`：未认证并发连接数上限，限制密码暴力破解/未认证 DoS 的代价
///
/// `try_acquire_*` 失败时调用方应返回 503，不进入 `handle_socket`。
/// 持有的 `OwnedSemaphorePermit` 跨 await 移动到 `handle_socket`，
/// 总连接 permit 持有到连接结束，未认证 permit 在认证成功时释放。
#[derive(Clone)]
pub struct ConnectionLimiter {
    total: Arc<Semaphore>,
    unauth: Arc<Semaphore>,
    max_total: usize,
    max_unauth: usize,
}

impl ConnectionLimiter {
    pub fn new(max_total: usize, max_unauth: usize) -> Self {
        Self {
            total: Arc::new(Semaphore::new(max_total)),
            unauth: Arc::new(Semaphore::new(max_unauth)),
            max_total,
            max_unauth,
        }
    }

    /// 尝试获取一个总连接 permit，失败返回 None
    pub fn try_acquire_total(&self) -> Option<OwnedSemaphorePermit> {
        self.total.clone().try_acquire_owned().ok()
    }

    /// 尝试获取一个未认证连接 permit，失败返回 None
    pub fn try_acquire_unauth(&self) -> Option<OwnedSemaphorePermit> {
        self.unauth.clone().try_acquire_owned().ok()
    }

    /// 当前已用的总连接数
    pub fn total_used(&self) -> usize {
        self.max_total - self.total.available_permits()
    }

    /// 当前已用的未认证连接数
    pub fn unauth_used(&self) -> usize {
        self.max_unauth - self.unauth.available_permits()
    }
}

/// 应用共享状态
pub struct AppState {
    pub auth: Arc<AuthManager>,
    pub gamepad: GamepadHandle,
    pub upnp: Arc<PortMapper>,
    pub config: Arc<Config>,
    pub connection_limiter: Arc<ConnectionLimiter>,
    pub ip_backoff: Arc<IpBackoffManager>,
}

/// 创建 axum 路由
pub fn create_router(state: Arc<AppState>) -> Router {
    Router::new()
        .route("/", get(handler::index))
        .route("/ws", get(handler::ws_upgrade))
        .with_state(state)
        .layer(axum::middleware::from_fn(security_headers_middleware))
}

/// 安全响应头中间件
///
/// 添加常见的 HTTP 安全头以减少攻击面：
/// - X-Content-Type-Options: nosniff
/// - X-Frame-Options: DENY
/// - Content-Security-Policy: 限制资源加载来源
/// - Referrer-Policy: no-referrer
/// - Permissions-Policy: 禁用不必要的浏览器特性
/// - Strict-Transport-Security: 强制 HTTPS（HSTS）
async fn security_headers_middleware(request: Request<Body>, next: Next) -> Response {
    let mut response = next.run(request).await;
    let headers = response.headers_mut();

    headers.insert(
        HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );
    headers.insert(
        HeaderName::from_static("content-security-policy"),
        HeaderValue::from_static("default-src 'self'; script-src 'self' 'unsafe-inline'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' wss: ws:"),
    );
    headers.insert(
        HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(
        HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static("geolocation=(), microphone=(), camera=(), payment=(), usb=(), bluetooth=()"),
    );
    headers.insert(
        HeaderName::from_static("strict-transport-security"),
        HeaderValue::from_static("max-age=31536000; includeSubDomains"),
    );

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limiter_starts_with_zero_used() {
        let limiter = ConnectionLimiter::new(8, 3);
        assert_eq!(limiter.total_used(), 0);
        assert_eq!(limiter.unauth_used(), 0);
    }

    #[test]
    fn limiter_acquire_increments_used() {
        let limiter = ConnectionLimiter::new(2, 1);
        let _t1 = limiter.try_acquire_total().unwrap();
        assert_eq!(limiter.total_used(), 1);
        let _u1 = limiter.try_acquire_unauth().unwrap();
        assert_eq!(limiter.unauth_used(), 1);
    }

    #[test]
    fn limiter_drop_decrements_used() {
        let limiter = ConnectionLimiter::new(2, 1);
        {
            let _t = limiter.try_acquire_total().unwrap();
            let _u = limiter.try_acquire_unauth().unwrap();
            assert_eq!(limiter.total_used(), 1);
            assert_eq!(limiter.unauth_used(), 1);
        }
        assert_eq!(limiter.total_used(), 0);
        assert_eq!(limiter.unauth_used(), 0);
    }

    #[test]
    fn limiter_total_limit_rejected() {
        let limiter = ConnectionLimiter::new(1, 1);
        let _t1 = limiter.try_acquire_total().unwrap();
        assert!(limiter.try_acquire_total().is_none());
        // unauth 仍可获取（独立 semaphore）
        assert!(limiter.try_acquire_unauth().is_some());
    }

    #[test]
    fn limiter_unauth_limit_rejected() {
        let limiter = ConnectionLimiter::new(10, 1);
        let _u1 = limiter.try_acquire_unauth().unwrap();
        assert!(limiter.try_acquire_unauth().is_none());
        // total 仍可获取
        assert!(limiter.try_acquire_total().is_some());
    }

    #[test]
    fn limiter_zero_permits_rejects_all() {
        let limiter = ConnectionLimiter::new(0, 0);
        assert!(limiter.try_acquire_total().is_none());
        assert!(limiter.try_acquire_unauth().is_none());
        assert_eq!(limiter.total_used(), 0);
        assert_eq!(limiter.unauth_used(), 0);
    }
}