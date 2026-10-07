use super::{
    finalize_device_revoke, request_desktop_revoke, route_supports_control,
    routed_device_capabilities, AppState, CONTROL_MIN_INTERVAL_MS,
    CONTROL_TIMEOUT, MAX_PENDING_CONTROLS, WS_HEARTBEAT,
    WS_IDLE_TIMEOUT,
};
use crate::web_utils::{bearer, json_error, now_ms, random_hex, send_json};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path as AxumPath, Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chatx_relay_protocol::{
    DesktopServerMessage, DeviceServerMessage, DeviceWsMessage,
    EncryptedSnapshot, PROTOCOL_VERSION,
};
use serde::Deserialize;
use serde_json::json;
use std::time::{Duration, Instant};
use tokio::sync::{broadcast, oneshot};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DeviceQuery {
    desktop_id: String,
    device_id: String,
}

pub(super) async fn device_ws(
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

pub(super) async fn desktop_snapshot_http(
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

pub(super) async fn device_snapshot_http(
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
            "desktopOnline": desktop_online,
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

pub(super) async fn device_revoke_http(
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

    if let Err(error) = request_desktop_revoke(&state, &desktop_id, &device_id).await {
        return json_error(StatusCode::CONFLICT, error);
    }
    if let Err(error) = finalize_device_revoke(&state, &desktop_id, &device_id).await {
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, error);
    }

    Json(json!({ "type": "revoked", "deviceId": device_id })).into_response()
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
        if !state.device_exists(&desktop_id, &device_id) {
            let _ = send_json(
                &mut socket,
                &DeviceServerMessage::Revoked {
                    device_id: device_id.clone(),
                },
            ).await;
            break;
        }
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
                        let revoked =
                            matches!(&message, DeviceServerMessage::Revoked { .. });
                        if send_json(&mut socket, &message).await.is_err() {
                            break;
                        }
                        if revoked {
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
                                if let Err(error) =
                                    request_desktop_revoke(&state, &desktop_id, &device_id).await
                                {
                                    let _ = send_json(
                                        &mut socket,
                                        &DeviceServerMessage::Error { message: error },
                                    ).await;
                                    break;
                                }
                                match finalize_device_revoke(
                                    &state,
                                    &desktop_id,
                                    &device_id,
                                ).await {
                                    Ok(()) => {
                                        let _ = send_json(
                                            &mut socket,
                                            &DeviceServerMessage::Revoked {
                                                device_id: device_id.clone(),
                                            },
                                        ).await;
                                        break;
                                    }
                                    Err(error) => {
                                        let _ = send_json(
                                            &mut socket,
                                            &DeviceServerMessage::Error { message: error },
                                        ).await;
                                        break;
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
