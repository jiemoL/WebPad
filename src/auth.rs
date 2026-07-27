use rand::Rng;
use std::collections::{HashMap, HashSet};
use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::password;

/// 认证管理器
#[derive(Clone)]
pub struct AuthManager {
    /// 密码哈希
    password_hash: String,
    /// 活跃的 session token 集合（std::sync::Mutex，锁持有极短）
    sessions: Arc<Mutex<HashSet<String>>>,
}

impl AuthManager {
    /// 创建认证管理器（需要密码哈希）
    pub fn new(password_hash: String) -> Self {
        Self {
            password_hash,
            sessions: Arc::new(Mutex::new(HashSet::new())),
        }
    }

    /// 验证密码
    ///
    /// 支持旧格式兼容（SHA-256）和新格式（Argon2id）。
    /// 若 `password_hash` 为空（未配置密码），所有认证均失败（fail-closed）。
    pub fn verify_password(&self, password: &str) -> bool {
        password::verify_password(password, &self.password_hash)
    }

    /// 创建新的 session token
    pub async fn create_session(&self) -> String {
        let token = generate_token();
        self.sessions.lock().unwrap().insert(token.clone());
        token
    }

    /// 验证 token 是否有效
    pub async fn validate_token(&self, token: &str) -> bool {
        if token.is_empty() {
            return false;
        }
        self.sessions.lock().unwrap().contains(token)
    }

    /// 移除 session
    pub async fn remove_session(&self, token: &str) {
        self.sessions.lock().unwrap().remove(token);
    }

    /// 当前活跃 session 数量
    pub fn session_count(&self) -> usize {
        self.sessions.lock().unwrap().len()
    }
}

fn generate_token() -> String {
    let mut rng = rand::thread_rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.gen()).collect();
    hex::encode(bytes)
}

/// IP 级别的认证退避管理器
///
/// 防止暴力破解攻击：同一 IP 连续认证失败后，该 IP 所有连接都必须等待退避时间。
/// 这比连接级别的退避更有效，因为攻击者无法通过建立大量连接来绕过退避。
#[derive(Clone)]
pub struct IpBackoffManager {
    inner: Arc<Mutex<IpBackoffInner>>,
    base_ms: u64,
    max_ms: u64,
    max_failures: u32,
}

struct IpBackoffInner {
    ip_failures: HashMap<IpAddr, IpFailureState>,
}

struct IpFailureState {
    failures: u32,
    last_failure: Instant,
    backoff_until: Instant,
}

impl IpBackoffManager {
    pub fn new(base_ms: u64, max_ms: u64, max_failures: u32) -> Self {
        Self {
            inner: Arc::new(Mutex::new(IpBackoffInner {
                ip_failures: HashMap::new(),
            })),
            base_ms,
            max_ms,
            max_failures,
        }
    }

    /// 检查 IP 当前是否在退避期内
    ///
    /// 返回需要等待的剩余时间，如果不在退避期则返回 None。
    pub fn check_backoff(&self, ip: IpAddr) -> Option<Duration> {
        let inner = self.inner.lock().unwrap();
        if let Some(state) = inner.ip_failures.get(&ip) {
            let now = Instant::now();
            if state.backoff_until > now {
                return Some(state.backoff_until.saturating_duration_since(now));
            }
        }
        None
    }

    /// 记录一次认证失败并更新退避时间
    ///
    /// 返回更新后的退避持续时间。
    pub fn record_failure(&self, ip: IpAddr) -> Duration {
        let mut inner = self.inner.lock().unwrap();
        let now = Instant::now();
        let state = inner.ip_failures.entry(ip).or_insert_with(|| IpFailureState {
            failures: 0,
            last_failure: now,
            backoff_until: now,
        });

        state.failures += 1;
        state.last_failure = now;

        // 指数退避：base * 2^(failures-1)，不超过最大值
        let backoff_ms = std::cmp::min(
            self.base_ms.saturating_mul(2u64.saturating_pow(state.failures.saturating_sub(1))),
            self.max_ms,
        );
        let backoff = Duration::from_millis(backoff_ms);
        state.backoff_until = now + backoff;

        // 清理过期的条目（防止内存泄漏）
        inner.ip_failures.retain(|_, s| {
            s.backoff_until > now || s.last_failure.elapsed() < Duration::from_secs(300)
        });

        backoff
    }

    /// 认证成功时重置 IP 的失败计数
    pub fn reset_ip(&self, ip: IpAddr) {
        let mut inner = self.inner.lock().unwrap();
        inner.ip_failures.remove(&ip);
    }

    /// 检查 IP 是否超过最大失败次数（应该被断开）
    pub fn should_disconnect(&self, ip: IpAddr) -> bool {
        let inner = self.inner.lock().unwrap();
        inner
            .ip_failures
            .get(&ip)
            .map(|s| s.failures >= self.max_failures)
            .unwrap_or(false)
    }
}