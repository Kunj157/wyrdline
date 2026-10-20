#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `wyrd open`: verify a bundle, then serve it with the Studio on loopback.

use std::collections::BTreeMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};

use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};

struct Env {
    tmp: tempfile::TempDir,
    studio: PathBuf,
}

impl Env {
    fn wyrd(&self) -> Command {
        let mut c = Command::new(env!("CARGO_BIN_EXE_wyrd"));
        c.env("WYRD_DATA_DIR", self.tmp.path().join("data"))
            .env("XDG_STATE_HOME", self.tmp.path().join("state"))
            .env("WYRD_DAEMON_SOCKET", self.tmp.path().join("no-daemon.sock"))
            .env_remove("WYRD_STUDIO_DIR");
        c
    }
}

/// A closed session exported as `ok.wyrd`, and a fake Studio build.
fn env() -> (Env, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join("a.txt"), "a\n").unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let mut s = Session::create(
        &data,
        CreateOptions {
            workspace: ws,
            command: vec!["agent".into()],
            observe: false,
            name: Some("open".into()),
            agent_kind: "fakeagent".into(),
            deny_tools: Vec::new(),
            policy: None,
            kernel_release: "test".into(),
            capabilities: BTreeMap::new(),
        },
    )
    .unwrap();
    std::fs::write(s.dir().upper("a").join("a.txt"), "edited\n").unwrap();
    s.checkpoint(None).unwrap();
    s.close("test").unwrap();
    let id = s.record().id.clone();
    let studio = tmp.path().join("studio");
    std::fs::create_dir_all(studio.join("assets")).unwrap();
    std::fs::write(
        studio.join("index.html"),
        "<!doctype html><title>Studio</title>",
    )
    .unwrap();
    std::fs::write(studio.join("assets/wyrd_wasm_bg.wasm"), b"\0asm").unwrap();
    let e = Env { tmp, studio };
    let bundle = e.tmp.path().join("ok.wyrd");
    let out = e
        .wyrd()
        .args(["export", &id, "-o"])
        .arg(&bundle)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    (e, bundle)
}

struct Server(Child);

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// `wyrd open --print`; returns the server and the URL it printed.
fn open(e: &Env, bundle: &Path) -> (Server, String) {
    let mut child = e
        .wyrd()
        .args(["open", "--print", "--studio"])
        .arg(&e.studio)
        .arg(bundle)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    (Server(child), line.trim().to_string())
}

/// `GET path` against `http://host:port/…`: status line, headers, body.
fn get(url: &str, path: &str) -> (String, String, Vec<u8>) {
    let host = url
        .strip_prefix("http://")
        .unwrap()
        .split('/')
        .next()
        .unwrap();
    let mut s = TcpStream::connect(host).unwrap();
    write!(s, "GET {path} HTTP/1.1\r\nHost: {host}\r\n\r\n").unwrap();
    let mut raw = Vec::new();
    s.read_to_end(&mut raw).unwrap();
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8(raw[..split].to_vec()).unwrap();
    let (status, headers) = head.split_once("\r\n").unwrap();
    (
        status.to_string(),
        headers.to_lowercase(),
        raw[split + 4..].to_vec(),
    )
}

#[test]
fn open_serves_studio_in_bundle_mode_on_loopback() {
    let (e, bundle) = env();
    let (_server, url) = open(&e, &bundle);
    assert!(
        url.starts_with("http://127.0.0.1:") && url.ends_with("/?bundle=/bundle/ok.wyrd"),
        "{url}"
    );
    let (status, headers, body) = get(&url, "/?bundle=/bundle/ok.wyrd");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert!(headers.contains("content-security-policy: default-src 'self'"));
    assert!(String::from_utf8_lossy(&body).contains("<title>Studio</title>"));
    let (status, _, body) = get(&url, "/bundle/ok.wyrd");
    assert_eq!(status, "HTTP/1.1 200 OK");
    assert_eq!(body, std::fs::read(&bundle).unwrap());
    let (_, headers, _) = get(&url, "/assets/wyrd_wasm_bg.wasm");
    assert!(
        headers.contains("content-type: application/wasm"),
        "{headers}"
    );
    for path in ["/v1/sessions", "/../ok.wyrd", "/bundle/../ok.wyrd"] {
        assert_eq!(get(&url, path).0, "HTTP/1.1 404 Not Found", "{path}");
    }
}

#[test]
fn open_refuses_a_bundle_that_does_not_verify() {
    let (e, bundle) = env();
    let mut bytes = std::fs::read(&bundle).unwrap();
    let n = bytes.len();
    bytes[n / 2] ^= 1;
    std::fs::write(&bundle, bytes).unwrap();
    let out = e
        .wyrd()
        .args(["open", "--print", "--studio"])
        .arg(&e.studio)
        .arg(&bundle)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    assert!(String::from_utf8_lossy(&out.stderr).contains("does not verify"));
    assert!(out.stdout.is_empty());
}

#[test]
fn open_needs_a_studio_build() {
    let (e, bundle) = env();
    let out = e
        .wyrd()
        .args(["open", "--print", "--studio"])
        .arg(e.tmp.path().join("nowhere"))
        .arg(&bundle)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("no built Studio"));
}
