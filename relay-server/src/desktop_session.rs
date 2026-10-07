use super::{
    AppState, DesktopRoute, DESKTOP_CHANNEL_CAPACITY, WS_HEARTBEAT,
    WS_IDLE_TIMEOUT,
};
use crate::web_utils::{bearer, json_error, now_ms, random_hex, send_json};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Query, State,
    },
    http::{HeaderMap, StatusCode},
    response::Response,
};
use chatx_relay_protocol::{
    device_capabilities, DesktopServerMessage, DesktopWsMessage,
    PairingServerMessage, PROTOCOL_VERSION,
};
use serde::Deserialize;
use std::time::{Duration, Instant};
use tokio::sync::broadcast;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DesktopQuery {
    desktop_id: String,
}

pub(super) async fn desktop_ws(
    ws: WebSocketUpgrade,
    State(state): State<AppState>,
    Query(query): Query<DesktopQuery>,
    headers: HeaderMap,
) -> Response {
    let Some(token) = bearer(&headers) else {
        return json_error(
            StatusCode::UNAUTHORIZED,
            "missing bearer token",
        );
    };
    if !state.desktop_authorized(&query.desktop_id, token) {
        return json_error(
            StatusCode::UNAUTHORIZED,
            "invalid desktop credential",
        );
    }
    let permit = match state
        .desktop_ws_slots
        .clone()
        .try_acquire_owned()
    {
        Ok(value) => value,
        Err(_) => {
            return json_error(
                StatusCode::SERVICE_UNAVAILABLE,
                "desktop websocket capacity reached",
            );
        }
    };
    ws.on_upgrade(move |socket| async move {
        let _permit = permit;
        desktop_socket(state, query.desktop_id, socket).await;
    })
}

async fn desktop_socket(
    state: AppState,
    desktop_id: String,
    mut socket: WebSocket,
) {
    let hello = tokio::time::timeout(
        Duration::from_secs(5),
        socket.recv(),
    )
    .await;
    let Ok(Some(Ok(Message::Text(text)))) = hello else {
        return;
    };
    let Ok(DesktopWsMessage::Hello {
        protocol_version,
        desktop_id: hello_id,
        capabilities,
        ..
    }) = serde_json::from_str::<DesktopWsMessage>(&text)
    else {
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
    state
        .online_desktops
        .write()
        .await
        .insert(desktop_id.clone(), session_id.clone());
    state.desktop_channels.write().await.insert(
        desktop_id.clone(),
        DesktopRoute {
            session_id: session_id.clone(),
            sender: desktop_sender,
            capabilities,
        },
    );
    state
        .broadcast_desktop_presence(&desktop_id, true)
        .await;

    let ack = DesktopServerMessage::HelloAck {
        session_id: session_id.clone(),
        server_time: now_ms(),
        capabilities: device_capabilities(),
    };
    if send_json(&mut socket, &ack).await.is_err() {
        remove_online_if_current(
            &state,
            &desktop_id,
            &session_id,
        )
        .await;
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
                if socket
                    .send(Message::Ping(Vec::new().into()))
                    .await
                    .is_err()
                {
                    break;
                }
            }
            routed = desktop_receiver.recv() => {
                match routed {
                    Ok(message) => {
                        if send_json(&mut socket, &message)
                            .await
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => {}
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
            incoming = socket.recv() => {
                let Some(Ok(message)) = incoming else {
                    break;
                };
                last_activity = Instant::now();
                match message {
                    Message::Text(text) => {
                        let Ok(message) =
                            serde_json::from_str::<DesktopWsMessage>(&text)
                        else {
                            let _ = send_json(
                                &mut socket,
                                &DesktopServerMessage::Error {
                                    message: "invalid websocket message".into(),
                                },
                            )
                            .await;
                            continue;
                        };
                        match message {
                            DesktopWsMessage::Hello {
                                protocol_version,
                                desktop_id: hello_id,
                                ..
                            } => {
                                if protocol_version != PROTOCOL_VERSION
                                    || hello_id != desktop_id
                                {
                                    break;
                                }
                            }
                            DesktopWsMessage::Ping { .. } => {
                                let _ = send_json(
                                    &mut socket,
                                    &DesktopServerMessage::Pong {
                                        server_time: now_ms(),
                                    },
                                )
                                .await;
                            }
                            DesktopWsMessage::Snapshot {
                                device_id,
                                snapshot,
                            } => {
                                if let Err(error) = state
                                    .cache_and_route_snapshot(
                                        &desktop_id,
                                        &device_id,
                                        snapshot,
                                    )
                                    .await
                                {
                                    let _ = send_json(
                                        &mut socket,
                                        &DesktopServerMessage::Error {
                                            message: error,
                                        },
                                    )
                                    .await;
                                }
                            }
                            DesktopWsMessage::PairingResponse {
                                pairing_id,
                                connection_id,
                                payload,
                            } => {
                                if let Some(sender) = state
                                    .pairing_connections
                                    .write()
                                    .await
                                    .remove(&connection_id)
                                {
                                    let _ = sender.send(
                                        PairingServerMessage::PairResult {
                                            pairing_id,
                                            payload,
                                        },
                                    );
                                }
                            }
                            DesktopWsMessage::PairingError {
                                connection_id,
                                message,
                                ..
                            } => {
                                if let Some(sender) = state
                                    .pairing_connections
                                    .write()
                                    .await
                                    .remove(&connection_id)
                                {
                                    let _ = sender.send(
                                        PairingServerMessage::Error {
                                            message: message
                                                .chars()
                                                .take(240)
                                                .collect(),
                                        },
                                    );
                                }
                            }
                            DesktopWsMessage::ControlResponse {
                                device_id,
                                response,
                            } => {
                                let request_id =
                                    response.request_id.clone();
                                state
                                    .complete_control(
                                        &desktop_id,
                                        &device_id,
                                        &request_id,
                                        Ok(response),
                                    )
                                    .await;
                            }
                            DesktopWsMessage::ControlError {
                                device_id,
                                request_id,
                                message,
                            } => {
                                state
                                    .complete_control(
                                        &desktop_id,
                                        &device_id,
                                        &request_id,
                                        Err(
                                            message
                                                .chars()
                                                .take(240)
                                                .collect(),
                                        ),
                                    )
                                    .await;
                            }
                            DesktopWsMessage::RevokeResult {
                                device_id,
                                request_id,
                            } => {
                                state
                                    .complete_revoke(
                                        &desktop_id,
                                        &device_id,
                                        &request_id,
                                        Ok(()),
                                    )
                                    .await;
                            }
                            DesktopWsMessage::RevokeError {
                                device_id,
                                request_id,
                                message,
                            } => {
                                state
                                    .complete_revoke(
                                        &desktop_id,
                                        &device_id,
                                        &request_id,
                                        Err(
                                            message
                                                .chars()
                                                .take(240)
                                                .collect(),
                                        ),
                                    )
                                    .await;
                            }
                        }
                    }
                    Message::Ping(payload) => {
                        if socket
                            .send(Message::Pong(payload))
                            .await
                            .is_err()
                        {
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

    remove_online_if_current(
        &state,
        &desktop_id,
        &session_id,
    )
    .await;
}
async fn remove_online_if_current(
    state: &AppState,
    desktop_id: &str,
    session_id: &str,
) {
    let mut online = state.online_desktops.write().await;
    if online.get(desktop_id).map(String::as_str)
        == Some(session_id)
    {
        online.remove(desktop_id);
        drop(online);
        {
            let mut channels = state.desktop_channels.write().await;
            if channels
                .get(desktop_id)
                .is_some_and(|route| route.session_id == session_id)
            {
                channels.remove(desktop_id);
            }
        }
        state
            .broadcast_desktop_presence(desktop_id, false)
            .await;
    }
}
