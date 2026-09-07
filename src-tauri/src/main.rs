#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicBool, Ordering},
        Mutex,
    },
    time::Duration,
};

#[cfg(windows)]
use std::os::windows::process::CommandExt;

use hmac::{Hmac, Mac};
use serde_json::Value;
use sha2::Sha256;
use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    Manager, State, WindowEvent,
};
use tauri_plugin_dialog::DialogExt;
use uuid::Uuid;

const BACKEND_BASE: &str = "http://127.0.0.1:3210";
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const ALLOWED_API_PATHS: &[&str] = &[
    "/healthz",
    "/api/tunnel/status",
    "/api/diagnostics",
    "/api/invocations",
    "/api/settings",
    "/api/tunnel/connect",
    "/api/tunnel/stop",
    "/api/tunnel/key/clear",
];

struct BackendState {
    child: Mutex<Option<Child>>,
    log_path: Mutex<Option<PathBuf>>,
    quitting: AtomicBool,
    lifecycle: tokio::sync::Mutex<()>,
    recovery_attempted: AtomicBool,
    session_secret: String,
}

impl Default for BackendState {
    fn default() -> Self {
        Self {
            child: Mutex::new(None),
            log_path: Mutex::new(None),
            quitting: AtomicBool::new(false),
            lifecycle: tokio::sync::Mutex::new(()),
            recovery_attempted: AtomicBool::new(false),
            session_secret: format!("{}{}", Uuid::new_v4().simple(), Uuid::new_v4().simple()),
        }
    }
}

fn project_root() -> Option<PathBuf> {
    if let Ok(raw) = std::env::var("CHATGPTX_PROJECT_ROOT") {
        let root = PathBuf::from(raw);
        if root.join("package.json").is_file() {
            return Some(root);
        }
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir
        .parent()
        .filter(|root| root.join("package.json").is_file())
        .map(Path::to_path_buf)
}

fn migrate_source_state(source_root: &Path, target: &Path) {
    let source = source_root.join(".chatgptx");
    if !source.is_dir() {
        return;
    }
    let _ = fs::create_dir_all(target);
    for name in ["settings.json", "runtime-key.dpapi"] {
        let from = source.join(name);
        let to = target.join(name);
        if from.is_file() && !to.exists() {
            let _ = fs::copy(from, to);
        }
    }
}

fn desktop_proof(secret: &str, challenge: &str) -> String {
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).expect("HMAC accepts any key size");
    mac.update(challenge.as_bytes());
    mac.finalize()
        .into_bytes()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

async fn backend_is_healthy(session_secret: &str) -> bool {
    let client = match reqwest::Client::builder()
        .timeout(Duration::from_millis(900))
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };
    let challenge = Uuid::new_v4().simple().to_string();
    let expected_proof = desktop_proof(session_secret, &challenge);

    match client
        .get(format!("{BACKEND_BASE}/healthz"))
        .header("x-chatx-desktop-challenge", challenge)
        .send()
        .await
    {
        Ok(response) if response.status().is_success() => response
            .json::<Value>()
            .await
            .ok()
            .map(|body| {
                body.get("service").and_then(Value::as_str) == Some("chatx")
                    && body.get("desktop_proof").and_then(Value::as_str) == Some(expected_proof.as_str())
            })
            .unwrap_or(false),
        _ => false,
    }
}

fn resolve_node() -> String {
    std::env::var("CHATGPTX_NODE").unwrap_or_else(|_| {
        if cfg!(windows) {
            "node.exe".into()
        } else {
            "node".into()
        }
    })
}

fn bundled_resource_dir(app: &tauri::AppHandle) -> Option<PathBuf> {
    let has_runtime = |dir: &Path| {
        dir.join("chatgptx-backend.mjs").is_file() && dir.join("node.exe").is_file()
    };

    // Installed builds place bundled resources beside the desktop executable.
    // Prefer that deterministic location before asking Tauri for its resource directory.
    if let Ok(executable) = std::env::current_exe() {
        if let Some(dir) = executable.parent() {
            if has_runtime(dir) {
                return Some(dir.to_path_buf());
            }
        }
    }

    app.path()
        .resource_dir()
        .ok()
        .filter(|dir| has_runtime(dir))
}

fn backend_log_tail(path: &Path) -> String {
    let Ok(text) = fs::read_to_string(path) else {
        return String::new();
    };
    let mut lines = text.lines().rev().take(24).collect::<Vec<_>>();
    lines.reverse();
    lines.join("\n")
}

fn spawn_backend(app: &tauri::AppHandle, session_secret: &str) -> Result<(Child, PathBuf), String> {
    let resource_dir = bundled_resource_dir(app);
    let use_bundle = resource_dir.is_some();

    let (node, entry, cwd, settings_dir, tunnel) = if let Some(resource_dir) = resource_dir {
        let state_dir = app
            .path()
            .app_local_data_dir()
            .map_err(|error| format!("无法定位 ChatX 本地数据目录：{error}"))?
            .join("state");
        fs::create_dir_all(&state_dir)
            .map_err(|error| format!("无法创建 ChatX 本地数据目录：{error}"))?;
        if let Some(root) = project_root() {
            migrate_source_state(&root, &state_dir);
        }
        (
            resource_dir.join("node.exe"),
            resource_dir.join("chatgptx-backend.mjs"),
            state_dir.clone(),
            Some(state_dir),
            Some(resource_dir.join("tunnel-client.exe")).filter(|path| path.is_file()),
        )
    } else {
        let root = project_root().ok_or_else(|| {
            "没有找到安装版后端资源，也无法定位源码目录。请重新安装 ChatX。".to_string()
        })?;
        let entry = root.join("dist").join("index.js");
        if !entry.is_file() {
            return Err("没有找到 dist/index.js。请先运行 npm run build。".into());
        }
        (PathBuf::from(resolve_node()), entry, root, None, None)
    };

    let log_dir = app
        .path()
        .app_local_data_dir()
        .map_err(|error| format!("无法定位 ChatX 日志目录：{error}"))?;
    fs::create_dir_all(&log_dir).map_err(|error| format!("无法创建 ChatX 日志目录：{error}"))?;
    let log_path = log_dir.join("backend.log");
    let log_file = fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&log_path)
        .map_err(|error| format!("无法创建后端日志：{error}"))?;
    let stderr_file = log_file
        .try_clone()
        .map_err(|error| format!("无法打开后端错误日志：{error}"))?;

    let mut command = Command::new(node);
    command
        .arg(entry)
        .current_dir(&cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::from(log_file))
        .stderr(Stdio::from(stderr_file));

    // The desktop application owns its local endpoint. Do not inherit stale shell
    // variables such as CHATGPTX_PORT from a terminal that launched the UI.
    command
        .env_remove("CHATGPTX_HOST")
        .env_remove("CHATGPTX_PORT")
        .env_remove("CHATX_DESKTOP_SESSION_SECRET")
        .env("CHATGPTX_HOST", "127.0.0.1")
        .env("CHATGPTX_PORT", "3210")
        .env("CHATX_DESKTOP_SESSION_SECRET", session_secret);

    if let Some(settings_dir) = settings_dir {
        command
            .env_remove("CHATGPTX_SETTINGS_DIR")
            .env_remove("CHATGPTX_ROOTS")
            .env("CHATGPTX_SETTINGS_DIR", &settings_dir)
            .env("CHATGPTX_ROOTS", &settings_dir);
    }
    if let Some(tunnel) = tunnel {
        command
            .env_remove("TUNNEL_CLIENT_PATH")
            .env("TUNNEL_CLIENT_PATH", tunnel);
    } else if use_bundle {
        command.env_remove("TUNNEL_CLIENT_PATH");
    }

    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let child = command
        .spawn()
        .map_err(|error| format!("启动 ChatX 后端失败：{error}"))?;
    Ok((child, log_path))
}

async fn ensure_backend_started(app: &tauri::AppHandle, state: &BackendState) -> Result<String, String> {
    if backend_is_healthy(&state.session_secret).await {
        return Ok("already-running".into());
    }

    let mut spawned = false;
    {
        let mut guard = state.child.lock().map_err(|_| "后端状态锁已损坏。".to_string())?;
        let running = if let Some(child) = guard.as_mut() {
            child.try_wait().map_err(|error| error.to_string())?.is_none()
        } else {
            false
        };

        if !running {
            let (child, log_path) = spawn_backend(app, &state.session_secret)?;
            *state
                .log_path
                .lock()
                .map_err(|_| "后端日志状态锁已损坏。".to_string())? = Some(log_path);
            *guard = Some(child);
            spawned = true;
        }
    }

    for _ in 0..40 {
        if backend_is_healthy(&state.session_secret).await {
            return Ok(if spawned { "started" } else { "recovered" }.into());
        }

        let exit_status = {
            let mut guard = state.child.lock().map_err(|_| "后端状态锁已损坏。".to_string())?;
            match guard.as_mut() {
                Some(child) => child.try_wait().map_err(|error| error.to_string())?,
                None => None,
            }
        };

        if let Some(status) = exit_status {
            let log_path = state
                .log_path
                .lock()
                .ok()
                .and_then(|value| value.clone());
            let tail = log_path
                .as_deref()
                .map(backend_log_tail)
                .unwrap_or_default();
            let code = status
                .code()
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".into());
            let suffix = if tail.is_empty() {
                String::new()
            } else {
                format!("\n\n后端日志：\n{tail}")
            };
            return Err(format!("ChatX 后端启动后立即退出（exit code {code}）。{suffix}"));
        }

        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    let tail = state
        .log_path
        .lock()
        .ok()
        .and_then(|value| value.clone())
        .as_deref()
        .map(backend_log_tail)
        .unwrap_or_default();
    let suffix = if tail.is_empty() {
        String::new()
    } else {
        format!("\n\n后端日志：\n{tail}")
    };
    Err(format!("ChatX 后端已经启动，但 6 秒内没有通过 /healthz。{suffix}"))
}


async fn clean_installed_runtime(app: &tauri::AppHandle) -> Result<(), String> {
    let directory = bundled_resource_dir(app).ok_or_else(|| {
        "未找到本安装目录的运行组件。源码模式请手动停止对应后端，避免终止其他 Node 进程。".to_string()
    })?;
    let app = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        stop_owned_backend(&app);
        #[cfg(windows)]
        {
            let script = include_str!("../windows/stop-runtime.ps1");
            let quoted_directory = directory.to_string_lossy().replace('\'', "''");
            let output = Command::new("powershell.exe")
                .args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command",
                    &format!("& {{ {script} }} -InstallDir '{quoted_directory}' -SkipDesktop")])
                .creation_flags(CREATE_NO_WINDOW)
                .output()
                .map_err(|error| format!("无法清理残留进程：{error}"))?;
            if !output.status.success() {
                return Err(format!("清理残留进程失败：{}\n{}",
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr)));
            }
            Ok(())
        }
        #[cfg(not(windows))]
        {
            let _ = directory;
            Err("残留进程清理目前仅支持 Windows 安装版。".into())
        }
    }).await.map_err(|error| format!("清理任务失败：{error}"))?
}

async fn ensure_backend_impl(app: &tauri::AppHandle, state: &BackendState) -> Result<String, String> {
    // Serialize startup/recovery across polling requests and the repair button.
    let _operation = state.lifecycle.lock().await;
    match ensure_backend_started(app, state).await {
        Ok(result) => Ok(result),
        Err(first_error) => {
            // One automatic attempt per desktop session; do not kill/restart on every poll.
            if state.recovery_attempted.swap(true, Ordering::SeqCst)
                || bundled_resource_dir(app).is_none() {
                return Err(first_error);
            }
            clean_installed_runtime(app).await
                .map_err(|error| format!("{first_error}\n{error}"))?;
            ensure_backend_started(app, state).await
                .map_err(|error| format!("{first_error}\n清理残留进程后仍无法启动：{error}"))
        }
    }
}

#[tauri::command]
async fn restart_backend(app: tauri::AppHandle, state: State<'_, BackendState>) -> Result<String, String> {
    let _operation = state.lifecycle.lock().await;
    state.recovery_attempted.store(true, Ordering::SeqCst);
    clean_installed_runtime(&app).await?;
    ensure_backend_started(&app, &state).await?;
    Ok("残留进程已清理，后端已重启。请重新连接 Tunnel。".into())
}

#[tauri::command]
async fn ensure_backend(app: tauri::AppHandle, state: State<'_, BackendState>) -> Result<String, String> {
    ensure_backend_impl(&app, &state).await
}

async fn send_backend_request(method: &str, path: &str, body: &Option<Value>, session_secret: &str) -> Result<Value, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(75))
        .build()
        .map_err(|error| error.to_string())?;
    let url = format!("{BACKEND_BASE}{path}");
    let empty = serde_json::json!({});

    let response = if method == "GET" {
        client
            .get(url)
            .header("x-chatx-desktop-session", session_secret)
            .send()
            .await
    } else {
        client
            .post(url)
            .header("x-chatx-desktop-session", session_secret)
            .header("content-type", "application/json")
            .header("origin", BACKEND_BASE)
            .header("sec-fetch-site", "same-origin")
            .json(body.as_ref().unwrap_or(&empty))
            .send()
            .await
    }
    .map_err(|error| format!("无法连接 ChatX 后端：{error}"))?;

    let status = response.status();
    let text = response
        .text()
        .await
        .map_err(|error| format!("读取后端响应失败：{error}"))?;
    let parsed = serde_json::from_str::<Value>(&text)
        .map_err(|error| format!("后端返回了无效 JSON（HTTP {status}）：{error}"))?;

    if !status.is_success() {
        let message = parsed
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("后端请求失败");
        return Err(format!("{message}（HTTP {status}）"));
    }

    Ok(parsed)
}

#[tauri::command]
async fn backend_request(
    app: tauri::AppHandle,
    state: State<'_, BackendState>,
    method: String,
    path: String,
    body: Option<Value>,
) -> Result<Value, String> {
    if !ALLOWED_API_PATHS.contains(&path.as_str()) {
        return Err(format!("桌面控制台不允许访问此后端路径：{path}"));
    }

    let method = method.to_ascii_uppercase();
    if method != "GET" && method != "POST" {
        return Err("只允许 GET 或 POST。".into());
    }

    if !backend_is_healthy(&state.session_secret).await {
        ensure_backend_impl(&app, &state).await?;
    }

    match send_backend_request(&method, &path, &body, &state.session_secret).await {
        Ok(value) => Ok(value),
        Err(first_error) if !backend_is_healthy(&state.session_secret).await => {
            ensure_backend_impl(&app, &state).await?;
            send_backend_request(&method, &path, &body, &state.session_secret)
                .await
                .map_err(|second_error| format!("{first_error}\n后端自动恢复后重试仍失败：{second_error}"))
        }
        Err(error) => Err(error),
    }
}

#[tauri::command]
async fn pick_folders(app: tauri::AppHandle) -> Result<Vec<String>, String> {
    let picked = app.dialog().file().blocking_pick_folders();
    let Some(items) = picked else {
        return Ok(Vec::new());
    };

    items
        .into_iter()
        .map(|item| {
            item.simplified()
                .into_path()
                .map(|path| path.to_string_lossy().into_owned())
                .map_err(|error| format!("无法读取所选目录：{error}"))
        })
        .collect()
}

#[tauri::command]
fn open_external(url: String) -> Result<(), String> {
    const ALLOWED: &[&str] = &[
        "https://platform.openai.com/settings/organization/tunnels",
        "https://platform.openai.com/settings/organization/api-keys",
        "https://developers.openai.com/api/docs/guides/secure-mcp-tunnels",
        "https://chatgpt.com/plugins",
        "https://help.openai.com/en/articles/12584461-developer-mode-and-full-mcp-connectors-in-chatgpt-beta",
    ];
    if !ALLOWED.contains(&url.as_str()) {
        return Err("不允许打开未列入白名单的外部地址。".into());
    }
    open::that(url).map_err(|error| format!("打开浏览器失败：{error}"))
}

fn show_main_window(app: &tauri::AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.show();
        let _ = window.unminimize();
        let _ = window.set_focus();
    }
}

fn stop_owned_backend(app: &tauri::AppHandle) {
    let state = app.state::<BackendState>();
    let mut guard = match state.child.lock() {
        Ok(guard) => guard,
        Err(_) => return,
    };

    if let Some(child) = guard.as_mut() {
        // Stop the tunnel first so a forceful Windows child termination does not orphan tunnel-client.
        if let Ok(client) = reqwest::blocking::Client::builder()
            .timeout(Duration::from_secs(2))
            .build()
        {
            let _ = client
                .post(format!("{BACKEND_BASE}/api/tunnel/stop"))
                .header("x-chatx-desktop-session", &state.session_secret)
                .header("content-type", "application/json")
                .header("origin", BACKEND_BASE)
                .header("sec-fetch-site", "same-origin")
                .body("{}")
                .send();
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    *guard = None;
}

fn main() {
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .manage(BackendState::default())
        .setup(|app| {
            let show_item = MenuItem::with_id(app, "show", "打开 ChatX", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "退出 ChatX", true, None::<&str>)?;
            let menu = Menu::with_items(app, &[&show_item, &quit_item])?;

            let mut tray = TrayIconBuilder::with_id("chatgptx-tray")
                .tooltip("ChatX")
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(|app, event| match event.id().as_ref() {
                    "show" => show_main_window(app),
                    "quit" => {
                        app.state::<BackendState>()
                            .quitting
                            .store(true, Ordering::SeqCst);
                        app.exit(0);
                    }
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    if let TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        show_main_window(tray.app_handle());
                    }
                });

            if let Some(icon) = app.default_window_icon() {
                tray = tray.icon(icon.clone());
            }
            tray.build(app)?;
            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() != "main" {
                return;
            }
            if let WindowEvent::CloseRequested { api, .. } = event {
                let quitting = window
                    .app_handle()
                    .state::<BackendState>()
                    .quitting
                    .load(Ordering::SeqCst);
                if !quitting {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            ensure_backend,
            restart_backend,
            backend_request,
            pick_folders,
            open_external,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build ChatX desktop application");

    app.run(|app_handle, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            stop_owned_backend(app_handle);
        }
    });
}
