use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        DefaultBodyLimit, Path as AxumPath, Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post, put},
    Json, Router,
};
use chatx_relay_protocol::{
    device_capabilities, DesktopRegisterRequest, DesktopRegisterResponse, DesktopServerMessage,
    DesktopWsMessage, DeviceAuthorizeRequest, DeviceServerMessage, DeviceWsMessage,
    EncryptedControlPayload, EncryptedSnapshot, PairingRouteOpenRequest, PairingServerMessage,
    PairingWsMessage, CAPABILITY_CONTROL_RECONNECT, CAPABILITY_CONTROL_REFRESH,
    CAPABILITY_SNAPSHOT, PROTOCOL_VERSION,
};
use ring::{digest, rand::{SecureRandom, SystemRandom}};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    collections::HashMap,
    env,
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::sync::{broadcast, oneshot, RwLock, Semaphore};

const SNAPSHOT_TTL_MS: u64 = 180_000;
const WS_HEARTBEAT: Duration = Duration::from_secs(15);
const WS_IDLE_TIMEOUT: Duration = Duration::from_secs(45);
const MAX_BODY_BYTES: usize = 64 * 1024;
const DEVICE_CHANNEL_CAPACITY: usize = 32;
const DESKTOP_CHANNEL_CAPACITY: usize = 32;
const PAIRING_TIMEOUT: Duration = Duration::from_secs(15);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_DESKTOP_WS: usize = 64;
const MAX_DEVICE_WS: usize = 512;
const MAX_PAIRING_WS: usize = 64;
const MAX_PENDING_CONTROLS: usize = 128;
const CONTROL_MIN_INTERVAL_MS: u64 = 500;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

fn random_hex(bytes: usize) -> Result<String, String> {
    let mut value = vec![0u8; bytes];
    SystemRandom::new()
        .fill(&mut value)
        .map_err(|_| "secure random generation failed".to_string())?;
    Ok(value.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn token_hash(token: &str) -> String {
    digest::digest(&digest::SHA256, token.as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn constant_time_hex_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.as_bytes().iter().zip(right.as_bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

fn hash_matches(token: &str, expected_hex: &str) -> bool {
    constant_time_hex_eq(&token_hash(token), expected_hex)
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get("authorization")?.to_str().ok()?;
    value.strip_prefix("Bearer ").map(str::trim).filter(|value| !value.is_empty())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceRecord {
    token_hash: String,
    created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopRecord {
    token_hash: String,
    device_name: String,
    app_version: String,
    platform: String,
    created_at: u64,
    #[serde(default)]
    devices: HashMap<String, DeviceRecord>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct Registry {
    #[serde(default)]
    desktops: HashMap<String, DesktopRecord>,
}

#[derive(Debug, Clone)]
struct SnapshotRecord {
    received_at: u64,
    snapshot: EncryptedSnapshot,
}

#[derive(Debug, Clone)]
struct PairingRoute {
    token_hash: String,
    expires_at: u64,
}

#[derive(Clone)]
struct DesktopRoute {
    session_id: String,
    sender: broadcast::Sender<DesktopServerMessage>,
    capabilities: Vec<String>,
}

type RouteKey = (String, String);
type ControlRouteKey = (String, String, String);
type ControlSender = oneshot::Sender<Result<EncryptedControlPayload, String>>;

fn routed_device_capabilities(route: Option<&DesktopRoute>) -> Vec<String> {
    let mut result = vec![CAPABILITY_SNAPSHOT.to_string()];
    let Some(route) = route else {
        return result;
    };
    for capability in device_capabilities() {
        if capability != CAPABILITY_SNAPSHOT
            && route.capabilities.iter().any(|value| value == &capability)
        {
            result.push(capability);
        }
    }
    result
}

fn route_supports_control(route: &DesktopRoute) -> bool {
    route.capabilities.iter().any(|capability| {
        capability == CAPABILITY_CONTROL_REFRESH
            || capability == CAPABILITY_CONTROL_RECONNECT
    })
}

#[derive(Clone)]
struct AppState {
    registry: Arc<Mutex<Registry>>,
    registry_path: PathBuf,
    bootstrap_token_hash: Option<String>,
    online_desktops: Arc<RwLock<HashMap<String, String>>>,
    desktop_channels: Arc<RwLock<HashMap<String, DesktopRoute>>>,
    device_channels: Arc<RwLock<HashMap<RouteKey, broadcast::Sender<DeviceServerMessage>>>>,
    snapshots: Arc<RwLock<HashMap<RouteKey, SnapshotRecord>>>,
    pairing_routes: Arc<RwLock<HashMap<RouteKey, PairingRoute>>>,
    pairing_connections: Arc<RwLock<HashMap<String, oneshot::Sender<PairingServerMessage>>>>,
    control_connections: Arc<RwLock<HashMap<ControlRouteKey, ControlSender>>>,
    control_last_at: Arc<Mutex<HashMap<RouteKey, u64>>>,
    desktop_ws_slots: Arc<Semaphore>,
    device_ws_slots: Arc<Semaphore>,
    pairing_ws_slots: Arc<Semaphore>,
}

impl AppState {
    fn load(registry_path: PathBuf, bootstrap_token: Option<String>) -> Self {
        let registry = fs::read_to_string(&registry_path)
            .ok()
            .and_then(|text| serde_json::from_str::<Registry>(&text).ok())
            .unwrap_or_default();
        Self {
            registry: Arc::new(Mutex::new(registry)),
            registry_path,
            bootstrap_token_hash: bootstrap_token
                .filter(|value| !value.trim().is_empty())
                .map(|value| token_hash(value.trim())),
            online_desktops: Arc::new(RwLock::new(HashMap::new())),
            desktop_channels: Arc::new(RwLock::new(HashMap::new())),
            device_channels: Arc::new(RwLock::new(HashMap::new())),
            snapshots: Arc::new(RwLock::new(HashMap::new())),
            pairing_routes: Arc::new(RwLock::new(HashMap::new())),
            pairing_connections: Arc::new(RwLock::new(HashMap::new())),
            control_connections: Arc::new(RwLock::new(HashMap::new())),
            control_last_at: Arc::new(Mutex::new(HashMap::new())),
            desktop_ws_slots: Arc::new(Semaphore::new(MAX_DESKTOP_WS)),
            device_ws_slots: Arc::new(Semaphore::new(MAX_DEVICE_WS)),
            pairing_ws_slots: Arc::new(Semaphore::new(MAX_PAIRING_WS)),
        }
    }

    fn save_registry(&self, registry: &Registry) -> Result<(), String> {
        if let Some(parent) = self.registry_path.parent() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("create relay data directory failed: {error}"))?;
        }
        let temp = self.registry_path.with_extension("json.tmp");
        let text = serde_json::to_string_pretty(registry)
            .map_err(|error| format!("serialize registry failed: {error}"))?;
        fs::write(&temp, format!("{text}\n"))
            .map_err(|error| format!("write relay registry failed: {error}"))?;
        fs::rename(&temp, &self.registry_path)
            .map_err(|error| format!("replace relay registry failed: {error}"))
    }

    fn desktop_authorized(&self, desktop_id: &str, token: &str) -> bool {
        self.registry.lock().ok()
            .and_then(|registry| registry.desktops.get(desktop_id).cloned())
            .map(|desktop| hash_matches(token, &desktop.token_hash))
            .unwrap_or(false)
    }

    fn device_authorized(&self, desktop_id: &str, device_id: &str, token: &str) -> bool {
        self.registry.lock().ok()
            .and_then(|registry| registry.desktops.get(desktop_id).cloned())
            .and_then(|desktop| desktop.devices.get(device_id).cloned())
            .map(|device| hash_matches(token, &device.token_hash))
            .unwrap_or(false)
    }

    fn device_exists(&self, desktop_id: &str, device_id: &str) -> bool {
        self.registry.lock().ok()
            .and_then(|registry| registry.desktops.get(desktop_id).cloned())
            .is_some_and(|desktop| desktop.devices.contains_key(device_id))
    }

    async fn route_sender(&self, desktop_id: &str, device_id: &str) -> broadcast::Sender<DeviceServerMessage> {
        let key = (desktop_id.to_string(), device_id.to_string());
        let mut channels = self.device_channels.write().await;
        channels.entry(key)
            .or_insert_with(|| broadcast::channel(DEVICE_CHANNEL_CAPACITY).0)
            .clone()
    }

    async fn broadcast_desktop_presence(&self, desktop_id: &str, online: bool) {
        let channels = self.device_channels.read().await;
        let message = DeviceServerMessage::DesktopPresence {
            online,
            changed_at: now_ms(),
        };
        for ((candidate_desktop, _), sender) in channels.iter() {
            if candidate_desktop == desktop_id {
                let _ = sender.send(message.clone());
            }
        }
    }

    async fn cache_and_route_snapshot(
        &self,
        desktop_id: &str,
        device_id: &str,
        snapshot: EncryptedSnapshot,
    ) -> Result<(), String> {
        if !self.device_exists(desktop_id, device_id) {
            return Err("device not authorized".into());
        }
        if snapshot.nonce.len() > 64 || snapshot.ciphertext.len() > 96 * 1024 {
            return Err("snapshot too large".into());
        }
        let key = (desktop_id.to_string(), device_id.to_string());
        let received_at = now_ms();
        {
            let mut snapshots = self.snapshots.write().await;
            if let Some(previous) = snapshots.get(&key) {
                if previous.snapshot.session_id == snapshot.session_id
                    && snapshot.sequence <= previous.snapshot.sequence
                {
                    return Err("snapshot sequence is not increasing".into());
                }
            }
            snapshots.insert(key.clone(), SnapshotRecord {
                received_at,
                snapshot: snapshot.clone(),
            });
        }
        let sender = self.route_sender(desktop_id, device_id).await;
        let _ = sender.send(DeviceServerMessage::Snapshot { received_at, snapshot });
        Ok(())
    }

    fn revoke_device_record(
        &self,
        desktop_id: &str,
        device_id: &str,
    ) -> Result<bool, String> {
        let mut registry = self.registry.lock()
            .map_err(|_| "registry lock poisoned".to_string())?;
        let Some(desktop) = registry.desktops.get_mut(desktop_id) else {
            return Ok(false);
        };
        let previous = desktop.devices.clone();
        let removed = desktop.devices.remove(device_id).is_some();
        if !removed {
            return Ok(false);
        }
        if let Err(error) = self.save_registry(&registry) {
            if let Some(desktop) = registry.desktops.get_mut(desktop_id) {
                desktop.devices = previous;
            }
            return Err(error);
        }
        Ok(true)
    }

    async fn latest_snapshot(&self, desktop_id: &str, device_id: &str) -> Option<SnapshotRecord> {
        let key = (desktop_id.to_string(), device_id.to_string());
        let mut snapshots = self.snapshots.write().await;
        let stale = snapshots.get(&key)
            .is_some_and(|record| now_ms().saturating_sub(record.received_at) > SNAPSHOT_TTL_MS);
        if stale {
            snapshots.remove(&key);
        }
        snapshots.get(&key).cloned()
    }

    async fn complete_control(
        &self,
        desktop_id: &str,
        device_id: &str,
        request_id: &str,
        result: Result<EncryptedControlPayload, String>,
    ) {
        let key = (
            desktop_id.to_string(),
            device_id.to_string(),
            request_id.to_string(),
        );
        if let Some(sender) = self.control_connections.write().await.remove(&key) {
            let _ = sender.send(result);
        }
    }
}

fn json_error(status: StatusCode, message: impl Into<String>) -> Response {
    (status, Json(json!({ "error": message.into() }))).into_response()
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "protocolVersion": PROTOCOL_VERSION,
        "onlineDesktops": state.online_desktops.read().await.len(),
        "serverTime": now_ms()
    }))
}

fn desktop_registration_record(
    existing: Option<&DesktopRecord>,
    desktop_token: &str,
    payload: &DesktopRegisterRequest,
) -> DesktopRecord {
    DesktopRecord {
        token_hash: token_hash(desktop_token),
        device_name: payload.device_name.chars().take(128).collect(),
        app_version: payload.app_version.chars().take(64).collect(),
        platform: payload.platform.chars().take(64).collect(),
        created_at: existing.map(|record| record.created_at).unwrap_or_else(now_ms),
        devices: existing.map(|record| record.devices.clone()).unwrap_or_default(),
    }
}

async fn register_desktop(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<DesktopRegisterRequest>,
) -> Response {
    let Some(expected) = state.bootstrap_token_hash.as_deref() else {
        return json_error(StatusCode::SERVICE_UNAVAILABLE, "desktop registration is disabled");
    };
    let Some(token) = bearer(&headers) else {
        return json_error(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    if !constant_time_hex_eq(&token_hash(token), expected) {
        return json_error(StatusCode::UNAUTHORIZED, "invalid bootstrap token");
    }

    let desktop_id = payload.desktop_id.trim().to_string();
    if !desktop_id.starts_with("d_")
        || desktop_id.len() < 18
        || desktop_id.len() > 80
        || !desktop_id[2..].bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return json_error(StatusCode::BAD_REQUEST, "invalid desktop id");
    }
    let desktop_token = match random_hex(32) {
        Ok(value) => value,
        Err(error) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    };

    let mut registry = match state.registry.lock() {
        Ok(value) => value,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "registry lock poisoned"),
    };
    let previous = registry.desktops.get(&desktop_id).cloned();
    let replacement = desktop_registration_record(previous.as_ref(), &desktop_token, &payload);
    registry.desktops.insert(desktop_id.clone(), replacement);
    if let Err(error) = state.save_registry(&registry) {
        if let Some(record) = previous {
            registry.desktops.insert(desktop_id.clone(), record);
        } else {
            registry.desktops.remove(&desktop_id);
        }
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, error);
    }
    Json(DesktopRegisterResponse { desktop_id, desktop_token }).into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DesktopQuery {
    desktop_id: String,
}

async fn desktop_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<DesktopQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(token) = bearer(&headers) else {
        return json_error(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    if !state.desktop_authorized(&query.desktop_id, token) {
        return json_error(StatusCode::UNAUTHORIZED, "invalid desktop credential");
    }
    let permit = match state.desktop_ws_slots.clone().try_acquire_owned() {
        Ok(value) => value,
        Err(_) => return json_error(StatusCode::SERVICE_UNAVAILABLE, "desktop websocket capacity reached"),
    };
    ws.on_upgrade(move |socket| async move {
        let _permit = permit;
        desktop_socket(state, query.desktop_id, socket).await;
    })
}

async fn desktop_socket(state: AppState, desktop_id: String, mut socket: WebSocket) {
    let hello = tokio::time::timeout(Duration::from_secs(5), socket.recv()).await;
    let Ok(Some(Ok(Message::Text(text)))) = hello else { return; };
    let Ok(DesktopWsMessage::Hello {
        protocol_version,
        desktop_id: hello_id,
        capabilities,
        ..
    }) = serde_json::from_str::<DesktopWsMessage>(&text) else {
        return;
    };
    if protocol_version != PROTOCOL_VERSION || hello_id != desktop_id {
        return;
    }

    let session_id = match random_hex(16) {
        Ok(value) => format!("s_{value}"),
        Err(_) => return,
    };
    let (desktop_sender, mut desktop_receiver) =
        broadcast::channel(DESKTOP_CHANNEL_CAPACITY);
    state.online_desktops.write().await
        .insert(desktop_id.clone(), session_id.clone());
    state.desktop_channels.write().await.insert(
        desktop_id.clone(),
        DesktopRoute {
            session_id: session_id.clone(),
            sender: desktop_sender,
            capabilities,
        },
    );
    state.broadcast_desktop_presence(&desktop_id, true).await;

    let ack = DesktopServerMessage::HelloAck {
        session_id: session_id.clone(),
        server_time: now_ms(),
        capabilities: device_capabilities(),
    };
    if send_json(&mut socket, &ack).await.is_err() {
        remove_online_if_current(&state, &desktop_id, &session_id).await;
        return;
    }

    let mut heartbeat = tokio::time::interval(WS_HEARTBEAT);
    let mut last_activity = Instant::now();

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if last_activity.elapsed() > WS_IDLE_TIMEOUT {
                    break;
                }
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            routed = desktop_receiver.recv() => {
                match routed {
                    Ok(message) => {
                        if send_json(&mut socket, &message).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = socket.recv() => {
                let Some(Ok(message)) = incoming else { break; };
                last_activity = Instant::now();
                match message {
                    Message::Text(text) => {
                        let Ok(message) = serde_json::from_str::<DesktopWsMessage>(&text) else {
                            let _ = send_json(&mut socket, &DesktopServerMessage::Error {
                                message: "invalid websocket message".into(),
                            }).await;
                            continue;
                        };
                        match message {
                            DesktopWsMessage::Hello { protocol_version, desktop_id: hello_id, .. } => {
                                if protocol_version != PROTOCOL_VERSION || hello_id != desktop_id {
                                    break;
                                }
                            }
                            DesktopWsMessage::Ping { .. } => {
                                let _ = send_json(&mut socket, &DesktopServerMessage::Pong {
                                    server_time: now_ms(),
                                }).await;
                            }
                            DesktopWsMessage::Snapshot { device_id, snapshot } => {
                                if let Err(error) = state.cache_and_route_snapshot(
                                    &desktop_id,
                                    &device_id,
                                    snapshot,
                                ).await {
                                    let _ = send_json(&mut socket, &DesktopServerMessage::Error {
                                        message: error,
                                    }).await;
                                }
                            }
                            DesktopWsMessage::PairingResponse {
                                pairing_id,
                                connection_id,
                                payload,
                            } => {
                                if let Some(sender) = state.pairing_connections
                                    .write().await.remove(&connection_id)
                                {
                                    let _ = sender.send(PairingServerMessage::PairResult {
                                        pairing_id,
                                        payload,
                                    });
                                }
                            }
                            DesktopWsMessage::PairingError {
                                connection_id,
                                message,
                                ..
                            } => {
                                if let Some(sender) = state.pairing_connections
                                    .write().await.remove(&connection_id)
                                {
                                    let _ = sender.send(PairingServerMessage::Error {
                                        message: message.chars().take(240).collect(),
                                    });
                                }
                            }
                            DesktopWsMessage::ControlResponse { device_id, response } => {
                                let request_id = response.request_id.clone();
                                state.complete_control(
                                    &desktop_id,
                                    &device_id,
                                    &request_id,
                                    Ok(response),
                                ).await;
                            }
                            DesktopWsMessage::ControlError {
                                device_id,
                                request_id,
                                message,
                            } => {
                                state.complete_control(
                                    &desktop_id,
                                    &device_id,
                                    &request_id,
                                    Err(message.chars().take(240).collect()),
                                ).await;
                            }
                        }
                    }
                    Message::Ping(payload) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Message::Pong(_) => {}
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }

    remove_online_if_current(&state, &desktop_id, &session_id).await;
}

async fn remove_online_if_current(state: &AppState, desktop_id: &str, session_id: &str) {
    let mut online = state.online_desktops.write().await;
    if online.get(desktop_id).map(String::as_str) == Some(session_id) {
        online.remove(desktop_id);
        drop(online);
        {
            let mut channels = state.desktop_channels.write().await;
            if channels.get(desktop_id)
                .is_some_and(|route| route.session_id == session_id)
            {
                channels.remove(desktop_id);
            }
        }
        state.broadcast_desktop_presence(desktop_id, false).await;
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct DeviceQuery {
    desktop_id: String,
    device_id: String,
}

async fn device_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<DeviceQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(token) = bearer(&headers) else {
        return json_error(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    if !state.device_authorized(&query.desktop_id, &query.device_id, token) {
        return json_error(StatusCode::UNAUTHORIZED, "invalid device credential");
    }
    let permit = match state.device_ws_slots.clone().try_acquire_owned() {
        Ok(value) => value,
        Err(_) => return json_error(StatusCode::SERVICE_UNAVAILABLE, "device websocket capacity reached"),
    };
    ws.on_upgrade(move |socket| async move {
        let _permit = permit;
        device_socket(state, query.desktop_id, query.device_id, socket).await;
    })
}

async fn desktop_snapshot_http(
    State(state): State<AppState>,
    AxumPath((desktop_id, device_id)): AxumPath<(String, String)>,
    headers: HeaderMap,
    Json(snapshot): Json<EncryptedSnapshot>,
) -> Response {
    let Some(token) = bearer(&headers) else {
        return json_error(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    if !state.desktop_authorized(&desktop_id, token) {
        return json_error(StatusCode::UNAUTHORIZED, "invalid desktop credential");
    }
    match state.cache_and_route_snapshot(&desktop_id, &device_id, snapshot).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) if error == "device not authorized" => {
            json_error(StatusCode::NOT_FOUND, error)
        }
        Err(error) => json_error(StatusCode::BAD_REQUEST, error),
    }
}

async fn device_snapshot_http(
    State(state): State<AppState>,
    AxumPath((desktop_id, device_id)): AxumPath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let Some(token) = bearer(&headers) else {
        return json_error(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    if !state.device_authorized(&desktop_id, &device_id, token) {
        return json_error(StatusCode::UNAUTHORIZED, "invalid device credential");
    }

    let desktop_online = state.online_desktops.read().await.contains_key(&desktop_id);
    if let Some(record) = state.latest_snapshot(&desktop_id, &device_id).await {
        return Json(json!({
            "type": "snapshot",
            "desktopOnline": true,
            "serverTime": now_ms(),
            "receivedAt": record.received_at,
            "snapshot": record.snapshot,
        }))
        .into_response();
    }

    Json(json!({
        "type": "status",
        "desktopOnline": desktop_online,
        "serverTime": now_ms(),
    }))
    .into_response()
}

async fn device_revoke_http(
    State(state): State<AppState>,
    AxumPath((desktop_id, device_id)): AxumPath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let Some(token) = bearer(&headers) else {
        return json_error(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    if !state.device_authorized(&desktop_id, &device_id, token) {
        return json_error(StatusCode::UNAUTHORIZED, "invalid device credential");
    }
    let desktop_route = state.desktop_channels.read().await.get(&desktop_id).cloned();
    let Some(desktop_route) = desktop_route else {
        return json_error(
            StatusCode::CONFLICT,
            "desktop offline; synchronized revoke unavailable",
        );
    };
    match state.revoke_device_record(&desktop_id, &device_id) {
        Ok(true) => {
            state.snapshots.write().await.remove(&(desktop_id, device_id.clone()));
            let _ = desktop_route.sender.send(DesktopServerMessage::DeviceRevoked {
                device_id: device_id.clone(),
            });
            Json(json!({ "type": "revoked", "deviceId": device_id })).into_response()
        }
        Ok(false) => json_error(StatusCode::NOT_FOUND, "device already revoked"),
        Err(error) => json_error(StatusCode::INTERNAL_SERVER_ERROR, error),
    }
}

async fn device_socket(
    state: AppState,
    desktop_id: String,
    device_id: String,
    mut socket: WebSocket,
) {
    let hello = tokio::time::timeout(Duration::from_secs(5), socket.recv()).await;
    let Ok(Some(Ok(Message::Text(text)))) = hello else { return; };
    let Ok(DeviceWsMessage::Hello {
        protocol_version,
        desktop_id: hello_desktop,
        device_id: hello_device,
    }) = serde_json::from_str::<DeviceWsMessage>(&text) else {
        return;
    };
    if protocol_version != PROTOCOL_VERSION
        || hello_desktop != desktop_id
        || hello_device != device_id
    {
        return;
    }

    let session_id = match random_hex(16) {
        Ok(value) => format!("s_{value}"),
        Err(_) => return,
    };
    let desktop_route = state.desktop_channels
        .read().await.get(&desktop_id).cloned();
    let desktop_online = desktop_route.is_some();
    let capabilities = routed_device_capabilities(desktop_route.as_ref());
    let sender = state.route_sender(&desktop_id, &device_id).await;
    let mut receiver = sender.subscribe();

    if send_json(&mut socket, &DeviceServerMessage::HelloAck {
        session_id,
        server_time: now_ms(),
        desktop_online,
        capabilities,
    }).await.is_err() {
        return;
    }
    if let Some(record) = state.latest_snapshot(&desktop_id, &device_id).await {
        let _ = send_json(&mut socket, &DeviceServerMessage::Snapshot {
            received_at: record.received_at,
            snapshot: record.snapshot,
        }).await;
    }

    let mut heartbeat = tokio::time::interval(WS_HEARTBEAT);
    let mut last_activity = Instant::now();

    loop {
        tokio::select! {
            _ = heartbeat.tick() => {
                if last_activity.elapsed() > WS_IDLE_TIMEOUT {
                    break;
                }
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    break;
                }
            }
            routed = receiver.recv() => {
                match routed {
                    Ok(message) => {
                        if send_json(&mut socket, &message).await.is_err() {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if let Some(record) = state.latest_snapshot(&desktop_id, &device_id).await {
                            if send_json(&mut socket, &DeviceServerMessage::Snapshot {
                                received_at: record.received_at,
                                snapshot: record.snapshot,
                            }).await.is_err() {
                                break;
                            }
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = socket.recv() => {
                let Some(Ok(message)) = incoming else { break; };
                last_activity = Instant::now();
                match message {
                    Message::Text(text) => {
                        let Ok(message) = serde_json::from_str::<DeviceWsMessage>(&text) else {
                            let _ = send_json(&mut socket, &DeviceServerMessage::Error {
                                message: "invalid websocket message".into(),
                            }).await;
                            continue;
                        };
                        match message {
                            DeviceWsMessage::Hello {
                                protocol_version,
                                desktop_id: hello_desktop,
                                device_id: hello_device,
                            } => {
                                if protocol_version != PROTOCOL_VERSION
                                    || hello_desktop != desktop_id
                                    || hello_device != device_id
                                {
                                    break;
                                }
                            }
                            DeviceWsMessage::Ping { .. } => {
                                let _ = send_json(&mut socket, &DeviceServerMessage::Pong {
                                    server_time: now_ms(),
                                }).await;
                            }
                            DeviceWsMessage::Control { request } => {
                                let now = now_ms();
                                let valid_id = request.request_id.starts_with("c_")
                                    && request.request_id.len() >= 18
                                    && request.request_id.len() <= 80
                                    && request.request_id[2..].bytes().all(|byte| byte.is_ascii_hexdigit());
                                let valid_time = request.issued_at <= now.saturating_add(30_000)
                                    && request.expires_at >= now
                                    && request.expires_at <= request.issued_at.saturating_add(60_000);
                                if !valid_id || !valid_time {
                                    let _ = send_json(
                                        &mut socket,
                                        &DeviceServerMessage::Error {
                                            message: "invalid control request".into(),
                                        },
                                    ).await;
                                    continue;
                                }
                                let rate_key = (desktop_id.clone(), device_id.clone());
                                let rate_limited = state.control_last_at.lock()
                                    .map(|mut values| {
                                        values.retain(|_, at| now.saturating_sub(*at) <= 60_000);
                                        let limited = values.get(&rate_key)
                                            .is_some_and(|at| now.saturating_sub(*at) < CONTROL_MIN_INTERVAL_MS);
                                        if !limited {
                                            values.insert(rate_key.clone(), now);
                                        }
                                        limited
                                    })
                                    .unwrap_or(true);
                                if rate_limited {
                                    let _ = send_json(
                                        &mut socket,
                                        &DeviceServerMessage::Error {
                                            message: "control request rate limited".into(),
                                        },
                                    ).await;
                                    continue;
                                }
                                let desktop_route = state.desktop_channels
                                    .read().await.get(&desktop_id).cloned();
                                let Some(desktop_route) = desktop_route else {
                                    let _ = send_json(
                                        &mut socket,
                                        &DeviceServerMessage::Error {
                                            message: "desktop offline; control unavailable".into(),
                                        },
                                    ).await;
                                    continue;
                                };
                                if !route_supports_control(&desktop_route) {
                                    let _ = send_json(
                                        &mut socket,
                                        &DeviceServerMessage::Error {
                                            message: "desktop control capability unavailable".into(),
                                        },
                                    ).await;
                                    continue;
                                }
                                let request_id = request.request_id.clone();
                                let control_key = (
                                    desktop_id.clone(),
                                    device_id.clone(),
                                    request_id.clone(),
                                );
                                let (sender, receiver) = oneshot::channel();
                                {
                                    let mut pending = state.control_connections.write().await;
                                    if pending.len() >= MAX_PENDING_CONTROLS {
                                        let _ = send_json(
                                            &mut socket,
                                            &DeviceServerMessage::Error {
                                                message: "control capacity reached".into(),
                                            },
                                        ).await;
                                        continue;
                                    }
                                    if pending.contains_key(&control_key) {
                                        let _ = send_json(
                                            &mut socket,
                                            &DeviceServerMessage::Error {
                                                message: "duplicate control request".into(),
                                            },
                                        ).await;
                                        continue;
                                    }
                                    pending.insert(control_key.clone(), sender);
                                }
                                if desktop_route.sender.send(
                                    DesktopServerMessage::DeviceControl {
                                        device_id: device_id.clone(),
                                        request,
                                    },
                                ).is_err() {
                                    state.control_connections.write().await.remove(&control_key);
                                    let _ = send_json(
                                        &mut socket,
                                        &DeviceServerMessage::Error {
                                            message: "desktop control channel unavailable".into(),
                                        },
                                    ).await;
                                    continue;
                                }
                                let result = tokio::time::timeout(CONTROL_TIMEOUT, receiver).await;
                                state.control_connections.write().await.remove(&control_key);
                                match result {
                                    Ok(Ok(Ok(response))) => {
                                        let _ = send_json(
                                            &mut socket,
                                            &DeviceServerMessage::ControlResult { response },
                                        ).await;
                                    }
                                    Ok(Ok(Err(message))) => {
                                        let _ = send_json(
                                            &mut socket,
                                            &DeviceServerMessage::Error { message },
                                        ).await;
                                    }
                                    _ => {
                                        let _ = send_json(
                                            &mut socket,
                                            &DeviceServerMessage::Error {
                                                message: "desktop control timeout".into(),
                                            },
                                        ).await;
                                    }
                                }
                            }
                            DeviceWsMessage::RevokeSelf => {
                                let desktop_route = state.desktop_channels
                                    .read().await.get(&desktop_id).cloned();
                                let Some(desktop_route) = desktop_route else {
                                    let _ = send_json(
                                        &mut socket,
                                        &DeviceServerMessage::Error {
                                            message: "desktop offline; synchronized revoke unavailable".into(),
                                        },
                                    ).await;
                                    break;
                                };
                                match state.revoke_device_record(&desktop_id, &device_id) {
                                    Ok(true) => {
                                        state.snapshots.write().await.remove(&(
                                            desktop_id.clone(),
                                            device_id.clone(),
                                        ));
                                        let _ = desktop_route.sender.send(
                                            DesktopServerMessage::DeviceRevoked {
                                                device_id: device_id.clone(),
                                            },
                                        );
                                        let _ = send_json(
                                            &mut socket,
                                            &DeviceServerMessage::Revoked {
                                                device_id: device_id.clone(),
                                            },
                                        ).await;
                                        break;
                                    }
                                    Ok(false) => {
                                        let _ = send_json(
                                            &mut socket,
                                            &DeviceServerMessage::Error {
                                                message: "device already revoked".into(),
                                            },
                                        ).await;
                                        break;
                                    }
                                    Err(error) => {
                                        let _ = send_json(
                                            &mut socket,
                                            &DeviceServerMessage::Error { message: error },
                                        ).await;
                                    }
                                }
                            }
                        }
                    }
                    Message::Ping(payload) => {
                        if socket.send(Message::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    Message::Pong(_) => {}
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
}

async fn open_pairing_route(
    State(state): State<AppState>,
    AxumPath((desktop_id, pairing_id)): AxumPath<(String, String)>,
    headers: HeaderMap,
    Json(payload): Json<PairingRouteOpenRequest>,
) -> Response {
    let Some(token) = bearer(&headers) else {
        return json_error(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    if !state.desktop_authorized(&desktop_id, token) {
        return json_error(StatusCode::UNAUTHORIZED, "invalid desktop credential");
    }
    if !pairing_id.starts_with("p_")
        || payload.token_hash.len() != 64
        || !payload.token_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return json_error(StatusCode::BAD_REQUEST, "invalid pairing route");
    }
    let now = now_ms();
    if payload.expires_at <= now || payload.expires_at > now.saturating_add(10 * 60_000) {
        return json_error(StatusCode::BAD_REQUEST, "invalid pairing expiry");
    }
    state.pairing_routes.write().await.insert(
        (desktop_id, pairing_id),
        PairingRoute {
            token_hash: payload.token_hash.to_ascii_lowercase(),
            expires_at: payload.expires_at,
        },
    );
    StatusCode::NO_CONTENT.into_response()
}

async fn delete_pairing_route(
    State(state): State<AppState>,
    AxumPath((desktop_id, pairing_id)): AxumPath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let Some(token) = bearer(&headers) else {
        return json_error(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    if !state.desktop_authorized(&desktop_id, token) {
        return json_error(StatusCode::UNAUTHORIZED, "invalid desktop credential");
    }
    state.pairing_routes.write().await.remove(&(desktop_id, pairing_id));
    StatusCode::NO_CONTENT.into_response()
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PairingQuery {
    desktop_id: String,
    pairing_id: String,
}

async fn pairing_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<PairingQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(token) = bearer(&headers) else {
        return json_error(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    let key = (query.desktop_id.clone(), query.pairing_id.clone());
    let route = state.pairing_routes.read().await.get(&key).cloned();
    let Some(route) = route else {
        return json_error(StatusCode::NOT_FOUND, "pairing route not found");
    };
    if now_ms() > route.expires_at {
        state.pairing_routes.write().await.remove(&key);
        return json_error(StatusCode::GONE, "pairing route expired");
    }
    if !hash_matches(token, &route.token_hash) {
        return json_error(StatusCode::UNAUTHORIZED, "invalid pairing route token");
    }
    let permit = match state.pairing_ws_slots.clone().try_acquire_owned() {
        Ok(value) => value,
        Err(_) => return json_error(StatusCode::SERVICE_UNAVAILABLE, "pairing websocket capacity reached"),
    };
    ws.on_upgrade(move |socket| async move {
        let _permit = permit;
        pairing_socket(state, query.desktop_id, query.pairing_id, socket).await;
    })
}

async fn pairing_socket(
    state: AppState,
    desktop_id: String,
    pairing_id: String,
    mut socket: WebSocket,
) {
    let incoming = tokio::time::timeout(Duration::from_secs(7), socket.recv()).await;
    let Ok(Some(Ok(Message::Text(text)))) = incoming else { return; };
    let Ok(PairingWsMessage::Pair {
        pairing_id: message_pairing_id,
        payload,
    }) = serde_json::from_str::<PairingWsMessage>(&text) else {
        let _ = send_json(&mut socket, &PairingServerMessage::Error {
            message: "invalid pairing message".into(),
        }).await;
        return;
    };
    if message_pairing_id != pairing_id {
        let _ = send_json(&mut socket, &PairingServerMessage::Error {
            message: "pairing route mismatch".into(),
        }).await;
        return;
    }

    let desktop_sender = state.desktop_channels.read().await
        .get(&desktop_id)
        .map(|route| route.sender.clone());
    let Some(desktop_sender) = desktop_sender else {
        let _ = send_json(&mut socket, &PairingServerMessage::Error {
            message: "desktop offline".into(),
        }).await;
        return;
    };

    let connection_id = match random_hex(16) {
        Ok(value) => format!("pc_{value}"),
        Err(_) => return,
    };
    let (response_tx, response_rx) = oneshot::channel();
    state.pairing_connections.write().await
        .insert(connection_id.clone(), response_tx);

    let routed = DesktopServerMessage::PairingRequest {
        pairing_id: pairing_id.clone(),
        connection_id: connection_id.clone(),
        payload,
    };
    if desktop_sender.send(routed).is_err() {
        state.pairing_connections.write().await.remove(&connection_id);
        let _ = send_json(&mut socket, &PairingServerMessage::Error {
            message: "desktop offline".into(),
        }).await;
        return;
    }

    let response = tokio::time::timeout(PAIRING_TIMEOUT, response_rx).await;
    state.pairing_connections.write().await.remove(&connection_id);
    match response {
        Ok(Ok(message)) => {
            let successful = matches!(message, PairingServerMessage::PairResult { .. });
            let _ = send_json(&mut socket, &message).await;
            if successful {
                state.pairing_routes.write().await
                    .remove(&(desktop_id, pairing_id));
            }
        }
        _ => {
            let _ = send_json(&mut socket, &PairingServerMessage::Error {
                message: "desktop pairing response timed out".into(),
            }).await;
        }
    }
}

async fn send_json<T: Serialize>(socket: &mut WebSocket, value: &T) -> Result<(), ()> {
    let text = serde_json::to_string(value).map_err(|_| ())?;
    socket.send(Message::Text(text.into())).await.map_err(|_| ())
}

async fn authorize_device(
    State(state): State<AppState>,
    AxumPath((desktop_id, device_id)): AxumPath<(String, String)>,
    headers: HeaderMap,
    Json(payload): Json<DeviceAuthorizeRequest>,
) -> Response {
    let Some(token) = bearer(&headers) else {
        return json_error(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    if !state.desktop_authorized(&desktop_id, token) {
        return json_error(StatusCode::UNAUTHORIZED, "invalid desktop credential");
    }
    if payload.token_hash.len() != 64 || !payload.token_hash.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return json_error(StatusCode::BAD_REQUEST, "invalid device token hash");
    }

    let mut registry = match state.registry.lock() {
        Ok(value) => value,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "registry lock poisoned"),
    };
    let Some(desktop) = registry.desktops.get_mut(&desktop_id) else {
        return json_error(StatusCode::NOT_FOUND, "desktop not found");
    };
    let previous = desktop.devices.clone();
    desktop.devices.insert(device_id, DeviceRecord {
        token_hash: payload.token_hash.to_ascii_lowercase(),
        created_at: now_ms(),
    });
    if let Err(error) = state.save_registry(&registry) {
        if let Some(desktop) = registry.desktops.get_mut(&desktop_id) {
            desktop.devices = previous;
        }
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, error);
    }
    StatusCode::NO_CONTENT.into_response()
}

async fn revoke_device(
    State(state): State<AppState>,
    AxumPath((desktop_id, device_id)): AxumPath<(String, String)>,
    headers: HeaderMap,
) -> Response {
    let Some(token) = bearer(&headers) else {
        return json_error(StatusCode::UNAUTHORIZED, "missing bearer token");
    };
    if !state.desktop_authorized(&desktop_id, token) {
        return json_error(StatusCode::UNAUTHORIZED, "invalid desktop credential");
    }

    {
        let mut registry = match state.registry.lock() {
            Ok(value) => value,
            Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "registry lock poisoned"),
        };
        let Some(desktop) = registry.desktops.get_mut(&desktop_id) else {
            return json_error(StatusCode::NOT_FOUND, "desktop not found");
        };
        let previous = desktop.devices.clone();
        desktop.devices.remove(&device_id);
        if let Err(error) = state.save_registry(&registry) {
            if let Some(desktop) = registry.desktops.get_mut(&desktop_id) {
                desktop.devices = previous;
            }
            return json_error(StatusCode::INTERNAL_SERVER_ERROR, error);
        }
    }

    state.snapshots.write().await.remove(&(desktop_id.clone(), device_id.clone()));
    state.device_channels.write().await.remove(&(desktop_id, device_id));
    StatusCode::NO_CONTENT.into_response()
}

fn registry_path() -> PathBuf {
    env::var_os("CHATX_RELAY_DATA")
        .map(PathBuf::from)
        .unwrap_or_else(|| Path::new("./relay-data.json").to_path_buf())
}

#[tokio::main]
async fn main() {
    let bind: SocketAddr = env::var("CHATX_RELAY_BIND")
        .unwrap_or_else(|_| "127.0.0.1:8787".into())
        .parse()
        .expect("CHATX_RELAY_BIND must be a valid socket address");
    let bootstrap = env::var("CHATX_RELAY_BOOTSTRAP_TOKEN").ok();
    let state = AppState::load(registry_path(), bootstrap);

    let app = Router::new()
        .route("/healthz", get(health))
        .route("/v1/desktops/register", post(register_desktop))
        .route("/v1/ws/desktop", get(desktop_ws))
        .route("/v1/ws/device", get(device_ws))
        .route("/v1/ws/pair", get(pairing_ws))
        .route(
            "/v1/desktops/{desktop_id}/devices/{device_id}/snapshot",
            get(device_snapshot_http).post(desktop_snapshot_http),
        )
        .route(
            "/v1/desktops/{desktop_id}/devices/{device_id}/revoke-self",
            post(device_revoke_http),
        )
        .route(
            "/v1/desktops/{desktop_id}/devices/{device_id}",
            put(authorize_device).delete(revoke_device),
        )
        .route(
            "/v1/desktops/{desktop_id}/pairings/{pairing_id}",
            put(open_pairing_route).delete(delete_pairing_route),
        )
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .expect("bind ChatX relay listener");
    eprintln!("chatx-relay listening on http://{bind}");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await
        .expect("serve ChatX relay");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_hash_is_stable_and_verifiable() {
        let token = "ab".repeat(32);
        let hash = token_hash(&token);
        assert_eq!(hash.len(), 64);
        assert!(hash_matches(&token, &hash));
        assert!(!hash_matches(&"cd".repeat(32), &hash));
    }

    #[test]
    fn re_registration_rotates_token_and_preserves_devices() {
        let mut devices = HashMap::new();
        devices.insert("device-1".into(), DeviceRecord {
            token_hash: token_hash("device-secret"),
            created_at: 7,
        });
        let existing = DesktopRecord {
            token_hash: token_hash("old-desktop-secret"),
            device_name: "Old Mac".into(),
            app_version: "0.4.9".into(),
            platform: "macos-arm64".into(),
            created_at: 42,
            devices,
        };
        let payload = DesktopRegisterRequest {
            desktop_id: "d_0123456789abcdef0123456789abcdef".into(),
            device_name: "Current Mac".into(),
            app_version: "0.4.10".into(),
            platform: "macos-arm64".into(),
        };

        let refreshed = desktop_registration_record(Some(&existing), "new-desktop-secret", &payload);

        assert!(hash_matches("new-desktop-secret", &refreshed.token_hash));
        assert!(!hash_matches("old-desktop-secret", &refreshed.token_hash));
        assert_eq!(refreshed.created_at, 42);
        assert_eq!(refreshed.device_name, "Current Mac");
        assert!(refreshed.devices.contains_key("device-1"));
    }

    #[test]
    fn registry_serialization_never_contains_plain_token() {
        let mut registry = Registry::default();
        registry.desktops.insert("d_test".into(), DesktopRecord {
            token_hash: token_hash("secret"),
            device_name: "Mac".into(),
            app_version: "0.4.7".into(),
            platform: "macos-arm64".into(),
            created_at: 1,
            devices: HashMap::new(),
        });
        let text = serde_json::to_string(&registry).unwrap();
        assert!(!text.contains("secret"));
    }

    #[test]
    fn device_control_capabilities_require_online_desktop_support() {
        assert_eq!(
            routed_device_capabilities(None),
            vec![CAPABILITY_SNAPSHOT.to_string()],
        );

        let (sender, _) = broadcast::channel(1);
        let legacy = DesktopRoute {
            session_id: "s_legacy".into(),
            sender: sender.clone(),
            capabilities: Vec::new(),
        };
        assert_eq!(
            routed_device_capabilities(Some(&legacy)),
            vec![CAPABILITY_SNAPSHOT.to_string()],
        );
        assert!(!route_supports_control(&legacy));

        let current = DesktopRoute {
            session_id: "s_current".into(),
            sender,
            capabilities: device_capabilities(),
        };
        assert_eq!(
            routed_device_capabilities(Some(&current)),
            device_capabilities(),
        );
        assert!(route_supports_control(&current));
    }

    #[tokio::test]
    async fn control_completion_is_scoped_by_desktop_device_and_request() {
        let state = AppState::load(
            std::env::temp_dir().join("chatx-relay-control-test.json"),
            None,
        );
        let request_id = "c_0123456789abcdef".to_string();
        let key_a = ("d_a".into(), "dev_a".into(), request_id.clone());
        let key_b = ("d_a".into(), "dev_b".into(), request_id.clone());
        let (sender_a, receiver_a) = oneshot::channel();
        let (sender_b, receiver_b) = oneshot::channel();
        {
            let mut pending = state.control_connections.write().await;
            pending.insert(key_a, sender_a);
            pending.insert(key_b.clone(), sender_b);
        }

        let response = EncryptedControlPayload {
            request_id: request_id.clone(),
            issued_at: 1,
            expires_at: 2,
            nonce: "nonce".into(),
            ciphertext: "ciphertext".into(),
        };
        state.complete_control(
            "d_a",
            "dev_a",
            &request_id,
            Ok(response.clone()),
        ).await;

        assert_eq!(receiver_a.await.unwrap().unwrap(), response);
        assert!(state.control_connections.read().await.contains_key(&key_b));

        state.complete_control(
            "d_a",
            "dev_b",
            &request_id,
            Err("denied".into()),
        ).await;
        assert_eq!(receiver_b.await.unwrap().unwrap_err(), "denied");
    }
}
