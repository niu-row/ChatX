use super::{
    desktop_commander_home, mcp_command, push_log,
    runtime_paths, secret_path, timestamp_ms, APP_VERSION,
};
use crate::{
    connection_coordinator::ConnectionCoordinator,
    app_state::{AppState, RuntimePaths},
    connection_intent::{self, Transport},
    mcp_sources, monitor,
    secrets::{
        clear_runtime_key, key_storage, load_runtime_key, protect_secret,
        runtime_key_saved,
    },
    settings::{load_settings, update_settings},
    tunnel_recovery::{
        reconnect_capability, recovery_status, set_recovery_status,
        update_runtime_snapshot,
    },
    tunnel_runtime::{cancel_connection_intent, stop_tunnel_without_saving, start_runtime_connection, stop_runtime},
    tunnel_status::runtime_status,
};
use serde_json::{json, Value};
use std::{fs, sync::atomic::Ordering};
use tauri::State;

fn manifest(paths: &RuntimePaths) -> Value {
    fs::read_to_string(paths.root.join("runtime-manifest.json")).ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok()).unwrap_or_else(|| json!({}))
}

#[tauri::command]
pub(super) fn get_status(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    let settings = load_settings(&app);
    let secret_saved = runtime_key_saved(&app);
    let now_ms = timestamp_ms();
    let activities = mcp_sources::activities(&app, state.inner(), now_ms).unwrap_or_default();
    let mcp_monitor = monitor::evaluate_activity(mcp_sources::primary_activity(&activities), now_ms);
    let mcp_transports = mcp_sources::monitor_statuses(&activities, now_ms);
    let recovery = recovery_status(state.inner());
    let tray_stop_error = state.tray_stop_error.lock()
        .map(|value| value.clone()).unwrap_or_default();
    let https_mcp_status = crate::https_mcp::status_view(&app, &settings.https_mcp, state.inner());
    let active_mcp_mode = crate::settings::effective_mcp_mode(
        &settings, https_mcp_status.enabled,
        state.desired_connected.load(Ordering::SeqCst),
    );
    let (reconnect_available, reconnect_unavailable_reason) =
        reconnect_capability(&app, state.inner(), &settings);
    match runtime_paths(&app) {
        Ok(paths) => {
            let dc_home = desktop_commander_home(&app)?;
            let runtime_manifest = manifest(&paths);
            let tunnel_version = runtime_manifest
                .get("tunnelClient")
                .and_then(|value| value.get("version"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let desktop_commander = runtime_manifest
                .get("desktopCommander")
                .cloned()
                .unwrap_or_else(|| json!({"version":"unknown"}));
            let tunnel_snapshot = state.runtime_snapshot.lock()
                .map(|value| value.clone())
                .unwrap_or_default();
            let runtime_state = tunnel_snapshot.state.clone();
            let runtime_active = tunnel_snapshot.active;
            let last_error = tunnel_snapshot.last_error.clone();
            let logs = state.logs.lock().map(|v| v.clone()).unwrap_or_default();
            Ok(json!({
                "service":{"name":"ChatX","version":APP_VERSION},
                "configured":!settings.tunnel_id.is_empty(),
                "tunnelId":settings.tunnel_id,
                "rememberKey":settings.remember_key,
                "autoReconnect":settings.auto_reconnect,
                "activeMcpMode":active_mcp_mode,
                "desiredConnected":state.desired_connected.load(Ordering::SeqCst),
                "reconnecting":state.reconnecting.load(Ordering::SeqCst),
                "reconnectAttempt":state.reconnect_attempt.load(Ordering::SeqCst),
                "reconnectPhase":recovery.phase.clone(),
                "nextReconnectAt":recovery.next_reconnect_at,
                "lastReconnectError":recovery.last_error.clone(),
                "reconnectAvailable":reconnect_available,
                "reconnectUnavailableReason":reconnect_unavailable_reason.clone(),
                "runtimeKeySaved":secret_saved,
                "keyStorage":key_storage(),
                "platform":format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                "runtimeState":runtime_state,
                "runtimeActive":runtime_active,
                "runtimeHealth":tunnel_snapshot.health,
                "runtime":Value::Null,
                "lastError":last_error,
                "tunnelVersion":tunnel_version,
                "desktopCommander":desktop_commander,
                "mcpCommand":mcp_command(&paths, &dc_home),
                "mcpMonitor":mcp_monitor,
                "mcpTransports":mcp_transports,
                "trayStopError":tray_stop_error,
                "httpsMcp":https_mcp_status,
                "logs":logs
            }))
        }
        Err(error) => Ok(json!({
            "service":{"name":"ChatX","version":APP_VERSION},
            "configured":!settings.tunnel_id.is_empty(),
            "tunnelId":settings.tunnel_id,
            "rememberKey":settings.remember_key,
            "autoReconnect":settings.auto_reconnect,
            "activeMcpMode":active_mcp_mode,
            "desiredConnected":state.desired_connected.load(Ordering::SeqCst),
            "reconnecting":state.reconnecting.load(Ordering::SeqCst),
            "reconnectAttempt":state.reconnect_attempt.load(Ordering::SeqCst),
            "reconnectPhase":recovery.phase.clone(),
            "nextReconnectAt":recovery.next_reconnect_at,
            "lastReconnectError":recovery.last_error.clone(),
            "reconnectAvailable":reconnect_available,
            "reconnectUnavailableReason":reconnect_unavailable_reason.clone(),
            "runtimeKeySaved":secret_saved,
            "keyStorage":key_storage(),
            "platform":format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            "runtimeState":"unavailable",
            "runtimeActive":false,
            "runtimeHealth":"unavailable",
            "lastError":error,
            "mcpMonitor":mcp_monitor,
            "mcpTransports":mcp_transports,
            "trayStopError":tray_stop_error,
            "httpsMcp":https_mcp_status,
            "logs":state.logs.lock().map(|v| v.clone()).unwrap_or_default()
        }))
    }
}

#[tauri::command]
pub(super) fn set_auto_reconnect(app: tauri::AppHandle, state: State<'_, AppState>, enabled: bool) -> Result<Value, String> {
    update_settings(&app, |settings| {
        settings.auto_reconnect = enabled;
        Ok(())
    })?;
    if !enabled {
        state.reconnecting.store(false, Ordering::SeqCst);
        state.reconnect_attempt.store(0, Ordering::SeqCst);
        set_recovery_status(
            state.inner(),
            if state.desired_connected.load(Ordering::SeqCst) { "disabled" } else { "stopped" },
            0,
            "",
        );
    } else if state.desired_connected.load(Ordering::SeqCst) {
        set_recovery_status(state.inner(), "detecting", 0, "");
    }
    push_log(&state, if enabled { "已开启 Tunnel 断线自动重连" } else { "已关闭 Tunnel 断线自动重连" });
    get_status(app, state)
}

pub(super) fn stop_requested(app: &tauri::AppHandle, state: &AppState) -> Result<(), String> {
    let _connection = ConnectionCoordinator::acquire(app, state)?;
    let marker = connection_intent::mark_stopped(app, Transport::Tunnel);
    let settings = load_settings(app);
    let stopped = stop_tunnel_without_saving(app, state, &settings);
    let saved = update_settings(app, |settings| {
        settings.connection_intent = false;
        Ok(())
    });
    if let Err(error) = &saved {
        push_log(state, format!("Tunnel 已要求停止，但保存停止状态失败：{error}"));
    }
    stopped?;
    saved?;
    marker
}

/// Stop both transports after one settings write, so concurrent UI calls cannot
/// restore the other transport's connection intent from a stale settings copy.
pub(super) fn stop_all_requested(app: &tauri::AppHandle, state: &AppState) -> Result<(), String> {
    ConnectionCoordinator::acquire(app, state)?.stop_all()
}
#[tauri::command]
pub(super) fn stop_all_connections(
    app: tauri::AppHandle, state: State<'_, AppState>,
) -> Result<Value, String> {
    stop_all_requested(&app, state.inner())?;
    get_status(app, state)
}

#[tauri::command]
pub(super) fn connect_tunnel(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    tunnel_id: String,
    runtime_key: String,
    remember_key: bool,
) -> Result<Value, String> {
    let tunnel_id = tunnel_id.trim().to_string();
    if !tunnel_id.starts_with("tunnel_") {
        return Err("Tunnel ID 应以 tunnel_ 开头。".into());
    }
    let key = load_runtime_key(&app, &runtime_key)?;
    let coordinator = ConnectionCoordinator::acquire(&app, state.inner())?;
    coordinator.ensure_available("ChatX 正在退出，拒绝连接 Tunnel")?;
    // The user chose Tunnel. Stop the HTTPS bridge and its SSH forward
    // before starting the Tunnel runtime; preserve a durable stop marker so
    // the HTTPS transport cannot silently resume after an interrupted save.
    coordinator.prepare_tunnel()?;
    let previous_settings = load_settings(&app);

    state.desired_connected.store(false, Ordering::SeqCst);
    state.reconnecting.store(false, Ordering::SeqCst);
    state.reconnect_attempt.store(0, Ordering::SeqCst);
    set_recovery_status(state.inner(), "reconnecting", 0, "");
    if let Ok(mut session_key) = state.session_runtime_key.lock() {
        *session_key = Some(key.clone());
    }

    if let Err(error) = start_runtime_connection(
        &app,
        state.inner(),
        &tunnel_id,
        &key,
        false,
        false,
    ) {
        cancel_connection_intent(state.inner());
        return Err(error);
    }

    state.desired_connected.store(true, Ordering::SeqCst);
    let next_settings = update_settings(&app, |settings| {
        settings.tunnel_id = tunnel_id.clone();
        settings.remember_key = remember_key;
        settings.connection_intent = true;
        settings.https_mcp.enabled = false;
        Ok(())
    });
    if let Err(error) = &next_settings {
        cancel_connection_intent(state.inner());
        let runtime_rollback = stop_runtime(
            &app,
            Some(state.inner()),
        );
        return match runtime_rollback {
            Ok(_) => Err(format!(
                "Tunnel 已启动但无法保存连接设置，已停止刚启动的连接：{error}"
            )),
            Err(rollback_error) => Err(format!(
                "Tunnel 已启动但无法保存连接设置：{error}；同时停止刚启动的 Tunnel 失败：{rollback_error}"
            )),
        };
    }

    let next_settings = next_settings?;
    let secret_result = if remember_key {
        secret_path(&app)
            .and_then(|path| protect_secret(&key, &path))
    } else {
        clear_runtime_key(&app)
    };
    if let Err(error) = secret_result {
        let settings_restore = update_settings(&app, |settings| {
            settings.tunnel_id.clone_from(&previous_settings.tunnel_id);
            settings.remember_key = previous_settings.remember_key;
            settings.connection_intent = previous_settings.connection_intent;
            Ok(())
        }).map(|_| ());
        cancel_connection_intent(state.inner());
        let runtime_rollback = stop_runtime(
            &app,
            Some(state.inner()),
        );
        return match (settings_restore, runtime_rollback) {
            (Ok(()), Ok(_)) => Err(format!(
                "Tunnel 已启动但 Runtime Key 持久化失败，已恢复原设置并停止连接：{error}"
            )),
            (Err(settings_error), Ok(_)) => Err(format!(
                "Tunnel Runtime Key 持久化失败：{error}；连接已停止，但恢复原设置失败：{settings_error}"
            )),
            (Ok(()), Err(runtime_error)) => Err(format!(
                "Tunnel Runtime Key 持久化失败：{error}；原设置已恢复，但停止刚启动的 Tunnel 失败：{runtime_error}"
            )),
            (Err(settings_error), Err(runtime_error)) => Err(format!(
                "Tunnel Runtime Key 持久化失败：{error}；恢复原设置失败：{settings_error}；停止刚启动的 Tunnel 也失败：{runtime_error}"
            )),
        };
    }

    if let Err(error) = connection_intent::clear_stopped(&app, Transport::Tunnel) {
        cancel_connection_intent(state.inner());
        let mut details = vec![error];
        if let Err(stop_error) = stop_runtime(&app, Some(state.inner())) {
            details.push(format!("停止未完成的 Tunnel 失败：{stop_error}"));
        }
        if let Err(settings_error) = update_settings(&app, |settings| {
            settings.connection_intent = false;
            Ok(())
        }) {
            details.push(format!("恢复停止状态失败：{settings_error}"));
        }
        return Err(details.join("；"));
    }
    if let Ok(mut cache) = state.permission_results.lock() {
        *cache = None;
    }
    if let Ok(paths) = runtime_paths(&app) {
        let observation = runtime_status(&app, &paths);
        update_runtime_snapshot(
            state.inner(),
            &observation,
            true,
            &next_settings,
        );
        set_recovery_status(
            state.inner(),
            if observation.active { "idle" } else { "verifying" },
            0,
            if observation.active { "" } else { "等待 Tunnel Ready。" },
        );
    }
    drop(coordinator);
    get_status(app, state)
}

#[tauri::command]
pub(super) fn stop_tunnel(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    stop_requested(&app, state.inner())?;
    get_status(app, state)
}

#[tauri::command]
pub(super) fn clear_saved_key(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    clear_runtime_key(&app)?;
    if let Ok(mut cache) = state.permission_results.lock() { *cache = None; }
    update_settings(&app, |settings| {
        settings.remember_key = false;
        Ok(())
    })?;
    push_log(&state, "已清除保存的 Runtime Key");
    get_status(app, state)
}
