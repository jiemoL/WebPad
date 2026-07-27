use rand::Rng;
use std::fmt;
use std::net::SocketAddr;
use std::path::PathBuf;

use crate::password;

/// 应用配置
#[derive(serde::Serialize, serde::Deserialize, Clone)]
pub struct Config {
    /// HTTPS/WSS 监听端口
    pub port: u16,
    /// HTTP 重定向端口（0 表示禁用 HTTP 重定向）
    #[serde(default)]
    pub http_redirect_port: u16,
    /// 连接密码（明文，仅用于配置文件和显示；运行时使用 password_hash）
    #[serde(default)]
    pub password: String,
    /// TLS 证书文件路径（None 表示不启用 TLS）
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cert_path: Option<String>,
    /// TLS 私钥文件路径
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_path: Option<String>,
    /// 是否启用 UPnP 端口映射
    #[serde(default)]
    pub enable_upnp: bool,
    /// 心跳超时秒数
    #[serde(default = "default_heartbeat")]
    pub heartbeat_timeout_secs: u64,
    /// 单个连接最大认证失败次数（超过后断开）
    #[serde(default = "default_max_auth_failures")]
    pub max_auth_failures: u32,
    /// 认证失败退避初始毫秒数（指数退避）
    #[serde(default = "default_auth_backoff_base_ms")]
    pub auth_backoff_base_ms: u64,
    /// 认证失败最大退避毫秒数
    #[serde(default = "default_auth_backoff_max_ms")]
    pub auth_backoff_max_ms: u64,
    /// 最大并发 WebSocket 连接数（含已认证和未认证）
    #[serde(default = "default_max_connections")]
    pub max_connections: usize,
    /// 最大未认证并发 WebSocket 连接数（用于密码认证流程）
    #[serde(default = "default_max_unauth_connections")]
    pub max_unauth_connections: usize,
}

fn default_heartbeat() -> u64 {
    30
}

fn default_max_auth_failures() -> u32 {
    5
}

fn default_auth_backoff_base_ms() -> u64 {
    500
}

fn default_auth_backoff_max_ms() -> u64 {
    10000
}

fn default_max_connections() -> usize {
    8
}

fn default_max_unauth_connections() -> usize {
    3
}

impl Default for Config {
    fn default() -> Self {
        Self {
            port: 8443,
            http_redirect_port: 8080,
            password: String::new(),
            cert_path: None,
            key_path: None,
            enable_upnp: false,
            heartbeat_timeout_secs: 30,
            max_auth_failures: 5,
            auth_backoff_base_ms: 500,
            auth_backoff_max_ms: 10000,
            max_connections: 8,
            max_unauth_connections: 3,
        }
    }
}

impl fmt::Display for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "WebPad Config:\n  HTTPS Port: {}\n  HTTP Redirect: {}\n  Auth: {}\n  UPnP: {}\n  TLS: {}\n  Heartbeat: {}s\n  Max Auth Failures: {}\n  Auth Backoff: {}ms - {}ms\n  Max Connections: {}\n  Max Unauth Connections: {}",
            self.port,
            if self.http_redirect_port == 0 { "disabled".to_string() } else { self.http_redirect_port.to_string() },
            if self.password.is_empty() { "disabled" } else { "enabled" },
            self.enable_upnp,
            self.cert_path.as_deref().unwrap_or("self-signed"),
            self.heartbeat_timeout_secs,
            self.max_auth_failures,
            self.auth_backoff_base_ms,
            self.auth_backoff_max_ms,
            self.max_connections,
            self.max_unauth_connections,
        )
    }
}

impl Config {
    /// 获取监听地址
    pub fn listen_addr(&self) -> SocketAddr {
        SocketAddr::from(([0, 0, 0, 0], self.port))
    }

    /// 密码的哈希值（使用 Argon2id）
    pub fn password_hash(&self) -> String {
        if self.password.is_empty() {
            String::new()
        } else {
            password::hash_password(&self.password)
        }
    }

    /// 验证密码
    ///
    /// 支持旧格式兼容（SHA-256）和新格式（Argon2id）。
    /// 若密码未配置（空字符串），所有认证均失败（fail-closed）。
    pub fn verify_password(&self, password: &str) -> bool {
        password::verify_password(password, &self.password_hash())
    }

    /// 心跳超时的 Duration
    pub fn heartbeat_timeout(&self) -> std::time::Duration {
        std::time::Duration::from_secs(self.heartbeat_timeout_secs.max(5))
    }

    /// 配置文件路径
    pub fn config_path() -> PathBuf {
        // 与可执行文件同目录的 webpad.toml
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()))
            .unwrap_or_else(|| PathBuf::from("."));
        exe_dir.join("webpad.toml")
    }

    /// 从配置文件加载，不存在则使用默认值并创建
    pub fn load_or_create() -> Self {
        let path = Self::config_path();
        if path.exists() {
            match std::fs::read_to_string(&path) {
                Ok(content) => match toml::from_str::<Config>(&content) {
                    Ok(mut config) => {
                        // 迁移：旧配置可能没有 password 字段但有 password_hash
                        // （当前版本只用 password，password_hash 已移除）

                        // 迁移 v1 -> v2：端口配置迁移
                        // 旧版默认 port=8080 且无 http_redirect_port 字段
                        // 新版默认 port=8443, http_redirect_port=8080
                        // 如果检测到 port=8080 且 http_redirect_port=0（新字段默认值），
                        // 说明是旧配置，自动迁移到新端口布局
                        if config.port == 8080 && config.http_redirect_port == 0 {
                            config.port = 8443;
                            config.http_redirect_port = 8080;
                            eprintln!("Config migrated: port 8080 -> 8443 (HTTPS), HTTP redirect on 8080");
                            if let Err(e) = config.save() {
                                eprintln!("Failed to save migrated config: {}", e);
                            }
                        }

                        return config;
                    }
                    Err(e) => {
                        eprintln!("Failed to parse config file: {}, using defaults", e);
                    }
                },
                Err(e) => {
                    eprintln!("Failed to read config file: {}, using defaults", e);
                }
            }
        }

        let config = Config::default();
        if let Err(e) = config.save() {
            eprintln!("Failed to create config file: {}", e);
        }
        config
    }

    /// 保存到配置文件
    pub fn save(&self) -> std::io::Result<()> {
        let path = Self::config_path();
        let content = toml::to_string_pretty(self)
            .map_err(std::io::Error::other)?;
        std::fs::write(path, content)
    }

    /// 生成密码并设置到配置中（如果密码为空）
    pub fn ensure_password(&mut self) {
        if self.password.is_empty() {
            self.password = generate_password(16);
        }
    }
}

/// 生成随机密码
pub fn generate_password(length: usize) -> String {
    const CHARSET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZabcdefghijkmnpqrstuvwxyz23456789";
    let mut rng = rand::thread_rng();
    (0..length)
        .map(|_| {
            let idx = rng.gen_range(0..CHARSET.len());
            CHARSET[idx] as char
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_empty_password() {
        let config = Config::default();
        assert!(config.password.is_empty());
        assert!(config.password_hash().is_empty());
    }

    #[test]
    fn default_config_uses_port_8443() {
        let config = Config::default();
        assert_eq!(config.port, 8443);
    }

    #[test]
    fn default_config_has_http_redirect_port_8080() {
        let config = Config::default();
        assert_eq!(config.http_redirect_port, 8080);
    }

    #[test]
    fn default_heartbeat_timeout_is_30_seconds() {
        let config = Config::default();
        assert_eq!(config.heartbeat_timeout_secs, 30);
    }

    #[test]
    fn config_listen_addr_uses_0_0_0_0() {
        let config = Config::default();
        assert_eq!(config.listen_addr().port(), 8443);
    }

    #[test]
    fn config_display_shows_port_and_auth_status() {
        let config = Config::default();
        let display = format!("{}", config);
        assert!(display.contains("8443"));
        assert!(display.contains("disabled"));
    }

    #[test]
    fn config_display_shows_heartbeat() {
        let config = Config::default();
        let display = format!("{}", config);
        assert!(display.contains("30s"));
    }

    #[test]
    fn config_display_shows_auth_enabled() {
        let mut config = Config::default();
        config.password = "test".to_string();
        let display = format!("{}", config);
        assert!(display.contains("enabled"));
    }

    #[test]
    fn hash_password_not_plaintext() {
        let hash = password::hash_password("hello");
        assert_ne!(hash, "hello");
    }

    #[test]
    fn different_inputs_different_verify_results() {
        let hash1 = password::hash_password("hello");
        let hash2 = password::hash_password("world");
        assert!(password::verify_password("hello", &hash1));
        assert!(!password::verify_password("world", &hash1));
        assert!(password::verify_password("world", &hash2));
    }

    #[test]
    fn generate_password_returns_non_empty_string() {
        let pwd = generate_password(16);
        assert!(!pwd.is_empty());
        assert_eq!(pwd.len(), 16);
    }

    #[test]
    fn generate_password_different_each_call() {
        assert_ne!(generate_password(16), generate_password(16));
    }

    #[test]
    fn password_hash_returns_argon2id() {
        let mut config = Config::default();
        config.password = "test".to_string();
        let hash = config.password_hash();
        assert!(hash.starts_with("$argon2id$"));
        assert!(!hash.is_empty());
    }

    #[test]
    fn verify_password_matches() {
        let mut config = Config::default();
        config.password = "test_password".to_string();
        assert!(config.verify_password("test_password"));
    }

    #[test]
    fn verify_password_wrong() {
        let mut config = Config::default();
        config.password = "test_password".to_string();
        assert!(!config.verify_password("wrong"));
    }

    #[test]
    fn ensure_password_generates_when_empty() {
        let mut config = Config::default();
        config.ensure_password();
        assert!(!config.password.is_empty());
        assert_eq!(config.password.len(), 16);
    }

    #[test]
    fn ensure_password_does_not_overwrite() {
        let mut config = Config::default();
        config.password = "existing".to_string();
        config.ensure_password();
        assert_eq!(config.password, "existing");
    }

    #[test]
    fn config_round_trip_serialization() {
        let mut config = Config::default();
        config.port = 9090;
        config.password = "mypass".to_string();
        config.heartbeat_timeout_secs = 60;
        config.enable_upnp = false;

        let toml_str = toml::to_string(&config).unwrap();
        let loaded: Config = toml::from_str(&toml_str).unwrap();
        assert_eq!(loaded.port, 9090);
        assert_eq!(loaded.password, "mypass");
        assert_eq!(loaded.heartbeat_timeout_secs, 60);
        assert!(!loaded.enable_upnp);
    }
}
