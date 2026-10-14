//! `wyrd hook <adapter>`: run by an agent's hook configuration inside the Cell.

use std::io::{Read as _, Write as _};
use std::path::PathBuf;

use anyhow::bail;
use wyrd_adapters::{Adapter, hook, protocol::MAX_LINE};

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Adapter whose hook format is on stdin (`claude-code`).
    adapter: String,
}

pub fn run(a: &Args) -> anyhow::Result<u8> {
    let Some(adapter) = Adapter::from_name(&a.adapter) else {
        bail!("unknown hook adapter {}", a.adapter);
    };
    let mut input = String::new();
    std::io::stdin()
        .lock()
        .take(MAX_LINE as u64)
        .read_to_string(&mut input)?;
    let socket = std::env::var_os(wyrd_adapters::SOCKET_ENV).map(PathBuf::from);
    let mode = hook::mode_from_env(std::env::var(wyrd_adapters::MODE_ENV).ok().as_deref());
    let out = hook::run(adapter, &input, socket.as_deref(), mode);
    if let Some(w) = &out.warning {
        eprintln!("wyrd hook: {w}");
    }
    if let Some(s) = &out.stdout {
        writeln!(std::io::stdout().lock(), "{s}")?;
    }
    Ok(out.exit_code)
}
