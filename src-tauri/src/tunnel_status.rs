use super::{
    monitor_status, output_text, parse_json_output, run_tunnel, runtime_not_running,
    RuntimePaths, RUNTIME_ALIAS,
};
use serde_json::Value;
use std::path::Path;

fn health_value_unhealthy(value: &Value) -> bool {
    match value {
        Value::Bool(ok) => !ok,
        Value::String(text) => matches!(
            text.to_ascii_lowercase().as_str(),
            "down" | "error" | "failed" | "failure" | "stale" | "unhealthy" | "disconnected"
        ),
        Value::Object(object) => {
            object.get("healthy").and_then(Value::as_bool) == Some(false)
                || object.get("ready").and_then(Value::as_bool) == Some(false)
                || object.get("ok").and_then(Value::as_bool) == Some(false)
                || object.get("state").map(health_value_unhealthy).unwrap_or(false)
                || object.get("status").map(health_value_unhealthy).unwrap_or(false)
        }
        _ => false,
    }
}
pub(super) fn runtime_payload_alive(payload: &Value) -> bool {
    if payload.get("process_running").and_then(Value::as_bool) == Some(false)
        || payload.get("healthy").and_then(Value::as_bool) == Some(false)
    {
        return false;
    }
    if payload.get("process_running").and_then(Value::as_bool) == Some(true)
        || payload.get("healthy").and_then(Value::as_bool) == Some(true)
    {
        return true;
    }
    matches!(
        payload
            .get("runtime_state")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "ready" | "running" | "healthy" | "connected" | "live"
    )
}

fn control_plane_health_value(payload: &Value) -> Option<&Value> {
    payload
        .get("control_plane_poll_health")
        .or_else(|| payload.pointer("/local/control_plane_poll_health"))
}
pub(super) fn runtime_payload_active(payload: &Value) -> bool {
    if payload.get("process_running").and_then(Value::as_bool) == Some(false) {
        return false;
    }
    if control_plane_health_value(payload)
        .map(health_value_unhealthy)
        .unwrap_or(false)
    {
        return false;
    }
    let ready = payload.get("ready").and_then(Value::as_bool);
    let healthy = payload.get("healthy").and_then(Value::as_bool);
    if ready == Some(false) || healthy == Some(false) {
        return false;
    }
    if let Some(ready) = ready {
        return ready;
    }
    if healthy == Some(true) {
        return true;
    }
    matches!(
        payload
            .get("runtime_state")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "ready" | "connected" | "live"
    )
}
#[derive(Debug, Clone)]
pub(super) struct ControlPlaneObservation {
    pub(super) state: String,
    pub(super) reason: String,
    pub(super) consecutive_failures: u32,
}

impl Default for ControlPlaneObservation {
    fn default() -> Self {
        Self {
            state: "unknown".into(),
            reason: String::new(),
            consecutive_failures: 0,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct RuntimeObservation {
    pub(super) state: String,
    pub(super) active: bool,
    pub(super) runtime_alive: bool,
    pub(super) payload: Option<Value>,
    pub(super) error: String,
    pub(super) control_plane: ControlPlaneObservation,
    pub(super) proxy_source: String,
}
#[derive(Debug, Clone)]
pub(super) struct RuntimeLogObservation {
    pub(super) control_plane: ControlPlaneObservation,
    pub(super) proxy_source: String,
}

fn control_plane_fallback(payload: &Value) -> ControlPlaneObservation {
    let mut fallback = ControlPlaneObservation::default();
    if let Some(value) = control_plane_health_value(payload) {
        match value {
            Value::String(text) => {
                fallback.state = text.to_ascii_lowercase();
            }
            Value::Bool(ok) => {
                fallback.state = if *ok { "healthy" } else { "down" }.into();
            }
            Value::Object(object) => {
                fallback.state = object
                    .get("state")
                    .and_then(Value::as_str)
                    .or_else(|| object.get("status").and_then(Value::as_str))
                    .unwrap_or("unknown")
                    .to_ascii_lowercase();
                fallback.reason = object
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars()
                    .take(300)
                    .collect();
            }
            _ => {}
        }
    }
    fallback
}
fn runtime_control_plane_events(payload: &Value) -> Vec<Value> {
    let Some(log_path) = payload.pointer("/local/log/path").and_then(Value::as_str) else {
        return Vec::new();
    };
    let tail = monitor_status::read_history_tail(Path::new(log_path), 512 * 1024);
    if tail.is_empty() {
        return Vec::new();
    }

    let mut instance_id: Option<String> = None;
    let mut events = Vec::new();
    for line in tail.lines().rev() {
        let Ok(event) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if event.get("component").and_then(Value::as_str) != Some("controlplane") {
            continue;
        }
        let current_id = event
            .get("client_instance_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        if instance_id.is_none() && !current_id.is_empty() {
            instance_id = Some(current_id.to_string());
        }
        if let Some(expected) = instance_id.as_deref() {
            if !current_id.is_empty() && current_id != expected {
                continue;
            }
        }
        events.push(event);
    }
    events
}

fn runtime_proxy_source_from_events(events: &[Value]) -> String {
    for event in events {
        if event.get("msg").and_then(Value::as_str) == Some("control-plane route resolved") {
            return event
                .get("proxy_source")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
        }
    }
    String::new()
}

fn control_plane_observation_from_events(
    mut fallback: ControlPlaneObservation,
    events: &[Value],
) -> ControlPlaneObservation {
    let mut failures = 0u32;
    let mut reason = String::new();
    let mut saw_start = false;
    for event in events {
        let message = event.get("msg").and_then(Value::as_str).unwrap_or("");
        match message {
            "poll failed; backing off" | "poll timed out; backing off" => {
                failures = failures.saturating_add(1);
                if reason.is_empty() {
                    reason = event
                        .get("error")
                        .and_then(Value::as_str)
                        .unwrap_or(message)
                        .chars()
                        .take(300)
                        .collect();
                }
            }
            "poller recovered; polling operational" | "tunnel metadata fetched" => {
                if failures == 0 {
                    return ControlPlaneObservation {
                        state: "healthy".into(),
                        reason: String::new(),
                        consecutive_failures: 0,
                    };
                }
                break;
            }
            "starting control-plane poller" | "poller started" => {
                saw_start = true;
                if failures == 0 {
                    break;
                }
            }
            _ => {}
        }
    }
    if failures >= 3 {
        return ControlPlaneObservation {
            state: "down".into(),
            reason,
            consecutive_failures: failures,
        };
    }
    if failures > 0 {
        return ControlPlaneObservation {
            state: "suspect".into(),
            reason,
            consecutive_failures: failures,
        };
    }
    if saw_start && fallback.state == "unknown" {
        fallback.state = "starting".into();
    }
    fallback
}

pub(super) fn runtime_log_observation(payload: &Value) -> RuntimeLogObservation {
    let fallback = control_plane_fallback(payload);
    let events = runtime_control_plane_events(payload);
    RuntimeLogObservation {
        control_plane: control_plane_observation_from_events(fallback, &events),
        proxy_source: runtime_proxy_source_from_events(&events),
    }
}
pub(super) fn runtime_status(
    app: &tauri::AppHandle,
    paths: &RuntimePaths,
) -> RuntimeObservation {
    let args = vec![
        "runtimes".into(),
        "status".into(),
        RUNTIME_ALIAS.into(),
        "--json".into(),
    ];
    match run_tunnel(app, paths, &args, None) {
        Ok(output) if output.status.success() => {
            let Some(parsed) = parse_json_output(&output) else {
                let text = output_text(&output);
                let error = if text.is_empty() {
                    "Tunnel 状态返回为空或不是有效 JSON。".to_string()
                } else {
                    format!("Tunnel 状态返回不是有效 JSON：{text}")
                };
                return RuntimeObservation {
                    state: "error".into(),
                    active: false,
                    runtime_alive: false,
                    payload: None,
                    error,
                    control_plane: ControlPlaneObservation::default(),
                    proxy_source: String::new(),
                };
            };
            let log_observation = runtime_log_observation(&parsed);
            let control_plane = log_observation.control_plane;
            let proxy_source = log_observation.proxy_source;
            let base_active = runtime_payload_active(&parsed);
            let active = base_active
                && !matches!(
                    control_plane.state.as_str(),
                    "down" | "suspect" | "starting"
                );
            let runtime_alive = runtime_payload_alive(&parsed);
            let state = parsed
                .get("runtime_state")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| if active { "ready".into() } else { "unknown".into() });
            RuntimeObservation {
                state,
                active,
                runtime_alive,
                payload: Some(parsed),
                error: String::new(),
                control_plane,
                proxy_source,
            }
        }
        Ok(output) => {
            let text = output_text(&output);
            if runtime_not_running(&text) {
                RuntimeObservation {
                    state: "stopped".into(),
                    active: false,
                    runtime_alive: false,
                    payload: None,
                    error: String::new(),
                    control_plane: ControlPlaneObservation::default(),
                    proxy_source: String::new(),
                }
            } else {
                RuntimeObservation {
                    state: "error".into(),
                    active: false,
                    runtime_alive: false,
                    payload: None,
                    error: text,
                    control_plane: ControlPlaneObservation::default(),
                    proxy_source: String::new(),
                }
            }
        }
        Err(error) => RuntimeObservation {
            state: "error".into(),
            active: false,
            runtime_alive: false,
            payload: None,
            error,
            control_plane: ControlPlaneObservation::default(),
            proxy_source: String::new(),
        },
    }
}
