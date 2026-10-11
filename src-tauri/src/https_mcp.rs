// Optional HTTPS MCP mode: local stdio bridge + WSS Relay or legacy SSH.
// It is independent of the legacy OpenAI Secure MCP Tunnel.
use crate::{
    app_state::AppState, load_settings, push_log, runtime_paths,
    connection_health::{self, HttpsConnectionHealth},
    connection_intent::{self, Transport},
    secrets::{ensure_https_oauth_master_key, rotate_https_oauth_master_key},
    settings::{update_settings, HttpsMcpSettings}, state_dir,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    io,
    path::Path,
    process::{Child, Command, Stdio},
    sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};
use tauri::State;
#[derive(Default, Clone)]
struct Snapshot {
    local_ready: bool,
    ssh_connected: bool,
    transport_connected: bool,
    remote_ready: bool,
    last_public_probe_at: u64,
    remote_last_error: String,
    last_error: String,
    restarts: u32,
}

#[derive(Default)]
pub(super) struct HttpsState {
    supervisor: Mutex<Option<Supervisor>>,
    status: Arc<Mutex<Snapshot>>,
    auto_reconnect: Arc<AtomicBool>,
}

struct Supervisor {
    stop: Arc<AtomicBool>,
    worker: JoinHandle<()>,
}

fn derive_secret(master: &[u8; 32], label: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(b"chatx-https-mcp-v1:");
    digest.update(label.as_bytes());
    digest.update(master);
    digest.finalize().iter().map(|part| format!("{part:02x}")).collect()
}

fn validate(settings: &HttpsMcpSettings) -> Result<(), String> {
    let url = settings.base_url.trim();
    let domain = url.strip_prefix("https://").ok_or("HTTPS MCP 地址必须以 https:// 开头")?;
    if domain.is_empty() || domain.len() > 253 || domain.starts_with('-')
        || !domain.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.') {
        return Err("HTTPS MCP 地址必须是没有端口和路径的 HTTPS 域名".into());
    }
    if !matches!(settings.transport.as_str(), "ssh" | "wss") {
        return Err("HTTPS MCP 传输模式无效".into());
    }
    if settings.transport == "ssh" {
        let host = settings.ssh_host.trim();
        if host.is_empty() || host.len() > 253 || host.starts_with('-')
            || !host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.') {
            return Err("SSH 主机无效".into());
        }
        let user = settings.ssh_user.trim();
        if user.is_empty() || user.len() > 64 ||
            !user.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-') ||
            user.starts_with('-') {
            return Err("SSH 用户名无效".into());
        }
        if settings.ssh_port == 0 || settings.remote_port < 1024 {
            return Err("SSH 端口无效".into());
        }
    }
    if settings.local_port < 1024 {
        return Err("本机 MCP 端口无效".into());
    }
    validate_authorization_lifetime(settings.authorization_ttl_hours)?;
    Ok(())
}

pub(super) fn validate_authorization_lifetime(hours: Option<u32>) -> Result<(), String> {
    if matches!(hours, Some(0 | 87601..)) {
        return Err("OAuth 授权期限必须是 1～87600 小时，或选择不限时间".into());
    }
    Ok(())
}

fn redirect_log(dir: &Path, name: &str) -> io::Result<Stdio> {
    OpenOptions::new().create(true).append(true).open(dir.join(name)).map(Stdio::from)
}

struct ManagedChild {
    process: Child,
    #[cfg(windows)]
    _job: crate::windows_process_job::KillOnCloseJob,
}

fn managed_spawn(command: &mut Command, name: &str) -> Result<ManagedChild, String> {
    #[cfg(windows)]
    {
        let (process, job) = crate::windows_process_job::KillOnCloseJob::spawn(command)
            .map_err(|e| format!("启动 {name} 并加入 Windows 进程组失败：{e}"))?;
        Ok(ManagedChild { process, _job: job })
    }
    #[cfg(not(windows))]
    {
        command.spawn().map(|process| ManagedChild { process })
            .map_err(|e| format!("启动 {name} 失败：{e}"))
    }
}

fn start_bridge(
    settings: &HttpsMcpSettings,
    paths: &crate::app_state::RuntimePaths,
    home: &Path,
    log_dir: &Path,
    password: &str,
    signing: &str,
    instance_nonce: &str,
) -> Result<ManagedChild, String> {
    let script = paths.root.join("https-mcp-bridge.mjs");
    if !script.is_file() {
        return Err("缺少 https-mcp-bridge.mjs；请重新运行 desktop:prepare 或更新 ChatX 安装包".into());
    }
    #[cfg(target_os = "macos")]
    let mut command = {
        let wrapper = paths.root.join("https-mcp-parent-watch.mjs");
        if !wrapper.is_file() { return Err("缺少 macOS HTTPS MCP 子进程监护脚本".into()); }
        let mut wrapped = Command::new(&paths.node);
        wrapped.arg(wrapper).arg(std::process::id().to_string())
            .arg(&paths.node).arg(&script);
        wrapped
    };
    #[cfg(not(target_os = "macos"))]
    let mut command = {
        let mut direct = Command::new(&paths.node);
        direct.arg(script);
        direct
    };
    command.stdin(Stdio::null()).stdout(Stdio::null())
        .stderr(redirect_log(log_dir, "https-mcp-bridge.log").map_err(|e| e.to_string())?)
        .env("CHATX_MCP_ISSUER", &settings.base_url)
        .env("CHATX_MCP_PORT", settings.local_port.to_string())
        .env("CHATX_MCP_LOGIN_PASSWORD", password)
        .env("CHATX_MCP_SIGNING_KEY", signing)
        .env("CHATX_MCP_INSTANCE_NONCE", instance_nonce)
        .env("CHATX_MCP_APP_VERSION", crate::APP_VERSION)
        .env("CHATX_MCP_LAUNCHER", &paths.launcher)
        .env("CHATX_MCP_ENTRY", &paths.desktop_commander)
        .env("CHATX_MCP_NODE", &paths.node)
        .env("CHATX_MCP_HOME", home)
        .env("CHATX_MCP_AUTH_STORE", log_dir.join("https-oauth-authorizations"))
        .env("CHATX_MCP_SETTINGS_PATH", log_dir.join("settings.json"))
        .env_remove("CHATX_TUNNEL_RUNTIME_KEY");
    managed_spawn(&mut command, "HTTPS MCP Bridge")
}

fn start_forward(
    settings: &HttpsMcpSettings, log_dir: &Path, paths: &crate::app_state::RuntimePaths,
) -> Result<ManagedChild, String> {
    #[cfg(not(target_os = "macos"))]
    let _ = paths;
    let bind = format!(
        "127.0.0.1:{}:127.0.0.1:{}",
        settings.remote_port, settings.local_port
    );
    let destination = format!("{}@{}", settings.ssh_user, settings.ssh_host);
    #[cfg(target_os = "macos")]
    let mut command = {
        let wrapper = paths.root.join("https-mcp-parent-watch.mjs");
        if !wrapper.is_file() { return Err("缺少 macOS SSH 子进程监护脚本".into()); }
        let mut wrapped = Command::new(&paths.node);
        wrapped.arg(wrapper).arg(std::process::id().to_string()).arg("ssh");
        wrapped
    };
    #[cfg(not(target_os = "macos"))]
    let mut command = Command::new("ssh");
    command.args([
        "-N", "-T", "-o", "BatchMode=yes", "-o", "StrictHostKeyChecking=yes",
        "-o", "ExitOnForwardFailure=yes", "-o", "ConnectTimeout=8",
        "-o", "ServerAliveInterval=20", "-o", "ServerAliveCountMax=3",
        "-p", &settings.ssh_port.to_string(), "-R", &bind, &destination,
    ]);
    // SSH itself needs the real account identity/known_hosts, not the isolated DC HOME.
    if cfg!(windows) {
        if let Ok(home) = std::env::var("USERPROFILE") {
            command.env("USERPROFILE", home);
        }
    }
    #[cfg(target_os = "macos")]
    command.env("HOME", crate::macos_account_home()?);
    command.stdin(Stdio::null()).stdout(Stdio::null())
        .stderr(redirect_log(log_dir, "https-mcp-ssh.log").map_err(|e| e.to_string())?);
    managed_spawn(&mut command, "SSH 反向转发")
}

fn is_alive(child: &mut Option<ManagedChild>) -> bool {
    if let Some(handle) = child {
        match handle.process.try_wait() {
            Ok(None) => return true,
            Ok(Some(_)) => {}
            Err(_) => {}
        }
    }
    if let Some(mut previous) = child.take() { let _ = previous.process.wait(); }
    false
}

fn stop_wss(client: &mut Option<crate::https_mcp_wss::WssClient>) {
    if let Some(client) = client.take() { client.stop(); }
}

fn kill_child(child: &mut Option<ManagedChild>) {
    if let Some(mut current) = child.take() {
        #[cfg(target_os = "macos")]
        {
            // The macOS child is a watchdog. SIGKILL would skip its cleanup
            // handler and orphan its detached SSH/Node process group.
            let pid = current.process.id() as i32;
            // SAFETY: pid came from an owned Child handle.
            unsafe { libc::kill(pid, libc::SIGTERM); }
            for _ in 0..25 {
                if current.process.try_wait().ok().flatten().is_some() { return; }
                thread::sleep(Duration::from_millis(100));
            }
        }
        let _ = current.process.kill();
        let _ = current.process.wait();
    }
}

fn probe_local_bridge(
    client: &reqwest::blocking::Client, port: u16, nonce: &str,
) -> bool {
    if nonce.is_empty() { return false; }
    client.get(format!("http://127.0.0.1:{port}/healthz"))
        .header("x-chatx-instance-nonce", nonce)
        .send().ok().filter(|r| r.status() == reqwest::StatusCode::OK)
        .and_then(|r| r.json::<Value>().ok())
        .is_some_and(|value| value["status"] == "ready" && value["instance"] == nonce)
}

fn probe_public_mcp(client: &reqwest::blocking::Client, issuer: &str) -> Result<(), String> {
    let metadata = client.get(format!("{issuer}/.well-known/oauth-authorization-server"))
        .send().map_err(|e| format!("HTTPS 元数据探测失败：{e}"))?;
    if metadata.status() != reqwest::StatusCode::OK {
        return Err(format!("HTTPS OAuth 元数据返回 {}", metadata.status()));
    }
    let response: Value = metadata.json().map_err(|e| format!("OAuth 元数据格式错误：{e}"))?;
    if response["issuer"].as_str() != Some(issuer) {
        return Err("OAuth 元数据的 issuer 与当前配置不一致".into());
    }
    let mcp = client.post(format!("{issuer}/mcp")).header("content-type", "application/json")
        .body("{}").send().map_err(|e| format!("HTTPS MCP 探测失败：{e}"))?;
    if mcp.status() != reqwest::StatusCode::UNAUTHORIZED
        || !mcp.headers().get(reqwest::header::WWW_AUTHENTICATE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.contains("resource_metadata")) {
        return Err(format!("HTTPS MCP 入口未按预期要求 OAuth 认证（{}）", mcp.status()));
    }
    Ok(())
}

fn should_launch(attempted: bool, retry_enabled: bool) -> bool {
    !attempted || retry_enabled
}

fn run(
    stop: Arc<AtomicBool>, status: Arc<Mutex<Snapshot>>,
    settings: HttpsMcpSettings, paths: crate::app_state::RuntimePaths,
    home: std::path::PathBuf, log_dir: std::path::PathBuf,
    password: String, signing: String,
    auto_reconnect: Arc<AtomicBool>,
    relay_credentials: Option<crate::relay::RelayCredentials>,
) {
    let mut local: Option<ManagedChild> = None;
    // Even without retry, each manually started session gets one attempt per
    // transport. Runtime toggling on permits further attempts.
    let mut bridge_attempted = false;
    let mut forward_attempted = false;
    let mut bridge_nonce = String::new();
    let mut forward: Option<ManagedChild> = None;
    let mut wss: Option<crate::https_mcp_wss::WssClient> = None;
    let mut retries = 0u32;
    let mut last_error = String::new();
    let mut public_ready = false;
    let mut public_probe_error = String::new();
    let mut last_public_probe_at = 0u64;
    let mut last_probe = Instant::now() - Duration::from_secs(30);
    let loopback = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_millis(500))
        .build();
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(6))
        .redirect(reqwest::redirect::Policy::none())
        .build();
    while !stop.load(Ordering::SeqCst) {
        if !is_alive(&mut local)
            && should_launch(bridge_attempted, auto_reconnect.load(Ordering::SeqCst))
        {
            bridge_attempted = true;
            forward_attempted = false;
            // A restarted bridge invalidates the upstream stdio session.
            public_ready = false;
            last_probe = Instant::now() - Duration::from_secs(30);
            kill_child(&mut forward);
            stop_wss(&mut wss);
            match crate::monitor_server::generate_monitor_token().and_then(|nonce| {
                start_bridge(&settings, &paths, &home, &log_dir, &password, &signing, &nonce)
                    .map(|child| (child, nonce))
            }) {
                Ok((child, nonce)) => {
                    bridge_nonce = nonce;
                    local = Some(child);
                    last_error.clear();
                }
                Err(error) => { last_error = error; retries = retries.saturating_add(1); }
            }
        }
        let ready = is_alive(&mut local)
            && loopback.as_ref().is_ok_and(|client|
                probe_local_bridge(client, settings.local_port, &bridge_nonce));
        // WSS reconnects within its own managed client; the supervisor must
        // not create duplicate sockets while the client is reconnecting.
        if wss.as_ref().is_some_and(|client| !client.alive()) {
            stop_wss(&mut wss);
        }
        let needs_transport = if settings.transport == "wss" {
            wss.is_none()
        } else {
            !is_alive(&mut forward)
        };
        if ready && needs_transport
            && should_launch(forward_attempted, auto_reconnect.load(Ordering::SeqCst))
        {
            forward_attempted = true;
            public_ready = false;
            last_probe = Instant::now() - Duration::from_secs(30);
            if settings.transport == "wss" {
                if let Some(credentials) = relay_credentials.clone() {
                    wss = Some(crate::https_mcp_wss::WssClient::start(
                        settings.base_url.clone(), settings.local_port, credentials,
                        auto_reconnect.clone(),
                    ));
                    last_error.clear();
                } else {
                    last_error = "缺少 HTTPS MCP WSS 凭据".into();
                    retries = retries.saturating_add(1);
                }
            } else {
                match start_forward(&settings, &log_dir, &paths) {
                    Ok(child) => { forward = Some(child); last_error.clear(); }
                    Err(error) => { last_error = error; retries = retries.saturating_add(1); }
                }
            }
        }
        if !ready { kill_child(&mut forward); stop_wss(&mut wss); }
        let transport_connected = ready && if settings.transport == "wss" {
            wss.as_ref().is_some_and(|client| client.connected())
        } else { is_alive(&mut forward) };
        if !transport_connected {
            public_ready = false;
            if settings.transport == "wss" {
                if let Some(error) = wss.as_ref().and_then(|client| client.last_error()) {
                    last_error = format!("WSS Relay: {error}");
                }
            }
        } else if settings.transport == "wss" {
            last_error.clear();
        }
        if transport_connected && last_probe.elapsed() >= Duration::from_secs(15) {
            last_probe = Instant::now();
            last_public_probe_at = crate::timestamp_ms();
            match &client {
                Ok(client) => match probe_public_mcp(client, &settings.base_url) {
                    Ok(()) => { public_ready = true; public_probe_error.clear(); }
                    Err(error) => { public_ready = false; public_probe_error = error; }
                },
                Err(error) => { public_ready = false; public_probe_error = error.to_string(); }
            }
        }
        if let Ok(mut snapshot) = status.lock() {
            snapshot.local_ready = ready;
            snapshot.ssh_connected = settings.transport == "ssh" && transport_connected;
            snapshot.transport_connected = transport_connected;
            snapshot.remote_ready = transport_connected && public_ready;
            snapshot.last_public_probe_at = last_public_probe_at;
            snapshot.remote_last_error.clone_from(&public_probe_error);
            snapshot.last_error.clone_from(&last_error);
            snapshot.restarts = retries;
        }
        for _ in 0..20 {
            if stop.load(Ordering::SeqCst) { break; }
            thread::sleep(Duration::from_millis(100));
        }
    }
    kill_child(&mut forward);
    stop_wss(&mut wss);
    kill_child(&mut local);
    if let Ok(mut snapshot) = status.lock() {
        snapshot.local_ready = false;
        snapshot.ssh_connected = false;
        snapshot.transport_connected = false;
        snapshot.remote_ready = false;
    }
}

fn stop_inner(state: &AppState) {
    if let Ok(mut lock) = state.https_mcp.supervisor.lock() {
        if let Some(supervisor) = lock.take() {
            supervisor.stop.store(true, Ordering::SeqCst);
            let _ = supervisor.worker.join();
        }
    }
}

pub(super) fn stop_on_exit(state: &AppState) {
    stop_inner(state);
}

pub(super) fn local_bridge_alive(state: &AppState) -> bool {
    state.https_mcp.status.lock().is_ok_and(|status| status.local_ready)
}

pub(super) fn monitor_public_connection(
    app: &tauri::AppHandle, settings: &HttpsMcpSettings, state: &AppState,
) -> (HttpsConnectionHealth, u64, u32) {
    let snapshot = state.https_mcp.status.lock()
        .map(|value| value.clone()).unwrap_or_default();
    let allowed = connection_intent::restore_allowed(app, Transport::HttpsMcp)
        .unwrap_or(false);
    let health = connection_health::https_connection_health(
        settings.enabled, allowed, snapshot.local_ready, snapshot.transport_connected,
        snapshot.remote_ready, snapshot.last_public_probe_at, crate::timestamp_ms(),
    );
    (health, snapshot.last_public_probe_at, snapshot.restarts)
}

fn start_inner(app: &tauri::AppHandle, state: &AppState, settings: HttpsMcpSettings)
    -> Result<(), String> {
    validate(&settings)?;
    let mut lock = state.https_mcp.supervisor.lock()
        .map_err(|_| "HTTPS MCP 连接管理锁已损坏".to_string())?;
    if lock.is_some() { return Ok(()); }
    let relay_credentials = if settings.transport == "wss" {
        let credentials = crate::secrets::read_relay_credentials(app)
            .map_err(|_| "WSS 需要 Relay Desktop 凭据：请先在网络设置注册 Relay".to_string())?;
        if crate::relay::normalized_base_url(&credentials.base_url)?
            != crate::relay::normalized_base_url(&settings.base_url)? {
            return Err("Relay 注册域名与 HTTPS MCP 域名不一致，请先为此域名注册 Relay".into());
        }
        Some(credentials)
    } else { None };
    let paths = runtime_paths(app)?;
    let dir = state_dir(app)?;
    // Run an independent Desktop Commander instance; do not race the Tunnel
    // launcher writing activity.json and tool-history.jsonl in its own HOME.
    let home = dir.join("https-desktop-commander-home");
    fs::create_dir_all(&home).map_err(|e| e.to_string())?;
    let master = ensure_https_oauth_master_key(app)?;
    let password = derive_secret(&master, "login");
    let signing = derive_secret(&master, "tokens");
    let stop = Arc::new(AtomicBool::new(false));
    let stopper = stop.clone();
    let status = state.https_mcp.status.clone();
    state.https_mcp.auto_reconnect.store(settings.auto_reconnect, Ordering::SeqCst);
    let auto_reconnect = state.https_mcp.auto_reconnect.clone();
    let worker = thread::spawn(move || {
        run(stopper, status, settings, paths, home, dir, password, signing,
            auto_reconnect, relay_credentials);
    });
    *lock = Some(Supervisor { stop, worker });
    push_log(state, if load_settings(app).https_mcp.transport == "wss" {
        "HTTPS MCP 已启用，本地 Bridge 与原生 WSS Relay 正在启动"
    } else {
        "HTTPS MCP 已启用，本地 Bridge 与 SSH 反向隧道正在启动"
    });
    Ok(())
}

pub(super) fn restore(app: &tauri::AppHandle, state: &AppState) {
    let settings = load_settings(app).https_mcp;
    let restore_allowed = connection_intent::restore_allowed(app, Transport::HttpsMcp)
        .unwrap_or_else(|error| {
            push_log(state, format!("HTTPS MCP 停止标记不可用，拒绝自动恢复：{error}"));
            false
        });
    // Compatibility: for pre-mutual-exclusion settings with both flags set,
    // HTTPS wins. Tunnel recovery independently suppresses its own intent.
    if settings.enabled && restore_allowed {
        // A pre-upgrade Tunnel daemon may survive a failed startup cleanup.
        // Never bring HTTPS online until we can confirm the old runtime is gone.
        let tunnel_absent = runtime_paths(app).map(|paths| {
            let observation = crate::tunnel_status::runtime_status(app, &paths);
            !observation.runtime_alive && !observation.active
        }).unwrap_or(false);
        if !tunnel_absent {
            push_log(state, "HTTPS MCP 自动恢复已阻止：残留 Tunnel runtime 未确认停止");
            return;
        }
        if let Err(error) = start_inner(app, state, settings) {
            push_log(state, format!("HTTPS MCP 恢复失败：{error}"));
        }
    }
}

pub(super) fn status_summary(app: &tauri::AppHandle, settings: &HttpsMcpSettings, state: &AppState) -> Value {
    let snapshot = state.https_mcp.status.lock().map(|v| v.clone()).unwrap_or_default();
    let (health, _, _) = monitor_public_connection(app, settings, state);
    json!({
        "enabled": health.enabled,
        "connected": health.connected,
        "state": health.state,
        "autoReconnect": settings.auto_reconnect,
        "manualStopBlocked": health.manual_stop_blocked,
        "baseUrl": settings.base_url,
        "endpoint": format!("{}/mcp", settings.base_url.trim_end_matches('/')),
        "sshHost": settings.ssh_host, "sshUser": settings.ssh_user, "sshPort": settings.ssh_port,
        "localPort": settings.local_port, "remotePort": settings.remote_port,
        "authorizationTtlHours": settings.authorization_ttl_hours,
        "transport": settings.transport,
        "localReady": health.local_ready, "sshConnected": snapshot.ssh_connected,
        "transportConnected": health.transport_connected,
        "remoteReady": health.remote_ready,
        "lastPublicProbeAt": snapshot.last_public_probe_at,
        "remoteLastError": snapshot.remote_last_error,
        "restarts": snapshot.restarts, "lastError": snapshot.last_error,
    })
}

#[tauri::command]
pub(super) fn get_https_mcp_status(app: tauri::AppHandle, state: State<'_, AppState>) -> Value {
    status_summary(&app, &load_settings(&app).https_mcp, state.inner())
}

#[tauri::command]
pub(super) fn start_https_mcp(
    app: tauri::AppHandle, state: State<'_, AppState>, config: HttpsMcpSettings,
) -> Result<Value, String> {
    let _connection = state.connection_operation.lock()
        .map_err(|_| "连接操作锁已损坏".to_string())?;
    if state.quitting.load(Ordering::SeqCst) { return Err("ChatX 正在退出，拒绝启动 HTTPS MCP".into()); }
    // Connection startup may never commit unsaved OAuth policy/auto-reconnect
    // UI changes. Those preferences have independent settings commands.
    let mut config = config;
    let saved = load_settings(&app);
    config.authorization_ttl_hours = saved.https_mcp.authorization_ttl_hours;
    config.auto_reconnect = saved.https_mcp.auto_reconnect;
    validate(&config)?;
    // Switching modes is fail-closed: persist the old transport's Stop
    // before its runtime is stopped, and never start HTTPS until it is gone.
    connection_intent::mark_stopped(&app, Transport::Tunnel)?;
    stop_inner(state.inner());
    let previous = load_settings(&app);
    crate::connection_commands::stop_tunnel_without_saving(&app, state.inner(), &previous)?;
    update_settings(&app, |settings| {
        settings.connection_intent = false;
        settings.https_mcp.enabled = false;
        Ok(())
    })?;
    start_inner(&app, state.inner(), config.clone())?;
    if let Err(error) = update_settings(&app, |settings| {
        settings.connection_intent = false;
        settings.https_mcp = HttpsMcpSettings { enabled: true, ..config.clone() };
        Ok(())
    }) {
        stop_inner(state.inner());
        return Err(error);
    }
    if let Err(error) = connection_intent::clear_stopped(&app, Transport::HttpsMcp) {
        stop_inner(state.inner());
        let _ = update_settings(&app, |settings| {
            settings.https_mcp.enabled = false;
            Ok(())
        });
        return Err(error);
    }
    drop(_connection);
    Ok(get_https_mcp_status(app, state))
}

pub(super) fn stop_requested(app: &tauri::AppHandle, state: &AppState) -> Result<(), String> {
    let _connection = state.connection_operation.lock()
        .map_err(|_| "连接操作锁已损坏".to_string())?;
    let marker = connection_intent::mark_stopped(app, Transport::HttpsMcp);
    stop_inner(state);
    let saved = update_settings(app, |settings| {
        settings.https_mcp.enabled = false;
        Ok(())
    });
    if let Err(error) = &saved {
        push_log(state, format!("HTTPS MCP 已停止，但保存停止状态失败：{error}"));
    }
    if let Err(error) = marker {
        push_log(state, format!("HTTPS MCP 停止标记保存失败：{error}"));
        saved?;
        return Err(error);
    }
    saved?;
    push_log(state, "HTTPS MCP 已手动停止");
    Ok(())
}

#[tauri::command]
pub(super) fn stop_https_mcp(
    app: tauri::AppHandle, state: State<'_, AppState>,
) -> Result<Value, String> {
    stop_requested(&app, state.inner())?;
    Ok(get_https_mcp_status(app, state))
}

#[tauri::command]
pub(super) fn set_https_mcp_auto_reconnect(
    app: tauri::AppHandle, state: State<'_, AppState>, enabled: bool,
) -> Result<Value, String> {
    let _connection = state.connection_operation.lock()
        .map_err(|_| "连接操作锁已损坏".to_string())?;
    update_settings(&app, |settings| {
        settings.https_mcp.auto_reconnect = enabled;
        Ok(())
    })?;
    state.https_mcp.auto_reconnect.store(enabled, Ordering::SeqCst);
    push_log(state.inner(), if enabled {
        "已开启 HTTPS MCP 断线自动重连"
    } else {
        "已关闭 HTTPS MCP 断线自动重连；下次仍可手动启动"
    });
    drop(_connection);
    Ok(get_https_mcp_status(app, state))
}

#[tauri::command]
pub(super) fn rotate_https_mcp_login_password(
    app: tauri::AppHandle, state: State<'_, AppState>,
) -> Result<String, String> {
    let _connection = state.connection_operation.lock()
        .map_err(|_| "连接操作锁已损坏".to_string())?;
    if state.quitting.load(Ordering::SeqCst) { return Err("ChatX 正在退出".into()); }
    // Fail closed: terminate the active remote bridge before changing secrets.
    stop_inner(state.inner());
    // Invalidate grants/codes before installing the new signing key.
    crate::https_mcp_authorizations::revoke_all_https_mcp_authorizations(app.clone())?;
    let master = rotate_https_oauth_master_key(&app)?;
    let settings = load_settings(&app).https_mcp;
    if settings.enabled && connection_intent::restore_allowed(&app, Transport::HttpsMcp)? {
        start_inner(&app, state.inner(), settings)?;
    }
    push_log(state.inner(), "HTTPS OAuth 密码已独立轮换；原有授权全部失效，请重新授权");
    Ok(derive_secret(&master, "login"))
}

#[tauri::command]
pub(super) fn get_https_mcp_login_password(app: tauri::AppHandle)
    -> Result<String, String> {
    let master = ensure_https_oauth_master_key(&app)?;
    Ok(derive_secret(&master, "login"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_insecure_or_malformed_destinations() {
        let mut cfg = HttpsMcpSettings::default();
        cfg.transport = "ssh".into();
        cfg.base_url = "http://newrow.fun".into();
        assert!(validate(&cfg).is_err());
        cfg.base_url = "https://newrow.fun".into();
        cfg.ssh_host = "-oProxyCommand=bad".into();
        assert!(validate(&cfg).is_err());
        cfg.ssh_host = "198.51.100.42".into();
        cfg.ssh_user = "chatx-mcp".into();
        assert!(validate(&cfg).is_ok());
    }
    #[test]
    fn https_reconnect_disabled_attempts_each_process_once_per_start() {
        assert!(should_launch(false, false));
        assert!(!should_launch(true, false));
        assert!(should_launch(true, true));
    }
    #[test]
    fn new_installs_use_wss_but_legacy_configs_keep_ssh() {
        let fresh = HttpsMcpSettings::default();
        assert_eq!(fresh.transport, "wss");
        let mut value = serde_json::to_value(fresh).unwrap();
        value.as_object_mut().unwrap().remove("transport");
        let legacy: HttpsMcpSettings = serde_json::from_value(value).unwrap();
        assert_eq!(legacy.transport, "ssh");
    }

    #[test]
    fn credential_domains_are_distinct() {
        let seed = [12u8; 32];
        assert_ne!(derive_secret(&seed, "login"), derive_secret(&seed, "tokens"));
    }
    #[test]
    fn authorization_lifetime_defaults_and_unlimited_are_distinct() {
        let mut value = serde_json::to_value(HttpsMcpSettings::default()).unwrap();
        value.as_object_mut().unwrap().remove("authorizationTtlHours");
        value.as_object_mut().unwrap().remove("autoReconnect");
        let legacy: HttpsMcpSettings = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(legacy.authorization_ttl_hours, Some(168));
        assert!(legacy.auto_reconnect, "Upgraded HTTPS must retain historical auto-retry behavior");
        value["authorizationTtlHours"] = Value::Null;
        let unlimited: HttpsMcpSettings = serde_json::from_value(value).unwrap();
        assert_eq!(unlimited.authorization_ttl_hours, None);
        assert!(validate_authorization_lifetime(None).is_ok());
        assert!(validate_authorization_lifetime(Some(1)).is_ok());
        assert!(validate_authorization_lifetime(Some(87600)).is_ok());
        assert!(validate_authorization_lifetime(Some(0)).is_err());
        assert!(validate_authorization_lifetime(Some(87601)).is_err());
    }
}
