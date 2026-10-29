//! Session orchestration: create, run, checkpoint, rewind, fork, diff,
//! apply, close. Every state change is appended to the session ledger.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use wyrd_cas::{Cas, ChunkHash, Manifest, Staged};
use wyrd_ledger::{HostKey, Ledger, Seal};
use wyrd_schema::{
    CaptureMode, CheckpointApplied, CheckpointCreated, CheckpointForked, CheckpointRewound, Event,
    EventBody, RewindConfirmed, RewindRejected, RewindRequested, SessionClosed, SessionStarted,
    SessionState, SessionStateChanged,
};

use crate::apply::{self, ApplyReport};
use crate::baseline::Baseline;
use crate::cgroup::Cgroup;
use crate::diff::{self, Change};
use crate::exec::{LaunchMode, LaunchSpec};
use crate::requests::{self, RequestStatus, RewindRequest};
use crate::state::{self, Branch, CheckpointRecord, SessionRecord};
use crate::{CellError, DataDir, Result, SessionDir, confine, io_err, launch, mono_ns, now_ms};

/// Options for [`Session::create`].
#[derive(Debug, Clone)]
pub struct CreateOptions {
    pub workspace: PathBuf,
    pub command: Vec<String>,
    pub observe: bool,
    pub name: Option<String>,
    pub agent_kind: String,
    pub deny_tools: Vec<String>,
    /// Wasm policy component and its version (`b3:…`).
    pub policy: Option<(PathBuf, String)>,
    pub kernel_release: String,
    pub capabilities: BTreeMap<String, bool>,
}

/// Per-run additions from the caller (intent capture).
#[derive(Default)]
pub struct RunOptions<'a> {
    /// Extra environment for the agent, e.g. the intent socket path.
    pub env: Vec<(String, String)>,
    /// Command to execute instead of the recorded one (e.g. with injected
    /// hook settings); the ledger keeps the user's command.
    pub command: Option<Vec<String>>,
    /// Called after the agent exits and before the final checkpoint; returns
    /// the tool call the final checkpoint should be attributed to. Runs
    /// without the session lock so in-flight checkpoints can finish.
    pub before_final: Option<Box<dyn FnOnce() -> Option<String> + 'a>>,
    /// Called once the agent's cgroup exists and before the agent may run
    /// (it is held at the start gate), e.g. to register with the sensor.
    pub on_started: Option<OnStarted<'a>>,
    /// Policy additions to the Cell's Landlock rules: extra writable paths,
    /// paths the agent may not read, TCP ports it may connect to. Wyrdline's
    /// own data and state directories are always unreadable in a Cell.
    pub confine: confine::Confinement,
}

/// Callback run when a Cell is launched and held at its start gate.
pub type OnStarted<'a> = Box<dyn FnOnce(&Started) + 'a>;

/// What [`RunOptions::on_started`] learns about a launched Cell.
#[derive(Debug, Clone)]
pub struct Started {
    pub session_id: String,
    pub cgroup: Option<PathBuf>,
    pub workspace: PathBuf,
}

impl std::fmt::Debug for RunOptions<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RunOptions")
            .field("env", &self.env.len())
            .field("command", &self.command)
            .finish_non_exhaustive()
    }
}

/// Where [`Session::file_at`] found a path's content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, schemars::JsonSchema, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum FileSource {
    /// The checkpoint's upper layer (the agent changed it).
    Checkpoint,
    /// The real workspace below the Cell (unchanged in the Cell).
    Workspace,
    /// The path does not exist at that checkpoint.
    Absent,
}

/// A path's content at a checkpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileAt {
    pub source: FileSource,
    pub bytes: Option<Vec<u8>>,
    /// More than the requested limit exists.
    pub truncated: bool,
}

impl FileAt {
    fn absent() -> Self {
        FileAt {
            source: FileSource::Absent,
            bytes: None,
            truncated: false,
        }
    }

    fn bytes(mut bytes: Vec<u8>, limit: usize, source: FileSource) -> Self {
        let truncated = bytes.len() > limit;
        bytes.truncate(limit);
        FileAt {
            source,
            bytes: Some(bytes),
            truncated,
        }
    }
}

/// An open session.
#[derive(Debug)]
pub struct Session {
    data: DataDir,
    dir: SessionDir,
    rec: SessionRecord,
    cas: Cas,
}

fn event(body: EventBody) -> Event {
    Event {
        seq: 0,
        ts_mono_ns: mono_ns(),
        ts_wall_ms: now_ms(),
        process: None,
        drops_since_last: 0,
        body,
    }
}

/// Create a directory and its missing parents, each new entry synced in its
/// parent before anything is written under it.
fn mkdirs(p: &Path) -> Result<()> {
    Ok(wyrd_cas::create_dirs_durable(p)?)
}

impl Session {
    /// Create a session directory, ledger, stat baseline, and branch `a`.
    pub fn create(data: &DataDir, opts: CreateOptions) -> Result<Session> {
        let workspace = fs::canonicalize(&opts.workspace).map_err(io_err(&opts.workspace))?;
        if !workspace.is_dir() {
            return Err(CellError::Invalid(format!(
                "{} is not a directory",
                workspace.display()
            )));
        }
        mkdirs(&data.sessions())?;
        let data_root = fs::canonicalize(data.root()).map_err(io_err(data.root()))?;
        if data_root.starts_with(&workspace) || workspace.starts_with(&data_root) {
            return Err(CellError::Invalid(format!(
                "the data directory {} and the workspace {} must not contain each other",
                data_root.display(),
                workspace.display()
            )));
        }
        if opts.command.is_empty() {
            return Err(CellError::Invalid("no agent command given".into()));
        }
        let id = ulid::Ulid::generate().to_string().to_lowercase();
        let dir = data.session(&id);
        mkdirs(dir.root())?;
        let mode = if opts.observe {
            CaptureMode::Observe
        } else {
            CaptureMode::Cell
        };
        if mode == CaptureMode::Cell {
            Baseline::capture(&workspace)?.save(&dir.baseline())?;
            mkdirs(&dir.upper("a"))?;
            mkdirs(&dir.work("a"))?;
        }
        let rec = SessionRecord {
            id: id.clone(),
            name: opts.name,
            workspace: workspace.clone(),
            command: opts.command.clone(),
            mode,
            created_ms: now_ms(),
            status: SessionState::Preparing,
            current_branch: "a".into(),
            branches: vec![Branch {
                id: "a".into(),
                name: "a".into(),
                forked_from: None,
                created_ms: now_ms(),
            }],
            checkpoints: Vec::new(),
            cgroup: None,
            pid: None,
            runs: 0,
            agent: opts.agent_kind.clone(),
            deny_tools: opts.deny_tools,
            policy: opts.policy.as_ref().map(|(p, _)| p.clone()),
        };
        state::save(&dir, &rec)?;
        let mut ledger = Ledger::create(dir.ledger(), &id)?;
        // The database file's name is an entry in the session directory.
        wyrd_cas::sync_dir(dir.root())?;
        ledger.append(vec![event(EventBody::SessionStarted(SessionStarted {
            session_id: id,
            agent_kind: opts.agent_kind,
            agent_version: None,
            command: opts.command,
            workspace_root: workspace.display().to_string(),
            mode,
            kernel_release: opts.kernel_release,
            capabilities: opts.capabilities,
            policy_set_hash: opts.policy.map(|(_, v)| v),
        }))])?;
        let cas = Cas::open(data.cas())?;
        Ok(Session {
            data: data.clone(),
            dir,
            rec,
            cas,
        })
    }

    /// Open a session by id or unique id prefix.
    pub fn open(data: &DataDir, id: &str) -> Result<Session> {
        let mut matches: Vec<String> = Vec::new();
        if let Ok(rd) = fs::read_dir(data.sessions()) {
            for e in rd.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                if name == id {
                    matches = vec![name];
                    break;
                }
                if name.starts_with(id) {
                    matches.push(name);
                }
            }
        }
        let id = match matches.as_slice() {
            [one] => one.clone(),
            [] => return Err(CellError::NotFound(format!("session {id}"))),
            _ => {
                return Err(CellError::Invalid(format!(
                    "session prefix {id} is ambiguous"
                )));
            }
        };
        let dir = data.session(&id);
        let rec = state::load(&dir)?;
        let cas = Cas::open(data.cas())?;
        Ok(Session {
            data: data.clone(),
            dir,
            rec,
            cas,
        })
    }

    /// All sessions, newest first.
    pub fn list(data: &DataDir) -> Result<Vec<SessionRecord>> {
        let mut out = Vec::new();
        if let Ok(rd) = fs::read_dir(data.sessions()) {
            for e in rd.flatten() {
                let dir = data.session(&e.file_name().to_string_lossy());
                if let Ok(rec) = state::load(&dir) {
                    out.push(rec);
                }
            }
        }
        out.sort_by_key(|r| std::cmp::Reverse(r.created_ms));
        Ok(out)
    }

    pub fn record(&self) -> &SessionRecord {
        &self.rec
    }

    pub fn dir(&self) -> &SessionDir {
        &self.dir
    }

    pub fn cas(&self) -> &Cas {
        &self.cas
    }

    fn append(&self, bodies: Vec<EventBody>) -> Result<u64> {
        let mut l = Ledger::open(self.dir.ledger())?;
        Ok(l.append(bodies.into_iter().map(event).collect())?)
    }

    fn reload(&mut self) -> Result<()> {
        self.rec = state::load(&self.dir)?;
        Ok(())
    }

    fn set_status(&mut self, to: SessionState, reason: Option<&str>) -> Result<()> {
        let from = self.rec.status;
        if from == to {
            return Ok(());
        }
        self.rec.status = to;
        state::save(&self.dir, &self.rec)?;
        self.append(vec![EventBody::SessionStateChanged(SessionStateChanged {
            from,
            to,
            reason: reason.map(str::to_string),
        })])?;
        Ok(())
    }

    fn running_cgroup(&self) -> Option<Cgroup> {
        self.rec
            .cgroup
            .as_ref()
            .map(Cgroup::at)
            .filter(|c| c.exists() && c.is_populated())
    }

    pub fn is_running(&self) -> bool {
        self.running_cgroup().is_some()
    }

    /// Launch the agent on the current branch (stage one).
    fn start(
        &mut self,
        helper: &Path,
        probe_userns: impl FnOnce() -> std::result::Result<(), String>,
        command: Option<Vec<String>>,
        extra_env: Vec<(String, String)>,
        extra: confine::Confinement,
    ) -> Result<launch::Running> {
        let _lock = state::lock(&self.dir.lock())?;
        self.reload()?;
        if self.is_running() {
            return Err(CellError::Invalid("the session is already running".into()));
        }
        let mode = match launch::choose_mode(self.rec.mode == CaptureMode::Observe, probe_userns) {
            Ok(m) => m,
            Err(e) => {
                if self.rec.status == SessionState::Preparing {
                    self.set_status(SessionState::Failed, Some(&e.to_string()))?;
                }
                return Err(e);
            }
        };
        let branch = self.rec.current_branch.clone();
        let workspace = self.rec.workspace.clone();
        let mut spec = LaunchSpec {
            mode: mode.clone(),
            workspace: workspace.clone(),
            upper: self.dir.upper(&branch),
            work: self.dir.work(&branch),
            cgroup_procs: None,
            confine: self.confinement(&workspace, extra),
            cwd: workspace,
            command: command.unwrap_or_else(|| self.rec.command.clone()),
            env: launch::agent_env(&self.rec.id)
                .into_iter()
                .chain(extra_env)
                .collect(),
            start_gate: None,
        };
        if mode != LaunchMode::Observe {
            // A previous run may have left overlay's internal work dir behind.
            let _ = fs::remove_dir(self.dir.work(&branch).join("work"));
            mkdirs(&self.dir.work(&branch))?;
        }
        self.rec.runs += 1;
        // The full id: ULIDs created in the same millisecond share their prefix.
        let unit = format!("wyrd-{}-r{}", self.rec.id, self.rec.runs);
        // Recording starts before the agent does, so everything the
        // agent reports lands after this transition in the ledger.
        let before = self.rec.status;
        self.set_status(SessionState::Recording, Some("agent starting"))?;
        let running = match launch::spawn(helper, &mut spec, &self.dir.spec(), &unit) {
            Ok(r) => r,
            Err(e) => {
                let back = if before == SessionState::Preparing {
                    SessionState::Failed
                } else {
                    SessionState::Paused
                };
                self.set_status(back, Some(&format!("launch failed: {e}")))?;
                return Err(e);
            }
        };
        self.rec.pid = Some(running.child.id());
        self.rec.cgroup = running.cgroup.as_ref().map(|c| c.path().to_path_buf());
        state::save(&self.dir, &self.rec)?;
        Ok(running)
    }

    /// Record that kernel capture is incomplete: Degraded with a reason.
    /// Safe to call from another thread while the agent runs.
    pub fn mark_degraded(data: &DataDir, session_id: &str, reason: &str) -> Result<()> {
        let mut s = Session::open(data, session_id)?;
        let _lock = state::lock(&s.dir.lock())?;
        s.reload()?;
        if matches!(s.rec.status, SessionState::Recording) {
            s.set_status(SessionState::Degraded, Some(reason))?;
        }
        Ok(())
    }

    /// Back from Degraded once capture works again.
    pub fn mark_recovered(data: &DataDir, session_id: &str) -> Result<()> {
        let mut s = Session::open(data, session_id)?;
        let _lock = state::lock(&s.dir.lock())?;
        s.reload()?;
        if s.rec.status == SessionState::Degraded {
            s.set_status(SessionState::Recording, Some("kernel capture resumed"))?;
        }
        Ok(())
    }

    /// Run the agent in the current branch and wait for it to exit. Returns
    /// its exit code. A final checkpoint is taken in Cell mode.
    /// The Cell's Landlock rules: the default write allow-list plus the
    /// policy's, the policy's read denials plus Wyrdline's own directories
    /// (ledgers, checkpoints, control tokens, the daemon token), and the
    /// policy's TCP ports.
    fn confinement(&self, workspace: &Path, extra: confine::Confinement) -> confine::Confinement {
        let mut write_allow =
            confine::default_write_allow(workspace, launch::home_dir().as_deref());
        write_allow.extend(extra.write_allow);
        let mut read_deny = extra.read_deny;
        // Wyrdline's own data: other sessions, the CAS, the host key, and of
        // this session everything but the files the agent is handed (hook
        // settings, MCP config).
        let data = self.data.root();
        read_deny.extend([data.join("cas"), data.join("keys")]);
        if let Ok(dir) = fs::read_dir(self.data.sessions()) {
            read_deny.extend(
                dir.flatten()
                    .map(|e| e.path())
                    .filter(|p| p != self.dir.root()),
            );
        }
        for name in [
            "control.token",
            "ledger.wyrdledger",
            "ledger.wyrdledger-wal",
            "ledger.wyrdledger-shm",
            "baseline.cbor",
            "branches",
        ] {
            read_deny.push(self.dir.root().join(name));
        }
        if let Some(state) = crate::paths::state_dir() {
            read_deny.push(state);
        }
        // Never deny what the agent must read: its own workspace.
        read_deny.retain(|d| !workspace.starts_with(d));
        read_deny.sort();
        read_deny.dedup();
        confine::Confinement {
            write_allow,
            read_deny,
            tcp_connect_ports: extra.tcp_connect_ports,
        }
    }

    pub fn run(
        &mut self,
        helper: &Path,
        probe_userns: impl FnOnce() -> std::result::Result<(), String>,
        opts: RunOptions<'_>,
    ) -> Result<i32> {
        self.writable()?;
        let RunOptions {
            env: extra_env,
            command,
            before_final,
            on_started,
            confine: extra_confine,
        } = opts;
        let mut running = self.start(helper, probe_userns, command, extra_env, extra_confine)?;
        // Register (callback) only once stage two has finished its setup and
        // waits at the gate, so its own syscalls are not recorded as the agent's.
        let at_gate = running.wait_at_gate()?;
        if let (true, Some(cb)) = (at_gate, on_started) {
            cb(&Started {
                session_id: self.rec.id.clone(),
                cgroup: running.cgroup.as_ref().map(|c| c.path().to_path_buf()),
                workspace: self.rec.workspace.clone(),
            });
        }
        if let Err(e) = running.release() {
            let _ = running.child.kill();
            let _ = running.child.wait();
            return Err(e);
        }
        let status = running.child.wait().map_err(io_err(&self.rec.workspace))?;
        if let Some(work) = running.root_workdir.as_deref() {
            // Overlay left a root-owned work/work behind; remove it the same way we got root.
            let cleaned = if nix::unistd::geteuid().is_root() {
                crate::exec::cleanup_workdir(work).is_ok()
            } else {
                std::process::Command::new("sudo")
                    .args(["-n", "--"])
                    .arg(helper)
                    .arg("__cell-cleanup")
                    .arg(work)
                    .status()
                    .is_ok_and(|s| s.success())
            };
            if !cleaned {
                tracing::warn!(path = %work.display(), "could not remove overlay work directory");
            }
        }
        let code = status
            .code()
            .unwrap_or(128 + std::os::unix::process::ExitStatusExt::signal(&status).unwrap_or(0));
        let final_call = before_final.and_then(|f| f());
        let _lock = state::lock(&self.dir.lock())?;
        self.reload()?;
        if let Some(cg) = &running.cgroup {
            // Background processes the agent left behind do not outlive the run.
            let _ = cg.kill();
            if nix::unistd::geteuid().is_root() {
                let _ = cg.remove();
            }
        }
        self.rec.pid = None;
        self.rec.cgroup = None;
        state::save(&self.dir, &self.rec)?;
        if self.rec.mode == CaptureMode::Cell {
            // Without a call to attribute it to, the exit checkpoint is only
            // kept when something changed since the last one.
            let skip_unchanged = final_call.is_none();
            self.checkpoint_inner(final_call, skip_unchanged)?;
        }
        self.set_status(
            SessionState::Paused,
            Some(&format!("agent exited with {code}")),
        )?;
        Ok(code)
    }

    /// Checkpoint the current branch (freezing the Cell while reading).
    pub fn checkpoint(&mut self, after_call: Option<String>) -> Result<CheckpointRecord> {
        self.writable()?;
        let _lock = state::lock(&self.dir.lock())?;
        self.reload()?;
        self.checkpoint_locked(after_call)
    }

    fn checkpoint_locked(&mut self, after_call: Option<String>) -> Result<CheckpointRecord> {
        self.checkpoint_inner(after_call, false)
    }

    fn checkpoint_inner(
        &mut self,
        after_call: Option<String>,
        skip_unchanged: bool,
    ) -> Result<CheckpointRecord> {
        if self.rec.mode != CaptureMode::Cell {
            return Err(CellError::Invalid(
                "observe-mode sessions have no checkpoints".into(),
            ));
        }
        let branch = self.rec.current_branch.clone();
        let parent = self.rec.branch_head(&branch).cloned().or_else(|| {
            // A fresh branch starts from the checkpoint it was forked from.
            self.rec
                .branch(&branch)
                .and_then(|b| b.forked_from.clone())
                .and_then(|c| self.rec.checkpoint(&c).cloned())
        });
        let base = match &parent {
            Some(p) => Some(self.manifest(&p.manifest)?),
            None => None,
        };
        let started = std::time::Instant::now();
        let frozen = self.running_cgroup();
        if let Some(cg) = &frozen {
            cg.freeze()?;
        }
        // The changed chunks and the manifest are staged inside the freeze
        // (no barrier, nothing visible), then committed after the thaw: two
        // barriers and the renames in between (see docs/cas-format.md). The
        // ledger event below is written only after the commit.
        let mut staged = Staged::default();
        let snap = self
            .cas
            .snapshot_staged(&self.dir.upper(&branch), base.as_ref(), &mut staged);
        if let Some(cg) = &frozen {
            cg.thaw()?;
        }
        let (manifest, stats) = snap?;
        let hash = self.cas.stage_manifest(&manifest, &mut staged)?;
        self.cas.commit(staged)?;
        if skip_unchanged
            && let Some(p) = &parent
            && p.manifest == hash.to_string()
            && p.branch == branch
        {
            return Ok(p.clone());
        }
        let id = self.rec.next_checkpoint_id();
        let duration_ms = u32::try_from(started.elapsed().as_millis()).unwrap_or(u32::MAX);
        let seq = self.append(vec![EventBody::CheckpointCreated(CheckpointCreated {
            checkpoint_id: id.clone(),
            branch_id: branch.clone(),
            parent_id: parent.as_ref().map(|p| p.id.clone()),
            after_call_id: after_call,
            manifest: hash.to_string(),
            files_changed: u32::try_from(stats.files_read).unwrap_or(u32::MAX),
            bytes_added: stats.bytes_read,
            duration_ms,
        })])?;
        let rec = CheckpointRecord {
            id,
            branch,
            parent: parent.map(|p| p.id),
            manifest: hash.to_string(),
            seq,
            created_ms: now_ms(),
            files: stats.files,
            bytes_read: stats.bytes_read,
        };
        self.rec.checkpoints.push(rec.clone());
        state::save(&self.dir, &self.rec)?;
        Ok(rec)
    }

    fn manifest(&self, hash: &str) -> Result<Manifest> {
        let h = ChunkHash::from_hex(hash)
            .ok_or_else(|| CellError::Invalid(format!("bad hash {hash}")))?;
        Ok(self.cas.get_manifest(h)?)
    }

    /// Resolve `cN`, a branch id/name (its latest checkpoint), or `head`.
    pub fn resolve(&self, target: &str) -> Result<CheckpointRecord> {
        if let Some(c) = self.rec.checkpoint(target) {
            return Ok(c.clone());
        }
        let branch = if target == "head" {
            self.rec.current_branch.as_str()
        } else {
            target
        };
        if let Some(b) = self.rec.branch(branch) {
            // A branch without its own checkpoints is still at its fork point.
            let head = self.rec.branch_head(&b.id).or_else(|| {
                b.forked_from
                    .as_deref()
                    .and_then(|c| self.rec.checkpoint(c))
            });
            if let Some(c) = head {
                return Ok(c.clone());
            }
        }
        Err(CellError::NotFound(format!(
            "checkpoint or branch {target}"
        )))
    }

    /// Stop a running Cell: freeze, kill, wait until the cgroup is empty.
    pub fn stop(&mut self) -> Result<bool> {
        let Some(cg) = self.running_cgroup() else {
            return Ok(false);
        };
        cg.freeze()?;
        cg.kill()?;
        Ok(true)
    }

    /// Materialise `target` into a new branch and make it current.
    /// `fork_name` = `None` is a rewind (auto-named branch).
    pub fn rewind(&mut self, target: &str, fork_name: Option<&str>) -> Result<Branch> {
        self.writable()?;
        let _lock = state::lock(&self.dir.lock())?;
        self.reload()?;
        self.rewind_locked(target, fork_name)
    }

    /// [`Session::rewind`] with the session lock already held.
    fn rewind_locked(&mut self, target: &str, fork_name: Option<&str>) -> Result<Branch> {
        let cp = self.resolve(target)?;
        if self.stop()? {
            self.set_status(SessionState::Rewinding, Some("stopped the agent to rewind"))?;
        }
        let manifest = self.manifest(&cp.manifest)?;
        let id = self.rec.next_branch_id();
        let upper = self.dir.upper(&id);
        let report = self.cas.materialize(&manifest, &upper)?;
        // The restored files are not durable until this sync. The branch change
        // and the rewind event are written after it, so they never refer to a
        // tree that a crash could leave incomplete.
        wyrd_cas::sync_fs_of(&upper)?;
        for w in &report.whiteouts {
            nix::sys::stat::mknod(
                w,
                nix::sys::stat::SFlag::S_IFCHR,
                nix::sys::stat::Mode::empty(),
                0,
            )
            .map_err(|e| CellError::Sys(format!("whiteout {}: {e}", w.display())))?;
        }
        for d in &report.opaque_dirs {
            rustix::fs::setxattr(
                d,
                "user.overlay.opaque",
                b"y",
                rustix::fs::XattrFlags::empty(),
            )
            .map_err(|e| CellError::Sys(format!("opaque {}: {e}", d.display())))?;
        }
        mkdirs(&self.dir.work(&id))?;
        let name = fork_name.map_or_else(|| id.clone(), str::to_string);
        if self.rec.branch(&name).is_some() {
            return Err(CellError::Invalid(format!("branch {name} already exists")));
        }
        let branch = Branch {
            id: id.clone(),
            name: name.clone(),
            forked_from: Some(cp.id.clone()),
            created_ms: now_ms(),
        };
        self.rec.branches.push(branch.clone());
        self.rec.current_branch.clone_from(&id);
        state::save(&self.dir, &self.rec)?;
        self.append(vec![match fork_name {
            Some(_) => EventBody::CheckpointForked(CheckpointForked {
                checkpoint_id: cp.id,
                branch_id: id,
                name,
            }),
            None => EventBody::CheckpointRewound(CheckpointRewound {
                checkpoint_id: cp.id,
                branch_id: id,
                irreversible_seqs: Vec::new(),
            }),
        }])?;
        self.set_status(SessionState::Paused, Some("rewound"))?;
        Ok(branch)
    }

    /// Every event of the ledger, in order.
    fn events(&self) -> Result<Vec<Event>> {
        let l = Ledger::open_read_only(self.dir.ledger())?;
        let mut all: Vec<Event> = Vec::new();
        loop {
            let batch = l.events_after(all.last().map_or(0, |e| e.seq), 5_000)?;
            if batch.is_empty() {
                return Ok(all);
            }
            all.extend(batch);
        }
    }

    /// The session's rewind requests with their decisions, oldest first.
    pub fn rewind_requests(&self) -> Result<Vec<RewindRequest>> {
        Ok(requests::project(&self.events()?))
    }

    fn pending_request(&self, id: u64) -> Result<RewindRequest> {
        let r = self
            .rewind_requests()?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| CellError::NotFound(format!("rewind request {id}")))?;
        if r.status != RequestStatus::Pending {
            return Err(CellError::Busy(format!(
                "rewind request {id} is already {}",
                match r.status {
                    RequestStatus::Confirmed => "confirmed",
                    _ => "rejected",
                }
            )));
        }
        Ok(r)
    }

    /// Append under the lock; a sealed ledger is a state error.
    fn append_decision(&self, body: EventBody) -> Result<u64> {
        match self.append(vec![body]) {
            Err(CellError::Ledger(wyrd_ledger::LedgerError::Sealed)) => Err(CellError::Busy(
                "the session is closed and its ledger sealed".into(),
            )),
            r => r,
        }
    }

    /// Record that the agent asks to be rewound to checkpoint
    /// `checkpoint_id` (an id such as `c2`; branch names are not
    /// accepted). Nothing else changes: the agent keeps running until a
    /// human confirms. The caller redacts `reason`.
    pub fn request_rewind(
        &mut self,
        checkpoint_id: &str,
        reason: &str,
        source: &str,
    ) -> Result<RewindRequest> {
        self.writable()?;
        let reason = reason.trim();
        if reason.is_empty() || reason.chars().count() > requests::MAX_REASON {
            return Err(CellError::Invalid(format!(
                "a reason of 1 to {} characters is required",
                requests::MAX_REASON
            )));
        }
        let _lock = state::lock(&self.dir.lock())?;
        self.reload()?;
        if self.rec.checkpoint(checkpoint_id).is_none() {
            return Err(CellError::NotFound(format!("checkpoint {checkpoint_id}")));
        }
        let pending = self
            .rewind_requests()?
            .iter()
            .filter(|r| r.status == RequestStatus::Pending)
            .count();
        if pending >= requests::MAX_PENDING {
            return Err(CellError::Busy(format!(
                "{pending} rewind requests are already waiting for a human"
            )));
        }
        let id = self.append_decision(EventBody::RewindRequested(RewindRequested {
            checkpoint_id: checkpoint_id.into(),
            reason: reason.into(),
            source: source.into(),
        }))?;
        self.pending_request(id)
    }

    /// A human confirms rewind request `id`: the decision is recorded, then
    /// the session is rewound to the requested checkpoint (stopping the
    /// agent if it runs). `by` names the surface (`cli`, `studio`).
    pub fn confirm_rewind(&mut self, id: u64, by: &str) -> Result<Branch> {
        self.writable()?;
        let _lock = state::lock(&self.dir.lock())?;
        self.reload()?;
        let r = self.pending_request(id)?;
        if self.rec.checkpoint(&r.checkpoint_id).is_none() {
            return Err(CellError::NotFound(format!(
                "checkpoint {}",
                r.checkpoint_id
            )));
        }
        self.append_decision(EventBody::RewindConfirmed(RewindConfirmed {
            request_seq: id,
            checkpoint_id: r.checkpoint_id.clone(),
            by: by.into(),
        }))?;
        self.rewind_locked(&r.checkpoint_id, None)
    }

    /// A human rejects rewind request `id`; nothing else changes.
    pub fn reject_rewind(
        &mut self,
        id: u64,
        by: &str,
        note: Option<String>,
    ) -> Result<RewindRequest> {
        self.writable()?;
        if note
            .as_ref()
            .is_some_and(|n| n.chars().count() > requests::MAX_NOTE)
        {
            return Err(CellError::Invalid(format!(
                "the note is longer than {} characters",
                requests::MAX_NOTE
            )));
        }
        let _lock = state::lock(&self.dir.lock())?;
        self.reload()?;
        self.pending_request(id)?;
        self.append_decision(EventBody::RewindRejected(RewindRejected {
            request_seq: id,
            by: by.into(),
            note,
        }))?;
        self.rewind_requests()?
            .into_iter()
            .find(|r| r.id == id)
            .ok_or_else(|| CellError::NotFound(format!("rewind request {id}")))
    }

    /// Changes from checkpoint/branch `a` to `b`.
    pub fn diff(&self, a: &str, b: &str) -> Result<Vec<Change>> {
        let ma = self.manifest(&self.resolve(a)?.manifest)?;
        let mb = self.manifest(&self.resolve(b)?.manifest)?;
        Ok(diff::diff(&ma, &mb))
    }

    /// The content of `path` (relative, `/`-separated) as seen inside the
    /// Cell at checkpoint `target`: from the checkpoint's upper layer when
    /// the path changed there, absent when it (or a parent) was deleted or
    /// hidden by an opaque directory, else the real workspace file the
    /// Cell's lower layer shows. At most `limit` bytes are returned.
    pub fn file_at(&self, target: &str, path: &str, limit: usize) -> Result<FileAt> {
        use wyrd_cas::EntryKind;
        if path.is_empty()
            || path.starts_with('/')
            || path
                .split('/')
                .any(|c| c.is_empty() || c == "." || c == "..")
        {
            return Err(CellError::Invalid(format!("bad path {path:?}")));
        }
        let m = self.manifest(&self.resolve(target)?.manifest)?;
        let mut hidden = false;
        let mut prefix = String::new();
        for comp in path.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(comp);
            let last = prefix.len() == path.len();
            let Some(e) = m.get(prefix.as_bytes()) else {
                continue;
            };
            match e.kind {
                EntryKind::Whiteout => return Ok(FileAt::absent()),
                EntryKind::File if last => {
                    let mut bytes = Vec::new();
                    for c in &e.chunks {
                        bytes.extend(self.cas.get(*c)?);
                        if bytes.len() > limit {
                            break;
                        }
                    }
                    return Ok(FileAt::bytes(bytes, limit, FileSource::Checkpoint));
                }
                EntryKind::Symlink if last => {
                    let t = e.target.as_ref().map(|t| t.to_vec()).unwrap_or_default();
                    return Ok(FileAt::bytes(t, limit, FileSource::Checkpoint));
                }
                EntryKind::Dir if last => return Ok(FileAt::absent()),
                // The lower layer below an opaque directory is invisible.
                EntryKind::Dir if e.opaque => hidden = true,
                _ => {}
            }
        }
        if hidden {
            return Ok(FileAt::absent());
        }
        let real = self.rec.workspace.join(path);
        match fs::symlink_metadata(&real) {
            Ok(md) if md.is_file() => {
                let mut bytes = Vec::new();
                fs::File::open(&real)
                    .and_then(|f| {
                        use std::io::Read as _;
                        f.take(limit as u64 + 1).read_to_end(&mut bytes)
                    })
                    .map_err(io_err(&real))?;
                Ok(FileAt::bytes(bytes, limit, FileSource::Workspace))
            }
            _ => Ok(FileAt::absent()),
        }
    }

    /// Apply a checkpoint/branch to the real workspace.
    pub fn apply(&mut self, target: &str, dry_run: bool, force: bool) -> Result<ApplyReport> {
        self.writable()?;
        let _lock = state::lock(&self.dir.lock())?;
        self.reload()?;
        let cp = self.resolve(target)?;
        let m = self.manifest(&cp.manifest)?;
        let baseline = Baseline::load(&self.dir.baseline())?;
        let report = apply::apply(
            &self.cas,
            &self.rec.workspace,
            &baseline,
            &m,
            dry_run,
            force,
        )?;
        if report.applied {
            self.append(vec![EventBody::CheckpointApplied(CheckpointApplied {
                checkpoint_id: cp.id,
                files: u32::try_from(report.ops.len()).unwrap_or(u32::MAX),
            })])?;
        }
        Ok(report)
    }

    /// Record verdict-plane events (claims, divergences, triage). Refused
    /// while the agent runs and after the ledger is sealed.
    pub fn record_verdicts(&mut self, bodies: Vec<EventBody>) -> Result<u64> {
        if bodies
            .iter()
            .any(|b| b.plane() != wyrd_schema::Plane::Verdict)
        {
            return Err(CellError::Invalid(
                "only verdict events can be recorded".into(),
            ));
        }
        self.writable()?;
        let _lock = state::lock(&self.dir.lock())?;
        self.reload()?;
        if self.is_running() {
            return Err(CellError::Busy("the session is recording".into()));
        }
        match self.append(bodies) {
            Err(CellError::Ledger(wyrd_ledger::LedgerError::Sealed)) => Err(CellError::Busy(
                "the session is closed and its ledger sealed".into(),
            )),
            r => r,
        }
    }

    /// Stop the agent if needed, write the closing event, and seal the ledger.
    pub fn close(&mut self, reason: &str) -> Result<Seal> {
        self.writable()?;
        let _lock = state::lock(&self.dir.lock())?;
        self.reload()?;
        self.stop()?;
        self.set_status(SessionState::Closing, Some(reason))?;
        self.append(vec![EventBody::SessionClosed(SessionClosed {
            reason: reason.into(),
            exit_code: None,
        })])?;
        self.set_status(SessionState::Closed, None)?;
        let key = HostKey::load_or_create(&self.data.host_key())?;
        let mut l = Ledger::open(self.dir.ledger())?;
        Ok(l.seal(&key, now_ms())?)
    }
}

/// Where an imported session came from (`import.json` in its directory).
/// An imported session is read-only: nothing can run, checkpoint, rewind,
/// apply, or append to it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ImportSource {
    /// Session id in the bundle (on the machine that recorded it).
    pub original_id: String,
    /// `b3:` + hex of the bundle's `manifest.json`.
    pub manifest_hash: String,
    pub signer_fingerprint: String,
    /// `standard` or `strict`.
    pub redaction: String,
    /// False when the bundle had no checkpoint content (`--no-content`).
    pub content: bool,
    pub imported_ms: i64,
}

/// A verified bundle's session, as [`Session::import`] takes it.
#[derive(Debug, Clone)]
pub struct ImportSpec {
    pub name: Option<String>,
    pub agent: String,
    pub mode: CaptureMode,
    pub created_ms: i64,
    /// Events in seq order, dense from 1.
    pub events: Vec<Event>,
    /// CAS chunks (each stored under the BLAKE3 of its bytes).
    pub chunks: Vec<Vec<u8>>,
    pub source: ImportSource,
}

fn add_branch(branches: &mut Vec<Branch>, id: &str, forked_from: Option<String>, ms: i64) {
    if !branches.iter().any(|b| b.id == id) {
        branches.push(Branch {
            id: id.into(),
            name: id.into(),
            forked_from,
            created_ms: ms,
        });
    }
}

/// Checkpoints and branches recorded in `events`.
fn recorded_tree(events: &[Event]) -> (Vec<Branch>, Vec<CheckpointRecord>) {
    let mut branches: Vec<Branch> = Vec::new();
    let mut checkpoints = Vec::new();
    for e in events {
        match &e.body {
            EventBody::CheckpointCreated(c) => {
                add_branch(&mut branches, &c.branch_id, None, e.ts_wall_ms);
                checkpoints.push(CheckpointRecord {
                    id: c.checkpoint_id.clone(),
                    branch: c.branch_id.clone(),
                    parent: c.parent_id.clone(),
                    manifest: c.manifest.clone(),
                    seq: e.seq,
                    created_ms: e.ts_wall_ms,
                    files: u64::from(c.files_changed),
                    bytes_read: c.bytes_added,
                });
            }
            EventBody::CheckpointForked(f) => {
                add_branch(
                    &mut branches,
                    &f.branch_id,
                    Some(f.checkpoint_id.clone()),
                    e.ts_wall_ms,
                );
            }
            _ => {}
        }
    }
    if branches.is_empty() {
        add_branch(&mut branches, "a", None, 0);
    }
    (branches, checkpoints)
}

impl Session {
    /// Add a verified bundle as a new, read-only session. The ledger is
    /// rebuilt from the events (so its chain head equals the bundle's) and
    /// left unsealed; `import.json` records the provenance. Nothing is
    /// visible under `sessions/` until the import is complete.
    pub fn import(data: &DataDir, spec: &ImportSpec) -> Result<Session> {
        mkdirs(&data.sessions())?;
        let cas = Cas::open(data.cas())?;
        for chunk in &spec.chunks {
            cas.put(chunk)?;
        }
        let id = ulid::Ulid::generate().to_string().to_lowercase();
        let staging = data.session(&format!(".import-{id}"));
        let result = (|| {
            mkdirs(staging.root())?;
            let (branches, checkpoints) = recorded_tree(&spec.events);
            let command = spec
                .events
                .iter()
                .find_map(|e| match &e.body {
                    EventBody::SessionStarted(s) => Some(s.command.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            let rec = SessionRecord {
                id: id.clone(),
                name: spec.name.clone(),
                workspace: PathBuf::new(),
                command,
                mode: spec.mode,
                created_ms: spec.created_ms,
                status: SessionState::Closed,
                current_branch: branches.last().map_or_else(|| "a".into(), |b| b.id.clone()),
                branches,
                checkpoints,
                cgroup: None,
                pid: None,
                runs: 0,
                agent: spec.agent.clone(),
                deny_tools: Vec::new(),
                policy: None,
            };
            let mut ledger = Ledger::create(staging.ledger(), &id)?;
            ledger.append(spec.events.clone())?;
            // The session becomes visible at the rename below, so its events must be on disk first.
            ledger.sync()?;
            wyrd_cas::note_step(wyrd_cas::Step::SyncFile(staging.ledger()));
            let source = serde_json::to_vec_pretty(&spec.source)
                .map_err(|e| CellError::Invalid(e.to_string()))?;
            wyrd_cas::write_durable(&staging.import(), &source)?;
            state::save(&staging, &rec)?;
            Ok(rec)
        })();
        let rec = match result {
            Ok(rec) => rec,
            Err(e) => {
                let _ = fs::remove_dir_all(staging.root());
                return Err(e);
            }
        };
        let dir = data.session(&id);
        wyrd_cas::note_step(wyrd_cas::Step::Rename(
            staging.root().to_path_buf(),
            dir.root().to_path_buf(),
        ));
        if let Err(e) = fs::rename(staging.root(), dir.root()) {
            let _ = fs::remove_dir_all(staging.root());
            return Err(io_err(dir.root())(e));
        }
        // The new session's name is an entry in `sessions/`.
        wyrd_cas::sync_dir(&data.sessions())?;
        Ok(Session {
            data: data.clone(),
            dir,
            rec,
            cas,
        })
    }

    /// Provenance of an imported session; `None` for a recorded one.
    pub fn imported(&self) -> Option<ImportSource> {
        let bytes = fs::read(self.dir.import()).ok()?;
        serde_json::from_slice(&bytes).ok()
    }

    /// Refuse to change an imported session.
    fn writable(&self) -> Result<()> {
        if self.dir.import().exists() {
            return Err(CellError::Busy(format!(
                "session {} was imported from a bundle and is read-only",
                self.rec.id
            )));
        }
        Ok(())
    }
}
