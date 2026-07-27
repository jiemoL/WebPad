use serde::{Deserialize, Serialize};

/// 客户端（手机）-> 服务端（电脑）的消息
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// 认证请求
    AuthRequest {
        password: String,
    },
    /// 手柄状态更新
    GamepadState {
        buttons: u16,
        left_trigger: u8,
        right_trigger: u8,
        thumb_lx: i16,
        thumb_ly: i16,
        thumb_rx: i16,
        thumb_ry: i16,
    },
    /// 心跳
    Heartbeat,
    /// 断开连接
    Disconnect {
        reason: String,
    },
}

/// 服务端（电脑）-> 客户端（手机）的消息
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// 认证成功
    AuthSuccess {
        token: String,
    },
    /// 认证失败
    AuthFailure {
        reason: String,
    },
    /// 手柄状态确认（可选回显）
    GamepadState {
        buttons: u16,
        left_trigger: u8,
        right_trigger: u8,
        thumb_lx: i16,
        thumb_ly: i16,
        thumb_rx: i16,
        thumb_ry: i16,
    },
    /// 手柄震动反馈（由 ViGEmBus 驱动通知触发，转发给客户端）
    Rumble {
        left_motor: u8,
        right_motor: u8,
    },
    /// 心跳响应
    Pong,
    /// 连接信息
    Connected {
        controller_name: String,
    },
}