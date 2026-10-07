use super::AppState;
use crate::registry::{
    hash_matches, token_hash, DesktopRecord, DeviceRecord,
};
use crate::web_utils::{bearer, json_error, now_ms, random_hex};
use axum::{
    extract::{Path as AxumPath, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chatx_relay_protocol::{
    DesktopRegisterRequest, DesktopRegisterResponse, DeviceAuthorizeRequest,
};

pub(super) fn desktop_registration_record(
    existing: Option<&DesktopRecord>,
    desktop_token: &str,
    payload: &DesktopRegisterRequest,
) -> DesktopRecord {
    DesktopRecord {
        token_hash: token_hash(desktop_token),
        device_name: payload.device_name.chars().take(128).collect(),
        app_version: payload.app_version.chars().take(64).collect(),
        platform: payload.platform.chars().take(64).collect(),
        created_at: existing
            .map(|record| record.created_at)
            .unwrap_or_else(now_ms),
        devices: existing
            .map(|record| record.devices.clone())
            .unwrap_or_default(),
    }
}

pub(super) async fn register_desktop(
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
    if !hash_matches(token, expected) {
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

pub(super) async fn authorize_device(
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

pub(super) async fn revoke_device(
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
