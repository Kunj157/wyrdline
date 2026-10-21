//! `wyrd` — the Wyrdline command-line interface.

mod cmd;

use std::process::ExitCode;

use clap::{Parser, Subcommand};

/// Exit codes shared by every command (plan §14.2).
pub mod exit {
    pub const OK: u8 = 0;
    pub const USAGE: u8 = 1;
    pub const CAPABILITY_MISSING: u8 = 2;
    /// The named session does not exist (shares 2 with a missing capability:
    /// what the command needs is not there).
    pub const NOT_FOUND: u8 = 2;
    pub const VERIFICATION_FAILED: u8 = 3;
    pub const CONFLICT: u8 = 4;
    pub const DAEMON_UNAVAILABLE: u8 = 5;
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
    /// Verify a session ledger's hash chain and seal signature.
    Verify(cmd::verify::Args),
    /// Run an agent inside a Cell and record the session.
    Run(cmd::session::RunArgs),
    /// List recorded sessions.
    #[command(visible_alias = "ls")]
    Sessions(cmd::session::ListArgs),
    /// Show one session: record, ledger head, verification, checkpoints.
    Show(cmd::inspect::ShowArgs),
    /// Follow a session's ledger until it closes.
    Tail(cmd::inspect::TailArgs),
    /// Print a session's ledger events.
    Events(cmd::session::EventsArgs),
    /// Show which tool call caused which kernel effect.
    Graph(cmd::graph::GraphArgs),
    /// Explain one effect: its process chain and attributed call.
    Explain(cmd::graph::ExplainArgs),
    /// Checkpoint the current branch of a session.
    Checkpoint(cmd::session::SessionArg),
    /// List a session's checkpoints and branches.
    Checkpoints(cmd::session::ListCheckpointsArgs),
    /// Rewind a session to a checkpoint (creates a new branch).
    Rewind(cmd::session::RewindArgs),
    /// Fork a named branch from a checkpoint.
    Fork(cmd::session::ForkArgs),
    /// Show changes between two checkpoints or branches.
    Diff(cmd::session::DiffArgs),
    /// Apply a checkpoint or branch to the real workspace.
    Apply(cmd::session::ApplyArgs),
    /// List a session's divergences: claims and effects that disagree.
    Divergences(cmd::verdict::DivergencesArgs),
    /// Acknowledge a divergence or mark it a false positive.
    Triage(cmd::verdict::TriageArgs),
    /// Stop a running session's agent.
    Stop(cmd::session::SessionArg),
    /// Close a session and seal its ledger.
    Close(cmd::session::SessionArg),
    /// Export a session as a signed, redacted `.wyrd` bundle.
    Export(cmd::bundle::ExportArgs),
    /// Upload a bundle to a Wyrdline Hub (token in `$WYRD_HUB_TOKEN`).
    Push(cmd::push::PushArgs),
    /// Open Studio in a browser (served by the running daemon).
    Studio(cmd::studio::Args),
    /// Manage the daemon token.
    #[command(subcommand)]
    Token(cmd::daemon::TokenCmd),
    /// Talk to the daemon.
    #[command(subcommand)]
    Daemon(cmd::daemon::DaemonCmd),
    /// Forward an agent hook payload (stdin) to the running session.
    Hook(cmd::hook::Args),
    /// Answer the questions a running session's policy asks.
    Ask(cmd::ask::Args),
    /// Build and test Wasm policies.
    #[command(subcommand)]
    Policy(cmd::policy::PolicyCmd),
    /// Proxy a stdio MCP server, recording and checking its traffic.
    McpProxy(cmd::mcp_proxy::Args),
    /// Internal: stage two of a Cell launch.
    #[command(name = "__cell-exec", hide = true)]
    CellExec { spec: std::path::PathBuf },
    /// Internal: remove overlay's root-owned work directory (runs as root).
    #[command(name = "__cell-cleanup", hide = true)]
    CellCleanup { work: std::path::PathBuf },
    /// Internal: check whether a user namespace can be created.
    #[command(name = "__probe-userns", hide = true)]
    ProbeUserns,
    /// Internal: print the Landlock ABI version (runs in a throwaway child).
    #[command(name = "__probe-landlock", hide = true)]
    ProbeLandlock,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Doctor(args) => cmd::doctor::run(&args),
        Command::Verify(args) => cmd::verify::run(&args),
        Command::ProbeLandlock => cmd::doctor::probe_landlock(),
        Command::Run(a) => cmd::session::run(&a),
        Command::Sessions(a) => cmd::session::list(&a),
        Command::Events(a) => cmd::session::events(&a),
        Command::Show(a) => cmd::inspect::show(&a),
        Command::Tail(a) => cmd::inspect::tail(&a),
        Command::Studio(a) => cmd::studio::run(&a),
        Command::Token(c) => cmd::daemon::token(&c),
        Command::Daemon(c) => cmd::daemon::daemon(&c),
        Command::Graph(a) => cmd::graph::graph(&a),
        Command::Explain(a) => cmd::graph::explain(&a),
        Command::Checkpoint(a) => cmd::session::checkpoint(&a),
        Command::Checkpoints(a) => cmd::session::checkpoints(&a),
        Command::Rewind(a) => cmd::session::rewind(&a),
        Command::Fork(a) => cmd::session::fork(&a),
        Command::Diff(a) => cmd::session::diff(&a),
        Command::Apply(a) => cmd::session::apply(&a),
        Command::Divergences(a) => cmd::verdict::list(&a),
        Command::Triage(a) => cmd::verdict::triage(&a),
        Command::Stop(a) => cmd::session::stop(&a),
        Command::Close(a) => cmd::session::close(&a),
        Command::Export(a) => cmd::bundle::export(&a),
        Command::Push(a) => cmd::push::push(&a),
        Command::Hook(a) => cmd::hook::run(&a),
        Command::McpProxy(a) => cmd::mcp_proxy::run(&a),
        Command::Policy(c) => cmd::policy::run(&c),
        Command::Ask(a) => cmd::ask::run(&a),
        Command::CellExec { spec } => cmd::session::cell_exec(&spec),
        Command::CellCleanup { work } => wyrd_cell::exec::cleanup_workdir(&work)
            .map(|()| exit::OK)
            .map_err(Into::into),
        Command::ProbeUserns => Ok(match wyrd_cell::launch::probe_userns_in_process() {
            Ok(()) => exit::OK,
            Err(_) => exit::CAPABILITY_MISSING,
        }),
    };
    match result {
        Ok(code) => ExitCode::from(code),
        Err(err) => {
            eprintln!("wyrd: {err:#}");
            ExitCode::from(exit::USAGE)
        }
    }
}
