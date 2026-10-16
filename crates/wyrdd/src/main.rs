//! `wyrdd` binary: bind the unix socket and loopback TCP, then serve.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::Context as _;
use clap::Parser;
use wyrd_cell::DataDir;
use wyrdd::{AppState, auth, listen::SameUidListener};

/// Per-user Wyrdline daemon.
#[derive(Debug, Parser)]
#[command(name = "wyrdd", version, about)]
struct Args {
    /// Unix socket (default `$WYRD_DAEMON_SOCKET`, else
    /// `$XDG_RUNTIME_DIR/wyrdline/wyrdd.sock`).
    #[arg(long)]
    socket: Option<PathBuf>,
    /// Loopback address for the TCP listener.
    #[arg(long, default_value = wyrdd::DEFAULT_LISTEN)]
    listen: String,
    /// Serve the unix socket only.
    #[arg(long)]
    no_tcp: bool,
    /// Built Studio to serve on loopback (default `$WYRD_STUDIO_DIR`, else
    /// `share/wyrdline/studio` next to the installed binary).
    #[arg(long)]
    studio: Option<PathBuf>,
    /// Bearer token file (default `$XDG_STATE_HOME/wyrdline/token`).
    #[arg(long)]
    token_file: Option<PathBuf>,
}

/// Where the built Studio is, if anywhere.
fn studio_dir(arg: Option<PathBuf>) -> Option<PathBuf> {
    arg.or_else(|| std::env::var_os("WYRD_STUDIO_DIR").map(PathBuf::from))
        .or_else(|| {
            let exe = std::env::current_exe().ok()?;
            Some(exe.parent()?.parent()?.join("share/wyrdline/studio"))
        })
}

/// `wyrd` next to this binary, else whatever `PATH` finds.
fn wyrd_bin() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|e| e.parent().map(|d| d.join("wyrd")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("wyrd"))
}

/// SIGINT or SIGTERM. Returning drops the listeners, which removes the
/// socket file.
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};
    match signal(SignalKind::terminate()) {
        Ok(mut term) => {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => {}
                _ = term.recv() => {}
            }
        }
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

async fn serve(args: Args) -> anyhow::Result<()> {
    let state = Arc::new(
        AppState::new(DataDir::from_env(), wyrd_bin()).with_studio(studio_dir(args.studio.clone())),
    );
    if let Some(dir) = &state.studio {
        tracing::info!(studio = %dir.display(), "serving Studio");
    } else {
        tracing::info!("no Studio build found; serving the API only");
    }
    let socket = args
        .socket
        .unwrap_or_else(wyrd_adapters::daemon::default_socket);
    let unix =
        SameUidListener::bind(&socket).with_context(|| format!("binding {}", socket.display()))?;
    tracing::info!(socket = %socket.display(), "listening on unix socket");
    let unix_task = tokio::spawn(axum::serve(unix, wyrdd::unix_app(state.clone())).into_future());

    if args.no_tcp {
        tokio::select! {
            r = unix_task => r??,
            () = shutdown_signal() => tracing::info!("shutting down"),
        }
        return Ok(());
    }
    let addr: std::net::SocketAddr = args.listen.parse().context("--listen")?;
    anyhow::ensure!(
        addr.ip().is_loopback(),
        "--listen must be a loopback address"
    );
    let token_file = args.token_file.unwrap_or_else(auth::default_token_path);
    let tokens = std::sync::Arc::new(
        auth::TokenSource::file(token_file.clone())
            .with_context(|| format!("token file {}", token_file.display()))?,
    );
    let tcp = tokio::net::TcpListener::bind(addr)
        .await
        .with_context(|| format!("binding {addr} (is another wyrdd running?)"))?;
    tracing::info!(%addr, "listening on loopback");
    let cors = wyrdd::cors(
        &args.listen,
        std::env::var("WYRD_STUDIO_ORIGIN").ok().as_deref(),
    );
    let tcp_task =
        tokio::spawn(axum::serve(tcp, wyrdd::tcp_app(state, tokens, cors)).into_future());
    tokio::select! {
        r = unix_task => r??,
        r = tcp_task => r??,
        () = shutdown_signal() => tracing::info!("shutting down"),
    }
    Ok(())
}

fn main() -> ExitCode {
    let args = Args::parse();
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("WYRD_LOG")
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let rt = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            tracing::error!(error = %e, "starting the runtime");
            return ExitCode::FAILURE;
        }
    };
    match rt.block_on(serve(args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = format!("{e:#}"), "wyrdd failed");
            ExitCode::FAILURE
        }
    }
}
