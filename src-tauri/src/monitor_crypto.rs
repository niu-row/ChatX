use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chatx_relay_protocol::{EncryptedPairingPayload, EncryptedSnapshot};
use ring::{
    aead,
    hkdf,
    rand::{SecureRandom, SystemRandom},
};

const MASTER_KEY_LEN: usize = 32;
const NONCE_LEN: usize = 12;

struct KeyLen;
impl hkdf::KeyType for KeyLen {
    fn len(&self) -> usize { 32 }
}

pub fn generate_master_key() -> Result<[u8; MASTER_KEY_LEN], String> {
    let mut key = [0u8; MASTER_KEY_LEN];
    SystemRandom::new()
        .fill(&mut key)
        .map_err(|_| "生成 Monitor Master Key 失败。".to_string())?;
    Ok(key)
}

pub fn encode_master_key(key: &[u8; MASTER_KEY_LEN]) -> String {
    URL_SAFE_NO_PAD.encode(key)
}

pub fn decode_master_key(value: &str) -> Result<[u8; MASTER_KEY_LEN], String> {
    let decoded = URL_SAFE_NO_PAD.decode(value.trim())
        .map_err(|_| "Monitor Master Key 编码无效。".to_string())?;
    if decoded.len() != MASTER_KEY_LEN {
        return Err("Monitor Master Key 长度无效。".into());
    }
    let mut key = [0u8; MASTER_KEY_LEN];
    key.copy_from_slice(&decoded);
    Ok(key)
}

fn pairing_key(pairing_code: &str) -> Result<[u8; 32], String> {
    let value = pairing_code.trim();
    if value.len() != 64 {
        return Err("Pairing secret 长度无效。".into());
    }
    let mut key = [0u8; 32];
    for (index, slot) in key.iter_mut().enumerate() {
        let start = index * 2;
        *slot = u8::from_str_radix(&value[start..start + 2], 16)
            .map_err(|_| "Pairing secret 编码无效。".to_string())?;
    }
    Ok(key)
}

pub fn derive_device_key(
    master_key: &[u8; MASTER_KEY_LEN],
    device_id: &str,
) -> Result<[u8; 32], String> {
    derive(master_key, device_id, b"chatx-monitor-device-e2ee-v1")
}

pub fn derive_direct_token(
    master_key: &[u8; MASTER_KEY_LEN],
    device_id: &str,
) -> Result<String, String> {
    Ok(hex(&derive(master_key, device_id, b"chatx-monitor-direct-auth-v1")?))
}

pub fn derive_relay_token(
    master_key: &[u8; MASTER_KEY_LEN],
    device_id: &str,
) -> Result<String, String> {
    Ok(hex(&derive(master_key, device_id, b"chatx-monitor-relay-auth-v1")?))
}

pub fn device_key_b64(
    master_key: &[u8; MASTER_KEY_LEN],
    device_id: &str,
) -> Result<String, String> {
    Ok(URL_SAFE_NO_PAD.encode(derive_device_key(master_key, device_id)?))
}

fn derive(
    master_key: &[u8; MASTER_KEY_LEN],
    device_id: &str,
    label: &[u8],
) -> Result<[u8; 32], String> {
    let salt = hkdf::Salt::new(hkdf::HKDF_SHA256, b"chatx-monitor-v1");
    let prk = salt.extract(master_key);
    let info = [label, b"|", device_id.as_bytes()];
    let okm = prk.expand(&info, KeyLen)
        .map_err(|_| "派生 Monitor 设备密钥失败。".to_string())?;
    let mut out = [0u8; 32];
    okm.fill(&mut out)
        .map_err(|_| "派生 Monitor 设备密钥失败。".to_string())?;
    Ok(out)
}

pub fn encrypt_snapshot(
    master_key: &[u8; MASTER_KEY_LEN],
    desktop_id: &str,
    device_id: &str,
    session_id: &str,
    sequence: u64,
    generated_at: u64,
    plaintext: &[u8],
) -> Result<EncryptedSnapshot, String> {
    let device_key = derive_device_key(master_key, device_id)?;
    let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, &device_key)
        .map_err(|_| "初始化 Monitor E2EE 失败。".to_string())?;
    let key = aead::LessSafeKey::new(unbound);

    let mut nonce_bytes = [0u8; NONCE_LEN];
    SystemRandom::new()
        .fill(&mut nonce_bytes)
        .map_err(|_| "生成 Monitor E2EE nonce 失败。".to_string())?;
    let nonce = aead::Nonce::assume_unique_for_key(nonce_bytes);
    let aad = snapshot_aad(
        desktop_id,
        device_id,
        session_id,
        sequence,
        generated_at,
    );

    let mut ciphertext = plaintext.to_vec();
    key.seal_in_place_append_tag(
        nonce,
        aead::Aad::from(aad.as_bytes()),
        &mut ciphertext,
    ).map_err(|_| "加密 Monitor Snapshot 失败。".to_string())?;

    Ok(EncryptedSnapshot {
        session_id: session_id.to_string(),
        sequence,
        generated_at,
        nonce: URL_SAFE_NO_PAD.encode(nonce_bytes),
        ciphertext: URL_SAFE_NO_PAD.encode(ciphertext),
    })
}

#[cfg(test)]
pub fn decrypt_snapshot(
    device_key: &[u8; 32],
    desktop_id: &str,
    device_id: &str,
    snapshot: &EncryptedSnapshot,
) -> Result<Vec<u8>, String> {
    let nonce_bytes = URL_SAFE_NO_PAD.decode(&snapshot.nonce)
        .map_err(|_| "Monitor Snapshot nonce 编码无效。".to_string())?;
    if nonce_bytes.len() != NONCE_LEN {
        return Err("Monitor Snapshot nonce 长度无效。".into());
    }
    let mut nonce_array = [0u8; NONCE_LEN];
    nonce_array.copy_from_slice(&nonce_bytes);

    let mut ciphertext = URL_SAFE_NO_PAD.decode(&snapshot.ciphertext)
        .map_err(|_| "Monitor Snapshot ciphertext 编码无效。".to_string())?;
    let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, device_key)
        .map_err(|_| "初始化 Monitor E2EE 解密失败。".to_string())?;
    let key = aead::LessSafeKey::new(unbound);
    let aad = snapshot_aad(
        desktop_id,
        device_id,
        &snapshot.session_id,
        snapshot.sequence,
        snapshot.generated_at,
    );
    let plaintext = key.open_in_place(
        aead::Nonce::assume_unique_for_key(nonce_array),
        aead::Aad::from(aad.as_bytes()),
        &mut ciphertext,
    ).map_err(|_| "Monitor Snapshot E2EE 验证失败。".to_string())?;
    Ok(plaintext.to_vec())
}

pub fn encrypt_pairing_payload(
    pairing_code: &str,
    pairing_id: &str,
    direction: &str,
    plaintext: &[u8],
) -> Result<EncryptedPairingPayload, String> {
    let raw_key = pairing_key(pairing_code)?;
    let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, &raw_key)
        .map_err(|_| "初始化 Pairing E2EE 失败。".to_string())?;
    let key = aead::LessSafeKey::new(unbound);
    let mut nonce_bytes = [0u8; NONCE_LEN];
    SystemRandom::new()
        .fill(&mut nonce_bytes)
        .map_err(|_| "生成 Pairing nonce 失败。".to_string())?;
    let mut ciphertext = plaintext.to_vec();
    key.seal_in_place_append_tag(
        aead::Nonce::assume_unique_for_key(nonce_bytes),
        aead::Aad::from(pairing_aad(pairing_id, direction).as_bytes()),
        &mut ciphertext,
    ).map_err(|_| "加密 Pairing payload 失败。".to_string())?;
    Ok(EncryptedPairingPayload {
        nonce: URL_SAFE_NO_PAD.encode(nonce_bytes),
        ciphertext: URL_SAFE_NO_PAD.encode(ciphertext),
    })
}

pub fn decrypt_pairing_payload(
    pairing_code: &str,
    pairing_id: &str,
    direction: &str,
    payload: &EncryptedPairingPayload,
) -> Result<Vec<u8>, String> {
    let raw_key = pairing_key(pairing_code)?;
    let nonce = URL_SAFE_NO_PAD.decode(&payload.nonce)
        .map_err(|_| "Pairing nonce 编码无效。".to_string())?;
    if nonce.len() != NONCE_LEN {
        return Err("Pairing nonce 长度无效。".into());
    }
    let mut nonce_bytes = [0u8; NONCE_LEN];
    nonce_bytes.copy_from_slice(&nonce);
    let mut ciphertext = URL_SAFE_NO_PAD.decode(&payload.ciphertext)
        .map_err(|_| "Pairing ciphertext 编码无效。".to_string())?;
    let unbound = aead::UnboundKey::new(&aead::AES_256_GCM, &raw_key)
        .map_err(|_| "初始化 Pairing E2EE 解密失败。".to_string())?;
    let key = aead::LessSafeKey::new(unbound);
    let plaintext = key.open_in_place(
        aead::Nonce::assume_unique_for_key(nonce_bytes),
        aead::Aad::from(pairing_aad(pairing_id, direction).as_bytes()),
        &mut ciphertext,
    ).map_err(|_| "Pairing E2EE 验证失败。".to_string())?;
    Ok(plaintext.to_vec())
}

fn pairing_aad(pairing_id: &str, direction: &str) -> String {
    format!("chatx-monitor-v1|pairing|{pairing_id}|{direction}")
}

fn snapshot_aad(
    desktop_id: &str,
    device_id: &str,
    session_id: &str,
    sequence: u64,
    generated_at: u64,
) -> String {
    format!(
        "chatx-monitor-v1|snapshot|{desktop_id}|{device_id}|{session_id}|{sequence}|{generated_at}"
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_device_material_is_deterministic_and_separated() {
        let master = [7u8; 32];
        let a_key = derive_device_key(&master, "dev_a").unwrap();
        let a_key_again = derive_device_key(&master, "dev_a").unwrap();
        let b_key = derive_device_key(&master, "dev_b").unwrap();
        assert_eq!(a_key, a_key_again);
        assert_ne!(a_key, b_key);
        assert_ne!(
            derive_direct_token(&master, "dev_a").unwrap(),
            derive_relay_token(&master, "dev_a").unwrap(),
        );
    }

    #[test]
    fn pairing_round_trip_binds_pairing_id_and_direction() {
        let pairing_code = "ab".repeat(32);
        let pairing_id = "p_0123456789abcdef01234567";
        let encrypted = encrypt_pairing_payload(
            &pairing_code,
            pairing_id,
            "request",
            br#"{"deviceName":"Pixel"}"#,
        ).unwrap();
        let clear = decrypt_pairing_payload(
            &pairing_code,
            pairing_id,
            "request",
            &encrypted,
        ).unwrap();
        assert_eq!(clear, br#"{"deviceName":"Pixel"}"#);
        assert!(decrypt_pairing_payload(
            &pairing_code,
            "p_other",
            "request",
            &encrypted,
        ).is_err());
        assert!(decrypt_pairing_payload(
            &pairing_code,
            pairing_id,
            "response",
            &encrypted,
        ).is_err());
    }

    #[test]
    fn snapshot_round_trip_binds_route_metadata() {
        let master = [9u8; 32];
        let key = derive_device_key(&master, "dev_a").unwrap();
        let encrypted = encrypt_snapshot(
            &master,
            "d_a",
            "dev_a",
            "session_a",
            3,
            1234,
            br#"{"ok":true}"#,
        ).unwrap();
        let clear = decrypt_snapshot(&key, "d_a", "dev_a", &encrypted).unwrap();
        assert_eq!(clear, br#"{"ok":true}"#);
        assert!(decrypt_snapshot(&key, "d_other", "dev_a", &encrypted).is_err());
        assert!(decrypt_snapshot(&key, "d_a", "dev_other", &encrypted).is_err());
    }
}
