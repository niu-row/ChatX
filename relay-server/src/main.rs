mod desktop_session;
mod device_session;
mod pairing_session;
mod registry;
mod registry_routes;
mod web_utils;

use axum::{
    extract::{DefaultBodyLimit, State},
    response::IntoResponse,
    routing::{get, post, put},
    Json, Router,
};
use chatx_relay_protocol::{
    device_capabilities, DesktopServerMessage, DeviceServerMessage,
    EncryptedControlPayload, EncryptedSnapshot, PairingServerMessage,
    CAPABILITY_CONTROL_RECONNECT, CAPABILITY_CONTROL_REFRESH,
    CAPABILITY_REVOKE_SYNC, CAPABILITY_SNAPSHOT, PROTOCOL_VERSION,
};
#[cfg(test)]
use chatx_relay_protocol::DesktopRegisterRequest;
use desktop_session::desktop_ws;
use device_session::{
    desktop_snapshot_http, device_revoke_http, device_snapshot_http, device_ws,
};
use pairing_session::{delete_pairing_route, open_pairing_route, pairing_ws};
use registry::{hash_matches, token_hash, Registry};
#[cfg(test)]
use registry::{DesktopRecord, DeviceRecord};
use registry_routes::{authorize_device, register_desktop, revoke_device};
#[cfg(test)]
use registry_routes::desktop_registration_record;
use serde_json::json;
use web_utils::{now_ms, random_hex};
use std::{
    collections::HashMap,
    env,
    net::SocketAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
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
const REVOKE_TIMEOUT: Duration = Duration::from_secs(8);
const MAX_DESKTOP_WS: usize = 64;
const MAX_DEVICE_WS: usize = 512;
const MAX_PAIRING_WS: usize = 64;
const MAX_PAIRING_ROUTES: usize = 256;
const MAX_PENDING_CONTROLS: usize = 128;
const MAX_PENDING_REVOKES: usize = 128;
const CONTROL_MIN_INTERVAL_MS: u64 = 500;

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
type RevokeSender = oneshot::Sender<Result<(), String>>;

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

fn route_supports_revoke(route: &DesktopRoute) -> bool {
    route.capabilities.iter().any(|capability| {
        capability == CAPABILITY_REVOKE_SYNC
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
    revoke_connections: Arc<RwLock<HashMap<ControlRouteKey, RevokeSender>>>,
    control_last_at: Arc<Mutex<HashMap<RouteKey, u64>>>,
    desktop_ws_slots: Arc<Semaphore>,
    device_ws_slots: Arc<Semaphore>,
    pairing_ws_slots: Arc<Semaphore>,
}

impl AppState {
    fn load(
        registry_path: PathBuf,
        bootstrap_token: Option<String>,
    ) -> Result<Self, String> {
        let registry = registry::load(&registry_path)?;
        Ok(Self {
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
            revoke_connections: Arc::new(RwLock::new(HashMap::new())),
            control_last_at: Arc::new(Mutex::new(HashMap::new())),
            desktop_ws_slots: Arc::new(Semaphore::new(MAX_DESKTOP_WS)),
            device_ws_slots: Arc::new(Semaphore::new(MAX_DEVICE_WS)),
            pairing_ws_slots: Arc::new(Semaphore::new(MAX_PAIRING_WS)),
        })
    }

    fn save_registry(&self, registry: &Registry) -> Result<(), String> {
        registry::save(&self.registry_path, registry)
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

    async fn complete_revoke(
        &self,
        desktop_id: &str,
        device_id: &str,
        request_id: &str,
        result: Result<(), String>,
    ) {
        let key = (
            desktop_id.to_string(),
            device_id.to_string(),
            request_id.to_string(),
        );
        if let Some(sender) = self.revoke_connections.write().await.remove(&key) {
            let _ = sender.send(result);
        }
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

async fn request_desktop_revoke(
    state: &AppState,
    desktop_id: &str,
    device_id: &str,
) -> Result<(), String> {
    let desktop_route = state.desktop_channels
        .read().await.get(desktop_id).cloned()
        .ok_or_else(|| "desktop offline; synchronized revoke unavailable".to_string())?;
    if !route_supports_revoke(&desktop_route) {
        return Err("desktop synchronized revoke capability unavailable".into());
    }

    let request_id = format!("r_{}", random_hex(16)?);
    let key = (
        desktop_id.to_string(),
        device_id.to_string(),
        request_id.clone(),
    );
    let (sender, receiver) = oneshot::channel();
    {
        let mut pending = state.revoke_connections.write().await;
        if pending.len() >= MAX_PENDING_REVOKES {
            return Err("revoke capacity reached".into());
        }
        pending.insert(key.clone(), sender);
    }

    if desktop_route.sender.send(DesktopServerMessage::DeviceRevoke {
        device_id: device_id.to_string(),
        request_id,
    }).is_err() {
        state.revoke_connections.write().await.remove(&key);
        return Err("desktop revoke channel unavailable".into());
    }

    let result = tokio::time::timeout(REVOKE_TIMEOUT, receiver).await;
    state.revoke_connections.write().await.remove(&key);
    match result {
        Ok(Ok(result)) => result,
        _ => Err("desktop revoke timeout".into()),
    }
}

async fn finalize_device_revoke(
    state: &AppState,
    desktop_id: &str,
    device_id: &str,
) -> Result<(), String> {
    let key = (desktop_id.to_string(), device_id.to_string());
    state.snapshots.write().await.remove(&key);
    state.revoke_device_record(desktop_id, device_id)?;

    if let Some(sender) = state.device_channels.read().await.get(&key).cloned() {
        let _ = sender.send(DeviceServerMessage::Revoked {
            device_id: device_id.to_string(),
        });
    }
    state.device_channels.write().await.remove(&key);
    Ok(())
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    Json(json!({
        "ok": true,
        "protocolVersion": PROTOCOL_VERSION,
        "onlineDesktops": state.online_desktops.read().await.len(),
        "serverTime": now_ms()
    }))
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
    let state = AppState::load(registry_path(), bootstrap)
        .expect("load ChatX relay registry");

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
    use std::fs;

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
        assert!(!route_supports_revoke(&legacy));

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
        assert!(route_supports_revoke(&current));
    }

    #[test]
    fn corrupt_registry_is_rejected_instead_of_reset() {
        let path = std::env::temp_dir().join(format!(
            "chatx-relay-corrupt-test-{}.json",
            std::process::id(),
        ));
        fs::write(&path, "{ definitely-not-json").unwrap();
        let result = AppState::load(path.clone(), None);
        let _ = fs::remove_file(path);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn synchronized_revoke_waits_for_desktop_ack_before_registry_delete() {
        let path = std::env::temp_dir().join(format!(
            "chatx-relay-revoke-ack-test-{}.json",
            std::process::id(),
        ));
        let _ = fs::remove_file(&path);
        let state = AppState::load(path.clone(), None).unwrap();
        {
            let mut registry = state.registry.lock().unwrap();
            let mut devices = HashMap::new();
            devices.insert(
                "dev_a".into(),
                DeviceRecord {
                    token_hash: token_hash("device-secret"),
                    created_at: 1,
                },
            );
            registry.desktops.insert(
                "d_a".into(),
                DesktopRecord {
                    token_hash: token_hash("desktop-secret"),
                    device_name: "Desktop".into(),
                    app_version: "0.4.10".into(),
                    platform: "windows-x64".into(),
                    created_at: 1,
                    devices,
                },
            );
            state.save_registry(&registry).unwrap();
        }

        let (desktop_sender, mut desktop_receiver) =
            broadcast::channel(DESKTOP_CHANNEL_CAPACITY);
        state.desktop_channels.write().await.insert(
            "d_a".into(),
            DesktopRoute {
                session_id: "s_current".into(),
                sender: desktop_sender,
                capabilities: device_capabilities(),
            },
        );

        let responder = state.clone();
        let response_task = tokio::spawn(async move {
            let message = desktop_receiver.recv().await.unwrap();
            let DesktopServerMessage::DeviceRevoke {
                device_id,
                request_id,
            } = message else {
                panic!("expected synchronized revoke request");
            };
            assert_eq!(device_id, "dev_a");
            responder.complete_revoke(
                "d_a",
                "dev_a",
                &request_id,
                Ok(()),
            ).await;
        });

        assert!(state.device_exists("d_a", "dev_a"));
        request_desktop_revoke(&state, "d_a", "dev_a")
            .await
            .unwrap();
        response_task.await.unwrap();
        assert!(state.device_exists("d_a", "dev_a"));

        finalize_device_revoke(&state, "d_a", "dev_a")
            .await
            .unwrap();
        assert!(!state.device_exists("d_a", "dev_a"));
        let _ = fs::remove_file(path);
    }

    #[tokio::test]
    async fn control_completion_is_scoped_by_desktop_device_and_request() {
        let state = AppState::load(
            std::env::temp_dir().join(format!(
                "chatx-relay-control-test-{}.json",
                std::process::id(),
            )),
            None,
        ).unwrap();
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

    #[tokio::test]
    async fn revoke_completion_is_correlated_and_idempotent() {
        let state = AppState::load(
            std::env::temp_dir().join(format!(
                "chatx-relay-revoke-test-{}.json",
                std::process::id(),
            )),
            None,
        ).unwrap();
        let request_id = "r_0123456789abcdef".to_string();
        let key = ("d_a".into(), "dev_a".into(), request_id.clone());
        let (sender, receiver) = oneshot::channel();
        state.revoke_connections.write().await.insert(key, sender);

        state.complete_revoke(
            "d_a",
            "dev_a",
            &request_id,
            Ok(()),
        ).await;
        assert!(receiver.await.unwrap().is_ok());

        state.complete_revoke(
            "d_a",
            "dev_a",
            &request_id,
            Ok(()),
        ).await;
        assert!(state.revoke_connections.read().await.is_empty());
    }
}
