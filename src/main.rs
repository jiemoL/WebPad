use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::Request;
use axum::response::Redirect;
use axum_server::tls_rustls::RustlsConfig;
use clap::Parser;
use rustls::ServerConfig;
use rustls_pemfile::{certs, private_key};
use tokio::signal;
use tracing::{error, info, warn};

use webpad::auth::{AuthManager, IpBackoffManager};
use webpad::config::Config;
use webpad::upnp::PortMapper;
use webpad::web::{create_router, AppState, ConnectionLimiter, GamepadHandle};

/// WebPad 命令行参数
#[derive(Parser)]
#[command(name = "WebPad", version = "0.1.0", about = "Virtual gamepad simulator")]
struct Cli {
    /// 监听端口（覆盖配置文件）
    #[arg(short, long)]
    port: Option<u16>,

    /// 连接密码（覆盖配置文件，留空使用配置文件中的密码或自动生成）
    #[arg(short = 'w', long)]
    password: Option<String>,

    /// 禁用 UPnP 端口映射
    #[arg(long)]
    no_upnp: bool,

    /// 心跳超时秒数
    #[arg(long)]
    heartbeat_timeout: Option<u64>,
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt::init();

    let cli = Cli::parse();

    // 从配置文件加载（不存在则自动创建）
    let mut config = Config::load_or_create();

    // 命令行参数覆盖配置文件
    if let Some(port) = cli.port {
        config.port = port;
    }
    if let Some(timeout) = cli.heartbeat_timeout {
        config.heartbeat_timeout_secs = timeout;
    }
    if cli.no_upnp {
        config.enable_upnp = false;
    }

    // 密码处理：命令行 > 配置文件 > 自动生成
    if let Some(pwd) = cli.password {
        config.password = pwd;
    }
    config.ensure_password();

    // 保存配置（确保密码写回文件）
    if let Err(e) = config.save() {
        error!("Failed to save config: {}", e);
    }

    println!("{}", config);
    println!("\nUse the password from webpad.toml to connect from your phone.\n");

    // 创建认证管理器（使用密码哈希）
    let password_hash = config.password_hash();
    let auth = Arc::new(AuthManager::new(password_hash));

    // 创建手柄管理器（仅 Windows）
    #[cfg(windows)]
    let gamepad = {
        info!("Initializing ViGEmBus gamepad manager...");
        let manager = webpad::gamepad::GamepadManager::new().await;
        GamepadHandle::new(Some(Arc::new(manager)))
    };
    #[cfg(not(windows))]
    let gamepad = {
        info!("Gamepad manager not available on non-Windows");
        GamepadHandle::new(None::<Arc<()>>)
    };

    // 创建 UPnP 端口映射器
    let upnp = Arc::new(PortMapper::new(config.port).await);

    // 构建 TLS 配置（必须在 config 被 move 到 Arc 之前）
    let tls_config = build_tls_config(&config).await;
    let tls_enabled = tls_config.is_some();
    let http_redirect_port = config.http_redirect_port;

    // 创建共享状态
    let port = config.port;
    let enable_upnp = config.enable_upnp;
    let max_connections = config.max_connections;
    let max_unauth_connections = config.max_unauth_connections;
    let connection_limiter = Arc::new(ConnectionLimiter::new(max_connections, max_unauth_connections));
    let ip_backoff = Arc::new(IpBackoffManager::new(
        config.auth_backoff_base_ms,
        config.auth_backoff_max_ms,
        config.max_auth_failures,
    ));
    let state = Arc::new(AppState {
        auth,
        gamepad,
        upnp: upnp.clone(),
        config: Arc::new(config),
        connection_limiter,
        ip_backoff,
    });

    // 创建路由
    let app = create_router(state);

    // 启动服务器
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    if tls_enabled {
        println!("WebPad listening on https://{}", addr);
    } else {
        println!("WebPad listening on http://{}", addr);
        warn!("TLS is disabled! Passwords will be transmitted in plaintext.");
    }

    // HTTP 重定向服务器（仅在 TLS 启用且配置了重定向端口时启动）
    let redirect_handle = if tls_enabled && http_redirect_port > 0 {
        let redirect_addr = SocketAddr::from(([0, 0, 0, 0], http_redirect_port));
        let redirect_app = axum::Router::new().fallback(move |req: Request| async move {
            let host = req
                .headers()
                .get("host")
                .and_then(|h| h.to_str().ok())
                .map(|h| {
                    if let Some(colon_pos) = h.find(':') {
                        &h[..colon_pos]
                    } else {
                        h
                    }
                })
                .unwrap_or("localhost");
            let uri = req.uri();
            let path_and_query = uri.path_and_query().map(|pq| pq.as_str()).unwrap_or("/");
            let redirect_url = format!("https://{}:{}{}", host, port, path_and_query);
            Redirect::permanent(&redirect_url)
        });
        let handle = axum_server::Handle::new();
        let handle_clone = handle.clone();
        tokio::spawn(async move {
            info!("HTTP redirect server listening on http://{}", redirect_addr);
            if let Err(e) = axum_server::bind(redirect_addr)
                .handle(handle_clone)
                .serve(redirect_app.into_make_service())
                .await
            {
                error!("HTTP redirect server error: {}", e);
            }
        });
        println!("HTTP redirect enabled on port {} -> {}", http_redirect_port, port);
        Some(handle)
    } else {
        None
    };

    // 尝试 UPnP 端口映射
    if enable_upnp {
        info!("Probing UPnP gateway...");
        match upnp.probe().await {
            Ok(output) => {
                if output.upnp {
                    info!("UPnP gateway found, requesting port mapping...");
                    upnp.procure_mapping();
                    if let Some(ext_addr) = upnp.current_external_address() {
                        let scheme = if tls_enabled { "https" } else { "http" };
                        println!("External address: {}://{}", scheme, ext_addr);
                    }
                } else {
                    info!("No UPnP gateway found");
                }
            }
            Err(e) => {
                info!("UPnP probe failed: {}", e);
            }
        }
    } else {
        info!("UPnP disabled");
    }

    // 使用 axum-server 启动（支持 TLS 和优雅关闭）
    let shutdown_upnp = upnp.clone();
    let handle = axum_server::Handle::new();
    let handle_clone = handle.clone();

    tokio::select! {
        result = async {
            if let Some(tls) = tls_config {
                axum_server::bind_rustls(addr, tls)
                    .handle(handle_clone)
                    .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                    .await
            } else {
                axum_server::bind(addr)
                    .handle(handle_clone)
                    .serve(app.into_make_service_with_connect_info::<SocketAddr>())
                    .await
            }
        } => {
            if let Err(e) = result {
                error!("Server error: {}", e);
            }
        }
        _ = signal::ctrl_c() => {
            info!("Received Ctrl-C, shutting down gracefully...");
            handle.graceful_shutdown(Some(Duration::from_secs(5)));
            if let Some(redirect_h) = redirect_handle {
                redirect_h.graceful_shutdown(Some(Duration::from_secs(2)));
            }
            shutdown_upnp.deactivate();
            info!("UPnP mapping deactivated");
            println!("\nWebPad stopped.");
        }
    }
}

/// 构建 TLS 配置
///
/// 优先使用用户配置的证书文件；如果未配置，则自动生成自签名证书。
/// 返回 None 表示 TLS 禁用（当前版本始终启用 TLS）。
async fn build_tls_config(config: &Config) -> Option<RustlsConfig> {
    if let (Some(cert_path), Some(key_path)) = (&config.cert_path, &config.key_path) {
        match RustlsConfig::from_pem_file(cert_path, key_path).await {
            Ok(tls) => {
                info!("TLS enabled with custom certificate: {}", cert_path);
                return Some(tls);
            }
            Err(e) => {
                error!("Failed to load TLS certificate from {}: {}", cert_path, e);
                println!("Warning: Failed to load TLS certificate, generating self-signed certificate instead.");
            }
        }
    }

    // 生成自签名证书（内存中构建，不写入磁盘）
    info!("Generating self-signed TLS certificate...");
    match generate_self_signed_cert() {
        Ok((cert_pem, key_pem)) => {
            // 从 PEM 字符串解析证书和私钥（内存中处理，不写磁盘）
            let cert_chain: Vec<rustls::pki_types::CertificateDer<'static>> = {
                let mut cursor = std::io::Cursor::new(cert_pem.as_bytes());
                let certs: Result<Vec<_>, _> = certs(&mut cursor).collect();
                match certs {
                    Ok(certs) if !certs.is_empty() => certs,
                    Ok(_) => {
                        error!("No certificates found in generated PEM");
                        return None;
                    }
                    Err(e) => {
                        error!("Failed to parse generated certificate: {}", e);
                        return None;
                    }
                }
            };

            let key = {
                let mut cursor = std::io::Cursor::new(key_pem.as_bytes());
                match private_key(&mut cursor) {
                    Ok(Some(key)) => key,
                    Ok(None) => {
                        error!("No private key found in generated PEM");
                        return None;
                    }
                    Err(e) => {
                        error!("Failed to parse generated private key: {}", e);
                        return None;
                    }
                }
            };

            // 构建 rustls ServerConfig
            let server_config = match ServerConfig::builder()
                .with_no_client_auth()
                .with_single_cert(cert_chain, key)
            {
                Ok(cfg) => Arc::new(cfg),
                Err(e) => {
                    error!("Failed to build TLS server config: {}", e);
                    return None;
                }
            };

            info!("TLS enabled with self-signed certificate");
            println!("Note: Using self-signed certificate. Browsers will show a security warning.");
            println!("      For production use, configure cert_path and key_path in webpad.toml");
            Some(RustlsConfig::from_config(server_config))
        }
        Err(e) => {
            error!("Failed to generate self-signed certificate: {}", e);
            None
        }
    }
}

/// 生成自签名证书（PEM 格式）
fn generate_self_signed_cert() -> Result<(String, String), Box<dyn std::error::Error>> {
    let mut params = rcgen::CertificateParams::new(vec![
        "localhost".to_string(),
        "127.0.0.1".to_string(),
    ])?;
    params.distinguished_name.push(
        rcgen::DnType::CommonName,
        rcgen::DnValue::Utf8String("WebPad".to_string()),
    );
    let key_pair = rcgen::KeyPair::generate()?;
    let cert = params.self_signed(&key_pair)?;
    let cert_pem = cert.pem();
    let key_pem = key_pair.serialize_pem();
    Ok((cert_pem, key_pem))
}
