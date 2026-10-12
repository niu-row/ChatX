// Optional, explicit SSH provisioning of the user's own Relay server.
// SSH is never part of the MCP runtime transport.
use crate::{app_state::AppState, resource_candidates, runtime_paths, state_dir};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Command, Stdio},
    sync::atomic::Ordering,
};
use tauri::{AppHandle, Emitter, State};

#[derive(Clone, Debug, Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RelayDeployConfig {
    host: String,
    user: String,
    port: u16,
    domain: String,
    desktop_id: String,
    #[serde(default)]
    key_path: String,
}

fn host_ok(value: &str) -> bool {
    !value.is_empty() && value.len() <= 253
        && !value.starts_with('-') && !value.contains("..")
        && value.bytes().all(|byte| byte.is_ascii_alphanumeric()
            || byte == b'.' || byte == b'-')
}
impl RelayDeployConfig {
    fn validate(&self) -> Result<(), String> {
        if !host_ok(&self.host) || !host_ok(&self.domain) || self.port == 0 {
            return Err("服务器地址、域名或 SSH 端口无效".into());
        }
        if self.user.len() > 32 || self.user.is_empty()
            || !self.user.bytes().all(|b| b.is_ascii_lowercase() ||
                b.is_ascii_digit() || b == b'_' || b == b'-')
            || !self.user.as_bytes()[0].is_ascii_lowercase()
                && self.user.as_bytes()[0] != b'_' {
            return Err("SSH 用户名不合法".into());
        }
        if !(18..=80).contains(&self.desktop_id.len()) || !self.desktop_id.starts_with("d_")
            || !self.desktop_id[2..].bytes()
                .all(|b| b.is_ascii_hexdigit()) {
            return Err("请先在网络设置注册本机 Desktop，再填写 Desktop ID".into());
        }
        if !self.key_path.is_empty() && !PathBuf::from(&self.key_path).is_absolute() {
            return Err("SSH 私钥必须使用绝对路径".into());
        }
        Ok(())
    }
}

fn run(app: AppHandle, config: RelayDeployConfig, apply: bool)
    -> Result<Value, String>
{
    let runtime = runtime_paths(&app)?;
    let log_path = state_dir(&app)?.join("relay-deploy.log");
    if std::fs::metadata(&log_path).is_ok_and(|meta| meta.len() > 1024 * 1024) {
        let _ = std::fs::rename(&log_path, log_path.with_extension("log.old"));
    }
    let mut log = std::fs::OpenOptions::new().create(true).append(true)
        .open(&log_path).map_err(|e| format!("无法写入部署日志：{e}"))?;
    let _ = writeln!(log, "DEPLOY SESSION mode={}", if apply { "apply" } else { "check" });
    let resource = resource_candidates(&app).into_iter().find(|root| {
        root.join("relay-deploy.mjs").is_file()
            && root.join("relay-deploy-install.sh").is_file()
            && root.join("relay-deploy-source.tar.gz").is_file()
    }).ok_or("部署资源缺失：请先执行 desktop:prepare 并重新安装 ChatX")?;
    let script = resource.join("relay-deploy.mjs");
    let archive = resource.join("relay-deploy-source.tar.gz");
    let mut child = Command::new(&runtime.node)
        .arg(&script).arg(if apply { "deploy" } else { "check" })
        .arg(&archive)
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
        .spawn().map_err(|e| format!("无法启动部署工具：{e}"))?;
    if let Some(mut input) = child.stdin.take() {
        let bytes = serde_json::to_vec(&config).map_err(|e| e.to_string())?;
        input.write_all(&bytes).map_err(|e| e.to_string())?;
    }
    let stderr = child.stderr.take().ok_or("无法读取部署错误输出")?;
    // Drain stderr concurrently; it can include third-party tool diagnostics.
    let errors = std::thread::spawn(move || {
        let mut reader = BufReader::new(stderr);
        let mut output = String::new();
        let _ = std::io::Read::read_to_string(&mut reader, &mut output);
        output.chars().rev().take(1200).collect::<String>()
            .chars().rev().collect::<String>()
    });
    let stdout = child.stdout.take().ok_or("无法读取部署进度")?;
    let mut last_result: Option<Value> = None;
    for line in BufReader::new(stdout).lines() {
        let line = line.map_err(|e| e.to_string())?;
        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue; };
        if value["kind"] == "progress" {
            if let Some(message) = value["message"].as_str() {
                let _ = writeln!(log, "{}", message.chars().take(200).collect::<String>());
            }
            let _ = app.emit("relay-deploy-progress", &value);
        } else if value["kind"] == "result" {
            let state = if value["ok"] == true { "OK" } else { "FAILED" };
            let _ = writeln!(log, "RESULT {}", state);
            last_result = Some(value);
        }
    }
    let status = child.wait().map_err(|e| e.to_string())?;
    let diagnostics = errors.join().unwrap_or_default();
    let result = last_result.ok_or_else(|| {
        format!("部署工具没有返回完成状态：{}",
            diagnostics.chars().take(350).collect::<String>())
    })?;
    if !status.success() || result["ok"] != true {
        return Err(result["message"].as_str().unwrap_or("部署预检失败").to_string());
    }
    if let Some(token) = result["bootstrapToken"].as_str() {
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("服务器返回的 Relay 注册令牌格式不正确".into());
        }
        #[cfg(target_os = "macos")]
        let keyfile = crate::macos_account_home()?
            .join("Library/Application Support/com.chatgptx.local/signing/relay-setup-token");
        #[cfg(not(target_os = "macos"))]
        let keyfile = state_dir(&app)?.join("relay-setup-token");
        let parent = keyfile.parent().ok_or("无法定位 Relay 令牌目录")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let stage = keyfile.with_extension("new");
        std::fs::write(&stage, token).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&stage, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| e.to_string())?;
        }
        if keyfile.exists() {
            std::fs::copy(&keyfile, keyfile.with_extension("previous"))
                .map_err(|e| e.to_string())?;
            std::fs::remove_file(&keyfile).map_err(|e| e.to_string())?;
        }
        std::fs::rename(stage, &keyfile).map_err(|e| e.to_string())?;
        let _ = writeln!(log, "BOOTSTRAP securely provisioned");
    }
    Ok(json!({ "ok": true, "message": result["message"] }))
}

#[tauri::command]
pub(super) async fn manage_relay_server(
    app: AppHandle,
    state: State<'_, AppState>,
    config: RelayDeployConfig,
    apply: bool,
) -> Result<Value, String> {
    config.validate()?;
    if state.relay.deploy_busy.swap(true, Ordering::SeqCst) {
        return Err("已有服务器部署或预检操作进行中".into());
    }
    let cloned = app.clone();
    let outcome = tauri::async_runtime::spawn_blocking(move || run(cloned, config, apply)).await;
    state.relay.deploy_busy.store(false, Ordering::SeqCst);
    outcome.map_err(|e| format!("部署任务异常退出：{e}"))?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_unsafe_ssh_inputs() {
        let config = RelayDeployConfig {
            host: "47.103.79.86".into(), user: "root".into(), port: 22,
            domain: "newrow.fun".into(),
            desktop_id: format!("d_{}", "a".repeat(32)),
            key_path: String::new(),
        };
        assert!(config.validate().is_ok());
        for length in [16, 32, 78] {
            let mut legacy = config.clone();
            legacy.desktop_id = format!("d_{}", "a".repeat(length));
            assert!(legacy.validate().is_ok(), "valid existing Desktop ID must be preserved");
        }
        let mut uppercase = config.clone();
        uppercase.desktop_id = format!("d_{}", "A".repeat(32));
        assert!(uppercase.validate().is_ok());
        for host in ["-oProxyCommand=evil", "hi;id", "a..b"] {
            let mut invalid = config.clone();
            invalid.host = host.into();
            assert!(invalid.validate().is_err());
        }
        let mut invalid = config.clone();
        invalid.desktop_id = "d_wrong".into();
        assert!(invalid.validate().is_err());
    }
}
