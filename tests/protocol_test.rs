use webpad::protocol::*;

#[test]
fn client_message_gamepad_state_serializes_to_json() {
    let msg = ClientMessage::GamepadState {
        buttons: 0x1000, // A button
        left_trigger: 128,
        right_trigger: 0,
        thumb_lx: 0,
        thumb_ly: 0,
        thumb_rx: 0,
        thumb_ry: 0,
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("\"buttons\":4096"));
    assert!(json.contains("\"left_trigger\":128"));
}

#[test]
fn server_message_gamepad_state_deserializes_from_json() {
    let json = r#"{"type":"gamepad_state","buttons":4096,"left_trigger":128,"right_trigger":0,"thumb_lx":0,"thumb_ly":0,"thumb_rx":0,"thumb_ry":0}"#;
    let msg: ServerMessage = serde_json::from_str(json).unwrap();
    match msg {
        ServerMessage::GamepadState { buttons, left_trigger, .. } => {
            assert_eq!(buttons, 4096);
            assert_eq!(left_trigger, 128);
        }
        _ => panic!("Expected GamepadState variant"),
    }
}

#[test]
fn client_message_auth_request_serializes() {
    let msg = ClientMessage::AuthRequest {
        password: "secret".to_string(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("auth_request"));
    assert!(json.contains("secret"));
}

#[test]
fn server_message_auth_success_serializes() {
    let msg = ServerMessage::AuthSuccess {
        token: "abc123".to_string(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("auth_success"));
    assert!(json.contains("abc123"));
}

#[test]
fn server_message_auth_failure_serializes() {
    let msg = ServerMessage::AuthFailure {
        reason: "Wrong password".to_string(),
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("auth_failure"));
    assert!(json.contains("Wrong password"));
}

#[test]
fn server_message_rumble_serializes() {
    let msg = ServerMessage::Rumble {
        left_motor: 128,
        right_motor: 64,
    };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("\"left_motor\":128"));
    assert!(json.contains("\"right_motor\":64"));
}

#[test]
fn client_message_heartbeat_serializes() {
    let msg = ClientMessage::Heartbeat;
    let json = serde_json::to_string(&msg).unwrap();
    assert_eq!(json, r#"{"type":"heartbeat"}"#);
}

#[test]
fn server_message_pong_serializes() {
    let msg = ServerMessage::Pong;
    let json = serde_json::to_string(&msg).unwrap();
    assert_eq!(json, r#"{"type":"pong"}"#);
}

#[test]
fn round_trip_gamepad_state() {
    let original = ClientMessage::GamepadState {
        buttons: 0x5109,
        left_trigger: 255,
        right_trigger: 128,
        thumb_lx: -32768,
        thumb_ly: 32767,
        thumb_rx: 0,
        thumb_ry: 0,
    };
    let json = serde_json::to_string(&original).unwrap();
    let server_msg: ServerMessage = serde_json::from_str(&json).unwrap();
    match server_msg {
        ServerMessage::GamepadState { buttons, left_trigger, right_trigger, thumb_lx, thumb_ly, .. } => {
            assert_eq!(buttons, 0x5109);
            assert_eq!(left_trigger, 255);
            assert_eq!(right_trigger, 128);
            assert_eq!(thumb_lx, -32768);
            assert_eq!(thumb_ly, 32767);
        }
        _ => panic!("Expected GamepadState"),
    }
}

#[test]
fn client_message_disconnect_serializes() {
    let msg = ClientMessage::Disconnect { reason: "User left".to_string() };
    let json = serde_json::to_string(&msg).unwrap();
    assert!(json.contains("disconnect"));
    assert!(json.contains("User left"));
}