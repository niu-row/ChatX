use chrono::DateTime;
use super::{
    host_snapshot, load_settings, mcp_sources, monitor,
    monitor_network, reconnect_capability, recovery_status, runtime_paths, runtime_status,
    timestamp_ms, AppState, RuntimeObservation, Settings, APP_VERSION,
};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    sync::{atomic::Ordering, Mutex, OnceLock, TryLockError},
    time::{Duration, Instant, SystemTime},
};

const RECENT_HISTORY_LIMIT: usize = 20;
const RECENT_HISTORY_TAIL_BYTES: u64 = 256 * 1024;
const ENDPOINT_CACHE_TTL: Duration = Duration::from_secs(15);

#[derive(Clone)]
struct RecentHistoryCache {
    len: u64,
    modified: Option<SystemTime>,
    calls: Vec<Value>,
}

struct EndpointCache {
    port: u16,
    refreshed_at: Instant,
    endpoints: Vec<monitor_network::MonitorEndpoint>,
}

static RECENT_HISTORY_CACHE: OnceLock<Mutex<HashMap<PathBuf, RecentHistoryCache>>> = OnceLock::new();
static ENDPOINT_CACHE: OnceLock<Mutex<Option<EndpointCache>>> = OnceLock::new();

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
            // Desktop Commander writes history at completion. Sort completed
            // and in-flight records by the same invocation start time.
            let started_at = timestamp.as_deref()
                .and_then(|text| DateTime::parse_from_rfc3339(text).ok())
                .and_then(|time| u64::try_from(time.timestamp_millis()).ok())
                .map(|finished_at| finished_at.saturating_sub(duration_ms.unwrap_or(0)));
            let success = record
                .get("output")
                .and_then(|output| output.get("isError"))
                .and_then(Value::as_bool)
                != Some(true);
            Some(json!({
                "toolName": tool_name,
                "timestamp": timestamp,
                "startedAt": started_at,
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

fn recent_history_calls(app: &tauri::AppHandle) -> Vec<Value> {
    let mut records = Vec::new();
    let paths = mcp_sources::history_paths(app).unwrap_or_default();
    let cache = RECENT_HISTORY_CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    for (source, path) in paths {
        let Ok(metadata) = fs::metadata(&path) else {
            if let Ok(mut cached) = cache.lock() { cached.remove(&path); }
            continue;
        };
        let len = metadata.len();
        let modified = metadata.modified().ok();
        let cached = cache.lock().ok()
            .and_then(|items| items.get(&path).cloned())
            .filter(|item| item.len == len && item.modified == modified);
        let calls = if let Some(cached) = cached {
            cached.calls
        } else {
            let tail = read_history_tail(&path, RECENT_HISTORY_TAIL_BYTES);
            let calls = parse_recent_history(&tail, RECENT_HISTORY_LIMIT).into_iter()
                .map(|item| mcp_sources::annotate(item, source)).collect::<Vec<_>>();
            if let Ok(mut cached) = cache.lock() {
                cached.insert(path.clone(), RecentHistoryCache {
                    len, modified, calls: calls.clone(),
                });
            }
            calls
        };
        records.extend(calls);
    }
    mcp_sources::sort_recent_by_time(&mut records, RECENT_HISTORY_LIMIT);
    records
}

fn monitor_endpoints(port: u16) -> Vec<monitor_network::MonitorEndpoint> {
    let cache = ENDPOINT_CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(cached) = cache.lock() {
        if let Some(cached) = cached.as_ref() {
            if cached.port == port && cached.refreshed_at.elapsed() < ENDPOINT_CACHE_TTL {
                return cached.endpoints.clone();
            }
        }
    }

    let endpoints = monitor_network::discover_monitor_endpoints(port);
    if let Ok(mut cached) = cache.lock() {
        *cached = Some(EndpointCache {
            port,
            refreshed_at: Instant::now(),
            endpoints: endpoints.clone(),
        });
    }
    endpoints
}

pub(super) fn in_flight_calls(
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
    calls
}

fn same_recent_call(a: &Value, b: &Value) -> bool {
    if a.get("source") != b.get("source") || a.get("toolName") != b.get("toolName") {
        return false;
    }

    let a_running = a.get("running").and_then(Value::as_bool) == Some(true);
    let b_running = b.get("running").and_then(Value::as_bool) == Some(true);
    if a_running || b_running {
        // In-flight calls have a stable ID. A finished history entry has no
        // ID, so a nearby same-named call is NOT proof of duplication.
        return a_running && b_running
            && a.get("id").and_then(Value::as_str)
                .zip(b.get("id").and_then(Value::as_str))
                .is_some_and(|(left, right)| left == right);
    }

    // Only deduplicate the synthetic, just-finished activity row against a
    // persisted history row. Two genuine history records remain distinct.
    let a_persisted = a.get("timestamp").and_then(Value::as_str).is_some_and(|v| !v.is_empty());
    let b_persisted = b.get("timestamp").and_then(Value::as_str).is_some_and(|v| !v.is_empty());
    if a_persisted && b_persisted { return false; }

    let close_starts = a.get("startedAt").and_then(Value::as_u64)
        .zip(b.get("startedAt").and_then(Value::as_u64))
        .is_some_and(|(left, right)| left.abs_diff(right) <= 100);
    let close_durations = a.get("durationMs").and_then(Value::as_u64)
        .zip(b.get("durationMs").and_then(Value::as_u64))
        .is_some_and(|(left, right)| left.abs_diff(right) <= 150);
    close_starts && close_durations
}

// Desktop Commander buffers completed history writes for up to one second.
// Bridge that short gap with the last completed activity, rather than making
// the row vanish on one refresh and reappear on the next.
fn merge_recent_calls(
    mut calls: Vec<Value>,
    sources: &[(&'static str, Option<monitor::ActivitySnapshot>)],
    now_ms: u64,
) -> Vec<Value> {
    for (source, activity) in sources {
        for running in in_flight_calls(activity.as_ref(), now_ms) {
            let item = mcp_sources::annotate(running, source);
            if !calls.iter().any(|saved| same_recent_call(saved, &item)) {
                calls.push(item);
            }
        }
        let Some(activity) = activity.as_ref() else { continue };
        if let (Some(started_at), Some(finished_at), Some(tool_name)) = (
            activity.last_call_started_at,
            activity.last_call_finished_at,
            activity.last_tool_name.as_ref(),
        ) {
            if finished_at >= started_at
                && finished_at <= now_ms.saturating_add(5_000)
                && now_ms.saturating_sub(finished_at) <= 15_000
            {
                let item = mcp_sources::annotate(json!({
                    "toolName": tool_name,
                    "timestamp": Value::Null,
                    "startedAt": started_at,
                    "durationMs": activity.last_duration_ms,
                    "success": activity.last_success,
                    "running": false
                }), source);
                if !calls.iter().any(|saved| same_recent_call(saved, &item)) {
                    calls.push(item);
                }
            }
        }
    }
    mcp_sources::sort_recent_by_time(&mut calls, RECENT_HISTORY_LIMIT);
    calls
}

pub(super) fn recent_calls(
    app: &tauri::AppHandle,
    sources: &[(&'static str, Option<monitor::ActivitySnapshot>)],
    now_ms: u64,
) -> Vec<Value> {
    merge_recent_calls(recent_history_calls(app), sources, now_ms)
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
    let sources = mcp_sources::activities(app, state, now_ms).unwrap_or_default();
    let mcp_status = monitor::evaluate_activity(mcp_sources::primary_activity(&sources), now_ms);
    let recent_calls = recent_calls(app, &sources, now_ms);
    let mut mcp = serde_json::to_value(mcp_status).unwrap_or_else(|_| json!({}));
    if let Value::Object(object) = &mut mcp {
        object.insert("recentCalls".into(), Value::Array(recent_calls));
        object.insert("transports".into(), mcp_sources::monitor_statuses(&sources, now_ms));
    }
    let tunnel = tunnel_override.unwrap_or_else(|| {
        state
            .runtime_snapshot
            .lock()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_default()
    });
    let settings = load_settings(app);
    let recovery = recovery_status(state);
    let (reconnect_available, reconnect_unavailable_reason) =
        reconnect_capability(app, state, &settings);
    // A separately named, read-only transport summary for Android. It uses
    // the same mode selection as the desktop UI, but never exposes SSH users,
    // passwords, OAuth tokens or local filesystem paths.
    let https_allowed = crate::connection_intent::restore_allowed(
        app, crate::connection_intent::Transport::HttpsMcp
    ).unwrap_or(false);
    let selected_mode = if settings.https_mcp.enabled && https_allowed {
        "https"
    } else if !settings.https_mcp.enabled &&
        state.desired_connected.load(Ordering::SeqCst) {
        "tunnel"
    } else { "none" };
    let (https_local_ready, https_ssh_connected, https_remote_ready,
        last_public_probe_at, https_restarts) =
        crate::https_mcp::monitor_public_connection(state);
    let https_connected = selected_mode == "https"
        && https_local_ready && https_ssh_connected
        && https_remote_ready;
    let https_state = if selected_mode != "https" { "stopped" }
        else if https_connected { "connected" }
        else if https_ssh_connected { "checking" }
        else { "reconnecting" };
    let tunnel_connected = selected_mode == "tunnel"
        && tunnel.active && tunnel.health == "healthy"
        && tunnel.last_probe_at > 0
        && now_ms.saturating_sub(tunnel.last_probe_at) <= 12_000;
    let connections = json!({
        "activeMode": selected_mode,
        "tunnel": {
            "enabled": selected_mode == "tunnel",
            "connected": tunnel_connected,
            "state": if selected_mode != "tunnel" { "stopped" }
                else if tunnel_connected { "connected" }
                else if state.reconnecting.load(Ordering::SeqCst) { "reconnecting" }
                else { "checking" },
        },
        "httpsMcp": {
            "enabled": selected_mode == "https",
            "connected": https_connected,
            "state": https_state,
            "localReady": https_local_ready,
            "sshConnected": https_ssh_connected,
            "remoteReady": https_remote_ready,
            "lastPublicProbeAt": last_public_probe_at,
            "restarts": https_restarts,
        }
    });
    let endpoints = monitor_endpoints(settings.monitor_port);
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
        "connections": connections,
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
            "autoReconnect": settings.auto_reconnect,
            "reconnecting": state.reconnecting.load(Ordering::SeqCst),
            "reconnectAttempt": state.reconnect_attempt.load(Ordering::SeqCst),
            "reconnectPhase": recovery.phase,
            "nextReconnectAt": recovery.next_reconnect_at,
            "lastReconnectError": recovery.last_error,
            "reconnectAvailable": reconnect_available,
            "reconnectUnavailableReason": reconnect_unavailable_reason
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

#[cfg(test)]
mod recent_calls_tests {
    use super::*;

    #[test]
    fn completed_history_uses_invocation_start_for_ordering() {
        let finished = "2026-10-11T09:00:03.000Z";
        let text = format!(
            "{{\"timestamp\":\"{finished}\",\"toolName\":\"read_file\",\"duration\":3000}}"
        );
        let history = parse_recent_history(&text, 5);
        let expected = u64::try_from(
            DateTime::parse_from_rfc3339(finished).unwrap().timestamp_millis()
        ).unwrap() - 3000;
        assert_eq!(history[0]["startedAt"], expected);
        let mut mixed = vec![
            history[0].clone(),
            json!({"toolName":"older-running","startedAt":expected - 1000,"running":true}),
        ];
        mcp_sources::sort_recent_by_time(&mut mixed, 10);
        assert_eq!(mixed[0]["toolName"], "read_file");
        assert_eq!(mixed[1]["toolName"], "older-running");
    }

    fn finished_activity() -> monitor::ActivitySnapshot {
        serde_json::from_value(json!({
            "schemaVersion":3, "sequence":3, "updatedAt":149_000,
            "lastCallStartedAt":140_000, "lastCallFinishedAt":149_000,
            "lastToolName":"read_file", "lastSuccess":true,
            "lastDurationMs":9000, "inFlight":0,
            "oldestInFlightStartedAt":null, "callsLastMinute":1
        })).unwrap()
    }

    #[test]
    fn finished_call_does_not_disappear_while_history_flushes() {
        let sources = [(mcp_sources::TUNNEL, Some(finished_activity()))];
        let pending = merge_recent_calls(vec![], &sources, 150_000);
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0]["toolName"], "read_file");
        assert_eq!(pending[0]["running"], false);
        let persisted = vec![mcp_sources::annotate(json!({
            "toolName":"read_file", "startedAt":140_020,
            "durationMs":9000, "running":false
        }), mcp_sources::TUNNEL)];
        let merged = merge_recent_calls(persisted, &sources, 150_000);
        assert_eq!(merged.len(), 1);
    }

    #[test]
    fn same_named_concurrent_calls_are_not_lost_to_timestamp_fuzzy_matching() {
        let mut activity = finished_activity();
        activity.in_flight = 2;
        activity.in_flight_calls = vec![
            monitor::InFlightCall {
                id: "call-1".into(), tool_name: "read_file".into(), started_at: 140_000,
            },
            monitor::InFlightCall {
                id: "call-2".into(), tool_name: "read_file".into(), started_at: 140_070,
            },
        ];
        let saved = vec![mcp_sources::annotate(json!({
            "toolName":"read_file", "startedAt":140_050,
            "durationMs":9000, "timestamp":"2026-10-11T09:00:00Z",
            "running":false,
        }), mcp_sources::TUNNEL)];
        let result = merge_recent_calls(saved, &[(mcp_sources::TUNNEL, Some(activity))], 150_000);
        assert_eq!(result.len(), 3, "A finished call must not hide two distinct active calls");
        assert_eq!(result.iter().filter(|item| item["running"] == true).count(), 2);
    }

    #[test]
    fn same_name_distinct_history_records_are_not_collapsed() {
        let records = vec![
            mcp_sources::annotate(json!({
                "toolName":"read_file", "startedAt":140_000, "durationMs":10,
                "timestamp":"2026-10-11T09:00:00.010Z", "running":false,
            }), mcp_sources::TUNNEL),
            mcp_sources::annotate(json!({
                "toolName":"read_file", "startedAt":140_030, "durationMs":10,
                "timestamp":"2026-10-11T09:00:00.040Z", "running":false,
            }), mcp_sources::TUNNEL),
        ];
        assert!(!same_recent_call(&records[0], &records[1]));
        assert_eq!(merge_recent_calls(records, &[], 150_000).len(), 2);
    }

    #[test]
    fn stale_activity_does_not_resurrect_cleared_history() {
        let sources = [(mcp_sources::TUNNEL, Some(finished_activity()))];
        assert!(merge_recent_calls(vec![], &sources, 170_000).is_empty());
    }
}
