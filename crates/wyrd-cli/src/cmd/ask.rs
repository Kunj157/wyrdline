//! `wyrd ask`: answer the questions a running session's policy asks.

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{Context as _, bail};
use wyrd_adapters::asks::PendingAsk;
use wyrd_adapters::protocol::call;
use wyrd_adapters::{Request, Response, server};
use wyrd_cell::{DataDir, Session};

use crate::cmd::intent::CONTROL_TOKEN_FILE;
use crate::exit;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Session id or unique prefix.
    session: String,
    /// Allow this pending ask.
    #[arg(long, value_name = "ID", conflicts_with = "deny")]
    allow: Option<u64>,
    /// Deny this pending ask.
    #[arg(long, value_name = "ID")]
    deny: Option<u64>,
    /// List the pending asks as JSON and exit.
    #[arg(long)]
    json: bool,
}

/// How a running session's asks are reached.
#[derive(Debug, Clone)]
pub struct Control {
    socket: PathBuf,
    token: String,
}

const TIMEOUT: Duration = Duration::from_secs(5);

impl Control {
    pub fn open(data: &DataDir, session: &str) -> anyhow::Result<Self> {
        let s = Session::open(data, session)?;
        let id = s.record().id.clone();
        let token_file = s.dir().root().join(CONTROL_TOKEN_FILE);
        let token = std::fs::read_to_string(&token_file)
            .with_context(|| format!("session {id} is not running (no {CONTROL_TOKEN_FILE})"))?;
        Ok(Control {
            socket: server::socket_path(s.dir().root(), &id),
            token: token.trim().to_string(),
        })
    }

    pub fn pending(&self) -> anyhow::Result<Vec<PendingAsk>> {
        match call(
            &self.socket,
            &Request::Asks {
                token: self.token.clone(),
            },
            TIMEOUT,
        )? {
            Response::Asks { asks } => Ok(asks),
            Response::Error { message } => bail!("{message}"),
            other => bail!("unexpected answer {other:?}"),
        }
    }

    pub fn answer(&self, id: u64, allow: bool) -> anyhow::Result<()> {
        let req = Request::Answer {
            token: self.token.clone(),
            id,
            allow,
            by: "cli".into(),
        };
        match call(&self.socket, &req, TIMEOUT)? {
            Response::Ok => Ok(()),
            Response::Error { message } => bail!("{message}"),
            other => bail!("unexpected answer {other:?}"),
        }
    }
}

fn describe(a: &PendingAsk) -> String {
    format!(
        "[{}] {} {}\n      {} (policy {}, {} s left)",
        a.id,
        a.tool,
        a.args,
        a.reason,
        a.policy_id,
        a.expires_in_ms / 1000
    )
}

pub fn run(a: &Args) -> anyhow::Result<u8> {
    let c = Control::open(&DataDir::from_env(), &a.session)?;
    if let Some(id) = a.allow.or(a.deny) {
        c.answer(id, a.allow.is_some())?;
        writeln!(
            std::io::stdout(),
            "{} ask {id}",
            if a.allow.is_some() {
                "allowed"
            } else {
                "denied"
            }
        )?;
        return Ok(exit::OK);
    }
    let pending = c.pending()?;
    if a.json {
        let mut o = std::io::stdout().lock();
        serde_json::to_writer_pretty(&mut o, &pending)?;
        writeln!(o)?;
        return Ok(exit::OK);
    }
    if !std::io::stdin().is_terminal() {
        for p in &pending {
            writeln!(std::io::stdout(), "{}", describe(p))?;
        }
        return Ok(exit::OK);
    }
    // Interactive: answer asks as they arrive until the session ends.
    writeln!(std::io::stdout(), "waiting for asks (Ctrl-C to stop)…")?;
    let mut seen = std::collections::BTreeSet::new();
    let stdin = std::io::stdin();
    loop {
        let Ok(pending) = c.pending() else {
            writeln!(std::io::stdout(), "session ended")?;
            return Ok(exit::OK);
        };
        for p in pending.into_iter().filter(|p| seen.insert(p.id)) {
            let mut o = std::io::stdout().lock();
            write!(o, "{}\n  allow? [y/N] ", describe(&p))?;
            o.flush()?;
            drop(o);
            let mut line = String::new();
            stdin.lock().read_line(&mut line)?;
            let allow = matches!(line.trim(), "y" | "Y" | "yes");
            if let Err(e) = c.answer(p.id, allow) {
                writeln!(std::io::stderr(), "wyrd: {e}")?;
            }
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}
