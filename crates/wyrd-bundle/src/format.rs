//! Bundle format version 1: names, the manifest, and the signed message.
//! The normative description is `docs/bundle-format.md`.

use serde::{Deserialize, Serialize};
use wyrd_schema::{CaptureMode, SessionState};

/// Value of [`Manifest::format`].
pub const FORMAT: &str = "wyrdline.bundle";
/// Value of [`Manifest::format_version`]; readers refuse any other.
pub const FORMAT_VERSION: u32 = 1;

pub const MANIFEST: &str = "manifest.json";
pub const SIGNATURE: &str = "signature.ed25519";
pub const LEDGER: &str = "ledger.cbor-seq";
pub const CLAIMS: &str = "claims.json";
pub const DIVERGENCES: &str = "divergences.json";
pub const ANNOTATIONS: &str = "annotations.automerge";
/// Directory of CAS chunks, each named by the hex BLAKE3 of its bytes.
pub const CAS_DIR: &str = "cas/";

/// Prefix of the signed message; the host key also signs ledger seals,
/// so every signature is domain-separated.
pub const SIGNATURE_DOMAIN: &[u8] = b"wyrdline.bundle.v1/manifest";

/// `signature.ed25519`: the 32-byte public key, then the 64-byte signature.
pub const SIGNATURE_LEN: usize = 96;

/// `manifest.json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: String,
    pub format_version: u32,
    /// Event schema of the ledger records, e.g. `wyrdline.event.v1`.
    pub event_schema: String,
    pub session: SessionMeta,
    pub redaction: Redaction,
    pub content: Content,
    pub ledger: LedgerSummary,
    pub signer: SignerInfo,
    /// Every entry except `manifest.json` and `signature.ed25519`, sorted by path.
    pub files: Vec<FileEntry>,
}

/// What the receiver learns about the session without reading events.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SessionMeta {
    pub id: String,
    pub name: Option<String>,
    pub agent: String,
    pub mode: CaptureMode,
    pub status: SessionState,
    pub created_ms: i64,
}

/// Redaction applied to every string of every event at export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Redaction {
    Standard,
    Strict,
}

impl Redaction {
    pub fn level(self) -> crate::redact::Level {
        match self {
            Redaction::Standard => crate::redact::Level::Standard,
            Redaction::Strict => crate::redact::Level::Strict,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Redaction::Standard => "standard",
            Redaction::Strict => "strict",
        }
    }
}

/// Checkpoint content (`cas/`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Content {
    /// False for `--no-content`: no `cas/` entries at all.
    pub included: bool,
    pub chunks: u64,
    /// Files of the checkpoints whose content was left out because their
    /// path is secret-classified (strict redaction).
    pub withheld_files: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LedgerSummary {
    pub events: u64,
    /// Chain head over the bundled (redacted) events, `b3:` + hex.
    pub head_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignerInfo {
    /// Ed25519 public key, hex.
    pub public_key: String,
    /// First 8 bytes of BLAKE3 of the public key, hex (as for ledger seals).
    pub fingerprint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileEntry {
    pub path: String,
    pub size: u64,
    /// BLAKE3 of the entry's bytes, `b3:` + hex.
    pub blake3: String,
}

/// The message signed by `signature.ed25519`.
pub fn signed_message(manifest_json: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(SIGNATURE_DOMAIN.len() + manifest_json.len());
    m.extend_from_slice(SIGNATURE_DOMAIN);
    m.extend_from_slice(manifest_json);
    m
}

/// Signer fingerprint: first 8 bytes of BLAKE3 of the public key, hex.
pub fn fingerprint(public_key: &[u8; 32]) -> String {
    hex::encode(&blake3::hash(public_key).as_bytes()[..8])
}

/// `b3:` + hex of BLAKE3 of `bytes`.
pub fn b3(bytes: &[u8]) -> String {
    format!("b3:{}", blake3::hash(bytes).to_hex())
}

/// Why an entry path is not allowed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathProblem {
    /// Starts with `/`.
    Absolute,
    /// Has a `..` component.
    Traversal,
    /// Empty, too long, `.`/empty components, `\`, NUL, or not part of the
    /// v1 layout.
    Invalid,
}

/// Check that `path` is a safe, relative path of the v1 layout: one of the
/// fixed names, or `cas/` + 64 lowercase hex digits.
pub fn check_path(path: &str) -> Result<(), PathProblem> {
    if path.starts_with('/') {
        return Err(PathProblem::Absolute);
    }
    if path.split('/').any(|c| c == "..") {
        return Err(PathProblem::Traversal);
    }
    if path.is_empty()
        || path.len() > crate::archive::MAX_NAME
        || path.contains(['\\', '\0'])
        || path.split('/').any(|c| c.is_empty() || c == ".")
    {
        return Err(PathProblem::Invalid);
    }
    let fixed = [
        MANIFEST,
        SIGNATURE,
        LEDGER,
        CLAIMS,
        DIVERGENCES,
        ANNOTATIONS,
    ];
    if fixed.contains(&path) || path.strip_prefix(CAS_DIR).is_some_and(is_chunk_name) {
        Ok(())
    } else {
        Err(PathProblem::Invalid)
    }
}

/// 64 lowercase hex digits.
pub fn is_chunk_name(name: &str) -> bool {
    name.len() == 64 && name.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_of_the_layout_are_accepted_and_others_classified() {
        let chunk = format!("cas/{}", "ab".repeat(32));
        for ok in [MANIFEST, LEDGER, CLAIMS, chunk.as_str()] {
            assert_eq!(check_path(ok), Ok(()), "{ok}");
        }
        for (p, why) in [
            ("/etc/passwd", PathProblem::Absolute),
            ("../x", PathProblem::Traversal),
            ("cas/../../x", PathProblem::Traversal),
            ("./manifest.json", PathProblem::Invalid),
            ("cas//x", PathProblem::Invalid),
            ("cas/ABCD", PathProblem::Invalid),
            ("notes.txt", PathProblem::Invalid),
            ("cas\\x", PathProblem::Invalid),
            ("", PathProblem::Invalid),
        ] {
            assert_eq!(check_path(p), Err(why), "{p}");
        }
    }
}
