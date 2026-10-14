//! Session orchestration: create, run, checkpoint, rewind, fork, diff,
//! apply, close. Every state change is appended to the session ledger.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use wyrd_cas::{Cas, ChunkHash, Manifest};
use wyrd_ledger::{HostKey, Ledger, Seal};
use wyrd_schema::{
    CaptureMode, CheckpointApplied, CheckpointCreated, CheckpointForked, CheckpointRewound, Event,
    EventBody, SessionClosed, SessionStarted, SessionState, SessionStateChanged,
};

use crate::apply::{self, ApplyReport};
use crate::baseline::Baseline;
use crate::cgroup::Cgroup;
use crate::diff::{self, Change};
use crate::exec::{LaunchMode, LaunchSpec};
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
    pub kernel_release: String,
    pub capabilities: BTreeMap<String, bool>,
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

fn mkdirs(p: &Path) -> Result<()> {
    fs::create_dir_all(p).map_err(io_err(p))
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
        };
        state::save(&dir, &rec)?;
        let mut ledger = Ledger::create(dir.ledger(), &id)?;
        ledger.append(vec![event(EventBody::SessionStarted(SessionStarted {
            session_id: id,
            agent_kind: opts.agent_kind,
            agent_version: None,
            command: opts.command,
            workspace_root: workspace.display().to_string(),
            mode,
            kernel_release: opts.kernel_release,
            capabilities: opts.capabilities,
            policy_set_hash: None,
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

    /// Run the agent in the current branch and wait for it to exit. Returns
    /// its exit code. A final checkpoint is taken in Cell mode.
    pub fn run(
        &mut self,
        helper: &Path,
        probe_userns: impl FnOnce() -> std::result::Result<(), String>,
    ) -> Result<i32> {
        let mut running = {
            let _lock = state::lock(&self.dir.lock())?;
            self.reload()?;
            if self.is_running() {
                return Err(CellError::Invalid("the session is already running".into()));
            }
            let mode =
                match launch::choose_mode(self.rec.mode == CaptureMode::Observe, probe_userns) {
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
                write_allow: confine::default_write_allow(
                    &workspace,
                    launch::home_dir().as_deref(),
                ),
                cwd: workspace,
                command: self.rec.command.clone(),
                env: launch::agent_env(&self.rec.id),
            };
            if mode != LaunchMode::Observe {
                // A previous run may have left overlay's internal work dir behind.
                let _ = fs::remove_dir(self.dir.work(&branch).join("work"));
                mkdirs(&self.dir.work(&branch))?;
            }
            self.rec.runs += 1;
            // The full id: ULIDs created in the same millisecond share their prefix.
            let unit = format!("wyrd-{}-r{}", self.rec.id, self.rec.runs);
            let running = launch::spawn(helper, &mut spec, &self.dir.spec(), &unit)?;
            self.rec.pid = Some(running.child.id());
            self.rec.cgroup = running.cgroup.as_ref().map(|c| c.path().to_path_buf());
            state::save(&self.dir, &self.rec)?;
            self.set_status(SessionState::Recording, Some("agent started"))?;
            running
        };
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
            self.checkpoint_locked(None)?;
        }
        self.set_status(
            SessionState::Paused,
            Some(&format!("agent exited with {code}")),
        )?;
        Ok(code)
    }

    /// Checkpoint the current branch (freezing the Cell while reading).
    pub fn checkpoint(&mut self, after_call: Option<String>) -> Result<CheckpointRecord> {
        let _lock = state::lock(&self.dir.lock())?;
        self.reload()?;
        self.checkpoint_locked(after_call)
    }

    fn checkpoint_locked(&mut self, after_call: Option<String>) -> Result<CheckpointRecord> {
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
        let snap = self.cas.snapshot(&self.dir.upper(&branch), base.as_ref());
        if let Some(cg) = &frozen {
            cg.thaw()?;
        }
        let (manifest, stats) = snap?;
        let hash = self.cas.put_manifest(&manifest)?;
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
        let _lock = state::lock(&self.dir.lock())?;
        self.reload()?;
        let cp = self.resolve(target)?;
        if self.stop()? {
            self.set_status(SessionState::Rewinding, Some("stopped the agent to rewind"))?;
        }
        let manifest = self.manifest(&cp.manifest)?;
        let id = self.rec.next_branch_id();
        let upper = self.dir.upper(&id);
        let report = self.cas.materialize(&manifest, &upper)?;
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

    /// Changes from checkpoint/branch `a` to `b`.
    pub fn diff(&self, a: &str, b: &str) -> Result<Vec<Change>> {
        let ma = self.manifest(&self.resolve(a)?.manifest)?;
        let mb = self.manifest(&self.resolve(b)?.manifest)?;
        Ok(diff::diff(&ma, &mb))
    }

    /// Apply a checkpoint/branch to the real workspace.
    pub fn apply(&mut self, target: &str, dry_run: bool, force: bool) -> Result<ApplyReport> {
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

    /// Stop the agent if needed, write the closing event, and seal the ledger.
    pub fn close(&mut self, reason: &str) -> Result<Seal> {
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
