// Dedicated native outbound WSS transport for the loopback HTTPS/OAuth MCP Bridge.
// Reuses the encrypted Relay desktop credential, not Android Monitor permissions.
use crate::relay::RelayCredentials;
use chatx_relay_protocol::mcp_stream::{self, Kind};
use futures_util::stream;
use reqwest::Body;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use futures_util::{SinkExt, StreamExt};
use reqwest::header::{HeaderName, HeaderValue};
use serde_json::{json, Value};
use std::{
    collections::HashMap,
    io,
    sync::{atomic::{AtomicBool, Ordering}, Arc, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::sync::{mpsc, Semaphore};
use bytes::Bytes;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{
        client::IntoClientRequest,
        http::{header::AUTHORIZATION, HeaderValue as WsHeaderValue},
        Message,
    },
};

const LEGACY_MAX_REQUEST: usize = 5 * 1024 * 1024;
const CHUNK_BYTES: usize = mcp_stream::MAX_FRAME_BODY;
const SESSION_TIMEOUT: Duration = Duration::from_secs(45);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const RETRY_SECONDS: [u64; 5] = [1, 2, 5, 10, 30];

pub(super) struct WssClient {
    stop: Arc<AtomicBool>,
    connected: Arc<AtomicBool>,
    last_error: Arc<Mutex<String>>,
    thread: Option<JoinHandle<()>>,
}

impl WssClient {
    pub(super) fn start(base_url: String, local_port: u16, creds: RelayCredentials,
        retry: Arc<AtomicBool>) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let connected = Arc::new(AtomicBool::new(false));
        let stop_signal = stop.clone();
        let connected_signal = connected.clone();
        let last_error = Arc::new(Mutex::new(String::new()));
        let worker_error = last_error.clone();
        let worker = thread::spawn(move || {
            let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                .enable_all().build() else { return; };
            runtime.block_on(async {
                let mut attempts = 0usize;
                loop {
                    if stop_signal.load(Ordering::SeqCst) { break; }
                    let outcome = session(&base_url, local_port, &creds,
                        stop_signal.clone(), connected_signal.clone()).await;
                    connected_signal.store(false, Ordering::SeqCst);
                    if let Ok(mut error) = worker_error.lock() {
                        *error = outcome.err().unwrap_or_default();
                    }
                    if stop_signal.load(Ordering::SeqCst) || !retry.load(Ordering::SeqCst) { break; }
                    // Only transport diagnostics are stored; no MCP payloads
                    // or OAuth credentials are included in these errors.
                    let delay = RETRY_SECONDS[attempts.min(RETRY_SECONDS.len() - 1)];
                    attempts = attempts.saturating_add(1);
                    tokio::select! {
                        _ = stopped(stop_signal.clone()) => break,
                        _ = tokio::time::sleep(Duration::from_secs(delay)) => {}
                    }
                }
            });
        });
        Self { stop, connected, last_error, thread: Some(worker) }
    }

    pub(super) fn alive(&self) -> bool {
        self.thread.as_ref().is_some_and(|thread| !thread.is_finished())
    }
    pub(super) fn connected(&self) -> bool {
        self.alive() && self.connected.load(Ordering::SeqCst)
    }
    pub(super) fn last_error(&self) -> Option<String> {
        self.last_error.lock().ok().map(|error| error.clone())
            .filter(|error| !error.is_empty())
    }
    pub(super) fn stop(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() { let _ = thread.join(); }
    }
}

async fn stopped(stop: Arc<AtomicBool>) {
    while !stop.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn allowed_path(path: &str) -> bool {
    matches!(path,
        "/mcp" |
        "/chatx-oauth/authorize" | "/chatx-oauth/approve" |
        "/chatx-oauth/token" | "/chatx-oauth/register" |
        "/.well-known/oauth-authorization-server" |
        "/.well-known/oauth-protected-resource" |
        "/.well-known/oauth-protected-resource/mcp")
}
fn allowed_header(name: &str) -> bool {
    matches!(name, "content-type" | "accept" | "authorization" | "mcp-session-id" |
        "mcp-protocol-version" | "last-event-id" | "user-agent" |
        "x-forwarded-for" | "origin" | "accept-language")
}
fn allowed_response_header(name: &str) -> bool {
    matches!(name, "content-type" | "cache-control" | "www-authenticate" |
        "location" | "mcp-session-id" | "retry-after" | "x-content-type-options" |
        "content-security-policy" | "x-frame-options" | "referrer-policy" |
        "allow" | "access-control-allow-origin" |
        "access-control-allow-methods" | "access-control-allow-headers" |
        "access-control-expose-headers" | "vary")
}

enum Outgoing {
    Text(Value),
    Binary(Vec<u8>),
}
struct Upload {
    tx: mpsc::Sender<Bytes>,
    bytes: usize,
}
struct Jobs(HashMap<String, tokio::task::JoinHandle<()>>);
impl Drop for Jobs {
    fn drop(&mut self) {
        for (_, job) in self.0.drain() { job.abort(); }
    }
}

fn enqueue_upload(
    uploads: &mut HashMap<String, Upload>, jobs: &mut Jobs,
    outbound: &mpsc::Sender<Outgoing>, id: &str, data: &[u8],
) {
    let Some(upload) = uploads.get_mut(id) else { return };
    upload.bytes = upload.bytes.saturating_add(data.len());
    // No .await: a slow local HTTP consumer must not block the
    // shared WSS receive loop, other calls or heartbeat processing.
    let over_limit = upload.bytes > mcp_stream::MAX_REQUEST_BYTES;
    let result = if over_limit { None }
        else { Some(upload.tx.try_send(Bytes::copy_from_slice(data))) };
    match result {
        Some(Ok(())) => {}
        Some(Err(mpsc::error::TrySendError::Closed(_))) => {
            uploads.remove(id); // Local Bridge answered without reading request body.
        }
        _ => {
            uploads.remove(id);
            if let Some(job) = jobs.0.remove(id) { job.abort(); }
            let packet = Outgoing::Text(json!({
                "type":"error", "requestId":id
            }));
            if let Err(mpsc::error::TrySendError::Full(packet)) = outbound.try_send(packet) {
                let tx = outbound.clone();
                tokio::spawn(async move {
                    let _ = tokio::time::timeout(Duration::from_secs(5),
                        tx.send(packet)).await;
                });
            }
            eprintln!("[chatx-mcp] slow upload consumer; only this request aborted");
        }
    }
}

async fn local_request(
    packet: Value, local_port: u16, client: reqwest::Client,
    tx: mpsc::Sender<Outgoing>, version: u8, upload: Option<mpsc::Receiver<Bytes>>,
) {
    let Some(request_id) = packet["requestId"].as_str() else { return; };
    let request_id = request_id.to_string();
    let run = async {
        if request_id.len() != 32 ||
            !request_id.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err("Invalid request ID");
        }
        let method = packet["method"].as_str().ok_or("Missing method")?
            .parse::<reqwest::Method>().map_err(|_| "Invalid method")?;
        if !matches!(method, reqwest::Method::GET | reqwest::Method::POST |
            reqwest::Method::DELETE | reqwest::Method::HEAD | reqwest::Method::OPTIONS) {
            return Err("Method not allowed");
        }
        let path = packet["path"].as_str().ok_or("Missing path")?;
        if !path.starts_with('/') || path.starts_with("//") ||
            path.contains('\\') || path.contains('#') {
            return Err("Invalid target");
        }
        let url = reqwest::Url::parse(&format!("http://127.0.0.1:{local_port}{path}"))
            .map_err(|_| "Invalid target")?;
        if url.host_str() != Some("127.0.0.1") || url.port() != Some(local_port) ||
            !allowed_path(url.path()) {
            return Err("Target is not allowed");
        }
        let mut request = client.request(method, url);
        if version == 2 {
            let rx = upload.ok_or("Missing binary request stream")?;
            if packet["bodyEncoding"] != "binary-v2" { return Err("Invalid upload encoding"); }
            let body = stream::unfold(rx, |mut rx| async move {
                rx.recv().await.map(|chunk| (Ok::<Bytes, io::Error>(chunk), rx))
            });
            request = request.body(Body::wrap_stream(body));
        } else {
            let encoded = packet["bodyBase64"].as_str().ok_or("Missing body")?;
            if encoded.len() > LEGACY_MAX_REQUEST * 4 / 3 + 8 { return Err("Request too large"); }
            let body = STANDARD.decode(encoded).map_err(|_| "Invalid body")?;
            if body.len() > LEGACY_MAX_REQUEST { return Err("Request too large"); }
            request = request.body(body);
        }
        if let Some(headers) = packet["headers"].as_object() {
            for (name, raw_value) in headers {
                if !allowed_header(name) { continue; }
                let Some(value) = raw_value.as_str() else { continue; };
                let Ok(name) = name.parse::<HeaderName>() else { continue; };
                let Ok(value) = value.parse::<HeaderValue>() else { continue; };
                request = request.header(name, value);
            }
        }
        let mut response = request.send().await.map_err(|_| "Local MCP unreachable")?;
        let status = response.status().as_u16();
        let headers: HashMap<String, String> = response.headers().iter()
            .filter_map(|(name, value)| {
                let key = name.as_str();
                (allowed_response_header(key)).then(|| {
                    value.to_str().ok().map(|v| (key.to_string(), v.to_string()))
                }).flatten()
            }).collect();
        tx.send(Outgoing::Text(json!({"type":"head","requestId":request_id,
            "status":status,"headers":headers}))).await.map_err(|_| "WSS closed")?;
        let mut sent = 0usize;
        while let Some(chunk) = response.chunk().await.map_err(|_| "Local MCP stream error")? {
            sent = sent.saturating_add(chunk.len());
            let limit = if version == 2 { mcp_stream::MAX_RESPONSE_BYTES }
                else { 16 * 1024 * 1024 };
            if sent > limit { return Err("MCP response too large"); }
            for piece in chunk.chunks(CHUNK_BYTES) {
                let frame = if version == 2 {
                    Outgoing::Binary(mcp_stream::encode(
                        Kind::ResponseBody, &request_id, piece).ok_or("Frame encode failed")?)
                } else {
                    Outgoing::Text(json!({"type":"chunk","requestId":request_id,
                        "data":STANDARD.encode(piece)}))
                };
                tx.send(frame).await.map_err(|_| "WSS closed")?;
            }
        }
        tx.send(Outgoing::Text(json!({"type":"end","requestId":request_id}))).await
            .map_err(|_| "WSS closed")?;
        Ok(())
    };
    if run.await.is_err() {
        let _ = tx.send(Outgoing::Text(json!({"type":"error","requestId":request_id}))).await;
    }
}

async fn session(
    base_url: &str, local_port: u16, creds: &RelayCredentials,
    stop: Arc<AtomicBool>, connected: Arc<AtomicBool>,
) -> Result<(), String> {
    let url = crate::relay::websocket_url(base_url,
        &format!("/v1/ws/mcp?desktopId={}", creds.desktop_id))?;
    let mut req = url.into_client_request()
        .map_err(|e| format!("WSS request failed: {e}"))?;
    req.headers_mut().insert(AUTHORIZATION,
        WsHeaderValue::from_str(&format!("Bearer {}", creds.desktop_token))
            .map_err(|_| "Invalid desktop credential".to_string())?);
    let socket = tokio::select! {
        _ = stopped(stop.clone()) => return Ok(()),
        result = tokio::time::timeout(CONNECT_TIMEOUT, connect_async(req)) => {
            result.map_err(|_| "WSS connect timeout".to_string())?
                .map_err(|error| format!("WSS connect failed: {error}"))?.0
        }
    };
    let (mut sink, mut stream) = socket.split();
    sink.send(Message::Text(json!({
        "type":"hello", "protocolVersion":1, "supportedVersions":[1,2]
    }).to_string().into())).await.map_err(|e| e.to_string())?;
    let (tx, mut rx) = mpsc::channel::<Outgoing>(32);
    let limit = Arc::new(Semaphore::new(mcp_stream::MAX_INFLIGHT));
    let mut uploads: HashMap<String, Upload> = HashMap::new();
    let mut jobs = Jobs(HashMap::new());
    let mut version = 1u8;
    let client = reqwest::Client::builder().no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(600))
        .build().map_err(|e| e.to_string())?;
    let mut heartbeat = tokio::time::interval(Duration::from_secs(15));
    let mut last_seen = tokio::time::Instant::now();
    let mut accepted = false;
    loop {
        tokio::select! {
            _ = stopped(stop.clone()) => return Ok(()),
            _ = heartbeat.tick() => {
                if last_seen.elapsed() > SESSION_TIMEOUT {
                    return Err("WSS heartbeat timeout".into());
                }
                sink.send(Message::Text(json!({"type":"ping"})
                    .to_string().into())).await.map_err(|e| e.to_string())?;
            }
            outbound = rx.recv() => {
                let Some(outbound) = outbound else { return Ok(()); };
                let frame = match outbound {
                    Outgoing::Text(value) => Message::Text(value.to_string().into()),
                    Outgoing::Binary(bytes) => Message::Binary(bytes.into()),
                };
                sink.send(frame).await.map_err(|e| e.to_string())?;
            }
            inbound = stream.next() => {
                let Some(Ok(message)) = inbound else {
                    return Err("WSS disconnected".into());
                };
                last_seen = tokio::time::Instant::now();
                match message {
                    Message::Text(text) => {
                        let Ok(packet) = serde_json::from_str::<Value>(&text) else { continue; };
                        match packet["type"].as_str() {
                            Some("hello_ack") => {
                                if !packet["sessionId"].is_string() { return Err("Invalid WSS ack".into()); }
                                version = if packet["protocolVersion"] == 2 { 2 } else { 1 };
                                accepted = true;
                                connected.store(true, Ordering::SeqCst);
                                eprintln!("[chatx-mcp] WSS connected, protocol=v{version}");
                            }
                            Some("request") if accepted => {
                                let Some(id) = packet["requestId"].as_str().map(str::to_string)
                                    else { continue };
                                if id.len() != 32 || !id.bytes().all(|b| b.is_ascii_hexdigit()) {
                                    continue;
                                }
                                if let Ok(permit) = limit.clone().try_acquire_owned() {
                                    let client = client.clone();
                                    let tx = tx.clone();
                                    let rx = if version == 2 && packet["bodyEncoding"] == "binary-v2" {
                                        let (send, receive) = mpsc::channel(32);
                                        uploads.insert(id.clone(), Upload { tx: send, bytes: 0 });
                                        Some(receive)
                                    } else { None };
                                    let job = tokio::spawn(async move {
                                        let _permit = permit;
                                        local_request(packet, local_port, client, tx, version, rx).await;
                                    });
                                    jobs.0.retain(|_, task| !task.is_finished());
                                    jobs.0.insert(id, job);
                                } else {
                                    tx.send(Outgoing::Text(json!({"type":"error","requestId":id})))
                                        .await.map_err(|e| e.to_string())?;
                                }
                            }
                            Some("request_end") if version == 2 => {
                                if let Some(id) = packet["requestId"].as_str() {
                                    uploads.remove(id);
                                }
                            }
                            Some("request_cancel") if version == 2 => {
                                if let Some(id) = packet["requestId"].as_str() {
                                    uploads.remove(id);
                                    if let Some(task) = jobs.0.remove(id) { task.abort(); }
                                }
                            }
                            Some("pong") => {},
                            _ => {},
                        }
                    }
                    Message::Ping(data) => {
                        sink.send(Message::Pong(data)).await.map_err(|e| e.to_string())?;
                    }
                    Message::Binary(frame) if accepted && version == 2 => {
                        let Some((Kind::RequestBody, id, data)) = mcp_stream::decode(&frame)
                            else { return Err("Invalid binary request frame".into()) };
                        enqueue_upload(&mut uploads, &mut jobs, &tx, &id, data);
                    }
                    Message::Close(_) => return Err("WSS closed".into()),
                    _ => {},
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn wss_roundtrip_reaches_only_loopback_bridge() {
        use std::io::{Read, Write};
        use tokio_tungstenite::accept_hdr_async;
        // No public host or real OAuth secret is used in this test.
        let http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let http_port = http.local_addr().unwrap().port();
        let http_worker = std::thread::spawn(move || {
            let (mut socket, _) = http.accept().unwrap();
            socket.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
            let mut request = [0u8; 8192];
            let size = socket.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..size])
                .starts_with("GET /.well-known/oauth-authorization-server "));
            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 14\r\nConnection: close\r\n\r\n{\"issuer\":123}").unwrap();
        });
        let ws_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_port = ws_listener.local_addr().unwrap().port();
        let ws_worker = tokio::spawn(async move {
            let (stream, _) = ws_listener.accept().await.unwrap();
            let mut ws = accept_hdr_async(stream, |request: &tokio_tungstenite::tungstenite::handshake::server::Request,
                        response: tokio_tungstenite::tungstenite::handshake::server::Response| {
                assert_eq!(request.headers()["authorization"], "Bearer test-desktop-token");
                Ok(response)
            }).await.unwrap();
            let first = ws.next().await.unwrap().unwrap().into_text().unwrap();
            assert_eq!(serde_json::from_str::<Value>(&first).unwrap()["type"], "hello");
            ws.send(Message::Text(json!({"type":"hello_ack","sessionId":"test"})
                .to_string().into())).await.unwrap();
            let id = "a".repeat(32);
            ws.send(Message::Text(json!({
                "type":"request","requestId":id,"method":"GET",
                "path":"/.well-known/oauth-authorization-server",
                "headers":{"accept":"application/json"},"bodyBase64":""
            }).to_string().into())).await.unwrap();
            let mut got_head = false;
            let mut data = Vec::new();
            loop {
                let incoming = tokio::time::timeout(Duration::from_secs(5), ws.next())
                    .await.unwrap().unwrap().unwrap();
                let Message::Text(text) = incoming else { continue; };
                let frame: Value = serde_json::from_str(&text).unwrap();
                match frame["type"].as_str() {
                    Some("ping") => ws.send(Message::Text(json!({"type":"pong"})
                        .to_string().into())).await.unwrap(),
                    Some("head") => {
                        assert_eq!(frame["requestId"], id);
                        assert_eq!(frame["status"], 200);
                        got_head = true;
                    }
                    Some("chunk") => data.extend(STANDARD.decode(
                        frame["data"].as_str().unwrap()).unwrap()),
                    Some("end") => break,
                    Some("error") => panic!("desktop WSS proxy failed: {frame}"),
                    _ => {}
                }
            }
            assert!(got_head);
            assert_eq!(data, br#"{"issuer":123}"#);
            let _ = ws.close(None).await;
        });
        let creds = RelayCredentials {
            base_url: format!("http://127.0.0.1:{ws_port}"),
            desktop_id: format!("d_{}", "1".repeat(32)),
            desktop_token: "test-desktop-token".into(),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let connected = Arc::new(AtomicBool::new(false));
        let client = tokio::spawn({
            let stop = stop.clone();
            let connected = connected.clone();
            let creds = creds.clone();
            async move { session(&creds.base_url, http_port, &creds, stop, connected).await }
        });
        tokio::time::timeout(Duration::from_secs(8), ws_worker).await.unwrap().unwrap();
        stop.store(true, Ordering::SeqCst);
        let _ = client.await.unwrap();
        http_worker.join().unwrap();
    }

    #[tokio::test]
    async fn native_v2_transfers_binary_response_without_base64() {
        use std::io::{Read, Write};
        use tokio_tungstenite::accept_hdr_async;
        let http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = http.local_addr().unwrap().port();
        let body = vec![42u8; 1_500_000];
        let http_job = std::thread::spawn(move || {
            let (mut conn, _) = http.accept().unwrap();
            let mut request = [0u8; 4096];
            let read = conn.read(&mut request).unwrap();
            assert!(String::from_utf8_lossy(&request[..read]).starts_with("GET /mcp "));
            conn.write_all(format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",body.len()).as_bytes()).unwrap();
            for chunk in body.chunks(32768) { conn.write_all(chunk).unwrap(); }
        });
        let ws_server = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let ws_port = ws_server.local_addr().unwrap().port();
        let ws_job = tokio::spawn(async move {
            let (stream, _) = ws_server.accept().await.unwrap();
            let mut ws = accept_hdr_async(stream, |_req: &tokio_tungstenite::tungstenite::handshake::server::Request,
                response: tokio_tungstenite::tungstenite::handshake::server::Response| Ok(response)).await.unwrap();
            let hello: Value = serde_json::from_str(&ws.next().await.unwrap().unwrap()
                .into_text().unwrap()).unwrap();
            assert!(hello["supportedVersions"].as_array().unwrap().contains(&json!(2)));
            ws.send(Message::Text(json!({
                "type":"hello_ack", "protocolVersion":2,"sessionId":"v2"
            }).to_string().into())).await.unwrap();
            let id = "c".repeat(32);
            ws.send(Message::Text(json!({
                "type":"request", "requestId":id, "method":"GET",
                "path":"/mcp", "headers":{},"bodyEncoding":"binary-v2"
            }).to_string().into())).await.unwrap();
            ws.send(Message::Text(json!({
                "type":"request_end","requestId":id
            }).to_string().into())).await.unwrap();
            let mut bytes = 0;
            let mut saw_head = false;
            loop {
                let msg = tokio::time::timeout(Duration::from_secs(8), ws.next())
                    .await.unwrap().unwrap().unwrap();
                match msg {
                    Message::Text(text) => {
                        let packet: Value = serde_json::from_str(&text).unwrap();
                        match packet["type"].as_str() {
                            Some("head") => { assert_eq!(packet["status"], 200); saw_head = true; }
                            Some("end") => break,
                            Some("ping") => ws.send(Message::Text(json!({"type":"pong"})
                                .to_string().into())).await.unwrap(),
                            Some("error") => panic!("WSS v2 failed"),
                            _ => {}
                        }
                    }
                    Message::Binary(frame) => {
                        let (kind, actual, chunk) = mcp_stream::decode(&frame).unwrap();
                        assert_eq!(kind, Kind::ResponseBody);
                        assert_eq!(actual, id);
                        assert!(chunk.iter().all(|byte| *byte == 42));
                        bytes += chunk.len();
                    }
                    _ => {}
                }
            }
            assert!(saw_head);
            assert_eq!(bytes, 1_500_000);
            let _ = ws.close(None).await;
        });
        let creds = RelayCredentials {
            base_url: format!("http://127.0.0.1:{ws_port}"),
            desktop_id: format!("d_{}", "1".repeat(32)),
            desktop_token: "test".into(),
        };
        let stop = Arc::new(AtomicBool::new(false));
        let connected = Arc::new(AtomicBool::new(false));
        let worker = tokio::spawn({
            let stop = stop.clone();
            let connected = connected.clone();
            let creds = creds.clone();
            async move { session(&creds.base_url, port, &creds, stop, connected).await }
        });
        tokio::time::timeout(Duration::from_secs(12), ws_job).await.unwrap().unwrap();
        stop.store(true, Ordering::SeqCst);
        let _ = worker.await.unwrap();
        http_job.join().unwrap();
    }

    #[tokio::test]
    async fn saturated_binary_upload_aborts_only_one_request() {
        let slow_id = "a".repeat(32);
        let fast_id = "b".repeat(32);
        let (slow_tx, _slow_rx) = mpsc::channel(1);
        slow_tx.try_send(Bytes::from_static(b"full")).unwrap();
        let (fast_tx, mut fast_rx) = mpsc::channel(1);
        let mut uploads = HashMap::from([
            (slow_id.clone(), Upload { tx: slow_tx, bytes: 0 }),
            (fast_id.clone(), Upload { tx: fast_tx, bytes: 0 }),
        ]);
        let (tx, mut outbound) = mpsc::channel(2);
        let mut jobs = Jobs(HashMap::new());
        enqueue_upload(&mut uploads, &mut jobs, &tx, &slow_id, b"slow");
        assert!(!uploads.contains_key(&slow_id));
        assert!(uploads.contains_key(&fast_id));
        let Outgoing::Text(error) = outbound.try_recv().unwrap() else {
            panic!("expected per-request cancellation");
        };
        assert_eq!(error["type"], "error");
        assert_eq!(error["requestId"], slow_id);
        enqueue_upload(&mut uploads, &mut jobs, &tx, &fast_id, b"ok");
        assert_eq!(fast_rx.try_recv().unwrap(), Bytes::from_static(b"ok"));
    }

    #[test]
    fn forbids_local_ssrf_and_token_forwarding() {
        assert!(allowed_path("/mcp"));
        assert!(allowed_path("/chatx-oauth/authorize"));
        assert!(!allowed_path("/healthz"));
        assert!(!allowed_header("host"));
        assert!(!allowed_header("cookie"));
        assert!(!allowed_header("x-chatx-instance-nonce"));
        assert!(!allowed_response_header("set-cookie"));
    }
}
