use super::{relay, state_dir, system_proxy_url};
use crate::secrets::runtime_key_saved;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fs;

fn settings_path(app: &tauri::AppHandle) -> Result<std::path::PathBuf, String> {
    Ok(state_dir(app)?.join("settings.json"))
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
        }
    }
}

fn normalize_settings(
    app: &tauri::AppHandle,
    mut settings: Settings,
) -> Settings {
    let mut changed = false;
    if !matches!(
        settings.proxy.mode.as_str(),
        "direct" | "system" | "manual"
    ) {
        settings.proxy = ProxySettings::default();
        changed = true;
    } else if settings.proxy.mode == "system"
        && settings.proxy.applied_url.trim().is_empty()
    {
        if let Ok(Some(url)) = system_proxy_url() {
            settings.proxy.applied_url = url;
            changed = true;
        }
    }
    if settings.relay.enabled
        && settings.relay.base_url.trim().is_empty()
    {
        settings.relay.enabled = false;
        changed = true;
    }
    if changed {
        let _ = save_settings(app, &settings);
    }
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
    };
    let _ = save_settings(app, &migrated);
    Ok(migrated)
}

pub(super) fn load_settings(app: &tauri::AppHandle) -> Settings {
    load_settings_checked(app).unwrap_or_default()
}

pub(super) fn save_settings(
    app: &tauri::AppHandle,
    settings: &Settings,
) -> Result<(), String> {
    let path = settings_path(app)?;
    let temp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    fs::write(&temp, format!("{text}\n"))
        .map_err(|e| format!("保存设置失败：{e}"))?;
    fs::rename(&temp, &path)
        .map_err(|e| format!("更新设置失败：{e}"))
}
