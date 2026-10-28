//! `wyrd mcp [--session <id>]`: a stdio MCP server that lets the agent
//! inspect its own session and ask a human to rewind it (plan §14.3).
//!
//! The server is bound to one session. Inside a Cell (`wyrd run` sets
//! `WYRD_SESSION_ID` and the intent socket), the agent cannot read its
//! ledger, so every tool is answered by `wyrd run` over the intent socket;
//! outside a Cell, the session's files are read directly. Either way the
//! tools are read-only except `request_rewind`, which only records a
//! pending request: confirming it needs the user, through `wyrd rewind
//! --confirm` or the daemon API, neither of which a Cell can reach.

mod server;
pub mod views;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::bail;
use serde_json::Value;
use wyrd_adapters::protocol::call;
use wyrd_adapters::{Request, Response, SessionQuery};
use wyrd_cell::{DataDir, Session};

use crate::exit;

/// Environment variable `wyrd run` sets to the session id in the Cell.
pub const SESSION_ENV: &str = "WYRD_SESSION_ID";

/// Longest wait for the running session to answer one query.
const TIMEOUT: Duration = Duration::from_secs(20);

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Session id or unique prefix (default: the session of the Cell this
    /// runs in).
    #[arg(long)]
    session: Option<String>,
}

/// Answers from the session's files (outside a Cell).
struct Direct {
    data: DataDir,
    session: String,
}

impl server::Backend for Direct {
    fn query(&self, q: &SessionQuery) -> Result<Value, String> {
        views::answer(&self.data, &self.session, q).map_err(|e| format!("{e:#}"))
    }
}

/// Answers from the running session over its intent socket (in a Cell).
struct Socket {
    path: PathBuf,
}

impl server::Backend for Socket {
    fn query(&self, q: &SessionQuery) -> Result<Value, String> {
        let req = Request::Session { query: q.clone() };
        match call(&self.path, &req, TIMEOUT) {
            Ok(Response::View { result }) => Ok(result),
            Ok(Response::Error { message }) => Err(message),
            Ok(other) => Err(format!("unexpected answer from the session: {other:?}")),
            Err(e) => Err(format!("the session is not running ({e})")),
        }
    }
}

/// How this server reaches its session.
#[derive(Debug, PartialEq, Eq)]
enum Binding {
    Socket(PathBuf),
    Direct(String),
}

/// Pick the session: the Cell's (through its intent socket) when running
/// in one, else `--session` or `WYRD_SESSION_ID` read from the data
/// directory. Inside a run, `--session` may only name that run's session.
fn bind(
    arg: Option<&str>,
    env_session: Option<&str>,
    env_socket: Option<PathBuf>,
) -> anyhow::Result<Binding> {
    match (env_session, env_socket) {
        (Some(own), Some(socket)) => {
            if let Some(a) = arg
                && !own.starts_with(a)
            {
                bail!("this server runs in session {own} and can only serve that session");
            }
            Ok(Binding::Socket(socket))
        }
        (own, _) => match arg.or(own) {
            Some(s) => Ok(Binding::Direct(s.to_string())),
            None => bail!(
                "no session to serve: pass --session <id>, or run `wyrd mcp` as an MCP \
                 server of an agent started with `wyrd run`"
            ),
        },
    }
}

pub fn run(a: &Args) -> anyhow::Result<u8> {
    let env_session = std::env::var(SESSION_ENV).ok().filter(|s| !s.is_empty());
    let env_socket = std::env::var_os(wyrd_adapters::SOCKET_ENV)
        .filter(|s| !s.is_empty())
        .map(PathBuf::from);
    let backend: Box<dyn server::Backend> =
        match bind(a.session.as_deref(), env_session.as_deref(), env_socket)? {
            Binding::Socket(path) => Box::new(Socket { path }),
            Binding::Direct(session) => {
                let data = DataDir::from_env();
                // Fail now rather than on every tool call.
                let id = Session::open(&data, &session)?.record().id.clone();
                Box::new(Direct { data, session: id })
            }
        };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    server::serve(backend.as_ref(), stdin.lock(), &mut stdout)?;
    Ok(exit::OK)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binding_prefers_the_cell_and_never_serves_another_session() {
        let sock = || Some(PathBuf::from("/run/i.sock"));
        assert_eq!(
            bind(None, Some("01abc"), sock()).ok(),
            Some(Binding::Socket(PathBuf::from("/run/i.sock")))
        );
        assert!(bind(Some("01ab"), Some("01abc"), sock()).is_ok());
        assert!(bind(Some("01zzz"), Some("01abc"), sock()).is_err());
        assert_eq!(
            bind(Some("01x"), None, None).ok(),
            Some(Binding::Direct("01x".into()))
        );
        assert_eq!(
            bind(None, Some("01abc"), None).ok(),
            Some(Binding::Direct("01abc".into()))
        );
        assert!(bind(None, None, None).is_err());
    }
}
