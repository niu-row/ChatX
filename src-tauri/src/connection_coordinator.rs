//! Serializes user-facing MCP transport lifecycle transitions.
//! Transport implementations still own their own workers; this module owns
//! cross-mode ordering, durable stop markers, and the shared operation lock.
use crate::{
    app_state::AppState,
    connection_intent::{self, Transport},
    settings::{load_settings, update_settings},
};
use std::sync::{Mutex, MutexGuard};

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
        connection_intent::mark_stopped(self.app, Transport::Tunnel)?;
        crate::https_mcp::stop_on_exit(self.state);
        let previous = load_settings(self.app);
        crate::tunnel_runtime::stop_tunnel_without_saving(
            self.app, self.state, &previous,
        )?;
        update_settings(self.app, |settings| {
            settings.connection_intent = false;
            settings.https_mcp.enabled = false;
            Ok(())
        })?;
        Ok(())
    }

    // Called before starting the legacy Secure MCP Tunnel. HTTPS can never
    // resume from stale saved intent while the Tunnel operation is pending.
    pub(super) fn prepare_tunnel(&self) -> Result<(), String> {
        connection_intent::mark_stopped(self.app, Transport::HttpsMcp)?;
        crate::https_mcp::stop_on_exit(self.state);
        update_settings(self.app, |settings| {
            settings.https_mcp.enabled = false;
            settings.connection_intent = false;
            Ok(())
        })?;
        Ok(())
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
