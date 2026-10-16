//! `wyrd studio`: open Studio, served by the running daemon.

use std::io::Write as _;
use std::process::{Command, Stdio};

use wyrd_adapters::daemon;

use crate::exit;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Print the URL (it carries the daemon token) instead of opening a browser.
    #[arg(long)]
    print: bool,
}

/// The Studio URL. The token travels in the fragment, which browsers never
/// send to a server; Studio moves it into session storage and drops it
/// from the address bar.
pub fn url(listen: &str, token: &str) -> String {
    format!("http://{listen}/#token={token}")
}

pub fn run(a: &Args) -> anyhow::Result<u8> {
    let mut out = std::io::stdout().lock();
    let status = super::daemon::daemon(&super::daemon::DaemonCmd::Status {
        socket: None,
        json: false,
    });
    if !matches!(status, Ok(exit::OK)) {
        writeln!(out, "wyrd: start the daemon first: wyrdd")?;
        return Ok(exit::DAEMON_UNAVAILABLE);
    }
    let token = std::fs::read_to_string(daemon::default_token_path())?;
    let listen =
        std::env::var("WYRD_DAEMON_LISTEN").unwrap_or_else(|_| daemon::DEFAULT_LISTEN.into());
    let url = url(&listen, token.trim());
    if !a.print {
        let opened = Command::new("xdg-open")
            .arg(&url)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success());
        if opened {
            writeln!(out, "wyrd: opened Studio at http://{listen}/")?;
            return Ok(exit::OK);
        }
        writeln!(out, "wyrd: could not open a browser; open this URL:")?;
    }
    writeln!(out, "{url}")?;
    Ok(exit::OK)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_token_rides_in_the_fragment() {
        assert_eq!(
            super::url("127.0.0.1:7717", "abc"),
            "http://127.0.0.1:7717/#token=abc"
        );
    }
}
