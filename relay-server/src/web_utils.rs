use axum::{
    extract::ws::{Message, WebSocket},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use ring::rand::{SecureRandom, SystemRandom};
use serde::Serialize;
use serde_json::json;
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

pub(super) fn random_hex(bytes: usize) -> Result<String, String> {
    let mut value = vec![0u8; bytes];
    SystemRandom::new()
        .fill(&mut value)
        .map_err(|_| "secure random generation failed".to_string())?;
    Ok(value
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}
pub(super) fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get("authorization")?.to_str().ok()?;
    value
        .strip_prefix("Bearer ")
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(super) fn json_error(
    status: StatusCode,
    message: impl Into<String>,
) -> Response {
    (
        status,
        Json(json!({ "error": message.into() })),
    )
        .into_response()
}

pub(super) async fn send_json<T: Serialize>(
    socket: &mut WebSocket,
    value: &T,
) -> Result<(), ()> {
    let text = serde_json::to_string(value).map_err(|_| ())?;
    socket
        .send(Message::Text(text.into()))
        .await
        .map_err(|_| ())
}
