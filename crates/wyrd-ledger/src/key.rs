//! Host signing key.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::Path;

use ed25519_dalek::{Signature, Signer as _, SigningKey, VerifyingKey};

use crate::{Hash, LedgerError, Result};

const SEAL_DOMAIN: &[u8] = b"wyrdline.ledger.v1/seal";

/// Message signed by a seal.
pub fn seal_message(session_id: &str, head_seq: u64, head_hash: &Hash) -> Vec<u8> {
    let mut m = Vec::with_capacity(SEAL_DOMAIN.len() + session_id.len() + 48);
    m.extend_from_slice(SEAL_DOMAIN);
    m.extend_from_slice(&(session_id.len() as u64).to_le_bytes());
    m.extend_from_slice(session_id.as_bytes());
    m.extend_from_slice(&head_seq.to_le_bytes());
    m.extend_from_slice(head_hash);
    m
}

/// The host's ed25519 key, stored as a 32-byte seed with mode 0600.
#[derive(Debug)]
pub struct HostKey {
    key: SigningKey,
}

impl HostKey {
    /// Load the key at `path`, creating it (and its directory, mode 0700) if missing.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        if path.exists() {
            return Self::load(path);
        }
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
            fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
        }
        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).map_err(|e| LedgerError::Key(e.to_string()))?;
        let mut f = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(&seed)?;
        f.sync_all()?;
        Ok(HostKey {
            key: SigningKey::from_bytes(&seed),
        })
    }

    /// Load an existing key; refuses keys readable by group or others.
    pub fn load(path: &Path) -> Result<Self> {
        let mode = fs::metadata(path)?.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(LedgerError::Key(format!(
                "{} has mode {:o}; it must not be accessible by group or others",
                path.display(),
                mode & 0o777
            )));
        }
        let bytes = fs::read(path)?;
        let seed: [u8; 32] = bytes
            .try_into()
            .map_err(|_| LedgerError::Key("key file must be 32 bytes".into()))?;
        Ok(HostKey {
            key: SigningKey::from_bytes(&seed),
        })
    }

    /// Deterministic key for tests.
    pub fn from_seed(seed: [u8; 32]) -> Self {
        HostKey {
            key: SigningKey::from_bytes(&seed),
        }
    }

    pub fn public_key(&self) -> [u8; 32] {
        self.key.verifying_key().to_bytes()
    }

    /// Hex fingerprint (first 16 hex chars of BLAKE3 of the public key).
    pub fn fingerprint(&self) -> String {
        fingerprint(&self.public_key())
    }

    pub fn sign(&self, msg: &[u8]) -> [u8; 64] {
        self.key.sign(msg).to_bytes()
    }
}

pub(crate) fn fingerprint(public_key: &[u8; 32]) -> String {
    hex::encode(&blake3::hash(public_key).as_bytes()[..8])
}

pub(crate) fn verify_signature(public_key: &[u8; 32], msg: &[u8], sig: &[u8; 64]) -> bool {
    let Ok(vk) = VerifyingKey::from_bytes(public_key) else {
        return false;
    };
    vk.verify_strict(msg, &Signature::from_bytes(sig)).is_ok()
}
