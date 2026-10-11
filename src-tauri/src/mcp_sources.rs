//! Two independent Desktop Commander transports and their observability files.
use super::{desktop_commander_home, https_mcp, monitor, state_dir};
use crate::app_state::AppState;
use serde_json::{json, Value};
use std::path::PathBuf;

pub(super) const TUNNEL: &str = "Secure MCP Tunnel";
pub(super) const HTTPS: &str = "HTTPS MCP";

pub(super) fn history_paths(
    app: &tauri::AppHandle,
) -> Result<Vec<(&'static str, PathBuf)>, String> {
    Ok(vec![
        (TUNNEL, desktop_commander_home(app)?
            .join(".claude-server-commander").join("tool-history.jsonl")),
        (HTTPS, state_dir(app)?.join("https-desktop-commander-home")
            .join(".claude-server-commander").join("tool-history.jsonl")),
    ])
}
pub(super) fn activity_paths(
    app: &tauri::AppHandle,
) -> Result<Vec<(&'static str, PathBuf)>, String> {
    Ok(vec![
        (TUNNEL, desktop_commander_home(app)?
            .join(".chatx-monitor").join("activity.json")),
        (HTTPS, state_dir(app)?.join("https-desktop-commander-home")
            .join(".chatx-monitor").join("activity.json")),
    ])
}
pub(super) fn activities(
    app: &tauri::AppHandle, state: &AppState, now: u64,
) -> Result<Vec<(&'static str, Option<monitor::ActivitySnapshot>)>, String> {
    const IDLE_FRESH_MS: u64 = 180_000;
    const RUNNING_FRESH_MS: u64 = 45 * 60_000;
    let tunnel_alive = state.runtime_snapshot.lock().is_ok_and(|v| v.active);
    let https_alive = https_mcp::local_bridge_alive(state);
    Ok(activity_paths(app)?.into_iter().map(|(source, path)| {
        let alive = if source == HTTPS { https_alive } else { tunnel_alive };
        let snapshot = if alive {
            monitor::read_activity_snapshot(&path).filter(|snapshot| {
                let max_age = if snapshot.in_flight > 0 { RUNNING_FRESH_MS } else { IDLE_FRESH_MS };
                snapshot.updated_at <= now.saturating_add(5_000)
                    && now.saturating_sub(snapshot.updated_at) <= max_age
            })
        } else { None };
        (source, snapshot)
    }).collect())
}

pub(super) fn primary_activity<'a>(
    sources: &'a [(&'static str, Option<monitor::ActivitySnapshot>)],
) -> Option<&'a monitor::ActivitySnapshot> {
    sources.iter().filter_map(|(_, activity)| activity.as_ref())
        .max_by_key(|activity| {
            let priority = if activity.in_flight > 0 { 2 }
                else if activity.calls_last_minute > 0 { 1 }
                else { 0 };
            (priority, activity.updated_at)
        })
}

pub(super) fn monitor_statuses(
    sources: &[(&'static str, Option<monitor::ActivitySnapshot>)],
    now: u64,
) -> Value {
    let mut result = serde_json::Map::new();
    for (source, snapshot) in sources {
        let key = if *source == HTTPS { "httpsMcp" } else { "tunnel" };
        result.insert(key.into(), json!(monitor::evaluate_activity(snapshot.as_ref(), now)));
    }
    Value::Object(result)
}

pub(super) fn annotate(mut item: Value, source: &str) -> Value {
    if let Value::Object(values) = &mut item {
        values.insert("source".into(), Value::String(source.into()));
    }
    item
}
pub(super) fn sort_recent_by_time(items: &mut Vec<Value>, limit: usize) {
    items.sort_by(|a, b| {
        let a_time = a.get("startedAt").and_then(Value::as_u64).unwrap_or(0);
        let b_time = b.get("startedAt").and_then(Value::as_u64).unwrap_or(0);
        let a_text = a.get("timestamp").and_then(Value::as_str).unwrap_or("");
        let b_text = b.get("timestamp").and_then(Value::as_str).unwrap_or("");
        b_time.cmp(&a_time).then_with(|| b_text.cmp(a_text))
    });
    items.truncate(limit);
}

#[cfg(test)]
mod tests {
    use super::*;
    fn activity(in_flight: u32, updated_at: u64) -> monitor::ActivitySnapshot {
        serde_json::from_value(json!({
            "schemaVersion": 3, "sequence": 1, "updatedAt": updated_at,
            "lastCallStartedAt": null, "lastCallFinishedAt": null,
            "lastToolName": null, "lastSuccess": null, "lastDurationMs": null,
            "inFlight": in_flight, "oldestInFlightStartedAt": null,
            "callsLastMinute": 0
        })).unwrap()
    }

    #[test]
    fn active_transport_outweighs_newer_idle_snapshot() {
        let sources = [
            (TUNNEL, Some(activity(1, 100))),
            (HTTPS, Some(activity(0, 200)))
        ];
        let chosen = primary_activity(&sources).unwrap();
        assert_eq!(chosen.in_flight, 1);
    }
    #[test]
    fn source_tag_survives_sorting() {
        let mut records = vec![
            annotate(json!({"timestamp":"2026-10-11T11:00:00Z"}), TUNNEL),
            annotate(json!({"timestamp":"2026-10-11T11:01:00Z"}), HTTPS),
        ];
        sort_recent_by_time(&mut records, 1);
        assert_eq!(records.len(), 1);
        assert_eq!(records[0]["source"], HTTPS);
    }
}
