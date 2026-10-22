//! Short-lived enrollment codes copied from the administrator's Relay panel.
//! Parsing does not authenticate a code; only the Relay verifies its signature.
use serde::{Deserialize, Serialize};

pub const CODE_PREFIX: &str = "cxr1";
pub const SIGNING_CONTEXT: &str = "chatx-registration-v1:";
pub const MAX_INVITE_LIFETIME_MS: u64 = 10 * 60 * 1000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegistrationInvite {
    pub base_url: String,
    pub expires_at: u64,
    pub nonce: String,
}

pub fn decode_hex(value: &str) -> Result<Vec<u8>, &'static str> {
    if value.len() % 2 != 0 || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("注册码格式不正确，请从宝塔插件重新复制。");
    }
    value.as_bytes().chunks_exact(2).map(|pair| {
        let text = std::str::from_utf8(pair).map_err(|_| "注册码编码无效。")?;
        u8::from_str_radix(text, 16).map_err(|_| "注册码编码无效。")
    }).collect()
}

pub fn parse_code(code: &str) -> Result<(RegistrationInvite, String, Vec<u8>), &'static str> {
    let code = code.trim();
    if code.len() > 2048 { return Err("注册码过长。"); }
    let parts: Vec<_> = code.split('.').collect();
    if parts.len() != 3 || parts[0] != CODE_PREFIX || parts[2].len() != 64 {
        return Err("注册码格式不正确，请从宝塔插件重新复制。");
    }
    let invite: RegistrationInvite = serde_json::from_slice(&decode_hex(parts[1])?)
        .map_err(|_| "注册码内容无效。")?;
    let host = invite.base_url.strip_prefix("https://")
        .ok_or("注册码中的 Relay 地址必须使用 HTTPS。")?;
    if host.is_empty() || host.len() > 253
        || !host.split('.').all(|label| !label.is_empty()
            && label.len() <= 63 && !label.starts_with('-') && !label.ends_with('-')
            && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
        || invite.nonce.len() != 32
        || !invite.nonce.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("注册码中的 Relay 地址或随机标识无效。");
    }
    Ok((invite, format!("{SIGNING_CONTEXT}{}", parts[1]), decode_hex(parts[2])?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn code(url: &str) -> String {
        let value = RegistrationInvite { base_url: url.into(), expires_at: 42, nonce: "ab".repeat(16) };
        let body: String = serde_json::to_vec(&value).unwrap().iter().map(|b| format!("{b:02x}")).collect();
        format!("cxr1.{body}.{}", "00".repeat(32))
    }

    #[test]
    fn registration_destination_must_be_an_https_origin() {
        assert_eq!(parse_code(&code("https://relay.example.com")).unwrap().0.expires_at, 42);
        for url in ["http://relay.example.com", "https://user:pass@relay.example.com",
            "https://relay.example.com/path", "https://relay.example.com:443",
            "https://relay..example.com", "https://-example.com"] {
            assert!(parse_code(&code(url)).is_err(), "{url}");
        }
        assert!(parse_code("cxr1.not-hex.signature").is_err());
        assert!(parse_code(&"x".repeat(2049)).is_err());
    }
}
