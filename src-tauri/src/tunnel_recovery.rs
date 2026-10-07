use super::{
    monitor_status, runtime_key_saved, AppState, Settings, TunnelRecoveryStatus,
};
use crate::tunnel_status::RuntimeObservation;
use std::sync::atomic::Ordering;

pub(super) fn update_runtime_snapshot(
    state: &AppState,
    observation: &RuntimeObservation,
    desired_connected: bool,
    settings: &Settings,
) {
    if let Ok(mut snapshot) = state.runtime_snapshot.lock() {
        let next = monitor_status::project_runtime_snapshot(
            &snapshot,
            observation,
            desired_connected,
            settings,
            true,
        );
        *snapshot = next;
    }
}

pub(super) fn reconnect_delay_secs(attempt: u32) -> u64 {
    match attempt {
        0 | 1 => 2,
        2 => 5,
        3 => 10,
        4 => 20,
        _ => 30,
    }
}
pub(super) fn set_recovery_status(
    state: &AppState,
    phase: &str,
    next_reconnect_at: u64,
    last_error: impl Into<String>,
) {
    if let Ok(mut recovery) = state.recovery_status.lock() {
        recovery.phase = phase.to_string();
        recovery.next_reconnect_at = next_reconnect_at;
        recovery.last_error = last_error.into();
    }
}

pub(super) fn recovery_status(state: &AppState) -> TunnelRecoveryStatus {
    state
        .recovery_status
        .lock()
        .map(|value| value.clone())
        .unwrap_or_default()
}

pub(super) fn restore_desired_connection_intent(
    state: &AppState,
    settings: &Settings,
) {
    state
        .desired_connected
        .store(settings.connection_intent, Ordering::SeqCst);
}

pub(super) fn ensure_runtime_start_intent(
    state: &AppState,
    settings: &Settings,
    required: bool,
) -> Result<(), String> {
    if required
        && (!settings.connection_intent
            || !state.desired_connected.load(Ordering::SeqCst))
    {
        return Err("Tunnel 连接意图已取消，拒绝重新启动。".into());
    }
    Ok(())
}
pub(super) fn reconnect_capability(
    app: &tauri::AppHandle,
    state: &AppState,
    settings: &Settings,
) -> (bool, String) {
    if !settings.connection_intent
        || !state.desired_connected.load(Ordering::SeqCst)
    {
        return (false, "manually_stopped".into());
    }
    if settings.tunnel_id.trim().is_empty() {
        return (false, "tunnel_not_configured".into());
    }
    if state.reconnecting.load(Ordering::SeqCst)
        || state
            .monitor_control
            .reconnect_running
            .load(Ordering::SeqCst)
    {
        return (false, "operation_busy".into());
    }
    let session_key_available = state
        .session_runtime_key
        .lock()
        .map(|value| {
            value
                .as_ref()
                .is_some_and(|key| !key.trim().is_empty())
        })
        .unwrap_or(false);
    if session_key_available || runtime_key_saved(app) {
        return (true, String::new());
    }
    (false, "runtime_key_unavailable".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persisted_connection_intent_is_the_only_startup_desire_source() {
        let state = AppState::default();
        let mut settings = Settings {
            connection_intent: true,
            ..Settings::default()
        };

        restore_desired_connection_intent(&state, &settings);
        assert!(state.desired_connected.load(Ordering::SeqCst));

        settings.connection_intent = false;
        restore_desired_connection_intent(&state, &settings);
        assert!(!state.desired_connected.load(Ordering::SeqCst));
    }
}
