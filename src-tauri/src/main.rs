#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{atomic::{AtomicBool, AtomicU32, Ordering}, Mutex},
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

#[cfg(all(target_os = "macos", test))]
const KEYCHAIN_SERVICE: &str = "com.chatgptx.local.runtime-key.tests";

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

// A GUI launched from an MCP shell can inherit Desktop Commander's isolated
// HOME. Restore the account home before Foundation, Tauri or Security cache it.
// Do not change the system keychain or its search list.
#[cfg(target_os = "macos")]
fn restore_account_home() -> Result<(), String> {
    use std::{ffi::{CStr, OsStr}, os::unix::ffi::OsStrExt};
    let mut buffer = vec![0u8; 16384];
    loop {
        let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
        let mut result = std::ptr::null_mut();
        // getpwuid_r stores strings in buffer; copy pw_dir before it is dropped.
        let status = unsafe {
            libc::getpwuid_r(libc::getuid(), entry.as_mut_ptr(), buffer.as_mut_ptr().cast(), buffer.len(), &mut result)
        };
        if status == libc::ERANGE && buffer.len() < 1024 * 1024 {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if status != 0 || result.is_null() {
            return Err(format!("无法读取 macOS 用户主目录（错误码 {status}）。"));
        }
        let entry = unsafe { entry.assume_init() };
        if entry.pw_dir.is_null() { return Err("macOS 用户主目录为空。".into()); }
        let home = OsStr::from_bytes(unsafe { CStr::from_ptr(entry.pw_dir) }.to_bytes());
        if !Path::new(home).is_absolute() || !Path::new(home).is_dir() {
            return Err("macOS 用户主目录无效。".into());
        }
        // Called only at process entry, before any application threads start.
        std::env::set_var("HOME", home);
        return Ok(());
    }
}

fn default_auto_reconnect() -> bool { true }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    tunnel_id: String,
    remember_key: bool,
    #[serde(default = "default_auto_reconnect")]
    auto_reconnect: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self { tunnel_id: String::new(), remember_key: false, auto_reconnect: true }
    }
}

#[derive(Default)]
struct AppState {
    logs: Mutex<Vec<String>>,
    quitting: AtomicBool,
    desired_connected: AtomicBool,
    reconnecting: AtomicBool,
    reconnect_attempt: AtomicU32,
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

fn push_log(state: &AppState, message: impl Into<String>) {
    let Ok(mut logs) = state.logs.lock() else { return; };
    logs.push(format!("[{}] {}", timestamp(), message.into()));
    if logs.len() > 300 {
        let drain = logs.len() - 300;
        logs.drain(0..drain);
    }
}

fn state_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    let dir = app.path().app_local_data_dir()
        .map_err(|e| format!("无法定位 ChatX 数据目录：{e}"))?.join("state");
    fs::create_dir_all(&dir).map_err(|e| format!("无法创建 ChatX 数据目录：{e}"))?;
    Ok(dir)
}

fn settings_path(app: &tauri::AppHandle) -> Result<PathBuf, String> { Ok(state_dir(app)?.join("settings.json")) }
fn secret_path(app: &tauri::AppHandle) -> Result<PathBuf, String> { Ok(state_dir(app)?.join("runtime-key.dpapi")) }
fn call_history_path(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(desktop_commander_home(app)?.join(".claude-server-commander").join("tool-history.jsonl"))
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

    if let Ok(settings) = serde_json::from_str::<Settings>(&text) {
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
    let migrated = Settings { tunnel_id: tunnel_id.to_string(), remember_key, auto_reconnect: true };
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

fn run_tunnel(paths: &RuntimePaths, args: &[String], runtime_key: Option<&str>) -> Result<Output, String> {
    let mut command = Command::new(&paths.tunnel);
    command.args(args);
    if let Some(key) = runtime_key { command.env("CHATX_TUNNEL_RUNTIME_KEY", key); }
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
    let script = r#"$b=[Text.Encoding]::UTF8.GetBytes($env:CHATX_SECRET);$p=[Security.Cryptography.ProtectedData]::Protect($b,$null,[Security.Cryptography.DataProtectionScope]::CurrentUser);[IO.File]::WriteAllText($env:CHATX_SECRET_FILE,[Convert]::ToBase64String($p))"#;
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
    let script = r#"$s=[IO.File]::ReadAllText($env:CHATX_SECRET_FILE);$p=[Convert]::FromBase64String($s);$b=[Security.Cryptography.ProtectedData]::Unprotect($p,$null,[Security.Cryptography.DataProtectionScope]::CurrentUser);[Console]::Out.Write([Text.Encoding]::UTF8.GetString($b))"#;
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
    let output = run_tunnel(&paths, &args, None)?;
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

fn runtime_payload_active(payload: &Value) -> bool {
    if payload.get("ready").and_then(Value::as_bool) == Some(true)
        || payload.get("healthy").and_then(Value::as_bool) == Some(true)
    {
        return true;
    }
    matches!(
        payload.get("runtime_state").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase().as_str(),
        "ready" | "running" | "healthy" | "connected" | "live"
    )
}

fn runtime_status(paths: &RuntimePaths) -> (String, bool, Option<Value>, String) {
    let args = vec!["runtimes".into(), "status".into(), RUNTIME_ALIAS.into(), "--json".into()];
    match run_tunnel(paths, &args, None) {
        Ok(output) if output.status.success() => {
            let Some(parsed) = parse_json_output(&output) else {
                let text = output_text(&output);
                let error = if text.is_empty() {
                    "Tunnel 状态返回为空或不是有效 JSON。".to_string()
                } else {
                    format!("Tunnel 状态返回不是有效 JSON：{text}")
                };
                return ("error".into(), false, None, error);
            };
            let active = runtime_payload_active(&parsed);
            let state = parsed
                .get("runtime_state")
                .and_then(Value::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| if active { "ready".into() } else { "unknown".into() });
            (state, active, Some(parsed), String::new())
        }
        Ok(output) => {
            let text = output_text(&output);
            if runtime_not_running(&text) {
                ("stopped".into(), false, None, String::new())
            } else {
                ("error".into(), false, None, text)
            }
        }
        Err(error) => ("error".into(), false, None, error),
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
    let output = run_tunnel(&paths, &args, Some(key))?;
    let text = output_text(&output);
    push_log(state, format!("Tunnel connect: {text}"));
    if !output.status.success() { return Err(format!("启动 Tunnel 失败：{text}")); }
    Ok(())
}

fn start_reconnect_monitor(app: tauri::AppHandle) {
    thread::spawn(move || {
        let mut inactive_checks = 0u32;
        loop {
            thread::sleep(Duration::from_secs(4));
            let state = app.state::<AppState>();
            if state.quitting.load(Ordering::SeqCst) { break; }

            let settings = load_settings(&app);
            if !settings.auto_reconnect || !state.desired_connected.load(Ordering::SeqCst) {
                inactive_checks = 0;
                state.reconnecting.store(false, Ordering::SeqCst);
                continue;
            }

            let paths = match runtime_paths(&app) {
                Ok(paths) => paths,
                Err(error) => {
                    push_log(&state, format!("自动重连检查失败：{error}"));
                    continue;
                }
            };
            let (runtime_state, active, _, status_error) = runtime_status(&paths);
            if active {
                if state.reconnecting.swap(false, Ordering::SeqCst) {
                    push_log(&state, "Secure MCP Tunnel 已恢复连接");
                }
                state.reconnect_attempt.store(0, Ordering::SeqCst);
                inactive_checks = 0;
                continue;
            }

            inactive_checks = inactive_checks.saturating_add(1);
            if inactive_checks < 2 || state.reconnecting.swap(true, Ordering::SeqCst) {
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

            let detail = if status_error.is_empty() { runtime_state } else { status_error };
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
fn get_status(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    let settings = load_settings(&app);
    let secret_saved = runtime_key_saved(&app);
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
            let (runtime_state, runtime_active, runtime, last_error) = runtime_status(&paths);
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
                "runtime":runtime,
                "lastError":last_error,
                "tunnelVersion":tunnel_version,
                "desktopCommander":desktop_commander,
                "mcpCommand":mcp_command(&paths, &dc_home),
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
            "lastError":error,
            "logs":state.logs.lock().map(|v| v.clone()).unwrap_or_default()
        }))
    }
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
    cancel_connection_intent(state);
    let _operation = state.runtime_operation.lock().map_err(|_| "Tunnel 操作锁已损坏。".to_string())?;
    stop_runtime(app, Some(state))?;
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
fn run_diagnostics(app: tauri::AppHandle) -> Result<Value, String> {
    let mut checks = Vec::new();
    match runtime_paths(&app) {
        Ok(paths) => {
            let dc_home = desktop_commander_home(&app)?;
            checks.push(json!({"name":"tunnel-client","ok":true,"detail":executable_version(&paths.tunnel)}));
            checks.push(json!({"name":"Node.js","ok":paths.node.is_file(),"detail":paths.node.to_string_lossy()}));
            checks.push(json!({"name":"Desktop Commander","ok":paths.desktop_commander.is_file(),"detail":paths.desktop_commander.to_string_lossy()}));
            checks.push(json!({"name":"Desktop Commander isolation","ok":paths.launcher.is_file(),"detail":dc_home.to_string_lossy()}));
            checks.push(json!({"name":"MCP command","ok":true,"detail":mcp_command(&paths, &dc_home)}));
            let (runtime_state, runtime_active, runtime, error) = runtime_status(&paths);
            checks.push(json!({"name":"Tunnel runtime","ok":runtime_active,"detail":if error.is_empty(){runtime.map(|v|v.to_string()).unwrap_or(runtime_state)}else{error}}));
        }
        Err(error) => checks.push(json!({"name":"Bundled runtime","ok":false,"detail":error}))
    }
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
    let _ = stop_requested(app, state.inner());
}

fn main() {
    #[cfg(target_os = "macos")]
    if let Err(error) = restore_account_home() {
        eprintln!("ChatX 启动失败：{error}");
        std::process::exit(1);
    }
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
            get_status, set_auto_reconnect,
            get_permission_center, request_all_permissions, open_full_disk_access_settings,
            get_power_settings, set_clamshell_awake,
            get_call_history, clear_call_history,
            connect_tunnel, stop_tunnel, clear_saved_key,
            run_diagnostics, open_logs, open_external
        ])
        .build(tauri::generate_context!()).expect("failed to build ChatX desktop application");
    start_reconnect_monitor(app.handle().clone());
    app.run(|app_handle, event| { if matches!(event, tauri::RunEvent::Exit) { stop_on_exit(app_handle); } });
}
