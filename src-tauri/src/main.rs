#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod monitor;
mod monitor_crypto;
mod monitor_network;
mod monitor_server;
mod relay;

use chatx_relay_protocol::{PairingServerMessage, PairingWsMessage};
use qrcode::{render::svg, QrCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{atomic::{AtomicBool, AtomicU32, Ordering}, Arc, Mutex},
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, State, WindowEvent,
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

const RUNTIME_ALIAS: &str = "chatx-local";
#[cfg(all(target_os = "macos", not(test)))]
const KEYCHAIN_SERVICE: &str = "com.chatgptx.local.runtime-key";
#[cfg(all(target_os = "macos", not(test)))]
const MONITOR_MASTER_KEYCHAIN_SERVICE: &str = "com.chatgptx.local.monitor-master-key";
#[cfg(all(target_os = "macos", not(test)))]
const RELAY_KEYCHAIN_SERVICE: &str = "com.chatgptx.local.relay-credentials";

#[cfg(all(target_os = "macos", test))]
const KEYCHAIN_SERVICE: &str = "com.chatgptx.local.runtime-key.tests";
#[cfg(all(target_os = "macos", test))]
const MONITOR_MASTER_KEYCHAIN_SERVICE: &str = "com.chatgptx.local.monitor-master-key.tests";
#[cfg(all(target_os = "macos", test))]
const RELAY_KEYCHAIN_SERVICE: &str = "com.chatgptx.local.relay-credentials.tests";

#[cfg(target_os = "macos")]
const MONITOR_MASTER_KEYCHAIN_ACCOUNT: &str = "master";
#[cfg(target_os = "macos")]
const RELAY_KEYCHAIN_ACCOUNT: &str = "desktop";

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

// A GUI launched from an MCP shell can inherit Desktop Commander's isolated
// HOME. Resolve the real account home from the passwd database so persistent
// ChatX state never follows an inherited HOME value.
#[cfg(target_os = "macos")]
fn macos_account_home() -> Result<PathBuf, String> {
    use std::{ffi::{CStr, OsStr}, os::unix::ffi::OsStrExt};
    let mut buffer = vec![0u8; 16384];
    loop {
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        let status = unsafe {
            libc::getpwuid_r(
                libc::getuid(),
                entry.as_mut_ptr(),
                buffer.as_mut_ptr().cast(),
                buffer.len(),
                &mut result,
            )
        };
        if status == libc::ERANGE && buffer.len() < 1024 * 1024 {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if status != 0 || result.is_null() {
            return Err(format!("无法读取 macOS 用户主目录（错误码 {status}）。"));
        }
        let entry = unsafe { entry.assume_init() };
        if entry.pw_dir.is_null() {
            return Err("macOS 用户主目录为空。".into());
        }
        let home = PathBuf::from(OsStr::from_bytes(
            unsafe { CStr::from_ptr(entry.pw_dir) }.to_bytes(),
        ));
        if !home.is_absolute() || !home.is_dir() {
            return Err("macOS 用户主目录无效。".into());
        }
        return Ok(home);
    }
}

#[cfg(target_os = "macos")]
fn restore_account_home() -> Result<(), String> {
    let home = macos_account_home()?;
    std::env::set_var("HOME", &home);
    std::env::set_var("USERPROFILE", &home);
    Ok(())
}

fn default_auto_reconnect() -> bool { true }
fn default_monitor_port() -> u16 { 18432 }
fn default_proxy_mode() -> String { "direct".into() }

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
struct ProxySettings {
    #[serde(default = "default_proxy_mode")]
    mode: String,
    #[serde(default)]
    url: String,
    #[serde(default)]
    applied_url: String,
}

impl Default for ProxySettings {
    fn default() -> Self {
        Self { mode: default_proxy_mode(), url: String::new(), applied_url: String::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    tunnel_id: String,
    remember_key: bool,
    #[serde(default = "default_auto_reconnect")]
    auto_reconnect: bool,
    #[serde(default)]
    connection_intent: bool,
    #[serde(default)]
    monitor_enabled: bool,
    #[serde(default = "default_monitor_port")]
    monitor_port: u16,
    #[serde(default)]
    proxy: ProxySettings,
    #[serde(default)]
    relay: relay::RelaySettings,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            tunnel_id: String::new(), remember_key: false, auto_reconnect: true,
            connection_intent: false, monitor_enabled: false, monitor_port: default_monitor_port(),
            proxy: ProxySettings::default(), relay: relay::RelaySettings::default(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MonitorDeviceRecord {
    id: String,
    name: String,
    token_hash: String,
    created_at: u64,
    last_seen_at: Option<u64>,
}

#[derive(Debug, Clone)]
struct MonitorPairingGrant {
    pairing_id: String,
    pairing_code: String,
    code_hash: String,
    expires_at: u64,
}

#[derive(Default)]
struct AppState {
    logs: Mutex<Vec<String>>,
    quitting: AtomicBool,
    desired_connected: AtomicBool,
    reconnecting: AtomicBool,
    reconnect_attempt: AtomicU32,
    runtime_snapshot: Mutex<monitor::TunnelSnapshot>,
    relay_status: Arc<Mutex<relay::RelayStatus>>,
    relay_client: Mutex<Option<relay::RelayClientHandle>>,
    monitor_servers: Mutex<Vec<monitor_server::MonitorServerHandle>>,
    monitor_pairing: Mutex<Option<MonitorPairingGrant>>,
    monitor_devices: Mutex<Option<Vec<MonitorDeviceRecord>>>,
    session_runtime_key: Mutex<Option<String>>,
    runtime_operation: Mutex<()>,
    permission_results: Mutex<Option<Value>>,
}

#[derive(Clone)]
struct RuntimePaths {
    root: PathBuf,
    tunnel: PathBuf,
    node: PathBuf,
    launcher: PathBuf,
    desktop_commander: PathBuf,
}

fn timestamp() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|v| v.as_secs()).unwrap_or(0)
}

fn timestamp_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH)
        .map(|v| v.as_millis() as u64).unwrap_or(0)
}

fn command_text(mut command: Command) -> Option<String> {
    let output = command_output(&mut command).ok()?;
    output.status.success().then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn validate_proxy_url(value: &str) -> Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        return Err("代理地址不能为空。".into());
    }
    if value.chars().any(char::is_whitespace) || value.contains('@') {
        return Err("代理地址不能包含空格或账号密码；当前版本仅支持无认证代理。".into());
    }
    if !["http://", "https://", "socks5://", "socks5h://"]
        .iter()
        .any(|prefix| value.starts_with(prefix))
    {
        return Err("代理地址必须以 http://、https://、socks5:// 或 socks5h:// 开头。".into());
    }
    let rest = value.split_once("://").map(|(_, rest)| rest).unwrap_or("");
    if rest.is_empty() || rest.contains('/') {
        return Err("代理地址仅支持 host:port，不支持路径。".into());
    }
    Ok(value.to_string())
}

fn proxy_host_port(value: &str) -> Option<(String, u16)> {
    let value = validate_proxy_url(value).ok()?;
    let (scheme, authority) = value.split_once("://")?;
    if authority.starts_with('[') {
        let end = authority.find(']')?;
        let host = authority[1..end].to_string();
        let port = authority.get(end + 1..)?.strip_prefix(':')?.parse().ok()?;
        return Some((host, port));
    }
    if let Some((host, port)) = authority.rsplit_once(':') {
        if !host.is_empty() {
            return Some((host.to_string(), port.parse().ok()?));
        }
    }
    let port = match scheme {
        "http" => 80,
        "https" => 443,
        "socks5" | "socks5h" => 1080,
        _ => return None,
    };
    Some((authority.to_string(), port))
}

fn tcp_probe(host: &str, port: u16) -> bool {
    use std::net::{TcpStream, ToSocketAddrs};
    let Ok(addresses) = (host, port).to_socket_addrs() else { return false; };
    addresses.into_iter().take(4).any(|address| {
        TcpStream::connect_timeout(&address, Duration::from_millis(800)).is_ok()
    })
}

fn proxy_value(text: &str, key: &str) -> Option<String> {
    let prefix = format!("{key} : ");
    text.lines()
        .map(str::trim)
        .find_map(|line| line.strip_prefix(&prefix).map(str::trim))
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

#[cfg(target_os = "macos")]
fn system_proxy_url() -> Result<Option<String>, String> {
    let mut command = Command::new("/usr/sbin/scutil");
    command.arg("--proxy");
    let output = command_output(&mut command)?;
    if !output.status.success() {
        return Err(format!("读取 macOS 系统代理失败：{}", output_text(&output)));
    }
    let text = String::from_utf8_lossy(&output.stdout);
    let enabled = |key: &str| proxy_value(&text, key).as_deref() == Some("1");
    let explicit = [
        ("HTTPSEnable", "HTTPSProxy", "HTTPSPort", "http"),
        ("HTTPEnable", "HTTPProxy", "HTTPPort", "http"),
        ("SOCKSEnable", "SOCKSProxy", "SOCKSPort", "socks5"),
    ];
    for (flag, host_key, port_key, scheme) in explicit {
        if !enabled(flag) { continue; }
        if let (Some(host), Some(port)) = (proxy_value(&text, host_key), proxy_value(&text, port_key)) {
            return Ok(Some(validate_proxy_url(&format!("{scheme}://{host}:{port}"))?));
        }
    }
    if enabled("ProxyAutoConfigEnable") {
        return Err("检测到 macOS PAC 自动代理；Tunnel 当前只支持明确的 HTTP/HTTPS/SOCKS 代理地址。".into());
    }
    Ok(None)
}

#[cfg(windows)]
fn system_proxy_url() -> Result<Option<String>, String> {
    let mut command = Command::new("powershell.exe");
    command.args([
        "-NoProfile", "-NonInteractive", "-Command",
        "$p=Get-ItemProperty 'HKCU:\\Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings'; if($p.ProxyEnable -eq 1){$p.ProxyServer}",
    ]);
    let Some(raw) = command_text(command) else { return Ok(None); };
    let selected = raw.split(';')
        .find_map(|part| part.strip_prefix("https=").or_else(|| part.strip_prefix("http=")))
        .unwrap_or(raw.as_str())
        .trim();
    if selected.is_empty() { return Ok(None); }
    let url = if selected.contains("://") { selected.to_string() } else { format!("http://{selected}") };
    Ok(Some(validate_proxy_url(&url)?))
}

#[cfg(not(any(target_os = "macos", windows)))]
fn system_proxy_url() -> Result<Option<String>, String> {
    for key in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                return Ok(Some(validate_proxy_url(&value)?));
            }
        }
    }
    Ok(None)
}

fn effective_proxy_url(settings: &Settings) -> Result<Option<String>, String> {
    match settings.proxy.mode.as_str() {
        "direct" => Ok(None),
        "system" => {
            if settings.proxy.applied_url.trim().is_empty() {
                Ok(None)
            } else {
                Ok(Some(validate_proxy_url(&settings.proxy.applied_url)?))
            }
        }
        "manual" => Ok(Some(validate_proxy_url(&settings.proxy.url)?)),
        _ => Err("未知 Tunnel 代理模式。".into()),
    }
}

fn apply_proxy_env(command: &mut Command, settings: &Settings) -> Result<Option<String>, String> {
    for key in [
        "HTTP_PROXY", "HTTPS_PROXY", "ALL_PROXY", "NO_PROXY",
        "http_proxy", "https_proxy", "all_proxy", "no_proxy",
    ] {
        command.env_remove(key);
    }
    let proxy = effective_proxy_url(settings)?;
    if let Some(value) = proxy.as_ref() {
        for key in ["HTTP_PROXY", "HTTPS_PROXY", "http_proxy", "https_proxy"] {
            command.env(key, value);
        }
        if value.starts_with("socks5") {
            command.env("ALL_PROXY", value).env("all_proxy", value);
        }
    }
    Ok(proxy)
}

fn host_device_name() -> String {
    let command = Command::new("hostname");
    command_text(command).filter(|value| !value.is_empty()).unwrap_or_else(|| "Computer".into())
}

#[cfg(target_os = "macos")]
fn battery_snapshot() -> Value {
    let mut command = Command::new("/usr/bin/pmset");
    command.args(["-g", "batt"]);
    let Some(text) = command_text(command) else { return Value::Null; };
    let percent = text.find('%').and_then(|index| {
        let bytes = text.as_bytes();
        let mut start = index;
        while start > 0 && bytes[start - 1].is_ascii_digit() { start -= 1; }
        text.get(start..index)?.parse::<u32>().ok()
    });
    json!({
        "present": percent.is_some(),
        "percent": percent,
        "charging": text.to_ascii_lowercase().contains("charging"),
        "powerSource": if text.contains("AC Power") { "ac" } else { "battery" }
    })
}

#[cfg(target_os = "windows")]
fn battery_snapshot() -> Value {
    let mut command = Command::new("powershell.exe");
    command.args([
        "-NoProfile",
        "-Command",
        "$b=Get-CimInstance Win32_Battery | Select-Object -First 1; if($b){$b.EstimatedChargeRemaining}",
    ]);
    let percent = command_text(command).and_then(|value| value.parse::<u32>().ok());
    json!({"present":percent.is_some(),"percent":percent,"charging":false,"powerSource":Value::Null})
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn battery_snapshot() -> Value { Value::Null }

fn host_snapshot() -> Value {
    json!({
        "deviceName": host_device_name(),
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "battery": battery_snapshot()
    })
}

fn push_log(state: &AppState, message: impl Into<String>) {
    let Ok(mut logs) = state.logs.lock() else { return; };
    logs.push(format!("[{}] {}", timestamp(), message.into()));
    if logs.len() > 300 {
        let drain = logs.len() - 300;
        logs.drain(0..drain);
    }
}

#[cfg(target_os = "macos")]
fn state_dir(_app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = macos_account_home()?
        .join("Library")
        .join("Application Support")
        .join("com.chatgptx.local")
        .join("state");
    fs::create_dir_all(&dir)
        .map_err(|e| format!("无法创建 ChatX 数据目录：{e}"))?;
    Ok(dir)
}

#[cfg(not(target_os = "macos"))]
fn state_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_local_data_dir()
        .map_err(|e| format!("无法定位 ChatX 数据目录：{e}"))?
        .join("state");
    fs::create_dir_all(&dir)
        .map_err(|e| format!("无法创建 ChatX 数据目录：{e}"))?;
    Ok(dir)
}

fn settings_path(app: &tauri::AppHandle) -> Result<PathBuf, String> { Ok(state_dir(app)?.join("settings.json")) }
fn monitor_identity_path(app: &tauri::AppHandle) -> Result<PathBuf, String> { Ok(state_dir(app)?.join("monitor-identity.json")) }
fn monitor_devices_path(app: &tauri::AppHandle) -> Result<PathBuf, String> { Ok(state_dir(app)?.join("monitor-devices.json")) }
fn secret_path(app: &tauri::AppHandle) -> Result<PathBuf, String> { Ok(state_dir(app)?.join("runtime-key.dpapi")) }
#[cfg(windows)]
fn monitor_secret_path(app: &tauri::AppHandle) -> Result<PathBuf, String> { Ok(state_dir(app)?.join("monitor-token.dpapi")) }
#[cfg(windows)]
fn monitor_master_secret_path(app: &tauri::AppHandle) -> Result<PathBuf, String> { Ok(state_dir(app)?.join("monitor-master-key.dpapi")) }
#[cfg(windows)]
fn relay_secret_path(app: &tauri::AppHandle) -> Result<PathBuf, String> { Ok(state_dir(app)?.join("relay-credentials.dpapi")) }
fn call_history_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(desktop_commander_home(app)?.join(".claude-server-commander").join("tool-history.jsonl"))
}
fn monitor_activity_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(desktop_commander_home(app)?.join(".chatx-monitor").join("activity.json"))
}

fn parse_recent_monitor_history(text: &str, limit: usize) -> Vec<Value> {
    text.lines()
        .rev()
        .filter_map(|line| serde_json::from_str::<Value>(line).ok())
        .filter_map(|record| {
            let tool_name = record.get("toolName")?.as_str()?.to_string();
            let timestamp = record.get("timestamp").and_then(Value::as_str).map(str::to_string);
            let duration_ms = record.get("duration").and_then(Value::as_u64);
            let success = record.get("output")
                .and_then(|output| output.get("isError"))
                .and_then(Value::as_bool) != Some(true);
            Some(json!({
                "toolName": tool_name,
                "timestamp": timestamp,
                "startedAt": Value::Null,
                "durationMs": duration_ms,
                "success": success,
                "running": false
            }))
        })
        .take(limit)
        .collect()
}

fn read_history_tail(path: &Path, max_bytes: u64) -> String {
    let Ok(mut file) = fs::File::open(path) else { return String::new(); };
    let Ok(metadata) = file.metadata() else { return String::new(); };
    let len = metadata.len();
    let start = len.saturating_sub(max_bytes);
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

fn recent_monitor_calls(
    app: &tauri::AppHandle,
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
    if calls.len() < LIMIT {
        if let Ok(path) = call_history_path(app) {
            let tail = read_history_tail(&path, 256 * 1024);
            calls.extend(parse_recent_monitor_history(&tail, LIMIT - calls.len()));
        }
    }
    calls
}

fn monitor_status_payload(app: &tauri::AppHandle, state: &AppState) -> Value {
    let now_ms = timestamp_ms();
    let activity = monitor_activity_path(app).ok()
        .and_then(|path| monitor::read_activity_snapshot(&path));
    let mcp_status = monitor::evaluate_activity(activity.as_ref(), now_ms);
    let recent_calls = recent_monitor_calls(app, activity.as_ref(), now_ms);
    let mut mcp = serde_json::to_value(mcp_status).unwrap_or_else(|_| json!({}));
    if let Value::Object(object) = &mut mcp {
        object.insert("recentCalls".into(), Value::Array(recent_calls));
    }
    let tunnel = state.runtime_snapshot.lock()
        .map(|snapshot| snapshot.clone()).unwrap_or_default();
    let settings = load_settings(app);
    let endpoints = monitor_network::discover_monitor_endpoints(settings.monitor_port);
    json!({
        "schemaVersion": 1,
        "serverTime": now_ms,
        "endpoints": endpoints,
        "chatx": {"version": APP_VERSION, "platform": format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)},
        "host": host_snapshot(),
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
            "reconnecting": state.reconnecting.load(Ordering::SeqCst),
            "reconnectAttempt": state.reconnect_attempt.load(Ordering::SeqCst)
        },
        "mcp": mcp
    })
}

fn runtime_key_saved(app: &tauri::AppHandle) -> bool {
    #[cfg(windows)]
    { secret_path(app).map(|path| path.is_file()).unwrap_or(false) }
    #[cfg(target_os = "macos")]
    {
        let _ = app;
        keychain_key_saved()
    }
    #[cfg(not(any(windows, target_os = "macos")))]
    { let _ = app; false }
}

fn runtime_binary_name(base: &str) -> String {
    if cfg!(windows) { format!("{base}.exe") } else { base.to_string() }
}

fn load_settings(app: &tauri::AppHandle) -> Settings {
    let Ok(path) = settings_path(app) else { return Settings::default(); };
    let Ok(text) = fs::read_to_string(&path) else { return Settings::default(); };

    if let Ok(mut settings) = serde_json::from_str::<Settings>(&text) {
        let mut changed = false;
        if !matches!(settings.proxy.mode.as_str(), "direct" | "system" | "manual") {
            settings.proxy = ProxySettings::default();
            changed = true;
        } else if settings.proxy.mode == "system" && settings.proxy.applied_url.trim().is_empty() {
            if let Ok(Some(url)) = system_proxy_url() {
                settings.proxy.applied_url = url;
                changed = true;
            }
        }
        if settings.relay.enabled && settings.relay.base_url.trim().is_empty() {
            // Settings written by the removed SSH reverse-relay implementation
            // deserialize with enabled=true but no baseUrl. Never silently map
            // an SSH host into the new WSS relay trust boundary.
            settings.relay.enabled = false;
            changed = true;
        }
        if changed {
            let _ = save_settings(app, &settings);
        }
        return settings;
    }

    // ChatX <= 0.2.1 stored the Tunnel ID under connection.tunnelId. Preserve
    // that value when the desktop bridge first starts, then rewrite the small
    // 0.3.x settings shape. The DPAPI file format itself is unchanged.
    let Ok(legacy) = serde_json::from_str::<Value>(&text) else { return Settings::default(); };
    let tunnel_id = legacy
        .get("connection")
        .and_then(|value| value.get("tunnelId"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let Some(tunnel_id) = tunnel_id else { return Settings::default(); };
    let remember_key = runtime_key_saved(app);
    let migrated = Settings {
        tunnel_id: tunnel_id.to_string(), remember_key, auto_reconnect: true,
        connection_intent: false, monitor_enabled: false, monitor_port: default_monitor_port(),
        proxy: ProxySettings::default(), relay: relay::RelaySettings::default(),
    };
    let _ = save_settings(app, &migrated);
    migrated
}

fn save_settings(app: &tauri::AppHandle, settings: &Settings) -> Result<(), String> {
    let path = settings_path(app)?;
    let temp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(settings).map_err(|e| e.to_string())?;
    fs::write(&temp, format!("{text}\n")).map_err(|e| format!("保存设置失败：{e}"))?;
    fs::rename(&temp, &path).map_err(|e| format!("更新设置失败：{e}"))
}

fn ensure_monitor_desktop_id(app: &tauri::AppHandle) -> Result<String, String> {
    let path = monitor_identity_path(app)?;
    if let Ok(text) = fs::read_to_string(&path) {
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            if let Some(desktop_id) = value.get("desktopId").and_then(Value::as_str) {
                let desktop_id = desktop_id.trim();
                if desktop_id.starts_with("d_")
                    && desktop_id.len() >= 18
                    && desktop_id.len() <= 80
                    && desktop_id[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
                {
                    return Ok(desktop_id.to_string());
                }
            }
        }
    }

    let random = monitor_server::generate_monitor_token()?;
    let desktop_id = format!("d_{}", &random[..32]);
    let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(&json!({
        "schemaVersion": 1,
        "desktopId": desktop_id
    })).map_err(|e| format!("序列化 Desktop Identity 失败：{e}"))?;
    fs::write(&temp, format!("{text}\n"))
        .map_err(|e| format!("保存 Desktop Identity 临时文件失败：{e}"))?;
    fs::rename(&temp, &path)
        .map_err(|e| format!("更新 Desktop Identity 失败：{e}"))?;
    Ok(desktop_id)
}

fn resource_candidates(app: &tauri::AppHandle) -> Vec<PathBuf> {
    let mut result = Vec::new();
    if let Ok(exe) = std::env::current_exe() {
        if let Some(parent) = exe.parent() { result.push(parent.to_path_buf()); }
    }
    if let Ok(resource_dir) = app.path().resource_dir() {
        if !result.iter().any(|item| item == &resource_dir) { result.push(resource_dir); }
    }
    if let Ok(root) = std::env::var("CHATX_RESOURCE_DIR") { result.insert(0, PathBuf::from(root)); }
    result
}

fn runtime_paths(app: &tauri::AppHandle) -> Result<RuntimePaths, String> {
    for root in resource_candidates(app) {
        let tunnel = root.join(runtime_binary_name("tunnel-client"));
        let node = root.join(runtime_binary_name("node"));
        let launcher = root.join("desktop-commander-launcher.mjs");
        let desktop_commander = root.join("desktop-commander").join("dist").join("index.js");
        if tunnel.is_file() && node.is_file() && launcher.is_file() && desktop_commander.is_file() {
            return Ok(RuntimePaths { root, tunnel, node, launcher, desktop_commander });
        }
    }
    Err("未找到完整运行组件。请重新安装 ChatX，或先运行 npm run desktop:prepare。".into())
}

fn command_output(command: &mut Command) -> Result<Output, String> {
    command.stdin(Stdio::null());
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);
    command.output().map_err(|e| format!("启动进程失败：{e}"))
}

fn run_tunnel(
    app: &tauri::AppHandle,
    paths: &RuntimePaths,
    args: &[String],
    runtime_key: Option<&str>,
) -> Result<Output, String> {
    let mut command = Command::new(&paths.tunnel);
    command.args(args);
    if let Some(key) = runtime_key { command.env("CHATX_TUNNEL_RUNTIME_KEY", key); }
    let settings = load_settings(app);
    let _ = apply_proxy_env(&mut command, &settings)?;
    command_output(&mut command)
}

fn output_text(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stdout.is_empty() { stderr } else if stderr.is_empty() { stdout } else { format!("{stdout}\n{stderr}") }
}

fn pmset_disables_sleep(text: &str) -> bool {
    text.lines().any(|line| {
        let mut parts = line.split_whitespace();
        matches!(
            (parts.next(), parts.next()),
            (Some("SleepDisabled"), Some("1")) | (Some("disablesleep"), Some("1"))
        )
    })
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "macos")]
fn macos_clamshell_marker_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(state_dir(app)?.join("clamshell-awake.owner"))
}

#[cfg(target_os = "macos")]
fn macos_clamshell_owned_by_current_process(app: &tauri::AppHandle) -> bool {
    let Ok(path) = macos_clamshell_marker_path(app) else { return false; };
    let Ok(value) = fs::read_to_string(path) else { return false; };
    value.trim().starts_with(&format!("{}:", std::process::id()))
}

#[cfg(target_os = "macos")]
fn macos_clamshell_awake_enabled() -> Result<bool, String> {
    let mut command = Command::new("/usr/bin/pmset");
    command.arg("-g");
    let output = command_output(&mut command)?;
    if !output.status.success() {
        return Err(format!("读取 macOS 电源设置失败：{}", output_text(&output)));
    }
    Ok(pmset_disables_sleep(&String::from_utf8_lossy(&output.stdout)))
}

#[cfg(target_os = "macos")]
fn run_macos_admin_shell(shell_command: &str) -> Result<(), String> {
    let mut command = Command::new("/usr/bin/osascript");
    command
        .args(["-e", "do shell script (system attribute \"CHATX_PRIVILEGED_COMMAND\") with administrator privileges"])
        .env("CHATX_PRIVILEGED_COMMAND", shell_command);
    let output = command_output(&mut command)?;
    if output.status.success() { return Ok(()); }
    let detail = output_text(&output);
    if detail.contains("(-128)") || detail.to_ascii_lowercase().contains("user canceled") {
        return Err("已取消管理员授权，合盖运行设置未更改。".into());
    }
    Err(format!("修改 macOS 合盖运行设置失败：{detail}"))
}

#[cfg(target_os = "macos")]
fn macos_enable_clamshell_with_watchdog(app: &tauri::AppHandle) -> Result<(), String> {
    let marker = macos_clamshell_marker_path(app)?;
    let pid = std::process::id();
    let nonce = SystemTime::now().duration_since(UNIX_EPOCH).map(|value| value.as_nanos()).unwrap_or(0);
    let owner = format!("{pid}:{nonce}");
    fs::write(&marker, format!("{owner}\n")).map_err(|e| format!("记录合盖运行所有权失败：{e}"))?;

    let marker_q = shell_quote(&marker.to_string_lossy());
    let owner_q = shell_quote(&owner);
    let watchdog = format!(
        "PID={pid}; MARKER={marker_q}; OWNER={owner_q}; while /bin/kill -0 \"$PID\" 2>/dev/null; do CURRENT=$(/bin/cat \"$MARKER\" 2>/dev/null || true); [ \"$CURRENT\" = \"$OWNER\" ] || exit 0; /bin/sleep 2; done; /bin/sleep 1; CURRENT=$(/bin/cat \"$MARKER\" 2>/dev/null || true); [ \"$CURRENT\" = \"$OWNER\" ] || exit 0; /usr/bin/pmset -a disablesleep 0 >/dev/null 2>&1; /bin/rm -f \"$MARKER\""
    );
    let privileged = format!(
        "set -e; /usr/bin/pmset -a disablesleep 1; /usr/bin/nohup /bin/sh -c {} >/dev/null 2>&1 </dev/null &",
        shell_quote(&watchdog)
    );
    if let Err(error) = run_macos_admin_shell(&privileged) {
        let _ = fs::remove_file(&marker);
        return Err(error);
    }
    if !macos_clamshell_awake_enabled()? {
        let _ = fs::remove_file(&marker);
        return Err("macOS 未应用合盖运行设置，请检查系统电源策略。".into());
    }
    Ok(())
}

fn parse_json_output(output: &Output) -> Option<Value> { serde_json::from_slice::<Value>(&output.stdout).ok() }

fn executable_version(path: &Path) -> String {
    let mut command = Command::new(path);
    command.arg("--version");
    match command_output(&mut command) { Ok(output) => output_text(&output), Err(error) => error }
}

#[cfg(windows)]
fn protect_secret(secret: &str, target: &Path) -> Result<(), String> {
    let script = r#"$ErrorActionPreference='Stop';[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);Add-Type -AssemblyName System.Security;$b=[Text.Encoding]::UTF8.GetBytes($env:CHATX_SECRET);$p=[System.Security.Cryptography.ProtectedData]::Protect($b,$null,[System.Security.Cryptography.DataProtectionScope]::CurrentUser);[IO.File]::WriteAllText($env:CHATX_SECRET_FILE,[Convert]::ToBase64String($p))"#;
    let mut command = Command::new("powershell.exe");
    command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script])
        .env("CHATX_SECRET", secret).env("CHATX_SECRET_FILE", target);
    let output = command_output(&mut command)?;
    if output.status.success() { Ok(()) } else { Err(format!("保存 Runtime Key 失败：{}", output_text(&output))) }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn protect_secret(_secret: &str, _target: &Path) -> Result<(), String> { Err("安全保存 Runtime Key 当前仅支持 Windows。".into()) }

#[cfg(windows)]
fn unprotect_secret(target: &Path) -> Result<String, String> {
    let script = r#"$ErrorActionPreference='Stop';[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);Add-Type -AssemblyName System.Security;$s=[IO.File]::ReadAllText($env:CHATX_SECRET_FILE);$p=[Convert]::FromBase64String($s);$b=[System.Security.Cryptography.ProtectedData]::Unprotect($p,$null,[System.Security.Cryptography.DataProtectionScope]::CurrentUser);[Console]::Out.Write([Text.Encoding]::UTF8.GetString($b))"#;
    let mut command = Command::new("powershell.exe");
    command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script]).env("CHATX_SECRET_FILE", target);
    let output = command_output(&mut command)?;
    if !output.status.success() { return Err(format!("读取已保存 Runtime Key 失败：{}", output_text(&output))); }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn unprotect_secret(_target: &Path) -> Result<String, String> { Err("安全读取 Runtime Key 当前仅支持 Windows。".into()) }

#[cfg(target_os = "macos")]
fn keychain_key_saved() -> bool {
    use security_framework::item::{ItemClass, ItemSearchOptions};
    // Poll metadata only; never retrieve the secret during status refresh.
    ItemSearchOptions::new().class(ItemClass::generic_password())
        .service(KEYCHAIN_SERVICE).account(RUNTIME_ALIAS)
        .load_attributes(true).load_data(false).search().is_ok()
}

#[cfg(target_os = "macos")]
fn clear_keychain_key() -> Result<(), String> {
    match security_framework::passwords::delete_generic_password(KEYCHAIN_SERVICE, RUNTIME_ALIAS) {
        Ok(()) => Ok(()),
        // errSecItemNotFound: clearing an absent key is already successful.
        Err(error) if error.code() == -25300 => Ok(()),
        Err(error) => Err(format!("清除 macOS 钥匙串中的 Runtime Key 失败：{error}")),
    }
}

#[cfg(target_os = "macos")]
fn protect_secret(secret: &str, _target: &Path) -> Result<(), String> {
    security_framework::passwords::set_generic_password(KEYCHAIN_SERVICE, RUNTIME_ALIAS, secret.as_bytes())
        .map_err(|e| format!("保存 Runtime Key 到 macOS 钥匙串失败（错误码 {}）：{e}。可取消勾选保存密钥后重试；无需还原系统钥匙串。", e.code()))
}

#[cfg(target_os = "macos")]
fn unprotect_secret(_target: &Path) -> Result<String, String> {
    let bytes = security_framework::passwords::get_generic_password(KEYCHAIN_SERVICE, RUNTIME_ALIAS)
        .map_err(|e| format!("读取 macOS 钥匙串失败，请允许 ChatX 访问钥匙串或重新输入 Runtime API Key：{e}"))?;
    String::from_utf8(bytes).map_err(|_| "钥匙串中的 Runtime Key 编码无效，请重新保存。".into())
}


#[cfg(windows)]
fn read_monitor_master_key(app: &tauri::AppHandle) -> Result<String, String> {
    unprotect_secret(&monitor_master_secret_path(app)?)
        .map_err(|e| e.replace("Runtime Key", "Monitor Master Key"))
}

#[cfg(target_os = "macos")]
fn read_monitor_master_key(_app: &tauri::AppHandle) -> Result<String, String> {
    let bytes = security_framework::passwords::get_generic_password(
        MONITOR_MASTER_KEYCHAIN_SERVICE,
        MONITOR_MASTER_KEYCHAIN_ACCOUNT,
    ).map_err(|e| format!("读取 Monitor Master Key 失败：{e}"))?;
    String::from_utf8(bytes).map_err(|_| "Monitor Master Key 编码无效。".into())
}

#[cfg(not(any(windows, target_os = "macos")))]
fn read_monitor_master_key(_app: &tauri::AppHandle) -> Result<String, String> {
    Err("安全读取 Monitor Master Key 当前仅支持 Windows 和 macOS。".into())
}

#[cfg(windows)]
fn save_monitor_master_key(app: &tauri::AppHandle, value: &str) -> Result<(), String> {
    protect_secret(value, &monitor_master_secret_path(app)?)
        .map_err(|e| e.replace("Runtime Key", "Monitor Master Key"))
}

#[cfg(target_os = "macos")]
fn save_monitor_master_key(_app: &tauri::AppHandle, value: &str) -> Result<(), String> {
    security_framework::passwords::set_generic_password(
        MONITOR_MASTER_KEYCHAIN_SERVICE,
        MONITOR_MASTER_KEYCHAIN_ACCOUNT,
        value.as_bytes(),
    ).map_err(|e| format!("保存 Monitor Master Key 到 macOS 钥匙串失败：{e}"))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn save_monitor_master_key(_app: &tauri::AppHandle, _value: &str) -> Result<(), String> {
    Err("安全保存 Monitor Master Key 当前仅支持 Windows 和 macOS。".into())
}

fn ensure_monitor_master_key(app: &tauri::AppHandle) -> Result<[u8; 32], String> {
    if let Ok(value) = read_monitor_master_key(app) {
        return monitor_crypto::decode_master_key(&value);
    }
    let key = monitor_crypto::generate_master_key()?;
    save_monitor_master_key(app, &monitor_crypto::encode_master_key(&key))?;
    Ok(key)
}

#[cfg(windows)]
fn save_relay_credentials(app: &tauri::AppHandle, credentials: &relay::RelayCredentials) -> Result<(), String> {
    let value = serde_json::to_string(credentials)
        .map_err(|e| format!("序列化 Relay Credential 失败：{e}"))?;
    protect_secret(&value, &relay_secret_path(app)?)
        .map_err(|e| e.replace("Runtime Key", "Relay Credential"))
}

#[cfg(target_os = "macos")]
fn save_relay_credentials(_app: &tauri::AppHandle, credentials: &relay::RelayCredentials) -> Result<(), String> {
    let value = serde_json::to_string(credentials)
        .map_err(|e| format!("序列化 Relay Credential 失败：{e}"))?;
    security_framework::passwords::set_generic_password(
        RELAY_KEYCHAIN_SERVICE,
        RELAY_KEYCHAIN_ACCOUNT,
        value.as_bytes(),
    ).map_err(|e| format!("保存 Relay Credential 到 macOS 钥匙串失败：{e}"))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn save_relay_credentials(_app: &tauri::AppHandle, _credentials: &relay::RelayCredentials) -> Result<(), String> {
    Err("安全保存 Relay Credential 当前仅支持 Windows 和 macOS。".into())
}

#[cfg(windows)]
fn read_relay_credentials(app: &tauri::AppHandle) -> Result<relay::RelayCredentials, String> {
    let value = unprotect_secret(&relay_secret_path(app)?)
        .map_err(|e| e.replace("Runtime Key", "Relay Credential"))?;
    serde_json::from_str(&value).map_err(|e| format!("Relay Credential 编码无效：{e}"))
}

#[cfg(target_os = "macos")]
fn read_relay_credentials(_app: &tauri::AppHandle) -> Result<relay::RelayCredentials, String> {
    let bytes = security_framework::passwords::get_generic_password(
        RELAY_KEYCHAIN_SERVICE,
        RELAY_KEYCHAIN_ACCOUNT,
    ).map_err(|e| format!("读取 Relay Credential 失败：{e}"))?;
    let value = String::from_utf8(bytes).map_err(|_| "Relay Credential 编码无效。".to_string())?;
    serde_json::from_str(&value).map_err(|e| format!("Relay Credential 编码无效：{e}"))
}

#[cfg(not(any(windows, target_os = "macos")))]
fn read_relay_credentials(_app: &tauri::AppHandle) -> Result<relay::RelayCredentials, String> {
    Err("安全读取 Relay Credential 当前仅支持 Windows 和 macOS。".into())
}

fn load_monitor_devices_from_disk(app: &tauri::AppHandle) -> Vec<MonitorDeviceRecord> {
    let Ok(path) = monitor_devices_path(app) else { return Vec::new(); };
    let Ok(text) = fs::read_to_string(path) else { return Vec::new(); };
    serde_json::from_str::<Vec<MonitorDeviceRecord>>(&text).unwrap_or_default()
}

fn save_monitor_devices(
    app: &tauri::AppHandle,
    devices: &[MonitorDeviceRecord],
) -> Result<(), String> {
    let path = monitor_devices_path(app)?;
    let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(devices)
        .map_err(|e| format!("序列化 Monitor 设备失败：{e}"))?;
    fs::write(&temp, format!("{text}\n"))
        .map_err(|e| format!("保存 Monitor 设备临时文件失败：{e}"))?;
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(&path).map_err(|e| format!("替换 Monitor 设备文件失败：{e}"))?;
    }
    fs::rename(&temp, &path).map_err(|e| format!("更新 Monitor 设备失败：{e}"))
}

fn monitor_devices_snapshot(
    app: &tauri::AppHandle,
    state: &AppState,
) -> Vec<MonitorDeviceRecord> {
    let Ok(mut registry) = state.monitor_devices.lock() else { return Vec::new(); };
    if registry.is_none() {
        *registry = Some(load_monitor_devices_from_disk(app));
    }
    registry.as_ref().cloned().unwrap_or_default()
}

fn revoke_monitor_device_local(
    app: &tauri::AppHandle,
    state: &AppState,
    device_id: &str,
) -> Result<bool, String> {
    let mut registry = state.monitor_devices.lock()
        .map_err(|_| "Monitor 设备状态锁已损坏。".to_string())?;
    if registry.is_none() {
        *registry = Some(load_monitor_devices_from_disk(app));
    }
    let devices = registry.as_mut()
        .ok_or_else(|| "Monitor 设备状态不可用。".to_string())?;
    let previous = devices.clone();
    devices.retain(|device| device.id != device_id);
    if devices.len() == previous.len() {
        return Ok(false);
    }
    if let Err(error) = save_monitor_devices(app, devices) {
        *devices = previous;
        return Err(error);
    }
    drop(registry);
    push_log(state, format!("已撤销手机 Monitor 设备：{device_id}"));
    Ok(true)
}

fn revoke_monitor_device_everywhere(
    app: &tauri::AppHandle,
    state: &AppState,
    device_id: &str,
) -> Result<bool, String> {
    let settings = load_settings(app);
    let relay_credentials = read_relay_credentials(app).ok();
    let relay_matches = if !settings.relay.base_url.trim().is_empty() {
        relay_credentials.as_ref().is_some_and(|credentials| {
            relay::normalized_base_url(&credentials.base_url).ok()
                == relay::normalized_base_url(&settings.relay.base_url).ok()
        })
    } else {
        false
    };

    if relay_matches {
        if let Some(credentials) = relay_credentials.as_ref() {
            relay::revoke_device(&settings.relay, credentials, device_id)
                .map_err(|error| {
                    format!(
                        "Relay 端设备撤销失败，本地凭据保持有效；请确认 Relay 可达后重试：{error}"
                    )
                })?;
        }
    }

    match revoke_monitor_device_local(app, state, device_id) {
        Ok(removed) => Ok(removed),
        Err(error) => {
            if relay_matches {
                if let (Some(credentials), Ok(master_key)) = (
                    relay_credentials.as_ref(),
                    ensure_monitor_master_key(app),
                ) {
                    if let Ok(relay_token) =
                        monitor_crypto::derive_relay_token(&master_key, device_id)
                    {
                        let _ = relay::authorize_device(
                            &settings.relay,
                            credentials,
                            device_id,
                            &relay_token,
                        );
                    }
                }
            }
            Err(error)
        }
    }
}

fn monitor_device_token_authorized(
    app: &tauri::AppHandle,
    state: &AppState,
    device_id: &str,
    token: &str,
) -> bool {
    let Ok(mut registry) = state.monitor_devices.lock() else { return false; };
    if registry.is_none() {
        *registry = Some(load_monitor_devices_from_disk(app));
    }
    let Some(devices) = registry.as_mut() else { return false; };
    let now = timestamp_ms();
    let Some(index) = devices.iter().position(|device| {
        device.id == device_id
            && monitor_server::verify_token_hash_hex(token, &device.token_hash)
    }) else {
        return false;
    };
    let should_update = devices[index].last_seen_at
        .map(|seen| now.saturating_sub(seen) >= 60_000)
        .unwrap_or(true);
    if should_update {
        let previous = devices[index].last_seen_at;
        devices[index].last_seen_at = Some(now);
        if save_monitor_devices(app, devices).is_err() {
            devices[index].last_seen_at = previous;
        }
    }
    true
}

fn complete_monitor_pairing(
    app: &tauri::AppHandle,
    state: &AppState,
    payload: Value,
) -> Result<Value, String> {
    let code = payload.get("pairingCode")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "缺少 pairingCode。".to_string())?;
    let raw_name = payload.get("deviceName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Android Device");
    let name = raw_name.chars().take(80).collect::<String>();

    let mut pairing = state.monitor_pairing.lock()
        .map_err(|_| "Monitor 配对状态锁已损坏。".to_string())?;
    let Some(grant) = pairing.as_ref() else {
        return Err("当前没有有效的配对请求。".into());
    };
    if timestamp_ms() > grant.expires_at {
        *pairing = None;
        return Err("配对二维码已过期，请在 ChatX 中重新生成。".into());
    }
    if !monitor_server::verify_token_hash_hex(code, &grant.code_hash) {
        return Err("配对码无效。".into());
    }
    let pairing_id = grant.pairing_id.clone();
    *pairing = None;
    drop(pairing);

    let mut registry = state.monitor_devices.lock()
        .map_err(|_| "Monitor 设备状态锁已损坏。".to_string())?;
    if registry.is_none() {
        *registry = Some(load_monitor_devices_from_disk(app));
    }
    let devices = registry.as_mut()
        .ok_or_else(|| "Monitor 设备状态不可用。".to_string())?;
    if devices.len() >= 32 {
        return Err("已配对设备数量已达到上限 32。".into());
    }
    let master_key = ensure_monitor_master_key(app)?;
    let random_id = monitor_server::generate_monitor_token()?;
    let device_id = format!("dev_{}", &random_id[..24]);
    let direct_token = monitor_crypto::derive_direct_token(&master_key, &device_id)?;
    let relay_token = monitor_crypto::derive_relay_token(&master_key, &device_id)?;
    let device_key = monitor_crypto::device_key_b64(&master_key, &device_id)?;
    let token_hash = monitor_server::token_hash_hex(&direct_token);
    let settings = load_settings(app);
    let relay_enrollment = if settings.relay.enabled {
        if let Some(credentials) = relay_credentials_for_settings(app, &settings) {
            relay::authorize_device(&settings.relay, &credentials, &device_id, &relay_token)?;
            Some(json!({
                "baseUrl": settings.relay.base_url,
                "desktopId": credentials.desktop_id,
                "deviceToken": relay_token
            }))
        } else {
            None
        }
    } else {
        None
    };

    let now = timestamp_ms();
    let previous = devices.clone();
    devices.push(MonitorDeviceRecord {
        id: device_id.clone(),
        name: name.clone(),
        token_hash,
        created_at: now,
        last_seen_at: Some(now),
    });
    if let Err(error) = save_monitor_devices(app, devices) {
        if settings.relay.enabled {
            if let Ok(credentials) = read_relay_credentials(app) {
                let _ = relay::revoke_device(&settings.relay, &credentials, &device_id);
            }
        }
        *devices = previous;
        return Err(error);
    }
    if settings.relay.enabled {
        if let Ok(credentials) = read_relay_credentials(app) {
            let _ = relay::delete_pairing_route(
                &settings.relay,
                &credentials,
                &pairing_id,
            );
        }
    }
    push_log(state, format!("手机 Monitor 已配对设备：{name} ({device_id})"));
    Ok(json!({
        "schemaVersion": 3,
        "deviceId": device_id,
        "deviceName": name,
        "directToken": direct_token,
        "deviceKey": device_key,
        "relay": relay_enrollment
    }))
}

fn handle_encrypted_monitor_pairing(
    app: &tauri::AppHandle,
    state: &AppState,
    message: PairingWsMessage,
) -> Result<PairingServerMessage, String> {
    let PairingWsMessage::Pair { pairing_id, payload } = message;
    let grant = {
        let mut pairing = state.monitor_pairing.lock()
            .map_err(|_| "Monitor 配对状态锁已损坏。".to_string())?;
        let Some(grant) = pairing.as_ref() else {
            return Err("当前没有有效的配对请求。".into());
        };
        if timestamp_ms() > grant.expires_at {
            *pairing = None;
            return Err("配对二维码已过期，请在 ChatX 中重新生成。".into());
        }
        if grant.pairing_id != pairing_id {
            return Err("Pairing route 无效。".into());
        }
        grant.clone()
    };

    let plaintext = monitor_crypto::decrypt_pairing_payload(
        &grant.pairing_code,
        &pairing_id,
        "request",
        &payload,
    )?;
    let request = serde_json::from_slice::<Value>(&plaintext)
        .map_err(|_| "Pairing E2EE payload 不是有效 JSON。".to_string())?;
    let result = complete_monitor_pairing(app, state, request)?;
    let clear = serde_json::to_vec(&result)
        .map_err(|e| format!("序列化 Pairing result 失败：{e}"))?;
    let payload = monitor_crypto::encrypt_pairing_payload(
        &grant.pairing_code,
        &pairing_id,
        "response",
        &clear,
    )?;
    Ok(PairingServerMessage::PairResult { pairing_id, payload })
}

fn monitor_tls_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(state_dir(app)?.join("monitor-tls"))
}

fn monitor_info_payload(app: &tauri::AppHandle, state: &AppState) -> Value {
    let settings = load_settings(app);
    let servers = state.monitor_servers.lock().ok();
    let running = servers.as_ref().is_some_and(|servers| !servers.is_empty());
    let bind_addresses = servers.as_ref().map(|servers| {
        servers.iter().map(|server| server.addr.to_string()).collect::<Vec<_>>()
    }).unwrap_or_default();
    let fingerprint = servers.as_ref()
        .and_then(|servers| servers.first())
        .map(|server| server.fingerprint_sha256.clone());
    let endpoints = if running {
        let supports_ipv4 = servers.as_ref().is_some_and(|servers| servers.iter().any(|server| server.addr.is_ipv4()));
        let supports_ipv6 = servers.as_ref().is_some_and(|servers| servers.iter().any(|server| server.addr.is_ipv6()));
        monitor_network::discover_monitor_endpoints(settings.monitor_port)
            .into_iter()
            .filter(|endpoint| {
                (endpoint.family == "ipv4" && supports_ipv4)
                    || (endpoint.family == "ipv6" && supports_ipv6)
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let devices = monitor_devices_snapshot(app, state).into_iter().map(|device| json!({
        "id": device.id,
        "name": device.name,
        "createdAt": device.created_at,
        "lastSeenAt": device.last_seen_at
    })).collect::<Vec<_>>();
    json!({
        "enabled": settings.monitor_enabled,
        "port": settings.monitor_port,
        "running": running,
        "desktopId": ensure_monitor_desktop_id(app).ok(),
        "protocol": "chatx-monitor-wss-v1",
        "encryption": "AES-256-GCM",
        "bindAddress": if bind_addresses.is_empty() { None } else { Some(bind_addresses.join(" · ")) },
        "bindAddresses": bind_addresses,
        "fingerprintSha256": fingerprint,
        "devices": devices,
        "endpoints": endpoints
    })
}

fn start_monitor_service(app: &tauri::AppHandle, state: &AppState) -> Result<(), String> {
    let settings = load_settings(app);
    if !settings.monitor_enabled {
        return Ok(());
    }
    if settings.monitor_port < 1024 {
        return Err("Monitor 端口必须为 1024-65535。".into());
    }
    let mut slot = state.monitor_servers.lock()
        .map_err(|_| "Monitor Server 状态锁已损坏。".to_string())?;
    if !slot.is_empty() {
        return Ok(());
    }

    let identity = monitor_server::ensure_tls_identity(&monitor_tls_dir(app)?)?;
    let desktop_id = ensure_monitor_desktop_id(app)?;
    let master_key = ensure_monitor_master_key(app)?;

    let app_for_auth = app.clone();
    let auth_checker: monitor_server::AuthChecker = Arc::new(move |device_id, candidate| {
        let state = app_for_auth.state::<AppState>();
        monitor_device_token_authorized(
            &app_for_auth,
            state.inner(),
            device_id,
            candidate,
        )
    });

    let app_for_pair = app.clone();
    let pair_handler: monitor_server::PairHandler = Arc::new(move |message| {
        let state = app_for_pair.state::<AppState>();
        handle_encrypted_monitor_pairing(&app_for_pair, state.inner(), message)
    });

    let app_for_revoke = app.clone();
    let revoke_handler: monitor_server::RevokeHandler = Arc::new(move |device_id| {
        let state = app_for_revoke.state::<AppState>();
        revoke_monitor_device_everywhere(
            &app_for_revoke,
            state.inner(),
            device_id,
        ).and_then(|removed| {
            if removed { Ok(()) } else { Err("没有找到该已配对设备。".into()) }
        })
    });

    let app_for_snapshot = app.clone();
    let snapshot_desktop_id = desktop_id.clone();
    let snapshot_provider: monitor_server::SnapshotProvider = Arc::new(
        move |device_id, session_id, sequence| {
            let state = app_for_snapshot.state::<AppState>();
            let plaintext = serde_json::to_vec(&monitor_status_payload(
                &app_for_snapshot,
                state.inner(),
            )).map_err(|e| format!("序列化 Monitor Snapshot 失败：{e}"))?;
            monitor_crypto::encrypt_snapshot(
                &master_key,
                &snapshot_desktop_id,
                device_id,
                session_id,
                sequence,
                timestamp_ms(),
                &plaintext,
            )
        },
    );

    let binds = [
        format!("0.0.0.0:{}", settings.monitor_port),
        format!("[::]:{}", settings.monitor_port),
    ];
    let mut errors = Vec::new();
    for bind_text in binds {
        let bind = match bind_text.parse() {
            Ok(bind) => bind,
            Err(error) => {
                errors.push(format!("{bind_text}: {error}"));
                continue;
            }
        };
        match monitor_server::start_monitor_server(
            bind,
            identity.clone(),
            desktop_id.clone(),
            auth_checker.clone(),
            pair_handler.clone(),
            revoke_handler.clone(),
            snapshot_provider.clone(),
        ) {
            Ok(server) => {
                push_log(state, format!("手机 Monitor Server 已启动：WSS {}", server.addr));
                slot.push(server);
            }
            Err(error) => errors.push(error),
        }
    }
    if slot.is_empty() {
        return Err(format!("Monitor Server 无法监听 IPv4/IPv6：{}", errors.join("；")));
    }
    for error in errors {
        push_log(state, format!("Monitor Server 部分监听失败：{error}"));
    }
    Ok(())
}

fn stop_monitor_service(state: &AppState) -> Result<(), String> {
    let servers = {
        let mut slot = state.monitor_servers.lock()
            .map_err(|_| "Monitor Server 状态锁已损坏。".to_string())?;
        std::mem::take(&mut *slot)
    };
    drop(servers);
    push_log(state, "手机 Monitor Server 已停止");
    Ok(())
}

fn clear_runtime_key(app: &tauri::AppHandle) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    clear_keychain_key()?;
    let path = secret_path(app)?;
    if path.exists() { fs::remove_file(path).map_err(|e| format!("清除 Runtime Key 失败：{e}"))?; }
    Ok(())
}

fn key_storage() -> &'static str {
    if cfg!(windows) { "Windows DPAPI" }
    else if cfg!(target_os = "macos") { "macOS Keychain" }
    else { "session only" }
}

fn load_runtime_key(app: &tauri::AppHandle, supplied: &str) -> Result<String, String> {
    if !supplied.trim().is_empty() { return Ok(supplied.trim().to_string()); }
    let path = secret_path(app)?;
    if !runtime_key_saved(app) { return Err("请输入 Runtime API Key，或先保存一个 Runtime Key。".into()); }
    let value = unprotect_secret(&path)?;
    if value.trim().is_empty() { return Err("已保存的 Runtime Key 为空。".into()); }
    Ok(value)
}

fn quote_mcp_path(path: &Path) -> String {
    let value = path.to_string_lossy().replace('\\', "/").replace('"', "\\\"");
    format!("\"{value}\"")
}

fn desktop_commander_home(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(state_dir(app)?.join("desktop-commander-home"))
}

fn mcp_command(paths: &RuntimePaths, dc_home: &Path) -> String {
    format!(
        "{} {} {} {} --no-onboarding",
        quote_mcp_path(&paths.node),
        quote_mcp_path(&paths.launcher),
        quote_mcp_path(dc_home),
        quote_mcp_path(&paths.desktop_commander),
    )
}

fn parse_call_history(text: &str, limit: usize, tool_name: Option<&str>, status: Option<&str>) -> Value {
    let mut records = Vec::new();
    let mut tool_names = BTreeSet::new();
    let mut total = 0usize;
    let mut invalid_entries = 0usize;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let Ok(mut record) = serde_json::from_str::<Value>(line) else {
            invalid_entries += 1;
            continue;
        };
        let Some(name) = record.get("toolName").and_then(Value::as_str).map(str::to_string) else {
            invalid_entries += 1;
            continue;
        };
        total += 1;
        tool_names.insert(name.clone());
        let success = record.get("output").and_then(|output| output.get("isError"))
            .and_then(Value::as_bool) != Some(true);
        if tool_name.is_some_and(|filter| filter != name) { continue; }
        if status == Some("success") && !success { continue; }
        if status == Some("error") && success { continue; }
        if let Value::Object(object) = &mut record {
            object.insert("success".into(), Value::Bool(success));
        }
        records.push(record);
    }
    let filtered_total = records.len();
    let success_count = records.iter().filter(|record| record.get("success").and_then(Value::as_bool) == Some(true)).count();
    let error_count = filtered_total.saturating_sub(success_count);
    let mut durations = records.iter().filter_map(|record| record.get("duration").and_then(Value::as_u64)).collect::<Vec<_>>();
    durations.sort_unstable();
    let average_duration_ms = if durations.is_empty() {
        None
    } else {
        let total_duration = durations.iter().map(|value| *value as f64).sum::<f64>();
        Some((total_duration / durations.len() as f64 * 10.0).round() / 10.0)
    };
    let p95_duration_ms = if durations.is_empty() {
        None
    } else {
        let index = ((durations.len() * 95 + 99) / 100).saturating_sub(1);
        durations.get(index).copied()
    };
    records.reverse();
    records.truncate(limit);
    json!({
        "items": records,
        "total": total,
        "filteredTotal": filtered_total,
        "toolNames": tool_names.into_iter().collect::<Vec<_>>(),
        "invalidEntries": invalid_entries,
        "stats": {
            "successCount": success_count,
            "errorCount": error_count,
            "averageDurationMs": average_duration_ms,
            "p95DurationMs": p95_duration_ms
        }
    })
}

fn runtime_not_running(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    let unknown_alias = text.trim().starts_with("alias ")
        && text.trim().ends_with(" is not known; run create or connect first");
    unknown_alias || text.contains("not found") || text.contains("no runtime") || text.contains("stopped")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn inherited_mcp_home_is_restored_before_keychain_access() {
        const MARKER: &str = "CHATX_HOME_REGRESSION_CHILD";
        if std::env::var_os(MARKER).is_some() {
            restore_account_home().unwrap();
            let home = std::env::var_os("HOME").unwrap();
            assert_ne!(Path::new(&home), std::env::temp_dir());
            assert!(Path::new(&home).is_absolute());
            let _no_ui = security_framework::os::macos::keychain::SecKeychain::disable_user_interaction().unwrap();
            security_framework::os::macos::keychain::SecKeychain::default().unwrap();
            let service = format!("{KEYCHAIN_SERVICE}.home.{}", std::process::id());
            use security_framework::passwords::{set_generic_password, get_generic_password, delete_generic_password};
            set_generic_password(&service, RUNTIME_ALIAS, b"home-regression-test").unwrap();
            let read = get_generic_password(&service, RUNTIME_ALIAS);
            delete_generic_password(&service, RUNTIME_ALIAS).unwrap();
            assert_eq!(read.unwrap(), b"home-regression-test");
            return;
        }
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "tests::inherited_mcp_home_is_restored_before_keychain_access", "--nocapture"])
            .env(MARKER, "1").env("HOME", std::env::temp_dir()).output().unwrap();
        assert!(output.status.success(), "{}", output_text(&output));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn keychain_save_update_read_and_clear() {
        // Test service is separate from the application's real credentials.
        let path = Path::new("unused");
        clear_keychain_key().unwrap();
        assert!(!keychain_key_saved());
        protect_secret("chatx-test-first", path).unwrap();
        assert!(keychain_key_saved());
        assert_eq!(unprotect_secret(path).unwrap(), "chatx-test-first");
        protect_secret("chatx-test-updated", path).unwrap();
        assert_eq!(unprotect_secret(path).unwrap(), "chatx-test-updated");
        clear_keychain_key().unwrap();
        assert!(!keychain_key_saved());
        assert!(unprotect_secret(path).is_err());
        clear_keychain_key().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_dpapi_round_trip() {
        let path = std::env::temp_dir().join(format!("chatx-dpapi-test-{}.txt", std::process::id()));
        let _ = fs::remove_file(&path);
        protect_secret("chatx-dpapi-regression", &path).unwrap();
        assert_eq!(unprotect_secret(&path).unwrap(), "chatx-dpapi-regression");
        fs::remove_file(&path).unwrap();
    }

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("plain path"), "'plain path'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn pmset_disables_sleep_parser_tracks_enabled_value() {
        let current = "System-wide power settings:\n SleepDisabled\t\t1\nCurrently in use:\n sleep                1\n";
        let legacy = "AC Power:\n disablesleep         1\n sleep                0\n";
        let disabled = "System-wide power settings:\nCurrently in use:\n sleep                1\n";
        assert!(pmset_disables_sleep(current));
        assert!(pmset_disables_sleep(legacy));
        assert!(!pmset_disables_sleep(disabled));
    }

    #[test]
    fn first_connection_can_proceed_without_an_existing_alias() {
        assert!(runtime_not_running("alias chatx-local is not known; run create or connect first\n"));
        assert!(runtime_not_running("runtime stopped"));
        assert!(!runtime_not_running("permission denied reading runtime registry"));
        assert!(!runtime_not_running("failed to parse runtime registry"));
    }

    #[test]
    fn runtime_health_requires_ready_when_reported() {
        assert!(runtime_payload_active(&json!({"process_running":true,"healthy":true,"ready":true})));
        assert!(runtime_payload_alive(&json!({"process_running":true,"healthy":true,"ready":false})));
        assert!(!runtime_payload_active(&json!({"process_running":true,"healthy":true,"ready":false})));
        assert!(!runtime_payload_active(&json!({"process_running":true,"healthy":true,"ready":true,"control_plane_poll_health":"unhealthy"})));
        assert!(runtime_payload_active(&json!({"runtime_state":"connected"})));
        assert!(!runtime_payload_active(&json!({"runtime_state":"running"})));
    }

    #[test]
    fn proxy_url_validation_is_explicit_and_secret_free() {
        assert_eq!(
            validate_proxy_url("http://127.0.0.1:7890").unwrap(),
            "http://127.0.0.1:7890"
        );
        assert_eq!(
            proxy_host_port("socks5://127.0.0.1:7891"),
            Some(("127.0.0.1".into(), 7891))
        );
        assert!(validate_proxy_url("http://user:pass@127.0.0.1:7890").is_err());
        assert!(validate_proxy_url("http://127.0.0.1:7890/path").is_err());
    }

    #[test]
    fn control_plane_log_failures_override_ready_health() {
        let path = std::env::temp_dir().join(format!(
            "chatx-control-plane-{}.jsonl",
            std::process::id()
        ));
        let text = concat!(
            r#"{"component":"controlplane","client_instance_id":"test","msg":"starting control-plane poller"}"#, "\n",
            r#"{"component":"controlplane","client_instance_id":"test","msg":"poll failed; backing off","error":"network down"}"#, "\n",
            r#"{"component":"controlplane","client_instance_id":"test","msg":"poll timed out; backing off","error":"timeout"}"#, "\n",
            r#"{"component":"controlplane","client_instance_id":"test","msg":"poll failed; backing off","error":"network down"}"#, "\n"
        );
        fs::write(&path, text).unwrap();
        let payload = json!({
            "ready": true,
            "healthy": true,
            "local": {"log": {"path": path.to_string_lossy()}}
        });
        let observation = control_plane_observation(&payload);
        assert_eq!(observation.state, "down");
        assert_eq!(observation.consecutive_failures, 3);
        assert!(observation.reason.contains("network down"));

        fs::write(
            &path,
            format!(
                "{text}{}\n",
                r#"{"component":"controlplane","client_instance_id":"test","msg":"poller recovered; polling operational"}"#
            ),
        ).unwrap();
        let recovered = control_plane_observation(&payload);
        assert_eq!(recovered.state, "healthy");
        assert_eq!(recovered.consecutive_failures, 0);
        let _ = fs::remove_file(path);
    }

    #[test]
    fn recent_monitor_history_exposes_only_safe_metadata() {
        let text = concat!(
            r#"{"timestamp":"2026-09-23T00:00:00.000Z","toolName":"read_file","arguments":{"path":"/secret/path"},"output":{"content":[{"text":"secret-output"}]},"duration":42}"#,
            "\n",
            r#"{"timestamp":"2026-09-23T00:00:01.000Z","toolName":"write_file","arguments":{"content":"secret-input"},"output":{"isError":true},"duration":84}"#,
            "\n"
        );
        let calls = parse_recent_monitor_history(text, 10);
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["toolName"], "write_file");
        assert_eq!(calls[0]["success"], false);
        let serialized = serde_json::to_string(&calls).unwrap();
        assert!(!serialized.contains("secret-output"));
        assert!(!serialized.contains("secret-input"));
        assert!(!serialized.contains("/secret/path"));
        assert!(!serialized.contains("arguments"));
        assert!(!serialized.contains("output"));
    }

    #[test]
    fn call_history_parser_filters_and_skips_bad_lines() {
        let text = concat!(
            "{\"timestamp\":\"2026-09-18T08:00:00.000Z\",\"toolName\":\"read_file\",\"arguments\":{},\"output\":{},\"duration\":4}\n",
            "not-json\n",
            "{\"timestamp\":\"2026-09-18T08:00:01.000Z\",\"toolName\":\"write_file\",\"arguments\":{},\"output\":{\"isError\":true},\"duration\":7}\n"
        );
        let all = parse_call_history(text, 10, None, None);
        assert_eq!(all["total"], 2);
        assert_eq!(all["invalidEntries"], 1);
        assert_eq!(all["items"][0]["toolName"], "write_file");
        assert_eq!(all["items"][0]["success"], false);
        assert_eq!(all["stats"]["successCount"], 1);
        assert_eq!(all["stats"]["errorCount"], 1);
        assert_eq!(all["stats"]["averageDurationMs"], 5.5);
        assert_eq!(all["stats"]["p95DurationMs"], 7);
        let limited = parse_call_history(text, 1, None, None);
        assert_eq!(limited["items"].as_array().map(Vec::len), Some(1));
        assert_eq!(limited["filteredTotal"], 2);
        assert_eq!(limited["stats"]["successCount"], 1);
        let failed = parse_call_history(text, 10, None, Some("error"));
        assert_eq!(failed["filteredTotal"], 1);
        assert_eq!(failed["items"][0]["toolName"], "write_file");
        let reads = parse_call_history(text, 10, Some("read_file"), Some("success"));
        assert_eq!(reads["filteredTotal"], 1);
        assert_eq!(reads["items"][0]["success"], true);
    }
}

fn stop_runtime(app: &tauri::AppHandle, state: Option<&AppState>) -> Result<Value, String> {
    let paths = runtime_paths(app)?;
    let args = vec!["runtimes".into(), "stop".into(), RUNTIME_ALIAS.into(), "--json".into()];
    let output = run_tunnel(app, &paths, &args, None)?;
    if let Some(state) = state { push_log(state, format!("停止 Tunnel runtime: {}", output_text(&output))); }
    if output.status.success() {
        Ok(parse_json_output(&output).unwrap_or_else(|| json!({"state":"stopped"})))
    } else {
        let text = output_text(&output);
        if runtime_not_running(&text) {
            Ok(json!({"state":"stopped"}))
        } else { Err(format!("停止 Tunnel 失败：{text}")) }
    }
}

fn health_value_unhealthy(value: &Value) -> bool {
    match value {
        Value::Bool(ok) => !ok,
        Value::String(text) => matches!(
            text.to_ascii_lowercase().as_str(),
            "down" | "error" | "failed" | "failure" | "stale" | "unhealthy" | "disconnected"
        ),
        Value::Object(object) => {
            object.get("healthy").and_then(Value::as_bool) == Some(false)
                || object.get("ready").and_then(Value::as_bool) == Some(false)
                || object.get("ok").and_then(Value::as_bool) == Some(false)
                || object.get("state").map(health_value_unhealthy).unwrap_or(false)
                || object.get("status").map(health_value_unhealthy).unwrap_or(false)
        }
        _ => false,
    }
}

fn runtime_payload_alive(payload: &Value) -> bool {
    if payload.get("process_running").and_then(Value::as_bool) == Some(false)
        || payload.get("healthy").and_then(Value::as_bool) == Some(false)
    {
        return false;
    }
    if payload.get("process_running").and_then(Value::as_bool) == Some(true)
        || payload.get("healthy").and_then(Value::as_bool) == Some(true)
    {
        return true;
    }
    matches!(
        payload.get("runtime_state").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase().as_str(),
        "ready" | "running" | "healthy" | "connected" | "live"
    )
}

fn control_plane_health_value(payload: &Value) -> Option<&Value> {
    payload.get("control_plane_poll_health")
        .or_else(|| payload.pointer("/local/control_plane_poll_health"))
}

fn runtime_payload_active(payload: &Value) -> bool {
    if payload.get("process_running").and_then(Value::as_bool) == Some(false) {
        return false;
    }
    if control_plane_health_value(payload).map(health_value_unhealthy).unwrap_or(false) {
        return false;
    }
    let ready = payload.get("ready").and_then(Value::as_bool);
    let healthy = payload.get("healthy").and_then(Value::as_bool);
    if ready == Some(false) || healthy == Some(false) {
        return false;
    }
    if let Some(ready) = ready {
        return ready;
    }
    if healthy == Some(true) {
        return true;
    }
    matches!(
        payload.get("runtime_state").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase().as_str(),
        "ready" | "connected" | "live"
    )
}

#[derive(Debug, Clone)]
struct ControlPlaneObservation {
    state: String,
    reason: String,
    consecutive_failures: u32,
}

impl Default for ControlPlaneObservation {
    fn default() -> Self {
        Self { state: "unknown".into(), reason: String::new(), consecutive_failures: 0 }
    }
}

#[derive(Debug, Clone)]
struct RuntimeObservation {
    state: String,
    active: bool,
    runtime_alive: bool,
    payload: Option<Value>,
    error: String,
    control_plane: ControlPlaneObservation,
    proxy_source: String,
}

fn runtime_proxy_source(payload: &Value) -> String {
    let Some(log_path) = payload.pointer("/local/log/path").and_then(Value::as_str) else {
        return String::new();
    };
    let tail = read_history_tail(Path::new(log_path), 512 * 1024);
    let mut instance_id: Option<String> = None;
    for line in tail.lines().rev() {
        let Ok(event) = serde_json::from_str::<Value>(line) else { continue; };
        if event.get("component").and_then(Value::as_str) != Some("controlplane") {
            continue;
        }
        let current_id = event.get("client_instance_id").and_then(Value::as_str).unwrap_or("");
        if instance_id.is_none() && !current_id.is_empty() {
            instance_id = Some(current_id.to_string());
        }
        if let Some(expected) = instance_id.as_deref() {
            if !current_id.is_empty() && current_id != expected {
                continue;
            }
        }
        if event.get("msg").and_then(Value::as_str) == Some("control-plane route resolved") {
            return event.get("proxy_source")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
        }
    }
    String::new()
}

fn control_plane_observation(payload: &Value) -> ControlPlaneObservation {
    let mut fallback = ControlPlaneObservation::default();
    if let Some(value) = control_plane_health_value(payload) {
        match value {
            Value::String(text) => {
                fallback.state = text.to_ascii_lowercase();
            }
            Value::Bool(ok) => {
                fallback.state = if *ok { "healthy" } else { "down" }.into();
            }
            Value::Object(object) => {
                fallback.state = object.get("state")
                    .and_then(Value::as_str)
                    .or_else(|| object.get("status").and_then(Value::as_str))
                    .unwrap_or("unknown")
                    .to_ascii_lowercase();
                fallback.reason = object.get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .chars().take(300).collect();
            }
            _ => {}
        }
    }

    let Some(log_path) = payload.pointer("/local/log/path").and_then(Value::as_str) else {
        return fallback;
    };
    let tail = read_history_tail(Path::new(log_path), 512 * 1024);
    if tail.is_empty() {
        return fallback;
    }

    let mut instance_id: Option<String> = None;
    let mut failures = 0u32;
    let mut reason = String::new();
    let mut saw_start = false;

    for line in tail.lines().rev() {
        let Ok(event) = serde_json::from_str::<Value>(line) else { continue; };
        if event.get("component").and_then(Value::as_str) != Some("controlplane") {
            continue;
        }
        let current_id = event.get("client_instance_id").and_then(Value::as_str).unwrap_or("");
        if instance_id.is_none() && !current_id.is_empty() {
            instance_id = Some(current_id.to_string());
        }
        if let Some(expected) = instance_id.as_deref() {
            if !current_id.is_empty() && current_id != expected {
                continue;
            }
        }
        let message = event.get("msg").and_then(Value::as_str).unwrap_or("");
        match message {
            "poll failed; backing off" | "poll timed out; backing off" => {
                failures = failures.saturating_add(1);
                if reason.is_empty() {
                    reason = event.get("error").and_then(Value::as_str)
                        .unwrap_or(message)
                        .chars().take(300).collect();
                }
            }
            "poller recovered; polling operational" | "tunnel metadata fetched" => {
                if failures == 0 {
                    return ControlPlaneObservation {
                        state: "healthy".into(),
                        reason: String::new(),
                        consecutive_failures: 0,
                    };
                }
                break;
            }
            "starting control-plane poller" | "poller started" => {
                saw_start = true;
                if failures == 0 { break; }
            }
            _ => {}
        }
    }

    if failures >= 3 {
        return ControlPlaneObservation {
            state: "down".into(),
            reason,
            consecutive_failures: failures,
        };
    }
    if failures > 0 {
        return ControlPlaneObservation {
            state: "suspect".into(),
            reason,
            consecutive_failures: failures,
        };
    }
    if saw_start && fallback.state == "unknown" {
        fallback.state = "starting".into();
    }
    fallback
}

fn runtime_status(app: &tauri::AppHandle, paths: &RuntimePaths) -> RuntimeObservation {
    let args = vec!["runtimes".into(), "status".into(), RUNTIME_ALIAS.into(), "--json".into()];
    match run_tunnel(app, paths, &args, None) {
        Ok(output) if output.status.success() => {
            let Some(parsed) = parse_json_output(&output) else {
                let text = output_text(&output);
                let error = if text.is_empty() {
                    "Tunnel 状态返回为空或不是有效 JSON。".to_string()
                } else {
                    format!("Tunnel 状态返回不是有效 JSON：{text}")
                };
                return RuntimeObservation {
                    state: "error".into(),
                    active: false,
                    runtime_alive: false,
                    payload: None,
                    error,
                    control_plane: ControlPlaneObservation::default(),
                    proxy_source: String::new(),
                };
            };
            let control_plane = control_plane_observation(&parsed);
            let proxy_source = runtime_proxy_source(&parsed);
            let base_active = runtime_payload_active(&parsed);
            let active = base_active && !matches!(control_plane.state.as_str(), "down" | "suspect" | "starting");
            let runtime_alive = runtime_payload_alive(&parsed);
            let state = parsed
                .get("runtime_state")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| if active { "ready".into() } else { "unknown".into() });
            RuntimeObservation {
                state,
                active,
                runtime_alive,
                payload: Some(parsed),
                error: String::new(),
                control_plane,
                proxy_source,
            }
        }
        Ok(output) => {
            let text = output_text(&output);
            if runtime_not_running(&text) {
                RuntimeObservation {
                    state: "stopped".into(),
                    active: false,
                    runtime_alive: false,
                    payload: None,
                    error: String::new(),
                    control_plane: ControlPlaneObservation::default(),
                    proxy_source: String::new(),
                }
            } else {
                RuntimeObservation {
                    state: "error".into(),
                    active: false,
                    runtime_alive: false,
                    payload: None,
                    error: text,
                    control_plane: ControlPlaneObservation::default(),
                    proxy_source: String::new(),
                }
            }
        }
        Err(error) => RuntimeObservation {
            state: "error".into(),
            active: false,
            runtime_alive: false,
            payload: None,
            error,
            control_plane: ControlPlaneObservation::default(),
            proxy_source: String::new(),
        },
    }
}

fn update_runtime_snapshot(
    state: &AppState,
    observation: &RuntimeObservation,
    desired_connected: bool,
    settings: &Settings,
) {
    let now = timestamp_ms();
    if let Ok(mut snapshot) = state.runtime_snapshot.lock() {
        let local_failures = if observation.active || !desired_connected {
            0
        } else {
            snapshot.consecutive_failures.saturating_add(1)
        };
        let failures = if observation.control_plane.consecutive_failures > 0 {
            observation.control_plane.consecutive_failures
        } else {
            local_failures
        };
        snapshot.state = observation.state.clone();
        snapshot.active = observation.active;
        snapshot.last_error = observation.error.clone();
        snapshot.updated_at = now;
        snapshot.last_probe_at = now;
        snapshot.consecutive_failures = failures;
        snapshot.control_plane_state = observation.control_plane.state.clone();
        snapshot.control_plane_reason = observation.control_plane.reason.clone();
        snapshot.control_plane_failures = observation.control_plane.consecutive_failures;
        snapshot.proxy_mode = settings.proxy.mode.clone();
        snapshot.proxy_source = observation.proxy_source.clone();

        if !desired_connected {
            snapshot.health = "stopped".into();
        } else if observation.control_plane.state == "down" {
            snapshot.health = "down".into();
        } else if matches!(observation.control_plane.state.as_str(), "suspect" | "starting") {
            snapshot.health = "suspect".into();
        } else if observation.active {
            snapshot.last_successful_probe_at = now;
            snapshot.health = "healthy".into();
        } else if failures >= if observation.runtime_alive { 8 } else { 2 } {
            snapshot.health = "down".into();
        } else {
            snapshot.health = "suspect".into();
        }
    }
}

fn reconnect_delay_secs(attempt: u32) -> u64 {
    match attempt {
        0 | 1 => 2,
        2 => 5,
        3 => 10,
        4 => 20,
        _ => 30,
    }
}

fn start_runtime_connection(
    app: &tauri::AppHandle,
    state: &AppState,
    tunnel_id: &str,
    key: &str,
    reconnect: bool,
) -> Result<(), String> {
    let _operation = state.runtime_operation.lock().map_err(|_| "Tunnel 操作锁已损坏。".to_string())?;
    if reconnect && (!state.desired_connected.load(Ordering::SeqCst) || !load_settings(app).auto_reconnect) {
        return Ok(());
    }

    let paths = runtime_paths(app)?;
    let profiles = state_dir(app)?.join("tunnel-profiles");
    let dc_home = desktop_commander_home(app)?;
    fs::create_dir_all(&profiles).map_err(|e| format!("创建 Tunnel profile 目录失败：{e}"))?;
    fs::create_dir_all(&dc_home).map_err(|e| format!("创建 Desktop Commander 数据目录失败：{e}"))?;

    match stop_runtime(app, None) {
        Ok(_) => {}
        Err(error) if reconnect => push_log(state, format!("自动重连清理旧 runtime 时继续执行：{error}")),
        Err(error) => return Err(error),
    }

    let args = vec![
        "runtimes".into(), "connect".into(), "--alias".into(), RUNTIME_ALIAS.into(),
        "--tunnel-id".into(), tunnel_id.to_string(),
        "--runtime-api-key".into(), "env:CHATX_TUNNEL_RUNTIME_KEY".into(),
        "--profile-dir".into(), profiles.to_string_lossy().into_owned(),
        "--mcp-command".into(), mcp_command(&paths, &dc_home), "--json".into()
    ];
    push_log(state, if reconnect {
        "正在自动重连 Secure MCP Tunnel → Desktop Commander"
    } else {
        "正在启动 Secure MCP Tunnel → Desktop Commander"
    });
    let output = run_tunnel(app, &paths, &args, Some(key))?;
    let text = output_text(&output);
    push_log(state, format!("Tunnel connect: {text}"));
    if !output.status.success() { return Err(format!("启动 Tunnel 失败：{text}")); }
    Ok(())
}

fn restore_connection_intent_if_active(app: &tauri::AppHandle, state: &AppState) {
    let mut settings = load_settings(app);
    if settings.tunnel_id.trim().is_empty() {
        return;
    }
    let Ok(paths) = runtime_paths(app) else { return; };
    let observation = runtime_status(app, &paths);

    // Upgrade compatibility: older settings did not persist connection intent.
    // If a runtime is already active, preserve the user's existing connection.
    if observation.active && !settings.connection_intent {
        settings.connection_intent = true;
        let _ = save_settings(app, &settings);
    }

    state.desired_connected.store(settings.connection_intent, Ordering::SeqCst);
    update_runtime_snapshot(
        state,
        &observation,
        settings.connection_intent,
        &settings,
    );
    if !settings.connection_intent {
        return;
    }

    if runtime_key_saved(app) {
        if let Ok(key) = load_runtime_key(app, "") {
            if let Ok(mut session_key) = state.session_runtime_key.lock() {
                *session_key = Some(key);
            }
        }
    }
    push_log(
        state,
        if observation.active {
            "检测到现有 Tunnel runtime，已恢复连接意图"
        } else {
            "已恢复持久化 Tunnel 连接意图，等待健康监测/自动重连"
        },
    );
}

fn start_reconnect_monitor(app: tauri::AppHandle) {
    thread::spawn(move || {
        let mut inactive_checks = 0u32;
        loop {
            thread::sleep(Duration::from_secs(4));
            let state = app.state::<AppState>();
            if state.quitting.load(Ordering::SeqCst) { break; }

            let settings = load_settings(&app);
            let desired_connected = state.desired_connected.load(Ordering::SeqCst);
            let paths = match runtime_paths(&app) {
                Ok(paths) => paths,
                Err(error) => {
                    push_log(&state, format!("Tunnel 健康检查失败：{error}"));
                    continue;
                }
            };
            let observation = runtime_status(&app, &paths);
            update_runtime_snapshot(
                state.inner(),
                &observation,
                desired_connected,
                &settings,
            );
            if observation.active {
                if state.reconnecting.swap(false, Ordering::SeqCst) {
                    push_log(&state, "Secure MCP Tunnel 已恢复连接");
                }
                state.reconnect_attempt.store(0, Ordering::SeqCst);
                inactive_checks = 0;
                continue;
            }

            if !desired_connected || !settings.auto_reconnect {
                inactive_checks = 0;
                state.reconnecting.store(false, Ordering::SeqCst);
                state.reconnect_attempt.store(0, Ordering::SeqCst);
                continue;
            }

            inactive_checks = inactive_checks.saturating_add(1);
            let failure_threshold = if observation.control_plane.state == "down" { 1 } else if observation.runtime_alive { 8 } else { 2 };
            if inactive_checks < failure_threshold || state.reconnecting.swap(true, Ordering::SeqCst) {
                continue;
            }

            let attempt = state.reconnect_attempt.fetch_add(1, Ordering::SeqCst) + 1;
            let key = state.session_runtime_key.lock().ok().and_then(|value| value.clone());
            let Some(key) = key else {
                push_log(&state, "Tunnel 已断开，但本次会话没有可用于自动重连的 Runtime Key");
                state.reconnecting.store(false, Ordering::SeqCst);
                thread::sleep(Duration::from_secs(reconnect_delay_secs(attempt)));
                continue;
            };

            let detail = if !observation.error.is_empty() {
                observation.error.clone()
            } else if !observation.control_plane.reason.is_empty() {
                format!("control-plane {}: {}", observation.control_plane.state, observation.control_plane.reason)
            } else {
                observation.state.clone()
            };
            push_log(&state, format!("检测到 Tunnel 掉线（{detail}），开始第 {attempt} 次自动重连"));
            if !state.desired_connected.load(Ordering::SeqCst) || !load_settings(&app).auto_reconnect {
                state.reconnecting.store(false, Ordering::SeqCst);
                continue;
            }

            match start_runtime_connection(&app, state.inner(), &settings.tunnel_id, &key, true) {
                Ok(()) if state.desired_connected.load(Ordering::SeqCst) => {
                    push_log(&state, format!("第 {attempt} 次自动重连命令已完成"));
                    state.reconnect_attempt.store(0, Ordering::SeqCst);
                    inactive_checks = 0;
                }
                Ok(()) => {}
                Err(error) => {
                    let delay = reconnect_delay_secs(attempt);
                    push_log(&state, format!("第 {attempt} 次自动重连失败：{error}；{delay} 秒后继续检查"));
                    thread::sleep(Duration::from_secs(delay));
                }
            }
            state.reconnecting.store(false, Ordering::SeqCst);
        }
    });
}

fn manifest(paths: &RuntimePaths) -> Value {
    fs::read_to_string(paths.root.join("runtime-manifest.json")).ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok()).unwrap_or_else(|| json!({}))
}

#[tauri::command]
fn get_power_settings(app: tauri::AppHandle) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        return Ok(json!({
            "supported": true,
            "clamshellAwake": macos_clamshell_awake_enabled()?,
            "managedByChatX": macos_clamshell_owned_by_current_process(&app)
        }));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Ok(json!({"supported": false, "clamshellAwake": false, "managedByChatX": false}))
    }
}

#[tauri::command]
fn set_clamshell_awake(app: tauri::AppHandle, enabled: bool) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        let before = macos_clamshell_awake_enabled()?;
        let marker = macos_clamshell_marker_path(&app)?;
        if enabled {
            if !before {
                macos_enable_clamshell_with_watchdog(&app)?;
            }
        } else {
            if before {
                run_macos_admin_shell("/usr/bin/pmset -a disablesleep 0")?;
            }
            let _ = fs::remove_file(&marker);
        }
        let actual = macos_clamshell_awake_enabled()?;
        if actual != enabled {
            return Err("macOS 未应用合盖运行设置，请检查系统电源策略。".into());
        }
        return get_power_settings(app);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, enabled);
        Err("合盖后保持运行当前仅支持 macOS。".into())
    }
}

#[cfg(target_os = "macos")]
fn permission_directory_result(id: &str, label: &str, path: &Path) -> Value {
    let (status, detail) = match fs::read_dir(path) {
        Ok(mut entries) => {
            let _ = entries.next();
            ("granted", format!("已允许访问 {}", path.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => {
            ("denied", format!("macOS 拒绝访问 {}", path.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ("unavailable", format!("目录不存在：{}", path.display()))
        }
        Err(error) => ("error", format!("检查 {} 失败：{error}", path.display())),
    };
    json!({"id":id,"label":label,"status":status,"detail":detail})
}

#[cfg(target_os = "macos")]
fn pending_permission_payload(app: &tauri::AppHandle) -> Value {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let key_saved = runtime_key_saved(app);
    json!({
        "supported": true,
        "platform": "macOS",
        "requested": false,
        "items": [
            {"id":"desktop","label":"桌面文件夹","status":"pending","detail":home.join("Desktop").to_string_lossy()},
            {"id":"documents","label":"文稿文件夹","status":"pending","detail":home.join("Documents").to_string_lossy()},
            {"id":"downloads","label":"下载文件夹","status":"pending","detail":home.join("Downloads").to_string_lossy()},
            {"id":"keychain","label":"macOS 钥匙串","status":if key_saved{"pending"}else{"notNeeded"},"detail":if key_saved{"将验证已保存的 Runtime Key 访问权限"}else{"尚未保存 Runtime Key，无需授权"}}
        ],
        "fullDiskAccess": {"status":"manual","detail":"macOS 不允许应用静默授予“完全磁盘访问”，需要在系统设置中手动打开。"}
    })
}

#[tauri::command]
fn get_permission_center(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        if let Ok(cache) = state.permission_results.lock() {
            if let Some(payload) = cache.clone() { return Ok(payload); }
        }
        return Ok(pending_permission_payload(&app));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, state);
        Ok(json!({
            "supported": false,
            "platform": std::env::consts::OS,
            "requested": false,
            "items": [],
            "fullDiskAccess": {"status":"notNeeded","detail":"当前平台不使用 macOS TCC 文件夹授权。"}
        }))
    }
}

#[tauri::command]
fn request_all_permissions(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").map(PathBuf::from)
            .ok_or_else(|| "无法定位当前 macOS 用户主目录。".to_string())?;
        let mut items = vec![
            permission_directory_result("desktop", "桌面文件夹", &home.join("Desktop")),
            permission_directory_result("documents", "文稿文件夹", &home.join("Documents")),
            permission_directory_result("downloads", "下载文件夹", &home.join("Downloads")),
        ];
        let keychain = if runtime_key_saved(&app) {
            match unprotect_secret(&secret_path(&app)?) {
                Ok(_) => json!({"id":"keychain","label":"macOS 钥匙串","status":"granted","detail":"已验证 ChatX 可读取保存的 Runtime Key"}),
                Err(error) => json!({"id":"keychain","label":"macOS 钥匙串","status":"denied","detail":error}),
            }
        } else {
            json!({"id":"keychain","label":"macOS 钥匙串","status":"notNeeded","detail":"尚未保存 Runtime Key，无需授权"})
        };
        items.push(keychain);
        let payload = json!({
            "supported": true,
            "platform": "macOS",
            "requested": true,
            "items": items,
            "fullDiskAccess": {"status":"manual","detail":"常用文件夹权限已集中触发；如需访问其他受保护位置，请在系统设置中为 ChatX 开启“完全磁盘访问”。"}
        });
        if let Ok(mut cache) = state.permission_results.lock() { *cache = Some(payload.clone()); }
        push_log(&state, "权限中心已完成常用 macOS 权限预授权检查");
        return Ok(payload);
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = (app, state);
        Ok(json!({
            "supported": false,
            "platform": std::env::consts::OS,
            "requested": true,
            "items": [],
            "fullDiskAccess": {"status":"notNeeded","detail":"当前平台不需要 macOS 文件夹预授权。"}
        }))
    }
}

#[tauri::command]
fn open_full_disk_access_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return open::that("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
            .map_err(|e| format!("打开 macOS 完全磁盘访问设置失败：{e}"));
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("“完全磁盘访问”仅适用于 macOS。".into())
    }
}

#[tauri::command]
fn get_mcp_live_status(app: tauri::AppHandle) -> Result<Value, String> {
    let now_ms = timestamp_ms();
    let activity = monitor_activity_path(&app).ok()
        .and_then(|path| monitor::read_activity_snapshot(&path));
    let status = monitor::evaluate_activity(activity.as_ref(), now_ms);
    let recent_calls = recent_monitor_calls(&app, activity.as_ref(), now_ms);
    Ok(json!({
        "serverTime": now_ms,
        "status": status,
        "recentCalls": recent_calls
    }))
}

#[tauri::command]
fn get_status(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    let settings = load_settings(&app);
    let secret_saved = runtime_key_saved(&app);
    let now_ms = timestamp_ms();
    let activity = monitor_activity_path(&app).ok()
        .and_then(|path| monitor::read_activity_snapshot(&path));
    let mcp_monitor = monitor::evaluate_activity(activity.as_ref(), now_ms);
    match runtime_paths(&app) {
        Ok(paths) => {
            let dc_home = desktop_commander_home(&app)?;
            let runtime_manifest = manifest(&paths);
            let tunnel_version = runtime_manifest
                .get("tunnelClient")
                .and_then(|value| value.get("version"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let desktop_commander = runtime_manifest
                .get("desktopCommander")
                .cloned()
                .unwrap_or_else(|| json!({"version":"unknown"}));
            let tunnel_snapshot = state.runtime_snapshot.lock()
                .map(|value| value.clone())
                .unwrap_or_default();
            let runtime_state = tunnel_snapshot.state.clone();
            let runtime_active = tunnel_snapshot.active;
            let last_error = tunnel_snapshot.last_error.clone();
            let logs = state.logs.lock().map(|v| v.clone()).unwrap_or_default();
            Ok(json!({
                "service":{"name":"ChatX","version":APP_VERSION},
                "configured":!settings.tunnel_id.is_empty(),
                "tunnelId":settings.tunnel_id,
                "rememberKey":settings.remember_key,
                "autoReconnect":settings.auto_reconnect,
                "desiredConnected":state.desired_connected.load(Ordering::SeqCst),
                "reconnecting":state.reconnecting.load(Ordering::SeqCst),
                "reconnectAttempt":state.reconnect_attempt.load(Ordering::SeqCst),
                "runtimeKeySaved":secret_saved,
                "keyStorage":key_storage(),
                "platform":format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
                "runtimeState":runtime_state,
                "runtimeActive":runtime_active,
                "runtimeHealth":tunnel_snapshot.health,
                "runtime":Value::Null,
                "lastError":last_error,
                "tunnelVersion":tunnel_version,
                "desktopCommander":desktop_commander,
                "mcpCommand":mcp_command(&paths, &dc_home),
                "mcpMonitor":mcp_monitor,
                "logs":logs
            }))
        }
        Err(error) => Ok(json!({
            "service":{"name":"ChatX","version":APP_VERSION},
            "configured":!settings.tunnel_id.is_empty(),
            "tunnelId":settings.tunnel_id,
            "rememberKey":settings.remember_key,
            "autoReconnect":settings.auto_reconnect,
            "desiredConnected":state.desired_connected.load(Ordering::SeqCst),
            "reconnecting":state.reconnecting.load(Ordering::SeqCst),
            "reconnectAttempt":state.reconnect_attempt.load(Ordering::SeqCst),
            "runtimeKeySaved":secret_saved,
            "keyStorage":key_storage(),
            "platform":format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
            "runtimeState":"unavailable",
            "runtimeActive":false,
            "runtimeHealth":"unavailable",
            "lastError":error,
            "mcpMonitor":mcp_monitor,
            "logs":state.logs.lock().map(|v| v.clone()).unwrap_or_default()
        }))
    }
}

#[cfg(target_os = "macos")]
fn relay_account_home() -> Result<PathBuf, String> {
    macos_account_home()
}

#[cfg(target_os = "macos")]
fn relay_setup_token_path(_app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(macos_account_home()?
        .join("Library")
        .join("Application Support")
        .join("com.chatgptx.local")
        .join("signing")
        .join("relay-setup-token"))
}

#[cfg(not(target_os = "macos"))]
fn relay_account_home() -> Result<PathBuf, String> {
    std::env::var_os("USERPROFILE")
        .or_else(|| std::env::var_os("HOME"))
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
        .ok_or_else(|| "无法定位当前用户主目录。".to_string())
}

#[cfg(not(target_os = "macos"))]
fn relay_setup_token_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(state_dir(app)?.join("relay-setup-token"))
}

fn read_relay_setup_token(app: &tauri::AppHandle) -> Result<String, String> {
    if let Ok(value) = std::env::var("CHATX_RELAY_SETUP_TOKEN") {
        let value = value.trim().to_string();
        if !value.is_empty() {
            return Ok(value);
        }
    }

    let path = relay_setup_token_path(app)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let metadata = fs::metadata(&path)
            .map_err(|_| "Relay 注册凭据尚未由管理员预配。".to_string())?;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err("Relay 注册凭据文件权限过宽；请限制为仅当前用户可读。".into());
        }
    }
    let value = fs::read_to_string(&path)
        .map_err(|_| "Relay 注册凭据尚未由管理员预配。".to_string())?;
    let value = value.trim().to_string();
    if value.is_empty() {
        return Err("Relay 注册凭据为空。".into());
    }
    Ok(value)
}

fn relay_credentials_for_settings(
    app: &tauri::AppHandle,
    settings: &Settings,
) -> Option<relay::RelayCredentials> {
    let credentials = read_relay_credentials(app).ok()?;
    let credential_url = relay::normalized_base_url(&credentials.base_url).ok()?;
    let settings_url = relay::normalized_base_url(&settings.relay.base_url).ok()?;
    (credential_url == settings_url).then_some(credentials)
}

fn stop_relay_client(state: &AppState) {
    if let Ok(mut slot) = state.relay_client.lock() {
        let previous = slot.take();
        drop(slot);
        drop(previous);
    }
    if let Ok(mut status) = state.relay_status.lock() {
        status.connected = false;
        status.connecting = false;
        status.session_id = None;
    }
}

fn relay_status_snapshot(
    app: &tauri::AppHandle,
    state: &AppState,
    settings: &Settings,
) -> relay::RelayStatus {
    let mut status = state.relay_status.lock()
        .map(|value| value.clone())
        .unwrap_or_default();
    status.enabled = settings.relay.enabled;
    let credentials = relay_credentials_for_settings(app, settings);
    status.configured = credentials.is_some();
    if status.desktop_id.is_none() {
        if let Some(credentials) = credentials {
            status.desktop_id = Some(credentials.desktop_id);
        }
    }
    status
}

fn start_relay_client_for_settings(
    app: &tauri::AppHandle,
    state: &AppState,
) -> Result<(), String> {
    stop_relay_client(state);
    let settings = load_settings(app);
    if !settings.relay.enabled {
        if let Ok(mut status) = state.relay_status.lock() {
            *status = relay::RelayStatus {
                enabled: false,
                configured: relay_credentials_for_settings(app, &settings).is_some(),
                ..relay::RelayStatus::default()
            };
        }
        return Ok(());
    }

    relay::validate(&settings.relay)?;
    let Some(credentials) = relay_credentials_for_settings(app, &settings) else {
        if let Ok(mut status) = state.relay_status.lock() {
            *status = relay::RelayStatus {
                enabled: true,
                configured: false,
                ..relay::RelayStatus::default()
            };
        }
        return Ok(());
    };
    let master_key = ensure_monitor_master_key(app)?;
    let app_for_snapshot = app.clone();
    let snapshot_provider: relay::SnapshotProvider = Arc::new(move || {
        let state = app_for_snapshot.state::<AppState>();
        let devices = monitor_devices_snapshot(&app_for_snapshot, state.inner());
        if devices.is_empty() {
            return Vec::new();
        }
        let payload = match serde_json::to_vec(&monitor_status_payload(
            &app_for_snapshot,
            state.inner(),
        )) {
            Ok(value) => value,
            Err(_) => return Vec::new(),
        };
        devices.into_iter().map(|device| relay::SnapshotSource {
            device_id: device.id,
            plaintext: payload.clone(),
        }).collect()
    });

    let app_for_pair = app.clone();
    let pairing_handler: relay::PairingHandler = Arc::new(move |message| {
        let state = app_for_pair.state::<AppState>();
        handle_encrypted_monitor_pairing(&app_for_pair, state.inner(), message)
    });

    let app_for_revoke = app.clone();
    let revoke_handler: relay::RevokeHandler = Arc::new(move |device_id| {
        let state = app_for_revoke.state::<AppState>();
        revoke_monitor_device_local(
            &app_for_revoke,
            state.inner(),
            device_id,
        ).map(|_| ())
    });

    let handle = relay::start_client(
        settings.relay.clone(),
        credentials,
        master_key,
        state.relay_status.clone(),
        snapshot_provider,
        pairing_handler,
        revoke_handler,
    )?;
    let mut slot = state.relay_client.lock()
        .map_err(|_| "Relay Client 状态锁已损坏。".to_string())?;
    *slot = Some(handle);
    Ok(())
}

fn network_settings_from_payload(
    current: &Settings,
    payload: &Value,
) -> Result<Settings, String> {
    let mut next = current.clone();
    let proxy = payload.get("proxy").unwrap_or(&Value::Null);
    if let Some(mode) = proxy.get("mode").and_then(Value::as_str) {
        next.proxy.mode = mode.trim().to_ascii_lowercase();
    }
    if let Some(url) = proxy.get("url").and_then(Value::as_str) {
        next.proxy.url = url.trim().to_string();
    }
    if !matches!(next.proxy.mode.as_str(), "direct" | "system" | "manual") {
        return Err("Tunnel 代理模式仅支持 direct、system 或 manual。".into());
    }
    match next.proxy.mode.as_str() {
        "direct" => next.proxy.applied_url.clear(),
        "manual" => {
            next.proxy.url = validate_proxy_url(&next.proxy.url)?;
            next.proxy.applied_url = next.proxy.url.clone();
        }
        "system" => {
            next.proxy.applied_url = system_proxy_url()?.unwrap_or_default();
        }
        _ => unreachable!("proxy mode validated above"),
    }

    let relay_payload = payload.get("relay").unwrap_or(&Value::Null);
    if let Some(enabled) = relay_payload.get("enabled").and_then(Value::as_bool) {
        next.relay.enabled = enabled;
    }
    if let Some(base_url) = relay_payload.get("baseUrl").and_then(Value::as_str) {
        next.relay.base_url = base_url.trim().to_string();
    }
    relay::validate(&next.relay)?;
    if next.relay.enabled && !next.monitor_enabled {
        return Err("启用 ChatX Relay 前请先开启手机监控。".into());
    }
    Ok(next)
}

fn network_settings_payload(app: &tauri::AppHandle, state: &AppState) -> Value {
    let settings = load_settings(app);
    let system_proxy = system_proxy_url();
    let effective_proxy = effective_proxy_url(&settings);
    let relay_status = relay_status_snapshot(app, state, &settings);
    let relay_registered = relay_credentials_for_settings(app, &settings).is_some();
    let relay_setup_provisioned = read_relay_setup_token(app).is_ok();
    let tunnel = state.runtime_snapshot.lock()
        .map(|snapshot| snapshot.clone())
        .unwrap_or_default();

    json!({
        "proxy": {
            "mode": settings.proxy.mode,
            "url": settings.proxy.url,
            "systemUrl": system_proxy.as_ref().ok().and_then(|value| value.clone()),
            "systemError": system_proxy.err(),
            "effectiveUrl": effective_proxy.as_ref().ok().and_then(|value| value.clone()),
            "effectiveError": effective_proxy.err()
        },
        "relay": {
            "enabled": settings.relay.enabled,
            "baseUrl": settings.relay.base_url,
            "registered": relay_registered,
            "credentialSaved": relay_registered,
            "setupProvisioned": relay_setup_provisioned,
            "status": relay_status
        },
        "tunnel": {
            "health": tunnel.health,
            "state": tunnel.state,
            "controlPlaneState": tunnel.control_plane_state,
            "controlPlaneReason": tunnel.control_plane_reason,
            "controlPlaneFailures": tunnel.control_plane_failures,
            "proxyMode": tunnel.proxy_mode,
            "proxySource": tunnel.proxy_source
        }
    })
}

#[tauri::command]
fn get_network_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    Ok(network_settings_payload(&app, state.inner()))
}

#[tauri::command]
fn register_relay_desktop(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    let settings = load_settings(&app);
    if settings.relay.base_url.trim().is_empty() {
        return Err("请先配置 Relay URL。".into());
    }
    relay::normalized_base_url(&settings.relay.base_url)?;
    let setup_token = read_relay_setup_token(&app)?;
    let desktop_id = ensure_monitor_desktop_id(&app)?;
    let credentials = relay::register_desktop(
        &settings.relay,
        &setup_token,
        &desktop_id,
        &host_device_name(),
        APP_VERSION,
        &format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
    )?;
    save_relay_credentials(&app, &credentials)?;
    if settings.relay.enabled {
        start_relay_client_for_settings(&app, state.inner())?;
    }
    push_log(
        state.inner(),
        format!("ChatX Relay Desktop 注册完成：{}", settings.relay.base_url),
    );
    Ok(network_settings_payload(&app, state.inner()))
}

#[tauri::command]
fn test_network_settings(
    app: tauri::AppHandle,
    payload: Value,
) -> Result<Value, String> {
    let current = load_settings(&app);
    let candidate = network_settings_from_payload(&current, &payload)?;
    let effective = effective_proxy_url(&candidate)?;
    let proxy_check = if let Some(url) = effective.as_ref() {
        let (host, port) = proxy_host_port(url)
            .ok_or_else(|| "无法解析代理 host:port。".to_string())?;
        json!({
            "mode": candidate.proxy.mode,
            "target": format!("{host}:{port}"),
            "ok": tcp_probe(&host, port)
        })
    } else {
        json!({
            "mode": candidate.proxy.mode,
            "target": "api.openai.com:443",
            "ok": tcp_probe("api.openai.com", 443)
        })
    };

    let relay_check = if candidate.relay.base_url.trim().is_empty() {
        json!({"ok": false, "target": Value::Null, "error": "尚未配置 Relay URL"})
    } else {
        match relay::health(&candidate.relay) {
            Ok(()) => json!({
                "ok": true,
                "target": candidate.relay.base_url,
                "error": Value::Null
            }),
            Err(error) => json!({
                "ok": false,
                "target": candidate.relay.base_url,
                "error": error
            })
        }
    };
    Ok(json!({
        "proxy": proxy_check,
        "relay": relay_check
    }))
}

#[tauri::command]
fn set_network_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    payload: Value,
) -> Result<Value, String> {
    let previous = load_settings(&app);
    let next = network_settings_from_payload(&previous, &payload)?;
    let wants_connection = state.desired_connected.load(Ordering::SeqCst);
    let proxy_changed = previous.proxy != next.proxy
        || (wants_connection && next.proxy.mode == "system");
    let relay_changed = previous.relay != next.relay;

    let reconnect_key = if proxy_changed && wants_connection {
        state.session_runtime_key.lock().ok().and_then(|value| value.clone())
            .or_else(|| load_runtime_key(&app, "").ok())
            .ok_or_else(|| "更改 Tunnel 代理需要重新连接，但当前没有可用 Runtime Key。".to_string())?
            .into()
    } else {
        None
    };

    save_settings(&app, &next)?;

    if proxy_changed && wants_connection {
        let key = reconnect_key.as_deref().unwrap_or("");
        if let Err(error) = start_runtime_connection(
            &app,
            state.inner(),
            &next.tunnel_id,
            key,
            false,
        ) {
            let _ = save_settings(&app, &previous);
            let _ = start_runtime_connection(
                &app,
                state.inner(),
                &previous.tunnel_id,
                key,
                false,
            );
            let _ = start_relay_client_for_settings(&app, state.inner());
            return Err(format!("应用 Tunnel 代理失败，已回滚原配置：{error}"));
        }
        if let Ok(paths) = runtime_paths(&app) {
            let observation = runtime_status(&app, &paths);
            update_runtime_snapshot(state.inner(), &observation, true, &next);
        }
    }

    if relay_changed {
        if let Err(error) = start_relay_client_for_settings(&app, state.inner()) {
            let _ = save_settings(&app, &previous);
            let _ = start_relay_client_for_settings(&app, state.inner());
            return Err(format!("应用 ChatX Relay 失败，已回滚原配置：{error}"));
        }
    }

    push_log(
        state.inner(),
        format!(
            "网络设置已更新：proxy={} relay={}",
            next.proxy.mode,
            if next.relay.enabled { "wss" } else { "off" }
        ),
    );
    Ok(network_settings_payload(&app, state.inner()))
}

#[tauri::command]
fn get_monitor_info(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    Ok(monitor_info_payload(&app, state.inner()))
}

#[tauri::command]
fn set_monitor_enabled(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    enabled: bool,
    port: Option<u16>,
) -> Result<Value, String> {
    let previous = load_settings(&app);
    let mut settings = previous.clone();
    if let Some(port) = port {
        if port < 1024 {
            return Err("Monitor 端口必须为 1024-65535。".into());
        }
        settings.monitor_port = port;
    }
    settings.monitor_enabled = enabled;
    if !enabled {
        settings.relay.enabled = false;
    }
    let restart = previous.monitor_enabled && enabled && previous.monitor_port != settings.monitor_port;
    if restart || !enabled {
        stop_monitor_service(state.inner())?;
    }
    save_settings(&app, &settings)?;
    if enabled {
        if let Err(error) = start_monitor_service(&app, state.inner()) {
            let _ = save_settings(&app, &previous);
            if previous.monitor_enabled {
                let _ = start_monitor_service(&app, state.inner());
            }
            return Err(error);
        }
    }

    if !enabled {
        stop_relay_client(state.inner());
    }
    Ok(monitor_info_payload(&app, state.inner()))
}

#[tauri::command]
fn create_monitor_pairing(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    let settings = load_settings(&app);
    if !settings.monitor_enabled {
        return Err("请先开启手机连接。".into());
    }
    start_monitor_service(&app, state.inner())?;

    let (supports_ipv4, supports_ipv6, fingerprint_sha256) = {
        let servers = state.monitor_servers.lock()
            .map_err(|_| "Monitor Server 状态锁已损坏。".to_string())?;
        if servers.is_empty() {
            return Err("Monitor WSS Server 未运行。".into());
        }
        (
            servers.iter().any(|server| server.addr.is_ipv4()),
            servers.iter().any(|server| server.addr.is_ipv6()),
            servers[0].fingerprint_sha256.clone(),
        )
    };

    let endpoints = monitor_network::discover_monitor_endpoints(settings.monitor_port)
        .into_iter()
        .filter(|endpoint| {
            (endpoint.family == "ipv4" && supports_ipv4)
                || (endpoint.family == "ipv6" && supports_ipv6)
        })
        .take(8)
        .collect::<Vec<_>>();
    let direct_candidates = endpoints.into_iter().map(|endpoint| {
        let pair_url = endpoint.url.replace("/v1/ws/monitor", "/v1/ws/pair");
        json!({
            "kind": endpoint.kind,
            "family": endpoint.family,
            "interface": endpoint.interface,
            "host": endpoint.host,
            "url": pair_url
        })
    }).collect::<Vec<_>>();

    let desktop_id = ensure_monitor_desktop_id(&app)?;
    let pairing_code = monitor_server::generate_monitor_token()?;
    let pairing_random = monitor_server::generate_monitor_token()?;
    let pairing_id = format!("p_{}", &pairing_random[..24]);
    let expires_at = timestamp_ms().saturating_add(5 * 60_000);

    let mut relay_pairing = None;
    let mut relay_error = None;
    if settings.relay.enabled {
        match read_relay_credentials(&app) {
            Ok(credentials)
                if credentials.desktop_id == desktop_id
                    && relay::normalized_base_url(&credentials.base_url).ok()
                        == relay::normalized_base_url(&settings.relay.base_url).ok() =>
            {
                let route_token = monitor_server::generate_monitor_token()?;
                match relay::open_pairing_route(
                    &settings.relay,
                    &credentials,
                    &pairing_id,
                    &route_token,
                    expires_at,
                ) {
                    Ok(()) => {
                        relay_pairing = Some(json!({
                            "baseUrl": settings.relay.base_url,
                            "url": relay::pairing_ws_url(
                                &settings.relay,
                                &credentials,
                                &pairing_id,
                            )?,
                            "desktopId": desktop_id,
                            "pairingId": pairing_id,
                            "routeToken": route_token
                        }));
                    }
                    Err(error) => relay_error = Some(error),
                }
            }
            Ok(_) => relay_error = Some(
                "Relay Credential 与当前 Desktop 身份或 Relay URL 不匹配。".into()
            ),
            Err(error) => relay_error = Some(error),
        }
    }

    if direct_candidates.is_empty() && relay_pairing.is_none() {
        return Err(relay_error.unwrap_or_else(|| {
            "没有发现可用的 Direct WSS 路径，ChatX Relay 也不可用。".into()
        }));
    }

    {
        let mut grant = state.monitor_pairing.lock()
            .map_err(|_| "Monitor 配对状态锁已损坏。".to_string())?;
        *grant = Some(MonitorPairingGrant {
            pairing_id: pairing_id.clone(),
            pairing_code: pairing_code.clone(),
            code_hash: monitor_server::token_hash_hex(&pairing_code),
            expires_at,
        });
    }

    let pairing = json!({
        "schemaVersion": 3,
        "protocol": "chatx-monitor-wss-v1",
        "scheme": "wss",
        "desktopId": desktop_id,
        "port": settings.monitor_port,
        "directCandidates": direct_candidates,
        "relayPairing": relay_pairing,
        "relayPairingError": relay_error,
        "fingerprintSha256": fingerprint_sha256,
        "pairingId": pairing_id,
        "pairingCode": pairing_code,
        "expiresAt": expires_at
    });
    let pairing_text = serde_json::to_string(&pairing)
        .map_err(|e| format!("生成配对 JSON 失败：{e}"))?;
    let qr = QrCode::new(pairing_text.as_bytes())
        .map_err(|e| format!("生成配对二维码失败：{e}"))?;
    let qr_svg = qr.render::<svg::Color>()
        .min_dimensions(280, 280)
        .dark_color(svg::Color("#111827"))
        .light_color(svg::Color("#ffffff"))
        .build();
    let mut result = pairing;
    result.as_object_mut()
        .ok_or_else(|| "生成配对资料失败。".to_string())?
        .insert("qrSvg".into(), Value::String(qr_svg));
    Ok(result)
}

#[tauri::command]
fn revoke_monitor_device(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    device_id: String,
) -> Result<Value, String> {
    let device_id = device_id.trim();
    if device_id.is_empty() {
        return Err("设备 ID 不能为空。".into());
    }

    let settings = load_settings(&app);
    let relay_credentials = read_relay_credentials(&app).ok();
    if !settings.relay.base_url.trim().is_empty() {
        if let Some(credentials) = relay_credentials.as_ref() {
            if relay::normalized_base_url(&credentials.base_url)?
                == relay::normalized_base_url(&settings.relay.base_url)?
            {
                relay::revoke_device(&settings.relay, credentials, device_id)
                    .map_err(|error| {
                        format!(
                            "Relay 端设备撤销失败，本地凭据保持有效；请确认 Relay 可达后重试：{error}"
                        )
                    })?;
            }
        }
    }

    let mut registry = state.monitor_devices.lock()
        .map_err(|_| "Monitor 设备状态锁已损坏。".to_string())?;
    if registry.is_none() {
        *registry = Some(load_monitor_devices_from_disk(&app));
    }
    let devices = registry.as_mut()
        .ok_or_else(|| "Monitor 设备状态不可用。".to_string())?;
    let previous = devices.clone();
    devices.retain(|device| device.id != device_id);
    if devices.len() == previous.len() {
        return Err("没有找到该已配对设备。".into());
    }
    if let Err(error) = save_monitor_devices(&app, devices) {
        if let (Some(credentials), Ok(master_key)) = (
            relay_credentials.as_ref(),
            ensure_monitor_master_key(&app),
        ) {
            if let Ok(relay_token) =
                monitor_crypto::derive_relay_token(&master_key, device_id)
            {
                let _ = relay::authorize_device(
                    &settings.relay,
                    credentials,
                    device_id,
                    &relay_token,
                );
            }
        }
        *devices = previous;
        return Err(error);
    }
    drop(registry);
    push_log(state.inner(), format!("已撤销手机 Monitor 设备：{device_id}"));
    Ok(monitor_info_payload(&app, state.inner()))
}

#[tauri::command]
fn set_auto_reconnect(app: tauri::AppHandle, state: State<'_, AppState>, enabled: bool) -> Result<Value, String> {
    let mut settings = load_settings(&app);
    settings.auto_reconnect = enabled;
    save_settings(&app, &settings)?;
    if !enabled {
        state.reconnecting.store(false, Ordering::SeqCst);
        state.reconnect_attempt.store(0, Ordering::SeqCst);
    }
    push_log(&state, if enabled { "已开启 Tunnel 断线自动重连" } else { "已关闭 Tunnel 断线自动重连" });
    get_status(app, state)
}

#[tauri::command]
fn get_call_history(app: tauri::AppHandle, limit: Option<usize>, tool_name: Option<String>, status: Option<String>) -> Result<Value, String> {
    let limit = limit.unwrap_or(100).clamp(1, 1000);
    let tool_name = tool_name.as_deref().map(str::trim).filter(|value| !value.is_empty());
    let status = status.as_deref().map(str::trim).filter(|value| !value.is_empty());
    if !matches!(status, None | Some("success") | Some("error")) {
        return Err("调用记录状态筛选仅支持 success 或 error。".into());
    }
    let path = call_history_path(&app)?;
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => return Err(format!("读取调用记录失败：{error}")),
    };
    Ok(parse_call_history(&text, limit, tool_name, status))
}

#[tauri::command]
fn clear_call_history(app: tauri::AppHandle) -> Result<Value, String> {
    let path = call_history_path(&app)?;
    if path.exists() {
        fs::write(&path, "").map_err(|e| format!("清空调用记录失败：{e}"))?;
    }
    Ok(json!({"cleared":true}))
}

fn cancel_connection_intent(state: &AppState) {
    state.desired_connected.store(false, Ordering::SeqCst);
    state.reconnecting.store(false, Ordering::SeqCst);
    state.reconnect_attempt.store(0, Ordering::SeqCst);
    if let Ok(mut key) = state.session_runtime_key.lock() { *key = None; }
}

fn stop_requested(app: &tauri::AppHandle, state: &AppState) -> Result<(), String> {
    let mut settings = load_settings(app);
    settings.connection_intent = false;
    save_settings(app, &settings)?;
    cancel_connection_intent(state);
    let _operation = state.runtime_operation.lock().map_err(|_| "Tunnel 操作锁已损坏。".to_string())?;
    stop_runtime(app, Some(state))?;
    let stopped = RuntimeObservation {
        state: "stopped".into(),
        active: false,
        runtime_alive: false,
        payload: None,
        error: String::new(),
        control_plane: ControlPlaneObservation::default(),
        proxy_source: String::new(),
    };
    update_runtime_snapshot(state, &stopped, false, &settings);
    Ok(())
}

#[tauri::command]
fn connect_tunnel(app: tauri::AppHandle, state: State<'_, AppState>, tunnel_id: String, runtime_key: String, remember_key: bool) -> Result<Value, String> {
    let tunnel_id = tunnel_id.trim().to_string();
    if !tunnel_id.starts_with("tunnel_") { return Err("Tunnel ID 应以 tunnel_ 开头。".into()); }
    let key = load_runtime_key(&app, &runtime_key)?;
    let secret = secret_path(&app)?;
    if remember_key {
        protect_secret(&key, &secret)?;
    } else {
        clear_runtime_key(&app)?;
    }
    if let Ok(mut cache) = state.permission_results.lock() { *cache = None; }
    let mut settings = load_settings(&app);
    settings.tunnel_id = tunnel_id.clone();
    settings.remember_key = remember_key;
    settings.connection_intent = false;
    save_settings(&app, &settings)?;

    state.desired_connected.store(false, Ordering::SeqCst);
    state.reconnecting.store(false, Ordering::SeqCst);
    state.reconnect_attempt.store(0, Ordering::SeqCst);
    if let Ok(mut session_key) = state.session_runtime_key.lock() {
        *session_key = Some(key.clone());
    }

    if let Err(error) = start_runtime_connection(&app, state.inner(), &tunnel_id, &key, false) {
        cancel_connection_intent(state.inner());
        return Err(error);
    }
    state.desired_connected.store(true, Ordering::SeqCst);
    let mut settings = load_settings(&app);
    settings.connection_intent = true;
    if let Err(error) = save_settings(&app, &settings) {
        cancel_connection_intent(state.inner());
        let _ = stop_runtime(&app, Some(state.inner()));
        return Err(format!("Tunnel 已启动但无法保存连接意图，已回滚连接：{error}"));
    }
    if let Ok(paths) = runtime_paths(&app) {
        let observation = runtime_status(&app, &paths);
        let settings = load_settings(&app);
        update_runtime_snapshot(
            state.inner(),
            &observation,
            true,
            &settings,
        );
    }
    get_status(app, state)
}

#[tauri::command]
fn stop_tunnel(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    stop_requested(&app, state.inner())?;
    get_status(app, state)
}

#[tauri::command]
fn clear_saved_key(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    clear_runtime_key(&app)?;
    if let Ok(mut cache) = state.permission_results.lock() { *cache = None; }
    let mut settings = load_settings(&app);
    settings.remember_key = false;
    save_settings(&app, &settings)?;
    push_log(&state, "已清除保存的 Runtime Key");
    get_status(app, state)
}

#[tauri::command]
fn run_diagnostics(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    let mut checks = Vec::new();
    match runtime_paths(&app) {
        Ok(paths) => {
            let dc_home = desktop_commander_home(&app)?;
            checks.push(json!({"name":"tunnel-client","ok":true,"detail":executable_version(&paths.tunnel)}));
            checks.push(json!({"name":"Node.js","ok":paths.node.is_file(),"detail":paths.node.to_string_lossy()}));
            checks.push(json!({"name":"Desktop Commander","ok":paths.desktop_commander.is_file(),"detail":paths.desktop_commander.to_string_lossy()}));
            checks.push(json!({"name":"Desktop Commander isolation","ok":paths.launcher.is_file(),"detail":dc_home.to_string_lossy()}));
            checks.push(json!({"name":"MCP command","ok":true,"detail":mcp_command(&paths, &dc_home)}));
            let observation = runtime_status(&app, &paths);
            let runtime_detail = if observation.error.is_empty() {
                observation.payload.clone().map(|value| value.to_string()).unwrap_or_else(|| observation.state.clone())
            } else {
                observation.error.clone()
            };
            checks.push(json!({"name":"Tunnel runtime","ok":observation.active,"detail":runtime_detail}));
            checks.push(json!({
                "name":"Control plane",
                "ok":observation.control_plane.state == "healthy",
                "detail":format!(
                    "state={} failures={} {}",
                    observation.control_plane.state,
                    observation.control_plane.consecutive_failures,
                    observation.control_plane.reason
                ).trim().to_string()
            }));
        }
        Err(error) => checks.push(json!({"name":"Bundled runtime","ok":false,"detail":error}))
    }
    let snapshot = state.runtime_snapshot.lock().map(|value| value.clone()).unwrap_or_default();
    let probe_age = timestamp_ms().saturating_sub(snapshot.last_probe_at);
    checks.push(json!({"name":"Tunnel probe freshness","ok":snapshot.last_probe_at > 0 && probe_age <= 12_000,"detail":format!("{} ms ago; health={}; failures={}", probe_age, snapshot.health, snapshot.consecutive_failures)}));
    let settings = load_settings(&app);
    let saved = runtime_key_saved(&app);
    checks.push(json!({"name":"Tunnel ID","ok":settings.tunnel_id.starts_with("tunnel_"),"detail":settings.tunnel_id}));
    checks.push(json!({"name":"Runtime Key","ok":saved,"detail":if saved{format!("{} saved", key_storage())}else{"not saved; enter it when connecting".into()}}));
    Ok(json!({"checks":checks}))
}

#[tauri::command]
fn open_logs(app: tauri::AppHandle) -> Result<(), String> {
    open::that(state_dir(&app)?).map_err(|e| format!("打开数据目录失败：{e}"))
}

#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    const ALLOWED_PREFIXES: &[&str] = &[
        "https://platform.openai.com/", "https://developers.openai.com/", "https://chatgpt.com/",
        "https://github.com/openai/tunnel-client", "https://github.com/wonderwhy-er/DesktopCommanderMCP"
    ];
    if !ALLOWED_PREFIXES.iter().any(|prefix| url.starts_with(prefix)) { return Err("不允许打开未列入白名单的外部地址。".into()); }
    open::that(url).map_err(|e| format!("打开浏览器失败：{e}"))
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show(); let _ = window.unminimize(); let _ = window.set_focus();
    }
}

fn stop_on_exit(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    state.quitting.store(true, Ordering::SeqCst);
    stop_relay_client(state.inner());
    let _ = stop_monitor_service(state.inner());
    // Exiting ChatX stops the local runtime but preserves the persisted
    // connection intent. Only an explicit Stop action clears that intent.
    cancel_connection_intent(state.inner());
    if let Ok(_operation) = state.runtime_operation.lock() {
        let _ = stop_runtime(app, Some(state.inner()));
    };
}

fn main() {
    #[cfg(target_os = "macos")]
    if let Err(error) = restore_account_home() {
        eprintln!("ChatX 启动失败：{error}");
        std::process::exit(1);
    }
    rustls::crypto::ring::default_provider()
        .install_default()
        .expect("failed to install rustls ring crypto provider");
    let app = tauri::Builder::default()
        .manage(AppState::default())
        .setup(|app| {
            let show_item = MenuItem::with_id(app, "show", "打开 ChatX", true, None::<&str>)?;
            let stop_item = MenuItem::with_id(app, "stop", "停止连接", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "退出 ChatX", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_item, &stop_item, &quit_item])?;
            let mut tray = TrayIconBuilder::with_id("chatx-tray")
                .tooltip("ChatX").menu(&menu).show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => show_main_window(app),
                    "stop" => { let state = app.state::<AppState>(); let _ = stop_requested(app, state.inner()); }
                    "quit" => { stop_on_exit(app); app.exit(0); }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click { button:MouseButton::Left, button_state:MouseButtonState::Up, .. } = event {
                        show_main_window(tray.app_handle());
                    }
                });
            if let Some(icon) = app.default_window_icon() { tray = tray.icon(icon.clone()); }
            tray.build(app)?;
            let handle = app.handle().clone();
            let state = app.state::<AppState>();
            restore_connection_intent_if_active(&handle, state.inner());
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    let quitting = window.app_handle().state::<AppState>().quitting.load(Ordering::SeqCst);
                    if !quitting { api.prevent_close(); let _ = window.hide(); }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status, get_mcp_live_status, set_auto_reconnect,
            get_network_settings, set_network_settings, test_network_settings,
            register_relay_desktop,
            get_monitor_info, set_monitor_enabled, create_monitor_pairing,
            revoke_monitor_device,
            get_permission_center, request_all_permissions, open_full_disk_access_settings,
            get_power_settings, set_clamshell_awake,
            get_call_history, clear_call_history,
            connect_tunnel, stop_tunnel, clear_saved_key,
            run_diagnostics, open_logs, open_external
        ])
        .build(tauri::generate_context!()).expect("failed to build ChatX desktop application");
    {
        let handle = app.handle().clone();
        let settings = load_settings(&handle);
        let state = handle.state::<AppState>();
        if settings.monitor_enabled {
            match start_monitor_service(&handle, state.inner()) {
                Ok(()) => {
                    if let Ok(dir) = state_dir(&handle) {
                        let _ = fs::remove_file(dir.join("monitor-startup-error.log"));
                    }
                }
                Err(error) => {
                    let message = format!("手机 Monitor Server 启动失败：{error}");
                    push_log(state.inner(), &message);
                    eprintln!("{message}");
                    if let Ok(dir) = state_dir(&handle) {
                        let _ = fs::write(
                            dir.join("monitor-startup-error.log"),
                            format!("[{}] {message}\n", timestamp()),
                        );
                    }
                }
            }
        }
        if let (Ok(home), Ok(dir)) = (relay_account_home(), state_dir(&handle)) {
            relay::cleanup_legacy_ssh(&home, &dir);
        }
        if settings.relay.enabled && settings.monitor_enabled {
            if let Err(error) = start_relay_client_for_settings(&handle, state.inner()) {
                push_log(state.inner(), format!("ChatX Relay 启动失败：{error}"));
            }
        }
    }
    start_reconnect_monitor(app.handle().clone());
    app.run(|app_handle, event| { if matches!(event, tauri::RunEvent::Exit) { stop_on_exit(app_handle); } });
}
