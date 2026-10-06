use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

const ACTIVE_WINDOW_MS: u64 = 60_000;
const ACTIVE_MIN_CALLS: usize = 3;
const GAP_THRESHOLD_MS: u64 = 60_000;
const IDLE_RESET_MS: u64 = 10 * 60_000;
const STALL_THRESHOLD_MS: u64 = 10 * 60_000;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelSnapshot {
    pub state: String,
    pub health: String,
    pub active: bool,
    pub last_error: String,
    pub updated_at: u64,
    pub last_probe_at: u64,
    pub last_successful_probe_at: u64,
    pub consecutive_failures: u32,
    pub control_plane_state: String,
    pub control_plane_reason: String,
    pub control_plane_failures: u32,
    pub proxy_mode: String,
    pub proxy_source: String,
}

impl Default for TunnelSnapshot {
    fn default() -> Self {
        Self {
            state: "unknown".into(),
            health: "unknown".into(),
            active: false,
            last_error: String::new(),
            updated_at: 0,
            last_probe_at: 0,
            last_successful_probe_at: 0,
            consecutive_failures: 0,
            control_plane_state: "unknown".into(),
            control_plane_reason: String::new(),
            control_plane_failures: 0,
            proxy_mode: "direct".into(),
            proxy_source: String::new(),
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InFlightCall {
    pub id: String,
    pub tool_name: String,
    pub started_at: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivitySnapshot {
    pub schema_version: u32,
    pub sequence: u64,
    pub updated_at: u64,
    #[serde(default)]
    pub launcher_started_at: Option<u64>,
    #[serde(default)]
    pub last_request_at: Option<u64>,
    #[serde(default)]
    pub last_request_method: Option<String>,
    #[serde(default)]
    pub last_list_tools_at: Option<u64>,
    pub last_call_started_at: Option<u64>,
    pub last_call_finished_at: Option<u64>,
    pub last_tool_name: Option<String>,
    pub last_success: Option<bool>,
    pub last_duration_ms: Option<u64>,
    pub in_flight: u32,
    #[serde(default)]
    pub in_flight_calls: Vec<InFlightCall>,
    pub oldest_in_flight_started_at: Option<u64>,
    #[serde(default)]
    pub recent_call_starts: Vec<u64>,
    #[serde(default)]
    pub calls_last_minute: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct McpMonitorStatus {
    pub state: &'static str,
    pub source_sequence: Option<u64>,
    pub source_updated_at: Option<u64>,
    pub launcher_started_at: Option<u64>,
    pub last_request_at: Option<u64>,
    pub last_request_method: Option<String>,
    pub last_list_tools_at: Option<u64>,
    pub calls_last_minute: usize,
    pub gap_duration_ms: Option<u64>,
    pub stall_duration_ms: Option<u64>,
    pub in_flight: u32,
    pub last_call_started_at: Option<u64>,
    pub last_call_finished_at: Option<u64>,
    pub last_tool_name: Option<String>,
    pub last_success: Option<bool>,
    pub last_duration_ms: Option<u64>,
}

pub fn read_activity_snapshot(path: &Path) -> Option<ActivitySnapshot> {
    let text = fs::read_to_string(path).ok()?;
    let snapshot = serde_json::from_str::<ActivitySnapshot>(&text).ok()?;
    (snapshot.schema_version == 1 || snapshot.schema_version == 2 || snapshot.schema_version == 3).then_some(snapshot)
}

fn elapsed(now: u64, then: u64) -> u64 {
    now.saturating_sub(then)
}

pub fn evaluate_activity(snapshot: Option<&ActivitySnapshot>, now: u64) -> McpMonitorStatus {
    let Some(snapshot) = snapshot else {
        return empty_status();
    };

    let current_calls = snapshot.recent_call_starts.iter()
        .filter(|timestamp| elapsed(now, **timestamp) <= ACTIVE_WINDOW_MS)
        .count();
    let burst_detected = snapshot.calls_last_minute >= ACTIVE_MIN_CALLS;
    let mut state = "idle";
    let mut gap_duration_ms = None;
    let mut stall_duration_ms = None;

    if snapshot.in_flight > 0 {
        state = "active";
        if let Some(started_at) = snapshot.oldest_in_flight_started_at {
            let stalled_for = elapsed(now, started_at);
            if stalled_for >= STALL_THRESHOLD_MS {
                state = "stalled";
                stall_duration_ms = Some(stalled_for);
            }
        }
    } else if let Some(last_started_at) = snapshot.last_call_started_at {
        let last_activity_at = snapshot.last_call_finished_at
            .map(|finished_at| finished_at.max(last_started_at))
            .unwrap_or(last_started_at);
        let gap = elapsed(now, last_activity_at);
        if gap < GAP_THRESHOLD_MS && (current_calls >= ACTIVE_MIN_CALLS || burst_detected) {
            state = "active";
        } else if burst_detected && (GAP_THRESHOLD_MS..=IDLE_RESET_MS).contains(&gap) {
            state = "gap";
            gap_duration_ms = Some(gap);
        }
    }

    McpMonitorStatus {
        state,
        source_sequence: Some(snapshot.sequence),
        source_updated_at: Some(snapshot.updated_at),
        launcher_started_at: snapshot.launcher_started_at,
        last_request_at: snapshot.last_request_at,
        last_request_method: snapshot.last_request_method.clone(),
        last_list_tools_at: snapshot.last_list_tools_at,
        calls_last_minute: current_calls,
        gap_duration_ms,
        stall_duration_ms,
        in_flight: snapshot.in_flight,

        last_call_started_at: snapshot.last_call_started_at,
        last_call_finished_at: snapshot.last_call_finished_at,
        last_tool_name: snapshot.last_tool_name.clone(),
        last_success: snapshot.last_success,
        last_duration_ms: snapshot.last_duration_ms,
    }
}

fn empty_status() -> McpMonitorStatus {
    McpMonitorStatus {
        state: "idle",
        source_sequence: None,
        source_updated_at: None,
        launcher_started_at: None,
        last_request_at: None,
        last_request_method: None,
        last_list_tools_at: None,
        calls_last_minute: 0,
        gap_duration_ms: None,
        stall_duration_ms: None,
        in_flight: 0,
        last_call_started_at: None,
        last_call_finished_at: None,
        last_tool_name: None,
        last_success: None,
        last_duration_ms: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(now: u64) -> ActivitySnapshot {
        ActivitySnapshot {
            schema_version: 3,
            sequence: 1,
            updated_at: now,
            launcher_started_at: Some(now - 60_000),
            last_request_at: Some(now - 5_000),
            last_request_method: Some("tools/call".into()),
            last_list_tools_at: Some(now - 30_000),
            last_call_started_at: Some(now - 5_000),
            last_call_finished_at: Some(now - 4_900),
            last_tool_name: Some("read_file".into()),
            last_success: Some(true),
            last_duration_ms: Some(100),
            in_flight: 0,
            in_flight_calls: Vec::new(),
            oldest_in_flight_started_at: None,
            recent_call_starts: vec![now - 40_000, now - 20_000, now - 5_000],
            calls_last_minute: 3,
        }
    }

    #[test]
    fn active_burst_is_active() {
        let now = 1_000_000;
        let status = evaluate_activity(Some(&snapshot(now)), now);
        assert_eq!(status.state, "active");
        assert_eq!(status.calls_last_minute, 3);
    }

    #[test]
    fn burst_becomes_gap_after_one_minute() {
        let now = 1_000_000;
        let mut item = snapshot(now);
        item.last_call_started_at = Some(now - 65_000);
        item.last_call_finished_at = Some(now - 64_900);
        item.recent_call_starts.clear();
        let status = evaluate_activity(Some(&item), now);
        assert_eq!(status.state, "gap");

        assert_eq!(status.gap_duration_ms, Some(64_900));
    }

    #[test]
    fn long_in_flight_call_is_stalled_not_gap() {
        let now = 1_000_000;
        let mut item = snapshot(now);
        item.in_flight = 1;
        item.oldest_in_flight_started_at = Some(now - 11 * 60_000);
        item.last_call_started_at = item.oldest_in_flight_started_at;
        let status = evaluate_activity(Some(&item), now);
        assert_eq!(status.state, "stalled");
        assert_eq!(status.gap_duration_ms, None);
        assert_eq!(status.stall_duration_ms, Some(11 * 60_000));
    }

    #[test]
    fn long_call_completion_does_not_create_gap() {
        let now = 1_000_000;
        let mut item = snapshot(now);
        item.last_call_started_at = Some(now - 120_000);
        item.last_call_finished_at = Some(now);
        item.recent_call_starts.clear();
        item.calls_last_minute = 0;
        let status = evaluate_activity(Some(&item), now);
        assert_eq!(status.state, "idle");
    }

    #[test]
    fn old_burst_resets_to_idle() {
        let now = 2_000_000;

        let mut item = snapshot(now);
        item.last_call_started_at = Some(now - 11 * 60_000);
        item.last_call_finished_at = Some(now - 11 * 60_000 + 100);
        item.recent_call_starts.clear();
        let status = evaluate_activity(Some(&item), now);
        assert_eq!(status.state, "idle");
    }
}
