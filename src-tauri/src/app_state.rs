use super::{connection_coordinator, https_mcp, monitor, monitor_control, monitor_manager, relay};
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

// The network Relay has its own lifecycle, independent of MCP mode switching.
#[derive(Default)]
pub(super) struct RelayLifecycleState {
    pub(super) status: Arc<Mutex<relay::RelayStatus>>,
    pub(super) client: Mutex<Option<relay::RelayClientHandle>>,
    pub(super) deploy_busy: AtomicBool,
}

#[derive(Default)]
pub(super) struct AppState {
    pub(super) logs: Mutex<Vec<String>>,
    pub(super) tray_stop_error: Mutex<Option<String>>,
    pub(super) quitting: AtomicBool,
    pub(super) desired_connected: AtomicBool,
    pub(super) reconnecting: AtomicBool,
    pub(super) reconnect_attempt: AtomicU32,
    pub(super) recovery_status: Mutex<TunnelRecoveryStatus>,
    pub(super) runtime_snapshot: Mutex<monitor::TunnelSnapshot>,
    pub(super) relay: RelayLifecycleState,
    pub(super) https_mcp: https_mcp::HttpsState,
    pub(super) monitor: monitor_manager::State,
    pub(super) monitor_control: monitor_control::State,
    pub(super) session_runtime_key: Mutex<Option<String>>,
    pub(super) runtime_operation: Mutex<()>,
    // Serialize manual start/stop/reconfigure across both remote transports.
    pub(super) connection: connection_coordinator::ConnectionCoordination,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread, time::Duration};

    #[test]
    fn manual_connection_operations_share_one_lock() {
        let state = Arc::new(AppState::default());
        let first = state.connection.operation.lock().unwrap();
        let clone = Arc::clone(&state);
        let (tx, rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _second = clone.connection.operation.lock().unwrap();
            tx.send(()).unwrap();
        });
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "The second connection operation must wait");
        drop(first);
        rx.recv_timeout(Duration::from_secs(2)).expect("Connection lock must be released");
        worker.join().unwrap();
    }
}
