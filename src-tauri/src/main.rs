#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

mod app_state;
mod call_history;
mod connection_commands;
mod connection_intent;
mod diagnostics_commands;
mod https_mcp;
mod https_mcp_wss;
mod mcp_sources;
mod monitor;
mod monitor_commands;
mod monitor_control;
mod monitor_manager;
mod monitor_status;
mod monitor_crypto;
mod monitor_network;
mod monitor_server;
mod network_commands;
mod platform_commands;
mod process_utils;
mod relay;
mod relay_deploy;
mod secrets;
mod settings;
mod tunnel_recovery;
mod tunnel_runtime;
mod tunnel_status;
#[cfg(windows)]
mod windows_process_job;

use app_state::{AppState, RuntimePaths, TunnelRecoveryStatus};
use call_history::{clear_call_history, get_call_history};
use connection_commands::{
    cancel_connection_intent, clear_saved_key, connect_tunnel, get_status,
    set_auto_reconnect, stop_all_connections, stop_tunnel,
};
use diagnostics_commands::{
    get_mcp_live_status, open_external, open_logs, run_diagnostics,
};
use https_mcp::{
    get_https_mcp_authorizations, get_https_mcp_login_password, get_https_mcp_status,
    revoke_all_https_mcp_authorizations, revoke_https_mcp_authorization,
    rotate_https_mcp_login_password, set_https_mcp_authorization_lifetime,
    set_https_mcp_auto_reconnect,
    start_https_mcp, stop_https_mcp,
};
use monitor_commands::{
    create_monitor_pairing, get_monitor_info, revoke_monitor_device,
    set_monitor_enabled,
};
use network_commands::{
    get_network_settings, register_relay_desktop, relay_account_home,
    relay_credentials_for_settings, set_network_settings,
    start_relay_client_for_settings, stop_relay_client,
    test_network_settings,
};
use platform_commands::{
    get_permission_center, get_power_settings, open_full_disk_access_settings,
    request_all_permissions, set_clamshell_awake,
};
use process_utils::{command_output, command_output_with_timeout};
use relay_deploy::manage_relay_server;
use secrets::{ensure_monitor_master_key, load_runtime_key, read_relay_credentials, runtime_key_saved};
use settings::{load_settings, Settings};
use tunnel_recovery::{reconnect_capability, recovery_status, set_recovery_status};
use tunnel_runtime::{
    restore_persisted_connection_state, start_reconnect_monitor, start_runtime_connection,
    stop_runtime, wait_for_runtime_recovery,
};
use tunnel_status::{runtime_status, RuntimeObservation};
#[cfg(test)]
use tunnel_recovery::{ensure_runtime_start_intent, reconnect_delay_secs};
#[cfg(test)]
use tunnel_status::{runtime_log_observation, runtime_payload_active, runtime_payload_alive};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output},
    sync::{atomic::Ordering, Mutex, OnceLock},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Emitter, Manager, WindowEvent,
};

const RUNTIME_ALIAS: &str = "chatx-local";
const MAIN_WINDOW_RECLAIM_DELAY: Duration = Duration::from_secs(30);
#[cfg(all(target_os = "macos", not(test)))]
const KEYCHAIN_SERVICE: &str = "com.chatgptx.local.runtime-key";
#[cfg(all(target_os = "macos", not(test)))]
const MONITOR_MASTER_KEYCHAIN_SERVICE: &str = "com.chatgptx.local.monitor-master-key";
#[cfg(all(target_os = "macos", not(test)))]
const RELAY_KEYCHAIN_SERVICE: &str = "com.chatgptx.local.relay-credentials";
#[cfg(all(target_os = "macos", not(test)))]
const HTTPS_OAUTH_KEYCHAIN_SERVICE: &str = "com.chatgptx.local.https-oauth";

#[cfg(all(target_os = "macos", test))]
const KEYCHAIN_SERVICE: &str = "com.chatgptx.local.runtime-key.tests";
#[cfg(all(target_os = "macos", test))]
const MONITOR_MASTER_KEYCHAIN_SERVICE: &str = "com.chatgptx.local.monitor-master-key.tests";
#[cfg(all(target_os = "macos", test))]
const RELAY_KEYCHAIN_SERVICE: &str = "com.chatgptx.local.relay-credentials.tests";
#[cfg(all(target_os = "macos", test))]
const HTTPS_OAUTH_KEYCHAIN_SERVICE: &str = "com.chatgptx.local.https-oauth.tests";

#[cfg(target_os = "macos")]
const MONITOR_MASTER_KEYCHAIN_ACCOUNT: &str = "master";
#[cfg(target_os = "macos")]
const RELAY_KEYCHAIN_ACCOUNT: &str = "desktop";
#[cfg(target_os = "macos")]
const HTTPS_OAUTH_KEYCHAIN_ACCOUNT: &str = "master";

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

#[cfg(target_os = "macos")]
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
    static DEVICE_NAME: OnceLock<String> = OnceLock::new();
    DEVICE_NAME.get_or_init(|| {
        let command = Command::new("hostname");
        command_text(command)
            .filter(|value| !value.is_empty())
            .unwrap_or_else(|| "Computer".into())
    }).clone()
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

fn cached_battery_snapshot() -> Value {
    const BATTERY_CACHE_TTL: Duration = Duration::from_secs(45);
    static BATTERY_CACHE: OnceLock<Mutex<Option<(Instant, Value)>>> = OnceLock::new();

    let cache = BATTERY_CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(mut cached) = cache.lock() {
        if let Some((refreshed_at, value)) = cached.as_ref() {
            if refreshed_at.elapsed() < BATTERY_CACHE_TTL {
                return value.clone();
            }
        }
        let value = battery_snapshot();
        *cached = Some((Instant::now(), value.clone()));
        return value;
    }
    battery_snapshot()
}

fn host_snapshot() -> Value {
    json!({
        "deviceName": host_device_name(),
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "battery": cached_battery_snapshot()
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

fn monitor_identity_path(app: &tauri::AppHandle) -> Result<PathBuf, String> { Ok(state_dir(app)?.join("monitor-identity.json")) }
fn monitor_devices_path(app: &tauri::AppHandle) -> Result<PathBuf, String> { Ok(state_dir(app)?.join("monitor-devices.json")) }
fn secret_path(app: &tauri::AppHandle) -> Result<PathBuf, String> { Ok(state_dir(app)?.join("runtime-key.dpapi")) }
#[cfg(windows)]
fn monitor_master_secret_path(app: &tauri::AppHandle) -> Result<PathBuf, String> { Ok(state_dir(app)?.join("monitor-master-key.dpapi")) }
#[cfg(windows)]
fn relay_secret_path(app: &tauri::AppHandle) -> Result<PathBuf, String> { Ok(state_dir(app)?.join("relay-credentials.dpapi")) }


fn runtime_binary_name(base: &str) -> String {
    if cfg!(windows) { format!("{base}.exe") } else { base.to_string() }
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

fn tunnel_command_timeout(args: &[String]) -> Duration {
    match args.get(1).map(String::as_str) {
        Some("status") => Duration::from_secs(10),
        Some("stop") => Duration::from_secs(10),
        Some("connect") => Duration::from_secs(30),
        _ => Duration::from_secs(20),
    }
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
    let label = if args.is_empty() {
        "tunnel-client".to_string()
    } else {
        format!("tunnel-client {}", args.iter().take(2).cloned().collect::<Vec<_>>().join(" "))
    };
    command_output_with_timeout(
        &mut command,
        tunnel_command_timeout(args),
        &label,
    )
}

fn output_text(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stdout.is_empty() { stderr } else if stderr.is_empty() { stdout } else { format!("{stdout}\n{stderr}") }
}

fn parse_json_output(output: &Output) -> Option<Value> { serde_json::from_slice::<Value>(&output.stdout).ok() }

fn executable_version(path: &Path) -> String {
    let mut command = Command::new(path);
    command.arg("--version");
    match command_output(&mut command) { Ok(output) => output_text(&output), Err(error) => error }
}

#[derive(Debug, Clone)]
struct RuntimeKeyRemoteProbe {
    ok: bool,
    state: &'static str,
    detail: String,
}

fn runtime_key_remote_status(status: u16) -> RuntimeKeyRemoteProbe {
    match status {
        200..=299 => RuntimeKeyRemoteProbe {
            ok: true,
            state: "valid",
            detail: format!("OpenAI Control Plane accepted the Runtime Key (HTTP {status})."),
        },
        401 => RuntimeKeyRemoteProbe {
            ok: false,
            state: "invalid",
            detail: "OpenAI rejected the Runtime Key (HTTP 401). The key may have been deleted or revoked.".into(),
        },
        403 => RuntimeKeyRemoteProbe {
            ok: false,
            state: "forbidden",
            detail: "OpenAI authenticated the request but denied access to this Tunnel (HTTP 403).".into(),
        },
        404 => RuntimeKeyRemoteProbe {
            ok: false,
            state: "tunnel_not_found",
            detail: "The configured Tunnel was not found or is not visible to this Runtime Key (HTTP 404).".into(),
        },
        429 => RuntimeKeyRemoteProbe {
            ok: false,
            state: "rate_limited",
            detail: "OpenAI rate-limited the credential probe (HTTP 429); Runtime Key validity is unverified.".into(),
        },
        _ => RuntimeKeyRemoteProbe {
            ok: false,
            state: "unexpected_response",
            detail: format!("OpenAI returned HTTP {status}; Runtime Key validity is unverified."),
        },
    }
}

fn runtime_key_probe_client(proxy_url: Option<&str>) -> Result<reqwest::blocking::Client, String> {
    let mut builder = reqwest::blocking::Client::builder()
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(8))
        .no_proxy();
    if let Some(proxy_url) = proxy_url {
        let proxy = reqwest::Proxy::all(proxy_url)
            .map_err(|error| format!("Runtime Key probe proxy is invalid: {error}"))?;
        builder = builder.proxy(proxy);
    }
    builder
        .build()
        .map_err(|error| format!("Could not create the OpenAI credential probe client: {error}"))
}

fn probe_runtime_key_remote(
    tunnel_id: &str,
    runtime_key: &str,
    proxy_url: Option<&str>,
) -> RuntimeKeyRemoteProbe {
    if !tunnel_id.starts_with("tunnel_") {
        return RuntimeKeyRemoteProbe {
            ok: false,
            state: "not_configured",
            detail: "Tunnel ID is not configured, so the Runtime Key cannot be verified remotely.".into(),
        };
    }
    if runtime_key.trim().is_empty() {
        return RuntimeKeyRemoteProbe {
            ok: false,
            state: "not_available",
            detail: "No Runtime Key is available for remote verification.".into(),
        };
    }
    let client = match runtime_key_probe_client(proxy_url) {
        Ok(client) => client,
        Err(error) => return RuntimeKeyRemoteProbe {
            ok: false,
            state: "client_error",
            detail: error,
        },
    };
    let url = format!("https://api.openai.com/v1/tunnels/{tunnel_id}");
    match client
        .get(url)
        .bearer_auth(runtime_key)
        .header(reqwest::header::USER_AGENT, format!("ChatX/{APP_VERSION}"))
        .send()
    {
        Ok(response) => runtime_key_remote_status(response.status().as_u16()),
        Err(error) => RuntimeKeyRemoteProbe {
            ok: false,
            state: "network_error",
            detail: format!("Could not reach OpenAI Control Plane to verify the Runtime Key: {error}"),
        },
    }
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

    #[test]
    fn explicit_stop_blocks_any_start_that_requires_connection_intent() {
        let state = AppState::default();
        let mut settings = Settings {
            connection_intent: true,
            ..Settings::default()
        };

        state.desired_connected.store(false, Ordering::SeqCst);
        assert!(ensure_runtime_start_intent(&state, &settings, true).is_err());

        state.desired_connected.store(true, Ordering::SeqCst);
        settings.connection_intent = false;
        assert!(ensure_runtime_start_intent(&state, &settings, true).is_err());

        settings.connection_intent = true;
        assert!(ensure_runtime_start_intent(&state, &settings, true).is_ok());
        assert!(ensure_runtime_start_intent(&state, &Settings::default(), false).is_ok());
    }

    #[test]
    fn tunnel_recovery_policy_keeps_bounded_backoff_and_command_timeouts() {
        assert_eq!(reconnect_delay_secs(1), 2);
        assert_eq!(reconnect_delay_secs(2), 5);
        assert_eq!(reconnect_delay_secs(3), 10);
        assert_eq!(reconnect_delay_secs(4), 20);
        assert_eq!(reconnect_delay_secs(5), 30);
        assert_eq!(reconnect_delay_secs(20), 30);

        let args = |operation: &str| vec!["runtimes".to_string(), operation.to_string()];
        assert_eq!(tunnel_command_timeout(&args("status")), Duration::from_secs(10));
        assert_eq!(tunnel_command_timeout(&args("stop")), Duration::from_secs(10));
        assert_eq!(tunnel_command_timeout(&args("connect")), Duration::from_secs(30));
    }

    #[test]
    fn runtime_key_remote_status_distinguishes_auth_failures() {
        assert!(runtime_key_remote_status(200).ok);
        assert_eq!(runtime_key_remote_status(401).state, "invalid");
        assert_eq!(runtime_key_remote_status(403).state, "forbidden");
        assert_eq!(runtime_key_remote_status(404).state, "tunnel_not_found");
        assert_eq!(runtime_key_remote_status(429).state, "rate_limited");
        assert_eq!(runtime_key_remote_status(500).state, "unexpected_response");
    }

    #[test]
    fn runtime_key_probe_client_accepts_supported_proxy_schemes() {
        assert!(runtime_key_probe_client(None).is_ok());
        assert!(runtime_key_probe_client(Some("http://127.0.0.1:7890")).is_ok());
        assert!(runtime_key_probe_client(Some("socks5://127.0.0.1:7891")).is_ok());
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
            r#"{"component":"controlplane","client_instance_id":"test","msg":"control-plane route resolved","proxy_source":"https_proxy"}"#, "\n",
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
        let observation = runtime_log_observation(&payload);
        assert_eq!(observation.proxy_source, "https_proxy");
        assert_eq!(observation.control_plane.state, "down");
        assert_eq!(observation.control_plane.consecutive_failures, 3);
        assert!(observation.control_plane.reason.contains("network down"));

        fs::write(
            &path,
            format!(
                "{text}{}\n",
                r#"{"component":"controlplane","client_instance_id":"test","msg":"poller recovered; polling operational"}"#
            ),
        ).unwrap();
        let recovered = runtime_log_observation(&payload).control_plane;
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
        let calls = monitor_status::parse_recent_history(text, 10);
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
    fn last_window_exit_is_prevented_while_tray_mode_is_active() {
        assert!(should_prevent_exit(false, None));
        assert!(!should_prevent_exit(true, None));
        assert!(!should_prevent_exit(false, Some(0)));
    }

}

fn should_prevent_exit(quitting: bool, code: Option<i32>) -> bool {
    !quitting && code.is_none()
}

fn create_main_window(app: &tauri::AppHandle) -> Result<tauri::WebviewWindow, String> {
    let config = app
        .config()
        .app
        .windows
        .iter()
        .find(|config| config.label == "main")
        .cloned()
        .ok_or_else(|| "找不到 main 窗口配置。".to_string())?;
    tauri::WebviewWindowBuilder::from_config(app, &config)
        .and_then(|builder| builder.build())
        .map_err(|error| format!("重建 ChatX 主窗口失败：{error}"))
}

fn show_main_window(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    state.main_window_generation.fetch_add(1, Ordering::SeqCst);
    let window = app
        .get_webview_window("main")
        .or_else(|| create_main_window(app).map_err(|error| eprintln!("{error}")).ok());
    if let Some(window) = window {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn schedule_main_window_reclaim(app: tauri::AppHandle) {
    let generation = app
        .state::<AppState>()
        .main_window_generation
        .fetch_add(1, Ordering::SeqCst)
        .saturating_add(1);
    std::thread::spawn(move || {
        std::thread::sleep(MAIN_WINDOW_RECLAIM_DELAY);
        let ui_app = app.clone();
        let _ = app.run_on_main_thread(move || {
            let state = ui_app.state::<AppState>();
            if state.quitting.load(Ordering::SeqCst)
                || state.main_window_generation.load(Ordering::SeqCst) != generation
            {
                return;
            }
            if let Some(window) = ui_app.get_webview_window("main") {
                if window.is_visible().ok() == Some(false) {
                    let _ = window.destroy();
                }
            }
        });
    });
}

fn stop_on_exit(app: &tauri::AppHandle) {
    let state = app.state::<AppState>();
    state.quitting.store(true, Ordering::SeqCst);
    let _connections = state.connection_operation.lock();
    stop_relay_client(state.inner());
    https_mcp::stop_on_exit(state.inner());
    let _ = monitor_manager::stop_monitor_service(state.inner());
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
        // Register first: the second invocation activates the existing window
        // instead of creating a second set of MCP/SSH processes.
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| {
            let handle = app.clone();
            let _ = app.run_on_main_thread(move || show_main_window(&handle));
        }))
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
                    "stop" => {
                        let state = app.state::<AppState>();
                        if let Err(error) = connection_commands::stop_all_requested(app, state.inner()) {
                            let message = format!("停止连接未能完全完成：{error}");
                            push_log(state.inner(), &message);
                            if let Ok(mut pending) = state.tray_stop_error.lock() {
                                *pending = Some(message.clone());
                            }
                            eprintln!("{message}");
                            show_main_window(app);
                            let _ = app.emit("chatx-stop-error", message);
                        }
                    }
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
            restore_persisted_connection_state(&handle, state.inner());
            https_mcp::restore(&handle, state.inner());
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let WindowEvent::CloseRequested { api, .. } = event {
                    let app = window.app_handle();
                    let quitting = app.state::<AppState>().quitting.load(Ordering::SeqCst);
                    if !quitting {
                        api.prevent_close();
                        let _ = window.hide();
                        schedule_main_window_reclaim(app.clone());
                    }
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            get_status, get_mcp_live_status, set_auto_reconnect,
            get_https_mcp_status, start_https_mcp, stop_https_mcp, get_https_mcp_login_password,
            get_https_mcp_authorizations, revoke_https_mcp_authorization,
            revoke_all_https_mcp_authorizations, rotate_https_mcp_login_password,
            set_https_mcp_authorization_lifetime, set_https_mcp_auto_reconnect,
            get_network_settings, set_network_settings, test_network_settings,
            register_relay_desktop, manage_relay_server,
            get_monitor_info, set_monitor_enabled, create_monitor_pairing,
            revoke_monitor_device,
            get_permission_center, request_all_permissions, open_full_disk_access_settings,
            get_power_settings, set_clamshell_awake,
            get_call_history, clear_call_history,
            connect_tunnel, stop_tunnel, stop_all_connections, clear_saved_key,
            run_diagnostics, open_logs, open_external
        ])
        .build(tauri::generate_context!()).expect("failed to build ChatX desktop application");
    {
        let handle = app.handle().clone();
        let settings = load_settings(&handle);
        let state = handle.state::<AppState>();
        if settings.monitor_enabled {
            match monitor_manager::start_monitor_service(&handle, state.inner()) {
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
    app.run(|app_handle, event| match event {
        tauri::RunEvent::ExitRequested { code, api, .. } => {
            let quitting = app_handle
                .state::<AppState>()
                .quitting
                .load(Ordering::SeqCst);
            if should_prevent_exit(quitting, code) {
                api.prevent_exit();
            }
        }
        tauri::RunEvent::Exit => stop_on_exit(app_handle),
        _ => {}
    });
}
