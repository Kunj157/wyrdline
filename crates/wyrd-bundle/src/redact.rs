//! Secret redaction.
//!
//! Applied at ingest (before the ledger) at the `Standard` level and again
//! at export at the `Strict` level. A redacted value becomes
//! `[REDACTED:<kind>:<hash8>]` (kind names never contain the trigger words
//! of the rules, so markers are stable), where `hash8` is the first 8 hex digits of
//! BLAKE3 of the secret: equal secrets stay correlatable without being
//! revealed.

use std::sync::LazyLock;

use regex::{Captures, Regex};

/// How aggressively to redact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    /// Pass everything through (tests and debugging only).
    Off,
    /// Known secret formats, credential assignments, auth headers, URL
    /// passwords, and high-entropy tokens.
    Standard,
    /// Standard plus home-directory user names and host names (replaced by
    /// stable pseudonyms, see [`host_pseudonym`]).
    Strict,
}

struct Rule {
    kind: &'static str,
    re: Regex,
    /// Capture group holding the secret (0 = whole match).
    group: usize,
}

fn rule(kind: &'static str, pattern: &str, group: usize) -> Rule {
    // The patterns are constants; a failure here is a programming error
    // caught by every test run.
    #[allow(clippy::expect_used)]
    let re = Regex::new(pattern).expect("valid redaction pattern");
    Rule { kind, re, group }
}

static RULES: LazyLock<Vec<Rule>> = LazyLock::new(|| {
    vec![
        rule(
            "pem_block",
            r"-----BEGIN [A-Z0-9 ]*PRIVATE KEY-----[\s\S]*?(?:-----END [A-Z0-9 ]*PRIVATE KEY-----|$)",
            0,
        ),
        rule("aws", r"\b(?:AKIA|ASIA|AGPA|AIDA|AROA)[0-9A-Z]{16}\b", 0),
        rule(
            "github",
            r"\b(?:gh[pousr]_[A-Za-z0-9]{36,}|github_pat_[A-Za-z0-9_]{60,})\b",
            0,
        ),
        rule("anthropic", r"\bsk-ant-[A-Za-z0-9_-]{20,}", 0),
        rule("openai", r"\bsk-(?:proj-)?[A-Za-z0-9_-]{32,}", 0),
        rule("slack", r"\bxox[abposr]-[A-Za-z0-9-]{10,}", 0),
        rule("google", r"\bAIza[0-9A-Za-z_-]{35}\b", 0),
        rule("stripe", r"\b(?:sk|rk)_(?:live|test)_[0-9a-zA-Z]{24,}\b", 0),
        rule(
            "jwt",
            r"\beyJ[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}\.[A-Za-z0-9_-]{8,}",
            0,
        ),
        rule(
            "header",
            r"(?i)\b(?:authorization|proxy-authorization|cookie|set-cookie|x-api-key|api-key)\s*:\s*([^\r\n]+)",
            1,
        ),
        rule(
            "url_credential",
            r"(?i)\b[a-z][a-z0-9+.-]*://[^\s:/@]+:([^\s@/]+)@",
            1,
        ),
        rule(
            "credential",
            r#"(?i)\b[A-Za-z0-9_]*(?:password|passwd|pwd|secret|token|api[_-]?key|access[_-]?key|private[_-]?key|client[_-]?secret)[A-Za-z0-9_]*\s*[=:]\s*["']?([^\s"'&;,\[][^\s"'&;,]{5,})"#,
            1,
        ),
    ]
});

static CANDIDATE: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used)]
    Regex::new(r"[A-Za-z0-9+/=_-]{32,}").expect("valid pattern")
});

static HOME: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used)]
    Regex::new(r"/home/([A-Za-z0-9._-]+)|/Users/([A-Za-z0-9._-]+)").expect("valid pattern")
});

/// A DNS name in a URL authority (after optional `user[:pass]@`).
static URL_HOST: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used)]
    Regex::new(
        r"(?i)\b[a-z][a-z0-9+.-]*://(?:[^\s/@]*@)?([a-z0-9-]+(?:\.[a-z0-9-]+)*\.[a-z]{2,63})\b",
    )
    .expect("valid pattern")
});

/// A DNS name after `user@` (e-mail addresses, `git@host:repo`, `ssh user@host`).
static AT_HOST: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used)]
    Regex::new(r"(?i)\b[a-z0-9._%+-]+@([a-z0-9-]+(?:\.[a-z0-9-]+)*\.[a-z]{2,63})\b")
        .expect("valid pattern")
});

/// Reserved top-level domain of pseudonyms (RFC 2606), so a pseudonym is
/// never a real host and is never pseudonymised again.
const PSEUDONYM_TLD: &str = ".invalid";

/// Stable pseudonym of a host name: `host-<hash8>.invalid`, where `hash8`
/// is the first 8 hex digits of BLAKE3 of the lower-cased name. The same
/// host always gets the same pseudonym, so connections stay correlatable.
/// `localhost` and existing pseudonyms are returned unchanged.
pub fn host_pseudonym(host: &str) -> String {
    let lower = host.to_ascii_lowercase();
    if lower.is_empty() || lower == "localhost" || lower.ends_with(PSEUDONYM_TLD) {
        return host.to_string();
    }
    format!(
        "host-{}{PSEUDONYM_TLD}",
        &hex::encode(blake3::hash(lower.as_bytes()).as_bytes())[..8]
    )
}

fn replace_hosts(re: &Regex, s: &str) -> String {
    re.replace_all(s, |c: &Captures<'_>| {
        let whole = c.get(0).map_or("", |m| m.as_str());
        match (c.get(0), c.get(1)) {
            (Some(w), Some(h)) => {
                let mut out = whole.to_string();
                let start = h.start() - w.start();
                out.replace_range(start..start + h.len(), &host_pseudonym(h.as_str()));
                out
            }
            _ => whole.to_string(),
        }
    })
    .into_owned()
}

/// JSON object keys whose values are always secrets.
const SECRET_KEYS: &[&str] = &[
    "password",
    "passwd",
    "secret",
    "token",
    "api_key",
    "apikey",
    "access_token",
    "refresh_token",
    "client_secret",
    "private_key",
    "authorization",
    "cookie",
];

fn marker(kind: &str, secret: &str) -> String {
    format!(
        "[REDACTED:{kind}:{}]",
        &hex::encode(blake3::hash(secret.as_bytes()).as_bytes())[..8]
    )
}

/// Shannon entropy in bits per character.
pub fn entropy(s: &str) -> f64 {
    let mut counts = [0u32; 256];
    for b in s.bytes() {
        counts[usize::from(b)] += 1;
    }
    #[allow(clippy::cast_precision_loss)]
    let n = s.len() as f64;
    counts
        .iter()
        .filter(|&&c| c > 0)
        .map(|&c| {
            let p = f64::from(c) / n;
            -p * p.log2()
        })
        .sum()
}

/// A token that looks random: mixed letter cases and digits, high entropy,
/// and not purely hexadecimal (commit hashes and digests are not secrets).
fn looks_random(t: &str) -> bool {
    let hex_only = t.bytes().all(|b| b.is_ascii_hexdigit());
    let upper = t.bytes().any(|b| b.is_ascii_uppercase());
    let lower = t.bytes().any(|b| b.is_ascii_lowercase());
    let digit = t.bytes().any(|b| b.is_ascii_digit());
    let pathish = t.contains('/') && !t.contains('+');
    !hex_only && !pathish && upper && lower && digit && entropy(t) >= 4.3
}

/// Redact a string.
pub fn redact_str(input: &str, level: Level) -> String {
    if level == Level::Off {
        return input.to_string();
    }
    let mut s = input.to_string();
    for r in RULES.iter() {
        s =
            r.re.replace_all(&s, |c: &Captures<'_>| {
                let whole = c.get(0).map_or("", |m| m.as_str());
                match c.get(r.group) {
                    Some(m) if m.as_str().starts_with("[REDACTED:") => whole.to_string(),
                    Some(m) => {
                        let start = m.start() - c.get(0).map_or(0, |w| w.start());
                        let mut out = whole.to_string();
                        out.replace_range(start..start + m.len(), &marker(r.kind, m.as_str()));
                        out
                    }
                    None => whole.to_string(),
                }
            })
            .into_owned();
    }
    s = CANDIDATE
        .replace_all(&s, |c: &Captures<'_>| {
            let t = c.get(0).map_or("", |m| m.as_str());
            if looks_random(t) {
                marker("high_entropy", t)
            } else {
                t.to_string()
            }
        })
        .into_owned();
    if level >= Level::Strict {
        s = HOME.replace_all(&s, "~").into_owned();
        s = replace_hosts(&URL_HOST, &s);
        s = replace_hosts(&AT_HOST, &s);
    }
    s
}

/// Redact every string in a JSON value; values under secret-named keys are
/// replaced entirely.
pub fn redact_json(v: &serde_json::Value, level: Level) -> serde_json::Value {
    use serde_json::Value;
    if level == Level::Off {
        return v.clone();
    }
    match v {
        Value::String(s) => Value::String(redact_str(s, level)),
        Value::Array(a) => Value::Array(a.iter().map(|x| redact_json(x, level)).collect()),
        Value::Object(o) => Value::Object(
            o.iter()
                .map(|(k, x)| {
                    let secret_key = SECRET_KEYS.contains(&k.to_ascii_lowercase().as_str());
                    let value = match x {
                        Value::String(s)
                            if secret_key && !s.is_empty() && !s.starts_with("[REDACTED:") =>
                        {
                            Value::String(marker("field", s))
                        }
                        _ => redact_json(x, level),
                    };
                    (k.clone(), value)
                })
                .collect(),
        ),
        other => other.clone(),
    }
}

/// Paths whose contents are never captured (only their path and hash).
pub fn is_secret_path(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path);
    let lower = path.to_ascii_lowercase();
    let ext = std::path::Path::new(name)
        .extension()
        .map(std::ffi::OsStr::to_ascii_lowercase);
    let key_file = ext.is_some_and(|e| e == "pem" || e == "key" || e == "p12");
    (name.starts_with(".env") && name != ".env.example" && name != ".env.sample")
        || name.starts_with("id_rsa")
        || name.starts_with("id_ed25519")
        || name.starts_with("id_ecdsa")
        || key_file
        || name == ".netrc"
        || name == ".npmrc"
        || name == ".pypirc"
        || lower.contains("/.ssh/")
        || lower.contains("/.aws/")
        || lower.contains("/.gnupg/")
        || lower.contains("/.config/gh/")
        || lower.contains("/.docker/config.json")
        || lower.contains("/.kube/config")
}
