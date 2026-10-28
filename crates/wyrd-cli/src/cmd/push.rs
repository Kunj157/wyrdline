//! `wyrd push`: upload a bundle to a Wyrdline Hub.

use std::fmt::Write as _;
use std::io::{Cursor, Read, Write as _};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;

use crate::exit;

#[derive(Debug, clap::Args)]
pub struct PushArgs {
    /// The `.wyrd` bundle to upload.
    bundle: PathBuf,
    /// Hub URL (default `$WYRD_HUB_URL`). The API token is read from
    /// `$WYRD_HUB_TOKEN`, never from the command line.
    #[arg(long)]
    hub: Option<String>,
    /// Organisation to upload into (its slug).
    #[arg(long)]
    org: String,
    /// Repository label stored with the bundle, e.g. `owner/repo`.
    #[arg(long)]
    repo: Option<String>,
    /// Print the Hub's answer as JSON.
    #[arg(long)]
    json: bool,
}

/// A multipart body streamed from the file: form fields, then the file.
fn body(
    boundary: &str,
    repo: Option<&str>,
    file: std::fs::File,
    name: &str,
) -> (u64, impl Read + Send + 'static) {
    let mut head = String::new();
    if let Some(r) = repo {
        let _ = write!(
            head,
            "--{boundary}\r\nContent-Disposition: form-data; name=\"repo\"\r\n\r\n{r}\r\n"
        );
    }
    let _ = write!(
        head,
        "--{boundary}\r\nContent-Disposition: form-data; name=\"bundle\"; filename=\"{name}\"\r\n\
         Content-Type: application/octet-stream\r\n\r\n"
    );
    let tail = format!("\r\n--{boundary}--\r\n");
    let file_len = file.metadata().map_or(0, |m| m.len());
    let len = head.len() as u64 + file_len + tail.len() as u64;
    let reader = Cursor::new(head.into_bytes())
        .chain(file)
        .chain(Cursor::new(tail.into_bytes()));
    (len, reader)
}

pub fn push(a: &PushArgs) -> anyhow::Result<u8> {
    let Some(hub) = a.hub.clone().or_else(|| std::env::var("WYRD_HUB_URL").ok()) else {
        eprintln!("wyrd push: no Hub URL (--hub or WYRD_HUB_URL)");
        return Ok(exit::USAGE);
    };
    let Ok(token) = std::env::var("WYRD_HUB_TOKEN") else {
        eprintln!("wyrd push: set WYRD_HUB_TOKEN to an API token of the Hub");
        return Ok(exit::USAGE);
    };
    // Check locally first: a bundle that does not verify here will not
    // verify on the Hub either, and this names the failing check.
    let file = std::fs::File::open(&a.bundle)
        .with_context(|| format!("opening {}", a.bundle.display()))?;
    let local = wyrd_bundle::verify(
        std::io::BufReader::new(file),
        &wyrd_bundle::Options::default(),
    );
    if let Err(e) = &local.result {
        eprintln!(
            "wyrd push: {} does not verify ({}): {e}",
            a.bundle.display(),
            e.stage().describe()
        );
        return Ok(exit::VERIFICATION_FAILED);
    }
    let file = std::fs::File::open(&a.bundle)?;
    let name = a.bundle.file_name().map_or_else(
        || "bundle.wyrd".into(),
        |n| n.to_string_lossy().replace('"', "_"),
    );
    let boundary = format!("wyrd-{}", hex::encode(rand_bytes()));
    let (len, reader) = body(&boundary, a.repo.as_deref(), file, &name);
    let url = format!(
        "{}/api/v1/orgs/{}/bundles",
        hub.trim_end_matches('/'),
        a.org
    );
    let agent = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .timeout_connect(Some(Duration::from_secs(10)))
        .build()
        .new_agent();
    let res = agent
        .post(&url)
        .header("authorization", &format!("Bearer {token}"))
        .header(
            "content-type",
            &format!("multipart/form-data; boundary={boundary}"),
        )
        .header("content-length", &len.to_string())
        .send(ureq::SendBody::from_owned_reader(reader));
    let mut res = match res {
        Ok(r) => r,
        Err(e) => {
            eprintln!("wyrd push: the Hub at {hub} is unreachable: {e}");
            return Ok(exit::DAEMON_UNAVAILABLE);
        }
    };
    let status = res.status().as_u16();
    let text = res
        .body_mut()
        .with_config()
        .limit(1 << 20)
        .read_to_string()
        .unwrap_or_default();
    let answer: serde_json::Value = serde_json::from_str(&text).unwrap_or(serde_json::Value::Null);
    report(a, status, &answer)
}

/// Print the Hub's answer; the exit code it maps to.
fn report(a: &PushArgs, status: u16, answer: &serde_json::Value) -> anyhow::Result<u8> {
    if a.json {
        writeln!(
            std::io::stdout().lock(),
            "{}",
            serde_json::to_string_pretty(answer)?
        )?;
    }
    match status {
        200 | 201 => {
            if !a.json {
                let id = answer["bundle"]["id"].as_str().unwrap_or("?");
                let what = if status == 200 {
                    "already on the Hub as"
                } else {
                    "uploaded as"
                };
                writeln!(
                    std::io::stdout().lock(),
                    "{} {what} {id}",
                    a.bundle.display()
                )?;
            }
            Ok(exit::OK)
        }
        _ => {
            let title = answer["title"].as_str().unwrap_or("request failed");
            let detail = answer["detail"]
                .as_str()
                .map(|d| format!(": {d}"))
                .unwrap_or_default();
            eprintln!("wyrd push: {status} {title}{detail}");
            Ok(match status {
                422 => exit::VERIFICATION_FAILED,
                413 | 429 | 409 => exit::CONFLICT,
                500.. => exit::DAEMON_UNAVAILABLE,
                _ => exit::USAGE,
            })
        }
    }
}

/// 8 bytes for the multipart boundary (uniqueness, not secrecy).
fn rand_bytes() -> [u8; 8] {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos());
    let mixed = nanos ^ u128::from(std::process::id()).rotate_left(64);
    let mut out = [0u8; 8];
    out.copy_from_slice(&mixed.to_le_bytes()[..8]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_length_matches_the_stream() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("s.wyrd");
        std::fs::write(&p, b"0123456789").unwrap();
        let (len, mut r) = body("b", Some("o/r"), std::fs::File::open(&p).unwrap(), "s.wyrd");
        let mut all = Vec::new();
        r.read_to_end(&mut all).unwrap();
        assert_eq!(all.len() as u64, len);
        let text = String::from_utf8(all).unwrap();
        assert!(text.contains("name=\"repo\"\r\n\r\no/r\r\n"));
        assert!(text.contains("filename=\"s.wyrd\""));
        assert!(text.ends_with("0123456789\r\n--b--\r\n"));
    }
}
