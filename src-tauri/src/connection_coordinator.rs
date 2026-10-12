//! Serializes user-facing MCP transport lifecycle transitions.
//! Transport implementations still own their own workers; this module owns
//! cross-mode ordering, durable stop markers, and the shared operation lock.
use crate::{
    app_state::AppState,
    connection_intent::{self, Transport},
    settings::{load_settings, update_settings},
};
use std::sync::{Mutex, MutexGuard};

// Persist BOTH durable Stop markers before shutting anything down. When a
// settings write fails or the process crashes between stop/start, neither the
// old nor the newly selected mode may reconnect from stale saved intent.
fn stage_mode_switch(
    target: Transport,
    mut mark_stopped: impl FnMut(Transport) -> Result<(), String>,
    stop_existing: impl FnOnce() -> Result<(), String>,
    persist_disabled: impl FnOnce() -> Result<(), String>,
) -> Result<(), String> {
    mark_stopped(target)?;
    mark_stopped(match target {
        Transport::HttpsMcp => Transport::Tunnel,
        Transport::Tunnel => Transport::HttpsMcp,
    })?;
    stop_existing()?;
    persist_disabled()
}

#[derive(Default)]
pub(super) struct ConnectionCoordination {
    pub(super) operation: Mutex<()>,
}

pub(super) struct ConnectionCoordinator<'a> {
    app: &'a tauri::AppHandle,
    state: &'a AppState,
    _operation: MutexGuard<'a, ()>,
}

impl<'a> ConnectionCoordinator<'a> {
    pub(super) fn acquire(
        app: &'a tauri::AppHandle,
        state: &'a AppState,
    ) -> Result<Self, String> {
        let operation = state.connection.operation.lock()
            .map_err(|_| "连接操作锁已损坏".to_string())?;
        Ok(Self { app, state, _operation: operation })
    }

    pub(super) fn ensure_available(&self, message: &'static str) -> Result<(), String> {
        if self.state.quitting.load(std::sync::atomic::Ordering::SeqCst) {
            Err(message.to_string())
        } else {
            Ok(())
        }
    }

    // Called before any HTTPS transport starts. The old Tunnel stop marker
    // precedes process termination; an interrupted settings write must not
    // silently restore the old transport after restarting ChatX.
    pub(super) fn prepare_https(&self) -> Result<(), String> {
        stage_mode_switch(
            Transport::HttpsMcp,
            |mode| connection_intent::mark_stopped(self.app, mode),
            || {
                crate::https_mcp::stop_on_exit(self.state);
                let previous = load_settings(self.app);
                crate::tunnel_runtime::stop_tunnel_without_saving(
                    self.app, self.state, &previous,
                )
            },
            || update_settings(self.app, |settings| {
                settings.connection_intent = false;
                settings.https_mcp.enabled = false;
                Ok(())
            }).map(|_| ()),
        )
    }

    // Called before starting the legacy Secure MCP Tunnel. HTTPS can never
    // resume from stale saved intent while the Tunnel operation is pending.
    pub(super) fn prepare_tunnel(&self) -> Result<(), String> {
        stage_mode_switch(
            Transport::Tunnel,
            |mode| connection_intent::mark_stopped(self.app, mode),
            || { crate::https_mcp::stop_on_exit(self.state); Ok(()) },
            || update_settings(self.app, |settings| {
                settings.https_mcp.enabled = false;
                settings.connection_intent = false;
                Ok(())
            }).map(|_| ()),
        )
    }

    pub(super) fn stop_all(&self) -> Result<(), String> {
        // Keep both stop markers independent of the subsequent settings write.
        let https_marker = connection_intent::mark_stopped(self.app, Transport::HttpsMcp);
        let tunnel_marker = connection_intent::mark_stopped(self.app, Transport::Tunnel);
        crate::https_mcp::stop_on_exit(self.state);
        let settings = load_settings(self.app);
        let stopped = crate::tunnel_runtime::stop_tunnel_without_saving(
            self.app, self.state, &settings,
        );
        let saved = update_settings(self.app, |settings| {
            settings.connection_intent = false;
            settings.https_mcp.enabled = false;
            Ok(())
        });
        if let Err(error) = &saved {
            crate::push_log(self.state, format!(
                "连接已停止，但保存停止状态失败：{error}",
            ));
        }
        stopped?;
        saved?;
        https_marker?;
        tunnel_marker?;
        if let Ok(mut pending) = self.state.tray_stop_error.lock() {
            *pending = None;
        }
        crate::push_log(self.state, "已停止全部 MCP 连接");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn markers_precede_shutdown_and_persistence_for_both_switches() {
        for target in [Transport::Tunnel, Transport::HttpsMcp] {
            let stages = RefCell::new(Vec::new());
            stage_mode_switch(target,
                |mode| {
                    stages.borrow_mut().push(match mode {
                        Transport::Tunnel => "marker-tunnel",
                        Transport::HttpsMcp => "marker-https",
                    });
                    Ok(())
                },
                || { stages.borrow_mut().push("stop"); Ok(()) },
                || { stages.borrow_mut().push("persist"); Ok(()) },
            ).unwrap();
            let expected = if matches!(target, Transport::Tunnel) {
                vec!["marker-tunnel", "marker-https", "stop", "persist"]
            } else {
                vec!["marker-https", "marker-tunnel", "stop", "persist"]
            };
            assert_eq!(*stages.borrow(), expected);
        }
    }

    #[test]
    fn marker_failure_never_stops_an_existing_transport() {
        for failed_at in [0, 1] {
            let calls = RefCell::new(Vec::new());
            let result = stage_mode_switch(Transport::HttpsMcp,
                |_| {
                    let mut values = calls.borrow_mut();
                    let index = values.len();
                    values.push("marker");
                    if index == failed_at { Err("disk error".into()) }
                    else { Ok(()) }
                },
                || { calls.borrow_mut().push("stop"); Ok(()) },
                || { calls.borrow_mut().push("persist"); Ok(()) },
            );
            assert!(result.is_err());
            assert_eq!(calls.borrow().len(), failed_at + 1);
        }
    }

    #[test]
    fn stop_or_save_failure_leaves_both_durable_markers() {
        for failed_stage in ["stop", "persist"] {
            let stages = RefCell::new(Vec::new());
            let result = stage_mode_switch(Transport::Tunnel,
                |mode| {
                    stages.borrow_mut().push(match mode {
                        Transport::Tunnel => "marker-tunnel",
                        Transport::HttpsMcp => "marker-https",
                    });
                    Ok(())
                },
                || {
                    stages.borrow_mut().push("stop");
                    if failed_stage == "stop" { Err("failed to stop".into()) }
                    else { Ok(()) }
                },
                || {
                    stages.borrow_mut().push("persist");
                    if failed_stage == "persist" { Err("write denied".into()) }
                    else { Ok(()) }
                },
            );
            assert!(result.is_err());
            assert_eq!(&stages.borrow()[..2], ["marker-tunnel", "marker-https"]);
            assert_eq!(stages.borrow().last(), Some(&failed_stage));
        }
    }
}
