use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;
pub const CAPABILITY_SNAPSHOT: &str = "snapshot";
pub const CAPABILITY_CONTROL_REFRESH: &str = "control.refresh_snapshot";
pub const CAPABILITY_CONTROL_RECONNECT: &str = "control.reconnect_tunnel";
pub const CAPABILITY_REVOKE_SYNC: &str = "revoke.sync";

pub fn device_capabilities() -> Vec<String> {
    vec![
        CAPABILITY_SNAPSHOT.into(),
        CAPABILITY_CONTROL_REFRESH.into(),
        CAPABILITY_CONTROL_RECONNECT.into(),
        CAPABILITY_REVOKE_SYNC.into(),
    ]
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRegisterRequest {
    pub desktop_id: String,
    pub device_name: String,
    pub app_version: String,
    pub platform: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DesktopRegisterResponse {
    pub desktop_id: String,
    pub desktop_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DeviceAuthorizeRequest {
    pub token_hash: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EncryptedSnapshot {
    pub session_id: String,
    pub sequence: u64,
    pub generated_at: u64,
    pub nonce: String,
    pub ciphertext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EncryptedPairingPayload {
    pub nonce: String,
    pub ciphertext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct EncryptedControlPayload {
    pub request_id: String,
    pub issued_at: u64,
    pub expires_at: u64,
    pub nonce: String,
    pub ciphertext: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PairingRouteOpenRequest {
    pub token_hash: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum DesktopWsMessage {
    Hello {
        protocol_version: u32,
        desktop_id: String,
        app_version: String,
        platform: String,
        #[serde(default)]
        capabilities: Vec<String>,
    },
    Ping {
        sent_at: u64,
    },
    Snapshot {
        device_id: String,
        snapshot: EncryptedSnapshot,
    },
    PairingResponse {
        pairing_id: String,
        connection_id: String,
        payload: EncryptedPairingPayload,
    },
    PairingError {
        pairing_id: String,
        connection_id: String,
        message: String,
    },
    ControlResponse {
        device_id: String,
        response: EncryptedControlPayload,
    },
    ControlError {
        device_id: String,
        request_id: String,
        message: String,
    },
    RevokeResult {
        device_id: String,
        request_id: String,
    },
    RevokeError {
        device_id: String,
        request_id: String,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum DesktopServerMessage {
    HelloAck {
        session_id: String,
        server_time: u64,
        #[serde(default)]
        capabilities: Vec<String>,
    },
    Pong {
        server_time: u64,
    },
    PairingRequest {
        pairing_id: String,
        connection_id: String,
        payload: EncryptedPairingPayload,
    },
    DeviceRevoked {
        device_id: String,
    },
    DeviceRevoke {
        device_id: String,
        request_id: String,
    },
    DeviceControl {
        device_id: String,
        request: EncryptedControlPayload,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum DeviceWsMessage {
    Hello {
        protocol_version: u32,
        desktop_id: String,
        device_id: String,
    },
    Ping {
        sent_at: u64,
    },
    RevokeSelf,
    Control {
        request: EncryptedControlPayload,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum DeviceServerMessage {
    HelloAck {
        session_id: String,
        server_time: u64,
        desktop_online: bool,
        #[serde(default)]
        capabilities: Vec<String>,
    },
    Pong {
        server_time: u64,
    },
    DesktopPresence {
        online: bool,
        changed_at: u64,
    },
    Snapshot {
        received_at: u64,
        snapshot: EncryptedSnapshot,
    },
    Revoked {
        device_id: String,
    },
    ControlResult {
        response: EncryptedControlPayload,
    },
    Error {
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum PairingWsMessage {
    Pair {
        pairing_id: String,
        payload: EncryptedPairingPayload,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum PairingServerMessage {
    PairResult {
        pairing_id: String,
        payload: EncryptedPairingPayload,
    },
    Error {
        message: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn websocket_messages_are_stably_tagged() {
        let hello = DesktopWsMessage::Hello {
            protocol_version: PROTOCOL_VERSION,
            desktop_id: "d_test".into(),
            app_version: "0.4.7".into(),
            platform: "macos-arm64".into(),
            capabilities: device_capabilities(),
        };
        let json = serde_json::to_string(&hello).unwrap();
        assert!(json.contains("\"type\":\"hello\""));
        let decoded: DesktopWsMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, hello);

        let legacy = r#"{"type":"hello","protocolVersion":1,"desktopId":"d_legacy","appVersion":"0.4.9","platform":"windows-x64"}"#;
        assert!(matches!(
            serde_json::from_str::<DesktopWsMessage>(legacy).unwrap(),
            DesktopWsMessage::Hello { capabilities, .. } if capabilities.is_empty()
        ));
    }

    #[test]
    fn pairing_messages_keep_ciphertext_opaque() {
        let payload = EncryptedPairingPayload {
            nonce: "nonce".into(),
            ciphertext: "ciphertext".into(),
        };
        let request = PairingWsMessage::Pair {
            pairing_id: "p_test".into(),
            payload: payload.clone(),
        };
        let text = serde_json::to_string(&request).unwrap();
        assert!(text.contains("\"type\":\"pair\""));
        assert!(text.contains("\"pairingId\":\"p_test\""));
        assert!(text.contains("\"ciphertext\":\"ciphertext\""));
        assert!(!text.contains("deviceName"));
        assert!(!text.contains("pairingCode"));

        let routed = DesktopServerMessage::PairingRequest {
            pairing_id: "p_test".into(),
            connection_id: "pc_test".into(),
            payload,
        };
        let routed_text = serde_json::to_string(&routed).unwrap();
        assert!(routed_text.contains("\"type\":\"pairing_request\""));
        assert!(routed_text.contains("\"connectionId\":\"pc_test\""));
    }

    #[test]
    fn device_revoke_messages_have_stable_wire_tags() {
        let request = serde_json::to_string(&DeviceWsMessage::RevokeSelf).unwrap();
        assert_eq!(request, r#"{"type":"revoke_self"}"#);

        let device_reply = DeviceServerMessage::Revoked {
            device_id: "dev_test".into(),
        };
        let reply = serde_json::to_string(&device_reply).unwrap();
        assert!(reply.contains("\"type\":\"revoked\""));
        assert!(reply.contains("\"deviceId\":\"dev_test\""));

        let desktop_notice = DesktopServerMessage::DeviceRevoked {
            device_id: "dev_test".into(),
        };
        let notice = serde_json::to_string(&desktop_notice).unwrap();
        assert!(notice.contains("\"type\":\"device_revoked\""));
        assert!(notice.contains("\"deviceId\":\"dev_test\""));

        let sync_request = DesktopServerMessage::DeviceRevoke {
            device_id: "dev_test".into(),
            request_id: "r_0123456789abcdef".into(),
        };
        let sync_text = serde_json::to_string(&sync_request).unwrap();
        assert!(sync_text.contains("\"type\":\"device_revoke\""));
        assert!(sync_text.contains("\"requestId\":\"r_0123456789abcdef\""));

        let ack = DesktopWsMessage::RevokeResult {
            device_id: "dev_test".into(),
            request_id: "r_0123456789abcdef".into(),
        };
        let ack_text = serde_json::to_string(&ack).unwrap();
        assert!(ack_text.contains("\"type\":\"revoke_result\""));
        assert!(ack_text.contains("\"requestId\":\"r_0123456789abcdef\""));
    }

    #[test]
    fn control_messages_keep_action_opaque_and_stably_tagged() {
        let payload = EncryptedControlPayload {
            request_id: "c_0123456789abcdef".into(),
            issued_at: 1_000,
            expires_at: 31_000,
            nonce: "nonce".into(),
            ciphertext: "ciphertext".into(),
        };
        let request = DeviceWsMessage::Control {
            request: payload.clone(),
        };
        let text = serde_json::to_string(&request).unwrap();
        assert!(text.contains("\"type\":\"control\""));
        assert!(text.contains("\"requestId\":\"c_0123456789abcdef\""));
        assert!(text.contains("\"ciphertext\":\"ciphertext\""));
        assert!(!text.contains("refresh_snapshot"));
        assert!(!text.contains("reconnect_tunnel"));

        let routed = DesktopServerMessage::DeviceControl {
            device_id: "dev_test".into(),
            request: payload,
        };
        let routed_text = serde_json::to_string(&routed).unwrap();
        assert!(routed_text.contains("\"type\":\"device_control\""));
        assert!(routed_text.contains("\"deviceId\":\"dev_test\""));
    }

    #[test]
    fn device_hello_ack_advertises_capabilities_and_accepts_legacy_shape() {
        let ack = DeviceServerMessage::HelloAck {
            session_id: "s_test".into(),
            server_time: 123,
            desktop_online: true,
            capabilities: device_capabilities(),
        };
        let text = serde_json::to_string(&ack).unwrap();
        assert!(text.contains(CAPABILITY_CONTROL_REFRESH));
        assert!(text.contains(CAPABILITY_CONTROL_RECONNECT));

        let legacy = r#"{"type":"hello_ack","sessionId":"s_legacy","serverTime":1,"desktopOnline":true}"#;
        let decoded: DeviceServerMessage = serde_json::from_str(legacy).unwrap();
        assert!(matches!(
            decoded,
            DeviceServerMessage::HelloAck { capabilities, .. } if capabilities.is_empty()
        ));
    }

    #[test]
    fn snapshot_is_a_desktop_wss_message() {
        let message = DesktopWsMessage::Snapshot {
            device_id: "dev_test".into(),
            snapshot: EncryptedSnapshot {
                session_id: "s_test".into(),
                sequence: 7,
                generated_at: 123,
                nonce: "n".into(),
                ciphertext: "c".into(),
            },
        };
        let text = serde_json::to_string(&message).unwrap();
        assert!(text.contains("\"type\":\"snapshot\""));
        assert!(text.contains("\"deviceId\":\"dev_test\""));
    }
}
