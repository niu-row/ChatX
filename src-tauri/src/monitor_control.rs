use super::*;
use chatx_relay_protocol::EncryptedControlPayload;
use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    thread,
};

#[derive(Debug, Clone)]
struct CacheEntry {
    expires_at: u64,
    issued_at: u64,
    nonce: String,
    ciphertext: String,
    response: EncryptedControlPayload,
}

#[derive(Default)]
struct ControlCache {
    entries: HashMap<String, CacheEntry>,
}

#[derive(Default)]
pub(super) struct State {
    cache: Mutex<ControlCache>,
    reconnect_running: AtomicBool,
    last_reconnect_at: AtomicU64,
}

impl ControlCache {
    fn cached_response(
        &mut self,
        device_id: &str,
        request: &EncryptedControlPayload,
    ) -> Result<Option<EncryptedControlPayload>, String> {
        let now = timestamp_ms();
        self.entries.retain(|_, entry| entry.expires_at >= now);
        let key = format!("{device_id}|{}", request.request_id);
        let Some(entry) = self.entries.get(&key) else {
            return Ok(None);
        };
        if entry.issued_at != request.issued_at
            || entry.expires_at != request.expires_at
            || entry.nonce != request.nonce
            || entry.ciphertext != request.ciphertext
        {
            return Err(
                "Monitor 控制 requestId 与先前请求冲突。".into(),
            );
        }
        Ok(Some(entry.response.clone()))
    }

    fn insert(
        &mut self,
        device_id: &str,
        request: EncryptedControlPayload,
        response: EncryptedControlPayload,
    ) {
        self.entries.insert(
            format!("{device_id}|{}", request.request_id),
            CacheEntry {
                expires_at: request.expires_at,
                issued_at: request.issued_at,
                nonce: request.nonce,
                ciphertext: request.ciphertext,
                response,
            },
        );
    }
}

fn validate_request(request: &EncryptedControlPayload) -> Result<(), String> {
    let now = timestamp_ms();
    let valid_id = request.request_id.starts_with("c_")
        && request.request_id.len() >= 18
        && request.request_id.len() <= 80
        && request.request_id[2..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit());
    let valid_time = request.issued_at <= now.saturating_add(30_000)
        && now.saturating_sub(request.issued_at) <= 60_000
        && request.expires_at >= now
        && request.expires_at
            <= request.issued_at.saturating_add(60_000);
    if !valid_id || !valid_time {
        return Err("Monitor 控制请求已过期或格式无效。".into());
    }
    Ok(())
}

fn trigger_tunnel_reconnect(
    app: &tauri::AppHandle,
    state: &AppState,
) -> Result<String, String> {
    let settings = load_settings(app);
    if !settings.connection_intent
        || !state.desired_connected.load(Ordering::SeqCst)
    {
        return Err(
            "Desktop 已手动停止 Tunnel，手机不能覆盖该连接意图。".into(),
        );
    }
    if settings.tunnel_id.trim().is_empty() {
        return Err("Desktop 尚未配置 Tunnel ID。".into());
    }

    let health_now = timestamp_ms();
    let already_healthy = state
        .runtime_snapshot
        .lock()
        .map(|snapshot| {
            snapshot.active
                && snapshot.health == "healthy"
                && snapshot.last_probe_at > 0
                && health_now.saturating_sub(snapshot.last_probe_at) <= 12_000
        })
        .unwrap_or(false);
    if already_healthy {
        return Err("Tunnel 当前健康，无需重连。".into());
    }
    if state.monitor_control.reconnect_running.load(Ordering::SeqCst)
        || state.reconnecting.load(Ordering::SeqCst)
    {
        return Err("Tunnel 已在执行重连。".into());
    }

    let now = timestamp_ms();
    let previous = state.monitor_control.last_reconnect_at.load(Ordering::SeqCst);
    if previous > 0 && now.saturating_sub(previous) < 10_000 {
        return Err("Tunnel 重连操作过于频繁，请稍后再试。".into());
    }
    let key = state
        .session_runtime_key
        .lock()
        .ok()
        .and_then(|value| value.clone())
        .or_else(|| load_runtime_key(app, "").ok())
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            "Desktop 当前没有可用于重连的 Runtime Key。".to_string()
        })?;

    if state.monitor_control.reconnect_running.swap(true, Ordering::SeqCst) {
        return Err("Tunnel 已在执行远程重连。".into());
    }
    state.monitor_control.last_reconnect_at.store(now, Ordering::SeqCst);
    state.reconnecting.store(true, Ordering::SeqCst);

    let app = app.clone();
    thread::spawn(move || {
        let state = app.state::<AppState>();
        let settings = load_settings(&app);
        push_log(
            state.inner(),
            "手机 Monitor 请求立即重连 Secure MCP Tunnel",
        );
        let result = start_runtime_connection(
            &app,
            state.inner(),
            &settings.tunnel_id,
            &key,
            false,
            true,
        );
        if let Ok(paths) = runtime_paths(&app) {
            let observation = runtime_status(&app, &paths);
            let current_settings = load_settings(&app);
            let desired_connected =
                state.desired_connected.load(Ordering::SeqCst);
            update_runtime_snapshot(
                state.inner(),
                &observation,
                desired_connected,
                &current_settings,
            );
        }
        match result {
            Ok(()) => {
                state.reconnect_attempt.store(0, Ordering::SeqCst);
                push_log(
                    state.inner(),
                    "手机触发的 Tunnel 重连命令已完成",
                );
            }
            Err(error) => {
                push_log(
                    state.inner(),
                    format!("手机触发的 Tunnel 重连失败：{error}"),
                );
            }
        }
        state.reconnecting.store(false, Ordering::SeqCst);
        state.monitor_control.reconnect_running.store(false, Ordering::SeqCst);
    });
    Ok("已向 Desktop 提交 Tunnel 重连请求。".into())
}

pub(super) fn handle(
    app: &tauri::AppHandle,
    state: &AppState,
    device_id: &str,
    request: EncryptedControlPayload,
) -> Result<EncryptedControlPayload, String> {
    if !monitor_manager::has_monitor_device(app, state, device_id) {
        return Err("Monitor 设备已被撤销。".into());
    }
    validate_request(&request)?;
    let master_key = ensure_monitor_master_key(app)?;
    let device_key =
        monitor_crypto::derive_device_key(&master_key, device_id)?;
    let desktop_id = ensure_monitor_desktop_id(app)?;
    let clear = monitor_crypto::decrypt_control_payload(
        &device_key,
        &desktop_id,
        device_id,
        "request",
        &request,
    )?;
    let payload = serde_json::from_slice::<Value>(&clear)
        .map_err(|_| "Monitor 控制 payload 无效。".to_string())?;
    let action = payload
        .get("action")
        .and_then(Value::as_str)
        .unwrap_or("");

    let mut cache = state
        .monitor_control
        .cache
        .lock()
        .map_err(|_| "Monitor 控制幂等缓存锁已损坏。".to_string())?;
    if let Some(response) = cache.cached_response(device_id, &request)? {
        return Ok(response);
    }

    let response_value = match action {
        "refresh_snapshot" => match monitor_status::fresh_payload(app, state) {
            Ok(snapshot) => json!({
                "ok": true,
                "action": action,
                "snapshot": snapshot
            }),
            Err(message) => json!({
                "ok": false,
                "action": action,
                "message": message
            }),
        },
        "reconnect_tunnel" => match trigger_tunnel_reconnect(app, state) {
            Ok(message) => json!({
                "ok": true,
                "action": action,
                "accepted": true,
                "message": message,
                "snapshot": monitor_status::payload(app, state)
            }),
            Err(message) => json!({
                "ok": false,
                "action": action,
                "message": message,
                "snapshot": monitor_status::payload(app, state)
            }),
        },
        _ => json!({
            "ok": false,
            "action": action,
            "message": "不支持的 Monitor 控制动作。"
        }),
    };
    let encrypted = monitor_crypto::encrypt_control_payload(
        &device_key,
        &desktop_id,
        device_id,
        &request.request_id,
        "response",
        request.issued_at,
        request.expires_at,
        &serde_json::to_vec(&response_value)
            .map_err(|error| {
                format!("序列化 Monitor 控制响应失败：{error}")
            })?,
    )?;
    cache.insert(device_id, request, encrypted.clone());
    Ok(encrypted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_cache_is_idempotent_and_rejects_conflicting_reuse() {
        let request = EncryptedControlPayload {
            request_id: "c_0123456789abcdef".into(),
            issued_at: timestamp_ms(),
            expires_at: timestamp_ms().saturating_add(30_000),
            nonce: "same-nonce".into(),
            ciphertext: "same-ciphertext".into(),
        };
        let response = EncryptedControlPayload {
            request_id: request.request_id.clone(),
            issued_at: request.issued_at,
            expires_at: request.expires_at,
            nonce: "response-nonce".into(),
            ciphertext: "response-ciphertext".into(),
        };
        let mut cache = ControlCache::default();
        cache.insert("dev_test", request.clone(), response.clone());

        assert_eq!(
            cache.cached_response("dev_test", &request).unwrap(),
            Some(response),
        );
        let mut conflicting = request;
        conflicting.ciphertext = "different".into();
        assert!(
            cache
                .cached_response("dev_test", &conflicting)
                .is_err()
        );
    }
}
