use std::net::SocketAddr;
use std::sync::Arc;

use clap::Parser;
use rustls::ServerConfig;
use rustls_pemfile::{certs, private_key};
use tokio::signal;
use tokio::sync::broadcast;
use tokio_rustls::TlsAcceptor;
use tracing::{error, info, warn};

use webpad::auth::{AuthManager, IpBackoffManager};
use webpad::config::Config;
use webpad::upnp::PortMapper;
use webpad::web::{create_router, run_server, AppState, ConnectionLimiter, GamepadHandle};

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
        match webpad::gamepad::GamepadManager::new().await {
            Ok(manager) => GamepadHandle::new(Some(Arc::new(manager))),
            Err(e) => {
                warn!("ViGEmBus driver not available: {}", e);
                print_vigembus_install_guide();
                GamepadHandle::new(None::<Arc<webpad::gamepad::GamepadManager>>)
            }
        }
    };
    #[cfg(not(windows))]
    let gamepad = {
        info!("Gamepad manager not available on non-Windows");
        GamepadHandle::new(None::<Arc<()>>)
    };

    // 创建 UPnP 端口映射器
    let upnp = Arc::new(PortMapper::new(config.port).await);

    // 构建 TLS 配置（必须在 config 被 move 到 Arc 之前）
    let tls_acceptor = if config.enable_tls {
        build_tls_acceptor(&config).await
    } else {
        None
    };

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

    if tls_acceptor.is_some() {
        println!("WebPad listening on https://{}", addr);
    } else {
        println!("WebPad listening on http://{}", addr);
        warn!("TLS is disabled! Passwords will be transmitted in plaintext.");
    }

    // 尝试 UPnP 端口映射
    if enable_upnp {
        info!("Probing UPnP gateway...");
        match upnp.probe().await {
            Ok(output) => {
                if output.upnp {
                    info!("UPnP gateway found, requesting port mapping...");
                    upnp.procure_mapping();
                    if let Some(ext_addr) = upnp.current_external_address() {
                        let scheme = if tls_acceptor.is_some() { "https" } else { "http" };
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

    // 优雅关闭通道
    let (shutdown_tx, shutdown_rx) = broadcast::channel::<()>(1);

    let shutdown_upnp = upnp.clone();

    tokio::select! {
        result = run_server(addr, app, tls_acceptor, enable_upnp, shutdown_rx) => {
            if let Err(e) = result {
                error!("Server error: {}", e);
            }
        }
        _ = signal::ctrl_c() => {
            info!("Received Ctrl-C, shutting down gracefully...");
            let _ = shutdown_tx.send(());
            shutdown_upnp.deactivate();
            info!("UPnP mapping deactivated");
            println!("\nWebPad stopped.");
        }
    }
}

/// 构建 TLS Acceptor
///
/// 优先使用用户配置的证书文件；如果未配置，则自动生成自签名证书。
/// 返回 None 表示 TLS 禁用。
async fn build_tls_acceptor(config: &Config) -> Option<TlsAcceptor> {
    if let (Some(cert_path), Some(key_path)) = (&config.cert_path, &config.key_path) {
        let cert_chain = match load_certs(cert_path) {
            Ok(certs) => certs,
            Err(e) => {
                error!("Failed to load TLS certificates from {}: {}", cert_path, e);
                return None;
            }
        };
        let key = match load_key(key_path) {
            Ok(key) => key,
            Err(e) => {
                error!("Failed to load TLS private key from {}: {}", key_path, e);
                return None;
            }
        };
        match create_tls_acceptor(cert_chain, key) {
            Ok(acceptor) => {
                info!("TLS enabled with custom certificate: {}", cert_path);
                return Some(acceptor);
            }
            Err(e) => {
                error!("Failed to create TLS acceptor: {}", e);
                return None;
            }
        }
    }

    // 生成自签名证书（内存中构建，不写入磁盘）
    info!("Generating self-signed TLS certificate...");
    match generate_self_signed_cert() {
        Ok((cert_pem, key_pem)) => {
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

            match create_tls_acceptor(cert_chain, key) {
                Ok(acceptor) => {
                    info!("TLS enabled with self-signed certificate");
                    println!("Note: Using self-signed certificate. Browsers will show a security warning.");
                    println!("      For production use, configure cert_path and key_path in webpad.toml");
                    Some(acceptor)
                }
                Err(e) => {
                    error!("Failed to create TLS acceptor: {}", e);
                    None
                }
            }
        }
        Err(e) => {
            error!("Failed to generate self-signed certificate: {}", e);
            None
        }
    }
}

fn create_tls_acceptor(
    cert_chain: Vec<rustls::pki_types::CertificateDer<'static>>,
    key: rustls::pki_types::PrivateKeyDer<'static>,
) -> Result<TlsAcceptor, Box<dyn std::error::Error + Send + Sync>> {
    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)?;
    Ok(TlsAcceptor::from(Arc::new(server_config)))
}

fn load_certs(path: &str) -> Result<Vec<rustls::pki_types::CertificateDer<'static>>, Box<dyn std::error::Error + Send + Sync>> {
    let cert_data = std::fs::read(path)?;
    let mut cursor = std::io::Cursor::new(cert_data);
    let certs: Result<Vec<_>, _> = certs(&mut cursor).collect();
    Ok(certs?)
}

fn load_key(path: &str) -> Result<rustls::pki_types::PrivateKeyDer<'static>, Box<dyn std::error::Error + Send + Sync>> {
    let key_data = std::fs::read(path)?;
    let mut cursor = std::io::Cursor::new(key_data);
    match private_key(&mut cursor)? {
        Some(key) => Ok(key),
        None => Err("No private key found".into()),
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

/// 输出 ViGEmBus 驱动安装引导
#[cfg(windows)]
fn print_vigembus_install_guide() {
    println!();
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║  ViGEmBus 驱动未安装 — 虚拟手柄功能不可用                    ║");
    println!("╠══════════════════════════════════════════════════════════════╣");
    println!("║  WebPad 需要 ViGEmBus 驱动来创建虚拟 Xbox 360 手柄。         ║");
    println!("║  服务仍可启动，但手机端连接后无法模拟手柄输入。               ║");
    println!("║                                                              ║");
    println!("║  安装方式（任选其一）：                                       ║");
    println!("║  1. 下载安装器：                                              ║");
    println!("║     https://github.com/nefarius/ViGEmBus/releases            ║");
    println!("║  2. 使用 winget 安装：                                       ║");
    println!("║     winget install Nefarius.ViGEmBus                         ║");
    println!("║                                                              ║");
    println!("║  安装后重启 WebPad 即可启用虚拟手柄功能。                     ║");
    println!("╚══════════════════════════════════════════════════════════════╝");
    println!();

    // 检查可执行文件同目录下是否有捆绑的 ViGEmBus 安装器
    if let Some(exe_dir) = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.to_path_buf())) {
        for name in &["ViGEmBus_Setup.exe", "ViGEmBus_1.21.411_x64.msi", "ViGEmBus.msi"] {
            let installer = exe_dir.join(name);
            if installer.exists() {
                println!("  检测到捆绑安装器：{}", installer.display());
                println!("  请运行上述安装器完成驱动安装，然后重启 WebPad。");
                println!();
                break;
            }
        }
    }
}
