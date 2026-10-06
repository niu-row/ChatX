use super::*;
use std::{
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::TryLockError,
};

pub(super) fn parse_recent_history(text: &str, limit: usize) -> Vec<Value> {
    text.lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|record| {
            let tool_name = record.get("toolName")?.as_str()?.to_string();
            let timestamp = record
                .get("timestamp")
                .and_then(Value::as_str)
                .map(str::to_string);
            let duration_ms = record.get("duration").and_then(Value::as_u64);
            let success = record
                .get("output")
                .and_then(|output| output.get("isError"))
                .and_then(Value::as_bool)
                != Some(true);
            Some(json!({
                "toolName": tool_name,
                "timestamp": timestamp,
                "startedAt": Value::Null,
                "durationMs": duration_ms,
                "success": success,
                "running": false
            }))
        })
        .take(limit)
        .collect()
}

pub(super) fn read_history_tail(path: &Path, max_bytes: u64) -> String {
    let Ok(mut file) = fs::File::open(path) else {
        return String::new();
    };
    let Ok(metadata) = file.metadata() else {
        return String::new();
    };
    let start = metadata.len().saturating_sub(max_bytes);
    if file.seek(SeekFrom::Start(start)).is_err() {
        return String::new();
    }
    let mut bytes = Vec::new();
    if file.read_to_end(&mut bytes).is_err() {
        return String::new();
    }
    let mut text = String::from_utf8_lossy(&bytes).to_string();
    if start > 0 {
        if let Some(index) = text.find('\n') {
            text = text[index + 1..].to_string();
        }
    }
    text
}

pub(super) fn recent_calls(
    app: &tauri::AppHandle,
    activity: Option<&monitor::ActivitySnapshot>,
    now_ms: u64,
) -> Vec<Value> {
    const LIMIT: usize = 20;
    let mut calls = Vec::new();
    if let Some(activity) = activity {
        for call in activity.in_flight_calls.iter().take(LIMIT) {
            calls.push(json!({
                "id": call.id,
                "toolName": call.tool_name,
                "timestamp": Value::Null,
                "startedAt": call.started_at,
                "durationMs": now_ms.saturating_sub(call.started_at),
                "success": Value::Null,
                "running": true
            }));
        }
        if calls.is_empty() && activity.in_flight > 0 {
            if let (Some(tool_name), Some(started_at)) = (
                activity.last_tool_name.as_ref(),
                activity.last_call_started_at,
            ) {
                calls.push(json!({
                    "toolName": tool_name,
                    "timestamp": Value::Null,
                    "startedAt": started_at,
                    "durationMs": now_ms.saturating_sub(started_at),
                    "success": Value::Null,
                    "running": true
                }));
            }
        }
    }
    if calls.len() < LIMIT {
        if let Ok(path) = call_history_path(app) {
            let tail = read_history_tail(&path, 256 * 1024);
            calls.extend(parse_recent_history(&tail, LIMIT - calls.len()));
        }
    }
    calls
}

pub(super) fn payload(app: &tauri::AppHandle, state: &AppState) -> Value {
    payload_with_tunnel(app, state, None)
}

fn payload_with_tunnel(
    app: &tauri::AppHandle,
    state: &AppState,
    tunnel_override: Option<monitor::TunnelSnapshot>,
) -> Value {
    let now_ms = timestamp_ms();
    let activity = monitor_activity_path(app)
        .ok()
        .and_then(|path| monitor::read_activity_snapshot(&path));
    let mcp_status = monitor::evaluate_activity(activity.as_ref(), now_ms);
    let recent_calls = recent_calls(app, activity.as_ref(), now_ms);
    let mut mcp = serde_json::to_value(mcp_status).unwrap_or_else(|_| json!({}));
    if let Value::Object(object) = &mut mcp {
        object.insert("recentCalls".into(), Value::Array(recent_calls));
    }
    let tunnel = tunnel_override.unwrap_or_else(|| {
        state
            .runtime_snapshot
            .lock()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_default()
    });
    let settings = load_settings(app);
    let endpoints =
        monitor_network::discover_monitor_endpoints(settings.monitor_port);
    json!({
        "schemaVersion": 1,
        "serverTime": now_ms,
        "endpoints": endpoints,
        "chatx": {
            "version": APP_VERSION,
            "platform": format!(
                "{}-{}",
                std::env::consts::OS,
                std::env::consts::ARCH
            )
        },
        "host": host_snapshot(),
        "tunnel": {
            "state": tunnel.state,
            "health": tunnel.health,
            "active": tunnel.active,
            "updatedAt": tunnel.updated_at,
            "lastProbeAt": tunnel.last_probe_at,
            "lastSuccessfulProbeAt": tunnel.last_successful_probe_at,
            "consecutiveFailures": tunnel.consecutive_failures,
            "controlPlaneState": tunnel.control_plane_state,
            "controlPlaneReason": tunnel.control_plane_reason,
            "controlPlaneFailures": tunnel.control_plane_failures,
            "proxyMode": tunnel.proxy_mode,
            "proxySource": tunnel.proxy_source,
            "desiredConnected": state.desired_connected.load(Ordering::SeqCst),
            "reconnecting": state.reconnecting.load(Ordering::SeqCst),
            "reconnectAttempt": state.reconnect_attempt.load(Ordering::SeqCst)
        },
        "mcp": mcp
    })
}

pub(super) fn project_runtime_snapshot(
    current: &monitor::TunnelSnapshot,
    observation: &RuntimeObservation,
    desired_connected: bool,
    settings: &Settings,
    advance_failures: bool,
) -> monitor::TunnelSnapshot {
    let now = timestamp_ms();
    let local_failures = if observation.active || !desired_connected {
        0
    } else if advance_failures {
        current.consecutive_failures.saturating_add(1)
    } else {
        current.consecutive_failures
    };
    let failures = if observation.control_plane.consecutive_failures > 0 {
        observation.control_plane.consecutive_failures
    } else {
        local_failures
    };

    let mut snapshot = current.clone();
    snapshot.state = observation.state.clone();
    snapshot.active = observation.active;
    snapshot.last_error = observation.error.clone();
    snapshot.updated_at = now;
    snapshot.last_probe_at = now;
    snapshot.consecutive_failures = failures;
    snapshot.control_plane_state = observation.control_plane.state.clone();
    snapshot.control_plane_reason = observation.control_plane.reason.clone();
    snapshot.control_plane_failures =
        observation.control_plane.consecutive_failures;
    snapshot.proxy_mode = settings.proxy.mode.clone();
    snapshot.proxy_source = observation.proxy_source.clone();

    if !desired_connected {
        snapshot.health = "stopped".into();
    } else if observation.control_plane.state == "down" {
        snapshot.health = "down".into();
    } else if matches!(
        observation.control_plane.state.as_str(),
        "suspect" | "starting"
    ) {
        snapshot.health = "suspect".into();
    } else if observation.active {
        snapshot.last_successful_probe_at = now;
        snapshot.health = "healthy".into();
    } else if failures >= if observation.runtime_alive { 8 } else { 2 } {
        snapshot.health = "down".into();
    } else {
        snapshot.health = "suspect".into();
    }
    snapshot
}

pub(super) fn fresh_payload(
    app: &tauri::AppHandle,
    state: &AppState,
) -> Result<Value, String> {
    let _operation = match state.runtime_operation.try_lock() {
        Ok(lock) => lock,
        Err(TryLockError::WouldBlock) => {
            return Err(
                "Tunnel 正在执行连接、停止或重连操作，请稍后刷新。".into(),
            );
        }
        Err(TryLockError::Poisoned(_)) => {
            return Err("Tunnel 操作锁已损坏。".into());
        }
    };
    let paths = runtime_paths(app)?;
    let settings = load_settings(app);
    let observation = runtime_status(app, &paths);
    let current = state
        .runtime_snapshot
        .lock()
        .map(|value| value.clone())
        .unwrap_or_default();
    let tunnel = project_runtime_snapshot(
        &current,
        &observation,
        state.desired_connected.load(Ordering::SeqCst),
        &settings,
        false,
    );
    Ok(payload_with_tunnel(app, state, Some(tunnel)))
}
