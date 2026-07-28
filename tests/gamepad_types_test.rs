use webpad::gamepad::types::GamepadState;

#[test]
fn default_gamepad_state_is_all_zero() {
    let state = GamepadState::default();
    assert_eq!(state.buttons, 0);
    assert_eq!(state.left_trigger, 0);
    assert_eq!(state.right_trigger, 0);
    assert_eq!(state.thumb_lx, 0);
    assert_eq!(state.thumb_ly, 0);
    assert_eq!(state.thumb_rx, 0);
    assert_eq!(state.thumb_ry, 0);
}

#[test]
fn from_protocol_message_converts_fields_correctly() {
    let msg = webpad::protocol::ClientMessage::GamepadState {
        buttons: 0x1000,
        left_trigger: 128,
        right_trigger: 255,
        thumb_lx: -16000,
        thumb_ly: 16000,
        thumb_rx: 1000,
        thumb_ry: -2000,
    };
    let state = GamepadState::from_client_message(&msg).unwrap();
    assert_eq!(state.buttons, 0x1000);
    assert_eq!(state.left_trigger, 128);
    assert_eq!(state.right_trigger, 255);
    assert_eq!(state.thumb_lx, -16000);
    assert_eq!(state.thumb_ly, 16000);
    assert_eq!(state.thumb_rx, 1000);
    assert_eq!(state.thumb_ry, -2000);
}

#[test]
fn from_protocol_message_ignores_non_gamepad_messages() {
    let msg = webpad::protocol::ClientMessage::Heartbeat;
    let state = GamepadState::from_client_message(&msg);
    assert!(state.is_none());
}

#[test]
fn is_zero_detects_all_zeros() {
    let state = GamepadState::default();
    assert!(state.is_zero());
}

#[test]
fn is_zero_false_when_buttons_pressed() {
    let state = GamepadState {
        buttons: 0x1000,
        ..Default::default()
    };
    assert!(!state.is_zero());
}

#[test]
fn is_zero_false_when_trigger_pressed() {
    let state = GamepadState {
        left_trigger: 1,
        ..Default::default()
    };
    assert!(!state.is_zero());
}

#[test]
fn is_zero_false_when_thumbstick_moved() {
    let state = GamepadState {
        thumb_lx: 1,
        ..Default::default()
    };
    assert!(!state.is_zero());
}

#[test]
fn merge_overwrites_all_fields() {
    let base = GamepadState {
        buttons: 0x1000,
        left_trigger: 100,
        right_trigger: 200,
        thumb_lx: 1000,
        thumb_ly: -1000,
        thumb_rx: 500,
        thumb_ry: -500,
    };
    let update = GamepadState {
        buttons: 0x2000,
        left_trigger: 50,
        right_trigger: 150,
        thumb_lx: -500,
        thumb_ly: 500,
        thumb_rx: -1000,
        thumb_ry: 1000,
    };
    let merged = base.merge(&update);
    assert_eq!(merged.buttons, 0x2000);
    assert_eq!(merged.left_trigger, 50);
    assert_eq!(merged.right_trigger, 150);
    assert_eq!(merged.thumb_lx, -500);
    assert_eq!(merged.thumb_ly, 500);
}

#[cfg(windows)]
#[cfg(test)]
mod windows_tests {
    use webpad::gamepad::GamepadManager;
    use webpad::gamepad::types::GamepadState;

    #[tokio::test]
    #[ignore = "requires ViGEmBus driver"]
    async fn manager_reports_controller_count() {
        let manager = GamepadManager::new().await.unwrap();
        assert_eq!(manager.controller_count(), 0);
        manager.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "requires ViGEmBus driver"]
    async fn create_and_destroy_controller() {
        let manager = GamepadManager::new().await.unwrap();
        let result = manager.create_controller().await;
        assert!(result.is_ok(), "Failed to create controller: {:?}", result.err());
        let (id, _rx) = result.unwrap();
        assert_eq!(manager.controller_count(), 1);
        manager.destroy_controller(id).await;
        assert_eq!(manager.controller_count(), 0);
        manager.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "requires ViGEmBus driver"]
    async fn update_controller_state() {
        let manager = GamepadManager::new().await.unwrap();
        let (id, _rx) = manager.create_controller().await.unwrap();

        let state = GamepadState {
            buttons: 0x1000,
            left_trigger: 128,
            ..Default::default()
        };
        let result = manager.update_state(id, &state).await;
        assert!(result.is_ok(), "Failed to update state: {:?}", result.err());

        manager.destroy_controller(id).await;
        manager.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "requires ViGEmBus driver"]
    async fn update_nonexistent_controller_fails() {
        let manager = GamepadManager::new().await.unwrap();
        let state = GamepadState::default();
        let result = manager.update_state(999, &state).await;
        assert!(result.is_err());
        manager.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "requires ViGEmBus driver"]
    async fn double_shutdown_is_safe() {
        let manager = GamepadManager::new().await.unwrap();
        manager.shutdown().await;
        manager.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "requires ViGEmBus driver"]
    async fn create_controller_returns_rumble_receiver() {
        let manager = GamepadManager::new().await.unwrap();
        let (id, mut rx) = manager.create_controller().await.expect("create failed");
        // 通道刚创建时应为空（无震动事件）
        assert!(rx.try_recv().is_err());
        manager.destroy_controller(id).await;
        // 控制器销毁后通道应关闭（sender drop 后 recv 返回 None）
        match rx.recv().await {
            None => {}
            other => panic!("expected None after destroy, got {:?}", other),
        }
        manager.shutdown().await;
    }
}