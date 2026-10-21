//! The Studio's bundle mode: the same verifying reader as `wyrd verify`,
//! then the views the Session screen needs (events, checkpoint diffs, and
//! file contents from `cas/`). Plain Rust, so it is tested natively; the
//! `#[wasm_bindgen]` wrapper is in `lib.rs`.

use std::collections::BTreeSet;

use serde_json::{Value, json};
use wyrd_bundle::read::{Bundle, Options, Verification, verify};
use wyrd_cas::{ChunkHash, EntryKind, Manifest};

/// Largest side of a file diff returned (as the daemon's).
pub const FILE_LIMIT: usize = 1 << 20;

/// The verification report, as `wyrd verify <file.wyrd> --json` prints it
/// (without the path).
pub fn report(v: &Verification) -> Value {
    let (bundle, error) = match &v.result {
        Ok(b) => (
            Some(json!({
                "session": b.manifest.session,
                "redaction": b.manifest.redaction,
                "content": b.manifest.content,
                "events": b.manifest.ledger.events,
                "head_hash": b.manifest.ledger.head_hash,
                "manifest_hash": b.manifest_hash,
                "signer_fingerprint": b.fingerprint(),
            })),
            None,
        ),
        Err(e) => (
            None,
            Some(json!({
                "stage": e.stage(),
                "seq": e.seq(),
                "message": e.to_string(),
            })),
        ),
    };
    json!({
        "ok": v.ok(),
        "checks": v.checks,
        "bundle": bundle,
        "error": error,
    })
}

/// Verify `bytes`; the report, and the bundle when every check passed.
pub fn open(bytes: &[u8]) -> (Value, Option<Opened>) {
    let v = verify(bytes, &Options::default());
    let report = report(&v);
    (report, v.result.ok().map(|bundle| Opened { bundle }))
}

/// A verified bundle.
#[derive(Debug)]
pub struct Opened {
    bundle: Bundle,
}

impl Opened {
    /// The events as a JSON array (the format of `wyrd events --json`).
    pub fn events_json(&self) -> Result<String, String> {
        serde_json::to_string(&self.bundle.events).map_err(|e| e.to_string())
    }

    fn manifest(&self, hash: &str) -> Result<Manifest, String> {
        let h = ChunkHash::from_hex(hash).ok_or_else(|| format!("bad manifest hash {hash:?}"))?;
        let bytes = self.bundle.chunks.get(h.as_bytes()).ok_or_else(|| {
            "this bundle has no checkpoint content (exported with --no-content)".to_string()
        })?;
        Manifest::from_cbor(bytes).map_err(|e| e.to_string())
    }

    /// Changes between two checkpoint manifests, as `GET …/diff/{a}/{b}`.
    pub fn diff_json(&self, a: &str, b: &str) -> Result<String, String> {
        let changes = changes(&self.manifest(a)?, &self.manifest(b)?);
        Ok(json!({ "changes": changes }).to_string())
    }

    /// Both sides of one path, as `GET …/diff/{a}/{b}/file?path=`.
    pub fn file_json(&self, a: &str, b: &str, path: &str) -> Result<String, String> {
        let before = self.side(&self.manifest(a)?, path)?;
        let after = self.side(&self.manifest(b)?, path)?;
        Ok(json!({ "path": path, "before": before, "after": after }).to_string())
    }

    /// A path in a checkpoint's upper layer. Paths the agent did not change
    /// live in the recording machine's workspace, which a bundle does not
    /// carry: they are `workspace` without text.
    fn side(&self, m: &Manifest, path: &str) -> Result<Value, String> {
        if path.is_empty()
            || path.starts_with('/')
            || path
                .split('/')
                .any(|c| c.is_empty() || c == "." || c == "..")
        {
            return Err(format!("bad path {path:?}"));
        }
        let absent =
            json!({ "source": "absent", "text": null, "binary": false, "truncated": false });
        let mut hidden = false;
        let mut prefix = String::new();
        for comp in path.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(comp);
            let last = prefix.len() == path.len();
            let Some(e) = m.get(prefix.as_bytes()) else {
                continue;
            };
            match e.kind {
                EntryKind::Whiteout => return Ok(absent),
                EntryKind::File if last => {
                    let mut bytes = Vec::new();
                    for c in &e.chunks {
                        let Some(chunk) = self.bundle.chunks.get(c.as_bytes()) else {
                            // Withheld (secret-classified path, strict redaction).
                            return Ok(json!({
                                "source": "checkpoint", "text": null, "binary": false,
                                "truncated": false, "withheld": true,
                            }));
                        };
                        bytes.extend_from_slice(chunk);
                        if bytes.len() > FILE_LIMIT {
                            break;
                        }
                    }
                    return Ok(text_side("checkpoint", bytes));
                }
                EntryKind::Symlink if last => {
                    let t = e.target.as_ref().map(|t| t.to_vec()).unwrap_or_default();
                    return Ok(text_side("checkpoint", t));
                }
                EntryKind::Dir if last => return Ok(absent),
                EntryKind::Dir if e.opaque => hidden = true,
                _ => {}
            }
        }
        if hidden {
            return Ok(absent);
        }
        Ok(json!({ "source": "workspace", "text": null, "binary": false, "truncated": false }))
    }
}

fn text_side(source: &str, mut bytes: Vec<u8>) -> Value {
    let truncated = bytes.len() > FILE_LIMIT;
    bytes.truncate(FILE_LIMIT);
    let (text, binary) = if bytes.contains(&0) {
        (None, true)
    } else {
        match String::from_utf8(bytes) {
            Ok(t) => (Some(t), false),
            // A cut through a multi-byte character is still text.
            Err(e) if truncated && e.utf8_error().error_len().is_none() => {
                let valid = e.utf8_error().valid_up_to();
                let mut b = e.into_bytes();
                b.truncate(valid);
                (String::from_utf8(b).ok(), false)
            }
            Err(_) => (None, true),
        }
    };
    json!({ "source": source, "text": text, "binary": binary, "truncated": truncated })
}

fn kind_name(k: EntryKind) -> &'static str {
    match k {
        EntryKind::File => "file",
        EntryKind::Dir => "dir",
        EntryKind::Symlink => "symlink",
        EntryKind::Whiteout => "deleted",
    }
}

/// Changes from `a` to `b`, with the semantics of `wyrd_cell::diff::diff`
/// (which needs the on-disk store and does not build for WebAssembly; a
/// native test keeps both equal).
pub fn changes(a: &Manifest, b: &Manifest) -> Vec<Value> {
    let paths: BTreeSet<&[u8]> = a
        .entries
        .iter()
        .chain(&b.entries)
        .map(|e| e.path.as_slice())
        .collect();
    let mut out = Vec::new();
    for p in paths {
        let path = String::from_utf8_lossy(p);
        let change = |kind: &str, entry: EntryKind| json!({ "path": path, "kind": kind, "entry": kind_name(entry) });
        match (a.get(p), b.get(p)) {
            (None, Some(n)) => out.push(change("added", n.kind)),
            (Some(o), None) => out.push(change("removed", o.kind)),
            (Some(o), Some(n)) => {
                let same = o.kind == n.kind
                    && o.mode == n.mode
                    && o.chunks == n.chunks
                    && o.target == n.target
                    && o.opaque == n.opaque
                    && (o.kind == EntryKind::Dir || o.size == n.size);
                if !same {
                    out.push(change("modified", n.kind));
                }
            }
            (None, None) => {}
        }
    }
    out
}
