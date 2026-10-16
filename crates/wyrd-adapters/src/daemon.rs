//! What agents may learn about the daemon: where its OTLP ingest listens
//! and a token scoped to one session's ingest routes. The daemon token
//! itself never reaches an agent.

use std::path::PathBuf;

/// The daemon's default loopback address (plan §11.1).
pub const DEFAULT_LISTEN: &str = "127.0.0.1:7717";

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
    use super::*;

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
