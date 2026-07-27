use crate::protocol::ClientMessage;

/// 震动事件（来自 ViGEmBus 驱动通知）
///
/// `large_motor` 对应 XInput 左马达（低频/重轰鸣），映射到协议的 `left_motor`；
/// `small_motor` 对应 XInput 右马达（高频/尖锐），映射到协议的 `right_motor`。
#[derive(Debug, Clone, Copy, Default)]
pub struct RumbleEvent {
    pub large_motor: u8,
    pub small_motor: u8,
}

/// 手柄状态（业务层类型，与协议层解耦）
#[derive(Debug, Clone, Default)]
pub struct GamepadState {
    pub buttons: u16,
    pub left_trigger: u8,
    pub right_trigger: u8,
    pub thumb_lx: i16,
    pub thumb_ly: i16,
    pub thumb_rx: i16,
    pub thumb_ry: i16,
}

impl GamepadState {
    /// 从协议消息转换
    pub fn from_client_message(msg: &ClientMessage) -> Option<Self> {
        match msg {
            ClientMessage::GamepadState {
                buttons,
                left_trigger,
                right_trigger,
                thumb_lx,
                thumb_ly,
                thumb_rx,
                thumb_ry,
            } => Some(Self {
                buttons: *buttons,
                left_trigger: *left_trigger,
                right_trigger: *right_trigger,
                thumb_lx: *thumb_lx,
                thumb_ly: *thumb_ly,
                thumb_rx: *thumb_rx,
                thumb_ry: *thumb_ry,
            }),
            _ => None,
        }
    }

    /// 判断是否为全零状态（无输入）
    pub fn is_zero(&self) -> bool {
        self.buttons == 0
            && self.left_trigger == 0
            && self.right_trigger == 0
            && self.thumb_lx == 0
            && self.thumb_ly == 0
            && self.thumb_rx == 0
            && self.thumb_ry == 0
    }

    /// 合并更新（用新状态完全覆盖）
    pub fn merge(&self, other: &Self) -> Self {
        Self {
            buttons: other.buttons,
            left_trigger: other.left_trigger,
            right_trigger: other.right_trigger,
            thumb_lx: other.thumb_lx,
            thumb_ly: other.thumb_ly,
            thumb_rx: other.thumb_rx,
            thumb_ry: other.thumb_ry,
        }
    }
}