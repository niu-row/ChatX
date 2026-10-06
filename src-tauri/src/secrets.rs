use super::*;

pub(super) fn runtime_key_saved(app: &tauri::AppHandle) -> bool {
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

#[cfg(windows)]
pub(super) fn protect_secret(secret: &str, target: &Path) -> Result<(), String> {
    let script = r#"$ErrorActionPreference='Stop';[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);Add-Type -AssemblyName System.Security;$b=[Text.Encoding]::UTF8.GetBytes($env:CHATX_SECRET);$p=[System.Security.Cryptography.ProtectedData]::Protect($b,$null,[System.Security.Cryptography.DataProtectionScope]::CurrentUser);[IO.File]::WriteAllText($env:CHATX_SECRET_FILE,[Convert]::ToBase64String($p))"#;
    let mut command = Command::new("powershell.exe");
    command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script])
        .env("CHATX_SECRET", secret).env("CHATX_SECRET_FILE", target);
    let output = command_output(&mut command)?;
    if output.status.success() { Ok(()) } else { Err(format!("保存 Runtime Key 失败：{}", output_text(&output))) }
}

#[cfg(not(any(windows, target_os = "macos")))]
pub(super) fn protect_secret(_secret: &str, _target: &Path) -> Result<(), String> { Err("安全保存 Runtime Key 当前仅支持 Windows。".into()) }

#[cfg(windows)]
pub(super) fn unprotect_secret(target: &Path) -> Result<String, String> {
    let script = r#"$ErrorActionPreference='Stop';[Console]::OutputEncoding=[Text.UTF8Encoding]::new($false);Add-Type -AssemblyName System.Security;$s=[IO.File]::ReadAllText($env:CHATX_SECRET_FILE);$p=[Convert]::FromBase64String($s);$b=[System.Security.Cryptography.ProtectedData]::Unprotect($p,$null,[System.Security.Cryptography.DataProtectionScope]::CurrentUser);[Console]::Out.Write([Text.Encoding]::UTF8.GetString($b))"#;
    let mut command = Command::new("powershell.exe");
    command.args(["-NoLogo", "-NoProfile", "-NonInteractive", "-Command", script]).env("CHATX_SECRET_FILE", target);
    let output = command_output(&mut command)?;
    if !output.status.success() { return Err(format!("读取已保存 Runtime Key 失败：{}", output_text(&output))); }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(not(any(windows, target_os = "macos")))]
pub(super) fn unprotect_secret(_target: &Path) -> Result<String, String> { Err("安全读取 Runtime Key 当前仅支持 Windows。".into()) }

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
pub(super) fn protect_secret(secret: &str, _target: &Path) -> Result<(), String> {
    security_framework::passwords::set_generic_password(KEYCHAIN_SERVICE, RUNTIME_ALIAS, secret.as_bytes())
        .map_err(|e| format!("保存 Runtime Key 到 macOS 钥匙串失败（错误码 {}）：{e}。可取消勾选保存密钥后重试；无需还原系统钥匙串。", e.code()))
}

#[cfg(target_os = "macos")]
pub(super) fn unprotect_secret(_target: &Path) -> Result<String, String> {
    let bytes = security_framework::passwords::get_generic_password(KEYCHAIN_SERVICE, RUNTIME_ALIAS)
        .map_err(|e| format!("读取 macOS 钥匙串失败，请允许 ChatX 访问钥匙串或重新输入 Runtime API Key：{e}"))?;
    String::from_utf8(bytes).map_err(|_| "钥匙串中的 Runtime Key 编码无效，请重新保存。".into())
}


enum StoredMonitorMasterKey {
    Missing,
    Value(String),
}

#[cfg(windows)]
fn load_monitor_master_key(
    app: &tauri::AppHandle,
) -> Result<StoredMonitorMasterKey, String> {
    let path = monitor_master_secret_path(app)?;
    if !path.is_file() {
        return Ok(StoredMonitorMasterKey::Missing);
    }
    unprotect_secret(&path)
        .map(StoredMonitorMasterKey::Value)
        .map_err(|e| e.replace("Runtime Key", "Monitor Master Key"))
}

#[cfg(target_os = "macos")]
fn load_monitor_master_key(
    _app: &tauri::AppHandle,
) -> Result<StoredMonitorMasterKey, String> {
    match security_framework::passwords::get_generic_password(
        MONITOR_MASTER_KEYCHAIN_SERVICE,
        MONITOR_MASTER_KEYCHAIN_ACCOUNT,
    ) {
        Ok(bytes) => String::from_utf8(bytes)
            .map(StoredMonitorMasterKey::Value)
            .map_err(|_| "Monitor Master Key 编码无效。".into()),
        Err(error) if error.code() == -25300 => {
            Ok(StoredMonitorMasterKey::Missing)
        }
        Err(error) => Err(format!(
            "读取 Monitor Master Key 失败：{error}"
        )),
    }
}

#[cfg(not(any(windows, target_os = "macos")))]
fn load_monitor_master_key(
    _app: &tauri::AppHandle,
) -> Result<StoredMonitorMasterKey, String> {
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

pub(super) fn ensure_monitor_master_key(app: &tauri::AppHandle) -> Result<[u8; 32], String> {
    match load_monitor_master_key(app)? {
        StoredMonitorMasterKey::Value(value) => {
            monitor_crypto::decode_master_key(&value)
                .map_err(|error| format!(
                    "Monitor Master Key 已损坏，拒绝静默轮换：{error}"
                ))
        }
        StoredMonitorMasterKey::Missing => {
            let key = monitor_crypto::generate_master_key()?;
            save_monitor_master_key(
                app,
                &monitor_crypto::encode_master_key(&key),
            )?;
            Ok(key)
        }
    }
}

#[cfg(windows)]
pub(super) fn save_relay_credentials(app: &tauri::AppHandle, credentials: &relay::RelayCredentials) -> Result<(), String> {
    let value = serde_json::to_string(credentials)
        .map_err(|e| format!("序列化 Relay Credential 失败：{e}"))?;
    protect_secret(&value, &relay_secret_path(app)?)
        .map_err(|e| e.replace("Runtime Key", "Relay Credential"))
}

#[cfg(target_os = "macos")]
pub(super) fn save_relay_credentials(_app: &tauri::AppHandle, credentials: &relay::RelayCredentials) -> Result<(), String> {
    let value = serde_json::to_string(credentials)
        .map_err(|e| format!("序列化 Relay Credential 失败：{e}"))?;
    security_framework::passwords::set_generic_password(
        RELAY_KEYCHAIN_SERVICE,
        RELAY_KEYCHAIN_ACCOUNT,
        value.as_bytes(),
    ).map_err(|e| format!("保存 Relay Credential 到 macOS 钥匙串失败：{e}"))
}

#[cfg(not(any(windows, target_os = "macos")))]
pub(super) fn save_relay_credentials(_app: &tauri::AppHandle, _credentials: &relay::RelayCredentials) -> Result<(), String> {
    Err("安全保存 Relay Credential 当前仅支持 Windows 和 macOS。".into())
}

#[cfg(windows)]
pub(super) fn read_relay_credentials(app: &tauri::AppHandle) -> Result<relay::RelayCredentials, String> {
    let value = unprotect_secret(&relay_secret_path(app)?)
        .map_err(|e| e.replace("Runtime Key", "Relay Credential"))?;
    serde_json::from_str(&value).map_err(|e| format!("Relay Credential 编码无效：{e}"))
}

#[cfg(target_os = "macos")]
pub(super) fn read_relay_credentials(_app: &tauri::AppHandle) -> Result<relay::RelayCredentials, String> {
    let bytes = security_framework::passwords::get_generic_password(
        RELAY_KEYCHAIN_SERVICE,
        RELAY_KEYCHAIN_ACCOUNT,
    ).map_err(|e| format!("读取 Relay Credential 失败：{e}"))?;
    let value = String::from_utf8(bytes).map_err(|_| "Relay Credential 编码无效。".to_string())?;
    serde_json::from_str(&value).map_err(|e| format!("Relay Credential 编码无效：{e}"))
}

#[cfg(not(any(windows, target_os = "macos")))]
pub(super) fn read_relay_credentials(_app: &tauri::AppHandle) -> Result<relay::RelayCredentials, String> {
    Err("安全读取 Relay Credential 当前仅支持 Windows 和 macOS。".into())
}


pub(super) fn clear_runtime_key(app: &tauri::AppHandle) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    clear_keychain_key()?;
    let path = secret_path(app)?;
    if path.exists() { fs::remove_file(path).map_err(|e| format!("清除 Runtime Key 失败：{e}"))?; }
    Ok(())
}

pub(super) fn key_storage() -> &'static str {
    if cfg!(windows) { "Windows DPAPI" }
    else if cfg!(target_os = "macos") { "macOS Keychain" }
    else { "session only" }
}

pub(super) fn load_runtime_key(app: &tauri::AppHandle, supplied: &str) -> Result<String, String> {
    if !supplied.trim().is_empty() { return Ok(supplied.trim().to_string()); }
    let path = secret_path(app)?;
    if !runtime_key_saved(app) { return Err("请输入 Runtime API Key，或先保存一个 Runtime Key。".into()); }
    let value = unprotect_secret(&path)?;
    if value.trim().is_empty() { return Err("已保存的 Runtime Key 为空。".into()); }
    Ok(value)
}


#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn keychain_save_update_read_and_clear() {
        let path = Path::new("unused");
        clear_keychain_key().unwrap();
        assert!(!keychain_key_saved());
        protect_secret("chatx-test-first", path).unwrap();
        assert!(keychain_key_saved());
        assert_eq!(
            unprotect_secret(path).unwrap(),
            "chatx-test-first",
        );
        protect_secret("chatx-test-updated", path).unwrap();
        assert_eq!(
            unprotect_secret(path).unwrap(),
            "chatx-test-updated",
        );
        clear_keychain_key().unwrap();
        assert!(!keychain_key_saved());
        assert!(unprotect_secret(path).is_err());
        clear_keychain_key().unwrap();
    }

    #[cfg(windows)]
    #[test]
    fn windows_dpapi_round_trip() {
        let path = std::env::temp_dir().join(format!(
            "chatx-dpapi-test-{}.txt",
            std::process::id(),
        ));
        let _ = fs::remove_file(&path);
        protect_secret("chatx-dpapi-regression", &path).unwrap();
        assert_eq!(
            unprotect_secret(&path).unwrap(),
            "chatx-dpapi-regression",
        );
        fs::remove_file(&path).unwrap();
    }
}
