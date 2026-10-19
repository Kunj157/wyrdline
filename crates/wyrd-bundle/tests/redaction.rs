#![allow(clippy::unwrap_used, clippy::panic)]

//! Redaction corpus. Secrets are assembled at runtime so that no
//! secret-shaped literal exists in the repository.

use proptest::prelude::*;
use wyrd_bundle::redact::{
    KnownHosts, Level, host_pseudonym, hosts_in, is_secret_path, redact_json, redact_str,
};

fn rep(c: char, n: usize) -> String {
    std::iter::repeat_n(c, n).collect()
}

fn mixed(n: usize) -> String {
    "aB3dE9fG2hJ7kL5mN8pQ1rS4tU6vW0xY"
        .chars()
        .cycle()
        .take(n)
        .collect()
}

fn corpus() -> Vec<(&'static str, String)> {
    vec![
        ("aws", format!("{}{}", "AKIA", rep('Q', 16))),
        ("github", format!("{}{}", "ghp_", mixed(36))),
        ("github_pat", format!("{}{}", "github_pat_", mixed(70))),
        ("anthropic", format!("{}{}", "sk-ant-api03-", mixed(40))),
        ("openai", format!("{}{}", "sk-proj-", mixed(40))),
        ("slack", format!("{}{}", "xoxb-", mixed(24))),
        ("google", format!("{}{}", "AIza", mixed(35))),
        ("stripe", format!("{}{}", "sk_live_", mixed(26))),
        (
            "jwt",
            format!("eyJ{}.eyJ{}.{}", mixed(12), mixed(20), mixed(30)),
        ),
        ("random", mixed(40)),
    ]
}

#[test]
fn every_known_format_is_redacted_in_context() {
    for (name, secret) in corpus() {
        for wrapped in [
            secret.clone(),
            format!("curl -H 'X-Token: {secret}' https://x"),
            format!("export TOKEN={secret}"),
            format!("{{\"value\": \"{secret}\"}}"),
            format!("line one\n  {secret}  \nline three"),
        ] {
            let out = redact_str(&wrapped, Level::Standard);
            assert!(
                !out.contains(&secret),
                "{name} leaked in {wrapped:?} -> {out:?}"
            );
            assert!(out.contains("[REDACTED:"), "{name}: {out}");
        }
    }
}

#[test]
fn private_key_blocks_are_removed_entirely() {
    let body = mixed(64);
    let key = format!(
        "-----BEGIN OPENSSH {}-----\n{body}\n{body}\n-----END OPENSSH {}-----",
        "PRIVATE KEY", "PRIVATE KEY"
    );
    let out = redact_str(&format!("cat id_ed25519\n{key}\ndone"), Level::Standard);
    assert!(!out.contains(&body));
    assert!(out.contains("[REDACTED:pem_block:"));
    assert!(out.ends_with("done"));
}

#[test]
fn credential_assignments_headers_and_url_passwords() {
    let pw = format!("hunter{}", "2xyz");
    for (input, secret) in [
        (format!("DB_PASSWORD={pw} ./run"), pw.clone()),
        (format!("api_key: \"{pw}\""), pw.clone()),
        (
            format!("Authorization: Bearer {pw}"),
            format!("Bearer {pw}"),
        ),
        (format!("postgres://admin:{pw}@db:5432/app"), pw.clone()),
        (format!("--client-secret={pw}"), pw.clone()),
    ] {
        let out = redact_str(&input, Level::Standard);
        assert!(!out.contains(&secret), "{input:?} -> {out:?}");
    }
    // The non-secret parts stay readable.
    let out = redact_str(
        &format!("postgres://admin:{pw}@db:5432/app"),
        Level::Standard,
    );
    assert!(out.starts_with("postgres://admin:[REDACTED:url_credential:"));
    assert!(out.ends_with("@db:5432/app"));
}

#[test]
fn ordinary_developer_text_is_left_alone() {
    for s in [
        "git show 3d3c42e5aac5ba805825da76410c181273ba90b1",
        "sha256:6323deb102c322ba6fcbdcafc7e3dddab59af2b6aaaabbbbccccdddd",
        "cargo test --workspace --locked -- --nocapture",
        "/home/sentics/kunj-personal/wyrdline/crates/wyrd-cell/src/session.rs",
        "pnpm --filter @wyrdline/studio exec playwright test",
        "fix the failing cart test in src/cart.test.ts",
        "token count: 1234, password reset flow",
        "b3:fa7b62dbcf8b196ef17c89a6342edc6f4ac01423de850c223152f0f09a8193a8",
    ] {
        assert_eq!(redact_str(s, Level::Standard), s, "false positive");
    }
}

#[test]
fn same_secret_same_marker() {
    let s = format!("{}{}", "ghp_", mixed(36));
    let a = redact_str(&format!("x {s}"), Level::Standard);
    let b = redact_str(&format!("y {s}"), Level::Standard);
    assert_eq!(a[2..], b[2..]);
}

#[test]
fn json_is_redacted_recursively_and_by_key() {
    let secret = format!("{}{}", "AKIA", rep('Z', 16));
    let v = serde_json::json!({
        "command": format!("aws s3 ls --key {secret}"),
        "env": {"PASSWORD": "short", "nested": [{"token": "abc"}, format!("x {secret}")]},
        "count": 3
    });
    let out = redact_json(&v, Level::Standard);
    let text = out.to_string();
    assert!(!text.contains(&secret));
    assert!(!text.contains("\"short\""));
    assert!(!text.contains("\"abc\""));
    assert_eq!(out["count"], 3);
    // Redacting again changes nothing, secret-named keys included.
    assert_eq!(redact_json(&out, Level::Standard), out);
    assert_eq!(
        redact_json(&out, Level::Strict),
        redact_json(&out, Level::Strict)
    );
}

#[test]
fn strict_mode_hides_home_user_names() {
    let p = "/home/alice/repo/src/main.rs";
    assert_eq!(redact_str(p, Level::Standard), p);
    assert_eq!(redact_str(p, Level::Strict), "~/repo/src/main.rs");
    assert_eq!(redact_str(p, Level::Off), p);
}

#[test]
fn strict_mode_pseudonymises_host_names() {
    for (input, host) in [
        (
            "curl https://diag.evil-corp.example/upload",
            "diag.evil-corp.example",
        ),
        ("git clone git@github.com:org/repo.git", "github.com"),
        (
            "ssh deploy@build-07.corp.internal",
            "build-07.corp.internal",
        ),
        ("mail alice@example.org", "example.org"),
        (
            "postgres://admin:pw@db.prod.example.net:5432/app",
            "db.prod.example.net",
        ),
    ] {
        let out = redact_str(input, Level::Strict);
        assert!(!out.contains(host), "{input:?} -> {out:?}");
        assert!(out.contains(&host_pseudonym(host)), "{input:?} -> {out:?}");
        assert!(redact_str(input, Level::Standard).contains(host));
    }
    // Stable and case-insensitive; the rest of the URL stays readable.
    let a = redact_str("see https://Docs.Example.com/a?b=1", Level::Strict);
    let b = redact_str("see https://docs.example.com/a?b=1", Level::Strict);
    assert_eq!(a, b);
    assert!(a.starts_with("see https://host-"));
    assert!(a.ends_with(".invalid/a?b=1"));
}

#[test]
fn strict_mode_keeps_what_is_not_a_host_name() {
    for s in [
        "http://localhost:7717/v1/sessions",
        "connect 10.0.0.7:443",
        "https://127.0.0.1:8080/health",
        "edit src/cart.test.ts and package.json",
        "npm install left-pad@1.3.0",
        "postgres://admin@db:5432/app",
    ] {
        assert_eq!(redact_str(s, Level::Strict), s, "false positive");
    }
    assert_eq!(host_pseudonym("localhost"), "localhost");
    let p = host_pseudonym("example.com");
    assert_eq!(host_pseudonym(&p), p);
    assert_eq!(p.len(), "host-12345678.invalid".len());
}

#[test]
fn known_hosts_are_replaced_in_prose() {
    let found = hosts_in("curl https://Exfil.Example.com/x; ssh me@db.corp.internal");
    assert_eq!(found, ["exfil.example.com", "db.corp.internal"]);
    let known = KnownHosts::new(found.into_iter().chain(["example.com".to_string()]));
    let out = known.replace("sent to exfil.example.com, then EXAMPLE.COM and db.corp.internal.");
    assert_eq!(
        out,
        format!(
            "sent to {}, then {} and {}.",
            host_pseudonym("exfil.example.com"),
            host_pseudonym("example.com"),
            host_pseudonym("db.corp.internal")
        )
    );
    // Not inside longer names or words.
    assert_eq!(
        known.replace("notexample.community"),
        "notexample.community"
    );
    assert_eq!(
        KnownHosts::new([]).replace("a.example.com"),
        "a.example.com"
    );
}

#[test]
fn secret_paths() {
    for p in [
        ".env",
        "app/.env.local",
        "/home/u/.ssh/id_ed25519",
        "certs/server.pem",
        "/home/u/.aws/credentials",
        "/home/u/.config/gh/hosts.yml",
    ] {
        assert!(is_secret_path(p), "{p}");
    }
    for p in [".env.example", "src/env.rs", "README.md", "keys.rs"] {
        assert!(!is_secret_path(p), "{p}");
    }
}

proptest! {
    #[test]
    fn redaction_never_panics_and_is_idempotent(s in ".{0,300}") {
        let once = redact_str(&s, Level::Strict);
        prop_assert_eq!(redact_str(&once, Level::Strict), once.clone());
    }
}
