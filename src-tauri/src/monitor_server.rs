use chatx_relay_protocol::{
    device_capabilities, DeviceServerMessage, DeviceWsMessage, EncryptedControlPayload,
    EncryptedSnapshot, PairingServerMessage, PairingWsMessage, PROTOCOL_VERSION,
};
use rcgen::{generate_simple_self_signed, CertifiedKey};
use ring::rand::{SecureRandom, SystemRandom};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};
use serde::Serialize;
use sha2::{Digest, Sha256};
use socket2::{Domain, Protocol, Socket, Type};
use std::{
    collections::HashMap,
    fs::{self, OpenOptions},
    io::{Cursor, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tungstenite::{
    accept_hdr,
    handshake::server::{Request, Response},
    Error as WsError,
    Message,
    WebSocket,
};

const MAX_ACTIVE_CONNECTIONS: usize = 32;
const MAX_WS_TEXT_BYTES: usize = 128 * 1024;
const MAX_HTTP_HEADER_BYTES: usize = 16 * 1024;
const IO_POLL_TIMEOUT: Duration = Duration::from_millis(500);
const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const SNAPSHOT_INTERVAL: Duration = Duration::from_secs(10);
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);
const IDLE_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Clone)]
pub struct MonitorTlsIdentity {
    config: Arc<ServerConfig>,
    #[cfg(test)]
    certificate_der: Vec<u8>,
    fingerprint_sha256: String,
}

#[cfg(test)]
#[derive(Clone)]
pub struct AuthTokenHash([u8; 32]);

#[cfg(test)]
impl AuthTokenHash {
    pub fn from_token(token: &str) -> Self {
        let digest = Sha256::digest(token.as_bytes());
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&digest);
        Self(bytes)
    }

    pub fn verify(&self, token: &str) -> bool {
        let candidate = Self::from_token(token);
        constant_time_eq(&self.0, &candidate.0)
    }
}

pub fn token_hash_hex(token: &str) -> String {
    hex(&Sha256::digest(token.as_bytes()))
}

pub fn verify_token_hash_hex(token: &str, expected_hex: &str) -> bool {
    let candidate = token_hash_hex(token);
    constant_time_eq(candidate.as_bytes(), expected_hex.as_bytes())
}

pub type AuthChecker = Arc<dyn Fn(&str, &str) -> bool + Send + Sync>;
pub type PairHandler = Arc<dyn Fn(PairingWsMessage) -> Result<PairingServerMessage, String> + Send + Sync>;
pub type RevokeHandler = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;
pub type ControlHandler = Arc<
    dyn Fn(&str, EncryptedControlPayload) -> Result<EncryptedControlPayload, String> + Send + Sync
>;
pub type SnapshotProvider =
    Arc<dyn Fn(&str, &str, u64) -> Result<EncryptedSnapshot, String> + Send + Sync>;

struct ConnectionGuard(Arc<AtomicUsize>);

impl Drop for ConnectionGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct MonitorServerHandle {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
    pub addr: SocketAddr,
    pub fingerprint_sha256: String,
}

impl Drop for MonitorServerHandle {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}
pub fn generate_monitor_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "生成 Monitor Token 失败。".to_string())?;
    Ok(hex(&bytes))
}

pub fn ensure_tls_identity(dir: &Path) -> Result<MonitorTlsIdentity, String> {
    fs::create_dir_all(dir).map_err(|e| format!("创建 Monitor TLS 目录失败：{e}"))?;
    let cert_path = dir.join("monitor-cert.der");
    let key_path = dir.join("monitor-key.der");

    let (cert_der, key_der) = if cert_path.is_file() && key_path.is_file() {
        (
            fs::read(&cert_path).map_err(|e| format!("读取 Monitor TLS 证书失败：{e}"))?,
            fs::read(&key_path).map_err(|e| format!("读取 Monitor TLS 私钥失败：{e}"))?,
        )
    } else {
        let CertifiedKey { cert, signing_key } = generate_simple_self_signed(vec![
            "chatx.local".to_string(),
            "localhost".to_string(),
        ])
        .map_err(|e| format!("生成 Monitor TLS 证书失败：{e}"))?;
        let cert_der = cert.der().to_vec();
        let key_der = signing_key.serialize_der();
        fs::write(&cert_path, &cert_der)
            .map_err(|e| format!("保存 Monitor TLS 证书失败：{e}"))?;
        write_private_file(&key_path, &key_der)?;
        (cert_der, key_der)
    };

    let config = build_tls_config(&cert_der, &key_der)?;
    let fingerprint_sha256 = hex(&Sha256::digest(&cert_der));
    Ok(MonitorTlsIdentity {
        config,
        #[cfg(test)]
        certificate_der: cert_der,
        fingerprint_sha256,
    })
}

fn write_private_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(path)
        .map_err(|e| format!("创建 Monitor TLS 私钥失败：{e}"))?;
    file.write_all(bytes)
        .map_err(|e| format!("保存 Monitor TLS 私钥失败：{e}"))
}

fn build_tls_config(cert_der: &[u8], key_der: &[u8]) -> Result<Arc<ServerConfig>, String> {
    let provider = rustls::crypto::ring::default_provider();
    let config = ServerConfig::builder_with_provider(Arc::new(provider))
        .with_safe_default_protocol_versions()
        .map_err(|e| format!("初始化 Monitor TLS 协议失败：{e}"))?
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(cert_der.to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key_der.to_vec())),
        )
        .map_err(|e| format!("加载 Monitor TLS 证书失败：{e}"))?;
    Ok(Arc::new(config))
}

pub fn start_monitor_server(
    bind: SocketAddr,
    identity: MonitorTlsIdentity,
    desktop_id: String,
    auth_checker: AuthChecker,
    pair_handler: PairHandler,
    revoke_handler: RevokeHandler,
    control_handler: ControlHandler,
    snapshot_provider: SnapshotProvider,
) -> Result<MonitorServerHandle, String> {
    let listener = bind_monitor_listener(bind)?;
    start_monitor_server_with_listener(
        listener,
        identity,
        desktop_id,
        auth_checker,
        pair_handler,
        revoke_handler,
        control_handler,
        snapshot_provider,
    )
}

fn bind_monitor_listener(bind: SocketAddr) -> Result<TcpListener, String> {
    let domain = if bind.is_ipv4() { Domain::IPV4 } else { Domain::IPV6 };
    let socket = Socket::new(domain, Type::STREAM, Some(Protocol::TCP))
        .map_err(|e| format!("创建 Monitor socket 失败：{e}"))?;
    socket.set_reuse_address(true)
        .map_err(|e| format!("配置 Monitor SO_REUSEADDR 失败：{e}"))?;
    if bind.is_ipv6() {
        socket.set_only_v6(true)
            .map_err(|e| format!("配置 Monitor IPv6-only 失败：{e}"))?;
    }
    socket.bind(&bind.into())
        .map_err(|e| format!("绑定 Monitor Server {bind} 失败：{e}"))?;
    socket.listen(128)
        .map_err(|e| format!("监听 Monitor Server 失败：{e}"))?;
    let listener: TcpListener = socket.into();
    listener.set_nonblocking(true)
        .map_err(|e| format!("配置 Monitor Server 失败：{e}"))?;
    Ok(listener)
}
fn start_monitor_server_with_listener(
    listener: TcpListener,
    identity: MonitorTlsIdentity,
    desktop_id: String,
    auth_checker: AuthChecker,
    pair_handler: PairHandler,
    revoke_handler: RevokeHandler,
    control_handler: ControlHandler,
    snapshot_provider: SnapshotProvider,
) -> Result<MonitorServerHandle, String> {
    let addr = listener
        .local_addr()
        .map_err(|e| format!("读取 Monitor Server 地址失败：{e}"))?;
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = stop.clone();
    let tls_config = identity.config.clone();
    let fingerprint_sha256 = identity.fingerprint_sha256.clone();
    let active_connections = Arc::new(AtomicUsize::new(0));

    let thread = thread::spawn(move || {
        while !thread_stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((stream, _)) => {
                    if active_connections.fetch_add(1, Ordering::SeqCst)
                        >= MAX_ACTIVE_CONNECTIONS
                    {
                        active_connections.fetch_sub(1, Ordering::SeqCst);
                        drop(stream);
                        continue;
                    }
                    let config = tls_config.clone();
                    let desktop_id = desktop_id.clone();
                    let auth = auth_checker.clone();
                    let pairing = pair_handler.clone();
                    let revoke = revoke_handler.clone();
                    let control = control_handler.clone();
                    let snapshots = snapshot_provider.clone();
                    let connection_count = active_connections.clone();
                    thread::spawn(move || {
                        let _guard = ConnectionGuard(connection_count);
                        if let Err(error) = handle_connection(
                            stream,
                            config,
                            &desktop_id,
                            auth,
                            pairing,
                            revoke,
                            control,
                            snapshots,
                        ) {
                            eprintln!("ChatX Monitor WSS connection error: {error}");
                        }
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(40));
                }
                Err(_) => thread::sleep(Duration::from_millis(100)),
            }
        }
    });

    Ok(MonitorServerHandle {
        stop,
        thread: Some(thread),
        addr,
        fingerprint_sha256,
    })
}

#[derive(Debug, Clone, Default)]
struct HandshakeContext {
    path: String,
    bearer_token: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct HttpRequestHead {
    method: String,
    path: String,
    bearer_token: Option<String>,
    websocket_upgrade: bool,
}

struct PrefixedStream<S> {
    prefix: Cursor<Vec<u8>>,
    inner: S,
}

impl<S> PrefixedStream<S> {
    fn new(prefix: Vec<u8>, inner: S) -> Self {
        Self {
            prefix: Cursor::new(prefix),
            inner,
        }
    }
}

impl<S: Read> Read for PrefixedStream<S> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.prefix.position() < self.prefix.get_ref().len() as u64 {
            let read = self.prefix.read(buf)?;
            if read > 0 {
                return Ok(read);
            }
        }
        self.inner.read(buf)
    }
}

impl<S: Write> Write for PrefixedStream<S> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

fn read_http_request_head<S: Read>(stream: &mut S) -> Result<(Vec<u8>, HttpRequestHead), String> {
    let started = Instant::now();
    let mut bytes = Vec::with_capacity(2048);
    let mut chunk = [0u8; 1024];
    loop {
        if bytes.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
        if bytes.len() >= MAX_HTTP_HEADER_BYTES {
            return Err("Monitor HTTP 请求头过大。".into());
        }
        if started.elapsed() > HELLO_TIMEOUT {
            return Err("Monitor HTTP 请求头超时。".into());
        }
        match stream.read(&mut chunk) {
            Ok(0) => return Err("Monitor HTTP 连接已关闭。".into()),
            Ok(read) => bytes.extend_from_slice(&chunk[..read]),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(format!("读取 Monitor HTTP 请求失败：{error}")),
        }
    }

    let header_end = bytes.windows(4)
        .position(|window| window == b"\r\n\r\n")
        .map(|index| index + 4)
        .ok_or_else(|| "Monitor HTTP 请求头无效。".to_string())?;
    let text = String::from_utf8_lossy(&bytes[..header_end]);
    let mut lines = text.split("\r\n");
    let request_line = lines.next().unwrap_or_default();
    let mut request_parts = request_line.split_whitespace();
    let method = request_parts.next().unwrap_or_default().to_ascii_uppercase();
    let path = request_parts.next().unwrap_or_default().to_string();
    if method.is_empty() || path.is_empty() {
        return Err("Monitor HTTP 请求行无效。".into());
    }

    let mut head = HttpRequestHead {
        method,
        path,
        ..HttpRequestHead::default()
    };
    for line in lines {
        let Some((name, value)) = line.split_once(':') else { continue; };
        let name = name.trim();
        let value = value.trim();
        if name.eq_ignore_ascii_case("authorization") {
            head.bearer_token = value
                .strip_prefix("Bearer ")
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string);
        } else if name.eq_ignore_ascii_case("upgrade")
            && value.eq_ignore_ascii_case("websocket")
        {
            head.websocket_upgrade = true;
        }
    }
    Ok((bytes, head))
}

fn write_http_json<S: Write, T: Serialize>(
    stream: &mut S,
    status: &str,
    value: &T,
) -> Result<(), String> {
    let body = serde_json::to_vec(value)
        .map_err(|e| format!("序列化 Monitor HTTPS 响应失败：{e}"))?;
    write!(
        stream,
        "HTTP/1.1 {status}\r\nContent-Type: application/json; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
        body.len(),
    )
    .map_err(|e| format!("写入 Monitor HTTPS 响应头失败：{e}"))?;
    stream
        .write_all(&body)
        .and_then(|_| stream.flush())
        .map_err(|e| format!("写入 Monitor HTTPS 响应失败：{e}"))
}

fn handle_https_request<S: Write>(
    stream: &mut S,
    desktop_id: &str,
    request: &HttpRequestHead,
    auth_checker: &AuthChecker,
    revoke_handler: &RevokeHandler,
    snapshot_provider: &SnapshotProvider,
) -> Result<(), String> {
    let route = request.path.split('?').next().unwrap_or("");
    let query = parse_query(&request.path);
    let requested_desktop = query.get("desktopId").map(String::as_str).unwrap_or("");
    let device_id = query.get("deviceId").map(String::as_str).unwrap_or("");
    let token = request.bearer_token.as_deref().unwrap_or("");
    let authorized = requested_desktop == desktop_id
        && !device_id.is_empty()
        && !token.is_empty()
        && auth_checker(device_id, token);

    match (request.method.as_str(), route) {
        ("GET", "/v1/monitor/snapshot") if authorized => {
            let session_id = format!("s_{}", &generate_monitor_token()?[..32]);
            match snapshot_provider(device_id, &session_id, 1) {
                Ok(snapshot) => write_http_json(
                    stream,
                    "200 OK",
                    &DeviceServerMessage::Snapshot {
                        received_at: now_ms(),
                        snapshot,
                    },
                ),
                Err(_) => write_http_json(
                    stream,
                    "500 Internal Server Error",
                    &serde_json::json!({ "error": "snapshot unavailable" }),
                ),
            }
        }
        ("POST", "/v1/monitor/revoke") if authorized => {
            match revoke_handler(device_id) {
                Ok(()) => write_http_json(
                    stream,
                    "200 OK",
                    &DeviceServerMessage::Revoked {
                        device_id: device_id.to_string(),
                    },
                ),
                Err(_) => write_http_json(
                    stream,
                    "409 Conflict",
                    &serde_json::json!({ "error": "device revoke failed" }),
                ),
            }
        }
        ("GET", "/v1/monitor/snapshot") | ("POST", "/v1/monitor/revoke") => {
            write_http_json(
                stream,
                "401 Unauthorized",
                &serde_json::json!({ "error": "unauthorized" }),
            )
        }
        _ => write_http_json(
            stream,
            "404 Not Found",
            &serde_json::json!({ "error": "unknown monitor https route" }),
        ),
    }
}

fn handle_connection(
    stream: TcpStream,
    config: Arc<ServerConfig>,
    desktop_id: &str,
    auth_checker: AuthChecker,
    pair_handler: PairHandler,
    revoke_handler: RevokeHandler,
    control_handler: ControlHandler,
    snapshot_provider: SnapshotProvider,
) -> Result<(), String> {
    stream
        .set_nonblocking(false)
        .map_err(|e| format!("配置 Monitor 连接失败：{e}"))?;
    let _ = stream.set_read_timeout(Some(IO_POLL_TIMEOUT));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));

    let connection = ServerConnection::new(config)
        .map_err(|e| format!("创建 Monitor TLS 会话失败：{e}"))?;
    let mut tls = StreamOwned::new(connection, stream);
    let (request_bytes, head) = read_http_request_head(&mut tls)?;

    if !head.websocket_upgrade {
        let result = handle_https_request(
            &mut tls,
            desktop_id,
            &head,
            &auth_checker,
            &revoke_handler,
            &snapshot_provider,
        );
        tls.conn.send_close_notify();
        let _ = tls.flush();
        return result;
    }

    let request = HandshakeContext {
        path: head.path.clone(),
        bearer_token: head.bearer_token.clone(),
    };
    let prefixed = PrefixedStream::new(request_bytes, tls);
    let mut websocket = accept_hdr(
        prefixed,
        |_request: &Request, response: Response| Ok(response),
    )
    .map_err(|e| format!("Monitor WebSocket 握手失败：{e}"))?;
    let route = request.path.split('?').next().unwrap_or("");

    match route {
        "/v1/ws/monitor" => {
            handle_monitor_socket(
                &mut websocket,
                desktop_id,
                &request,
                auth_checker,
                revoke_handler,
                control_handler,
                snapshot_provider,
            )
        }
        "/v1/ws/pair" => handle_pair_socket(&mut websocket, pair_handler),
        _ => {
            let _ = send_json(
                &mut websocket,
                &DeviceServerMessage::Error {
                    message: "unknown monitor websocket route".into(),
                },
            );
            let _ = websocket.close(None);
            Err("未知 Monitor WebSocket 路径。".into())
        }
    }
}
fn handle_monitor_socket<S: Read + Write>(
    websocket: &mut WebSocket<S>,
    desktop_id: &str,
    request: &HandshakeContext,
    auth_checker: AuthChecker,
    revoke_handler: RevokeHandler,
    control_handler: ControlHandler,
    snapshot_provider: SnapshotProvider,
) -> Result<(), String> {
    let query = parse_query(&request.path);
    let requested_desktop = query.get("desktopId").map(String::as_str).unwrap_or("");
    let device_id = query.get("deviceId").map(String::as_str).unwrap_or("");
    let token = request.bearer_token.as_deref().unwrap_or("");

    if requested_desktop != desktop_id
        || device_id.is_empty()
        || token.is_empty()
        || !auth_checker(device_id, token)
    {
        let _ = send_json(
            websocket,
            &DeviceServerMessage::Error {
                message: "unauthorized".into(),
            },
        );
        let _ = websocket.close(None);
        return Err("Monitor WSS 认证失败。".into());
    }

    let hello = read_json_until::<_, DeviceWsMessage>(websocket, HELLO_TIMEOUT)?;
    match hello {
        DeviceWsMessage::Hello {
            protocol_version,
            desktop_id: hello_desktop,
            device_id: hello_device,
        } if protocol_version == PROTOCOL_VERSION
            && hello_desktop == desktop_id
            && hello_device == device_id => {}
        _ => {
            let _ = send_json(
                websocket,
                &DeviceServerMessage::Error {
                    message: "invalid hello".into(),
                },
            );
            let _ = websocket.close(None);
            return Err("Monitor WSS Hello 无效。".into());
        }
    }

    let session_id = format!("s_{}", &generate_monitor_token()?[..32]);
    send_json(
        websocket,
        &DeviceServerMessage::HelloAck {
            session_id: session_id.clone(),
            server_time: now_ms(),
            desktop_online: true,
            capabilities: device_capabilities(),
        },
    )?;

    let mut sequence = 0u64;
    push_snapshot(
        websocket,
        &snapshot_provider,
        device_id,
        &session_id,
        &mut sequence,
    )?;

    let mut last_activity = Instant::now();
    let mut last_snapshot = Instant::now();
    let mut last_ping = Instant::now();

    loop {
        if last_activity.elapsed() > IDLE_TIMEOUT {
            let _ = websocket.close(None);
            return Err("Monitor WSS heartbeat timeout。".into());
        }

        if last_snapshot.elapsed() >= SNAPSHOT_INTERVAL {
            push_snapshot(
                websocket,
                &snapshot_provider,
                device_id,
                &session_id,
                &mut sequence,
            )?;
            last_snapshot = Instant::now();
        }

        if last_ping.elapsed() >= HEARTBEAT_INTERVAL {
            websocket
                .send(Message::Ping(Vec::new().into()))
                .map_err(|e| format!("Monitor WSS Ping 失败：{e}"))?;
            last_ping = Instant::now();
        }

        match websocket.read() {
            Ok(Message::Ping(payload)) => {
                last_activity = Instant::now();
                websocket
                    .send(Message::Pong(payload))
                    .map_err(|e| format!("Monitor WSS Pong 失败：{e}"))?;
            }
            Ok(Message::Pong(_)) => {
                last_activity = Instant::now();
            }
            Ok(Message::Text(text)) => {
                last_activity = Instant::now();
                if text.len() > MAX_WS_TEXT_BYTES {
                    return Err("Monitor WSS 消息过大。".into());
                }
                if let Ok(message) = serde_json::from_str::<DeviceWsMessage>(&text) {
                    match message {
                        DeviceWsMessage::Ping { .. } => {
                            send_json(
                                websocket,
                                &DeviceServerMessage::Pong {
                                    server_time: now_ms(),
                                },
                            )?;
                        }
                        DeviceWsMessage::Control { request } => {
                            match control_handler(device_id, request) {
                                Ok(response) => {
                                    send_json(
                                        websocket,
                                        &DeviceServerMessage::ControlResult { response },
                                    )?;
                                }
                                Err(message) => {
                                    send_json(
                                        websocket,
                                        &DeviceServerMessage::Error { message },
                                    )?;
                                }
                            }
                        }
                        DeviceWsMessage::RevokeSelf => {
                            revoke_handler(device_id)?;
                            send_json(
                                websocket,
                                &DeviceServerMessage::Revoked {
                                    device_id: device_id.to_string(),
                                },
                            )?;
                            let _ = websocket.close(None);
                            return Ok(());
                        }
                        DeviceWsMessage::Hello { .. } => {}
                    }
                }
            }
            Ok(Message::Close(_)) => return Ok(()),
            Ok(_) => {}
            Err(WsError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(format!("Monitor WSS 读取失败：{error}")),
        }
    }
}

fn push_snapshot<S: Read + Write>(
    websocket: &mut WebSocket<S>,
    provider: &SnapshotProvider,
    device_id: &str,
    session_id: &str,
    sequence: &mut u64,
) -> Result<(), String> {
    *sequence = sequence.saturating_add(1);
    let snapshot = provider(device_id, session_id, *sequence)?;
    send_json(
        websocket,
        &DeviceServerMessage::Snapshot {
            received_at: now_ms(),
            snapshot,
        },
    )
}
fn handle_pair_socket<S: Read + Write>(
    websocket: &mut WebSocket<S>,
    pair_handler: PairHandler,
) -> Result<(), String> {
    let request = read_json_until::<_, PairingWsMessage>(websocket, HELLO_TIMEOUT)?;
    match pair_handler(request) {
        Ok(response) => {
            send_json(websocket, &response)?;
            let _ = websocket.close(None);
            Ok(())
        }
        Err(message) => {
            send_json(websocket, &PairingServerMessage::Error { message })?;
            let _ = websocket.close(None);
            Ok(())
        }
    }
}

fn read_json_until<S, T>(
    websocket: &mut WebSocket<S>,
    timeout: Duration,
) -> Result<T, String>
where
    S: Read + Write,
    T: serde::de::DeserializeOwned,
{
    let started = Instant::now();
    loop {
        if started.elapsed() > timeout {
            return Err("Monitor WSS 等待消息超时。".into());
        }
        match websocket.read() {
            Ok(Message::Ping(payload)) => {
                websocket
                    .send(Message::Pong(payload))
                    .map_err(|e| format!("Monitor WSS Pong 失败：{e}"))?;
            }
            Ok(Message::Text(text)) => {
                if text.len() > MAX_WS_TEXT_BYTES {
                    return Err("Monitor WSS 消息过大。".into());
                }
                return serde_json::from_str::<T>(&text)
                    .map_err(|e| format!("Monitor WSS JSON 无效：{e}"));
            }
            Ok(Message::Close(_)) => return Err("Monitor WSS 已关闭。".into()),
            Ok(_) => {}
            Err(WsError::Io(error))
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) => {}
            Err(error) => return Err(format!("Monitor WSS 读取失败：{error}")),
        }
    }
}

fn send_json<S: Read + Write, T: Serialize>(
    websocket: &mut WebSocket<S>,
    value: &T,
) -> Result<(), String> {
    let text = serde_json::to_string(value)
        .map_err(|e| format!("序列化 Monitor WSS 消息失败：{e}"))?;
    websocket
        .send(Message::Text(text.into()))
        .map_err(|e| format!("发送 Monitor WSS 消息失败：{e}"))
}

fn parse_query(uri: &str) -> HashMap<String, String> {
    let mut output = HashMap::new();
    let Some((_, query)) = uri.split_once('?') else {
        return output;
    };
    for part in query.split('&') {
        let Some((key, value)) = part.split_once('=') else { continue; };
        output.insert(percent_decode(key), percent_decode(value));
    }
    output
}

fn percent_decode(value: &str) -> String {
    let mut output = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            let hex = &value[index + 1..index + 3];
            if let Ok(byte) = u8::from_str_radix(hex, 16) {
                output.push(byte);
                index += 3;
                continue;
            }
        }
        output.push(if bytes[index] == b'+' { b' ' } else { bytes[index] });
        index += 1;
    }
    String::from_utf8_lossy(&output).to_string()
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push(DIGITS[(byte >> 4) as usize] as char);
        output.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    output
}
#[cfg(test)]
mod tests {
    use super::*;
    use rustls::client::ClientConnection;
    use rustls::pki_types::ServerName;
    use rustls::{ClientConfig, RootCertStore};
    use std::{env, net::TcpStream, sync::Arc};

    fn temp_dir(name: &str) -> std::path::PathBuf {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        env::temp_dir().join(format!("chatx-{name}-{}-{nonce}", std::process::id()))
    }

    #[test]
    fn auth_token_hash_verifies_without_storing_plaintext() {
        let token = generate_monitor_token().unwrap();
        let hash = AuthTokenHash::from_token(&token);
        assert!(hash.verify(&token));
        assert!(!hash.verify("wrong-token"));
        assert_eq!(token.len(), 64);
    }

    #[test]
    fn tls_identity_is_persistent() {
        let dir = temp_dir("monitor-tls");
        let first = ensure_tls_identity(&dir).unwrap();
        let second = ensure_tls_identity(&dir).unwrap();
        assert_eq!(first.fingerprint_sha256, second.fingerprint_sha256);
        assert_eq!(first.certificate_der, second.certificate_der);
        assert!(dir.join("monitor-key.der").is_file());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn query_parser_handles_monitor_route_metadata() {
        let query = parse_query(
            "/v1/ws/monitor?desktopId=d_abc123&deviceId=dev_001",
        );
        assert_eq!(query.get("desktopId").map(String::as_str), Some("d_abc123"));
        assert_eq!(query.get("deviceId").map(String::as_str), Some("dev_001"));
    }

    #[test]
    fn direct_https_snapshot_requires_auth_and_returns_encrypted_snapshot() {
        let desktop_id = "d_0123456789abcdef0123456789abcdef";
        let device_id = "dev_0123456789abcdef";
        let token = "aa".repeat(32);
        let auth_token = token.clone();
        let auth: AuthChecker = Arc::new(move |candidate_device, candidate_token| {
            candidate_device == device_id && candidate_token == auth_token
        });
        let revoke: RevokeHandler = Arc::new(|_| Ok(()));
        let snapshots: SnapshotProvider = Arc::new(|_, session_id, sequence| {
            Ok(EncryptedSnapshot {
                session_id: session_id.to_string(),
                sequence,
                generated_at: 123,
                nonce: "nonce".into(),
                ciphertext: "ciphertext".into(),
            })
        });
        let request = HttpRequestHead {
            method: "GET".into(),
            path: format!(
                "/v1/monitor/snapshot?desktopId={desktop_id}&deviceId={device_id}"
            ),
            bearer_token: Some(token),
            websocket_upgrade: false,
        };
        let mut output = Vec::new();
        handle_https_request(
            &mut output,
            desktop_id,
            &request,
            &auth,
            &revoke,
            &snapshots,
        ).unwrap();
        let response = String::from_utf8(output).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("\"type\":\"snapshot\""));
        assert!(response.contains("\"sessionId\":\"s_"));
        assert!(response.contains("\"ciphertext\":\"ciphertext\""));
    }

    #[test]
    fn direct_https_round_trip_over_tls() {
        let dir = temp_dir("monitor-https");
        let identity = ensure_tls_identity(&dir).unwrap();
        let desktop_id = "d_0123456789abcdef0123456789abcdef".to_string();
        let device_id = "dev_0123456789abcdef".to_string();
        let token = generate_monitor_token().unwrap();
        let auth_token = token.clone();
        let auth_device = device_id.clone();
        let auth: AuthChecker = Arc::new(move |candidate_device, candidate_token| {
            candidate_device == auth_device && candidate_token == auth_token
        });
        let pairing: PairHandler = Arc::new(|_| Err("pairing disabled in test".into()));
        let revoke: RevokeHandler = Arc::new(|_| Ok(()));
        let control: ControlHandler = Arc::new(|_, _| Err("control disabled in test".into()));
        let snapshots: SnapshotProvider = Arc::new(|_, session_id, sequence| {
            Ok(EncryptedSnapshot {
                session_id: session_id.to_string(),
                sequence,
                generated_at: 123,
                nonce: "nonce".into(),
                ciphertext: "ciphertext".into(),
            })
        });
        let server = start_monitor_server(
            "127.0.0.1:0".parse().unwrap(),
            identity.clone(),
            desktop_id.clone(),
            auth,
            pairing,
            revoke,
            control,
            snapshots,
        ).unwrap();

        let mut roots = RootCertStore::empty();
        roots.add(CertificateDer::from(identity.certificate_der.clone())).unwrap();
        let provider = rustls::crypto::ring::default_provider();
        let config = ClientConfig::builder_with_provider(Arc::new(provider))
            .with_safe_default_protocol_versions().unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connection = ClientConnection::new(
            Arc::new(config),
            ServerName::try_from("localhost").unwrap().to_owned(),
        ).unwrap();
        let stream = TcpStream::connect(server.addr).unwrap();
        let mut tls = StreamOwned::new(connection, stream);
        write!(
            tls,
            "GET /v1/monitor/snapshot?desktopId={desktop_id}&deviceId={device_id} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nConnection: close\r\n\r\n"
        ).unwrap();
        tls.flush().unwrap();
        let mut response = String::new();
        tls.read_to_string(&mut response).unwrap();
        assert!(response.starts_with("HTTP/1.1 200 OK"));
        assert!(response.contains("\"type\":\"snapshot\""));
        assert!(response.contains("\"sessionId\":\"s_"));
        assert!(response.contains("\"ciphertext\":\"ciphertext\""));
        drop(server);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn direct_https_snapshot_rejects_missing_auth() {
        let desktop_id = "d_0123456789abcdef0123456789abcdef";
        let auth: AuthChecker = Arc::new(|_, _| false);
        let revoke: RevokeHandler = Arc::new(|_| Ok(()));
        let snapshots: SnapshotProvider = Arc::new(|_, _, _| {
            Err("snapshot provider must not run".into())
        });
        let request = HttpRequestHead {
            method: "GET".into(),
            path: format!(
                "/v1/monitor/snapshot?desktopId={desktop_id}&deviceId=dev_0123456789abcdef"
            ),
            bearer_token: None,
            websocket_upgrade: false,
        };
        let mut output = Vec::new();
        handle_https_request(
            &mut output,
            desktop_id,
            &request,
            &auth,
            &revoke,
            &snapshots,
        ).unwrap();
        let response = String::from_utf8(output).unwrap();
        assert!(response.starts_with("HTTP/1.1 401 Unauthorized"));
        assert!(response.contains("\"error\":\"unauthorized\""));
    }

    #[test]
    fn direct_wss_requires_auth_and_emits_encrypted_snapshot() {
        let dir = temp_dir("monitor-wss");
        let identity = ensure_tls_identity(&dir).unwrap();
        let desktop_id = "d_0123456789abcdef0123456789abcdef".to_string();
        let device_id = "dev_0123456789abcdef".to_string();
        let token = generate_monitor_token().unwrap();
        let auth_token = token.clone();
        let auth_device = device_id.clone();
        let auth: AuthChecker = Arc::new(move |candidate_device, candidate_token| {
            candidate_device == auth_device && candidate_token == auth_token
        });
        let pairing: PairHandler = Arc::new(|_| Err("pairing disabled in test".into()));
        let revoked = Arc::new(AtomicBool::new(false));
        let revoked_for_handler = revoked.clone();
        let revoke: RevokeHandler = Arc::new(move |candidate_device| {
            if candidate_device != "dev_0123456789abcdef" {
                return Err("unexpected device".into());
            }
            revoked_for_handler.store(true, Ordering::SeqCst);
            Ok(())
        });
        let control: ControlHandler = Arc::new(|candidate_device, request| {
            if candidate_device != "dev_0123456789abcdef" {
                return Err("unexpected control device".into());
            }
            Ok(EncryptedControlPayload {
                request_id: request.request_id,
                issued_at: request.issued_at,
                expires_at: request.expires_at,
                nonce: "response-nonce".into(),
                ciphertext: "response-ciphertext".into(),
            })
        });
        let snapshots: SnapshotProvider = Arc::new(|_, session_id, sequence| {
            Ok(EncryptedSnapshot {
                session_id: session_id.to_string(),
                sequence,
                generated_at: 123,
                nonce: "nonce".into(),
                ciphertext: "ciphertext".into(),
            })
        });
        let server = start_monitor_server(
            "127.0.0.1:0".parse().unwrap(),
            identity.clone(),
            desktop_id.clone(),
            auth,
            pairing,
            revoke,
            control,
            snapshots,
        ).unwrap();

        let mut roots = RootCertStore::empty();
        roots.add(CertificateDer::from(identity.certificate_der.clone())).unwrap();
        let provider = rustls::crypto::ring::default_provider();
        let config = ClientConfig::builder_with_provider(Arc::new(provider))
            .with_safe_default_protocol_versions().unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        let connection = ClientConnection::new(
            Arc::new(config),
            ServerName::try_from("localhost").unwrap().to_owned(),
        ).unwrap();
        let stream = TcpStream::connect(server.addr).unwrap();
        let tls = StreamOwned::new(connection, stream);
        let request = tungstenite::http::Request::builder()
            .uri(format!(
                "wss://localhost:{}/v1/ws/monitor?desktopId={desktop_id}&deviceId={device_id}",
                server.addr.port(),
            ))
            .header("Host", format!("localhost:{}", server.addr.port()))
            .header("Authorization", format!("Bearer {token}"))
            .header("Connection", "Upgrade")
            .header("Upgrade", "websocket")
            .header("Sec-WebSocket-Version", "13")
            .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==")
            .body(())
            .unwrap();
        let (mut websocket, _) = tungstenite::client(request, tls).unwrap();
        websocket.send(Message::Text(
            serde_json::to_string(&DeviceWsMessage::Hello {
                protocol_version: PROTOCOL_VERSION,
                desktop_id,
                device_id: device_id.clone(),
            }).unwrap().into()
        )).unwrap();

        let ack = websocket.read().unwrap();
        let Message::Text(ack) = ack else { panic!("expected hello ack"); };
        assert!(matches!(
            serde_json::from_str::<DeviceServerMessage>(&ack).unwrap(),
            DeviceServerMessage::HelloAck { desktop_online: true, .. }
        ));
        let snapshot = websocket.read().unwrap();
        let Message::Text(snapshot) = snapshot else { panic!("expected snapshot"); };
        assert!(matches!(
            serde_json::from_str::<DeviceServerMessage>(&snapshot).unwrap(),
            DeviceServerMessage::Snapshot { .. }
        ));

        let control_request = EncryptedControlPayload {
            request_id: "c_0123456789abcdef".into(),
            issued_at: 1_000,
            expires_at: 31_000,
            nonce: "request-nonce".into(),
            ciphertext: "request-ciphertext".into(),
        };
        websocket.send(Message::Text(
            serde_json::to_string(&DeviceWsMessage::Control {
                request: control_request,
            }).unwrap().into(),
        )).unwrap();
        let control_message = websocket.read().unwrap();
        let Message::Text(control_message) = control_message else {
            panic!("expected control result");
        };
        assert!(matches!(
            serde_json::from_str::<DeviceServerMessage>(&control_message).unwrap(),
            DeviceServerMessage::ControlResult { response }
                if response.request_id == "c_0123456789abcdef"
                    && response.ciphertext == "response-ciphertext"
        ));

        websocket.send(Message::Text(
            serde_json::to_string(&DeviceWsMessage::RevokeSelf)
                .unwrap()
                .into(),
        )).unwrap();
        let revoked_message = websocket.read().unwrap();
        let Message::Text(revoked_message) = revoked_message else {
            panic!("expected revoked message");
        };
        assert!(matches!(
            serde_json::from_str::<DeviceServerMessage>(&revoked_message).unwrap(),
            DeviceServerMessage::Revoked { device_id: ref revoked_id }
                if revoked_id == &device_id
        ));
        assert!(revoked.load(Ordering::SeqCst));

        drop(websocket);
        drop(server);
        fs::remove_dir_all(dir).unwrap();
    }
}
