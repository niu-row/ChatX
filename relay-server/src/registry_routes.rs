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
use chatx_relay_protocol::registration::{parse_code, MAX_INVITE_LIFETIME_MS};
use ring::hmac;

fn registration_invite(token: &str, expected: &str, now: u64)
    -> Result<Option<(String, u64)>, &'static str> {
    if hash_matches(token, expected) { return Ok(None); }
    let (invite, signed, signature) = parse_code(token)
        .map_err(|_| "invalid registration code")?;
    hmac::verify(&hmac::Key::new(hmac::HMAC_SHA256, expected.as_bytes()),
        signed.as_bytes(), &signature).map_err(|_| "invalid registration code")?;
    if invite.expires_at <= now
        || invite.expires_at > now.saturating_add(MAX_INVITE_LIFETIME_MS) {
        return Err("registration code expired; generate a new code in the Relay panel");
    }
    Ok(Some((token_hash(&invite.nonce), invite.expires_at)))
}

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
    let invite = match registration_invite(token, expected, now_ms()) {
        Ok(invite) => invite,
        Err(error) => return json_error(StatusCode::UNAUTHORIZED, error),
    };

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

    {
    let mut registry = match state.registry.lock() {
        Ok(value) => value,
        Err(_) => return json_error(StatusCode::INTERNAL_SERVER_ERROR, "registry lock poisoned"),
    };
    let previous = registry.desktops.get(&desktop_id).cloned();
    let previous_invites = registry.used_registration_invites.clone();
    if let Some((id, expires_at)) = invite {
        if registry.used_registration_invites.contains_key(&id) {
            return json_error(StatusCode::UNAUTHORIZED, "registration code already used; generate a new code");
        }
        registry.used_registration_invites.retain(|_, expiry| *expiry > now_ms());
        registry.used_registration_invites.insert(id, expires_at);
    }
    let replacement = desktop_registration_record(previous.as_ref(), &desktop_token, &payload);
    registry.desktops.insert(desktop_id.clone(), replacement);
    if let Err(error) = state.save_registry(&registry) {
        registry.used_registration_invites = previous_invites;
        if let Some(record) = previous {
            registry.desktops.insert(desktop_id.clone(), record);
        } else {
            registry.desktops.remove(&desktop_id);
        }
        return json_error(StatusCode::INTERNAL_SERVER_ERROR, error);
    }
    }
    // A fresh Desktop Token revokes the previous MCP websocket immediately.
    state.mcp_gateway.revoke_desktop(&desktop_id).await;
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

#[cfg(test)]
mod tests {
    use super::*;
    use chatx_relay_protocol::registration::{RegistrationInvite, SIGNING_CONTEXT};

    fn invite_code(secret: &str, expires_at: u64) -> String {
        let invite = RegistrationInvite {
            base_url: "https://relay.example.com".into(), expires_at,
            nonce: "ab".repeat(16),
        };
        let body: String = serde_json::to_vec(&invite).unwrap().iter()
            .map(|byte| format!("{byte:02x}")).collect();
        let key = hmac::Key::new(hmac::HMAC_SHA256, token_hash(secret).as_bytes());
        let signature: String = hmac::sign(&key, format!("{SIGNING_CONTEXT}{body}").as_bytes())
            .as_ref().iter().map(|byte| format!("{byte:02x}")).collect();
        format!("cxr1.{body}.{signature}")
    }

    fn headers(code: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", format!("Bearer {code}").parse().unwrap());
        headers
    }

    fn payload(digit: &str) -> Json<DesktopRegisterRequest> {
        Json(DesktopRegisterRequest {
            desktop_id: format!("d_{}", digit.repeat(32)), device_name: "Test desktop".into(),
            app_version: "test".into(), platform: "test".into(),
        })
    }

    #[test]
    fn invitation_signature_and_lifetime_are_checked() {
        let hash = token_hash("admin-secret");
        assert!(registration_invite("admin-secret", &hash, 10).unwrap().is_none());
        let code = invite_code("admin-secret", 1000);
        assert!(registration_invite(&code, &hash, 100).unwrap().is_some());
        assert!(registration_invite(&code, &hash, 1000).is_err());
        assert!(registration_invite(&code, &token_hash("wrong-server"), 100).is_err());
        assert!(registration_invite(&invite_code("admin-secret", MAX_INVITE_LIFETIME_MS + 101),
            &hash, 100).is_err());
        let tampered = code.replacen("6874747073", "6874747072", 1);
        assert!(registration_invite(&tampered, &hash, 100).is_err());
    }

    #[tokio::test]
    async fn invitation_is_consumed_once_under_concurrency_and_after_restart() {
        let path = std::env::temp_dir().join(format!("chatx-invite-{}.json", random_hex(8).unwrap()));
        let state = AppState::load(path.clone(), Some("admin-secret".into())).unwrap();
        let code = invite_code("admin-secret", now_ms() + MAX_INVITE_LIFETIME_MS);
        let (a, b) = tokio::join!(
            register_desktop(State(state.clone()), headers(&code), payload("a")),
            register_desktop(State(state.clone()), headers(&code), payload("b")),
        );
        let statuses = [a.status(), b.status()];
        assert_eq!(statuses.iter().filter(|s| **s == StatusCode::OK).count(), 1);
        assert_eq!(statuses.iter().filter(|s| **s == StatusCode::UNAUTHORIZED).count(), 1);
        let restored = AppState::load(path.clone(), Some("admin-secret".into())).unwrap();
        assert_eq!(register_desktop(State(restored), headers(&code), payload("c")).await.status(),
            StatusCode::UNAUTHORIZED);
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(!text.contains(&code));
        assert!(!text.contains("admin-secret"));
        std::fs::remove_file(path).unwrap();
    }

    #[tokio::test]
    async fn failed_save_does_not_consume_invitation_or_replace_desktop() {
        let path = std::env::temp_dir().join(format!("chatx-invite-fail-{}", random_hex(8).unwrap()));
        let mut state = AppState::load(path.clone(), Some("admin-secret".into())).unwrap();
        std::fs::write(&path, b"a file cannot be a parent directory").unwrap();
        state.registry_path = path.join("registry.json");
        let code = invite_code("admin-secret", now_ms() + MAX_INVITE_LIFETIME_MS);
        assert_eq!(register_desktop(State(state.clone()), headers(&code), payload("a")).await.status(),
            StatusCode::INTERNAL_SERVER_ERROR);
        let registry = state.registry.lock().unwrap();
        assert!(registry.desktops.is_empty());
        assert!(registry.used_registration_invites.is_empty());
        std::fs::remove_file(path).unwrap();
    }
}
