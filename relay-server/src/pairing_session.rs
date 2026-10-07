use super::{
    AppState, PairingRoute, RouteKey, MAX_PAIRING_ROUTES, PAIRING_TIMEOUT,
};
use crate::registry::hash_matches;
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
    DesktopServerMessage, PairingRouteOpenRequest, PairingServerMessage,
    PairingWsMessage,
};
use serde::Deserialize;
use std::{collections::HashMap, time::Duration};
use tokio::sync::oneshot;

fn prepare_pairing_route_insert(
    routes: &mut HashMap<RouteKey, PairingRoute>,
    desktop_id: &str,
    now: u64,
) -> Result<(), String> {
    routes.retain(|_, route| route.expires_at > now);
    routes.retain(|(candidate_desktop, _), _| candidate_desktop != desktop_id);
    if routes.len() >= MAX_PAIRING_ROUTES {
        return Err("pairing route capacity reached".into());
    }
    Ok(())
}

pub(super) async fn open_pairing_route(
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
    let mut routes = state.pairing_routes.write().await;
    if let Err(error) = prepare_pairing_route_insert(&mut routes, &desktop_id, now) {
        return json_error(StatusCode::TOO_MANY_REQUESTS, error);
    }
    routes.insert(
        (desktop_id, pairing_id),
        PairingRoute {
            token_hash: payload.token_hash.to_ascii_lowercase(),
            expires_at: payload.expires_at,
        },
    );
    StatusCode::NO_CONTENT.into_response()
}

pub(super) async fn delete_pairing_route(
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
pub(super) struct PairingQuery {
    desktop_id: String,
    pairing_id: String,
}

pub(super) async fn pairing_ws(
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

    let route_key = (desktop_id.clone(), pairing_id.clone());
    let route_active = state.pairing_routes.read().await
        .get(&route_key)
        .is_some_and(|route| route.expires_at > now_ms());
    if !route_active {
        state.pairing_routes.write().await.remove(&route_key);
        let _ = send_json(&mut socket, &PairingServerMessage::Error {
            message: "pairing route is no longer active".into(),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn route(expires_at: u64) -> PairingRoute {
        PairingRoute {
            token_hash: "ab".repeat(32),
            expires_at,
        }
    }

    #[test]
    fn pairing_route_insert_prunes_expired_and_superseded_routes() {
        let now = 1_000;
        let mut routes = HashMap::new();
        routes.insert(("d_a".into(), "p_old".into()), route(now + 100));
        routes.insert(("d_b".into(), "p_expired".into()), route(now));
        routes.insert(("d_c".into(), "p_keep".into()), route(now + 100));

        prepare_pairing_route_insert(&mut routes, "d_a", now).unwrap();

        assert!(!routes.contains_key(&("d_a".into(), "p_old".into())));
        assert!(!routes.contains_key(&("d_b".into(), "p_expired".into())));
        assert!(routes.contains_key(&("d_c".into(), "p_keep".into())));
    }

    #[test]
    fn pairing_route_insert_enforces_global_capacity() {
        let now = 1_000;
        let mut routes = HashMap::new();
        for index in 0..MAX_PAIRING_ROUTES {
            routes.insert(
                (format!("d_{index:04x}"), format!("p_{index:04x}")),
                route(now + 100),
            );
        }

        assert!(prepare_pairing_route_insert(&mut routes, "d_new", now).is_err());
        assert_eq!(routes.len(), MAX_PAIRING_ROUTES);
    }
}
