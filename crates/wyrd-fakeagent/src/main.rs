//! `wyrd-fakeagent`: a deterministic scripted coding agent.
//!
//! It reads a `.wyrdscript.toml`, reports every step through the real
//! `wyrd hook claude-code` command (the same payloads a real agent's hooks
//! send), executes the scripted tool calls as real subprocesses and file
//! operations, talks to MCP servers through `wyrd mcp-proxy`, and prints
//! a transcript. It is the backbone of the scenario tests. `serve-mcp`
//! runs a toy MCP server for those scenarios.
#![forbid(unsafe_code)]

mod agent;
mod mcp_client;
mod mcp_server;
mod script;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(name = "wyrd-fakeagent", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Debug, Subcommand)]
enum Cmd {
    /// Play a script in the current directory.
    Run { script: PathBuf },
    /// Serve the toy MCP server on stdin/stdout.
    ServeMcp {
        /// Change the `echo` tool's description after this many listings.
        #[arg(long)]
        rug_pull_after: Option<u32>,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Cmd::Run { script } => agent::run(&script),
        Cmd::ServeMcp { rug_pull_after } => mcp_server::serve(
            std::io::stdin().lock(),
            &mut std::io::stdout().lock(),
            rug_pull_after,
        )
        .map(|()| 0),
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("wyrd-fakeagent: {e:#}");
            ExitCode::from(1)
        }
    }
}
