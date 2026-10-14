//! `wyrd mcp-proxy --server <name> -- <command…>`: the MCP gateway (stdio).

use std::path::PathBuf;

use wyrd_adapters::hook::mode_from_env;
use wyrd_gateway::{Gateway, SocketReporter, run_stdio};

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Server name recorded in the ledger (as in the agent's MCP config).
    #[arg(long)]
    server: String,
    /// The real MCP server command, after `--`.
    #[arg(last = true, required = true)]
    command: Vec<String>,
}

pub fn run(a: &Args) -> anyhow::Result<u8> {
    let socket = std::env::var_os(wyrd_adapters::SOCKET_ENV).map(PathBuf::from);
    let gateway = Gateway {
        server: a.server.clone(),
        mode: mode_from_env(std::env::var(wyrd_adapters::MODE_ENV).ok().as_deref()),
        reporter: socket.map(|socket| SocketReporter { socket }),
    };
    let code = run_stdio(
        gateway,
        &a.command,
        std::io::stdin(),
        Box::new(std::io::stdout()),
    )?;
    Ok(u8::try_from(code.clamp(0, 255)).unwrap_or(1))
}
