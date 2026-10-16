//! `wyrd token` and `wyrd daemon status`: the CLI side of `wyrdd`.

use std::io::{Read as _, Write as _};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Context as _;
use wyrd_adapters::daemon;

use crate::exit;

#[derive(Debug, clap::Subcommand)]
pub enum TokenCmd {
    /// Replace the daemon token; a running daemon picks it up at once.
    /// Ingest tokens handed to agents before the rotation stop working.
    Rotate,
    /// Print where the token file is (never the token).
    Path,
}

#[derive(Debug, clap::Subcommand)]
pub enum DaemonCmd {
    /// Check that wyrdd answers on its unix socket (exit 5 if not).
    Status {
        /// Unix socket of the daemon.
        #[arg(long)]
        socket: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
}

pub fn token(cmd: &TokenCmd) -> anyhow::Result<u8> {
    let path = daemon::default_token_path();
    match cmd {
        TokenCmd::Rotate => {
            daemon::write_new_token(&path)
                .with_context(|| format!("writing {}", path.display()))?;
            writeln!(
                std::io::stdout(),
                "wyrd: token rotated ({})",
                path.display()
            )?;
        }
        TokenCmd::Path => writeln!(std::io::stdout(), "{}", path.display())?,
    }
    Ok(exit::OK)
}

/// One HTTP/1.1 GET over the unix socket; returns (status, body).
fn get(socket: &std::path::Path, path: &str) -> std::io::Result<(u16, String)> {
    let mut s = UnixStream::connect(socket)?;
    s.set_read_timeout(Some(Duration::from_secs(5)))?;
    write!(
        s,
        "GET {path} HTTP/1.1\r\nhost: wyrdd\r\naccept: application/json\r\nconnection: close\r\n\r\n"
    )?;
    let mut raw = String::new();
    s.read_to_string(&mut raw)?;
    let (head, body) = raw.split_once("\r\n\r\n").unwrap_or((&raw, ""));
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    // Responses may be chunked; the JSON object sits between the braces.
    let body = match (body.find('{'), body.rfind('}')) {
        (Some(a), Some(b)) if a <= b => body[a..=b].to_string(),
        _ => body.trim().to_string(),
    };
    Ok((status, body))
}

pub fn daemon(cmd: &DaemonCmd) -> anyhow::Result<u8> {
    let DaemonCmd::Status { socket, json } = cmd;
    let socket = socket.clone().unwrap_or_else(daemon::default_socket);
    let mut out = std::io::stdout().lock();
    match get(&socket, "/v1/health") {
        Ok((200, body)) => {
            let health: serde_json::Value = serde_json::from_str(&body).unwrap_or_default();
            if *json {
                let v = serde_json::json!({ "running": true, "socket": socket, "health": health });
                serde_json::to_writer_pretty(&mut out, &v)?;
                writeln!(out)?;
            } else {
                writeln!(
                    out,
                    "wyrdd running on {} (version {})",
                    socket.display(),
                    health["version"].as_str().unwrap_or("?")
                )?;
            }
            Ok(exit::OK)
        }
        other => {
            let reason = match other {
                Ok((status, _)) => format!("answered HTTP {status}"),
                Err(e) => e.to_string(),
            };
            if *json {
                let v = serde_json::json!({ "running": false, "socket": socket, "error": reason });
                serde_json::to_writer_pretty(&mut out, &v)?;
                writeln!(out)?;
            } else {
                writeln!(
                    out,
                    "wyrdd is not answering on {}: {reason}",
                    socket.display()
                )?;
            }
            Ok(exit::DAEMON_UNAVAILABLE)
        }
    }
}
