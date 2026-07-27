pub mod types;

#[cfg(windows)]
pub mod manager;

#[cfg(windows)]
pub use manager::GamepadManager;

/// 手柄控制器 ID 类型（所有平台可用）
pub type ControllerId = u32;