use std::net::SocketAddr;

#[test]
fn default_config_uses_port_8443() {
    let config = webpad::config::Config::default();
    assert_eq!(config.port, 8443);
}

#[test]
fn default_config_has_http_redirect_port_8080() {
    let config = webpad::config::Config::default();
    assert_eq!(config.http_redirect_port, 8080);
}

#[test]
fn default_config_has_empty_password() {
    let config = webpad::config::Config::default();
    assert!(config.password.is_empty());
    assert!(config.password_hash().is_empty());
}

#[test]
fn config_display_shows_port_and_auth_status() {
    let config = webpad::config::Config::default();
    let display = format!("{}", config);
    assert!(display.contains("8443"));
}

#[test]
fn config_listen_addr_uses_0_0_0_0() {
    let config = webpad::config::Config::default();
    let addr = config.listen_addr();
    assert_eq!(addr, SocketAddr::from(([0, 0, 0, 0], 8443)));
}

#[test]
fn generate_password_returns_non_empty_string() {
    let password = webpad::config::generate_password(16);
    assert_eq!(password.len(), 16);
    assert!(!password.is_empty());
}

#[test]
fn generate_password_different_each_call() {
    let p1 = webpad::config::generate_password(16);
    let p2 = webpad::config::generate_password(16);
    assert_ne!(p1, p2);
}

#[test]
fn hash_password_verify_matches() {
    let hash = webpad::password::hash_password("test123");
    assert!(webpad::password::verify_password("test123", &hash));
    assert!(!webpad::password::verify_password("wrong", &hash));
}

#[test]
fn hash_password_different_inputs_different_hashes() {
    let hash1 = webpad::password::hash_password("password1");
    let hash2 = webpad::password::hash_password("password2");
    assert!(webpad::password::verify_password("password1", &hash1));
    assert!(!webpad::password::verify_password("password2", &hash1));
    assert!(webpad::password::verify_password("password2", &hash2));
}

#[test]
fn hash_password_not_plaintext() {
    let hash = webpad::password::hash_password("mysecret");
    assert_ne!(hash, "mysecret");
}

#[test]
fn default_heartbeat_timeout_is_30_seconds() {
    let config = webpad::config::Config::default();
    assert_eq!(config.heartbeat_timeout_secs, 30);
}

#[test]
fn heartbeat_timeout_returns_correct_duration() {
    let config = webpad::config::Config {
        heartbeat_timeout_secs: 15,
        ..Default::default()
    };
    assert_eq!(config.heartbeat_timeout(), std::time::Duration::from_secs(15));
}

#[test]
fn config_display_shows_heartbeat() {
    let config = webpad::config::Config::default();
    let display = format!("{}", config);
    assert!(display.contains("30s"));
}