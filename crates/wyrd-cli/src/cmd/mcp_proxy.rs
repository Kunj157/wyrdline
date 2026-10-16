//! `wyrd mcp-proxy --server <name> -- <command…>`: the MCP gateway for a
//! stdio server, or `--remote <file>` for a Streamable HTTP server.

use std::path::PathBuf;

use wyrd_adapters::hook::mode_from_env;
use wyrd_gateway::{Gateway, SocketReporter, run_stdio};

/// Longest wait for one remote answer (a tool call may stream for a while).
const REMOTE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Server name recorded in the ledger (as in the agent's MCP config).
    #[arg(long)]
    server: String,
    /// A Streamable HTTP server: JSON file with `url` and `headers`.
    #[arg(long, conflicts_with = "command")]
    remote: Option<PathBuf>,
    /// The real MCP server command, after `--`.
    #[arg(last = true, required_unless_present = "remote")]
    command: Vec<String>,
}

pub fn run(a: &Args) -> anyhow::Result<u8> {
    let socket = std::env::var_os(wyrd_adapters::SOCKET_ENV).map(PathBuf::from);
    let gateway = Gateway {
        server: a.server.clone(),
        mode: mode_from_env(std::env::var(wyrd_adapters::MODE_ENV).ok().as_deref()),
        reporter: socket.map(|socket| SocketReporter { socket }),
    };
    if let Some(file) = &a.remote {
        let remote = wyrd_gateway::remote::Remote::load(file)
            .map_err(|e| anyhow::anyhow!("reading {}: {e}", file.display()))?;
        let http = wyrd_gateway::remote::Http::new(remote, REMOTE_TIMEOUT);
        wyrd_gateway::remote::run_remote(
            &gateway,
            &http,
            std::io::stdin(),
            Box::new(std::io::stdout()),
        )?;
        return Ok(0);
    }
    let code = run_stdio(
        gateway,
        &a.command,
        std::io::stdin(),
        Box::new(std::io::stdout()),
    )?;
    Ok(u8::try_from(code.clamp(0, 255)).unwrap_or(1))
}
