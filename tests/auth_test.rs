use webpad::auth::AuthManager;

#[tokio::test]
async fn create_token_returns_non_empty_string() {
    let manager = AuthManager::new("test-secret-hash".to_string());
    let token = manager.create_session().await;
    assert!(!token.is_empty());
}

#[tokio::test]
async fn validate_valid_token_returns_true() {
    let manager = AuthManager::new("test-secret-hash".to_string());
    let token = manager.create_session().await;
    assert!(manager.validate_token(&token).await);
}

#[tokio::test]
async fn validate_invalid_token_returns_false() {
    let manager = AuthManager::new("test-secret-hash".to_string());
    assert!(!manager.validate_token("nonexistent-token").await);
}

#[tokio::test]
async fn validate_empty_token_returns_false() {
    let manager = AuthManager::new("test-secret-hash".to_string());
    assert!(!manager.validate_token("").await);
}

#[tokio::test]
async fn removed_token_is_invalid() {
    let manager = AuthManager::new("test-secret-hash".to_string());
    let token = manager.create_session().await;
    manager.remove_session(&token).await;
    assert!(!manager.validate_token(&token).await);
}

#[tokio::test]
async fn verify_password_matches() {
    let hash = webpad::password::hash_password("test-secret-password");
    let manager = AuthManager::new(hash);
    assert!(manager.verify_password("test-secret-password"));
}

#[tokio::test]
async fn verify_password_wrong() {
    let manager = AuthManager::new("test-secret-hash".to_string());
    assert!(!manager.verify_password("wrong-password"));
}

#[tokio::test]
async fn different_tokens_are_unique() {
    let manager = AuthManager::new("hash".to_string());
    let t1 = manager.create_session().await;
    let t2 = manager.create_session().await;
    assert_ne!(t1, t2);
}

#[tokio::test]
async fn session_count_tracks_correctly() {
    let manager = AuthManager::new("hash".to_string());
    assert_eq!(manager.session_count(), 0);
    let _t1 = manager.create_session().await;
    assert_eq!(manager.session_count(), 1);
    let _t2 = manager.create_session().await;
    assert_eq!(manager.session_count(), 2);
    manager.remove_session(&_t1).await;
    assert_eq!(manager.session_count(), 1);
}