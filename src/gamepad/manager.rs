use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::task::spawn_blocking;
use vigem_client::{Client, TargetId, Xbox360Wired, XGamepad};

use super::types::{GamepadState, RumbleEvent};

/// 手柄控制器 ID 类型
pub type ControllerId = u32;

/// 手柄管理器错误
#[derive(Debug, Clone)]
pub enum GamepadError {
    /// ViGEmBus 驱动未安装或连接失败
    ConnectionFailed(String),
    /// 控制器创建失败
    CreateFailed(String),
    /// 控制器未找到
    ControllerNotFound(ControllerId),
    /// 控制器更新失败
    UpdateFailed(String),
    /// 管理器已关闭
    Shutdown,
}

impl std::fmt::Display for GamepadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            GamepadError::ConnectionFailed(msg) => write!(f, "Connection failed: {}", msg),
            GamepadError::CreateFailed(msg) => write!(f, "Create failed: {}", msg),
            GamepadError::ControllerNotFound(id) => write!(f, "Controller {} not found", id),
            GamepadError::UpdateFailed(msg) => write!(f, "Update failed: {}", msg),
            GamepadError::Shutdown => write!(f, "Manager is shut down"),
        }
    }
}

impl std::error::Error for GamepadError {}

/// 默认的 Xbox 360 控制器 TargetId（Microsoft Xbox 360 Controller）
fn default_target_id() -> TargetId {
    TargetId {
        vendor: 0x045E,
        product: 0x028E,
    }
}

/// 将业务层 GamepadState 转换为 vigem 的 XGamepad
fn to_xgamepad(state: &GamepadState) -> XGamepad {
    use vigem_client::XButtons;
    XGamepad {
        buttons: XButtons(state.buttons),
        left_trigger: state.left_trigger,
        right_trigger: state.right_trigger,
        thumb_lx: state.thumb_lx,
        thumb_ly: state.thumb_ly,
        thumb_rx: state.thumb_rx,
        thumb_ry: state.thumb_ry,
    }
}

/// 控制器条目：持有 target 和震动通知线程句柄
struct ControllerEntry {
    target: Arc<Mutex<Xbox360Wired<Arc<Client>>>>,
    notification_thread: Option<std::thread::JoinHandle<()>>,
}

/// 控制器映射表类型
type ControllerMap = Arc<Mutex<HashMap<ControllerId, ControllerEntry>>>;

/// 虚拟手柄管理器
///
/// 封装 ViGEmBus 驱动连接，提供异步接口创建/销毁/更新虚拟 Xbox 360 手柄。
/// 所有同步 ViGEm 操作通过 `spawn_blocking` 在阻塞线程池中执行。
pub struct GamepadManager {
    client: Arc<Client>,
    controllers: ControllerMap,
    next_id: AtomicUsize,
    shutdown: Arc<AtomicBool>,
}

impl GamepadManager {
    /// 创建新的手柄管理器并连接 ViGEmBus 驱动
    ///
    /// 返回 `Err(GamepadError::ConnectionFailed)` 如果驱动未安装或连接失败。
    pub async fn new() -> Result<Self, GamepadError> {
        let client = spawn_blocking(move || {
            Client::connect().map_err(|e| GamepadError::ConnectionFailed(e.to_string()))
        })
        .await
        .map_err(|_| GamepadError::ConnectionFailed("spawn_blocking join failed".into()))??;

        Ok(Self {
            client: Arc::new(client),
            controllers: Arc::new(Mutex::new(HashMap::new())),
            next_id: AtomicUsize::new(1),
            shutdown: Arc::new(AtomicBool::new(false)),
        })
    }

    /// 获取当前连接的控制器数量
    pub fn controller_count(&self) -> usize {
        self.controllers.lock().unwrap().len()
    }

    /// 创建新的虚拟手柄控制器
    ///
    /// 在 ViGEmBus 中插入一个虚拟 Xbox 360 手柄，等待其就绪后返回 ControllerId
    /// 和震动事件接收端。震动事件由独立 OS 线程从 ViGEmBus 驱动通知捕获，
    /// 通过 tokio mpsc 通道传递给调用方。
    pub async fn create_controller(
        &self,
    ) -> Result<(ControllerId, tokio::sync::mpsc::Receiver<RumbleEvent>), GamepadError> {
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(GamepadError::Shutdown);
        }

        let id = self.next_id.fetch_add(1, Ordering::SeqCst) as ControllerId;
        let client = self.client.clone();
        let target_id = default_target_id();

        let (target, notification_thread, rumble_rx) =
            spawn_blocking(move || -> Result<(Xbox360Wired<Arc<Client>>, Option<std::thread::JoinHandle<()>>, tokio::sync::mpsc::Receiver<RumbleEvent>), GamepadError> {
                let mut target = Xbox360Wired::new(client, target_id);
                target
                    .plugin()
                    .map_err(|e| GamepadError::CreateFailed(format!("plugin: {}", e)))?;
                target
                    .wait_ready()
                    .map_err(|e| GamepadError::CreateFailed(format!("wait_ready: {}", e)))?;

                // 注册震动通知：在 target 被 Arc<Mutex> 包裹前调用（需 &mut self）
                let notification = target
                    .request_notification()
                    .map_err(|e| GamepadError::CreateFailed(format!("request_notification: {}", e)))?;

                let (tx, rx) = tokio::sync::mpsc::channel::<RumbleEvent>(16);
                let thread_handle = notification.spawn_thread(move |_, data| {
                    // try_send 而非 blocking_send：通知线程是普通 OS 线程，不应阻塞；
                    // 通道满时丢弃事件（震动是瞬态信号，丢弃可接受）
                    let _ = tx.try_send(RumbleEvent {
                        large_motor: data.large_motor,
                        small_motor: data.small_motor,
                    });
                });

                Ok((target, Some(thread_handle), rx))
            })
            .await
            .map_err(|_| GamepadError::ConnectionFailed("spawn_blocking join failed".into()))??;

        let mut controllers = self.controllers.lock().unwrap();
        controllers.insert(
            id,
            ControllerEntry {
                target: Arc::new(Mutex::new(target)),
                notification_thread,
            },
        );
        Ok((id, rumble_rx))
    }

    /// 更新指定控制器的状态
    ///
    /// 将 `GamepadState` 转换为 `XGamepad` 并发送到 ViGEmBus 驱动。
    pub async fn update_state(&self, id: ControllerId, state: &GamepadState) -> Result<(), GamepadError> {
        if self.shutdown.load(Ordering::SeqCst) {
            return Err(GamepadError::Shutdown);
        }

        let controller = {
            let controllers = self.controllers.lock().unwrap();
            controllers
                .get(&id)
                .ok_or(GamepadError::ControllerNotFound(id))?
                .target
                .clone()
        };

        let xgamepad = to_xgamepad(state);

        spawn_blocking(move || {
            let mut target = controller.lock().unwrap();
            target
                .update(&xgamepad)
                .map_err(|e| GamepadError::UpdateFailed(e.to_string()))
        })
        .await
        .map_err(|_| GamepadError::ConnectionFailed("spawn_blocking join failed".into()))?
    }

    /// 销毁指定控制器
    ///
    /// 从 ViGEmBus 中移除指定手柄。先 drop target（触发 unplug，通知线程的 poll
    /// 返回 Err(OperationAborted) 退出循环），再 join 通知线程确保干净回收。
    /// 如果控制器不存在，静默忽略。
    pub async fn destroy_controller(&self, id: ControllerId) {
        let entry = {
            let mut controllers = self.controllers.lock().unwrap();
            controllers.remove(&id)
        };

        if let Some(entry) = entry {
            // 先 drop target：触发 unplug，通知线程会收到 abort 信号退出
            let target = entry.target;
            let _ = spawn_blocking(move || {
                drop(target);
            })
            .await;

            // 再 join 通知线程，确保线程回收（避免线程泄漏）
            if let Some(handle) = entry.notification_thread {
                let _ = spawn_blocking(move || {
                    let _ = handle.join();
                })
                .await;
            }
        }
    }

    /// 关闭管理器，销毁所有控制器
    ///
    /// 安全地移除所有已创建的虚拟手柄并断开 ViGEmBus 连接。
    /// 先 drop 所有 target 触发 unplug，再 join 所有通知线程确保干净回收。
    /// 可多次调用，第二次及后续调用为无操作。
    pub async fn shutdown(&self) {
        self.shutdown.store(true, Ordering::SeqCst);

        let controllers = {
            let mut controllers = self.controllers.lock().unwrap();
            std::mem::take(&mut *controllers)
        };

        if !controllers.is_empty() {
            let _ = spawn_blocking(move || {
                // 先 drop 所有 target（触发 unplug，通知线程收到 abort 退出）
                let mut handles = Vec::new();
                for (_, entry) in controllers {
                    drop(entry.target);
                    if let Some(h) = entry.notification_thread {
                        handles.push(h);
                    }
                }
                // 再 join 所有通知线程
                for h in handles {
                    let _ = h.join();
                }
            })
            .await;
        }
    }
}