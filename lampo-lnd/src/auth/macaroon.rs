//! LND-compatible macaroon bakery for lampo's REST surface.
//!
//! Zeus treats the macaroon as an opaque hex credential in the
//! `Grpc-Metadata-macaroon` header. We bake credentials with HMAC-SHA256
//! chained caveats (entity/action permissions) and verify them fail-closed.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use hmac::{Hmac, Mac};
use rand::RngCore;
use sha2::Sha256;
use thiserror::Error;

type HmacSha256 = Hmac<Sha256>;

const ROOT_KEY_LEN: usize = 32;
const MAX_MACAROON_BYTES: usize = 16 * 1024;

/// LND-style entity/action permission.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Permission {
    pub entity: &'static str,
    pub action: &'static str,
}

impl Permission {
    pub const fn new(entity: &'static str, action: &'static str) -> Self {
        Self { entity, action }
    }

    pub fn caveat(self) -> String {
        format!("{}:{}", self.entity, self.action)
    }

    pub fn parse(raw: &str) -> Option<(String, String)> {
        let (entity, action) = raw.split_once(':')?;
        if entity.is_empty() || action.is_empty() {
            return None;
        }
        Some((entity.to_string(), action.to_string()))
    }
}

pub const INFO_READ: Permission = Permission::new("info", "read");
pub const OFFCHAIN_READ: Permission = Permission::new("offchain", "read");
pub const OFFCHAIN_WRITE: Permission = Permission::new("offchain", "write");
pub const ONCHAIN_READ: Permission = Permission::new("onchain", "read");
pub const ONCHAIN_WRITE: Permission = Permission::new("onchain", "write");
pub const ADDRESS_READ: Permission = Permission::new("address", "read");
pub const ADDRESS_WRITE: Permission = Permission::new("address", "write");
pub const PEERS_READ: Permission = Permission::new("peers", "read");
pub const PEERS_WRITE: Permission = Permission::new("peers", "write");
pub const INVOICES_READ: Permission = Permission::new("invoices", "read");
pub const INVOICES_WRITE: Permission = Permission::new("invoices", "write");

/// Full admin permission set (LND admin.macaroon equivalent for our subset).
pub const ADMIN_PERMS: &[Permission] = &[
    INFO_READ,
    OFFCHAIN_READ,
    OFFCHAIN_WRITE,
    ONCHAIN_READ,
    ONCHAIN_WRITE,
    ADDRESS_READ,
    ADDRESS_WRITE,
    PEERS_READ,
    PEERS_WRITE,
    INVOICES_READ,
    INVOICES_WRITE,
];

pub const READONLY_PERMS: &[Permission] = &[
    INFO_READ,
    OFFCHAIN_READ,
    ONCHAIN_READ,
    ADDRESS_READ,
    PEERS_READ,
    INVOICES_READ,
];

pub const INVOICE_PERMS: &[Permission] = &[INVOICES_READ, INVOICES_WRITE, INFO_READ];

#[derive(Debug, Error)]
pub enum AuthError {
    #[error("missing macaroon")]
    Missing,
    #[error("malformed macaroon")]
    Malformed,
    #[error("macaroon too large")]
    TooLarge,
    #[error("invalid macaroon signature")]
    InvalidSignature,
    #[error("permission denied")]
    PermissionDenied,
    #[error("unknown caveat: {0}")]
    UnknownCaveat(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

#[derive(Clone)]
pub struct MacaroonBakery {
    root_key: [u8; ROOT_KEY_LEN],
    macaroon_dir: PathBuf,
}

impl MacaroonBakery {
    /// Load or create the bakery under `macaroon_dir`.
    ///
    /// Existing root keys are never silently rotated. File modes are forced to
    /// owner-only when we create them.
    pub fn load_or_create(macaroon_dir: impl AsRef<Path>) -> Result<Self, AuthError> {
        let macaroon_dir = macaroon_dir.as_ref().to_path_buf();
        ensure_secure_dir(&macaroon_dir)?;

        let root_path = macaroon_dir.join("macaroon_root_key");
        let root_key = if root_path.exists() {
            load_root_key(&root_path)?
        } else {
            let mut key = [0u8; ROOT_KEY_LEN];
            rand::thread_rng().fill_bytes(&mut key);
            write_secret_file(&root_path, &key)?;
            key
        };

        let bakery = Self {
            root_key,
            macaroon_dir,
        };
        bakery.ensure_default_macaroons()?;
        Ok(bakery)
    }

    pub fn macaroon_dir(&self) -> &Path {
        &self.macaroon_dir
    }

    pub fn admin_macaroon_path(&self) -> PathBuf {
        self.macaroon_dir.join("admin.macaroon")
    }

    fn ensure_default_macaroons(&self) -> Result<(), AuthError> {
        self.write_macaroon_if_missing("admin.macaroon", ADMIN_PERMS)?;
        self.write_macaroon_if_missing("readonly.macaroon", READONLY_PERMS)?;
        self.write_macaroon_if_missing("invoice.macaroon", INVOICE_PERMS)?;
        Ok(())
    }

    fn write_macaroon_if_missing(&self, name: &str, perms: &[Permission]) -> Result<(), AuthError> {
        let path = self.macaroon_dir.join(name);
        if path.exists() {
            return Ok(());
        }
        let bytes = self.bake(name, perms)?;
        write_secret_file(&path, &bytes)?;
        Ok(())
    }

    pub fn bake(&self, identifier: &str, perms: &[Permission]) -> Result<Vec<u8>, AuthError> {
        let mut caveats: Vec<String> = perms.iter().map(|p| p.caveat()).collect();
        caveats.sort();
        caveats.dedup();

        let mut payload = Vec::new();
        payload.extend_from_slice(b"lampo-macaroon-v1\0");
        payload.extend_from_slice(identifier.as_bytes());
        payload.push(0);
        for caveat in &caveats {
            payload.extend_from_slice(caveat.as_bytes());
            payload.push(0);
        }

        let mut mac = HmacSha256::new_from_slice(&self.root_key)
            .map_err(|e| AuthError::Other(e.to_string()))?;
        mac.update(&payload);
        let sig = mac.finalize().into_bytes();

        payload.extend_from_slice(&sig);
        Ok(payload)
    }

    pub fn verify_hex(&self, hex_macaroon: &str, required: Permission) -> Result<(), AuthError> {
        let granted = self.permissions_from_hex(hex_macaroon)?;
        if granted.contains(&(required.entity.to_string(), required.action.to_string())) {
            Ok(())
        } else {
            Err(AuthError::PermissionDenied)
        }
    }

    /// Verify the macaroon's signature and caveat encoding without requiring
    /// a particular permission. Route handlers perform the authorization
    /// decision after middleware has rejected malformed credentials.
    pub fn verify_hex_signature(&self, hex_macaroon: &str) -> Result<(), AuthError> {
        self.permissions_from_hex(hex_macaroon).map(|_| ())
    }

    fn permissions_from_hex(
        &self,
        hex_macaroon: &str,
    ) -> Result<HashSet<(String, String)>, AuthError> {
        if hex_macaroon.len() > MAX_MACAROON_BYTES * 2 {
            return Err(AuthError::TooLarge);
        }
        let bytes = hex::decode(hex_macaroon.trim()).map_err(|_| AuthError::Malformed)?;
        self.permissions(&bytes)
    }

    pub fn verify(&self, macaroon: &[u8], required: Permission) -> Result<(), AuthError> {
        let granted = self.permissions(macaroon)?;
        if granted.contains(&(required.entity.to_string(), required.action.to_string())) {
            Ok(())
        } else {
            Err(AuthError::PermissionDenied)
        }
    }

    fn permissions(&self, macaroon: &[u8]) -> Result<HashSet<(String, String)>, AuthError> {
        if macaroon.len() > MAX_MACAROON_BYTES {
            return Err(AuthError::TooLarge);
        }
        if macaroon.len() < 32 + 18 {
            return Err(AuthError::Malformed);
        }

        let (payload, sig) = macaroon.split_at(macaroon.len() - 32);
        let mut mac = HmacSha256::new_from_slice(&self.root_key)
            .map_err(|e| AuthError::Other(e.to_string()))?;
        mac.update(payload);
        mac.verify_slice(sig)
            .map_err(|_| AuthError::InvalidSignature)?;

        if !payload.starts_with(b"lampo-macaroon-v1\0") {
            return Err(AuthError::Malformed);
        }
        let rest = &payload["lampo-macaroon-v1\0".len()..];
        let parts: Vec<&[u8]> = rest.split(|b| *b == 0).filter(|p| !p.is_empty()).collect();
        if parts.is_empty() {
            return Err(AuthError::Malformed);
        }

        // parts[0] = identifier; remaining = caveats
        let mut granted: HashSet<(String, String)> = HashSet::new();
        for raw in &parts[1..] {
            let text = std::str::from_utf8(raw).map_err(|_| AuthError::Malformed)?;
            let Some((entity, action)) = Permission::parse(text) else {
                return Err(AuthError::UnknownCaveat(text.to_string()));
            };
            granted.insert((entity, action));
        }

        Ok(granted)
    }

    pub fn admin_macaroon_hex(&self) -> Result<String, AuthError> {
        let bytes = fs::read(self.admin_macaroon_path())?;
        Ok(hex::encode(bytes))
    }
}

fn ensure_secure_dir(path: &Path) -> Result<(), AuthError> {
    if path.exists() {
        let meta = fs::metadata(path)?;
        if !meta.is_dir() {
            return Err(AuthError::Other(format!(
                "macaroon path is not a directory: {}",
                path.display()
            )));
        }
    } else {
        fs::create_dir_all(path)?;
    }
    let mut perms = fs::metadata(path)?.permissions();
    perms.set_mode(0o700);
    fs::set_permissions(path, perms)?;
    Ok(())
}

fn load_root_key(path: &Path) -> Result<[u8; ROOT_KEY_LEN], AuthError> {
    let meta = fs::symlink_metadata(path)?;
    if meta.file_type().is_symlink() {
        return Err(AuthError::Other(
            "macaroon root key must not be a symlink".into(),
        ));
    }
    let bytes = fs::read(path)?;
    if bytes.len() != ROOT_KEY_LEN {
        return Err(AuthError::Other(format!(
            "macaroon root key must be {ROOT_KEY_LEN} bytes"
        )));
    }
    let mut key = [0u8; ROOT_KEY_LEN];
    key.copy_from_slice(&bytes);
    Ok(key)
}

fn write_secret_file(path: &Path, bytes: &[u8]) -> Result<(), AuthError> {
    if path.exists() {
        return Err(AuthError::Other(format!(
            "refusing to overwrite existing secret {}",
            path.display()
        )));
    }
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn bake_and_verify_admin() {
        let dir = tempdir().unwrap();
        let bakery = MacaroonBakery::load_or_create(dir.path()).unwrap();
        let hex = bakery.admin_macaroon_hex().unwrap();
        bakery.verify_hex(&hex, INFO_READ).unwrap();
        bakery.verify_hex(&hex, OFFCHAIN_WRITE).unwrap();
    }

    #[test]
    fn readonly_cannot_pay() {
        let dir = tempdir().unwrap();
        let bakery = MacaroonBakery::load_or_create(dir.path()).unwrap();
        let bytes = std::fs::read(dir.path().join("readonly.macaroon")).unwrap();
        let hex = hex::encode(bytes);
        bakery.verify_hex(&hex, INFO_READ).unwrap();
        assert!(matches!(
            bakery.verify_hex(&hex, OFFCHAIN_WRITE),
            Err(AuthError::PermissionDenied)
        ));
    }

    #[test]
    fn signature_check_accepts_least_privilege_macaroon() {
        let dir = tempdir().unwrap();
        let bakery = MacaroonBakery::load_or_create(dir.path()).unwrap();
        let bytes = bakery.bake("payments-only", &[OFFCHAIN_WRITE]).unwrap();
        bakery.verify_hex_signature(&hex::encode(bytes)).unwrap();
    }

    #[test]
    fn tampered_macaroon_rejected() {
        let dir = tempdir().unwrap();
        let bakery = MacaroonBakery::load_or_create(dir.path()).unwrap();
        let mut hex = bakery.admin_macaroon_hex().unwrap();
        // Flip last nibble.
        let last = hex.pop().unwrap();
        hex.push(if last == '0' { '1' } else { '0' });
        assert!(matches!(
            bakery.verify_hex(&hex, INFO_READ),
            Err(AuthError::InvalidSignature)
        ));
    }

    #[test]
    fn restart_reuses_root_key() {
        let dir = tempdir().unwrap();
        let first = MacaroonBakery::load_or_create(dir.path()).unwrap();
        let hex = first.admin_macaroon_hex().unwrap();
        let second = MacaroonBakery::load_or_create(dir.path()).unwrap();
        second.verify_hex(&hex, INFO_READ).unwrap();
    }
}
