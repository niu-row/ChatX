use crate::monitor_crypto;
use chatx_relay_protocol::{
    device_capabilities, DesktopRegisterRequest, DesktopRegisterResponse, DesktopServerMessage,
    DesktopWsMessage, DeviceAuthorizeRequest, EncryptedControlPayload, PairingRouteOpenRequest,
    PairingServerMessage, PairingWsMessage, PROTOCOL_VERSION,
};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    thread::{self, JoinHandle},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
#[cfg(target_os = "macos")]
use std::process::Command;

use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::{header::AUTHORIZATION, HeaderValue},
        Message,
    },
};

const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(10);
const CLIENT_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
const CLIENT_IDLE_TIMEOUT: Duration = Duration::from_secs(12);
const RECOVERY_PROBE_INTERVAL: Duration = Duration::from_secs(2);
const RECOVERY_PROBE_TIMEOUT: Duration = Duration::from_secs(3);
const RECONNECT_DELAYS: [u64; 5] = [1, 2, 5, 10, 30];

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

fn snapshot_session_id() -> String {
    format!("s_{}_{}", std::process::id(), now_ms())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RelaySettings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub base_url: String,
}

impl Default for RelaySettings {
    fn default() -> Self {
        Self { enabled: false, base_url: String::new() }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayCredentials {
    pub base_url: String,
    pub desktop_id: String,
    pub desktop_token: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayStatus {
    pub enabled: bool,
    pub configured: bool,
    pub connected: bool,
    pub connecting: bool,
    pub desktop_id: Option<String>,
    pub session_id: Option<String>,
    pub connected_at: Option<u64>,
    pub last_heartbeat_at: Option<u64>,
    pub reconnect_attempt: u32,
    pub last_error: String,
}

impl Default for RelayStatus {
    fn default() -> Self {
        Self {
            enabled: false,
            configured: false,
            connected: false,
            connecting: false,
            desktop_id: None,
            session_id: None,
            connected_at: None,
            last_heartbeat_at: None,
            reconnect_attempt: 0,
            last_error: String::new(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct SnapshotSource {
    pub device_id: String,
    pub plaintext: Vec<u8>,
}

pub type SnapshotProvider = Arc<dyn Fn() -> Vec<SnapshotSource> + Send + Sync>;
pub type PairingHandler = Arc<
    dyn Fn(PairingWsMessage) -> Result<PairingServerMessage, String> + Send + Sync
>;
pub type RevokeHandler = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;
pub type ControlHandler = Arc<
    dyn Fn(&str, EncryptedControlPayload) -> Result<EncryptedControlPayload, String> + Send + Sync
>;

pub struct RelayClientHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Drop for RelayClientHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
pub fn validate(settings: &RelaySettings) -> Result<(), String> {
    if !settings.enabled {
        return Ok(());
    }
    normalized_base_url(&settings.base_url).map(|_| ())
}

pub fn normalized_base_url(value: &str) -> Result<String, String> {
    let value = value.trim().trim_end_matches('/');
    if value.is_empty() || value.chars().any(char::is_whitespace) || value.contains('@') {
        return Err("Relay URL 无效。".into());
    }
    let secure = value.starts_with("https://");
    let loopback = value.starts_with("http://127.0.0.1")
        || value.starts_with("http://localhost")
        || value.starts_with("http://[::1]");
    if !secure && !loopback {
        return Err("Relay 必须使用 https://；仅本机开发允许 loopback http://。".into());
    }
    Ok(value.to_string())
}

fn websocket_url(base_url: &str, path: &str) -> Result<String, String> {
    let base = normalized_base_url(base_url)?;
    if let Some(rest) = base.strip_prefix("https://") {
        Ok(format!("wss://{rest}{path}"))
    } else if let Some(rest) = base.strip_prefix("http://") {
        Ok(format!("ws://{rest}{path}"))
    } else {
        Err("Relay URL scheme 无效。".into())
    }
}

fn client() -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| format!("创建 Relay HTTP 客户端失败：{error}"))
}

pub fn register_desktop(
    settings: &RelaySettings,
    bootstrap_token: &str,
    desktop_id: &str,
    device_name: &str,
    app_version: &str,
    platform: &str,
) -> Result<RelayCredentials, String> {
    let base = normalized_base_url(&settings.base_url)?;
    let token = bootstrap_token.trim();
    if token.is_empty() {
        return Err("Relay 一次性注册凭据为空。".into());
    }
    let response = client()?
        .post(format!("{base}/v1/desktops/register"))
        .bearer_auth(token)
        .json(&DesktopRegisterRequest {
            desktop_id: desktop_id.to_string(),
            device_name: device_name.to_string(),
            app_version: app_version.to_string(),
            platform: platform.to_string(),
        })
        .send()
        .map_err(|error| format!("Relay 注册失败：{error}"))?;
    if !response.status().is_success() {
        return Err(format!("Relay 注册失败：HTTP {}", response.status()));
    }
    let registered = response.json::<DesktopRegisterResponse>()
        .map_err(|error| format!("Relay 注册响应无效：{error}"))?;
    Ok(RelayCredentials {
        base_url: base,
        desktop_id: registered.desktop_id,
        desktop_token: registered.desktop_token,
    })
}

pub fn authorize_device(
    settings: &RelaySettings,
    credentials: &RelayCredentials,
    device_id: &str,
    relay_device_token: &str,
) -> Result<(), String> {
    let base = normalized_base_url(&settings.base_url)?;
    let response = client()?
        .put(format!(
            "{base}/v1/desktops/{}/devices/{device_id}",
            credentials.desktop_id
        ))
        .bearer_auth(&credentials.desktop_token)
        .json(&DeviceAuthorizeRequest {
            token_hash: crate::monitor_server::token_hash_hex(relay_device_token),
        })
        .send()
        .map_err(|error| format!("Relay 设备授权失败：{error}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("Relay 设备授权失败：HTTP {}", response.status()))
    }
}

pub fn revoke_device(
    settings: &RelaySettings,
    credentials: &RelayCredentials,
    device_id: &str,
) -> Result<(), String> {
    let base = normalized_base_url(&settings.base_url)?;
    let response = client()?
        .delete(format!(
            "{base}/v1/desktops/{}/devices/{device_id}",
            credentials.desktop_id
        ))
        .bearer_auth(&credentials.desktop_token)
        .send()
        .map_err(|error| format!("Relay 设备撤销失败：{error}"))?;
    if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
        Ok(())
    } else {
        Err(format!("Relay 设备撤销失败：HTTP {}", response.status()))
    }
}

pub fn open_pairing_route(
    settings: &RelaySettings,
    credentials: &RelayCredentials,
    pairing_id: &str,
    route_token: &str,
    expires_at: u64,
) -> Result<(), String> {
    let base = normalized_base_url(&settings.base_url)?;
    let response = client()?
        .put(format!(
            "{base}/v1/desktops/{}/pairings/{pairing_id}",
            credentials.desktop_id
        ))
        .bearer_auth(&credentials.desktop_token)
        .json(&PairingRouteOpenRequest {
            token_hash: crate::monitor_server::token_hash_hex(route_token),
            expires_at,
        })
        .send()
        .map_err(|error| format!("Relay Pairing route 创建失败：{error}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!(
            "Relay Pairing route 创建失败：HTTP {}",
            response.status()
        ))
    }
}

pub fn delete_pairing_route(
    settings: &RelaySettings,
    credentials: &RelayCredentials,
    pairing_id: &str,
) -> Result<(), String> {
    let base = normalized_base_url(&settings.base_url)?;
    let response = client()?
        .delete(format!(
            "{base}/v1/desktops/{}/pairings/{pairing_id}",
            credentials.desktop_id
        ))
        .bearer_auth(&credentials.desktop_token)
        .send()
        .map_err(|error| format!("Relay Pairing route 删除失败：{error}"))?;
    if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
        Ok(())
    } else {
        Err(format!(
            "Relay Pairing route 删除失败：HTTP {}",
            response.status()
        ))
    }
}

pub fn pairing_ws_url(
    settings: &RelaySettings,
    credentials: &RelayCredentials,
    pairing_id: &str,
) -> Result<String, String> {
    websocket_url(
        &settings.base_url,
        &format!(
            "/v1/ws/pair?desktopId={}&pairingId={pairing_id}",
            credentials.desktop_id
        ),
    )
}

pub fn health(settings: &RelaySettings) -> Result<(), String> {
    let base = normalized_base_url(&settings.base_url)?;
    let response = client()?
        .get(format!("{base}/healthz"))
        .send()
        .map_err(|error| format!("Relay 健康检查失败：{error}"))?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(format!("Relay 健康检查失败：HTTP {}", response.status()))
    }
}
pub fn start_client(
    settings: RelaySettings,
    credentials: RelayCredentials,
    master_key: [u8; 32],
    status: Arc<Mutex<RelayStatus>>,
    snapshot_provider: SnapshotProvider,
    pairing_handler: PairingHandler,
    revoke_handler: RevokeHandler,
    control_handler: ControlHandler,
) -> Result<RelayClientHandle, String> {
    validate(&settings)?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let thread_status = status.clone();

    if let Ok(mut value) = status.lock() {
        *value = RelayStatus {
            enabled: settings.enabled,
            configured: true,
            desktop_id: Some(credentials.desktop_id.clone()),
            ..RelayStatus::default()
        };
    }

    let thread = thread::spawn(move || {
        let runtime = match tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        {
            Ok(value) => value,
            Err(error) => {
                set_error(&thread_status, format!("创建 Relay runtime 失败：{error}"));
                return;
            }
        };
        runtime.block_on(async move {
            relay_loop(
                settings,
                credentials,
                master_key,
                thread_status,
                snapshot_provider,
                pairing_handler,
                revoke_handler,
                control_handler,
                thread_stop,
            ).await;
        });
    });

    Ok(RelayClientHandle {
        stop,
        thread: Some(thread),
    })
}

async fn relay_loop(
    settings: RelaySettings,
    credentials: RelayCredentials,
    master_key: [u8; 32],
    status: Arc<Mutex<RelayStatus>>,
    snapshot_provider: SnapshotProvider,
    pairing_handler: PairingHandler,
    revoke_handler: RevokeHandler,
    control_handler: ControlHandler,
    stop: Arc<AtomicBool>,
) {
    let uploader = tokio::spawn(snapshot_http_loop(
        settings.clone(),
        credentials.clone(),
        master_key,
        status.clone(),
        snapshot_provider,
        stop.clone(),
    ));

    let mut attempt = 0u32;
    while !stop.load(Ordering::SeqCst) && settings.enabled {
        let result = relay_session(
            &settings,
            &credentials,
            status.clone(),
            pairing_handler.clone(),
            revoke_handler.clone(),
            control_handler.clone(),
            stop.clone(),
        ).await;

        if stop.load(Ordering::SeqCst) {
            break;
        }
        attempt = attempt.saturating_add(1);
        {
            if let Ok(mut value) = status.lock() {
                value.connected = false;
                value.connecting = false;
                value.session_id = None;
                value.reconnect_attempt = attempt;
                if let Err(error) = result {
                    value.last_error = error;
                }
            }
        }
        let base = RECONNECT_DELAYS[(attempt.saturating_sub(1) as usize).min(RECONNECT_DELAYS.len() - 1)];
        let delay = jittered_delay(base);
        if wait_for_network_recovery(&settings, delay, stop.clone()).await {
            attempt = 0;
            if let Ok(mut value) = status.lock() {
                value.reconnect_attempt = 0;
            }
        }
    }
    uploader.abort();
    let _ = uploader.await;
    if let Ok(mut value) = status.lock() {
        value.connected = false;
        value.connecting = false;
        value.session_id = None;
    }
}

fn jittered_delay(seconds: u64) -> Duration {
    let jitter = (now_ms() % 401) as i64 - 200;
    let millis = (seconds as i64 * 1000)
        .saturating_mul(1000 + jitter)
        / 1000;
    Duration::from_millis(millis.max(200) as u64)
}

async fn wait_for_network_recovery(
    settings: &RelaySettings,
    delay: Duration,
    stop: Arc<AtomicBool>,
) -> bool {
    let health_url = normalized_base_url(&settings.base_url)
        .ok()
        .map(|base| format!("{base}/healthz"));
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(2))
        .timeout(RECOVERY_PROBE_TIMEOUT)
        .build()
        .ok();
    let deadline = tokio::time::Instant::now() + delay;
    let mut saw_unreachable = false;

    loop {
        if stop.load(Ordering::SeqCst) || tokio::time::Instant::now() >= deadline {
            return false;
        }

        if let (Some(client), Some(health_url)) = (&client, &health_url) {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let probe_timeout = RECOVERY_PROBE_TIMEOUT.min(remaining);
            let reachable = tokio::time::timeout(
                probe_timeout,
                client.get(health_url).send(),
            )
            .await
            .ok()
            .and_then(Result::ok)
            .map(|response| response.status().is_success())
            .unwrap_or(false);
            if reachable && saw_unreachable {
                return true;
            }
            saw_unreachable |= !reachable;
        }

        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return false;
        }
        let sleep_for = if client.is_some() && health_url.is_some() {
            RECOVERY_PROBE_INTERVAL.min(remaining)
        } else {
            Duration::from_millis(200).min(remaining)
        };
        tokio::time::sleep(sleep_for).await;
    }
}

async fn snapshot_http_loop(
    settings: RelaySettings,
    credentials: RelayCredentials,
    master_key: [u8; 32],
    status: Arc<Mutex<RelayStatus>>,
    snapshot_provider: SnapshotProvider,
    stop: Arc<AtomicBool>,
) {
    let base = match normalized_base_url(&settings.base_url) {
        Ok(value) => value,
        Err(error) => {
            set_error(&status, error);
            return;
        }
    };
    let client = match reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(10))
        .build()
    {
        Ok(value) => value,
        Err(error) => {
            set_error(&status, format!("创建 Relay HTTPS 客户端失败：{error}"));
            return;
        }
    };
    let session_id = snapshot_session_id();
    let mut sequences = std::collections::HashMap::<String, u64>::new();
    let mut tick = tokio::time::interval(SNAPSHOT_INTERVAL);

    loop {
        tick.tick().await;
        if stop.load(Ordering::SeqCst) || !settings.enabled {
            return;
        }
        for source in snapshot_provider() {
            let sequence = sequences.entry(source.device_id.clone()).or_insert(0);
            *sequence = sequence.saturating_add(1);
            let generated_at = now_ms();
            let encrypted = match monitor_crypto::encrypt_snapshot(
                &master_key,
                &credentials.desktop_id,
                &source.device_id,
                &session_id,
                *sequence,
                generated_at,
                &source.plaintext,
            ) {
                Ok(value) => value,
                Err(error) => {
                    set_error(&status, format!("Relay HTTPS Snapshot 加密失败：{error}"));
                    continue;
                }
            };
            let url = format!(
                "{base}/v1/desktops/{}/devices/{}/snapshot",
                credentials.desktop_id,
                source.device_id,
            );
            match client
                .post(url)
                .bearer_auth(&credentials.desktop_token)
                .json(&encrypted)
                .send()
                .await
            {
                Ok(response) if response.status().is_success() => {
                    if let Ok(mut value) = status.lock() {
                        value.last_heartbeat_at = Some(now_ms());
                    }
                }
                Ok(response) => set_error(
                    &status,
                    format!("Relay HTTPS Snapshot 上传失败：HTTP {}", response.status()),
                ),
                Err(error) => set_error(
                    &status,
                    format!("Relay HTTPS Snapshot 上传失败：{error}"),
                ),
            }
        }
    }
}

async fn relay_session(
    settings: &RelaySettings,
    credentials: &RelayCredentials,
    status: Arc<Mutex<RelayStatus>>,
    pairing_handler: PairingHandler,
    revoke_handler: RevokeHandler,
    control_handler: ControlHandler,
    stop: Arc<AtomicBool>,
) -> Result<(), String> {
    if let Ok(mut value) = status.lock() {
        value.connecting = true;
        value.connected = false;
        value.last_error.clear();
    }

    let url = websocket_url(
        &settings.base_url,
        &format!("/v1/ws/desktop?desktopId={}", credentials.desktop_id),
    )?;
    let mut request = url.into_client_request()
        .map_err(|error| format!("构造 Relay WSS 请求失败：{error}"))?;
    request.headers_mut().insert(
        AUTHORIZATION,
        HeaderValue::from_str(&format!("Bearer {}", credentials.desktop_token))
            .map_err(|_| "Relay Desktop Token 无效。".to_string())?,
    );

    let (mut socket, _) = connect_async(request)
        .await
        .map_err(|error| format!("连接 Relay WSS 失败：{error}"))?;

    let hello = DesktopWsMessage::Hello {
        protocol_version: PROTOCOL_VERSION,
        desktop_id: credentials.desktop_id.clone(),
        app_version: env!("CARGO_PKG_VERSION").to_string(),
        platform: format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH),
        capabilities: device_capabilities(),
    };
    send_desktop_message(&mut socket, &hello).await?;

    let heartbeat_start = tokio::time::Instant::now() + CLIENT_HEARTBEAT_INTERVAL;
    let mut heartbeat_tick = tokio::time::interval_at(
        heartbeat_start,
        CLIENT_HEARTBEAT_INTERVAL,
    );
    let mut last_server_activity = tokio::time::Instant::now();

    loop {
        if stop.load(Ordering::SeqCst) {
            let _ = socket.close(None).await;
            return Ok(());
        }
        tokio::select! {
            _ = heartbeat_tick.tick() => {
                if last_server_activity.elapsed() > CLIENT_IDLE_TIMEOUT {
                    return Err("Relay WSS heartbeat timeout。".into());
                }
                send_desktop_message(
                    &mut socket,
                    &DesktopWsMessage::Ping { sent_at: now_ms() },
                ).await?;
            }
            incoming = socket.next() => {
                let Some(message) = incoming else {
                    return Err("Relay WSS 已关闭。".into());
                };
                let message = message.map_err(|error| format!("Relay WSS 读取失败：{error}"))?;
                last_server_activity = tokio::time::Instant::now();
                if let Ok(mut value) = status.lock() {
                    value.last_heartbeat_at = Some(now_ms());
                }
                match message {
                    Message::Text(text) => {
                        match serde_json::from_str::<DesktopServerMessage>(&text) {
                            Ok(DesktopServerMessage::HelloAck { session_id: accepted, .. }) => {
                                if let Ok(mut value) = status.lock() {
                                    value.connected = true;
                                    value.connecting = false;
                                    value.connected_at = Some(now_ms());
                                    value.last_heartbeat_at = Some(now_ms());
                                    value.reconnect_attempt = 0;
                                    value.session_id = Some(accepted);
                                    value.last_error.clear();
                                }
                            }
                            Ok(DesktopServerMessage::Pong { .. }) => {}
                            Ok(DesktopServerMessage::PairingRequest {
                                pairing_id,
                                connection_id,
                                payload,
                            }) => {
                                let result = pairing_handler(PairingWsMessage::Pair {
                                    pairing_id: pairing_id.clone(),
                                    payload,
                                });
                                let response = match result {
                                    Ok(PairingServerMessage::PairResult {
                                        pairing_id: response_pairing_id,
                                        payload,
                                    }) => DesktopWsMessage::PairingResponse {
                                        pairing_id: response_pairing_id,
                                        connection_id,
                                        payload,
                                    },
                                    Ok(PairingServerMessage::Error { message }) | Err(message) => {
                                        DesktopWsMessage::PairingError {
                                            pairing_id,
                                            connection_id,
                                            message,
                                        }
                                    }
                                };
                                send_desktop_message(&mut socket, &response).await?;
                            }
                            Ok(DesktopServerMessage::DeviceRevoked { device_id }) => {
                                if let Err(error) = revoke_handler(&device_id) {
                                    set_error(
                                        &status,
                                        format!(
                                            "Relay 已撤销设备，但 Desktop 本地同步失败：{error}"
                                        ),
                                    );
                                }
                            }
                            Ok(DesktopServerMessage::DeviceControl { device_id, request }) => {
                                let request_id = request.request_id.clone();
                                let response = match control_handler(&device_id, request) {
                                    Ok(response) => DesktopWsMessage::ControlResponse {
                                        device_id,
                                        response,
                                    },
                                    Err(message) => {
                                        set_error(
                                            &status,
                                            format!("Monitor 控制请求被拒绝：{message}"),
                                        );
                                        DesktopWsMessage::ControlError {
                                            device_id,
                                            request_id,
                                            message: "desktop rejected control request".into(),
                                        }
                                    },
                                };
                                send_desktop_message(&mut socket, &response).await?;
                            }
                            Ok(DesktopServerMessage::Error { message }) => {
                                set_error(&status, message);
                            }
                            Err(error) => {
                                set_error(&status, format!("Relay WSS 消息无效：{error}"));
                            }
                        }
                    }
                    Message::Ping(payload) => {
                        socket.send(Message::Pong(payload)).await
                            .map_err(|error| format!("Relay WSS Pong 失败：{error}"))?;
                    }
                    Message::Pong(_) => {}
                    Message::Close(_) => return Err("Relay WSS 已关闭。".into()),
                    _ => {}
                }
            }
        }
    }
}

async fn send_desktop_message<S>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
    message: &DesktopWsMessage,
) -> Result<(), String>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    let text = serde_json::to_string(message)
        .map_err(|error| format!("序列化 Relay WSS 消息失败：{error}"))?;
    socket.send(Message::Text(text.into())).await
        .map_err(|error| format!("发送 Relay WSS 消息失败：{error}"))
}

fn set_error(status: &Arc<Mutex<RelayStatus>>, error: String) {
    if let Ok(mut value) = status.lock() {
        value.last_error = error;
    }
}
#[cfg(target_os = "macos")]
pub fn cleanup_legacy_ssh(home: &Path, state_dir: &Path) {
    const LABEL: &str = "com.chatx.relay";
    let domain = format!("gui/{}", unsafe { libc::getuid() });
    let service = format!("{domain}/{LABEL}");
    let _ = Command::new("/bin/launchctl")
        .args(["bootout", &service])
        .output();
    let plist = home.join("Library").join("LaunchAgents").join(format!("{LABEL}.plist"));
    let _ = fs::remove_file(plist);
    let _ = fs::remove_file(state_dir.join("relay-run.sh"));
    let _ = fs::remove_file(state_dir.join("monitor-relay.json"));
}

#[cfg(not(target_os = "macos"))]
pub fn cleanup_legacy_ssh(_home: &Path, state_dir: &Path) {
    let _ = fs::remove_file(state_dir.join("relay-run.sh"));
    let _ = fs::remove_file(state_dir.join("monitor-relay.json"));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relay_requires_https_except_loopback_development() {
        assert!(normalized_base_url("https://relay.example.com/").is_ok());
        assert_eq!(
            normalized_base_url("https://relay.example.com/").unwrap(),
            "https://relay.example.com"
        );
        assert!(normalized_base_url("http://127.0.0.1:8787").is_ok());
        assert!(normalized_base_url("http://relay.example.com").is_err());
        assert!(normalized_base_url("https://user:pass@relay.example.com").is_err());
    }

    #[test]
    fn settings_disabled_do_not_require_url() {
        assert!(validate(&RelaySettings::default()).is_ok());
    }

    #[test]
    fn https_snapshot_session_uses_monitor_session_namespace() {
        assert!(snapshot_session_id().starts_with("s_"));
    }
}
