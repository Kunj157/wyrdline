//! Session commands: run, sessions, checkpoint(s), rewind, fork, diff, apply, stop, close.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context as _;
use wyrd_cell::session::{CreateOptions, RunOptions};
use wyrd_cell::{CellError, DataDir, Session};

use crate::cmd::intent;
use crate::exit;

#[derive(Debug, clap::Args)]
pub struct RunArgs {
    /// Record only: no sandbox, checkpoints, or rewind.
    #[arg(long)]
    observe: bool,
    /// Human-readable session name.
    #[arg(long)]
    name: Option<String>,
    /// Workspace to sandbox (default: current directory).
    #[arg(long)]
    workspace: Option<PathBuf>,
    /// Agent adapter (`claude-code`, `fakeagent`, `generic`); detected
    /// from the program name when omitted.
    #[arg(long)]
    agent: Option<String>,
    /// Deny a tool before it runs (exact name, or a prefix ending in `*`).
    #[arg(long = "deny-tool", value_name = "TOOL")]
    deny_tools: Vec<String>,
    /// Wasm policy component deciding tool calls (`policies/default.wasm`).
    #[arg(long, value_name = "FILE")]
    policy: Option<PathBuf>,
    /// Seconds a tool call waits for an answer when the policy asks
    /// (`wyrd ask`, Studio) before it is denied.
    #[arg(long, value_name = "SECS", default_value_t = 30)]
    ask_timeout: u64,
    /// Agent command, after `--`.
    #[arg(last = true, required = true)]
    command: Vec<String>,
}

#[derive(Debug, clap::Args)]
pub struct ListArgs {
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
pub struct EventsArgs {
    /// Session id or unique prefix.
    session: String,
    /// Only this plane (meta, intent, effect, state, guard, verdict).
    #[arg(long)]
    plane: Option<String>,
    /// One JSON event per line.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
pub struct SessionArg {
    /// Session id or unique prefix.
    session: String,
}

#[derive(Debug, clap::Args)]
pub struct ListCheckpointsArgs {
    session: String,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
pub struct RewindArgs {
    session: String,
    /// Checkpoint id (`c3`), branch name, or `head`.
    #[arg(required_unless_present_any = ["requests", "confirm", "reject"])]
    target: Option<String>,
    /// Start the agent again on the new branch.
    #[arg(long, conflicts_with_all = ["requests", "reject"])]
    relaunch: bool,
    /// List the rewind requests the agent made (`wyrd mcp`).
    #[arg(long, conflicts_with_all = ["target", "confirm", "reject"])]
    requests: bool,
    /// Confirm the agent's rewind request with this id: rewind to the
    /// checkpoint it names.
    #[arg(long, value_name = "REQUEST", conflicts_with_all = ["target", "reject"])]
    confirm: Option<u64>,
    /// Reject the agent's rewind request with this id.
    #[arg(long, value_name = "REQUEST", conflicts_with = "target")]
    reject: Option<u64>,
    /// Why a request is rejected (stored in the ledger).
    #[arg(long, requires = "reject")]
    note: Option<String>,
    /// JSON output (with `--requests`).
    #[arg(long, requires = "requests")]
    json: bool,
}

#[derive(Debug, clap::Args)]
pub struct ForkArgs {
    session: String,
    target: String,
    /// Name of the new branch.
    #[arg(long)]
    name: String,
    #[arg(long)]
    relaunch: bool,
}

#[derive(Debug, clap::Args)]
pub struct DiffArgs {
    session: String,
    a: String,
    b: String,
    #[arg(long)]
    json: bool,
}

#[derive(Debug, clap::Args)]
pub struct ApplyArgs {
    session: String,
    target: String,
    /// Only list what would change.
    #[arg(long)]
    dry_run: bool,
    /// Apply even over files changed in the real workspace.
    #[arg(long)]
    force: bool,
    #[arg(long)]
    json: bool,
}

fn data() -> DataDir {
    DataDir::from_env()
}

fn out() -> std::io::StdoutLock<'static> {
    std::io::stdout().lock()
}

fn helper() -> anyhow::Result<PathBuf> {
    std::env::current_exe().context("locating the wyrd binary")
}

fn probe_userns(helper: &Path) -> impl FnOnce() -> Result<(), String> + '_ {
    move || match Command::new(helper).arg("__probe-userns").status() {
        Ok(s) if s.success() => Ok(()),
        Ok(_) => Err(read_userns_reason()),
        Err(e) => Err(e.to_string()),
    }
}

fn read_userns_reason() -> String {
    match std::fs::read_to_string("/proc/sys/kernel/apparmor_restrict_unprivileged_userns") {
        Ok(v) if v.trim() == "1" => "AppArmor restricts unprivileged user namespaces".into(),
        _ => "the kernel refused to create a user namespace".into(),
    }
}

fn load_policy(path: &Path) -> anyhow::Result<std::sync::Arc<wyrd_policy::Policy>> {
    let p = wyrd_policy::Policy::load_file(path)
        .with_context(|| format!("loading the policy {}", path.display()))?;
    Ok(std::sync::Arc::new(p))
}

/// The policy's kernel rules for this session: what the Cell enforces,
/// recorded as a kernel-stage decision. A policy whose `kernel-rules` fails
/// cannot start a session.
fn kernel_plan(
    s: &Session,
    policy: &wyrd_policy::Policy,
) -> anyhow::Result<(
    wyrd_policy::kernel::Compiled,
    wyrd_cell::confine::Confinement,
)> {
    let rec = s.record();
    let ctx = wyrd_policy::SessionCtx {
        repo_root: rec.workspace.display().to_string(),
        scope_globs: Vec::new(),
        prior_denials: 0,
    };
    let out = policy.kernel_rules(&ctx);
    let Some(rules) = out.value else {
        anyhow::bail!(
            "the policy {} failed to produce kernel rules ({}); not starting an unenforced session",
            policy.id,
            out.error.unwrap_or_default()
        );
    };
    let home = wyrd_cell::launch::home_dir();
    let compiled = wyrd_policy::kernel::compile(&rules, home.as_deref(), &rec.workspace);
    let confine = wyrd_cell::confine::Confinement {
        write_allow: compiled.write_allow.clone(),
        read_deny: compiled.read_deny.clone(),
        tcp_connect_ports: compiled.tcp_connect_ports.clone(),
    };
    let list = |v: &[std::path::PathBuf]| {
        v.iter()
            .map(|p| p.display().to_string())
            .collect::<Vec<_>>()
            .join(", ")
    };
    let expressible_elsewhere = if compiled.not_expressible.is_empty() {
        String::new()
    } else {
        format!(
            "; left to tool-call and effect-time checks: {}",
            compiled.not_expressible.join("; ")
        )
    };
    let reason = format!(
        "kernel rules: no reads of [{}]; extra writes to [{}]; TCP ports {}{expressible_elsewhere}",
        list(&compiled.read_deny),
        list(&compiled.write_allow),
        compiled
            .tcp_connect_ports
            .as_ref()
            .map_or_else(|| "any".to_string(), |p| format!("{p:?}")),
    );
    let mut ledger = wyrd_ledger::Ledger::open(s.dir().ledger())?;
    ledger.append(vec![wyrd_schema::Event {
        seq: 0,
        ts_mono_ns: wyrd_cell::mono_ns(),
        ts_wall_ms: wyrd_cell::now_ms(),
        process: None,
        drops_since_last: 0,
        body: wyrd_schema::EventBody::GuardDecision(wyrd_schema::GuardDecision {
            stage: wyrd_schema::GuardStage::Kernel,
            decision: wyrd_schema::Decision::Allow,
            policy_id: policy.id.clone(),
            policy_version: policy.version.clone(),
            reason,
            call_id: None,
            subject_seq: None,
        }),
    }])?;
    Ok((compiled, confine))
}

/// What the cgroup connect programs enforce for this session, if anything.
fn egress_plan(
    k: &wyrd_policy::kernel::Compiled,
    policy: &wyrd_policy::Policy,
) -> Option<super::effects::EgressPlan> {
    if k.egress_cidrs.is_none() && k.tcp_connect_ports.is_none() {
        return None;
    }
    Some(super::effects::EgressPlan {
        cidrs: k
            .egress_cidrs
            .iter()
            .flatten()
            .map(ToString::to_string)
            .collect(),
        hosts: k.egress_hosts.clone(),
        ports: k.tcp_connect_ports.clone().unwrap_or_default(),
        restrict_hosts: k.egress_cidrs.is_some(),
        policy_id: policy.id.clone(),
        policy_version: policy.version.clone(),
    })
}

/// Why the Cell cannot enforce everything the policy asked for.
fn unenforced(s: &Session, confine: &wyrd_cell::confine::Confinement) -> Vec<String> {
    if s.record().mode == wyrd_schema::CaptureMode::Observe {
        return vec!["observe mode: the policy's kernel rules are not enforced".into()];
    }
    let abi = super::doctor::landlock_abi_from_child().and_then(|a| u32::try_from(a).ok());
    wyrd_cell::confine::unenforced(confine, abi)
}

/// Record divergences and print the run summary.
fn report_finished(s: &mut Session, code: i32) -> anyhow::Result<()> {
    // Divergences are part of the record; a failure to compute them is
    // reported but does not change the agent's exit code.
    let divergences =
        super::graph::load(&s.record().id).and_then(|events| super::verdict::record(s, &events));
    let rec = s.record();
    writeln!(
        out(),
        "wyrd: session {} finished (exit {code}); branch {}, {} checkpoint(s)",
        rec.id,
        rec.current_branch,
        rec.checkpoints.len()
    )?;
    match divergences {
        Ok(0) => {}
        Ok(n) => writeln!(
            out(),
            "wyrd: {n} divergence(s); see `wyrd divergences {}`",
            rec.id
        )?,
        Err(e) => writeln!(
            std::io::stderr(),
            "wyrd: warning: divergences not recorded: {e:#}"
        )?,
    }
    Ok(())
}

fn run_session(
    s: &mut Session,
    policy: Option<std::sync::Arc<wyrd_policy::Policy>>,
    ask_timeout: Option<std::time::Duration>,
) -> anyhow::Result<u8> {
    let helper = helper()?;
    let policy = match (policy, &s.record().policy) {
        (Some(p), _) => Some(p),
        (None, Some(path)) => Some(load_policy(path)?),
        (None, None) => None,
    };
    let kernel = policy.as_deref().map(|p| kernel_plan(s, p)).transpose()?;
    let degraded = kernel
        .as_ref()
        .map(|(_, c)| unenforced(s, c))
        .unwrap_or_default();
    let observe = s.record().mode == wyrd_schema::CaptureMode::Observe;
    let egress = match (&kernel, &policy) {
        (Some((k, _)), Some(p)) if !observe => egress_plan(k, p),
        _ => None,
    };
    let effect_guard = match (&kernel, &policy) {
        (Some((k, _)), Some(p)) => Some(super::effects::EffectGuard {
            policy: Some(p.clone()),
            ctx: wyrd_policy::SessionCtx {
                repo_root: s.record().workspace.display().to_string(),
                scope_globs: Vec::new(),
                prior_denials: 0,
            },
            exec_deny: wyrd_policy::effects::ExecDeny::resolve(
                &k.exec_deny,
                &std::env::var("PATH").unwrap_or_default(),
            ),
            enforce: !observe,
            workspace: s.record().workspace.clone(),
            upper: s.dir().upper(&s.record().current_branch),
            policy_id: p.id.clone(),
            policy_version: p.version.clone(),
        }),
        _ => None,
    };
    let confine = kernel.map(|(_, c)| c).unwrap_or_default();
    let capture = intent::start(
        &data(),
        s,
        &helper,
        intent::GuardOptions {
            policy,
            ask_timeout,
        },
    )?;
    let checkpoints = capture.checkpoints.clone();
    let data_dir = data();
    let ledger = s.dir().ledger();
    let mut effects: Option<super::effects::EffectCapture> = None;
    let opts = RunOptions {
        env: capture.env.clone(),
        command: capture.command.clone(),
        before_final: checkpoints
            .map(|cp| Box::new(move || cp.stop()) as Box<dyn FnOnce() -> Option<String>>),
        on_started: Some(Box::new(|st: &wyrd_cell::session::Started| {
            effects = Some(super::effects::start(
                &data_dir,
                &st.session_id,
                ledger.clone(),
                st.cgroup.as_deref(),
                &st.workspace,
                egress,
                effect_guard,
            ));
            if !degraded.is_empty() {
                let reason = degraded.join("; ");
                let _ = writeln!(std::io::stderr(), "wyrd: warning: {reason}");
                let _ = Session::mark_degraded(&data_dir, &st.session_id, &reason);
            }
        })),
        confine,
    };
    let code = s.run(&helper, probe_userns(&helper), opts);
    if let Some(e) = effects {
        let warning = e.warning.lock().ok().and_then(|w| w.clone());
        e.finish();
        if let Some(w) = warning {
            let _ = writeln!(std::io::stderr(), "wyrd: warning: {w}");
        }
    }
    let code = code?;
    drop(capture);
    report_finished(s, code)?;
    Ok(u8::try_from(code.clamp(0, 255)).unwrap_or(1))
}

pub fn run(a: &RunArgs) -> anyhow::Result<u8> {
    let workspace = match &a.workspace {
        Some(w) => w.clone(),
        None => std::env::current_dir()?,
    };
    let doctor = wyrd_doctor::run(&wyrd_doctor::Env::new(
        "/",
        wyrd_doctor::Probes {
            landlock_abi: None,
            uid: rustix::process::getuid().as_raw(),
        },
    ));
    let policy_path = a
        .policy
        .as_ref()
        .map(|p| {
            p.canonicalize()
                .with_context(|| format!("policy {}", p.display()))
        })
        .transpose()?;
    let loaded = policy_path.as_deref().map(load_policy).transpose()?;
    let mut s = Session::create(
        &data(),
        CreateOptions {
            workspace,
            command: a.command.clone(),
            observe: a.observe,
            name: a.name.clone(),
            agent_kind: a
                .agent
                .clone()
                .unwrap_or_else(|| intent::detect_agent(&a.command)),
            deny_tools: a.deny_tools.clone(),
            policy: policy_path.zip(loaded.as_ref().map(|l| l.version.clone())),
            kernel_release: doctor.kernel_release.clone(),
            capabilities: doctor.capabilities(),
        },
    )?;
    writeln!(out(), "wyrd: session {} started", s.record().id)?;
    if let Some(p) = &loaded {
        writeln!(out(), "wyrd: policy {} {}", p.id, p.version)?;
    }
    run_session(
        &mut s,
        loaded,
        Some(std::time::Duration::from_secs(a.ask_timeout)),
    )
}

pub fn list(a: &ListArgs) -> anyhow::Result<u8> {
    let sessions = Session::list(&data())?;
    let mut o = out();
    if a.json {
        serde_json::to_writer_pretty(&mut o, &sessions)?;
        writeln!(o)?;
        return Ok(exit::OK);
    }
    for r in sessions {
        writeln!(
            o,
            "{}  {:<10} {:>3} cp  branch {:<4} {}  {}",
            r.id,
            format!("{:?}", r.status).to_lowercase(),
            r.checkpoints.len(),
            r.current_branch,
            r.workspace.display(),
            r.command.join(" ")
        )?;
    }
    Ok(exit::OK)
}

pub fn checkpoint(a: &SessionArg) -> anyhow::Result<u8> {
    let mut s = Session::open(&data(), &a.session)?;
    let c = s.checkpoint(None)?;
    writeln!(
        out(),
        "{} on branch {}: {} ({} files, {} bytes read)",
        c.id,
        c.branch,
        c.manifest,
        c.files,
        c.bytes_read
    )?;
    Ok(exit::OK)
}

pub fn checkpoints(a: &ListCheckpointsArgs) -> anyhow::Result<u8> {
    let s = Session::open(&data(), &a.session)?;
    let r = s.record();
    let mut o = out();
    if a.json {
        serde_json::to_writer_pretty(
            &mut o,
            &serde_json::json!({
                "current_branch": r.current_branch,
                "branches": r.branches,
                "checkpoints": r.checkpoints,
            }),
        )?;
        writeln!(o)?;
        return Ok(exit::OK);
    }
    for b in &r.branches {
        let mark = if b.id == r.current_branch { "*" } else { " " };
        let from = b
            .forked_from
            .as_deref()
            .map(|c| format!(" (from {c})"))
            .unwrap_or_default();
        writeln!(o, "{mark} branch {} [{}]{from}", b.name, b.id)?;
        for c in r.checkpoints.iter().filter(|c| c.branch == b.id) {
            writeln!(
                o,
                "    {:<5} seq {:<6} {} files  {}",
                c.id, c.seq, c.files, c.manifest
            )?;
        }
    }
    Ok(exit::OK)
}

fn rewind_or_fork(
    session: &str,
    target: &str,
    name: Option<&str>,
    relaunch: bool,
) -> anyhow::Result<u8> {
    let mut s = Session::open(&data(), session)?;
    let b = s.rewind(target, name)?;
    writeln!(
        out(),
        "wyrd: now on branch {} (from {})",
        b.name,
        b.forked_from.as_deref().unwrap_or("-")
    )?;
    if relaunch {
        run_session(&mut s, None, None)
    } else {
        Ok(exit::OK)
    }
}

pub fn rewind(a: &RewindArgs) -> anyhow::Result<u8> {
    if a.requests {
        return rewind_requests(&a.session, a.json);
    }
    if let Some(id) = a.reject {
        let mut s = Session::open(&data(), &a.session)?;
        let r = s.reject_rewind(id, "cli", a.note.clone())?;
        writeln!(
            out(),
            "wyrd: rejected rewind request {} (to {}); nothing was rewound",
            r.id,
            r.checkpoint_id
        )?;
        return Ok(exit::OK);
    }
    if let Some(id) = a.confirm {
        let mut s = Session::open(&data(), &a.session)?;
        let b = s.confirm_rewind(id, "cli")?;
        writeln!(
            out(),
            "wyrd: confirmed rewind request {id}; now on branch {} (from {})",
            b.name,
            b.forked_from.as_deref().unwrap_or("-")
        )?;
        return if a.relaunch {
            run_session(&mut s, None, None)
        } else {
            Ok(exit::OK)
        };
    }
    let target = a.target.as_deref().context("a checkpoint is required")?;
    rewind_or_fork(&a.session, target, None, a.relaunch)
}

/// Agent-written text for the terminal: control characters (escape
/// sequences, newlines) become spaces.
fn printable(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

/// `wyrd rewind <session> --requests`: the agent's rewind requests,
/// pending ones marked.
fn rewind_requests(session: &str, json: bool) -> anyhow::Result<u8> {
    let s = Session::open(&data(), session)?;
    let reqs = s.rewind_requests()?;
    let mut o = out();
    if json {
        serde_json::to_writer_pretty(&mut o, &serde_json::json!({ "requests": reqs }))?;
        writeln!(o)?;
        return Ok(exit::OK);
    }
    if reqs.is_empty() {
        writeln!(o, "no rewind requests")?;
    }
    let id = &s.record().id;
    for r in &reqs {
        let status = match r.status {
            wyrd_cell::requests::RequestStatus::Pending => "pending".to_string(),
            wyrd_cell::requests::RequestStatus::Confirmed => {
                format!("confirmed ({})", r.decided_by.as_deref().unwrap_or("-"))
            }
            wyrd_cell::requests::RequestStatus::Rejected => {
                format!("rejected ({})", r.decided_by.as_deref().unwrap_or("-"))
            }
        };
        writeln!(
            o,
            "{:<6} to {:<5} {:<20} via {}: {}",
            r.id,
            r.checkpoint_id,
            status,
            r.source,
            printable(&r.reason)
        )?;
    }
    if reqs
        .iter()
        .any(|r| r.status == wyrd_cell::requests::RequestStatus::Pending)
    {
        writeln!(
            o,
            "decide with `wyrd rewind {id} --confirm <id>` or `--reject <id>`"
        )?;
    }
    Ok(exit::OK)
}

pub fn fork(a: &ForkArgs) -> anyhow::Result<u8> {
    rewind_or_fork(&a.session, &a.target, Some(&a.name), a.relaunch)
}

pub fn diff(a: &DiffArgs) -> anyhow::Result<u8> {
    let s = Session::open(&data(), &a.session)?;
    let changes = s.diff(&a.a, &a.b)?;
    let mut o = out();
    if a.json {
        serde_json::to_writer_pretty(&mut o, &changes)?;
        writeln!(o)?;
    } else {
        for c in &changes {
            let sign = match c.kind {
                wyrd_cell::diff::ChangeKind::Added => "+",
                wyrd_cell::diff::ChangeKind::Removed => "-",
                wyrd_cell::diff::ChangeKind::Modified => "~",
            };
            writeln!(o, "{sign} {:<8} {}", c.entry, c.path)?;
        }
        writeln!(o, "{} change(s)", changes.len())?;
    }
    Ok(exit::OK)
}

pub fn apply(a: &ApplyArgs) -> anyhow::Result<u8> {
    let mut s = Session::open(&data(), &a.session)?;
    let report = match s.apply(&a.target, a.dry_run, a.force) {
        Ok(r) => r,
        Err(CellError::Conflicts(paths)) => {
            let mut e = std::io::stderr().lock();
            writeln!(
                e,
                "wyrd: apply refused; changed in the real workspace since the session started:"
            )?;
            for p in paths {
                writeln!(e, "  {p}")?;
            }
            writeln!(e, "Re-run with --force to overwrite them.")?;
            return Ok(exit::CONFLICT);
        }
        Err(e) => return Err(e.into()),
    };
    let mut o = out();
    if a.json {
        serde_json::to_writer_pretty(&mut o, &report)?;
        writeln!(o)?;
    } else {
        for op in &report.ops {
            writeln!(o, "{}", serde_json::to_string(op)?)?;
        }
        if !report.conflicts.is_empty() {
            writeln!(o, "conflicts: {}", report.conflicts.join(", "))?;
        }
        writeln!(
            o,
            "{}",
            if report.applied {
                "applied"
            } else {
                "dry run: nothing written"
            }
        )?;
    }
    Ok(exit::OK)
}

pub fn stop(a: &SessionArg) -> anyhow::Result<u8> {
    let mut s = Session::open(&data(), &a.session)?;
    let stopped = s.stop()?;
    writeln!(out(), "{}", if stopped { "stopped" } else { "not running" })?;
    Ok(exit::OK)
}

pub fn close(a: &SessionArg) -> anyhow::Result<u8> {
    let mut s = Session::open(&data(), &a.session)?;
    let seal = s.close("closed by user")?;
    writeln!(
        out(),
        "sealed at seq {} (b3:{})",
        seal.head_seq,
        hex::encode(seal.head_hash)
    )?;
    Ok(exit::OK)
}

pub fn cell_exec(spec_path: &Path) -> anyhow::Result<u8> {
    let bytes =
        std::fs::read(spec_path).with_context(|| format!("reading {}", spec_path.display()))?;
    let spec: wyrd_cell::exec::LaunchSpec = serde_json::from_slice(&bytes)?;
    match wyrd_cell::exec::run(&spec) {
        Ok(never) => match never {},
        Err(e) => {
            eprintln!("wyrd: cell: {e}");
            Ok(127)
        }
    }
}

/// One event per line: JSON, or seq, pid, kind, and a payload excerpt.
pub fn print_event(
    o: &mut impl std::io::Write,
    ev: &wyrd_schema::Event,
    json: bool,
) -> anyhow::Result<()> {
    if json {
        serde_json::to_writer(&mut *o, ev)?;
        writeln!(o)?;
        return Ok(());
    }
    let pid = ev.process.map(|p| p.pid.to_string()).unwrap_or_default();
    let payload = serde_json::to_value(&ev.body)?;
    let mut detail = payload["payload"].to_string();
    if detail.len() > 140 {
        let mut cut = 140;
        while !detail.is_char_boundary(cut) {
            cut -= 1;
        }
        detail.truncate(cut);
        detail.push('…');
    }
    writeln!(o, "{:>6} {:>7} {:<30} {detail}", ev.seq, pid, ev.kind())?;
    Ok(())
}

pub fn events(a: &EventsArgs) -> anyhow::Result<u8> {
    let s = Session::open(&data(), &a.session)?;
    let ledger = wyrd_ledger::Ledger::open_read_only(s.dir().ledger())?;
    let mut o = out();
    let mut after = 0;
    loop {
        let batch = ledger.events_after(after, 1000)?;
        let Some(last) = batch.last() else { break };
        after = last.seq;
        for ev in batch {
            if a.plane.as_deref().is_some_and(|p| p != ev.plane().as_str()) {
                continue;
            }
            print_event(&mut o, &ev, a.json)?;
        }
    }
    Ok(exit::OK)
}
