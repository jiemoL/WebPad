use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use sha2::{Sha256, Digest};
use std::fmt;

const ARGON2_PREFIX: &str = "$argon2id$";

/// 密码哈希错误
#[derive(Debug)]
pub enum PasswordError {
    InvalidFormat,
    HashFailed,
    VerifyFailed,
}

impl fmt::Display for PasswordError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PasswordError::InvalidFormat => write!(f, "invalid password hash format"),
            PasswordError::HashFailed => write!(f, "failed to hash password"),
            PasswordError::VerifyFailed => write!(f, "password verification failed"),
        }
    }
}

impl std::error::Error for PasswordError {}

/// 计算密码的哈希值
///
/// 始终使用 Argon2id 算法生成安全哈希。
/// 返回值格式为 Argon2 标准格式字符串（`$argon2id$...`）。
/// 若 Argon2 哈希失败则直接 panic（表示系统级严重错误，不应降级到弱哈希）。
pub fn hash_password(password: &str) -> String {
    if password.is_empty() {
        return String::new();
    }
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    argon2
        .hash_password(password.as_bytes(), &salt)
        .expect("Argon2 password hashing failed")
        .to_string()
}

/// 验证密码是否匹配哈希值
///
/// 支持两种格式的哈希：
/// 1. **新格式**：Argon2id 格式（以 `$argon2id$` 开头）
/// 2. **旧格式**：纯 SHA-256 hex 字符串（64 字符）
///
/// 空哈希值表示无密码模式，所有验证均返回 false（fail-closed）。
pub fn verify_password(password: &str, hash: &str) -> bool {
    if hash.is_empty() {
        return false;
    }
    if hash.starts_with(ARGON2_PREFIX) {
        verify_argon2(password, hash)
    } else {
        verify_sha256(password, hash)
    }
}

/// 判断哈希值是否为旧格式（纯 SHA-256 hex）
///
/// 用于检测是否需要迁移到新格式。
pub fn is_old_format(hash: &str) -> bool {
    !hash.is_empty() && !hash.starts_with(ARGON2_PREFIX) && hash.len() == 64
}

fn verify_argon2(password: &str, hash: &str) -> bool {
    let parsed_hash = PasswordHash::new(hash);
    if parsed_hash.is_err() {
        return false;
    }
    let parsed_hash = parsed_hash.unwrap();
    Argon2::default()
        .verify_password(password.as_bytes(), &parsed_hash)
        .is_ok()
}

fn verify_sha256(password: &str, hash: &str) -> bool {
    hash_sha256(password) == hash
}

fn hash_sha256(input: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hash_empty_password_returns_empty() {
        assert_eq!(hash_password(""), "");
    }

    #[test]
    fn hash_non_empty_password_returns_argon2_format() {
        let hash = hash_password("test_password");
        assert!(hash.starts_with(ARGON2_PREFIX));
        assert!(!hash.is_empty());
    }

    #[test]
    fn verify_argon2_hash_matches() {
        let password = "test_password_123";
        let hash = hash_password(password);
        assert!(verify_password(password, &hash));
    }

    #[test]
    fn verify_argon2_hash_wrong_password() {
        let hash = hash_password("correct_password");
        assert!(!verify_password("wrong_password", &hash));
    }

    #[test]
    fn verify_sha256_hash_backwards_compatible() {
        let password = "test_password";
        let sha256_hash = hash_sha256(password);
        assert!(verify_password(password, &sha256_hash));
        assert!(!verify_password("wrong", &sha256_hash));
    }

    #[test]
    fn is_old_format_detects_sha256() {
        let sha256_hash = hash_sha256("test");
        assert!(is_old_format(&sha256_hash));
    }

    #[test]
    fn is_old_format_false_for_argon2() {
        let argon2_hash = hash_password("test");
        assert!(!is_old_format(&argon2_hash));
    }

    #[test]
    fn is_old_format_false_for_empty() {
        assert!(!is_old_format(""));
    }

    #[test]
    fn empty_hash_is_fail_closed() {
        assert!(!verify_password("anything", ""));
        assert!(!verify_password("", ""));
    }

    #[test]
    fn hash_is_deterministic_for_same_input() {
        let hash1 = hash_sha256("hello");
        let hash2 = hash_sha256("hello");
        assert_eq!(hash1, hash2);
    }

    #[test]
    fn hash_is_not_plaintext() {
        let hash = hash_password("hello");
        assert_ne!(hash, "hello");
    }

    #[test]
    fn different_inputs_different_hashes() {
        let hash1 = hash_sha256("hello");
        let hash2 = hash_sha256("world");
        assert_ne!(hash1, hash2);
    }
}