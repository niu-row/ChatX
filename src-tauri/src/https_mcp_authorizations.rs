//! Desktop-side OAuth administration gateway. The bundled Node store owns
//! the grant format and revocation rules; Rust never directly edits grants.
use crate::{resource_candidates, runtime_paths, state_dir};
use serde_json::{json, Value};
use crate::settings::update_settings;
use std::{
    io::Read,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};
const ADMIN_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_ADMIN_OUTPUT: u64 = 1024 * 1024;

fn administer(
    app: &tauri::AppHandle, action: &str, grant_id: Option<&str>,
) -> Result<Value, String> {
    if !matches!(action, "list" | "revoke" | "revoke-all") {
        return Err("不支持的 OAuth 管理操作".into());
    }
    if action == "revoke" && !grant_id.is_some_and(|id| {
        id == "legacy" || (id.len() == 64 &&
            id.bytes().all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
    }) {
        return Err("OAuth 授权 ID 无效".into());
    }
    let paths = runtime_paths(app)?;
    let script = resource_candidates(app).into_iter()
        .map(|root| root.join("https-mcp-authorizations-admin.mjs"))
        .find(|path| path.is_file())
        .ok_or("缺少 OAuth 授权管理资源；请重新构建或安装 ChatX")?;
    let state = state_dir(app)?;
    let mut command = Command::new(&paths.node);
    command.arg(script).arg(action);
    if let Some(id) = grant_id { command.arg(id); }
    let mut child = command
        .env("CHATX_MCP_AUTH_STORE", state.join("https-oauth-authorizations"))
        .env("CHATX_MCP_SETTINGS_PATH", state.join("settings.json"))
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null())
        .spawn().map_err(|e| format!("启动 OAuth 授权管理失败：{e}"))?;
    // Drain stdout while the child runs: waiting before reading can deadlock
    // when many grant records fill the OS pipe. Never retain unbounded JSON.
    let stdout = child.stdout.take().ok_or("无法读取 OAuth 授权管理结果")?;
    let reader = thread::spawn(move || {
        let mut data = Vec::new();
        stdout.take(MAX_ADMIN_OUTPUT + 1).read_to_end(&mut data).map(|_| data)
    });
    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() < ADMIN_TIMEOUT =>
                thread::sleep(Duration::from_millis(25)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = reader.join();
                return Err("OAuth 授权管理超时或执行异常，已终止子进程".into());
            }
        }
    };
    let output = reader.join().map_err(|_| "OAuth 授权管理输出异常".to_string())?
        .map_err(|_| "无法读取 OAuth 授权管理结果".to_string())?;
    if !status.success() {
        // Do not return internal paths, secrets or Node diagnostics to UI.
        return Err("OAuth 授权管理失败，请检查本机诊断日志或授权数据".into());
    }
    if output.len() as u64 > MAX_ADMIN_OUTPUT {
        return Err("OAuth 授权记录过多，请先清理历史记录".into());
    }
    let result: Value = serde_json::from_slice(&output)
        .map_err(|_| "OAuth 授权管理返回无效数据".to_string())?;
    if !result["grants"].is_array() {
        return Err("OAuth 授权管理返回无效格式".into());
    }
    Ok(result)
}

#[tauri::command]
pub(super) fn set_https_mcp_authorization_lifetime(
    app: tauri::AppHandle, ttl_hours: Option<u32>,
) -> Result<Value, String> {
    crate::https_mcp::validate_authorization_lifetime(ttl_hours)?;
    update_settings(&app, |settings| {
        settings.https_mcp.authorization_ttl_hours = ttl_hours;
        Ok(())
    })?;
    Ok(json!({ "authorizationTtlHours": ttl_hours }))
}

// Password rotation calls this under the connection-operation lock. Keep
// the same bounded subprocess behavior without blocking a second async lock.
pub(super) fn revoke_all_sync(app: &tauri::AppHandle) -> Result<Value, String> {
    administer(app, "revoke-all", None)
}

#[tauri::command]
pub(super) async fn get_https_mcp_authorizations(
    app: tauri::AppHandle,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || administer(&app, "list", None))
        .await.map_err(|_| "OAuth 授权管理任务中断".to_string())?
}

#[tauri::command]
pub(super) async fn revoke_https_mcp_authorization(
    app: tauri::AppHandle, grant_id: String,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(
        move || administer(&app, "revoke", Some(&grant_id)))
        .await.map_err(|_| "OAuth 授权管理任务中断".to_string())?
}

#[tauri::command]
pub(super) async fn revoke_all_https_mcp_authorizations(
    app: tauri::AppHandle,
) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || revoke_all_sync(&app))
        .await.map_err(|_| "OAuth 授权管理任务中断".to_string())?
}
