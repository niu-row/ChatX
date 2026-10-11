use super::{monitor_manager, stop_relay_client};
use crate::{
    app_state::AppState,
    settings::{load_settings, update_settings},
};
use serde_json::Value;
use tauri::State;

#[tauri::command]
pub(super) fn get_monitor_info(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    monitor_manager::monitor_info_payload(&app, state.inner())
}

#[tauri::command]
pub(super) fn set_monitor_enabled(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
    port: Option<u16>,
) -> Result<Value, String> {
    let previous = load_settings(&app);
    let mut settings = previous.clone();
    if let Some(port) = port {
        if port < 1024 {
            return Err("Monitor 端口必须为 1024-65535。".into());
        }
        settings.monitor_port = port;
    }
    settings.monitor_enabled = enabled;
    if !enabled {
        settings.relay.enabled = false;
    }
    let restart = previous.monitor_enabled
        && enabled
        && previous.monitor_port != settings.monitor_port;
    if restart || !enabled {
        monitor_manager::stop_monitor_service(state.inner())?;
    }

    update_settings(&app, |current| {
        current.monitor_enabled = settings.monitor_enabled;
        current.monitor_port = settings.monitor_port;
        current.relay.enabled = settings.relay.enabled;
        Ok(())
    })?;
    if enabled {
        if let Err(error) =
            monitor_manager::start_monitor_service(&app, state.inner())
        {
            let _ = update_settings(&app, |current| {
                current.monitor_enabled = previous.monitor_enabled;
                current.monitor_port = previous.monitor_port;
                current.relay.enabled = previous.relay.enabled;
                Ok(())
            });
            if previous.monitor_enabled {
                let _ = monitor_manager::start_monitor_service(
                    &app,
                    state.inner(),
                );
            }
            return Err(error);
        }
    }

    if !enabled {
        stop_relay_client(state.inner());
    }
    monitor_manager::monitor_info_payload(&app, state.inner())
}

#[tauri::command]
pub(super) fn create_monitor_pairing(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    monitor_manager::create_pairing(&app, state.inner())
}

#[tauri::command]
pub(super) fn revoke_monitor_device(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    device_id: String,
) -> Result<Value, String> {
    let device_id = device_id.trim();
    if device_id.is_empty() {
        return Err("设备 ID 不能为空。".into());
    }
    if !monitor_manager::revoke_monitor_device_everywhere(
        &app,
        state.inner(),
        device_id,
    )? {
        return Err("没有找到该已配对设备。".into());
    }
    monitor_manager::monitor_info_payload(&app, state.inner())
}
