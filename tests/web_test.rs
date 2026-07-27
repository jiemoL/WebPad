use std::sync::Arc;

use tower::ServiceExt;

use webpad::auth::{AuthManager, IpBackoffManager};
use webpad::config::Config;
use webpad::upnp::PortMapper;
use webpad::web::{AppState, ConnectionLimiter, GamepadHandle};

/// 创建一个测试用的 AppState
async fn create_test_state() -> Arc<AppState> {
    let config = Arc::new(Config::default());
    let auth = Arc::new(AuthManager::new("".to_string()));
    let upnp = Arc::new(PortMapper::new(0).await);
    let gamepad = GamepadHandle::new(None);
    let connection_limiter = Arc::new(ConnectionLimiter::new(
        config.max_connections,
        config.max_unauth_connections,
    ));
    let ip_backoff = Arc::new(IpBackoffManager::new(
        config.auth_backoff_base_ms,
        config.auth_backoff_max_ms,
        config.max_auth_failures,
    ));

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
async fn index_returns_200() {
    let state = create_test_state().await;
    let router = webpad::web::create_router(state);

    let response = router
        .oneshot(
            axum::http::Request::builder()
                .uri("/")
                .method("GET")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
}

#[tokio::test]
async fn index_returns_html() {
    let state = create_test_state().await;
    let router = webpad::web::create_router(state);

    let response = router
        .oneshot(
            axum::http::Request::builder()
                .uri("/")
                .method("GET")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    let content_type = response.headers().get("content-type").unwrap();
    assert!(content_type.to_str().unwrap().contains("text/html"));
}

#[tokio::test]
async fn unknown_route_returns_404() {
    let state = create_test_state().await;
    let router = webpad::web::create_router(state);

    let response = router
        .oneshot(
            axum::http::Request::builder()
                .uri("/nonexistent")
                .method("GET")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 404);
}

#[tokio::test]
#[ignore = "requires full WebSocket connection (not simulable with oneshot); auth is now deferred to WS message phase"]
async fn ws_without_token_returns_101() {
    let config = Arc::new(Config::default());
    let auth = Arc::new(AuthManager::new("test_password".to_string()));
    let upnp = Arc::new(PortMapper::new(0).await);
    let gamepad = GamepadHandle::new(None);
    let connection_limiter = Arc::new(ConnectionLimiter::new(
        config.max_connections,
        config.max_unauth_connections,
    ));
    let ip_backoff = Arc::new(IpBackoffManager::new(
        config.auth_backoff_base_ms,
        config.auth_backoff_max_ms,
        config.max_auth_failures,
    ));

    let state = Arc::new(AppState {
        auth,
        gamepad,
        upnp,
        config,
        connection_limiter,
        ip_backoff,
    });

    let router = webpad::web::create_router(state);

    let response = router
        .oneshot(
            axum::http::Request::builder()
                .uri("/ws")
                .method("GET")
                .header("Upgrade", "websocket")
                .header("Connection", "Upgrade")
                .header("Sec-WebSocket-Version", "13")
                .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // 现在始终允许升级，认证延迟到 WebSocket 消息阶段
    assert_eq!(response.status(), 101);
}

#[tokio::test]
#[ignore = "requires full WebSocket connection (not simulable with oneshot)"]
async fn ws_with_valid_token_returns_101() {
    let config = Arc::new(Config::default());
    let auth = Arc::new(AuthManager::new("test_password".to_string()));
    let token = auth.create_session().await;
    let upnp = Arc::new(PortMapper::new(0).await);
    let gamepad = GamepadHandle::new(None);
    let connection_limiter = Arc::new(ConnectionLimiter::new(
        config.max_connections,
        config.max_unauth_connections,
    ));
    let ip_backoff = Arc::new(IpBackoffManager::new(
        config.auth_backoff_base_ms,
        config.auth_backoff_max_ms,
        config.max_auth_failures,
    ));

    let state = Arc::new(AppState {
        auth,
        gamepad,
        upnp,
        config,
        connection_limiter,
        ip_backoff,
    });

    let router = webpad::web::create_router(state);

    let response = router
        .oneshot(
            axum::http::Request::builder()
                .uri(format!("/ws?token={}", token))
                .method("GET")
                .header("Upgrade", "websocket")
                .header("Connection", "Upgrade")
                .header("Sec-WebSocket-Version", "13")
                .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), 101);
}

#[tokio::test]
#[ignore = "requires full WebSocket connection (not simulable with oneshot)"]
async fn ws_with_wrong_token_returns_101() {
    let config = Arc::new(Config::default());
    let auth = Arc::new(AuthManager::new("test_password".to_string()));
    let upnp = Arc::new(PortMapper::new(0).await);
    let gamepad = GamepadHandle::new(None);
    let connection_limiter = Arc::new(ConnectionLimiter::new(
        config.max_connections,
        config.max_unauth_connections,
    ));
    let ip_backoff = Arc::new(IpBackoffManager::new(
        config.auth_backoff_base_ms,
        config.auth_backoff_max_ms,
        config.max_auth_failures,
    ));

    let state = Arc::new(AppState {
        auth,
        gamepad,
        upnp,
        config,
        connection_limiter,
        ip_backoff,
    });

    let router = webpad::web::create_router(state);

    let response = router
        .oneshot(
            axum::http::Request::builder()
                .uri("/ws?token=invalid_token_123")
                .method("GET")
                .header("Upgrade", "websocket")
                .header("Connection", "Upgrade")
                .header("Sec-WebSocket-Version", "13")
                .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    // 无效 token 不再拒绝升级，认证延迟到 WebSocket 消息阶段
    assert_eq!(response.status(), 101);
}