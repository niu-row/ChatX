use ring::digest;
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    path::Path,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DeviceRecord {
    pub(crate) token_hash: String,
    pub(crate) created_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct DesktopRecord {
    pub(crate) token_hash: String,
    pub(crate) device_name: String,
    pub(crate) app_version: String,
    pub(crate) platform: String,
    pub(crate) created_at: u64,
    #[serde(default)]
    pub(crate) devices: HashMap<String, DeviceRecord>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct Registry {
    #[serde(default)]
    pub(crate) desktops: HashMap<String, DesktopRecord>,
}

pub(crate) fn token_hash(token: &str) -> String {
    digest::digest(&digest::SHA256, token.as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn constant_time_hex_eq(left: &str, right: &str) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.as_bytes().iter().zip(right.as_bytes()) {
        diff |= a ^ b;
    }
    diff == 0
}

pub(crate) fn hash_matches(token: &str, expected_hex: &str) -> bool {
    constant_time_hex_eq(&token_hash(token), expected_hex)
}

fn parse_registry(path: &Path, text: &str) -> Result<Registry, String> {
    serde_json::from_str::<Registry>(text).map_err(|error| {
        format!(
            "relay registry is corrupt at {}: {error}",
            path.display()
        )
    })
}

fn backup_path(path: &Path) -> std::path::PathBuf {
    path.with_extension("json.bak")
}

pub(crate) fn load(path: &Path) -> Result<Registry, String> {
    match fs::read_to_string(path) {
        Ok(text) => parse_registry(path, &text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let backup = backup_path(path);
            match fs::read_to_string(&backup) {
                Ok(text) => {
                    let registry = parse_registry(&backup, &text)?;
                    let _ = fs::rename(&backup, path);
                    Ok(registry)
                }
                Err(backup_error)
                    if backup_error.kind() == std::io::ErrorKind::NotFound =>
                {
                    Ok(Registry::default())
                }
                Err(backup_error) => Err(format!(
                    "read relay registry backup {} failed: {backup_error}",
                    backup.display()
                )),
            }
        }
        Err(error) => Err(format!(
            "read relay registry {} failed: {error}",
            path.display()
        )),
    }
}

pub(crate) fn save(path: &Path, registry: &Registry) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|error| {
                format!("create relay data directory failed: {error}")
            })?;
    }
    let temp = path.with_extension("json.tmp");
    let text = serde_json::to_string_pretty(registry)
        .map_err(|error| format!("serialize registry failed: {error}"))?;
    fs::write(&temp, format!("{text}\n"))
        .map_err(|error| format!("write relay registry failed: {error}"))?;

    #[cfg(windows)]
    {
        let backup = backup_path(path);
        let _ = fs::remove_file(&backup);
        if path.exists() {
            fs::rename(path, &backup).map_err(|error| {
                format!("backup relay registry failed: {error}")
            })?;
        }
        match fs::rename(&temp, path) {
            Ok(()) => {
                let _ = fs::remove_file(backup);
                Ok(())
            }
            Err(error) => {
                let _ = fs::rename(&backup, path);
                Err(format!("replace relay registry failed: {error}"))
            }
        }
    }

    #[cfg(not(windows))]
    {
        fs::rename(&temp, path)
            .map_err(|error| format!("replace relay registry failed: {error}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_hash_is_stable_and_verifiable() {
        let hash = token_hash("secret");
        assert_eq!(hash.len(), 64);
        assert!(hash_matches("secret", &hash));
        assert!(!hash_matches("other", &hash));
    }

    #[test]
    fn missing_primary_recovers_valid_backup() {
        let path = std::env::temp_dir().join(format!(
            "chatx-relay-registry-backup-test-{}.json",
            std::process::id(),
        ));
        let backup = backup_path(&path);
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&backup);

        let mut registry = Registry::default();
        registry.desktops.insert(
            "d_test".into(),
            DesktopRecord {
                token_hash: token_hash("desktop-secret"),
                device_name: "Desktop".into(),
                app_version: "0.4.10".into(),
                platform: "windows-x64".into(),
                created_at: 1,
                devices: HashMap::new(),
            },
        );
        fs::write(
            &backup,
            serde_json::to_string_pretty(&registry).unwrap(),
        ).unwrap();

        let recovered = load(&path).unwrap();
        assert!(recovered.desktops.contains_key("d_test"));
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(&backup);
    }
}
