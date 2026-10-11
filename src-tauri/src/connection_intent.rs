//! Durable manual-stop markers. A failed settings.json write must not let an
//! older persisted "enabled" flag silently reconnect after the next launch.
use crate::state_dir;
use std::{fs::{self, OpenOptions}, io::{self, Write}, path::{Path, PathBuf}};

#[derive(Clone, Copy)]
pub(super) enum Transport {
    Tunnel,
    HttpsMcp,
}
impl Transport {
    fn name(self) -> &'static str {
        match self {
            Self::Tunnel => "tunnel",
            Self::HttpsMcp => "https-mcp",
        }
    }
}

fn marker_path(app: &tauri::AppHandle, transport: Transport) -> Result<PathBuf, String> {
    Ok(state_dir(app)?.join(format!("manually-stopped-{}.flag", transport.name())))
}

fn write_marker(path: &Path) -> io::Result<()> {
    if let Some(parent) = path.parent() { fs::create_dir_all(parent)?; }
    let mut file = OpenOptions::new().create(true).write(true).truncate(true).open(path)?;
    file.write_all(b"manually-stopped-v1\n")?;
    file.sync_all()
}
fn clear_marker(path: &Path) -> io::Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}
fn has_marker(path: &Path) -> io::Result<bool> {
    match fs::metadata(path) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

pub(super) fn mark_stopped(app: &tauri::AppHandle, mode: Transport) -> Result<(), String> {
    write_marker(&marker_path(app, mode)?)
        .map_err(|e| format!("记录 {} 手动停止意图失败：{e}", mode.name()))
}
pub(super) fn clear_stopped(app: &tauri::AppHandle, mode: Transport) -> Result<(), String> {
    clear_marker(&marker_path(app, mode)?)
        .map_err(|e| format!("清除 {} 停止标记失败：{e}", mode.name()))
}
pub(super) fn restore_allowed(app: &tauri::AppHandle, mode: Transport) -> Result<bool, String> {
    has_marker(&marker_path(app, mode)?)
        .map(|stopped| !stopped)
        .map_err(|e| format!("无法检查 {} 停止标记：{e}", mode.name()))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistent_stop_marker_requires_explicit_clear() {
        let path = std::env::temp_dir().join(format!(
            "chatx-stop-intent-{}-{:?}.flag", std::process::id(), std::thread::current().id()
        ));
        clear_marker(&path).unwrap();
        assert!(!has_marker(&path).unwrap());
        write_marker(&path).unwrap();
        assert!(has_marker(&path).unwrap());
        // Idempotent stop; the second stop must not accidentally clear it.
        write_marker(&path).unwrap();
        assert!(has_marker(&path).unwrap());
        clear_marker(&path).unwrap();
        assert!(!has_marker(&path).unwrap());
    }
}
