use super::{
    effective_proxy_url, ensure_monitor_desktop_id, host_device_name,
    proxy_host_port, push_log, runtime_paths, state_dir, system_proxy_url,
    tcp_probe, validate_proxy_url, APP_VERSION,
};
#[cfg(target_os = "macos")]
use super::macos_account_home;
use crate::{
    connection_coordinator::ConnectionCoordinator,
    app_state::AppState,
    monitor_control, monitor_manager, monitor_status, relay,
    secrets::{
        ensure_monitor_master_key, load_runtime_key, read_relay_credentials,
        save_relay_credentials,
    },
    settings::{load_settings, update_settings, Settings},
    tunnel_recovery::update_runtime_snapshot,
    tunnel_runtime::start_runtime_connection,
    tunnel_status::runtime_status,
};
use serde_json::{json, Value};
use std::{
    fs,
    path::PathBuf,
    sync::{atomic::Ordering, Arc},
};
use tauri::{Manager, State};

#[cfg(target_os = "macos")]
pub(super) fn relay_account_home() -> Result<PathBuf, String> {
    macos_account_home()
}

#[cfg(target_os = "macos")]
fn relay_setup_token_path(_app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(macos_account_home()?
        .join("Library")
        .join("Application Support")
        .join("com.chatgptx.local")
        .join("signing")
        .join("relay-setup-token"))
}

#[cfg(not(target_os = "macos"))]
pub(super) fn relay_account_home() -> Result<PathBuf, String> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| "无法定位当前用户主目录。".to_string())
}

#[cfg(not(target_os = "macos"))]
fn relay_setup_token_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(state_dir(app)?.join("relay-setup-token"))
}

fn read_relay_setup_token(app: &tauri::AppHandle) -> Result<String, String> {
    if let Ok(value) = std::env::var("CHATX_RELAY_SETUP_TOKEN") {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return Ok(value);
        }
    }

    let path = relay_setup_token_path(app)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::metadata(&path)
            .map_err(|_| "Relay 注册凭据尚未由管理员预配。".to_string())?;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("Relay 注册凭据文件权限过宽；请限制为仅当前用户可读。".into());
        }
    }
    let value = fs::read_to_string(&path)
        .map_err(|_| "Relay 注册凭据尚未由管理员预配。".to_string())?;
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err("Relay 注册凭据为空。".into());
    }
    Ok(value)
}

pub(super) fn relay_credentials_for_settings(
    app: &tauri::AppHandle,
    settings: &Settings,
) -> Option<relay::RelayCredentials> {
    let credentials = read_relay_credentials(app).ok()?;
    let credential_url = relay::normalized_base_url(&credentials.base_url).ok()?;
    let settings_url = relay::normalized_base_url(&settings.relay.base_url).ok()?;
    (credential_url == settings_url).then_some(credentials)
}

pub(super) fn stop_relay_client(state: &AppState) {
    if let Ok(mut slot) = state.relay.client.lock() {
        let previous = slot.take();
        drop(slot);
        drop(previous);
    }
    if let Ok(mut status) = state.relay.status.lock() {
        status.connected = false;
        status.connecting = false;
        status.session_id = None;
    }
}

fn relay_status_snapshot(
    app: &tauri::AppHandle,
    state: &AppState,
    settings: &Settings,
) -> relay::RelayStatus {
    let mut status = state.relay.status.lock()
        .map(|value| value.clone())
        .unwrap_or_default();
    status.enabled = settings.relay.enabled;
    let credentials = relay_credentials_for_settings(app, settings);
    status.configured = credentials.is_some();
    if status.desktop_id.is_none() {
        if let Some(credentials) = credentials {
            status.desktop_id = Some(credentials.desktop_id);
        }
    }
    status
}

pub(super) fn start_relay_client_for_settings(
    app: &tauri::AppHandle,
    state: &AppState,
) -> Result<(), String> {
    stop_relay_client(state);
    let settings = load_settings(app);
    if !settings.relay.enabled {
        if let Ok(mut status) = state.relay.status.lock() {
            *status = relay::RelayStatus {
                enabled: false,
                configured: relay_credentials_for_settings(app, &settings).is_some(),
                ..relay::RelayStatus::default()
            };
        }
        return Ok(());
    }

    relay::validate(&settings.relay)?;
    let Some(credentials) = relay_credentials_for_settings(app, &settings) else {
        if let Ok(mut status) = state.relay.status.lock() {
            *status = relay::RelayStatus {
                enabled: true,
                configured: false,
                ..relay::RelayStatus::default()
            };
        }
        return Ok(());
    };
    let _ = monitor_manager::monitor_device_ids(app, state)?;
    let master_key = ensure_monitor_master_key(app)?;
    let app_for_snapshot = app.clone();
    let snapshot_provider: relay::SnapshotProvider = Arc::new(move || {
        let state = app_for_snapshot.state::<AppState>();
        let device_ids = match
            monitor_manager::monitor_device_ids(&app_for_snapshot, state.inner())
        {
            Ok(device_ids) => device_ids,
            Err(_) => return Vec::new(),
        };
        if device_ids.is_empty() {
            return Vec::new();
        }
        let payload = match serde_json::to_vec(&monitor_status::payload(
            &app_for_snapshot,
            state.inner(),
        )) {
            Ok(value) => value,
            Err(_) => return Vec::new(),
        };
        device_ids.into_iter().map(|device_id| relay::SnapshotSource {
            device_id,
            plaintext: payload.clone(),
        }).collect()
    });

    let app_for_pair = app.clone();
    let pairing_handler: relay::PairingHandler = Arc::new(move |message| {
        let state = app_for_pair.state::<AppState>();
        monitor_manager::handle_encrypted_monitor_pairing(&app_for_pair, state.inner(), message)
    });

    let app_for_revoke = app.clone();
    let revoke_handler: relay::RevokeHandler = Arc::new(move |device_id| {
        let state = app_for_revoke.state::<AppState>();
        monitor_manager::revoke_monitor_device_local(
            &app_for_revoke,
            state.inner(),
            device_id,
        ).map(|_| ())
    });

    let app_for_control = app.clone();
    let control_handler: relay::ControlHandler = Arc::new(
        move |device_id, request| {
            let state = app_for_control.state::<AppState>();
            monitor_control::handle(
                &app_for_control,
                state.inner(),
                device_id,
                request,
            )
        },
    );

    let handlers = relay::RelayHandlers::new(
        snapshot_provider,
        pairing_handler,
        revoke_handler,
        control_handler,
    );
    let handle = relay::start_client(
        settings.relay.clone(),
        credentials,
        master_key,
        state.relay.status.clone(),
        handlers,
    )?;
    let mut slot = state.relay.client.lock()
        .map_err(|_| "Relay Client 状态锁已损坏。".to_string())?;
    *slot = Some(handle);
    Ok(())
}

fn network_settings_from_payload(
    current: &Settings,
    payload: &Value,
) -> Result<Settings, String> {
    let mut next = current.clone();
    if let Some(proxy) = payload.get("proxy") {
        if !proxy.is_object() { return Err("代理设置格式无效。".into()); }
        if let Some(mode) = proxy.get("mode").and_then(Value::as_str) {
            next.proxy.mode = mode.trim().to_ascii_lowercase();
        }
        if let Some(url) = proxy.get("url").and_then(Value::as_str) {
            next.proxy.url = url.trim().to_string();
        }
        if !matches!(next.proxy.mode.as_str(), "direct" | "system" | "manual") {
            return Err("Tunnel 代理模式仅支持 direct、system 或 manual。".into());
        }
        match next.proxy.mode.as_str() {
            "direct" => next.proxy.applied_url.clear(),
            "manual" => {
                next.proxy.url = validate_proxy_url(&next.proxy.url)?;
                next.proxy.applied_url = next.proxy.url.clone();
            }
            "system" => {
                next.proxy.applied_url = system_proxy_url()?.unwrap_or_default();
            }
            _ => unreachable!("proxy mode validated above"),
        }
    }
    if let Some(relay_payload) = payload.get("relay") {
        if !relay_payload.is_object() { return Err("Relay 设置格式无效。".into()); }
        if let Some(enabled) = relay_payload.get("enabled").and_then(Value::as_bool) {
            next.relay.enabled = enabled;
        }
        if let Some(base_url) = relay_payload.get("baseUrl").and_then(Value::as_str) {
            next.relay.base_url = base_url.trim().to_string();
        }
        relay::validate(&next.relay)?;
        if next.relay.enabled && !next.monitor_enabled {
            return Err("启用 ChatX Relay 前请先开启手机监控。".into());
        }
    }
    Ok(next)
}

fn network_settings_payload(app: &tauri::AppHandle, state: &AppState) -> Value {
    let settings = load_settings(app);
    let system_proxy = system_proxy_url();
    let effective_proxy = effective_proxy_url(&settings);
    let relay_status = relay_status_snapshot(app, state, &settings);
    let relay_registered = relay_credentials_for_settings(app, &settings).is_some();
    let relay_setup_provisioned = read_relay_setup_token(app).is_ok();
    let tunnel = state.runtime_snapshot.lock()
        .map(|snapshot| snapshot.clone())
        .unwrap_or_default();

    json!({
        "proxy": {
            "mode": settings.proxy.mode,
            "url": settings.proxy.url,
            "systemUrl": system_proxy.as_ref().ok().and_then(|value| value.clone()),
            "systemError": system_proxy.err(),
            "effectiveUrl": effective_proxy.as_ref().ok().and_then(|value| value.clone()),
            "effectiveError": effective_proxy.err()
        },
        "relay": {
            "enabled": settings.relay.enabled,
            "baseUrl": settings.relay.base_url,
            "registered": relay_registered,
            "credentialSaved": relay_registered,
            "setupProvisioned": relay_setup_provisioned,
            "status": relay_status
        },
        "tunnel": {
            "health": tunnel.health,
            "state": tunnel.state,
            "controlPlaneState": tunnel.control_plane_state,
            "controlPlaneReason": tunnel.control_plane_reason,
            "controlPlaneFailures": tunnel.control_plane_failures,
            "proxyMode": tunnel.proxy_mode,
            "proxySource": tunnel.proxy_source
        }
    })
}

#[tauri::command]
pub(super) fn get_network_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    Ok(network_settings_payload(&app, state.inner()))
}

// Identity creation is an explicit, idempotent user action. A read-only
// network status refresh must not create or change the Desktop identity.
// The deploy wizard needs the stable ID *before* first Relay registration.
#[tauri::command]
pub(super) fn prepare_relay_deploy_desktop_id(app: tauri::AppHandle) -> Result<String, String> {
    ensure_monitor_desktop_id(&app)
}

#[tauri::command]
pub(super) fn register_relay_desktop(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let settings = load_settings(&app);
    if settings.relay.base_url.trim().is_empty() {
        return Err("请先配置 Relay URL。".into());
    }
    relay::normalized_base_url(&settings.relay.base_url)?;
    let setup_token = read_relay_setup_token(&app)?;
    let desktop_id = ensure_monitor_desktop_id(&app)?;
    let credentials = relay::register_desktop(
        &settings.relay,
        &setup_token,
        &desktop_id,
        &host_device_name(),
        APP_VERSION,
        &format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    )?;
    save_relay_credentials(&app, &credentials)?;
    if settings.relay.enabled {
        start_relay_client_for_settings(&app, state.inner())?;
    }
    push_log(
        state.inner(),
        format!("ChatX Relay Desktop 注册完成：{}", settings.relay.base_url),
    );
    Ok(network_settings_payload(&app, state.inner()))
}

#[tauri::command]
pub(super) fn test_network_settings(
    app: tauri::AppHandle,
    payload: Value,
) -> Result<Value, String> {
    let current = load_settings(&app);
    let candidate = network_settings_from_payload(&current, &payload)?;
    if payload.get("proxy").is_none() && payload.get("relay").is_none() {
        return Err("没有指定需要测试的网络设置。".into());
    }
    // Test only the selected settings area: probing the unrelated route can
    // stall a quick proxy check on an unreachable Relay (or vice versa).
    let proxy_check = if payload.get("proxy").is_some() {
        let effective = effective_proxy_url(&candidate)?;
        if let Some(url) = effective.as_ref() {
            let (host, port) = proxy_host_port(url)
                .ok_or_else(|| "无法解析代理 host:port。".to_string())?;
            json!({
                "mode": candidate.proxy.mode,
                "target": format!("{host}:{port}"),
                "ok": tcp_probe(&host, port)
            })
        } else {
            json!({
                "mode": candidate.proxy.mode,
                "target": "api.openai.com:443",
                "ok": tcp_probe("api.openai.com", 443)
            })
        }
    } else { json!({"skipped":true}) };

    let relay_check = if payload.get("relay").is_none() {
        json!({"skipped":true})
    } else if candidate.relay.base_url.trim().is_empty() {
        json!({"ok": false, "target": Value::Null, "error": "尚未配置 Relay URL"})
    } else {
        match relay::health(&candidate.relay) {
            Ok(()) => json!({
                "ok": true,
                "target": candidate.relay.base_url,
                "error": Value::Null
            }),
            Err(error) => json!({
                "ok": false,
                "target": candidate.relay.base_url,
                "error": error
            })
        }
    };
    Ok(json!({
        "proxy": proxy_check,
        "relay": relay_check
    }))
}

fn rollback_network_settings(
    app: &tauri::AppHandle,
    state: &AppState,
    previous: &Settings,
    reconnect_key: Option<&str>,
    restore_proxy_runtime: bool,
    restore_relay: bool,
) -> Vec<String> {
    let mut errors = Vec::new();
    if let Err(error) = update_settings(app, |current| {
        current.proxy.clone_from(&previous.proxy);
        current.relay.clone_from(&previous.relay);
        Ok(())
    }) {
        errors.push(format!("恢复原设置失败：{error}"));
        return errors;
    }

    if restore_proxy_runtime {
        match reconnect_key {
            Some(key) => {
                if let Err(error) = start_runtime_connection(
                    app,
                    state,
                    &previous.tunnel_id,
                    key,
                    false,
                    true,
                ) {
                    errors.push(format!(
                        "恢复原 Tunnel 代理失败：{error}"
                    ));
                } else if let Ok(paths) = runtime_paths(app) {
                    let observation = runtime_status(app, &paths);
                    update_runtime_snapshot(
                        state,
                        &observation,
                        true,
                        previous,
                    );
                }
            }
            None => errors.push(
                "恢复原 Tunnel 代理失败：没有可用 Runtime Key。"
                    .into(),
            ),
        }
    }

    if restore_relay {
        if let Err(error) =
            start_relay_client_for_settings(app, state)
        {
            errors.push(format!("恢复原 Relay 状态失败：{error}"));
        }
    }
    errors
}

fn rollback_suffix(errors: &[String]) -> String {
    if errors.is_empty() {
        "原配置已恢复。".into()
    } else {
        format!("回滚未完全成功：{}", errors.join("；"))
    }
}

#[tauri::command]
pub(super) fn set_network_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    payload: Value,
) -> Result<Value, String> {
    // Proxy updates may restart Tunnel; serialize against MCP mode switches.
    let _connection = ConnectionCoordinator::acquire(&app, state.inner())?;
    let update_proxy = payload.get("proxy").is_some();
    let update_relay = payload.get("relay").is_some();
    if !update_proxy && !update_relay {
        return Err("没有指定需要保存的网络设置。".into());
    }
    let previous = load_settings(&app);
    let next = network_settings_from_payload(&previous, &payload)?;
    let wants_connection = state.desired_connected.load(Ordering::SeqCst);
    let proxy_changed = previous.proxy != next.proxy
        || (wants_connection && next.proxy.mode == "system");
    let relay_changed = previous.relay != next.relay;

    let reconnect_key = if proxy_changed && wants_connection {
        state.session_runtime_key.lock().ok().and_then(|value| value.clone())
            .or_else(|| load_runtime_key(&app, "").ok())
            .ok_or_else(|| "更改 Tunnel 代理需要重新连接，但当前没有可用 Runtime Key。".to_string())?
            .into()
    } else {
        None
    };

    update_settings(&app, |current| {
        if update_proxy { current.proxy.clone_from(&next.proxy); }
        if update_relay { current.relay.clone_from(&next.relay); }
        Ok(())
    })?;

    if proxy_changed && wants_connection {
        let key = reconnect_key.as_deref().unwrap_or("");
        if let Err(error) = start_runtime_connection(
            &app,
            state.inner(),
            &next.tunnel_id,
            key,
            false,
            true,
        ) {
            let rollback_errors = rollback_network_settings(
                &app,
                state.inner(),
                &previous,
                reconnect_key.as_deref(),
                true,
                false,
            );
            return Err(format!(
                "应用 Tunnel 代理失败：{error}；{}",
                rollback_suffix(&rollback_errors),
            ));
        }
        if let Ok(paths) = runtime_paths(&app) {
            let observation = runtime_status(&app, &paths);
            update_runtime_snapshot(state.inner(), &observation, true, &next);
        }
    }

    if relay_changed {
        if let Err(error) =
            start_relay_client_for_settings(&app, state.inner())
        {
            let rollback_errors = rollback_network_settings(
                &app,
                state.inner(),
                &previous,
                reconnect_key.as_deref(),
                proxy_changed && wants_connection,
                true,
            );
            return Err(format!(
                "应用 ChatX Relay 失败：{error}；{}",
                rollback_suffix(&rollback_errors),
            ));
        }
    }

    push_log(
        state.inner(),
        format!(
            "网络设置已更新：proxy={} relay={}",
            next.proxy.mode,
            if next.relay.enabled { "wss" } else { "off" }
        ),
    );
    Ok(network_settings_payload(&app, state.inner()))
}

#[cfg(test)]
mod partial_network_settings_tests {
    use super::*;

    #[test]
    fn saving_relay_does_not_recalculate_unsaved_proxy_and_vice_versa() {
        let mut original = Settings::default();
        original.monitor_enabled = true;
        original.proxy.mode = "manual".into();
        original.proxy.url = "http://127.0.0.1:7890".into();
        original.proxy.applied_url = original.proxy.url.clone();
        let relay_only = network_settings_from_payload(
            &original,
            &json!({"relay":{"enabled":true,"baseUrl":"https://relay.example.com"}}),
        ).unwrap();
        assert_eq!(relay_only.proxy, original.proxy);
        assert!(relay_only.relay.enabled);
        assert_eq!(relay_only.relay.base_url, "https://relay.example.com");

        let proxy_only = network_settings_from_payload(
            &relay_only,
            &json!({"proxy":{"mode":"direct","url":""}}),
        ).unwrap();
        assert_eq!(proxy_only.relay, relay_only.relay);
        assert_eq!(proxy_only.proxy.mode, "direct");
        assert_eq!(proxy_only.proxy.applied_url, "");
    }
}
