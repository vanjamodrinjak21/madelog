//! The device key: one ed25519 keypair per machine, used to sign the chain
//! root when you publish. It never leaves `~/.config/pop/device.key`.

use std::path::PathBuf;

use anyhow::{anyhow, Context, Result};
use ed25519_dalek::{Signer, SigningKey, SECRET_KEY_LENGTH};

/// Override the key directory (used by tests and by per-project installs).
pub const CONFIG_DIR_ENV: &str = "POP_CONFIG_DIR";

/// `~/.config/pop`, or `$POP_CONFIG_DIR`.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(CONFIG_DIR_ENV) {
        if !dir.is_empty() {
            return Ok(PathBuf::from(dir));
        }
    }
    let home = dirs::home_dir().ok_or_else(|| anyhow!("cannot find your home directory"))?;
    Ok(home.join(".config").join("pop"))
}

pub fn key_path() -> Result<PathBuf> {
    Ok(config_dir()?.join("device.key"))
}

pub struct DeviceKey {
    signing: SigningKey,
}

/// Never print the secret, not even by accident in a panic message.
impl std::fmt::Debug for DeviceKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DeviceKey")
            .field("public_key", &self.public_key_hex())
            .finish_non_exhaustive()
    }
}

impl DeviceKey {
    /// Load the device key, creating it on first use.
    pub fn load_or_create() -> Result<DeviceKey> {
        let path = key_path()?;
        if path.exists() {
            return DeviceKey::load();
        }
        let dir = path
            .parent()
            .ok_or_else(|| anyhow!("bad key path {}", path.display()))?;
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
        restrict_dir(dir)?;

        let signing = SigningKey::generate(&mut rand::rngs::OsRng);
        let mut body = hex::encode(signing.to_bytes());
        body.push('\n');
        std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
        restrict_file(&path)?;
        Ok(DeviceKey { signing })
    }

    pub fn load() -> Result<DeviceKey> {
        let path = key_path()?;
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading {}", path.display()))?;
        let bytes = hex::decode(raw.trim())
            .with_context(|| format!("{} is not a hex device key", path.display()))?;
        let bytes: [u8; SECRET_KEY_LENGTH] = bytes.try_into().map_err(|_| {
            anyhow!(
                "{} is not a {SECRET_KEY_LENGTH}-byte ed25519 key",
                path.display()
            )
        })?;
        Ok(DeviceKey {
            signing: SigningKey::from_bytes(&bytes),
        })
    }

    pub fn public_key_hex(&self) -> String {
        hex::encode(self.signing.verifying_key().to_bytes())
    }

    /// Detached ed25519 signature.
    pub fn sign(&self, bytes: &[u8]) -> [u8; 64] {
        self.signing.sign(bytes).to_bytes()
    }

    pub fn sign_hex(&self, bytes: &[u8]) -> String {
        hex::encode(self.sign(bytes))
    }
}

#[cfg(unix)]
fn restrict_file(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("restricting {}", path.display()))
}

#[cfg(unix)]
fn restrict_dir(path: &std::path::Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("restricting {}", path.display()))
}

#[cfg(not(unix))]
fn restrict_file(_path: &std::path::Path) -> Result<()> {
    // Windows inherits the user profile ACL, which is already owner-only.
    Ok(())
}

#[cfg(not(unix))]
fn restrict_dir(_path: &std::path::Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};

    /// These tests mutate a process-wide env var, so they run behind a lock.
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn with_temp_dir<T>(f: impl FnOnce(&std::path::Path) -> T) -> T {
        let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let dir = tempfile::TempDir::new().unwrap();
        let previous = std::env::var_os(CONFIG_DIR_ENV);
        std::env::set_var(CONFIG_DIR_ENV, dir.path());
        let out = f(dir.path());
        match previous {
            Some(v) => std::env::set_var(CONFIG_DIR_ENV, v),
            None => std::env::remove_var(CONFIG_DIR_ENV),
        }
        out
    }

    #[test]
    fn the_key_is_created_once_and_reused() {
        with_temp_dir(|dir| {
            let a = DeviceKey::load_or_create().unwrap();
            let b = DeviceKey::load_or_create().unwrap();
            assert_eq!(a.public_key_hex(), b.public_key_hex());
            assert!(dir.join("device.key").is_file());
        });
    }

    #[test]
    fn signatures_verify_against_the_public_key() {
        with_temp_dir(|_| {
            let key = DeviceKey::load_or_create().unwrap();
            let sig = key.sign(b"root||ts||slug");
            let pk_bytes: [u8; 32] = hex::decode(key.public_key_hex())
                .unwrap()
                .try_into()
                .unwrap();
            let vk = VerifyingKey::from_bytes(&pk_bytes).unwrap();
            vk.verify(b"root||ts||slug", &Signature::from_bytes(&sig))
                .expect("signature verifies");
            assert!(vk
                .verify(b"something else", &Signature::from_bytes(&sig))
                .is_err());
            assert_eq!(key.sign_hex(b"x").len(), 128);
        });
    }

    #[cfg(unix)]
    #[test]
    fn the_key_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        with_temp_dir(|dir| {
            DeviceKey::load_or_create().unwrap();
            let mode = std::fs::metadata(dir.join("device.key"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        });
    }

    #[test]
    fn a_corrupt_key_file_reports_the_path() {
        with_temp_dir(|dir| {
            std::fs::write(dir.join("device.key"), "not hex").unwrap();
            let err = DeviceKey::load_or_create().unwrap_err();
            assert!(format!("{err:#}").contains("device.key"), "{err:#}");
        });
    }
}
