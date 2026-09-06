use std::{
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::Mutex,
    time::Duration,
};

use serde_json::Value;
use tauri::{Manager, State};
use tauri_plugin_dialog::DialogExt;

const BACKEND_BASE: &str = "http://127.0.0.1:3210";
const ALLOWED_API_PATHS: &[&str] = &[
    "/healthz",
    "/api/tunnel/status",
    "/api/diagnostics",
    "/api/settings",
    "/api/tunnel/connect",
    "/api/tunnel/stop",
    "/api/tunnel/key/clear",
];

#[derive(Default)]
struct BackendState {
    child: Mutex<Option<Child>>,
}

fn project_root() -> Result<PathBuf, String> {
    if let Ok(raw) = std::env::var("CHATGPTX_PROJECT_ROOT") {
        let root = PathBuf::from(raw);
        if root.join("package.json").is_file() {
            return Ok(root);
        }
    }

    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    if let Some(root) = manifest_dir.parent() {
        if root.join("package.json").is_file() {
            return Ok(root.to_path_buf());
        }
    }

    Err("无法定位 ChatGPTX 项目目录。可设置 CHATGPTX_PROJECT_ROOT 后重试。".into())
}

fn backend_is_healthy() -> bool {
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_millis(900))
        .build()
    {
        Ok(client) => client,
        Err(_) => return false,
    };

    match client.get(format!("{BACKEND_BASE}/healthz")).send() {
        Ok(response) if response.status().is_success() => response
            .json::<Value>()
            .ok()
            .and_then(|body| body.get("service").and_then(Value::as_str).map(str::to_owned))
            .as_deref()
            == Some("chatgptx"),
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

fn spawn_backend(root: &Path) -> Result<Child, String> {
    let entry = root.join("dist").join("index.js");
    if !entry.is_file() {
        return Err("没有找到 dist/index.js。请先运行 npm run build。".into());
    }

    Command::new(resolve_node())
        .arg(entry)
        .current_dir(root)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("启动 ChatGPTX 后端失败：{error}"))
}

#[tauri::command]
async fn ensure_backend(state: State<'_, BackendState>) -> Result<String, String> {
    if backend_is_healthy() {
        return Ok("already-running".into());
    }

    let root = project_root()?;
    {
        let mut guard = state.child.lock().map_err(|_| "后端状态锁已损坏。".to_string())?;
        if let Some(child) = guard.as_mut() {
            if child.try_wait().map_err(|error| error.to_string())?.is_none() {
                return Ok("starting".into());
            }
        }
        *guard = Some(spawn_backend(&root)?);
    }

    for _ in 0..40 {
        if backend_is_healthy() {
            return Ok("started".into());
        }
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    Err("ChatGPTX 后端已经启动，但 6 秒内没有通过 /healthz。".into())
}

#[tauri::command]
async fn backend_request(method: String, path: String, body: Option<Value>) -> Result<Value, String> {
    if !ALLOWED_API_PATHS.contains(&path.as_str()) {
        return Err(format!("桌面控制台不允许访问此后端路径：{path}"));
    }

    let method = method.to_ascii_uppercase();
    if method != "GET" && method != "POST" {
        return Err("只允许 GET 或 POST。".into());
    }

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(75))
        .build()
        .map_err(|error| error.to_string())?;
    let url = format!("{BACKEND_BASE}{path}");

    let response = if method == "GET" {
        client.get(url).send().await
    } else {
        client
            .post(url)
            .header("content-type", "application/json")
            .header("origin", BACKEND_BASE)
            .header("sec-fetch-site", "same-origin")
            .json(&body.unwrap_or_else(|| serde_json::json!({})))
            .send()
            .await
    }
    .map_err(|error| format!("无法连接 ChatGPTX 后端：{error}"))?;

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
    ];
    if !ALLOWED.contains(&url.as_str()) {
        return Err("不允许打开未列入白名单的外部地址。".into());
    }
    open::that(url).map_err(|error| format!("打开浏览器失败：{error}"))
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
        .invoke_handler(tauri::generate_handler![
            ensure_backend,
            backend_request,
            pick_folders,
            open_external,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build ChatGPTX desktop application");

    app.run(|app_handle, event| {
        if matches!(event, tauri::RunEvent::Exit) {
            stop_owned_backend(app_handle);
        }
    });
}
