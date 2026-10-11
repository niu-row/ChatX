use super::{relay, state_dir, system_proxy_url};
use crate::secrets::runtime_key_saved;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    fs::{self, OpenOptions}, io::Write, path::Path,
    sync::{Mutex, OnceLock, atomic::{AtomicU64, Ordering}},
};

static SETTINGS_WRITE_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(1);

fn settings_path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(state_dir(app)?.join("settings.json"))
}

/// The actual user intent may be blocked by a durable Stop marker.
// At most one ChatGPT-facing MCP transport is effective.
// The same predicate is shared by Desktop and Android Monitor.
// Both Desktop status and Android Monitor must choose the same effective mode.
pub(super) fn effective_mcp_mode(
    settings: &Settings, https_allowed: bool, tunnel_desired: bool,
) -> &'static str {
    if settings.https_mcp.enabled && https_allowed { "https" }
    else if !settings.https_mcp.enabled && tunnel_desired { "tunnel" }
    else { "none" }
}

fn default_auto_reconnect() -> bool {
    true
}

fn default_monitor_port() -> u16 {
    18432
}

fn default_proxy_mode() -> String {
    "direct".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub(super) struct ProxySettings {
    #[serde(default = "default_proxy_mode")]
    pub(super) mode: String,
    #[serde(default)]
    pub(super) url: String,
    #[serde(default)]
    pub(super) applied_url: String,
}

impl Default for ProxySettings {
    fn default() -> Self {
        Self {
            mode: default_proxy_mode(),
            url: String::new(),
            applied_url: String::new(),
        }
    }
}

fn default_ssh_port() -> u16 { 22 }
fn default_https_transport() -> String { "ssh".into() }
fn default_local_mcp_port() -> u16 { 3000 }
fn default_reverse_mcp_port() -> u16 { 18080 }
fn default_https_auth_ttl_hours() -> Option<u32> { Some(7 * 24) }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(super) struct HttpsMcpSettings {
    #[serde(default)] pub(super) enabled: bool,
    #[serde(default = "default_auto_reconnect")] pub(super) auto_reconnect: bool,
    #[serde(default)] pub(super) base_url: String,
    #[serde(default = "default_https_transport")] pub(super) transport: String,
    #[serde(default)] pub(super) ssh_host: String,
    #[serde(default)] pub(super) ssh_user: String,
    #[serde(default = "default_ssh_port")] pub(super) ssh_port: u16,
    #[serde(default = "default_local_mcp_port")] pub(super) local_port: u16,
    #[serde(default = "default_reverse_mcp_port")] pub(super) remote_port: u16,
    // None = explicitly unlimited; a missing setting migrates to seven days.
    #[serde(default = "default_https_auth_ttl_hours")]
    pub(super) authorization_ttl_hours: Option<u32>,
}

impl Default for HttpsMcpSettings {
    fn default() -> Self {
        Self { enabled: false, auto_reconnect: true, base_url: String::new(),
            transport: "wss".into(), ssh_host: String::new(),
            ssh_user: String::new(), ssh_port: default_ssh_port(),
            local_port: default_local_mcp_port(), remote_port: default_reverse_mcp_port(),
            authorization_ttl_hours: default_https_auth_ttl_hours() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Settings {
    pub(super) tunnel_id: String,
    pub(super) remember_key: bool,
    #[serde(default = "default_auto_reconnect")]
    pub(super) auto_reconnect: bool,
    #[serde(default)]
    pub(super) connection_intent: bool,
    #[serde(default)]
    pub(super) monitor_enabled: bool,
    #[serde(default = "default_monitor_port")]
    pub(super) monitor_port: u16,
    #[serde(default)]
    pub(super) proxy: ProxySettings,
    #[serde(default)]
    pub(super) relay: relay::RelaySettings,
    #[serde(default)]
    pub(super) https_mcp: HttpsMcpSettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            tunnel_id: String::new(),
            remember_key: false,
            auto_reconnect: true,
            connection_intent: false,
            monitor_enabled: false,
            monitor_port: default_monitor_port(),
            proxy: ProxySettings::default(),
            relay: relay::RelaySettings::default(),
            https_mcp: HttpsMcpSettings::default(),
        }
    }
}

fn normalize_settings(
    _app: &tauri::AppHandle,
    mut settings: Settings,
) -> Settings {
    if !matches!(
        settings.proxy.mode.as_str(),
        "direct" | "system" | "manual"
    ) {
        settings.proxy = ProxySettings::default();
    } else if settings.proxy.mode == "system"
        && settings.proxy.applied_url.trim().is_empty()
    {
        if let Ok(Some(url)) = system_proxy_url() {
            settings.proxy.applied_url = url;
        }
    }
    if settings.relay.enabled
        && settings.relay.base_url.trim().is_empty()
    {
        settings.relay.enabled = false;
    }
    // Reading settings must not write to disk: mutations are serialized separately.
    settings
}

pub(super) fn load_settings_checked(
    app: &tauri::AppHandle,
) -> Result<Settings, String> {
    let path = settings_path(app)?;
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound =>
        {
            return Ok(Settings::default());
        }
        Err(error) => {
            return Err(format!(
                "读取设置失败（{}）：{error}",
                path.display()
            ));
        }
    };

    if let Ok(settings) = serde_json::from_str::<Settings>(&text) {
        if settings.connection_intent
            && settings.tunnel_id.trim().is_empty()
        {
            return Err(format!(
                "设置文件状态无效（{}）：存在连接意图但 Tunnel ID 为空。",
                path.display()
            ));
        }
        return Ok(normalize_settings(app, settings));
    }

    let legacy = serde_json::from_str::<Value>(&text).map_err(|error| {
        format!(
            "设置文件已损坏（{}）：{error}",
            path.display()
        )
    })?;
    let tunnel_id = legacy
        .get("connection")
        .and_then(|value| value.get("tunnelId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            format!("无法识别设置文件格式：{}", path.display())
        })?;
    let migrated = Settings {
        tunnel_id: tunnel_id.to_string(),
        remember_key: runtime_key_saved(app),
        auto_reconnect: true,
        connection_intent: false,
        monitor_enabled: false,
        monitor_port: default_monitor_port(),
        proxy: ProxySettings::default(),
        relay: relay::RelaySettings::default(),
        https_mcp: HttpsMcpSettings::default(),
    };
    // Persisted on the next update_settings() transaction; do not write during a read.
    Ok(migrated)
}

pub(super) fn load_settings(app: &tauri::AppHandle) -> Settings {
    load_settings_checked(app).unwrap_or_default()
}

fn write_settings_file(path: &Path, settings: &Settings) -> Result<(), String> {
    let text = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    let parent = path.parent().ok_or("设置文件路径无效")?;
    fs::create_dir_all(parent).map_err(|e| format!("创建设置目录失败：{e}"))?;
    let file_name = path.file_name().and_then(|v| v.to_str()).unwrap_or("settings.json");
    let temp_name = format!(
        "{file_name}.tmp.{}.{}", std::process::id(), TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
    );
    let temp = parent.join(temp_name);
    let result = (|| -> Result<(), String> {
        let mut output = OpenOptions::new().write(true).create_new(true)
            .open(&temp).map_err(|e| format!("创建设置临时文件失败：{e}"))?;
        output.write_all(format!("{text}\n").as_bytes())
            .map_err(|e| format!("写入设置失败：{e}"))?;
        output.sync_all().map_err(|e| format!("同步设置文件失败：{e}"))?;
        fs::rename(&temp, path).map_err(|e| format!("更新设置失败：{e}"))
    })();
    if result.is_err() { let _ = fs::remove_file(&temp); }
    result
}

pub(super) fn update_settings(
    app: &tauri::AppHandle,
    update: impl FnOnce(&mut Settings) -> Result<(), String>,
) -> Result<Settings, String> {
    let _guard = SETTINGS_WRITE_LOCK.get_or_init(|| Mutex::new(())).lock()
        .map_err(|_| "设置操作锁已损坏".to_string())?;
    let mut settings = load_settings_checked(app)?;
    update(&mut settings)?;
    write_settings_file(&settings_path(app)?, &settings)?;
    Ok(settings)
}

#[cfg(test)]
mod settings_file_tests {
    use super::*;
    #[test]
    fn effective_mode_is_shared_and_obeys_stop_markers() {
        let mut settings = Settings::default();
        assert_eq!(effective_mcp_mode(&settings, false, false), "none");
        assert_eq!(effective_mcp_mode(&settings, false, true), "tunnel");
        settings.https_mcp.enabled = true;
        settings.connection_intent = true; // old two-mode settings
        assert_eq!(effective_mcp_mode(&settings, true, true), "https");
        assert_eq!(effective_mcp_mode(&settings, false, true), "none");
        settings.https_mcp.enabled = false;
        assert_eq!(effective_mcp_mode(&settings, false, true), "tunnel");
    }
    #[test]
    fn concurrent_transactions_preserve_both_fields() {
        let dir = std::env::temp_dir().join(format!(
            "chatx-settings-concurrency-{}-{}",
            std::process::id(), TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let path = dir.join("settings.json");
        write_settings_file(&path, &Settings::default()).unwrap();
        let mut handles = Vec::new();
        for index in 0..20 {
            let path = path.clone();
            handles.push(std::thread::spawn(move || {
                let _guard = SETTINGS_WRITE_LOCK.get_or_init(|| Mutex::new(())).lock().unwrap();
                let mut settings: Settings =
                    serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
                if index % 2 == 0 {
                    settings.monitor_port += 1;
                } else {
                    settings.https_mcp.remote_port += 1;
                }
                write_settings_file(&path, &settings).unwrap();
            }));
        }
        for handle in handles { handle.join().unwrap(); }
        let settings: Settings = serde_json::from_str(
            &fs::read_to_string(&path).unwrap()
        ).unwrap();
        assert_eq!(settings.monitor_port, default_monitor_port() + 10);
        assert_eq!(settings.https_mcp.remote_port, default_reverse_mcp_port() + 10);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
    #[test]
    fn atomic_settings_file_roundtrip_and_unique_temps() {
        let dir = std::env::temp_dir().join(format!(
            "chatx-settings-{}-{}", std::process::id(), TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed)
        ));
        let path = dir.join("settings.json");
        let mut value = Settings::default();
        value.https_mcp.authorization_ttl_hours = None;
        write_settings_file(&path, &value).unwrap();
        let first: Settings = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(first.https_mcp.authorization_ttl_hours, None);
        value.monitor_port = 18654;
        write_settings_file(&path, &value).unwrap();
        let second: Settings = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(second.monitor_port, 18654);
        assert_eq!(fs::read_dir(&dir).unwrap().count(), 1);
        fs::remove_dir_all(dir).unwrap();
    }
}
