//! What agents may learn about the daemon: where its OTLP ingest listens
//! and a token scoped to one session's ingest routes. The daemon token
//! itself never reaches an agent.

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};

/// The daemon's default loopback address (plan §11.1).
pub const DEFAULT_LISTEN: &str = "127.0.0.1:7717";

/// The daemon's unix socket: `$WYRD_DAEMON_SOCKET`, else
/// `$XDG_RUNTIME_DIR/wyrdline/wyrdd.sock`, else a per-UID path in the
/// temporary directory.
pub fn default_socket() -> PathBuf {
    if let Some(p) = std::env::var_os("WYRD_DAEMON_SOCKET") {
        return PathBuf::from(p);
    }
    std::env::var_os("XDG_RUNTIME_DIR")
        .map_or_else(
            || {
                std::env::temp_dir()
                    .join(format!("wyrdline-{}", rustix::process::getuid().as_raw()))
            },
            |d| PathBuf::from(d).join("wyrdline"),
        )
        .join("wyrdd.sock")
}

/// `$XDG_STATE_HOME/wyrdline/token`, else `~/.local/state/wyrdline/token`.
pub fn default_token_path() -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME").map_or_else(
        || {
            std::env::var_os("HOME")
                .map_or_else(|| PathBuf::from("/"), PathBuf::from)
                .join(".local/state")
        },
        PathBuf::from,
    );
    base.join("wyrdline/token")
}

/// Read the token at `path`, creating it (0600, directory 0700) if absent.
pub fn load_or_create_token(path: &Path) -> std::io::Result<String> {
    private_dir(path)?;
    match fs::read_to_string(path) {
        Ok(t) if !t.trim().is_empty() => return Ok(t.trim().to_string()),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    write_new_token(path)
}

/// Create the token's directory (0700) if needed.
fn private_dir(path: &Path) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Replace the token at `path` with a fresh one and return it.
pub fn write_new_token(path: &Path) -> std::io::Result<String> {
    private_dir(path)?;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
    let token = hex::encode(bytes);
    let tmp = path.with_extension("tmp");
    let _ = fs::remove_file(&tmp);
    let mut f = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(token.as_bytes())?;
    f.sync_all()?;
    fs::rename(&tmp, path)?;
    Ok(token)
}

/// The ingest token of session `sid`, derived one-way from the daemon
/// token (keyed BLAKE3), so an agent can hold it without learning the
/// daemon token.
pub fn ingest_token(daemon_token: &str, sid: &str) -> String {
    let key = blake3::hash(daemon_token.as_bytes());
    let mut h = blake3::Hasher::new_keyed(key.as_bytes());
    h.update(b"wyrdline ingest v1\0");
    h.update(sid.as_bytes());
    h.finalize().to_hex().to_string()
}

/// OpenTelemetry exporter settings that send an agent's traces to the
/// daemon for session `sid`. Empty when the user already configured an
/// OTLP endpoint (`is_set` reports existing variables): theirs wins.
pub fn otlp_env(
    listen: &str,
    sid: &str,
    daemon_token: &str,
    is_set: impl Fn(&str) -> bool,
) -> Vec<(String, String)> {
    if is_set("OTEL_EXPORTER_OTLP_ENDPOINT") || is_set("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT") {
        return Vec::new();
    }
    vec![
        (
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT".into(),
            format!("http://{listen}/v1/sessions/{sid}/otlp/v1/traces"),
        ),
        (
            "OTEL_EXPORTER_OTLP_TRACES_PROTOCOL".into(),
            "http/json".into(),
        ),
        (
            "OTEL_EXPORTER_OTLP_TRACES_HEADERS".into(),
            format!("authorization=Bearer {}", ingest_token(daemon_token, sid)),
        ),
    ]
}

#[cfg(test)]
mod tests {
    // nosemgrep: wyrd-no-lint-allow-for-unwrap-on-input-paths -- inside #[cfg(test)] mod tests; the allow is test-only.
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn token_file_is_private_and_stable() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state/wyrdline/token");
        let t1 = load_or_create_token(&path).unwrap();
        assert_eq!(t1.len(), 64);
        assert_eq!(load_or_create_token(&path).unwrap(), t1);
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&path), 0o600);
        assert_eq!(mode(path.parent().unwrap()), 0o700);
        let t2 = write_new_token(&path).unwrap();
        assert_ne!(t1, t2);
        assert_eq!(mode(&path), 0o600);
    }

    #[test]
    fn ingest_tokens_are_per_session_and_per_daemon() {
        let a = ingest_token("daemon-1", "01aaa");
        assert_eq!(a.len(), 64);
        assert_eq!(a, ingest_token("daemon-1", "01aaa"));
        assert_ne!(a, ingest_token("daemon-1", "01aab"));
        assert_ne!(a, ingest_token("daemon-2", "01aaa"));
    }

    #[test]
    fn otlp_env_never_carries_the_daemon_token_and_respects_the_user() {
        let env = otlp_env(DEFAULT_LISTEN, "01aaa", "secret-daemon-token", |_| false);
        assert_eq!(env.len(), 3);
        assert!(env.iter().all(|(_, v)| !v.contains("secret-daemon-token")));
        assert_eq!(
            env[0].1,
            "http://127.0.0.1:7717/v1/sessions/01aaa/otlp/v1/traces"
        );
        assert!(
            env[2]
                .1
                .ends_with(&ingest_token("secret-daemon-token", "01aaa"))
        );
        let user_set = otlp_env(DEFAULT_LISTEN, "01aaa", "t", |k| {
            k == "OTEL_EXPORTER_OTLP_ENDPOINT"
        });
        assert_eq!(user_set, Vec::new());
    }
}
