// Native WSS reverse transport for HTTPS MCP. The public gateway has no OAuth
// credentials: all HTTP/OAuth checks stay in the desktop's loopback Bridge.
use super::{AppState, now_ms, random_hex};
use crate::registry::token_hash;
use axum::{
    body::{to_bytes, Body, Bytes},
    http::header::CONTENT_LENGTH,
    extract::{Query, State, WebSocketUpgrade, ws::{Message, WebSocket}},
    http::{HeaderMap, Method, StatusCode, Uri},
    response::{IntoResponse, Response},
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use futures_util::{stream, StreamExt};
use chatx_relay_protocol::mcp_control::ControlKind;
use chatx_relay_protocol::mcp_http::{self, allowed_path, allowed_request_header, allowed_response_header};
use chatx_relay_protocol::mcp_stream::{self, Kind};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io,
    sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{mpsc, watch, RwLock};

const LEGACY_MAX_REQUEST: usize = mcp_http::LEGACY_MAX_REQUEST_BYTES;
const MAX_CHUNK: usize = mcp_stream::MAX_FRAME_BODY;
const MAX_RESPONSE: usize = mcp_stream::MAX_RESPONSE_BYTES;
const MAX_PENDING: usize = mcp_stream::MAX_INFLIGHT;
const PUBLIC_HEAD_TIMEOUT: Duration = Duration::from_secs(605);
const PUBLIC_CHUNK_TIMEOUT: Duration = Duration::from_secs(605);
const IDLE_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Clone)]
pub(super) struct Gateway {
    pub(super) desktop_id: Option<String>,
    active: Arc<RwLock<Option<Active>>>,
    pending: Arc<Mutex<HashMap<String, Pending>>>,
}

#[derive(Clone)]
struct Active {
    session_id: String,
    tx: mpsc::Sender<Outgoing>,
    version: u8,
    credential_hash: String,
    close: watch::Sender<bool>,
}
enum Outgoing {
    Text(String),
    Binary(Vec<u8>),
}

struct Pending {
    session_id: String,
    tx: mpsc::Sender<Reply>,
    bytes: usize,
    errored: Arc<AtomicBool>,
}

enum Reply {
    Head(u16, Vec<(String, String)>),
    Chunk(Bytes),
    End,
    Error,
    UploadTooLarge,
    UploadFailed,
}

struct PendingGuard {
    pending: Arc<Mutex<HashMap<String, Pending>>>,
    request_id: String,
    errored: Arc<AtomicBool>,
    upload: Option<tokio::task::JoinHandle<()>>,
    cancel: Option<mpsc::Sender<Outgoing>>,
    finished: bool,
}
impl PendingGuard {
    fn mark_finished(&mut self) { self.finished = true; }
}
impl Drop for PendingGuard {
    fn drop(&mut self) {
        if let Some(upload) = self.upload.take() { upload.abort(); }
        if !self.finished {
            if let Some(outbound) = self.cancel.as_ref() {
                let packet = Outgoing::Text(json!({
                    "type":"request_cancel", "requestId":self.request_id
                }).to_string());
                if let Err(mpsc::error::TrySendError::Full(packet)) = outbound.try_send(packet) {
                    // Control frames must not be lost under normal WSS traffic.
                    // Do not block Drop or spawn an unbounded retry loop.
                    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
                        let tx = outbound.clone();
                        runtime.spawn(async move {
                            let _ = tokio::time::timeout(Duration::from_secs(5),
                                tx.send(packet)).await;
                        });
                    }
                }
            }
        }
        if let Ok(mut pending) = self.pending.lock() {
            pending.remove(&self.request_id);
        }
    }
}

impl Gateway {
    pub(super) fn from_env() -> Self {
        // A server must explicitly select its authorized desktop. Never route
        // public MCP requests to an arbitrary online Android-monitor desktop.
        let desktop_id = std::env::var("CHATX_MCP_DESKTOP_ID").ok()
            .filter(|id| valid_desktop_id(id));
        Self {
            desktop_id,
            active: Arc::new(RwLock::new(None)),
            pending: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    // Registration rotates the desktop credential. Immediately retire any
    // websocket authenticated with the previous credential, without touching
    // Android Monitor's independent control channel.
    pub(super) async fn revoke_desktop(&self, desktop_id: &str) {
        if self.desktop_id.as_deref() != Some(desktop_id) { return; }
        let old = self.active.write().await.take();
        if let Some(active) = old {
            let _ = active.close.send(true);
            self.fail_session(&active.session_id);
            eprintln!("[chatx-mcp] desktop credential rotated; session revoked");
        }
    }

    pub(super) async fn status(&self) -> Value {
        let active = self.active.read().await;
        let queued = self.pending.lock().map(|pending| pending.len()).unwrap_or(0);
        json!({
            "enabled": self.desktop_id.is_some(),
            "connected": active.is_some(),
            "transportProtocol": active.as_ref().map(|session| session.version),
            "inFlight": queued,
            "maxInFlight": MAX_PENDING,
            "maxRequestBytesV2": mcp_stream::MAX_REQUEST_BYTES,
            "maxResponseBytesV2": MAX_RESPONSE,
        })
    }

    fn fail_session(&self, session_id: &str) {
        if let Ok(mut pending) = self.pending.lock() {
            pending.retain(|_, item| {
                if item.session_id == session_id {
                    item.errored.store(true, Ordering::SeqCst);
                    let _ = item.tx.try_send(Reply::Error);
                    false
                } else {
                    true
                }
            });
        }
    }

    // Bounded async delivery applies backpressure to the WebSocket reader.
    // A slow HTTP consumer must never result in silent truncated 200 responses.
    async fn receive(&self, session_id: &str, version: u8,
        packet: Option<&Value>, binary: Option<&[u8]>) {
        let decoded = binary.and_then(mcp_stream::decode);
        let id = if let Some((Kind::ResponseBody, ref id, _)) = decoded {
            id.as_str()
        } else if let Some(id) = packet.and_then(|p| p.get("requestId"))
            .and_then(Value::as_str) { id } else { return };
        if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) { return; }
        let (tx, error, reply) = {
        let Ok(mut pending) = self.pending.lock() else { return; };
        let Some(item) = pending.get_mut(id) else { return; };
        if item.session_id != session_id { return; }
        let reply = if let Some((Kind::ResponseBody, _, body)) = decoded {
            if version != 2 { return; }
            if item.bytes.saturating_add(body.len()) > MAX_RESPONSE {
                Reply::Error
            } else {
                item.bytes += body.len();
                Reply::Chunk(Bytes::copy_from_slice(body))
            }
        } else {
            let Some(packet) = packet else { return };
            match packet.get("type").and_then(Value::as_str).and_then(ControlKind::parse) {
                Some(ControlKind::Head) => {
                    let Some(status) = packet.get("status").and_then(Value::as_u64) else { return };
                    if !(200..=599).contains(&status) { return; }
                    let headers = packet.get("headers").and_then(Value::as_object)
                        .map(|o| o.iter().filter_map(|(key, value)| {
                            value.as_str().filter(|_| allowed_response_header(key))
                                .map(|value| (key.clone(), value.to_string()))
                        }).collect()).unwrap_or_default();
                    Reply::Head(status as u16, headers)
                }
                Some(ControlKind::Chunk) if version == mcp_http::WSS_PROTOCOL_V1 => {
                    let Some(encoded) = packet.get("data").and_then(Value::as_str) else { return };
                    if encoded.len() > MAX_CHUNK * 4 / 3 + 8 { return; }
                    let Ok(bytes) = STANDARD.decode(encoded) else { return };
                    if bytes.len() > MAX_CHUNK || item.bytes + bytes.len() > mcp_http::LEGACY_MAX_RESPONSE_BYTES {
                        Reply::Error
                    } else {
                        item.bytes += bytes.len();
                        Reply::Chunk(Bytes::from(bytes))
                    }
                }
                Some(ControlKind::End) => Reply::End,
                Some(ControlKind::Error) => Reply::Error,
                _ => return,
            }
        };
        let finished = matches!(reply, Reply::End | Reply::Error);
        if matches!(reply, Reply::Error) { item.errored.store(true, Ordering::SeqCst); }
        let tx = item.tx.clone();
        let error = item.errored.clone();
        if finished { pending.remove(id); }
        (tx, error, reply)
        };
        // NEVER await an individual HTTP reader in the shared WSS receive
        // loop: one slow stream must not block other streams or heartbeats.
        // If its bounded queue fills, abort only that response and let the
        // HTTP body emit a stream error instead of silently truncating 200.
        let is_chunk = matches!(reply, Reply::Chunk(_));
        if tx.try_send(reply).is_err() {
            error.store(true, Ordering::SeqCst);
            if let Ok(mut pending) = self.pending.lock() { pending.remove(id); }
            eprintln!("[chatx-mcp] slow HTTP consumer; request aborted (no payload logged)");
        } else if is_chunk {
            tokio::task::yield_now().await;
        }
    }
}

fn valid_desktop_id(id: &str) -> bool {
    id.starts_with("d_") && (18..=80).contains(&id.len()) &&
        id[2..].bytes().all(|b| b.is_ascii_hexdigit())
}
fn fail(status: StatusCode, message: &'static str) -> Response {
    (status, [("cache-control", "no-store")], message).into_response()
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct McpQuery {
    desktop_id: String,
}

pub(super) async fn websocket(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<McpQuery>,
    headers: HeaderMap,
) -> Response {
    if state.mcp_gateway.desktop_id.as_deref() != Some(&query.desktop_id) {
        return fail(StatusCode::NOT_FOUND, "MCP gateway not enabled for this desktop");
    }
    let bearer = headers.get("authorization").and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    if !bearer.is_some_and(|token| state.desktop_authorized(&query.desktop_id, token)) {
        return fail(StatusCode::UNAUTHORIZED, "Invalid desktop credential");
    }
    let credential_hash = token_hash(bearer.expect("bearer validated"));
    ws.max_message_size(8 * 1024 * 1024).max_frame_size(8 * 1024 * 1024)
        .on_upgrade(move |socket| socket_loop(state, socket, credential_hash))
}

async fn socket_loop(state: AppState, mut socket: WebSocket, credential_hash: String) {
    let gateway = state.mcp_gateway.clone();
    let hello = tokio::time::timeout(Duration::from_secs(5), socket.recv()).await;
    let Ok(Some(Ok(Message::Text(text)))) = hello else { return; };
    let Ok(data) = serde_json::from_str::<Value>(&text) else { return; };
    if ControlKind::parse(data["type"].as_str().unwrap_or("")) != Some(ControlKind::Hello)
        || data["protocolVersion"] != 1 { return; }
    let version: u8 = if data["supportedVersions"].as_array()
        .is_some_and(|versions| versions.contains(&json!(2))) { 2 } else { 1 };
    let Ok(session_id) = random_hex(16) else { return; };
    let (tx, mut rx) = mpsc::channel::<Outgoing>(64);
    let (close, mut closed) = watch::channel(false);
    let previous = gateway.active.write().await.replace(Active {
        session_id: session_id.clone(), tx, version, credential_hash: credential_hash.clone(), close,
    });
    // Recheck *after* installing the route. If a token rotates during the
    // upgrade handshake, either this check or the registrar's revoke catches it.
    if let Some(previous) = previous {
        let _ = previous.close.send(true);
        gateway.fail_session(&previous.session_id);
    }
    let authorized = state.registry.lock().ok()
        .and_then(|registry| gateway.desktop_id.as_ref()
            .and_then(|id| registry.desktops.get(id))
            .map(|record| record.token_hash == credential_hash))
        .unwrap_or(false);
    if !authorized {
        gateway.revoke_desktop(gateway.desktop_id.as_deref().unwrap_or("")).await;
        return;
    }
    eprintln!("[chatx-mcp] session online protocol=v{version}");
    if socket.send(Message::Text(json!({
        "type": ControlKind::HelloAck.as_str(), "sessionId": session_id, "serverTime": now_ms(),
        "protocolVersion": version
    }).to_string().into())).await.is_err() {
        disconnect(&gateway, &session_id).await;
        return;
    }
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    let mut last_activity = Instant::now();
    loop {
        tokio::select! {
            _ = closed.changed() => { break; }
            _ = heartbeat.tick() => {
                if last_activity.elapsed() > IDLE_TIMEOUT ||
                    socket.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
            }
            outgoing = rx.recv() => {
                let Some(outgoing) = outgoing else { break; };
                let message = match outgoing {
                    Outgoing::Text(text) => Message::Text(text.into()),
                    Outgoing::Binary(body) => Message::Binary(body.into()),
                };
                if socket.send(message).await.is_err() { break; }
            }
            incoming = socket.recv() => {
                let Some(Ok(message)) = incoming else { break; };
                last_activity = Instant::now();
                match message {
                    Message::Text(text) => {
                        if let Ok(packet) = serde_json::from_str::<Value>(&text) {
                            if packet["type"] == ControlKind::Ping.as_str() {
                                let _ = socket.send(Message::Text(
                                    json!({"type":ControlKind::Pong.as_str()}).to_string().into())).await;
                            } else {
                                gateway.receive(&session_id, version, Some(&packet), None).await;
                            }
                        }
                    }
                    Message::Ping(payload) => {
                        if socket.send(Message::Pong(payload)).await.is_err() { break; }
                    }
                    Message::Binary(data) if version == 2 => {
                        gateway.receive(&session_id, version, None, Some(&data)).await;
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        }
    }
    disconnect(&gateway, &session_id).await;
    eprintln!("[chatx-mcp] session offline protocol=v{version}");
}

async fn disconnect(gateway: &Gateway, session_id: &str) {
    let mut current = gateway.active.write().await;
    if current.as_ref().is_some_and(|route| route.session_id == session_id) {
        *current = None;
    }
    drop(current);
    gateway.fail_session(session_id);
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ProxyRequest<'a> {
    #[serde(rename = "type")]
    kind: &'static str,
    request_id: &'a str,
    method: &'a str,
    path: &'a str,
    headers: HashMap<String, String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body_base64: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    body_encoding: Option<&'static str>,
}

// Streaming upload is naturally bounded by the 64-slot websocket writer queue.
async fn stream_upload(tx: mpsc::Sender<Outgoing>, reply: mpsc::Sender<Reply>,
    id: String, body: Body) {
    let mut total = 0usize;
    let mut incoming = body.into_data_stream();
    while let Some(item) = incoming.next().await {
        let Ok(chunk) = item else {
            let _ = reply.send(Reply::UploadFailed).await;
            return;
        };
        total = total.saturating_add(chunk.len());
        if total > mcp_stream::MAX_REQUEST_BYTES {
            let _ = reply.send(Reply::UploadTooLarge).await;
            return;
        }
        for piece in chunk.chunks(MAX_CHUNK) {
            let Some(frame) = mcp_stream::encode(Kind::RequestBody, &id, piece) else { return };
            if tx.send(Outgoing::Binary(frame)).await.is_err() { return; }
        }
    }
    let _ = tx.send(Outgoing::Text(json!({
        "type":"request_end", "requestId": id
    }).to_string())).await;
}

pub(super) async fn public_request(
    State(state): State<AppState>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    if state.mcp_gateway.desktop_id.is_none() {
        return fail(StatusCode::SERVICE_UNAVAILABLE, "MCP gateway not configured");
    }
    if !allowed_path(uri.path()) { return fail(StatusCode::NOT_FOUND, "Not found"); }
    if !matches!(method, Method::GET | Method::POST | Method::DELETE |
        Method::HEAD | Method::OPTIONS) {
        return fail(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed");
    }
    let active = {
        let route = state.mcp_gateway.active.read().await;
        route.clone()
    };
    let Some(active) = active else {
        return fail(StatusCode::SERVICE_UNAVAILABLE, "Desktop MCP is offline");
    };
    // An already-open WSS socket must not outlive a replaced desktop token.
    let authorized = state.registry.lock().ok()
        .and_then(|reg| state.mcp_gateway.desktop_id.as_ref()
            .and_then(|id| reg.desktops.get(id))
            .map(|record| record.token_hash == active.credential_hash))
        .unwrap_or(false);
    if !authorized {
        state.mcp_gateway.revoke_desktop(
            state.mcp_gateway.desktop_id.as_deref().unwrap_or("")).await;
        return fail(StatusCode::SERVICE_UNAVAILABLE, "Desktop credential rotated");
    }
    if active.version == 2 {
        if headers.get(CONTENT_LENGTH).and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<usize>().ok())
            .is_some_and(|len| len > mcp_stream::MAX_REQUEST_BYTES) {
            return fail(StatusCode::PAYLOAD_TOO_LARGE, "MCP request too large");
        }
    }
    let legacy_body = if active.version == 1 {
        match to_bytes(body, LEGACY_MAX_REQUEST).await {
            Ok(bytes) => (Some(STANDARD.encode(bytes)), None),
            Err(_) => return fail(StatusCode::PAYLOAD_TOO_LARGE, "MCP request too large"),
        }
    } else { (None, Some(body)) };
    let Ok(request_id) = random_hex(16) else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Gateway error");
    };
    let (tx, mut rx) = mpsc::channel(16);
    let upload_reply = tx.clone();
    let errored = Arc::new(AtomicBool::new(false));
    {
        let Ok(mut pending) = state.mcp_gateway.pending.lock() else {
            return fail(StatusCode::INTERNAL_SERVER_ERROR, "Gateway error");
        };
        if pending.len() >= MAX_PENDING {
            eprintln!("[chatx-mcp] admission denied: pending queue at capacity");
            return fail(StatusCode::TOO_MANY_REQUESTS, "MCP gateway busy");
        }
        pending.insert(request_id.clone(), Pending {
            session_id: active.session_id.clone(), tx, bytes: 0,
            errored: errored.clone(),
        });
    }
    let mut guard = PendingGuard {
        pending: state.mcp_gateway.pending.clone(),
        request_id: request_id.clone(),
        errored,
        upload: None,
        cancel: Some(active.tx.clone()),
        finished: false,
    };
    let request_headers = headers.iter().filter_map(|(key, value)| {
        let key = key.as_str();
        (allowed_request_header(key)).then(|| {
            value.to_str().ok().map(|value| (key.to_string(), value.to_string()))
        }).flatten()
    }).collect();
    let request = ProxyRequest {
        kind: "request", request_id: &request_id, method: method.as_str(),
        path: &uri.to_string(), headers: request_headers,
        body_base64: legacy_body.0,
        body_encoding: if active.version == 2 { Some("binary-v2") } else { None },
    };
    let Ok(payload) = serde_json::to_string(&request) else {
        return fail(StatusCode::INTERNAL_SERVER_ERROR, "Gateway error");
    };
    if !matches!(tokio::time::timeout(Duration::from_secs(15),
        active.tx.send(Outgoing::Text(payload))).await, Ok(Ok(()))) {
        return fail(StatusCode::SERVICE_UNAVAILABLE, "Desktop MCP backpressure");
    }
    if let Some(body) = legacy_body.1 {
        guard.upload = Some(tokio::spawn(stream_upload(
            active.tx.clone(), upload_reply, request_id.clone(), body,
        )));
    }
    let head_timeout = PUBLIC_HEAD_TIMEOUT; // Local Bridge has 600s total timeout.
    let head = tokio::time::timeout(head_timeout, rx.recv()).await;
    let (status, response_headers) = match head {
        Ok(Some(Reply::Head(status, headers))) => (status, headers),
        Ok(Some(Reply::UploadTooLarge)) => {
            return fail(StatusCode::PAYLOAD_TOO_LARGE, "MCP request exceeds 64 MiB")
        }
        Ok(Some(Reply::UploadFailed)) => {
            return fail(StatusCode::BAD_REQUEST, "MCP request body interrupted")
        }
        Ok(Some(Reply::Error)) | Ok(None) => {
            return fail(StatusCode::BAD_GATEWAY, "Desktop MCP connection lost")
        }
        _ => {
            eprintln!("[chatx-mcp] desktop response head timeout");
            return fail(StatusCode::GATEWAY_TIMEOUT, "Desktop MCP did not respond");
        },
    };
    let body_stream = stream::unfold((rx, guard, false),
        |(mut rx, mut guard, reported_error)| async move {
        if reported_error { return None; }
        match tokio::time::timeout(PUBLIC_CHUNK_TIMEOUT, rx.recv()).await {
            Ok(Some(Reply::Chunk(bytes))) => {
                Some((Ok::<Bytes, io::Error>(bytes), (rx, guard, false)))
            }
            Ok(Some(Reply::End)) => { guard.mark_finished(); None },
            Ok(None) if !guard.errored.load(Ordering::SeqCst) => None,
            _ => Some((Err(io::Error::other("MCP response stream interrupted")),
                (rx, guard, true))),
        }
    });
    let mut builder = Response::builder().status(status);
    for (key, value) in response_headers {
        if let (Ok(k), Ok(v)) = (key.parse::<axum::http::HeaderName>(),
                                value.parse::<axum::http::HeaderValue>()) {
            builder = builder.header(k, v);
        }
    }
    builder.body(Body::from_stream(body_stream))
        .unwrap_or_else(|_| fail(StatusCode::BAD_GATEWAY, "Invalid MCP response"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::DesktopRecord;
    #[test]
    fn gateway_is_allowlisted_and_opt_in() {
        assert!(allowed_path("/mcp"));
        assert!(allowed_path("/.well-known/oauth-protected-resource/mcp"));
        assert!(allowed_path("/chatx-oauth/token"));
        assert!(!allowed_path("/healthz"));
        assert!(!allowed_path("/v1/desktops/register"));
        assert!(!allowed_path("/chatx-oauth/../../healthz"));
        assert!(!allowed_request_header("cookie"));
        assert!(!allowed_request_header("host"));
        assert!(!allowed_response_header("set-cookie"));
        assert!(valid_desktop_id(&format!("d_{}", "a".repeat(32))));
    }
    #[tokio::test]
    async fn gateway_forwards_oauth_challenge_and_streamed_body() {
        let path = std::env::temp_dir().join(format!(
            "chatx-mcp-wss-test-{}-{}.json", std::process::id(), now_ms()
        ));
        let mut state = AppState::load(path.clone(), None).unwrap();
        state.mcp_gateway.desktop_id = Some(format!("d_{}", "a".repeat(32)));
        let (out_tx, mut out_rx) = mpsc::channel(4);
        *state.mcp_gateway.active.write().await = Some(Active {
            session_id: "live".into(), tx: out_tx, version: 1,
            credential_hash: token_hash("test-private-token"),
            close: watch::channel(false).0,
        });
        state.registry.lock().unwrap().desktops.insert(
            format!("d_{}", "a".repeat(32)),
            DesktopRecord {
                token_hash: token_hash("test-private-token"),
                device_name: "test".into(), app_version: "test".into(),
                platform: "test".into(), created_at: now_ms(),
                devices: HashMap::new(),
            },
        );
        let gateway = state.mcp_gateway.clone();
        let req = tokio::spawn(public_request(
            State(state),
            Method::POST,
            "/mcp?test=1".parse().unwrap(),
            HeaderMap::new(),
            Body::from("{}"),
        ));
        let Outgoing::Text(outgoing) = out_rx.recv().await.unwrap() else { panic!("unexpected binary"); };
        let outgoing: Value = serde_json::from_str(&outgoing).unwrap();
        assert_eq!(outgoing["type"], "request");
        assert_eq!(outgoing["method"], "POST");
        assert_eq!(outgoing["path"], "/mcp?test=1");
        let id = outgoing["requestId"].as_str().unwrap();
        gateway.receive("live", 1, Some(&json!({
            "type": "head", "requestId": id, "status": 401,
            "headers": {"www-authenticate": "Bearer resource_metadata=\\\"https://newrow.fun/.well-known/oauth-protected-resource/mcp\\\""}
        })), None).await;
        gateway.receive("live", 1, Some(&json!({
            "type": "chunk", "requestId": id, "data": STANDARD.encode("unauthorized")
        })), None).await;
        gateway.receive("live", 1, Some(&json!({"type":"end","requestId":id})), None).await;
        let response = req.await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        assert!(response.headers().get("www-authenticate").is_some());
        let body = to_bytes(response.into_body(), 256).await.unwrap();
        assert_eq!(&body[..], b"unauthorized");
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn authenticated_wss_routes_a_real_http_request() {
        use crate::registry::{token_hash, DesktopRecord};
        use axum::routing::{any, get};
        use futures_util::{SinkExt, StreamExt};
        use tokio_tungstenite::{
            connect_async,
            tungstenite::{client::IntoClientRequest, Message as ClientMessage},
        };
        let path = std::env::temp_dir().join(format!(
            "chatx-mcp-wss-roundtrip-{}-{}.json", std::process::id(), now_ms()
        ));
        let mut state = AppState::load(path.clone(), None).unwrap();
        let desktop_id = format!("d_{}", "1".repeat(32));
        state.mcp_gateway.desktop_id = Some(desktop_id.clone());
        state.registry.lock().unwrap().desktops.insert(desktop_id.clone(), DesktopRecord {
            token_hash: token_hash("test-private-token"),
            device_name: "MCP test".into(), app_version: "test".into(),
            platform: "test".into(), created_at: now_ms(),
            devices: HashMap::new(),
        });
        let gateway = state.mcp_gateway.clone();
        let registry = state.registry.clone();
        let app = axum::Router::new()
            .route("/mcp", any(public_request))
            .route("/v1/ws/mcp", get(websocket))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let url = format!("ws://{address}/v1/ws/mcp?desktopId={desktop_id}");
        let unauthenticated = connect_async(url.clone()).await;
        assert!(unauthenticated.is_err(), "WSS must reject missing credentials");
        let mut request = url.clone().into_client_request().unwrap();
        request.headers_mut().insert("Authorization",
            "Bearer test-private-token".parse().unwrap());
        let (mut ws, _) = connect_async(request).await.unwrap();
        ws.send(ClientMessage::Text(
            json!({"type":"hello","protocolVersion":1}).to_string().into()
        )).await.unwrap();
        let ack = ws.next().await.unwrap().unwrap().into_text().unwrap();
        assert_eq!(serde_json::from_str::<Value>(&ack).unwrap()["type"], "hello_ack");
        let url = format!("http://{address}/mcp");
        let http = tokio::spawn(async move {
            reqwest::Client::new().post(url).body("request body").send().await.unwrap()
        });
        let packet = tokio::time::timeout(Duration::from_secs(4), async {
            loop {
                let frame = ws.next().await.unwrap().unwrap();
                if let ClientMessage::Text(text) = frame {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    if value["type"] == "request" { break value; }
                }
            }
        }).await.unwrap();
        assert_eq!(packet["path"], "/mcp");
        assert_eq!(STANDARD.decode(packet["bodyBase64"].as_str().unwrap()).unwrap(),
            b"request body");
        let request_id = packet["requestId"].as_str().unwrap();
        ws.send(ClientMessage::Text(json!({
            "type":"head","requestId":request_id,"status":401,
            "headers":{"www-authenticate":"Bearer test-challenge"}
        }).to_string().into())).await.unwrap();
        ws.send(ClientMessage::Text(json!({
            "type":"chunk","requestId":request_id,"data":STANDARD.encode("denied")
        }).to_string().into())).await.unwrap();
        ws.send(ClientMessage::Text(json!({
            "type":"end","requestId":request_id
        }).to_string().into())).await.unwrap();
        let response = http.await.unwrap();
        assert_eq!(response.status(), 401);
        assert_eq!(response.headers()["www-authenticate"], "Bearer test-challenge");
        assert_eq!(response.text().await.unwrap(), "denied");
        registry.lock().unwrap().desktops.get_mut(&desktop_id)
            .unwrap().token_hash = token_hash("rotated-token");
        gateway.revoke_desktop(&desktop_id).await;
        let disconnected = tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                match ws.next().await {
                    Some(Ok(ClientMessage::Ping(_))) | Some(Ok(ClientMessage::Pong(_))) => {},
                    Some(Ok(ClientMessage::Close(_))) | None | Some(Err(_)) => break,
                    other => panic!("revoked websocket is still receiving traffic: {other:?}"),
                }
            }
        }).await;
        assert!(disconnected.is_ok(), "rotated Desktop Token must close active WSS");
        let status = reqwest::Client::new().post(format!("http://{address}/mcp"))
            .send().await.unwrap().status();
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        let mut old = format!("ws://{address}/v1/ws/mcp?desktopId={desktop_id}")
            .into_client_request().unwrap();
        old.headers_mut().insert("Authorization",
            "Bearer test-private-token".parse().unwrap());
        assert!(connect_async(old).await.is_err(), "old token must be rejected");
        task.abort();
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn binary_v2_streams_request_and_response_above_legacy_limit() {
        let path = std::env::temp_dir().join(format!(
            "chatx-v2-size-{}-{}.json", std::process::id(), now_ms()
        ));
        let mut state = AppState::load(path.clone(), None).unwrap();
        state.mcp_gateway.desktop_id = Some(format!("d_{}", "a".repeat(32)));
        let (tx, mut out) = mpsc::channel::<Outgoing>(64);
        *state.mcp_gateway.active.write().await = Some(Active {
            session_id: "v2".into(), tx, version: 2,
            credential_hash: token_hash("test-private-token"),
            close: watch::channel(false).0,
        });
        state.registry.lock().unwrap().desktops.insert(
            format!("d_{}", "a".repeat(32)),
            DesktopRecord {
                token_hash: token_hash("test-private-token"),
                device_name: "test".into(), app_version: "test".into(),
                platform: "test".into(), created_at: now_ms(),
                devices: HashMap::new(),
            },
        );
        let gateway = state.mcp_gateway.clone();
        let request_len = 6 * 1024 * 1024 + 3;
        let public = tokio::spawn(public_request(
            State(state), Method::POST, "/mcp".parse().unwrap(),
            HeaderMap::new(), Body::from(vec![0x5a; request_len]),
        ));
        let Outgoing::Text(head) = out.recv().await.unwrap() else { panic!("missing head"); };
        let head: Value = serde_json::from_str(&head).unwrap();
        assert_eq!(head["bodyEncoding"], "binary-v2");
        assert!(head.get("bodyBase64").is_none());
        let id = head["requestId"].as_str().unwrap();
        let mut uploaded = 0;
        loop {
            match out.recv().await.unwrap() {
                Outgoing::Binary(frame) => {
                    let (kind, actual, data) = mcp_stream::decode(&frame).unwrap();
                    assert_eq!(kind, Kind::RequestBody);
                    assert_eq!(actual, id);
                    assert!(data.iter().all(|byte| *byte == 0x5a));
                    uploaded += data.len();
                }
                Outgoing::Text(text) => {
                    let value: Value = serde_json::from_str(&text).unwrap();
                    assert_eq!(value["type"], "request_end");
                    break;
                }
            }
        }
        assert_eq!(uploaded, request_len);
        gateway.receive("v2", 2, Some(&json!({
            "type":"head","requestId":id,"status":200,
            "headers":{"content-type":"application/octet-stream"}
        })), None).await;
        let response = public.await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let reader = tokio::spawn(async move {
            to_bytes(response.into_body(), 32 * 1024 * 1024).await.unwrap()
        });
        let response_len = 17 * 1024 * 1024 + 7;
        for chunk in vec![0x7b; response_len].chunks(MAX_CHUNK) {
            let frame = mcp_stream::encode(Kind::ResponseBody, id, chunk).unwrap();
            gateway.receive("v2", 2, None, Some(&frame)).await;
        }
        gateway.receive("v2", 2,
            Some(&json!({"type":"end","requestId":id})), None).await;
        let body = reader.await.unwrap();
        assert_eq!(body.len(), response_len);
        assert!(body.iter().all(|b| *b == 0x7b));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn registration_rotates_token_and_disconnects_mcp_only() {
        use chatx_relay_protocol::DesktopRegisterRequest;
        use axum::Json;
        let path = std::env::temp_dir().join(format!(
            "chatx-rotation-{}-{}.json", std::process::id(), now_ms()
        ));
        let mut state = AppState::load(path.clone(), Some("test-bootstrap".into())).unwrap();
        let id = format!("d_{}", "a".repeat(32));
        state.mcp_gateway.desktop_id = Some(id.clone());
        state.registry.lock().unwrap().desktops.insert(id.clone(), DesktopRecord {
            token_hash: token_hash("old-token"),
            device_name: "previous".into(), app_version: "0.4".into(),
            platform: "test".into(), created_at: now_ms(), devices: HashMap::new(),
        });
        let (out, _rx) = mpsc::channel(2);
        let (close, mut closed) = watch::channel(false);
        *state.mcp_gateway.active.write().await = Some(Active {
            session_id: "old".into(), tx: out, version: 2,
            credential_hash: token_hash("old-token"), close,
        });
        let gateway = state.mcp_gateway.clone();
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer test-bootstrap".parse().unwrap());
        let result = crate::registry_routes::register_desktop(
            State(state.clone()), headers,
            Json(DesktopRegisterRequest {
                desktop_id: id.clone(), device_name: "new".into(),
                app_version: "0.4.13".into(), platform: "test".into(),
            })
        ).await;
        assert_eq!(result.status(), StatusCode::OK);
        tokio::time::timeout(Duration::from_secs(1), closed.changed())
            .await.unwrap().unwrap();
        assert!(*closed.borrow());
        assert!(gateway.active.read().await.is_none());
        assert_ne!(state.registry.lock().unwrap().desktops[&id].token_hash,
            token_hash("old-token"));
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn slow_http_reader_cannot_block_other_mcp_requests() {
        let gateway = Gateway {
            desktop_id: None,
            active: Arc::new(RwLock::new(None)),
            pending: Arc::new(Mutex::new(HashMap::new())),
        };
        let slow_id = "a".repeat(32);
        let fast_id = "b".repeat(32);
        let (slow_tx, _slow_reader) = mpsc::channel(1);
        slow_tx.try_send(Reply::Chunk(Bytes::from_static(b"full"))).unwrap();
        let slow_error = Arc::new(AtomicBool::new(false));
        let (fast_tx, mut fast_rx) = mpsc::channel(1);
        {
            let mut pending = gateway.pending.lock().unwrap();
            pending.insert(slow_id.clone(), Pending {
                session_id: "live".into(), tx: slow_tx, bytes: 0,
                errored: slow_error.clone(),
            });
            pending.insert(fast_id.clone(), Pending {
                session_id: "live".into(), tx: fast_tx, bytes: 0,
                errored: Arc::new(AtomicBool::new(false)),
            });
        }
        let slow = mcp_stream::encode(Kind::ResponseBody, &slow_id, b"next").unwrap();
        tokio::time::timeout(Duration::from_millis(400), async {
            gateway.receive("live", 2, None, Some(&slow)).await;
            gateway.receive("live", 2, Some(&json!({
                "type":"head","requestId":fast_id,"status":204
            })), None).await;
        }).await.expect("slow response blocked WebSocket receiver");
        assert!(slow_error.load(Ordering::SeqCst));
        assert!(matches!(fast_rx.try_recv(), Ok(Reply::Head(204, _))));
    }

    #[tokio::test]
    async fn chunked_oversized_upload_returns_413_without_stalling() {
        let path = std::env::temp_dir().join(format!(
            "chatx-upload-limit-{}-{}.json", std::process::id(), now_ms()));
        let mut state = AppState::load(path.clone(), None).unwrap();
        let desktop = format!("d_{}", "a".repeat(32));
        state.mcp_gateway.desktop_id = Some(desktop.clone());
        state.registry.lock().unwrap().desktops.insert(desktop, DesktopRecord {
            token_hash: token_hash("upload-token"),
            device_name: "test".into(), app_version: "test".into(),
            platform: "test".into(), created_at: now_ms(), devices: HashMap::new(),
        });
        let (tx, mut packets) = mpsc::channel(64);
        *state.mcp_gateway.active.write().await = Some(Active {
            session_id: "upload".into(), tx, version: 2,
            credential_hash: token_hash("upload-token"),
            close: watch::channel(false).0,
        });
        let frame = Bytes::from(vec![0x21; MAX_CHUNK]);
        let data = stream::iter((0..=mcp_stream::MAX_REQUEST_BYTES / MAX_CHUNK)
            .map(move |_| Ok::<Bytes, io::Error>(frame.clone())));
        let draining = tokio::spawn(async move {
            while packets.recv().await.is_some() {}
        });
        let response = tokio::time::timeout(Duration::from_secs(20), public_request(
            State(state), Method::POST, "/mcp".parse().unwrap(),
            HeaderMap::new(), Body::from_stream(data),
        )).await.expect("streamed over-limit upload hung");
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        draining.abort();
        let _ = std::fs::remove_file(path);
    }

    #[tokio::test]
    async fn canceled_http_reader_sends_cancel_even_with_full_wss_queue() {
        let id = "a".repeat(32);
        let (tx, mut rx) = mpsc::channel(1);
        tx.try_send(Outgoing::Text("occupied".into())).unwrap();
        let guard = PendingGuard {
            pending: Arc::new(Mutex::new(HashMap::new())),
            request_id: id.clone(),
            errored: Arc::new(AtomicBool::new(false)),
            upload: None, cancel: Some(tx), finished: false,
        };
        drop(guard);
        assert!(matches!(rx.recv().await, Some(Outgoing::Text(x)) if x=="occupied"));
        let next = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await.unwrap().unwrap();
        let Outgoing::Text(next) = next else { panic!("expected cancel control frame") };
        let packet: Value = serde_json::from_str(&next).unwrap();
        assert_eq!(packet["type"], "request_cancel");
        assert_eq!(packet["requestId"], id);
    }

    #[tokio::test]
    async fn stale_session_cannot_complete_request() {
        let gateway = Gateway {
            desktop_id: None,
            active: Arc::new(RwLock::new(None)),
            pending: Arc::new(Mutex::new(HashMap::new())),
        };
        let (tx, mut rx) = mpsc::channel(4);
        let id = "a".repeat(32);
        gateway.pending.lock().unwrap().insert(id.clone(), Pending {
            session_id: "right".into(), tx, bytes: 0,
            errored: Arc::new(AtomicBool::new(false)),
        });
        gateway.receive("wrong", 1, Some(&json!({"type":"head","requestId":id,"status":200})), None).await;
        assert!(rx.try_recv().is_err());
        gateway.receive("right", 1, Some(&json!({"type":"head","requestId":id,"status":200})), None).await;
        assert!(matches!(rx.try_recv(), Ok(Reply::Head(200, _))));
    }
}
