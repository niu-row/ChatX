#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Output, Stdio},
    sync::{atomic::{AtomicBool, Ordering}, Mutex},
    time::{SystemTime, UNIX_EPOCH},
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
const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Settings {
    tunnel_id: String,
    remember_key: bool,
}

#[derive(Default)]
struct AppState {
    logs: Mutex<Vec<String>>,
    quitting: AtomicBool,
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
    let remember_key = secret_path(app).map(|secret| secret.is_file()).unwrap_or(false);
    let migrated = Settings { tunnel_id: tunnel_id.to_string(), remember_key };
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
        let tunnel = root.join("tunnel-client.exe");
        let node = root.join("node.exe");
        let launcher = root.join("desktop-commander-launcher.mjs");
        let desktop_commander = root.join("desktop-commander").join("node_modules")
            .join("@wonderwhy-er").join("desktop-commander").join("dist").join("index.js");
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

#[cfg(not(windows))]
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

#[cfg(not(windows))]
fn unprotect_secret(_target: &Path) -> Result<String, String> { Err("安全读取 Runtime Key 当前仅支持 Windows。".into()) }

fn load_runtime_key(app: &tauri::AppHandle, supplied: &str) -> Result<String, String> {
    if !supplied.trim().is_empty() { return Ok(supplied.trim().to_string()); }
    let path = secret_path(app)?;
    if !path.is_file() { return Err("请输入 Runtime API Key，或先保存一个 Runtime Key。".into()); }
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

fn runtime_not_running(text: &str) -> bool {
    let text = text.to_ascii_lowercase();
    text.contains("not found") || text.contains("no runtime") || text.contains("stopped")
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
            let parsed = parse_json_output(&output);
            let state = parsed.as_ref().and_then(|v| v.get("runtime_state")).and_then(Value::as_str).unwrap_or("unknown").to_string();
            let active = parsed.as_ref().map(runtime_payload_active).unwrap_or(false);
            (state, active, parsed, String::new())
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

fn manifest(paths: &RuntimePaths) -> Value {
    fs::read_to_string(paths.root.join("runtime-manifest.json")).ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok()).unwrap_or_else(|| json!({}))
}

#[tauri::command]
fn get_status(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    let settings = load_settings(&app);
    let secret_saved = secret_path(&app)?.is_file();
    match runtime_paths(&app) {
        Ok(paths) => {
            let dc_home = desktop_commander_home(&app)?;
            let (runtime_state, runtime_active, runtime, last_error) = runtime_status(&paths);
            let logs = state.logs.lock().map(|v| v.clone()).unwrap_or_default();
            Ok(json!({
                "service":{"name":"ChatX","version":APP_VERSION},
                "configured":!settings.tunnel_id.is_empty(),
                "tunnelId":settings.tunnel_id,
                "rememberKey":settings.remember_key,
                "runtimeKeySaved":secret_saved,
                "runtimeState":runtime_state,
                "runtimeActive":runtime_active,
                "runtime":runtime,
                "lastError":last_error,
                "tunnelVersion":executable_version(&paths.tunnel),
                "desktopCommander":manifest(&paths).get("desktopCommander").cloned().unwrap_or_else(|| json!({"version":"unknown"})),
                "mcpCommand":mcp_command(&paths, &dc_home),
                "logs":logs
            }))
        }
        Err(error) => Ok(json!({
            "service":{"name":"ChatX","version":APP_VERSION},
            "configured":!settings.tunnel_id.is_empty(),
            "tunnelId":settings.tunnel_id,
            "rememberKey":settings.remember_key,
            "runtimeKeySaved":secret_saved,
            "runtimeState":"unavailable",
            "runtimeActive":false,
            "lastError":error,
            "logs":state.logs.lock().map(|v| v.clone()).unwrap_or_default()
        }))
    }
}

#[tauri::command]
fn connect_tunnel(app: tauri::AppHandle, state: State<'_, AppState>, tunnel_id: String, runtime_key: String, remember_key: bool) -> Result<Value, String> {
    let tunnel_id = tunnel_id.trim().to_string();
    if !tunnel_id.starts_with("tunnel_") { return Err("Tunnel ID 应以 tunnel_ 开头。".into()); }
    let key = load_runtime_key(&app, &runtime_key)?;
    let secret = secret_path(&app)?;
    if remember_key {
        protect_secret(&key, &secret)?;
    } else if secret.exists() {
        fs::remove_file(&secret).map_err(|e| format!("清除旧 Runtime Key 失败：{e}"))?;
    }
    save_settings(&app, &Settings { tunnel_id: tunnel_id.clone(), remember_key })?;
    let paths = runtime_paths(&app)?;
    let profiles = state_dir(&app)?.join("tunnel-profiles");
    let dc_home = desktop_commander_home(&app)?;
    fs::create_dir_all(&profiles).map_err(|e| format!("创建 Tunnel profile 目录失败：{e}"))?;
    fs::create_dir_all(&dc_home).map_err(|e| format!("创建 Desktop Commander 数据目录失败：{e}"))?;
    stop_runtime(&app, None)?;
    let args = vec![
        "runtimes".into(), "connect".into(), "--alias".into(), RUNTIME_ALIAS.into(),
        "--tunnel-id".into(), tunnel_id,
        "--runtime-api-key".into(), "env:CHATX_TUNNEL_RUNTIME_KEY".into(),
        "--profile-dir".into(), profiles.to_string_lossy().into_owned(),
        "--mcp-command".into(), mcp_command(&paths, &dc_home), "--json".into()
    ];
    push_log(&state, "正在启动 Secure MCP Tunnel → Desktop Commander");
    let output = run_tunnel(&paths, &args, Some(&key))?;
    let text = output_text(&output);
    push_log(&state, format!("Tunnel connect: {text}"));
    if !output.status.success() { return Err(format!("启动 Tunnel 失败：{text}")); }
    get_status(app, state)
}

#[tauri::command]
fn stop_tunnel(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    stop_runtime(&app, Some(state.inner()))?;
    get_status(app, state)
}

#[tauri::command]
fn clear_saved_key(app: tauri::AppHandle, state: State<'_, AppState>) -> Result<Value, String> {
    let path = secret_path(&app)?;
    if path.exists() { fs::remove_file(path).map_err(|e| format!("清除 Runtime Key 失败：{e}"))?; }
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
    let saved = secret_path(&app)?.is_file();
    checks.push(json!({"name":"Tunnel ID","ok":settings.tunnel_id.starts_with("tunnel_"),"detail":settings.tunnel_id}));
    checks.push(json!({"name":"Runtime Key","ok":saved,"detail":if saved{"DPAPI saved"}else{"not saved; enter it when connecting"}}));
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

fn stop_on_exit(app: &tauri::AppHandle) { let _ = stop_runtime(app, None); }

fn main() {
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
                    "stop" => { let state = app.state::<AppState>(); let _ = stop_runtime(app, Some(state.inner())); }
                    "quit" => { app.state::<AppState>().quitting.store(true, Ordering::SeqCst); stop_on_exit(app); app.exit(0); }
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
        .invoke_handler(tauri::generate_handler![get_status, connect_tunnel, stop_tunnel, clear_saved_key, run_diagnostics, open_logs, open_external])
        .build(tauri::generate_context!()).expect("failed to build ChatX desktop application");
    app.run(|app_handle, event| { if matches!(event, tauri::RunEvent::Exit) { stop_on_exit(app_handle); } });
}
