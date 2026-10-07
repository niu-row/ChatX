use super::{
    ensure_monitor_desktop_id, ensure_monitor_master_key, load_settings, monitor_control,
    monitor_crypto, monitor_devices_path, monitor_network, monitor_server, monitor_status,
    push_log, read_relay_credentials, relay, relay_credentials_for_settings, state_dir,
    timestamp_ms, AppState,
};
use chatx_relay_protocol::{PairingServerMessage, PairingWsMessage};
use qrcode::{render::svg, QrCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};
use tauri::Manager;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MonitorDeviceRecord {
    id: String,
    name: String,
    token_hash: String,
    created_at: u64,
    last_seen_at: Option<u64>,
}

#[derive(Debug, Clone)]
struct MonitorPairingGrant {
    pairing_id: String,
    pairing_code: String,
    code_hash: String,
    expires_at: u64,
}

#[derive(Default)]
pub(super) struct State {
    servers: Mutex<Vec<monitor_server::MonitorServerHandle>>,
    pairing: Mutex<Option<MonitorPairingGrant>>,
    devices: Mutex<Option<Vec<MonitorDeviceRecord>>>,
}

fn claim_pairing_grant(
    pairing: &Mutex<Option<MonitorPairingGrant>>,
    code: &str,
    now: u64,
) -> Result<MonitorPairingGrant, String> {
    let mut slot = pairing
        .lock()
        .map_err(|_| "Monitor 配对状态锁已损坏。".to_string())?;
    let Some(grant) = slot.as_ref() else {
        return Err("当前没有有效的配对请求。".into());
    };
    if now > grant.expires_at {
        *slot = None;
        return Err("配对二维码已过期，请在 ChatX 中重新生成。".into());
    }
    if !monitor_server::verify_token_hash_hex(code, &grant.code_hash) {
        return Err("配对码无效。".into());
    }
    slot.take()
        .ok_or_else(|| "当前没有有效的配对请求。".to_string())
}

fn restore_pairing_grant(
    pairing: &Mutex<Option<MonitorPairingGrant>>,
    grant: MonitorPairingGrant,
    now: u64,
) {
    if now > grant.expires_at {
        return;
    }
    if let Ok(mut slot) = pairing.lock() {
        if slot.is_none() {
            *slot = Some(grant);
        }
    }
}

fn load_monitor_devices_from_path(path: &Path) -> Result<Vec<MonitorDeviceRecord>, String> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(Vec::new());
        }
        Err(error) => {
            return Err(format!(
                "读取 Monitor 设备注册表失败（{}）：{error}",
                path.display()
            ));
        }
    };
    serde_json::from_str::<Vec<MonitorDeviceRecord>>(&text).map_err(|error| {
        format!(
            "Monitor 设备注册表已损坏（{}）：{error}",
            path.display()
        )
    })
}

fn load_monitor_devices_from_disk(
    app: &tauri::AppHandle,
) -> Result<Vec<MonitorDeviceRecord>, String> {
    let path = monitor_devices_path(app)?;
    load_monitor_devices_from_path(&path)
}

fn ensure_monitor_devices_loaded(
    app: &tauri::AppHandle,
    registry: &mut Option<Vec<MonitorDeviceRecord>>,
) -> Result<(), String> {
    if registry.is_none() {
        *registry = Some(load_monitor_devices_from_disk(app)?);
    }
    Ok(())
}

fn save_monitor_devices(
    app: &tauri::AppHandle,
    devices: &[MonitorDeviceRecord],
) -> Result<(), String> {
    let path = monitor_devices_path(app)?;
    let temp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let text = serde_json::to_string_pretty(devices)
        .map_err(|e| format!("序列化 Monitor 设备失败：{e}"))?;
    fs::write(&temp, format!("{text}\n"))
        .map_err(|e| format!("保存 Monitor 设备临时文件失败：{e}"))?;
    #[cfg(windows)]
    if path.exists() {
        fs::remove_file(&path).map_err(|e| format!("替换 Monitor 设备文件失败：{e}"))?;
    }
    fs::rename(&temp, &path).map_err(|e| format!("更新 Monitor 设备失败：{e}"))
}

fn monitor_devices_snapshot(
    app: &tauri::AppHandle,
    state: &AppState,
) -> Result<Vec<MonitorDeviceRecord>, String> {
    let mut registry = state.monitor.devices.lock()
        .map_err(|_| "Monitor 设备状态锁已损坏。".to_string())?;
    ensure_monitor_devices_loaded(app, &mut registry)?;
    Ok(registry.as_ref().cloned().unwrap_or_default())
}

pub(super) fn monitor_device_ids(
    app: &tauri::AppHandle,
    state: &AppState,
) -> Result<Vec<String>, String> {
    Ok(monitor_devices_snapshot(app, state)?
        .into_iter()
        .map(|device| device.id)
        .collect())
}

pub(super) fn has_monitor_device(
    app: &tauri::AppHandle,
    state: &AppState,
    device_id: &str,
) -> Result<bool, String> {
    Ok(monitor_devices_snapshot(app, state)?
        .iter()
        .any(|device| device.id == device_id))
}

pub(super) fn revoke_monitor_device_local(
    app: &tauri::AppHandle,
    state: &AppState,
    device_id: &str,
) -> Result<bool, String> {
    let mut registry = state.monitor.devices.lock()
        .map_err(|_| "Monitor 设备状态锁已损坏。".to_string())?;
    ensure_monitor_devices_loaded(app, &mut registry)?;
    let devices = registry.as_mut()
        .ok_or_else(|| "Monitor 设备状态不可用。".to_string())?;
    let previous = devices.clone();
    devices.retain(|device| device.id != device_id);
    if devices.len() == previous.len() {
        return Ok(false);
    }
    if let Err(error) = save_monitor_devices(app, devices) {
        *devices = previous;
        return Err(error);
    }
    drop(registry);
    push_log(state, format!("已撤销手机 Monitor 设备：{device_id}"));
    Ok(true)
}

pub(super) fn revoke_monitor_device_everywhere(
    app: &tauri::AppHandle,
    state: &AppState,
    device_id: &str,
) -> Result<bool, String> {
    let settings = load_settings(app);
    let relay_credentials = read_relay_credentials(app).ok();
    let relay_matches = if !settings.relay.base_url.trim().is_empty() {
        relay_credentials.as_ref().is_some_and(|credentials| {
            relay::normalized_base_url(&credentials.base_url).ok()
                == relay::normalized_base_url(&settings.relay.base_url).ok()
        })
    } else {
        false
    };

    if relay_matches {
        if let Some(credentials) = relay_credentials.as_ref() {
            relay::revoke_device(&settings.relay, credentials, device_id)
                .map_err(|error| {
                    format!(
                        "Relay 端设备撤销失败，本地凭据保持有效；请确认 Relay 可达后重试：{error}"
                    )
                })?;
        }
    }

    match revoke_monitor_device_local(app, state, device_id) {
        Ok(removed) => Ok(removed),
        Err(error) => {
            if relay_matches {
                if let (Some(credentials), Ok(master_key)) = (
                    relay_credentials.as_ref(),
                    ensure_monitor_master_key(app),
                ) {
                    if let Ok(relay_token) =
                        monitor_crypto::derive_relay_token(&master_key, device_id)
                    {
                        let _ = relay::authorize_device(
                            &settings.relay,
                            credentials,
                            device_id,
                            &relay_token,
                        );
                    }
                }
            }
            Err(error)
        }
    }
}

fn monitor_device_token_authorized(
    app: &tauri::AppHandle,
    state: &AppState,
    device_id: &str,
    token: &str,
) -> bool {
    let Ok(mut registry) = state.monitor.devices.lock() else { return false; };
    if ensure_monitor_devices_loaded(app, &mut registry).is_err() {
        return false;
    }
    let Some(devices) = registry.as_mut() else { return false; };
    let now = timestamp_ms();
    let Some(index) = devices.iter().position(|device| {
        device.id == device_id
            && monitor_server::verify_token_hash_hex(token, &device.token_hash)
    }) else {
        return false;
    };
    let should_update = devices[index].last_seen_at
        .map(|seen| now.saturating_sub(seen) >= 60_000)
        .unwrap_or(true);
    if should_update {
        let previous = devices[index].last_seen_at;
        devices[index].last_seen_at = Some(now);
        if save_monitor_devices(app, devices).is_err() {
            devices[index].last_seen_at = previous;
        }
    }
    true
}

fn complete_claimed_monitor_pairing(
    app: &tauri::AppHandle,
    state: &AppState,
    pairing_id: &str,
    name: &str,
) -> Result<Value, String> {
    let mut registry = state.monitor.devices.lock()
        .map_err(|_| "Monitor 设备状态锁已损坏。".to_string())?;
    ensure_monitor_devices_loaded(app, &mut registry)?;
    let devices = registry.as_mut()
        .ok_or_else(|| "Monitor 设备状态不可用。".to_string())?;
    if devices.len() >= 32 {
        return Err("已配对设备数量已达到上限 32。".into());
    }

    let master_key = ensure_monitor_master_key(app)?;
    let random_id = monitor_server::generate_monitor_token()?;
    let device_id = format!("dev_{}", &random_id[..24]);
    let direct_token = monitor_crypto::derive_direct_token(&master_key, &device_id)?;
    let relay_token = monitor_crypto::derive_relay_token(&master_key, &device_id)?;
    let device_key = monitor_crypto::device_key_b64(&master_key, &device_id)?;
    let token_hash = monitor_server::token_hash_hex(&direct_token);
    let settings = load_settings(app);
    let relay_credentials = if settings.relay.enabled {
        relay_credentials_for_settings(app, &settings)
    } else {
        None
    };
    let relay_enrollment =
        if let Some(credentials) = relay_credentials.as_ref() {
            relay::authorize_device(
                &settings.relay,
                credentials,
                &device_id,
                &relay_token,
            )?;
            Some(json!({
                "baseUrl": settings.relay.base_url,
                "desktopId": credentials.desktop_id,
                "deviceToken": relay_token
            }))
        } else {
            None
        };

    let now = timestamp_ms();
    let previous = devices.clone();
    devices.push(MonitorDeviceRecord {
        id: device_id.clone(),
        name: name.to_string(),
        token_hash,
        created_at: now,
        last_seen_at: Some(now),
    });
    if let Err(error) = save_monitor_devices(app, devices) {
        let relay_rollback = relay_credentials
            .as_ref()
            .map(|credentials| {
                relay::revoke_device(
                    &settings.relay,
                    credentials,
                    &device_id,
                )
            })
            .transpose();
        *devices = previous;
        return match relay_rollback {
            Ok(_) => Err(error),
            Err(rollback_error) => Err(format!(
                "{error}；同时回滚 Relay 设备授权失败：{rollback_error}"
            )),
        };
    }

    if let Some(credentials) = relay_credentials.as_ref() {
        let _ = relay::delete_pairing_route(
            &settings.relay,
            credentials,
            pairing_id,
        );
    }
    push_log(state, format!("手机 Monitor 已配对设备：{name} ({device_id})"));
    Ok(json!({
        "schemaVersion": 3,
        "deviceId": device_id,
        "deviceName": name,
        "directToken": direct_token,
        "deviceKey": device_key,
        "relay": relay_enrollment
    }))
}

fn complete_monitor_pairing(
    app: &tauri::AppHandle,
    state: &AppState,
    payload: Value,
) -> Result<Value, String> {
    let code = payload.get("pairingCode")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| "缺少 pairingCode。".to_string())?;
    let raw_name = payload.get("deviceName")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("Android Device");
    let name = raw_name.chars().take(80).collect::<String>();

    let grant = claim_pairing_grant(
        &state.monitor.pairing,
        code,
        timestamp_ms(),
    )?;
    let result = complete_claimed_monitor_pairing(
        app,
        state,
        &grant.pairing_id,
        &name,
    );
    if result.is_err() {
        restore_pairing_grant(
            &state.monitor.pairing,
            grant,
            timestamp_ms(),
        );
    }
    result
}

pub(super) fn handle_encrypted_monitor_pairing(
    app: &tauri::AppHandle,
    state: &AppState,
    message: PairingWsMessage,
) -> Result<PairingServerMessage, String> {
    let PairingWsMessage::Pair { pairing_id, payload } = message;
    let grant = {
        let mut pairing = state.monitor.pairing.lock()
            .map_err(|_| "Monitor 配对状态锁已损坏。".to_string())?;
        let Some(grant) = pairing.as_ref() else {
            return Err("当前没有有效的配对请求。".into());
        };
        if timestamp_ms() > grant.expires_at {
            *pairing = None;
            return Err("配对二维码已过期，请在 ChatX 中重新生成。".into());
        }
        if grant.pairing_id != pairing_id {
            return Err("Pairing route 无效。".into());
        }
        grant.clone()
    };

    let plaintext = monitor_crypto::decrypt_pairing_payload(
        &grant.pairing_code,
        &pairing_id,
        "request",
        &payload,
    )?;
    let request = serde_json::from_slice::<Value>(&plaintext)
        .map_err(|_| "Pairing E2EE payload 不是有效 JSON。".to_string())?;
    let result = complete_monitor_pairing(app, state, request)?;
    let clear = serde_json::to_vec(&result)
        .map_err(|e| format!("序列化 Pairing result 失败：{e}"))?;
    let payload = monitor_crypto::encrypt_pairing_payload(
        &grant.pairing_code,
        &pairing_id,
        "response",
        &clear,
    )?;
    Ok(PairingServerMessage::PairResult { pairing_id, payload })
}

fn monitor_tls_dir(app: &tauri::AppHandle) -> Result<PathBuf, String> {
    Ok(state_dir(app)?.join("monitor-tls"))
}

pub(super) fn monitor_info_payload(
    app: &tauri::AppHandle,
    state: &AppState,
) -> Result<Value, String> {
    let settings = load_settings(app);
    let servers = state.monitor.servers.lock().ok();
    let running = servers.as_ref().is_some_and(|servers| !servers.is_empty());
    let bind_addresses = servers.as_ref().map(|servers| {
        servers.iter().map(|server| server.addr.to_string()).collect::<Vec<_>>()
    }).unwrap_or_default();
    let fingerprint = servers.as_ref()
        .and_then(|servers| servers.first())
        .map(|server| server.fingerprint_sha256.clone());
    let endpoints = if running {
        let supports_ipv4 = servers
            .as_ref()
            .is_some_and(|servers| {
                servers.iter().any(|server| server.addr.is_ipv4())
            });
        let supports_ipv6 = servers
            .as_ref()
            .is_some_and(|servers| {
                servers.iter().any(|server| server.addr.is_ipv6())
            });
        monitor_network::discover_monitor_endpoints(settings.monitor_port)
            .into_iter()
            .filter(|endpoint| {
                (endpoint.family == "ipv4" && supports_ipv4)
                    || (endpoint.family == "ipv6" && supports_ipv6)
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let devices = monitor_devices_snapshot(app, state)?.into_iter().map(|device| json!({
        "id": device.id,
        "name": device.name,
        "createdAt": device.created_at,
        "lastSeenAt": device.last_seen_at
    })).collect::<Vec<_>>();
    Ok(json!({
        "enabled": settings.monitor_enabled,
        "port": settings.monitor_port,
        "running": running,
        "desktopId": ensure_monitor_desktop_id(app).ok(),
        "protocol": "chatx-monitor-wss-v1",
        "encryption": "AES-256-GCM",
        "bindAddress": if bind_addresses.is_empty() { None } else { Some(bind_addresses.join(" · ")) },
        "bindAddresses": bind_addresses,
        "fingerprintSha256": fingerprint,
        "devices": devices,
        "endpoints": endpoints
    }))
}

pub(super) fn start_monitor_service(app: &tauri::AppHandle, state: &AppState) -> Result<(), String> {
    let settings = load_settings(app);
    if !settings.monitor_enabled {
        return Ok(());
    }
    if settings.monitor_port < 1024 {
        return Err("Monitor 端口必须为 1024-65535。".into());
    }
    let _ = monitor_devices_snapshot(app, state)?;
    let mut slot = state.monitor.servers.lock()
        .map_err(|_| "Monitor Server 状态锁已损坏。".to_string())?;
    if !slot.is_empty() {
        return Ok(());
    }

    let identity = monitor_server::ensure_tls_identity(&monitor_tls_dir(app)?)?;
    let desktop_id = ensure_monitor_desktop_id(app)?;
    let master_key = ensure_monitor_master_key(app)?;

    let app_for_auth = app.clone();
    let auth_checker: monitor_server::AuthChecker = Arc::new(move |device_id, candidate| {
        let state = app_for_auth.state::<AppState>();
        monitor_device_token_authorized(
            &app_for_auth,
            state.inner(),
            device_id,
            candidate,
        )
    });

    let app_for_pair = app.clone();
    let pair_handler: monitor_server::PairHandler = Arc::new(move |message| {
        let state = app_for_pair.state::<AppState>();
        handle_encrypted_monitor_pairing(&app_for_pair, state.inner(), message)
    });

    let app_for_revoke = app.clone();
    let revoke_handler: monitor_server::RevokeHandler = Arc::new(move |device_id| {
        let state = app_for_revoke.state::<AppState>();
        revoke_monitor_device_everywhere(
            &app_for_revoke,
            state.inner(),
            device_id,
        ).and_then(|removed| {
            if removed { Ok(()) } else { Err("没有找到该已配对设备。".into()) }
        })
    });

    let app_for_control = app.clone();
    let control_handler: monitor_server::ControlHandler = Arc::new(
        move |device_id, request| {
            let state = app_for_control.state::<AppState>();
            monitor_control::handle(
                &app_for_control,
                state.inner(),
                device_id,
                request,
            )
        },
    );

    let app_for_snapshot = app.clone();
    let snapshot_desktop_id = desktop_id.clone();
    let snapshot_provider: monitor_server::SnapshotProvider = Arc::new(
        move |device_id, session_id, sequence| {
            let state = app_for_snapshot.state::<AppState>();
            if !has_monitor_device(&app_for_snapshot, state.inner(), device_id)? {
                return Err("Monitor 设备已被撤销。".into());
            }
            let plaintext = serde_json::to_vec(&monitor_status::payload(
                &app_for_snapshot,
                state.inner(),
            )).map_err(|e| format!("序列化 Monitor Snapshot 失败：{e}"))?;
            monitor_crypto::encrypt_snapshot(
                &master_key,
                &snapshot_desktop_id,
                device_id,
                session_id,
                sequence,
                timestamp_ms(),
                &plaintext,
            )
        },
    );

    let binds = [
        format!("0.0.0.0:{}", settings.monitor_port),
        format!("[::]:{}", settings.monitor_port),
    ];
    let mut errors = Vec::new();
    for bind_text in binds {
        let bind = match bind_text.parse() {
            Ok(bind) => bind,
            Err(error) => {
                errors.push(format!("{bind_text}: {error}"));
                continue;
            }
        };
        let handlers = monitor_server::MonitorHandlers::new(
            auth_checker.clone(),
            pair_handler.clone(),
            revoke_handler.clone(),
            control_handler.clone(),
            snapshot_provider.clone(),
        );
        match monitor_server::start_monitor_server(
            bind,
            identity.clone(),
            desktop_id.clone(),
            handlers,
        ) {
            Ok(server) => {
                push_log(
                    state,
                    format!("手机 Monitor Server 已启动：WSS {}", server.addr),
                );
                slot.push(server);
            }
            Err(error) => errors.push(error),
        }
    }
    if slot.is_empty() {
        return Err(format!(
            "Monitor Server 无法监听 IPv4/IPv6：{}",
            errors.join("；"),
        ));
    }
    for error in errors {
        push_log(state, format!("Monitor Server 部分监听失败：{error}"));
    }
    Ok(())
}

pub(super) fn create_pairing(app: &tauri::AppHandle, state: &AppState) -> Result<Value, String> {
    let settings = load_settings(app);
    if !settings.monitor_enabled {
        return Err("请先开启手机连接。".into());
    }
    start_monitor_service(app, state)?;

    let (supports_ipv4, supports_ipv6, fingerprint_sha256) = {
        let servers = state.monitor.servers.lock()
            .map_err(|_| "Monitor Server 状态锁已损坏。".to_string())?;
        if servers.is_empty() {
            return Err("Monitor WSS Server 未运行。".into());
        }
        (
            servers.iter().any(|server| server.addr.is_ipv4()),
            servers.iter().any(|server| server.addr.is_ipv6()),
            servers[0].fingerprint_sha256.clone(),
        )
    };

    let endpoints =
        monitor_network::discover_monitor_endpoints(settings.monitor_port)
            .into_iter()
            .filter(|endpoint| {
                (endpoint.family == "ipv4" && supports_ipv4)
                    || (endpoint.family == "ipv6" && supports_ipv6)
            })
            .take(8)
            .collect::<Vec<_>>();
    let direct_candidates = endpoints.into_iter().map(|endpoint| {
        let pair_url = endpoint.url.replace("/v1/ws/monitor", "/v1/ws/pair");
        json!({
            "kind": endpoint.kind,
            "family": endpoint.family,
            "interface": endpoint.interface,
            "host": endpoint.host,
            "url": pair_url
        })
    }).collect::<Vec<_>>();

    let desktop_id = ensure_monitor_desktop_id(app)?;
    let pairing_code = monitor_server::generate_monitor_token()?;
    let pairing_random = monitor_server::generate_monitor_token()?;
    let pairing_id = format!("p_{}", &pairing_random[..24]);
    let expires_at = timestamp_ms().saturating_add(5 * 60_000);

    let mut relay_pairing = None;
    let mut relay_error = None;
    if settings.relay.enabled {
        match read_relay_credentials(app) {
            Ok(credentials)
                if credentials.desktop_id == desktop_id
                    && relay::normalized_base_url(&credentials.base_url).ok()
                        == relay::normalized_base_url(&settings.relay.base_url).ok() =>
            {
                let route_token = monitor_server::generate_monitor_token()?;
                match relay::open_pairing_route(
                    &settings.relay,
                    &credentials,
                    &pairing_id,
                    &route_token,
                    expires_at,
                ) {
                    Ok(()) => {
                        relay_pairing = Some(json!({
                            "baseUrl": settings.relay.base_url,
                            "url": relay::pairing_ws_url(
                                &settings.relay,
                                &credentials,
                                &pairing_id,
                            )?,
                            "desktopId": desktop_id,
                            "pairingId": pairing_id,
                            "routeToken": route_token
                        }));
                    }
                    Err(error) => relay_error = Some(error),
                }
            }
            Ok(_) => relay_error = Some(
                "Relay Credential 与当前 Desktop 身份或 Relay URL 不匹配。".into()
            ),
            Err(error) => relay_error = Some(error),
        }
    }

    if direct_candidates.is_empty() && relay_pairing.is_none() {
        return Err(relay_error.unwrap_or_else(|| {
            "没有发现可用的 Direct WSS 路径，ChatX Relay 也不可用。".into()
        }));
    }

    {
        let mut grant = state.monitor.pairing.lock()
            .map_err(|_| "Monitor 配对状态锁已损坏。".to_string())?;
        *grant = Some(MonitorPairingGrant {
            pairing_id: pairing_id.clone(),
            pairing_code: pairing_code.clone(),
            code_hash: monitor_server::token_hash_hex(&pairing_code),
            expires_at,
        });
    }

    let pairing = json!({
        "schemaVersion": 3,
        "protocol": "chatx-monitor-wss-v1",
        "scheme": "wss",
        "desktopId": desktop_id,
        "port": settings.monitor_port,
        "directCandidates": direct_candidates,
        "relayPairing": relay_pairing,
        "relayPairingError": relay_error,
        "fingerprintSha256": fingerprint_sha256,
        "pairingId": pairing_id,
        "pairingCode": pairing_code,
        "expiresAt": expires_at
    });
    let pairing_text = serde_json::to_string(&pairing)
        .map_err(|e| format!("生成配对 JSON 失败：{e}"))?;
    let qr = QrCode::new(pairing_text.as_bytes())
        .map_err(|e| format!("生成配对二维码失败：{e}"))?;
    let qr_svg = qr.render::<svg::Color>()
        .min_dimensions(280, 280)
        .dark_color(svg::Color("#111827"))
        .light_color(svg::Color("#ffffff"))
        .build();
    let mut result = pairing;
    result.as_object_mut()
        .ok_or_else(|| "生成配对资料失败。".to_string())?
        .insert("qrSvg".into(), Value::String(qr_svg));
    Ok(result)
}

pub(super) fn stop_monitor_service(state: &AppState) -> Result<(), String> {
    let servers = {
        let mut slot = state.monitor.servers.lock()
            .map_err(|_| "Monitor Server 状态锁已损坏。".to_string())?;
        std::mem::take(&mut *slot)
    };
    drop(servers);
    push_log(state, "手机 Monitor Server 已停止");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_registry(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "chatx-monitor-{name}-{}-{}.json",
            std::process::id(),
            timestamp_ms(),
        ))
    }

    #[test]
    fn missing_monitor_registry_is_an_empty_registry() {
        let path = temp_registry("missing");
        let _ = fs::remove_file(&path);
        assert!(load_monitor_devices_from_path(&path).unwrap().is_empty());
    }

    #[test]
    fn corrupt_monitor_registry_is_rejected_instead_of_reset() {
        let path = temp_registry("corrupt");
        fs::write(&path, "{ definitely-not-json").unwrap();
        let result = load_monitor_devices_from_path(&path);
        let _ = fs::remove_file(&path);
        assert!(result.is_err());
    }

    #[test]
    fn valid_monitor_registry_is_loaded() {
        let path = temp_registry("valid");
        let devices = vec![MonitorDeviceRecord {
            id: "dev_test".into(),
            name: "Test Phone".into(),
            token_hash: "ab".repeat(32),
            created_at: 1,
            last_seen_at: Some(2),
        }];
        fs::write(&path, serde_json::to_string(&devices).unwrap()).unwrap();
        let loaded = load_monitor_devices_from_path(&path).unwrap();
        let _ = fs::remove_file(&path);
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "dev_test");
    }

    fn pairing_grant(id: &str, code: &str, expires_at: u64) -> MonitorPairingGrant {
        MonitorPairingGrant {
            pairing_id: id.into(),
            pairing_code: code.into(),
            code_hash: monitor_server::token_hash_hex(code),
            expires_at,
        }
    }

    #[test]
    fn pairing_claim_is_single_consumer_and_invalid_code_does_not_consume() {
        let slot = Mutex::new(Some(pairing_grant("p_one", "secret", 2_000)));

        assert!(claim_pairing_grant(&slot, "wrong", 1_000).is_err());
        assert!(slot.lock().unwrap().is_some());

        let claimed = claim_pairing_grant(&slot, "secret", 1_000).unwrap();
        assert_eq!(claimed.pairing_id, "p_one");
        assert!(slot.lock().unwrap().is_none());
        assert!(claim_pairing_grant(&slot, "secret", 1_000).is_err());
    }

    #[test]
    fn failed_pairing_can_restore_unexpired_grant_without_overwriting_newer_one() {
        let slot = Mutex::new(None);
        restore_pairing_grant(
            &slot,
            pairing_grant("p_old", "old", 2_000),
            1_000,
        );
        assert_eq!(
            slot.lock().unwrap().as_ref().unwrap().pairing_id,
            "p_old"
        );

        *slot.lock().unwrap() = Some(pairing_grant("p_new", "new", 3_000));
        restore_pairing_grant(
            &slot,
            pairing_grant("p_old", "old", 2_000),
            1_000,
        );
        assert_eq!(
            slot.lock().unwrap().as_ref().unwrap().pairing_id,
            "p_new"
        );
    }

    #[test]
    fn expired_pairing_grant_is_not_restored() {
        let slot = Mutex::new(None);
        restore_pairing_grant(
            &slot,
            pairing_grant("p_expired", "expired", 999),
            1_000,
        );
        assert!(slot.lock().unwrap().is_none());
    }
}
