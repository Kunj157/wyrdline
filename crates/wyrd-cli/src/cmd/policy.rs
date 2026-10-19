//! `wyrd policy build|test`: build a policy crate into a component and
//! check it against conformance cases.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use anyhow::{Context as _, bail};
use wyrd_policy::{Policy, conformance};

use crate::exit;

#[derive(Debug, clap::Subcommand)]
pub enum PolicyCmd {
    /// Build a policy crate (Rust, `wasm32-wasip2`) into a component.
    Build(BuildArgs),
    /// Run conformance cases against a policy component.
    Test(TestArgs),
}

#[derive(Debug, clap::Args)]
pub struct BuildArgs {
    /// Directory of the policy crate (e.g. `policies/default`).
    dir: PathBuf,
    /// Output file (default: the directory name plus `.wasm`).
    #[arg(short, long)]
    out: Option<PathBuf>,
}

#[derive(Debug, clap::Args)]
pub struct TestArgs {
    /// Policy component (`.wasm`).
    policy: PathBuf,
    /// Cases file (`cases.toml`).
    cases: PathBuf,
}

pub fn run(c: &PolicyCmd) -> anyhow::Result<u8> {
    match c {
        PolicyCmd::Build(a) => build(a),
        PolicyCmd::Test(a) => test(a),
    }
}

/// The `.wasm` artifact cargo reported for the crate at `manifest`.
fn artifact(messages: &str, manifest: &Path) -> Option<PathBuf> {
    messages
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|m| m["reason"] == "compiler-artifact")
        .filter(|m| m["manifest_path"].as_str().map(Path::new) == Some(manifest))
        .flat_map(|m| {
            m["filenames"]
                .as_array()
                .cloned()
                .unwrap_or_default()
                .into_iter()
                .filter_map(|f| f.as_str().map(PathBuf::from))
                .collect::<Vec<_>>()
        })
        .find(|f| f.extension().is_some_and(|e| e == "wasm"))
}

fn build(a: &BuildArgs) -> anyhow::Result<u8> {
    let manifest = a
        .dir
        .join("Cargo.toml")
        .canonicalize()
        .with_context(|| format!("{} is not a policy crate", a.dir.display()))?;
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["build", "--release", "--target", "wasm32-wasip2"])
        .arg("--manifest-path")
        .arg(&manifest)
        .args(["--message-format", "json-render-diagnostics"])
        .stderr(Stdio::inherit())
        .output()
        .context("running cargo")?;
    if !output.status.success() {
        bail!("cargo build failed for {}", a.dir.display());
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let Some(built) = artifact(&stdout, &manifest) else {
        bail!("cargo produced no .wasm for {}", a.dir.display());
    };
    let out = a
        .out
        .clone()
        .unwrap_or_else(|| a.dir.with_extension("wasm"));
    std::fs::copy(&built, &out).with_context(|| format!("writing {}", out.display()))?;
    let policy = Policy::load_file(&out)?;
    let size = std::fs::metadata(&out)?.len();
    writeln!(
        std::io::stdout(),
        "built {} ({}, {size} bytes)",
        out.display(),
        policy.version
    )?;
    Ok(exit::OK)
}

fn test(a: &TestArgs) -> anyhow::Result<u8> {
    let policy = Policy::load_file(&a.policy)?;
    let text = std::fs::read_to_string(&a.cases)
        .with_context(|| format!("reading {}", a.cases.display()))?;
    let cases =
        conformance::parse(&text).map_err(|e| anyhow::anyhow!("{}: {e}", a.cases.display()))?;
    let results = conformance::run(&policy, &cases);
    let mut o = std::io::stdout().lock();
    writeln!(
        o,
        "{} {} against {}",
        policy.id,
        policy.version,
        a.cases.display()
    )?;
    for r in &results {
        writeln!(
            o,
            "  {} {}  →  {}",
            if r.passed { "ok  " } else { "FAIL" },
            r.name,
            r.got
        )?;
    }
    let failed = results.iter().filter(|r| !r.passed).count();
    writeln!(o, "{} passed, {failed} failed", results.len() - failed)?;
    Ok(if failed == 0 {
        exit::OK
    } else {
        exit::VERIFICATION_FAILED
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_wasm_artifact_of_the_crate() {
        let m = Path::new("/p/default/Cargo.toml");
        let msgs = [
            r#"{"reason":"compiler-artifact","manifest_path":"/p/common/Cargo.toml","filenames":["/t/libcommon.rlib"]}"#,
            r#"{"reason":"compiler-artifact","manifest_path":"/p/default/Cargo.toml","filenames":["/t/default.wasm"]}"#,
            r#"{"reason":"build-finished","success":true}"#,
        ]
        .join("\n");
        assert_eq!(artifact(&msgs, m), Some(PathBuf::from("/t/default.wasm")));
        assert_eq!(artifact("", m), None);
    }
}
