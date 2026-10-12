// Platform-dependent system proxy discovery and command proxy environment.
// Kept distinct from the Tauri lifecycle entry point.
#[cfg(windows)]
use super::command_text;
#[cfg(target_os = "macos")]
use super::output_text;
#[cfg(target_os = "macos")]
use crate::process_utils::command_output;
use crate::settings::Settings;
use std::{process::Command, time::Duration};

pub(super) fn validate_proxy_url(value: &str) -> Result<String, String> {
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

pub(super) fn proxy_host_port(value: &str) -> Option<(String, u16)> {
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

pub(super) fn tcp_probe(host: &str, port: u16) -> bool {
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
pub(super) fn system_proxy_url() -> Result<Option<String>, String> {
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
pub(super) fn system_proxy_url() -> Result<Option<String>, String> {
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
pub(super) fn system_proxy_url() -> Result<Option<String>, String> {
    for key in ["HTTPS_PROXY", "https_proxy", "HTTP_PROXY", "http_proxy"] {
        if let Ok(value) = std::env::var(key) {
            if !value.trim().is_empty() {
                return Ok(Some(validate_proxy_url(&value)?));
            }
        }
    }
    Ok(None)
}

pub(super) fn effective_proxy_url(settings: &Settings) -> Result<Option<String>, String> {
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

pub(super) fn apply_proxy_env(command: &mut Command, settings: &Settings) -> Result<Option<String>, String> {
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
