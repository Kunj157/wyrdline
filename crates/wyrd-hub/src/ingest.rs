//! Bundle ingest: spool the upload to a local file while counting bytes,
//! verify it with `wyrd-bundle`'s reader (signature, hashes, chain), and
//! summarise what the Hub stores about it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use axum::http::StatusCode;
use serde_json::json;
use wyrd_bundle::Bundle;
use wyrd_bundle::read::{CheckStatus, Limits, Options, Verification};
use wyrd_schema::EventBody;

use crate::Problem;

/// A spooled upload, removed when dropped.
#[derive(Debug)]
pub struct Spool {
    pub path: PathBuf,
    pub file: tokio::fs::File,
}

impl Spool {
    pub async fn create(data_dir: &Path) -> Result<Spool, Problem> {
        let dir = data_dir.join("uploads");
        tokio::fs::create_dir_all(&dir)
            .await
            .map_err(|e| Problem::internal(&e))?;
        let path = dir.join(format!("{}.part", ulid::Ulid::generate()));
        let file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .await
            .map_err(|e| Problem::internal(&e))?;
        Ok(Spool { path, file })
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Reader limits for uploads: the reader's defaults, with the total
/// decompressed size bounded relative to the upload limit.
pub fn options(max_upload_bytes: u64) -> Options {
    let limits = Limits {
        max_total_bytes: max_upload_bytes.saturating_mul(16).clamp(64 << 20, 4 << 30),
        ..Limits::default()
    };
    Options {
        limits,
        signer: None,
    }
}

/// 422 naming the failed check, with the whole check list.
pub fn rejection(v: &Verification) -> Problem {
    let (stage, detail) = match &v.result {
        Err(e) => (e.stage().describe(), e.to_string()),
        Ok(_) => ("none", String::new()),
    };
    let checks: Vec<serde_json::Value> = v
        .checks
        .iter()
        .map(|c| {
            json!({
                "check": c.stage.describe(),
                "status": match c.status {
                    CheckStatus::Passed => "passed",
                    CheckStatus::Failed => "failed",
                    CheckStatus::Skipped => "skipped",
                },
            })
        })
        .collect();
    Problem::new(
        StatusCode::UNPROCESSABLE_ENTITY,
        "bundle-rejected",
        "The bundle failed verification",
    )
    .detail(format!("{stage}: {detail}"))
    .with("failed_check", json!(stage))
    .with("checks", json!(checks))
}

/// Verify the spooled file (blocking work on the blocking pool).
pub async fn verify(path: PathBuf, max_upload_bytes: u64) -> Result<Bundle, Problem> {
    let v = tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(&path)?;
        Ok::<_, std::io::Error>(wyrd_bundle::verify(
            std::io::BufReader::new(file),
            &options(max_upload_bytes),
        ))
    })
    .await
    .map_err(|e| Problem::internal(&e))?
    .map_err(|e| Problem::internal(&e))?;
    let rejected = rejection(&v);
    v.result.map_err(|_| rejected)
}

/// What the Hub records about a verified bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    pub manifest_hash: String,
    pub signer: String,
    pub session_id: String,
    pub session_name: Option<String>,
    pub agent: String,
    pub session_created_ms: i64,
    pub events: i64,
    /// Divergence rule ids raised in the session, sorted, unique.
    pub rules: Vec<String>,
}

pub fn summarize(b: &Bundle) -> Summary {
    let rules: BTreeSet<String> = b
        .events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::DivergenceRaised(d) => Some(d.rule_id.clone()),
            _ => None,
        })
        .collect();
    Summary {
        manifest_hash: b.manifest_hash.clone(),
        signer: b.fingerprint(),
        session_id: b.manifest.session.id.clone(),
        session_name: b.manifest.session.name.clone(),
        agent: b.manifest.session.agent.clone(),
        session_created_ms: b.manifest.session.created_ms,
        events: i64::try_from(b.events.len()).unwrap_or(i64::MAX),
        rules: rules.into_iter().collect(),
    }
}

/// A repository label given by the uploader: short, printable.
pub fn valid_repo(repo: &str) -> bool {
    !repo.is_empty()
        && repo.len() <= 200
        && repo
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:@".contains(c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn repos() {
        for ok in ["shop", "Kunj157/wyrdline", "github.com/a/b", "a_b-c.d"] {
            assert!(valid_repo(ok), "{ok}");
        }
        for bad in ["", "a b", "a\nb", "<script>", &"x".repeat(201)] {
            assert!(!valid_repo(bad), "{bad:?}");
        }
    }

    #[test]
    fn reader_limits_follow_the_upload_limit() {
        assert_eq!(options(1 << 20).limits.max_total_bytes, 64 << 20);
        assert_eq!(options(256 << 20).limits.max_total_bytes, 4 << 30);
        assert_eq!(options(u64::MAX).limits.max_total_bytes, 4 << 30);
    }
}
