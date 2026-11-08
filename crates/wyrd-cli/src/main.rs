//! `wyrd` — the Wyrdline command-line interface.
#![forbid(unsafe_code)]

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
    /// Standard output was closed by its reader (`wyrd events … | head`).
    /// 128 + SIGPIPE, the status a shell reports for a filter stopped by a
    /// closed pipe. Nothing is printed for it.
    pub const BROKEN_PIPE: u8 = 141;
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
    /// Verify a session ledger (chain and seal) or a `.wyrd` bundle.
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
    /// Verify a `.wyrd` bundle and add it as a read-only session.
    Import(cmd::bundle::ImportArgs),
    /// Open Studio in a browser (served by the running daemon).
    Studio(cmd::studio::Args),
    /// Open a `.wyrd` bundle in Studio, without the daemon.
    Open(cmd::open::Args),
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
    /// Serve a session's inspection tools and rewind requests over stdio
    /// MCP (for the agent; rewinds still need a human).
    Mcp(cmd::mcp::Args),
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
        Command::Open(a) => cmd::open::run(&a),
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
        Command::Import(a) => cmd::bundle::import(&a),
        Command::Hook(a) => cmd::hook::run(&a),
        Command::McpProxy(a) => cmd::mcp_proxy::run(&a),
        Command::Mcp(a) => cmd::mcp::run(&a),
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
        Err(err) if stdout_reader_gone(&err) => ExitCode::from(exit::BROKEN_PIPE),
        Err(err) => {
            eprintln!("wyrd: {err:#}");
            ExitCode::from(exit::USAGE)
        }
    }
}

/// Whether `err` is a broken pipe and standard output has no reader left. A
/// broken pipe on another channel (a daemon socket, say) is still an error,
/// because stdout is then still open.
fn stdout_reader_gone(err: &anyhow::Error) -> bool {
    let broken_pipe = err.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe)
    });
    broken_pipe && stdout_has_no_reader()
}

fn stdout_has_no_reader() -> bool {
    fd_has_no_reader(&std::io::stdout())
}

/// On Linux, polling a pipe whose read end is closed reports `POLLERR`.
fn fd_has_no_reader(fd: &impl std::os::fd::AsFd) -> bool {
    use rustix::event::{PollFd, PollFlags, poll};
    let mut fds = [PollFd::new(fd, PollFlags::OUT)];
    let now = rustix::event::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    poll(&mut fds, Some(&now)).is_ok() && fds[0].revents().contains(PollFlags::ERR)
}

#[cfg(test)]
mod tests {
    use super::fd_has_no_reader;

    #[test]
    #[allow(clippy::unwrap_used)]
    fn a_pipe_is_reported_once_its_reader_is_gone() {
        let (reader, writer) = std::io::pipe().unwrap();
        assert!(!fd_has_no_reader(&writer), "a reader is still open");
        drop(reader);
        assert!(fd_has_no_reader(&writer), "no reader is left");
    }
}
