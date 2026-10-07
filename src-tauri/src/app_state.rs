use super::{monitor, monitor_control, monitor_manager, relay};
use serde_json::Value;
use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, AtomicU32, AtomicU64},
        Arc, Mutex,
    },
};

#[derive(Debug, Clone)]
pub(super) struct TunnelRecoveryStatus {
    pub(super) phase: String,
    pub(super) next_reconnect_at: u64,
    pub(super) last_error: String,
}

impl Default for TunnelRecoveryStatus {
    fn default() -> Self {
        Self {
            phase: "idle".into(),
            next_reconnect_at: 0,
            last_error: String::new(),
        }
    }
}

#[derive(Default)]
pub(super) struct AppState {
    pub(super) logs: Mutex<Vec<String>>,
    pub(super) quitting: AtomicBool,
    pub(super) desired_connected: AtomicBool,
    pub(super) reconnecting: AtomicBool,
    pub(super) reconnect_attempt: AtomicU32,
    pub(super) recovery_status: Mutex<TunnelRecoveryStatus>,
    pub(super) runtime_snapshot: Mutex<monitor::TunnelSnapshot>,
    pub(super) relay_status: Arc<Mutex<relay::RelayStatus>>,
    pub(super) relay_client: Mutex<Option<relay::RelayClientHandle>>,
    pub(super) monitor: monitor_manager::State,
    pub(super) monitor_control: monitor_control::State,
    pub(super) session_runtime_key: Mutex<Option<String>>,
    pub(super) runtime_operation: Mutex<()>,
    pub(super) permission_results: Mutex<Option<Value>>,
    pub(super) main_window_generation: AtomicU64,
}

#[derive(Clone)]
pub(super) struct RuntimePaths {
    pub(super) root: PathBuf,
    pub(super) tunnel: PathBuf,
    pub(super) node: PathBuf,
    pub(super) launcher: PathBuf,
    pub(super) desktop_commander: PathBuf,
}
