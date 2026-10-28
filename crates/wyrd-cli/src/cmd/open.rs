//! `wyrd open <file.wyrd>`: Studio in bundle mode, without the daemon.
//!
//! The bundle is verified here first (a bundle that does not verify is
//! refused with exit 3), then served with the built Studio on a loopback
//! port; Studio verifies it again in the browser before showing anything.
//! The server answers `GET`/`HEAD` for the Studio's files and the one
//! bundle, nothing else, and stops with Ctrl-C.

use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::net::{Ipv4Addr, TcpListener, TcpStream};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context as _;

use crate::exit;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Bundle to open.
    file: PathBuf,
    /// Built Studio to serve (default `$WYRD_STUDIO_DIR`, else
    /// `share/wyrdline/studio` next to the installed binary).
    #[arg(long, value_name = "DIR")]
    studio: Option<PathBuf>,
    /// Loopback port (default: any free one).
    #[arg(long, default_value_t = 0)]
    port: u16,
    /// Print the URL instead of opening a browser.
    #[arg(long)]
    print: bool,
}

/// Same Content-Security-Policy as the daemon's Studio.
const CSP: &str = "default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; \
style-src 'self' 'unsafe-inline'; img-src 'self' data: blob:; font-src 'self' data:; \
connect-src 'self'; worker-src 'self' blob:; object-src 'none'; base-uri 'none'; \
frame-ancestors 'none'; form-action 'none'";

/// Longest request head read.
const MAX_HEAD: usize = 16 * 1024;

/// What the server serves: the Studio directory and one bundle.
#[derive(Debug)]
pub struct Site {
    studio: PathBuf,
    /// URL path of the bundle (`/bundle/<name>`).
    bundle_path: String,
    bundle: Vec<u8>,
}

fn studio_dir(arg: Option<&Path>) -> Option<PathBuf> {
    arg.map(Path::to_path_buf)
        .or_else(|| std::env::var_os("WYRD_STUDIO_DIR").map(PathBuf::from))
        .or_else(|| {
            let exe = std::env::current_exe().ok()?;
            Some(exe.parent()?.parent()?.join("share/wyrdline/studio"))
        })
        .filter(|d| d.join("index.html").is_file())
}

/// The bundle's file name if it is URL-safe, else `bundle.wyrd`.
fn bundle_name(file: &Path) -> String {
    file.file_name()
        .and_then(|n| n.to_str())
        .filter(|n| {
            !n.is_empty()
                && !n.starts_with('.')
                && n.bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        })
        .unwrap_or("bundle.wyrd")
        .to_string()
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" | "map" => "application/json",
        "wasm" => "application/wasm",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "ico" => "image/x-icon",
        "woff2" => "font/woff2",
        "txt" | "jsonl" => "text/plain; charset=utf-8",
        _ => "application/octet-stream",
    }
}

/// The response to `GET <target>`: status, content type, body.
pub fn respond(site: &Site, target: &str) -> (u16, &'static str, Vec<u8>) {
    let path = target.split(['?', '#']).next().unwrap_or("/");
    if path == site.bundle_path {
        return (200, "application/octet-stream", site.bundle.clone());
    }
    if path.starts_with("/v1/") || path == "/v1" {
        return not_found();
    }
    let rel = Path::new(path.trim_start_matches('/'));
    // Only plain names below the Studio directory; no `..`, no escapes.
    if path.contains('\\')
        || path.contains('%')
        || !rel.components().all(|c| matches!(c, Component::Normal(_)))
    {
        return not_found();
    }
    let file = site.studio.join(rel);
    let file = if file.is_file() {
        file
    } else if rel.extension().is_none() {
        // A single-page route: the app decides.
        site.studio.join("index.html")
    } else {
        return not_found();
    };
    // Symlinks may not lead out of the Studio directory.
    let inside = match (file.canonicalize(), site.studio.canonicalize()) {
        (Ok(f), Ok(root)) => f.starts_with(root),
        _ => false,
    };
    if !inside {
        return not_found();
    }
    match std::fs::read(&file) {
        Ok(body) => (200, content_type(&file), body),
        Err(_) => not_found(),
    }
}

fn not_found() -> (u16, &'static str, Vec<u8>) {
    (404, "text/plain; charset=utf-8", b"not found\n".to_vec())
}

fn handle(site: &Site, stream: TcpStream) -> std::io::Result<()> {
    stream.set_read_timeout(Some(Duration::from_secs(10)))?;
    let mut reader = BufReader::new(stream.try_clone()?.take(MAX_HEAD as u64));
    let mut line = String::new();
    reader.read_line(&mut line)?;
    // Drain the rest of the head.
    let mut header = String::new();
    while reader.read_line(&mut header)? > 2 {
        header.clear();
    }
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let (status, ctype, body) = match method {
        "GET" | "HEAD" => respond(site, target),
        _ => (
            405,
            "text/plain; charset=utf-8",
            b"method not allowed\n".to_vec(),
        ),
    };
    let reason = match status {
        200 => "OK",
        404 => "Not Found",
        _ => "Method Not Allowed",
    };
    let mut out = stream;
    write!(
        out,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ctype}\r\nContent-Length: {}\r\n\
         Content-Security-Policy: {CSP}\r\nX-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\nX-Frame-Options: DENY\r\nCache-Control: no-cache\r\n\
         Connection: close\r\n\r\n",
        body.len()
    )?;
    if method != "HEAD" {
        out.write_all(&body)?;
    }
    out.flush()
}

pub fn run(a: &Args) -> anyhow::Result<u8> {
    let mut o = std::io::stdout().lock();
    let bundle = std::fs::read(&a.file).with_context(|| format!("reading {}", a.file.display()))?;
    if let Err(e) = wyrd_bundle::read(&bundle[..], &wyrd_bundle::Options::default()) {
        writeln!(
            std::io::stderr(),
            "wyrd: {} does not verify ({}: {e}); run `wyrd verify {}`",
            a.file.display(),
            e.stage().describe(),
            a.file.display()
        )?;
        return Ok(exit::VERIFICATION_FAILED);
    }
    let Some(studio) = studio_dir(a.studio.as_deref()) else {
        writeln!(
            std::io::stderr(),
            "wyrd: no built Studio found; pass --studio <dir> or set WYRD_STUDIO_DIR"
        )?;
        return Ok(exit::USAGE);
    };
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, a.port))
        .with_context(|| format!("listening on 127.0.0.1:{}", a.port))?;
    let addr = listener.local_addr()?;
    let site = Arc::new(Site {
        studio,
        bundle_path: format!("/bundle/{}", bundle_name(&a.file)),
        bundle,
    });
    let url = format!("http://{addr}/?bundle={}", site.bundle_path);
    let opened = !a.print
        && Command::new("xdg-open")
            .arg(&url)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
    let first = if opened {
        format!("wyrd: opened {} in Studio at {url}", a.file.display())
    } else {
        url
    };
    // One write: whoever reads the URL may close stdout right after, and
    // the server keeps serving regardless.
    let _ = write!(o, "{first}\nwyrd: serving on {addr} until Ctrl-C\n");
    let _ = o.flush();
    drop(o);
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        let site = Arc::clone(&site);
        std::thread::spawn(move || {
            let _ = handle(&site, stream);
        });
    }
    Ok(exit::OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn site() -> (tempfile::TempDir, Site) {
        let dir = tempfile::tempdir().unwrap();
        let studio = dir.path().join("studio");
        std::fs::create_dir_all(studio.join("assets")).unwrap();
        std::fs::write(studio.join("index.html"), "<!doctype html>").unwrap();
        std::fs::write(studio.join("assets/app.js"), "export {};").unwrap();
        std::fs::write(dir.path().join("secret.txt"), "outside").unwrap();
        std::os::unix::fs::symlink(dir.path().join("secret.txt"), studio.join("link.txt")).unwrap();
        let site = Site {
            studio,
            bundle_path: "/bundle/x.wyrd".into(),
            bundle: vec![0x28, 0xb5, 0x2f, 0xfd],
        };
        (dir, site)
    }

    #[test]
    fn serves_the_studio_the_bundle_and_nothing_else() {
        let (_dir, site) = site();
        assert_eq!(respond(&site, "/bundle/x.wyrd").2, site.bundle);
        let (status, ctype, body) = respond(&site, "/assets/app.js");
        assert_eq!((status, ctype), (200, "text/javascript; charset=utf-8"));
        assert_eq!(body, b"export {};");
        // Single-page routes fall back to the app; missing assets do not.
        assert_eq!(respond(&site, "/sessions/01j?x=1").2, b"<!doctype html>");
        assert_eq!(
            respond(&site, "/?bundle=/bundle/x.wyrd").2,
            b"<!doctype html>"
        );
        assert_eq!(respond(&site, "/assets/missing.js").0, 404);
        for bad in [
            "/v1/sessions",
            "/../secret.txt",
            "/assets/../../secret.txt",
            "/%2e%2e/secret.txt",
            "/link.txt",
            "/assets\\..\\..\\secret.txt",
            "/bundle/other.wyrd",
        ] {
            let (status, _, body) = respond(&site, bad);
            assert!(
                status == 404 || body == b"<!doctype html>",
                "{bad}: {status}"
            );
            assert_ne!(body, b"outside", "{bad}");
        }
    }

    #[test]
    fn bundle_names_are_url_safe() {
        assert_eq!(
            bundle_name(Path::new("/tmp/session-1.wyrd")),
            "session-1.wyrd"
        );
        assert_eq!(bundle_name(Path::new("a b.wyrd")), "bundle.wyrd");
        assert_eq!(bundle_name(Path::new("x?.wyrd")), "bundle.wyrd");
        assert_eq!(bundle_name(Path::new(".hidden")), "bundle.wyrd");
    }
}
