//! One authoritative HTTPS MCP connection verdict for Desktop and Android.
//! Transport process health, public endpoint verification and user stop intent
//! are separate signals. Presentation code must not recreate this logic.
use serde::Serialize;

const PUBLIC_PROBE_FRESHNESS_MS: u64 = 45_000;

#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct HttpsConnectionHealth {
    pub enabled: bool,
    pub manual_stop_blocked: bool,
    pub local_ready: bool,
    pub transport_connected: bool,
    pub remote_ready: bool,
    pub connected: bool,
    pub state: &'static str,
}

pub(super) fn https_connection_health(
    configured: bool,
    restore_allowed: bool,
    local_ready: bool,
    transport_connected: bool,
    last_remote_ready: bool,
    last_public_probe_at: u64,
    now_ms: u64,
) -> HttpsConnectionHealth {
    let enabled = configured && restore_allowed;
    let fresh = last_public_probe_at != 0
        && now_ms >= last_public_probe_at
        && now_ms - last_public_probe_at <= PUBLIC_PROBE_FRESHNESS_MS;
    let remote_ready = transport_connected && last_remote_ready && fresh;
    let connected = enabled && local_ready && transport_connected && remote_ready;
    let state = if !enabled { "stopped" }
        else if connected { "connected" }
        else if transport_connected && local_ready { "checking" }
        else { "reconnecting" };
    HttpsConnectionHealth {
        enabled,
        manual_stop_blocked: configured && !restore_allowed,
        local_ready,
        transport_connected,
        remote_ready,
        connected,
        state,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verified_wss_is_connected_without_ssh() {
        let view = https_connection_health(true, true, true, true, true, 10_000, 11_000);
        assert!(view.connected);
        assert_eq!(view.state, "connected");
    }

    #[test]
    fn stop_intent_overrides_a_cached_online_probe() {
        let view = https_connection_health(true, false, true, true, true, 10_000, 11_000);
        assert!(!view.connected);
        assert!(view.manual_stop_blocked);
        assert_eq!(view.state, "stopped");
    }

    #[test]
    fn stale_public_probe_cannot_keep_desktop_or_android_green() {
        let view = https_connection_health(true, true, true, true, true, 10_000, 60_001);
        assert!(!view.remote_ready);
        assert!(!view.connected);
        assert_eq!(view.state, "checking");
        let future = https_connection_health(true, true, true, true, true, 61_000, 60_000);
        assert!(!future.connected);
    }

    #[test]
    fn offline_and_public_checking_are_distinct() {
        assert_eq!(https_connection_health(true, true, true, false, true, 10_000, 11_000).state, "reconnecting");
        assert_eq!(https_connection_health(true, true, true, true, false, 10_000, 11_000).state, "checking");
    }
}
