//! `wyrd` — the Wyrdline command-line interface.

mod cmd;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Exit codes shared by every command (plan §14.2).
pub mod exit {
    pub const OK: u8 = 0;
    pub const USAGE: u8 = 1;
    pub const CAPABILITY_MISSING: u8 = 2;
}

/// Causal flight recorder and rewind engine for AI coding agents.
#[derive(Debug, Parser)]
#[command(name = "wyrd", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Report which kernel capabilities Wyrdline can use on this machine.
    Doctor(cmd::doctor::Args),
    /// Internal: print the Landlock ABI version (runs in a throwaway child).
    #[command(name = "__probe-landlock", hide = true)]
    ProbeLandlock,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Doctor(args) => cmd::doctor::run(&args),
        Command::ProbeLandlock => cmd::doctor::probe_landlock(),
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("wyrd: {err:#}");
            ExitCode::from(exit::USAGE)
        }
    }
}
