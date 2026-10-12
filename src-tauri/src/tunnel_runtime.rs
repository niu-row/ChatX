use super::{
    desktop_commander_home, load_runtime_key, load_settings, mcp_command, output_text,
    parse_json_output, push_log, run_tunnel, runtime_key_saved, runtime_not_running,
    runtime_paths, state_dir, timestamp_ms, AppState, RuntimePaths, RUNTIME_ALIAS,
};
use crate::settings::{load_settings_checked, Settings};
use crate::connection_coordinator::ConnectionCoordinator;
use crate::connection_intent::{self, Transport};
use crate::tunnel_recovery::{
    ensure_runtime_start_intent, reconnect_delay_secs, restore_desired_connection_intent,
    set_recovery_status, update_runtime_snapshot,
};
use crate::tunnel_status::{runtime_status, ControlPlaneObservation, RuntimeObservation};
use serde_json::{json, Value};
use std::{
    fs,
    sync::atomic::Ordering,
    thread,
    time::{Duration, Instant},
};
use tauri::Manager;

pub(super) fn cancel_connection_intent(state: &AppState) {
    state.desired_connected.store(false, Ordering::SeqCst);
    state.reconnecting.store(false, Ordering::SeqCst);
    state.reconnect_attempt.store(0, Ordering::SeqCst);
    set_recovery_status(state, "stopped", 0, "");
    if let Ok(mut key) = state.session_runtime_key.lock() { *key = None; }
}

pub(super) fn stop_tunnel_without_saving(
    app: &tauri::AppHandle,
    state: &AppState,
    settings: &crate::settings::Settings,
) -> Result<(), String> {
    cancel_connection_intent(state);
    let _operation = state.runtime_operation.lock().map_err(|_| "Tunnel 操作锁已损坏。".to_string())?;
    stop_runtime(app, Some(state))?;
    let stopped = RuntimeObservation {
        state: "stopped".into(),
        active: false,
        runtime_alive: false,
        payload: None,
        error: String::new(),
        control_plane: ControlPlaneObservation::default(),
        proxy_source: String::new(),
    };
    update_runtime_snapshot(state, &stopped, false, settings);
    Ok(())
}

pub(super) fn stop_runtime(
    app: &tauri::AppHandle,
    state: Option<&AppState>,
) -> Result<Value, String> {
    let paths = runtime_paths(app)?;
    let args = vec![
        "runtimes".into(),
        "stop".into(),
        RUNTIME_ALIAS.into(),
        "--json".into(),
    ];
    let output = run_tunnel(app, &paths, &args, None)?;
    if let Some(state) = state {
        push_log(
            state,
            format!("停止 Tunnel runtime: {}", output_text(&output)),
        );
    }
    if output.status.success() {
        Ok(parse_json_output(&output)
            .unwrap_or_else(|| json!({"state":"stopped"})))
    } else {
        let text = output_text(&output);
        if runtime_not_running(&text) {
            Ok(json!({"state":"stopped"}))
        } else {
            Err(format!("停止 Tunnel 失败：{text}"))
        }
    }
}

pub(super) fn start_runtime_connection(
    app: &tauri::AppHandle,
    state: &AppState,
    tunnel_id: &str,
    key: &str,
    reconnect: bool,
    require_connection_intent: bool,
) -> Result<(), String> {
    let _operation = state
        .runtime_operation
        .lock()
        .map_err(|_| "Tunnel 操作锁已损坏。".to_string())?;
    let current_settings = load_settings(app);
    ensure_runtime_start_intent(
        state,
        &current_settings,
        require_connection_intent,
    )?;
    if reconnect && !current_settings.auto_reconnect {
        return Ok(());
    }

    let paths = runtime_paths(app)?;
    let profiles = state_dir(app)?.join("tunnel-profiles");
    let dc_home = desktop_commander_home(app)?;
    fs::create_dir_all(&profiles)
        .map_err(|error| format!("创建 Tunnel profile 目录失败：{error}"))?;
    fs::create_dir_all(&dc_home)
        .map_err(|error| format!("创建 Desktop Commander 数据目录失败：{error}"))?;

    match stop_runtime(app, None) {
        Ok(_) => {}
        Err(error) if reconnect => push_log(
            state,
            format!("自动重连清理旧 runtime 时继续执行：{error}"),
        ),
        Err(error) => return Err(error),
    }
    let args = vec![
        "runtimes".into(),
        "connect".into(),
        "--alias".into(),
        RUNTIME_ALIAS.into(),
        "--tunnel-id".into(),
        tunnel_id.to_string(),
        "--runtime-api-key".into(),
        "env:CHATX_TUNNEL_RUNTIME_KEY".into(),
        "--profile-dir".into(),
        profiles.to_string_lossy().into_owned(),
        "--mcp-command".into(),
        mcp_command(&paths, &dc_home),
        "--json".into(),
    ];
    push_log(
        state,
        if reconnect {
            "正在自动重连 Secure MCP Tunnel → Desktop Commander"
        } else {
            "正在启动 Secure MCP Tunnel → Desktop Commander"
        },
    );
    let output = run_tunnel(app, &paths, &args, Some(key))?;
    let text = output_text(&output);
    push_log(state, format!("Tunnel connect: {text}"));
    if !output.status.success() {
        return Err(format!("启动 Tunnel 失败：{text}"));
    }
    Ok(())
}
pub(super) fn restore_persisted_connection_state(
    app: &tauri::AppHandle,
    state: &AppState,
) {
    let mut settings = match load_settings_checked(app) {
        Ok(settings) => settings,
        Err(error) => {
            let fallback = Settings::default();
            restore_desired_connection_intent(state, &fallback);
            set_recovery_status(state, "stopped", 0, error.clone());
            push_log(
                state,
                format!(
                    "读取持久设置失败，按停止连接意图处理并清理残留 runtime：{error}"
                ),
            );
            let Ok(paths) = runtime_paths(app) else {
                return;
            };
            let mut observation = runtime_status(app, &paths);
            if observation.runtime_alive || observation.active {
                if let Err(stop_error) = stop_runtime(app, None) {
                    push_log(
                        state,
                        format!(
                            "设置不可用且清理残留 Tunnel runtime 失败：{stop_error}"
                        ),
                    );
                } else {
                    observation = runtime_status(app, &paths);
                }
            }
            update_runtime_snapshot(
                state,
                &observation,
                false,
                &fallback,
            );
            return;
        }
    };

    if settings.https_mcp.enabled && settings.connection_intent {
        // Legacy versions could persist both intents. HTTPS takes precedence
        // during the one-time transition to mutually exclusive transports.
        settings.connection_intent = false;
        if let Err(error) = connection_intent::mark_stopped(app, Transport::Tunnel) {
            push_log(state, format!("迁移双链路设置时记录 Tunnel 停止标记失败：{error}"));
        }
        if let Err(error) = crate::settings::update_settings(app, |current| {
            if current.https_mcp.enabled { current.connection_intent = false; }
            Ok(())
        }) {
            push_log(state, format!("迁移互斥连接设置失败（本次仍仅恢复 HTTPS）：{error}"));
        }
        push_log(state, "检测到旧版双链路连接意图，已优先保留 HTTPS MCP、关闭 Tunnel");
    }
    if settings.connection_intent {
        match connection_intent::restore_allowed(app, Transport::Tunnel) {
            Ok(true) => {}
            Ok(false) => {
                settings.connection_intent = false;
                push_log(state, "检测到手动停止标记，不自动恢复 Tunnel");
            }
            Err(error) => {
                settings.connection_intent = false;
                push_log(state, format!("无法验证 Tunnel 停止标记，拒绝自动重连：{error}"));
            }
        }
    }
    restore_desired_connection_intent(state, &settings);
    let Ok(paths) = runtime_paths(app) else {
        return;
    };
    let mut observation = runtime_status(app, &paths);

    if !settings.connection_intent
        && (observation.runtime_alive || observation.active)
    {
        push_log(
            state,
            "检测到未提交或残留的 Tunnel runtime；持久连接意图为停止，正在清理 runtime",
        );
        match stop_runtime(app, None) {
            Ok(_) => {
                observation = runtime_status(app, &paths);
            }
            Err(error) => {
                push_log(
                    state,
                    format!("清理残留 Tunnel runtime 失败，连接意图仍保持停止：{error}"),
                );
            }
        }
    }

    update_runtime_snapshot(
        state,
        &observation,
        settings.connection_intent,
        &settings,
    );
    if !settings.connection_intent {
        return;
    }
    if settings.tunnel_id.trim().is_empty() {
        state.desired_connected.store(false, Ordering::SeqCst);
        set_recovery_status(
            state,
            "failed",
            0,
            "持久连接意图存在，但 Tunnel ID 为空。",
        );
        return;
    }
    if runtime_key_saved(app) {
        if let Ok(key) = load_runtime_key(app, "") {
            if let Ok(mut session_key) = state.session_runtime_key.lock() {
                *session_key = Some(key);
            }
        }
    }
    push_log(
        state,
        if observation.active {
            "检测到现有 Tunnel runtime，已恢复连接意图"
        } else {
            "已恢复持久化 Tunnel 连接意图，等待健康监测/自动重连"
        },
    );
}

pub(super) fn wait_for_runtime_recovery(
    app: &tauri::AppHandle,
    state: &AppState,
    paths: &RuntimePaths,
    require_auto_reconnect: bool,
) -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let settings = load_settings(app);
        let desired_connected =
            state.desired_connected.load(Ordering::SeqCst);
        if !desired_connected
            || (require_auto_reconnect && !settings.auto_reconnect)
        {
            return false;
        }

        let observation = runtime_status(app, paths);
        update_runtime_snapshot(
            state,
            &observation,
            desired_connected,
            &settings,
        );
        if observation.active {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        thread::sleep(Duration::from_secs(2));
    }
}

pub(super) fn start_reconnect_monitor(app: tauri::AppHandle) {
    thread::spawn(move || {
        let mut inactive_checks = 0u32;
        let mut next_reconnect_at: Option<Instant> = None;
        loop {
            thread::sleep(Duration::from_secs(4));
            let state = app.state::<AppState>();
            if state.quitting.load(Ordering::SeqCst) {
                break;
            }
            let settings = load_settings(&app);
            // Fail closed if a stale Tunnel intent survived a mode switch.
            if settings.https_mcp.enabled {
                if state.desired_connected.swap(false, Ordering::SeqCst) {
                    push_log(&state, "HTTPS MCP 已选择，停止 Tunnel 自动重连");
                }
                state.reconnecting.store(false, Ordering::SeqCst);
            }
            let desired_connected =
                state.desired_connected.load(Ordering::SeqCst);
            let paths = match runtime_paths(&app) {
                Ok(paths) => paths,
                Err(error) => {
                    set_recovery_status(
                        state.inner(),
                        "failed",
                        0,
                        error.clone(),
                    );
                    push_log(
                        &state,
                        format!("Tunnel 健康检查失败：{error}"),
                    );
                    continue;
                }
            };
            let observation = runtime_status(&app, &paths);
            update_runtime_snapshot(
                state.inner(),
                &observation,
                desired_connected,
                &settings,
            );
            if !desired_connected {
                inactive_checks = 0;
                next_reconnect_at = None;
                set_recovery_status(state.inner(), "stopped", 0, "");
                state.reconnecting.store(false, Ordering::SeqCst);
                state.reconnect_attempt.store(0, Ordering::SeqCst);
                continue;
            }
            if observation.active {
                if state.reconnecting.swap(false, Ordering::SeqCst) {
                    push_log(&state, "Secure MCP Tunnel 已恢复连接");
                }
                set_recovery_status(state.inner(), "idle", 0, "");
                state.reconnect_attempt.store(0, Ordering::SeqCst);
                inactive_checks = 0;
                next_reconnect_at = None;
                continue;
            }
            if !settings.auto_reconnect {
                inactive_checks = 0;
                next_reconnect_at = None;
                set_recovery_status(state.inner(), "disabled", 0, "");
                state.reconnecting.store(false, Ordering::SeqCst);
                state.reconnect_attempt.store(0, Ordering::SeqCst);
                continue;
            }
            if state.reconnecting.load(Ordering::SeqCst) {
                continue;
            }

            inactive_checks = inactive_checks.saturating_add(1);
            let failure_threshold =
                if observation.control_plane.state == "down" {
                    1
                } else if observation.runtime_alive {
                    8
                } else {
                    2
                };
            if inactive_checks < failure_threshold {
                set_recovery_status(
                    state.inner(),
                    "detecting",
                    0,
                    "",
                );
                continue;
            }
            if next_reconnect_at
                .is_some_and(|next| Instant::now() < next)
            {
                continue;
            }
            if state.reconnecting.swap(true, Ordering::SeqCst) {
                continue;
            }

            let attempt =
                state.reconnect_attempt.fetch_add(1, Ordering::SeqCst) + 1;
            let delay = reconnect_delay_secs(attempt);
            let key = state
                .session_runtime_key
                .lock()
                .ok()
                .and_then(|value| value.clone());
            let Some(key) = key else {
                next_reconnect_at =
                    Some(Instant::now() + Duration::from_secs(delay));
                let next_at = timestamp_ms()
                    .saturating_add(delay.saturating_mul(1000));
                set_recovery_status(
                    state.inner(),
                    "blocked",
                    next_at,
                    "Desktop 当前没有可用于自动重连的 Runtime Key。",
                );
                push_log(
                    &state,
                    format!(
                        "Tunnel 已断开，但本次会话没有可用于自动重连的 Runtime Key；{delay} 秒后继续检查"
                    ),
                );
                state.reconnecting.store(false, Ordering::SeqCst);
                continue;
            };
            let detail = if !observation.error.is_empty() {
                observation.error.clone()
            } else if !observation.control_plane.reason.is_empty() {
                format!(
                    "control-plane {}: {}",
                    observation.control_plane.state,
                    observation.control_plane.reason
                )
            } else {
                observation.state.clone()
            };
            set_recovery_status(
                state.inner(),
                "reconnecting",
                0,
                "",
            );
            push_log(
                &state,
                format!(
                    "检测到 Tunnel 掉线（{detail}），开始第 {attempt} 次自动重连"
                ),
            );
            if !state.desired_connected.load(Ordering::SeqCst)
                || !load_settings(&app).auto_reconnect
            {
                state.reconnecting.store(false, Ordering::SeqCst);
                continue;
            }

            // Auto-reconnect must take the same cross-transport lifecycle
            // lock as manual start/stop, otherwise it could bring Tunnel back
            // after HTTPS MCP completed a mode switch.
            let reconnect_result = {
                let guard = ConnectionCoordinator::acquire(&app, state.inner());
                match guard {
                    Err(_) => Err("连接操作锁不可用，拒绝 Tunnel 自动重连".to_string()),
                    Ok(_guard) => {
                        let latest = load_settings(&app);
                        if !state.desired_connected.load(Ordering::SeqCst)
                            || state.quitting.load(Ordering::SeqCst)
                            || latest.https_mcp.enabled
                            || !latest.auto_reconnect
                        {
                            Err("已停止 Tunnel 或切换至 HTTPS MCP，不执行自动重连".into())
                        } else {
                            start_runtime_connection(
                                &app, state.inner(), &latest.tunnel_id,
                                &key, true, true,
                            )
                        }
                    }
                }
            };
            match reconnect_result {
                Ok(())
                    if state
                        .desired_connected
                        .load(Ordering::SeqCst) =>
                {
                    set_recovery_status(
                        state.inner(),
                        "verifying",
                        0,
                        "",
                    );
                    push_log(
                        &state,
                        format!(
                            "第 {attempt} 次自动重连命令已完成，正在验证 Tunnel Ready"
                        ),
                    );
                    if wait_for_runtime_recovery(
                        &app,
                        state.inner(),
                        &paths,
                        true,
                    ) {
                        set_recovery_status(
                            state.inner(),
                            "idle",
                            0,
                            "",
                        );
                        push_log(
                            &state,
                            format!(
                                "第 {attempt} 次自动重连已确认 Tunnel 恢复"
                            ),
                        );
                        state
                            .reconnect_attempt
                            .store(0, Ordering::SeqCst);
                        inactive_checks = 0;
                        next_reconnect_at = None;
                    } else if state
                        .desired_connected
                        .load(Ordering::SeqCst)
                        && load_settings(&app).auto_reconnect
                    {
                        next_reconnect_at =
                            Some(Instant::now() + Duration::from_secs(delay));
                        let next_at = timestamp_ms()
                            .saturating_add(delay.saturating_mul(1000));
                        let error =
                            "重连命令已完成，但 15 秒内未恢复 Tunnel Ready。";
                        set_recovery_status(
                            state.inner(),
                            "backoff",
                            next_at,
                            error,
                        );
                        push_log(
                            &state,
                            format!(
                                "第 {attempt} 次自动重连命令已完成，但 15 秒内未恢复 Ready；{delay} 秒后重试"
                            ),
                        );
                    } else {
                        next_reconnect_at = None;
                    }
                }
                Ok(()) => {
                    next_reconnect_at = None;
                }
                Err(error) => {
                    next_reconnect_at =
                        Some(Instant::now() + Duration::from_secs(delay));
                    let next_at = timestamp_ms()
                        .saturating_add(delay.saturating_mul(1000));
                    set_recovery_status(
                        state.inner(),
                        "backoff",
                        next_at,
                        error.clone(),
                    );
                    push_log(
                        &state,
                        format!(
                            "第 {attempt} 次自动重连失败：{error}；{delay} 秒后重试"
                        ),
                    );
                }
            }
            state.reconnecting.store(false, Ordering::SeqCst);
        }
    });
}
