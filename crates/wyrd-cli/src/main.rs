//! `wyrd` — the Wyrdline command-line interface.

use clap::Parser;

/// Causal flight recorder and rewind engine for AI coding agents.
#[derive(Debug, Parser)]
#[command(name = "wyrd", version, about, long_about = None)]
struct Cli {}

fn main() {
    let _cli = Cli::parse();
}
