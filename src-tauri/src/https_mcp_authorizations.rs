//! Desktop-side OAuth administration gateway. The bundled Node store owns
//! the grant format and revocation rules; Rust never directly edits grants.
use crate::{resource_candidates, runtime_paths, state_dir};
use serde_json::{json, Value};
use crate::settings::update_settings;
use std::process::{Command, Stdio};

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
    let output = command
        .env("CHATX_MCP_AUTH_STORE", state.join("https-oauth-authorizations"))
        .env("CHATX_MCP_SETTINGS_PATH", state.join("settings.json"))
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .output().map_err(|e| format!("启动 OAuth 授权管理失败：{e}"))?;
    if !output.status.success() {
        // Do not return internal paths, secrets or other Node diagnostics to UI.
        return Err("OAuth 授权管理失败，请检查本机诊断日志或授权数据".into());
    }
    let result: Value = serde_json::from_slice(&output.stdout)
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

#[tauri::command]
pub(super) fn get_https_mcp_authorizations(
    app: tauri::AppHandle,
) -> Result<Value, String> {
    administer(&app, "list", None)
}

#[tauri::command]
pub(super) fn revoke_https_mcp_authorization(
    app: tauri::AppHandle, grant_id: String,
) -> Result<Value, String> {
    administer(&app, "revoke", Some(&grant_id))
}

#[tauri::command]
pub(super) fn revoke_all_https_mcp_authorizations(
    app: tauri::AppHandle,
) -> Result<Value, String> {
    administer(&app, "revoke-all", None)
}
