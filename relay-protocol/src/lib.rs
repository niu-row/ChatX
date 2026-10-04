use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u32 = 1;

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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum DesktopServerMessage {
    HelloAck {
        session_id: String,
        server_time: u64,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case", rename_all_fields = "camelCase")]
pub enum DeviceServerMessage {
    HelloAck {
        session_id: String,
        server_time: u64,
        desktop_online: bool,
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
        };
        let json = serde_json::to_string(&hello).unwrap();
        assert!(json.contains("\"type\":\"hello\""));
        let decoded: DesktopWsMessage = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded, hello);
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
