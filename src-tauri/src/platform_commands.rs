use crate::app_state::AppState;
use serde_json::{json, Value};
use tauri::State;

#[cfg(target_os = "macos")]
use super::{output_text, push_log, secret_path, state_dir};
#[cfg(target_os = "macos")]
use crate::{
    process_utils::command_output,
    secrets::{runtime_key_saved, unprotect_secret},
};
#[cfg(target_os = "macos")]
use std::{
    fs,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(any(target_os = "macos", test))]
fn pmset_disables_sleep(text: &str) -> bool {
    text.lines().any(|line| {
        let mut parts = line.split_whitespace();
        matches!(
            (parts.next(), parts.next()),
            (Some("SleepDisabled"), Some("1"))
                | (Some("disablesleep"), Some("1"))
        )
    })
}
#[cfg(any(target_os = "macos", test))]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(target_os = "macos")]
fn macos_clamshell_marker_path(
    app: &tauri::AppHandle,
) -> Result<PathBuf, String> {
    Ok(state_dir(app)?.join("clamshell-awake.owner"))
}

#[cfg(target_os = "macos")]
fn macos_clamshell_owned_by_current_process(
    app: &tauri::AppHandle,
) -> bool {
    let Ok(path) = macos_clamshell_marker_path(app) else {
        return false;
    };
    let Ok(value) = fs::read_to_string(path) else {
        return false;
    };
    value
        .trim()
        .starts_with(&format!("{}:", std::process::id()))
}

#[cfg(target_os = "macos")]
fn macos_clamshell_awake_enabled() -> Result<bool, String> {
    let mut command = Command::new("/usr/bin/pmset");
    command.arg("-g");
    let output = command_output(&mut command)?;
    if !output.status.success() {
        return Err(format!(
            "读取 macOS 电源设置失败：{}",
            output_text(&output)
        ));
    }
    Ok(pmset_disables_sleep(
        &String::from_utf8_lossy(&output.stdout),
    ))
}

#[cfg(target_os = "macos")]
fn run_macos_admin_shell(shell_command: &str) -> Result<(), String> {
    let mut command = Command::new("/usr/bin/osascript");
    command
        .args([
            "-e",
            "do shell script (system attribute \"CHATX_PRIVILEGED_COMMAND\") with administrator privileges",
        ])
        .env("CHATX_PRIVILEGED_COMMAND", shell_command);
    let output = command_output(&mut command)?;
    if output.status.success() {
        return Ok(());
    }
    let detail = output_text(&output);
    if detail.contains("(-128)")
        || detail.to_ascii_lowercase().contains("user canceled")
    {
        return Err(
            "已取消管理员授权，合盖运行设置未更改。".into(),
        );
    }
    Err(format!("修改 macOS 合盖运行设置失败：{detail}"))
}

#[cfg(target_os = "macos")]
fn macos_enable_clamshell_with_watchdog(
    app: &tauri::AppHandle,
) -> Result<(), String> {
    let marker = macos_clamshell_marker_path(app)?;
    let pid = std::process::id();
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_nanos())
        .unwrap_or(0);
    let owner = format!("{pid}:{nonce}");
    fs::write(&marker, format!("{owner}\n"))
        .map_err(|e| format!("记录合盖运行所有权失败：{e}"))?;

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
        return Err(
            "macOS 未应用合盖运行设置，请检查系统电源策略。".into(),
        );
    }
    Ok(())
}

#[tauri::command]
pub(super) fn get_power_settings(
    app: tauri::AppHandle,
) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        return Ok(json!({
            "supported": true,
            "clamshellAwake": macos_clamshell_awake_enabled()?,
            "managedByChatX":
                macos_clamshell_owned_by_current_process(&app)
        }));
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = app;
        Ok(json!({
            "supported": false,
            "clamshellAwake": false,
            "managedByChatX": false
        }))
    }
}

#[tauri::command]
pub(super) fn set_clamshell_awake(
    app: tauri::AppHandle,
    enabled: bool,
) -> Result<Value, String> {
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
                run_macos_admin_shell(
                    "/usr/bin/pmset -a disablesleep 0",
                )?;
            }
            let _ = fs::remove_file(&marker);
        }
        let actual = macos_clamshell_awake_enabled()?;
        if actual != enabled {
            return Err(
                "macOS 未应用合盖运行设置，请检查系统电源策略。".into(),
            );
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
fn permission_directory_result(
    id: &str,
    label: &str,
    path: &Path,
) -> Value {
    let (status, detail) = match fs::read_dir(path) {
        Ok(mut entries) => {
            let _ = entries.next();
            ("granted", format!("已允许访问 {}", path.display()))
        }
        Err(error)
            if error.kind() == std::io::ErrorKind::PermissionDenied =>
        {
            ("denied", format!("macOS 拒绝访问 {}", path.display()))
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ("unavailable", format!("目录不存在：{}", path.display()))
        }
        Err(error) => (
            "error",
            format!("检查 {} 失败：{error}", path.display()),
        ),
    };
    json!({
        "id": id,
        "label": label,
        "status": status,
        "detail": detail
    })
}

#[cfg(target_os = "macos")]
fn pending_permission_payload(app: &tauri::AppHandle) -> Value {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
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
        "fullDiskAccess": {
            "status":"manual",
            "detail":"macOS 不允许应用静默授予“完全磁盘访问”，需要在系统设置中手动打开。"
        }
    })
}

#[tauri::command]
pub(super) fn get_permission_center(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        if let Ok(cache) = state.permission_results.lock() {
            if let Some(payload) = cache.clone() {
                return Ok(payload);
            }
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
            "fullDiskAccess": {
                "status":"notNeeded",
                "detail":"当前平台不使用 macOS TCC 文件夹授权。"
            }
        }))
    }
}

#[tauri::command]
pub(super) fn request_all_permissions(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
) -> Result<Value, String> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| {
                "无法定位当前 macOS 用户主目录。".to_string()
            })?;
        let mut items = vec![
            permission_directory_result(
                "desktop",
                "桌面文件夹",
                &home.join("Desktop"),
            ),
            permission_directory_result(
                "documents",
                "文稿文件夹",
                &home.join("Documents"),
            ),
            permission_directory_result(
                "downloads",
                "下载文件夹",
                &home.join("Downloads"),
            ),
        ];
        let keychain = if runtime_key_saved(&app) {
            match unprotect_secret(&secret_path(&app)?) {
                Ok(_) => json!({
                    "id":"keychain",
                    "label":"macOS 钥匙串",
                    "status":"granted",
                    "detail":"已验证 ChatX 可读取保存的 Runtime Key"
                }),
                Err(error) => json!({
                    "id":"keychain",
                    "label":"macOS 钥匙串",
                    "status":"denied",
                    "detail":error
                }),
            }
        } else {
            json!({
                "id":"keychain",
                "label":"macOS 钥匙串",
                "status":"notNeeded",
                "detail":"尚未保存 Runtime Key，无需授权"
            })
        };
        items.push(keychain);
        let payload = json!({
            "supported": true,
            "platform": "macOS",
            "requested": true,
            "items": items,
            "fullDiskAccess": {
                "status":"manual",
                "detail":"常用文件夹权限已集中触发；如需访问其他受保护位置，请在系统设置中为 ChatX 开启“完全磁盘访问”。"
            }
        });
        if let Ok(mut cache) = state.permission_results.lock() {
            *cache = Some(payload.clone());
        }
        push_log(
            &state,
            "权限中心已完成常用 macOS 权限预授权检查",
        );
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
            "fullDiskAccess": {
                "status":"notNeeded",
                "detail":"当前平台不需要 macOS 文件夹预授权。"
            }
        }))
    }
}
#[tauri::command]
pub(super) fn open_full_disk_access_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        return open::that(
            "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles",
        )
        .map_err(|e| {
            format!("打开 macOS 完全磁盘访问设置失败：{e}")
        });
    }
    #[cfg(not(target_os = "macos"))]
    {
        Err("“完全磁盘访问”仅适用于 macOS。".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_escapes_single_quotes() {
        assert_eq!(shell_quote("plain path"), "'plain path'");
        assert_eq!(shell_quote("a'b"), "'a'\\''b'");
    }

    #[test]
    fn pmset_disables_sleep_parser_tracks_enabled_value() {
        let current =
            "System-wide power settings:\n SleepDisabled\t\t1\nCurrently in use:\n sleep                1\n";
        let legacy =
            "AC Power:\n disablesleep         1\n sleep                0\n";
        let disabled =
            "System-wide power settings:\nCurrently in use:\n sleep                1\n";
        assert!(pmset_disables_sleep(current));
        assert!(pmset_disables_sleep(legacy));
        assert!(!pmset_disables_sleep(disabled));
    }
}
