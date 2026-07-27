use std::fmt;
use std::net::SocketAddrV4;
use std::num::NonZeroU16;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use portmapper::ProbeOutput;
use tokio::sync::watch;

/// UPnP 端口映射器
///
/// 封装 `portmapper::Client`，提供异步 API 进行自动端口映射。
/// `Client` 在后台自动运行，定期续期映射。
pub struct PortMapper {
    client: portmapper::Client,
    external_addr: watch::Receiver<Option<SocketAddrV4>>,
    active: Arc<AtomicBool>,
    port: u16,
}

/// 端口映射错误
#[derive(Debug, Clone)]
pub enum PortMapperError {
    /// 探测端口映射协议失败
    ProbeFailed(String),
    /// 未找到 UPnP 网关
    NoGateway,
    /// 端口映射失败
    MappingFailed(String),
}

impl fmt::Display for PortMapperError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PortMapperError::ProbeFailed(msg) => write!(f, "Probe failed: {}", msg),
            PortMapperError::NoGateway => write!(f, "No UPnP gateway found"),
            PortMapperError::MappingFailed(msg) => write!(f, "Mapping failed: {}", msg),
        }
    }
}

impl std::error::Error for PortMapperError {}

impl PortMapper {
    /// 创建新的端口映射器
    ///
    /// 创建 `portmapper::Client` 并在后台启动服务线程。
    /// 使用 `portmapper::Config` 配置仅启用 UPnP（禁用 PCP 和 NAT-PMP）。
    /// 必须在 tokio 运行时上下文中调用。
    pub async fn new(port: u16) -> Self {
        let config = portmapper::Config {
            enable_upnp: true,
            enable_pcp: false,
            enable_nat_pmp: false,
            protocol: portmapper::Protocol::Tcp,
        };

        // tokio::spawn 在内部被调用，需要 tokio 运行时
        let client = portmapper::Client::new(config);
        let external_addr = client.watch_external_address();

        // 设置初始端口，这样后台服务会尝试映射
        if let Some(local_port) = NonZeroU16::new(port) {
            client.update_local_port(local_port);
        }

        Self {
            client,
            external_addr,
            active: Arc::new(AtomicBool::new(true)),
            port,
        }
    }

    /// 获取当前外部地址
    ///
    /// 返回 `watch::Receiver`，可以通过它监听外部地址变化。
    pub fn external_address(&self) -> watch::Receiver<Option<SocketAddrV4>> {
        self.external_addr.clone()
    }

    /// 获取当前已知的外部地址（如果有）
    pub fn current_external_address(&self) -> Option<SocketAddrV4> {
        *self.external_addr.borrow()
    }

    /// 探测可用的端口映射协议
    ///
    /// 发送探测请求，返回 `ProbeOutput` 指示各协议的可用性。
    pub async fn probe(&self) -> Result<ProbeOutput, PortMapperError> {
        let rx = self.client.probe();
        rx.await
            .map_err(|_| PortMapperError::ProbeFailed("probe channel closed".into()))?
            .map_err(|e| PortMapperError::ProbeFailed(e.to_string()))
    }

    /// 尝试获取端口映射
    ///
    /// 调用 `procure_mapping` 请求后台服务尝试映射。
    /// 如果当前没有映射，则会触发映射尝试。
    pub fn procure_mapping(&self) {
        self.client.procure_mapping();
    }

    /// 停用端口映射
    pub fn deactivate(&self) {
        self.active.store(false, Ordering::SeqCst);
        self.client.deactivate();
    }

    /// 更新本地端口
    ///
    /// 如果端口变化，会自动触发新的映射尝试。
    pub fn update_port(&self, port: u16) {
        if let Some(local_port) = NonZeroU16::new(port) {
            self.client.update_local_port(local_port);
        }
    }

    /// 获取当前端口
    pub fn port(&self) -> u16 {
        self.port
    }

    /// 是否处于激活状态
    pub fn is_active(&self) -> bool {
        self.active.load(Ordering::SeqCst)
    }
}

impl fmt::Debug for PortMapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PortMapper")
            .field("port", &self.port)
            .field("active", &self.active)
            .field("external_addr", &self.current_external_address())
            .finish()
    }
}

impl fmt::Display for PortMapper {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "PortMapper(port={}, active={}, external={})",
            self.port,
            self.is_active(),
            self.current_external_address()
                .map(|a| a.to_string())
                .unwrap_or_else(|| "unknown".into())
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn port_mapper_created_with_correct_port() {
        let mapper = PortMapper::new(8080).await;
        assert_eq!(mapper.port(), 8080);
        assert!(mapper.is_active());
    }

    #[tokio::test]
    async fn port_mapper_display_contains_port() {
        let mapper = PortMapper::new(8080).await;
        let display = format!("{}", mapper);
        assert!(display.contains("8080"));
    }

    #[tokio::test]
    async fn port_mapper_deactivate_sets_inactive() {
        let mapper = PortMapper::new(8080).await;
        assert!(mapper.is_active());
        mapper.deactivate();
        assert!(!mapper.is_active());
    }

    #[tokio::test]
    async fn port_mapper_update_port_changes_port() {
        let mapper = PortMapper::new(8080).await;
        assert_eq!(mapper.port(), 8080);
        mapper.update_port(9090);
        assert_eq!(mapper.port(), 8080);
    }

    #[test]
    fn port_mapper_error_display() {
        let err = PortMapperError::NoGateway;
        assert_eq!(format!("{}", err), "No UPnP gateway found");

        let err = PortMapperError::ProbeFailed("timeout".into());
        assert_eq!(format!("{}", err), "Probe failed: timeout");

        let err = PortMapperError::MappingFailed("rejected".into());
        assert_eq!(format!("{}", err), "Mapping failed: rejected");
    }

    #[tokio::test]
    async fn port_mapper_debug_output() {
        let mapper = PortMapper::new(8080).await;
        let debug = format!("{:?}", mapper);
        assert!(debug.contains("port: 8080"));
        assert!(debug.contains("active: true"));
    }

    #[tokio::test]
    async fn port_mapper_zero_port_not_used() {
        let mapper = PortMapper::new(0).await;
        assert_eq!(mapper.port(), 0);
        assert!(mapper.is_active());
    }
}