use super::{
    desktop_commander_home, effective_proxy_url, executable_version,
    mcp_command, probe_runtime_key_remote,
    runtime_paths, state_dir, timestamp_ms, RuntimeKeyRemoteProbe,
};
use crate::{
    app_state::AppState,
    mcp_sources, monitor, monitor_status,
    secrets::{key_storage, load_runtime_key, runtime_key_saved},
    settings::load_settings,
    tunnel_status::runtime_status,
};
use serde_json::{json, Value};
use tauri::State;

#[tauri::command]
pub(super) fn get_mcp_live_status(
    app: tauri::AppHandle, state: State<'_, AppState>,
) -> Result<Value, String> {
    let now_ms = timestamp_ms();
    let sources = mcp_sources::activities(&app, state.inner(), now_ms)?;
    let status = monitor::evaluate_activity(mcp_sources::primary_activity(&sources), now_ms);
    let mut in_flight_calls = Vec::new();
    for (source, activity) in &sources {
        in_flight_calls.extend(monitor_status::in_flight_calls(activity.as_ref(), now_ms)
            .into_iter().map(|value| mcp_sources::annotate(value, source)));
    }
    mcp_sources::sort_recent_by_time(&mut in_flight_calls, 20);
    Ok(json!({
        "serverTime": now_ms,
        "status": status,
        "transports": mcp_sources::monitor_statuses(&sources, now_ms),
        "recentCalls": in_flight_calls
    }))
}

#[tauri::command(async)]
pub(super) fn run_diagnostics(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    let mut checks = Vec::new();
    match runtime_paths(&app) {
        Ok(paths) => {
            let dc_home = desktop_commander_home(&app)?;
            checks.push(json!({"name":"tunnel-client","ok":true,"detail":executable_version(&paths.tunnel)}));
            checks.push(json!({"name":"Node.js","ok":paths.node.is_file(),"detail":paths.node.to_string_lossy()}));
            checks.push(json!({"name":"Desktop Commander","ok":paths.desktop_commander.is_file(),"detail":paths.desktop_commander.to_string_lossy()}));
            checks.push(json!({"name":"Desktop Commander isolation","ok":paths.launcher.is_file(),"detail":dc_home.to_string_lossy()}));
            checks.push(json!({"name":"MCP command","ok":true,"detail":mcp_command(&paths, &dc_home)}));
            let observation = runtime_status(&app, &paths);
            let runtime_detail = if observation.error.is_empty() {
                observation.payload.clone().map(|value| value.to_string()).unwrap_or_else(|| observation.state.clone())
            } else {
                observation.error.clone()
            };
            checks.push(json!({"name":"Tunnel runtime","ok":observation.active,"detail":runtime_detail}));
            checks.push(json!({
                "name":"Control plane",
                "ok":observation.control_plane.state == "healthy",
                "detail":format!(
                    "state={} failures={} {}",
                    observation.control_plane.state,
                    observation.control_plane.consecutive_failures,
                    observation.control_plane.reason
                ).trim().to_string()
            }));
        }
        Err(error) => checks.push(json!({"name":"Bundled runtime","ok":false,"detail":error}))
    }
    let snapshot = state.runtime_snapshot.lock().map(|value| value.clone()).unwrap_or_default();
    let now_ms = timestamp_ms();
    let probe_age = now_ms.saturating_sub(snapshot.last_probe_at);
    checks.push(json!({"name":"Tunnel probe freshness","ok":snapshot.last_probe_at > 0 && probe_age <= 12_000,"detail":format!("{} ms ago; health={}; failures={}", probe_age, snapshot.health, snapshot.consecutive_failures)}));
    let sources = mcp_sources::activities(&app, state.inner(), now_ms)?;
    let remote_activity = mcp_sources::primary_activity(&sources);
    for (source, snapshot) in &sources {
        let last_request = snapshot.as_ref().and_then(|activity| activity.last_request_at);
        checks.push(json!({
            "name": format!("{source} MCP activity"),
            "ok": last_request.is_some(),
            "detail": last_request.map(|time| format!("last request {} ms ago", now_ms.saturating_sub(time)))
                .unwrap_or_else(|| "not observed in this transport".into())
        }));
    }
    match remote_activity {
        Some(activity) => {
            let launcher_age = activity.launcher_started_at
                .map(|started_at| now_ms.saturating_sub(started_at));
            let request_age = activity.last_request_at
                .map(|request_at| now_ms.saturating_sub(request_at));
            let list_tools_age = activity.last_list_tools_at
                .map(|request_at| now_ms.saturating_sub(request_at));
            let call_age = activity.last_call_started_at
                .map(|request_at| now_ms.saturating_sub(request_at));
            checks.push(json!({
                "name":"Remote MCP activity",
                "ok":activity.last_request_at.is_some(),
                "detail":match (activity.last_request_at, request_age) {
                    (Some(_), Some(age)) => format!("last={} {} ms ago; launcher age={} ms",
                        activity.last_request_method.as_deref().unwrap_or("unknown"),
                        age,
                        launcher_age.unwrap_or(0)),
                    _ => format!("no remote MCP request observed since launcher start; launcher age={} ms; ChatGPT Connector session may be stale -- try a new chat or reconnect the Connector", launcher_age.unwrap_or(0)),
                }
            }));
            checks.push(json!({
                "name":"Remote tools/list",
                "ok":activity.last_list_tools_at.is_some(),
                "detail":list_tools_age.map(|age| format!("{} ms ago", age))
                    .unwrap_or_else(|| "not observed in the current launcher session".into())
            }));
            checks.push(json!({
                "name":"Remote tools/call",
                "ok":activity.last_call_started_at.is_some(),
                "detail":call_age.map(|age| format!("{} ms ago; last tool={}", age, activity.last_tool_name.as_deref().unwrap_or("unknown")))
                    .unwrap_or_else(|| "not observed in the current launcher session".into())
            }));
        }
        None => {
            checks.push(json!({"name":"Remote MCP activity","ok":false,"detail":"no launcher activity snapshot is available"}));
            checks.push(json!({"name":"Remote tools/list","ok":false,"detail":"not observed"}));
            checks.push(json!({"name":"Remote tools/call","ok":false,"detail":"not observed"}));
        }
    }
    let settings = load_settings(&app);
    let saved = runtime_key_saved(&app);
    let session_key = state.session_runtime_key.lock().ok().and_then(|value| value.clone())
        .filter(|value| !value.trim().is_empty());
    let saved_key = if saved { Some(load_runtime_key(&app, "")) } else { None };
    let (local_key_ok, local_key_detail) = match &saved_key {
        Some(Ok(_)) => (true, format!("{} saved and readable", key_storage())),
        Some(Err(error)) => (false, format!("{} exists but cannot be read: {error}", key_storage())),
        None if session_key.is_some() => (true, "available in the current session; not persisted".into()),
        None => (false, "not available; enter a Runtime Key when connecting".into()),
    };
    let probe_key = session_key.as_deref()
        .or_else(|| saved_key.as_ref().and_then(|result| result.as_ref().ok()).map(String::as_str));
    let remote_probe = match (probe_key, effective_proxy_url(&settings)) {
        (Some(key), Ok(proxy_url)) => {
            probe_runtime_key_remote(&settings.tunnel_id, key, proxy_url.as_deref())
        }
        (Some(_), Err(error)) => RuntimeKeyRemoteProbe {
            ok: false,
            state: "proxy_error",
            detail: format!("Could not resolve ChatX proxy settings for remote verification: {error}"),
        },
        (None, _) => RuntimeKeyRemoteProbe {
            ok: false,
            state: "not_available",
            detail: "No readable Runtime Key is available for remote verification.".into(),
        },
    };
    checks.push(json!({"name":"Tunnel ID","ok":settings.tunnel_id.starts_with("tunnel_"),"detail":settings.tunnel_id}));
    checks.push(json!({"name":"Runtime Key local","ok":local_key_ok,"detail":local_key_detail}));
    checks.push(json!({
        "name":"Runtime Key remote auth",
        "ok":remote_probe.ok,
        "detail":format!("state={} {}", remote_probe.state, remote_probe.detail)
    }));
    Ok(json!({"checks":checks}))
}

#[tauri::command]
pub(super) fn open_logs(app: tauri::AppHandle) -> Result<(), String> {
    open::that(state_dir(&app)?).map_err(|e| format!("打开数据目录失败：{e}"))
}

#[tauri::command]
pub(super) fn open_external(url: String) -> Result<(), String> {
    const ALLOWED_PREFIXES: &[&str] = &[
        "https://platform.openai.com/", "https://developers.openai.com/", "https://chatgpt.com/",
        "https://github.com/openai/tunnel-client", "https://github.com/wonderwhy-er/DesktopCommanderMCP"
    ];
    if !ALLOWED_PREFIXES.iter().any(|prefix| url.starts_with(prefix)) { return Err("不允许打开未列入白名单的外部地址。".into()); }
    open::that(url).map_err(|e| format!("打开浏览器失败：{e}"))
}
