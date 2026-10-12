//! Version-stable API shape consumed by desktop JavaScript and Android.
//! The Rust health predicate supplies the verdict; JSON serialization is
//! confined to this DTO, not duplicated across presentation surfaces.
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct HttpsMcpStatus {
    pub(super) enabled: bool,
    pub(super) connected: bool,
    pub(super) state: &'static str,
    pub(super) auto_reconnect: bool,
    pub(super) manual_stop_blocked: bool,
    pub(super) base_url: String,
    pub(super) endpoint: String,
    pub(super) ssh_host: String,
    pub(super) ssh_user: String,
    pub(super) ssh_port: u16,
    pub(super) local_port: u16,
    pub(super) remote_port: u16,
    pub(super) authorization_ttl_hours: Option<u32>,
    pub(super) transport: String,
    pub(super) local_ready: bool,
    pub(super) ssh_connected: bool,
    pub(super) transport_connected: bool,
    pub(super) remote_ready: bool,
    pub(super) last_public_probe_at: u64,
    pub(super) remote_last_error: String,
    pub(super) restarts: u32,
    pub(super) last_error: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_status_json_preserves_legacy_contract() {
        let status = HttpsMcpStatus {
            enabled: true, connected: true, state: "connected",
            auto_reconnect: true, manual_stop_blocked: false,
            base_url: "https://relay.example.com".into(),
            endpoint: "https://relay.example.com/mcp".into(),
            ssh_host: String::new(), ssh_user: String::new(), ssh_port: 22,
            local_port: 3000, remote_port: 18080,
            authorization_ttl_hours: Some(168), transport: "wss".into(),
            local_ready: true, ssh_connected: false, transport_connected: true,
            remote_ready: true, last_public_probe_at: 1000,
            remote_last_error: String::new(), restarts: 0, last_error: String::new(),
        };
        let value = serde_json::to_value(status).unwrap();
        let expected = [
            "enabled", "connected", "state", "autoReconnect",
            "manualStopBlocked", "baseUrl", "endpoint", "sshHost", "sshUser",
            "sshPort", "localPort", "remotePort", "authorizationTtlHours",
            "transport", "localReady", "sshConnected", "transportConnected",
            "remoteReady", "lastPublicProbeAt", "remoteLastError",
            "restarts", "lastError",
        ];
        let object = value.as_object().unwrap();
        assert_eq!(object.len(), expected.len());
        for name in expected { assert!(object.contains_key(name), "{name}"); }
        assert_eq!(value["sshConnected"], false);
        assert_eq!(value["connected"], true);
    }
}
