//! Intent capture around a run: the per-session intent socket, a ledger
//! sink, and the debounced checkpoint after every finished tool call.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use anyhow::Context as _;
use wyrd_adapters::{Ingest, Sink, StaticPolicy, claude_code, server};
use wyrd_cell::{DataDir, Session};
use wyrd_ledger::Ledger;
use wyrd_schema::{CaptureMode, Event, EventBody, Role};

/// Quiet time after a finished tool call before its checkpoint is taken.
/// The next `PreToolUse` takes a pending checkpoint immediately.
pub const DEBOUNCE: Duration = Duration::from_millis(250);

/// Debounced checkpoints attributed to tool calls.
#[derive(Debug)]
pub struct Checkpointer {
    data: DataDir,
    session: String,
    state: Mutex<Pending>,
    wake: Condvar,
    /// Held while a checkpoint is taken so a flush waits for one in flight.
    taking: Mutex<()>,
}

#[derive(Debug, Default)]
struct Pending {
    call: Option<String>,
    due: Option<Instant>,
    stopped: bool,
}

impl Checkpointer {
    pub fn start(data: DataDir, session: String) -> Arc<Self> {
        let cp = Arc::new(Checkpointer {
            data,
            session,
            state: Mutex::new(Pending::default()),
            wake: Condvar::new(),
            taking: Mutex::new(()),
        });
        let worker = Arc::clone(&cp);
        if let Err(e) = std::thread::Builder::new()
            .name("wyrd-checkpointer".into())
            .spawn(move || worker.run())
        {
            eprintln!("wyrd: cannot start the checkpointer: {e}");
        }
        cp
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Pending> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    pub fn schedule(&self, call_id: &str) {
        let mut p = self.lock();
        if let Some(prev) = p.call.replace(call_id.to_string()) {
            // Two calls finished within the debounce window (parallel
            // calls): one checkpoint covers both, attributed to the later.
            tracing::debug!(previous = %prev, "coalescing checkpoints");
        }
        p.due = Some(Instant::now() + DEBOUNCE);
        self.wake.notify_all();
    }

    /// Take the pending checkpoint now, if any.
    pub fn flush(&self) {
        let _taking = self.taking.lock().unwrap_or_else(PoisonError::into_inner);
        let call = {
            let mut p = self.lock();
            p.due = None;
            p.call.take()
        };
        if let Some(call) = call {
            self.take(call);
        }
    }

    /// Stop the worker and hand the pending call (if any) to the caller,
    /// which attributes the exit checkpoint to it.
    pub fn stop(&self) -> Option<String> {
        let _taking = self.taking.lock().unwrap_or_else(PoisonError::into_inner);
        let mut p = self.lock();
        p.stopped = true;
        p.due = None;
        self.wake.notify_all();
        p.call.take()
    }

    fn take(&self, call: String) {
        let r = Session::open(&self.data, &self.session).and_then(|mut s| s.checkpoint(Some(call)));
        if let Err(e) = r {
            eprintln!("wyrd: checkpoint after tool call failed: {e}");
        }
    }

    fn run(&self) {
        let mut p = self.lock();
        loop {
            if p.stopped {
                return;
            }
            match p.due {
                Some(due) if Instant::now() >= due => {
                    drop(p);
                    self.flush();
                    p = self.lock();
                }
                Some(due) => {
                    let wait = due.saturating_duration_since(Instant::now());
                    p = self
                        .wake
                        .wait_timeout(p, wait)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0;
                }
                None => {
                    p = self.wake.wait(p).unwrap_or_else(PoisonError::into_inner);
                }
            }
        }
    }
}

/// Appends ingested events to the session ledger.
#[derive(Debug)]
pub struct LedgerSink {
    ledger: Mutex<Ledger>,
    checkpoints: Option<Arc<Checkpointer>>,
}

impl Sink for LedgerSink {
    fn append(&self, bodies: Vec<EventBody>) -> Result<(), String> {
        let events = bodies
            .into_iter()
            .map(|body| Event {
                seq: 0,
                ts_mono_ns: wyrd_cell::mono_ns(),
                ts_wall_ms: wyrd_cell::now_ms(),
                process: None,
                drops_since_last: 0,
                body,
            })
            .collect();
        let mut l = self.ledger.lock().unwrap_or_else(PoisonError::into_inner);
        l.append(events).map(|_| ()).map_err(|e| e.to_string())
    }

    fn before_tool_call(&self) {
        if let Some(cp) = &self.checkpoints {
            cp.flush();
        }
    }

    fn after_tool_call(&self, call_id: &str) {
        if let Some(cp) = &self.checkpoints {
            cp.schedule(call_id);
        }
    }
}

/// Last user turn recorded in a ledger.
fn last_turn(ledger: &Ledger) -> anyhow::Result<u32> {
    Ok(ledger
        .events_after(0, usize::MAX)?
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::Message(m) if m.role == Role::User => Some(m.turn),
            _ => None,
        })
        .max()
        .unwrap_or(0))
}

/// Where the intent socket of a session lives: under `$XDG_RUNTIME_DIR`
/// when possible (short path, never writable by the Cell), else in the
/// session directory.
pub fn socket_path(session_dir: &Path, session_id: &str) -> PathBuf {
    if let Some(rt) = std::env::var_os("XDG_RUNTIME_DIR") {
        let dir = PathBuf::from(rt).join("wyrdline");
        let ok = std::fs::create_dir_all(&dir).is_ok() && {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).is_ok()
        };
        if ok {
            return dir.join(format!("intent-{session_id}.sock"));
        }
    }
    session_dir.join("intent.sock")
}

/// Intent capture for one run.
#[derive(Debug)]
pub struct Capture {
    pub socket: PathBuf,
    pub checkpoints: Option<Arc<Checkpointer>>,
    /// Environment for the agent.
    pub env: Vec<(String, String)>,
    /// Command with injected hook settings, when the adapter needs it.
    pub command: Option<Vec<String>>,
    /// Wrapped MCP config (may hold server environment values); removed
    /// when the run ends.
    pub mcp_config: Option<PathBuf>,
}

impl Drop for Capture {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.socket);
        if let Some(f) = &self.mcp_config {
            let _ = std::fs::remove_file(f);
        }
    }
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    serde_json::from_slice(&std::fs::read(path).ok()?).ok()
}

/// The agent's user-level config file (`$CLAUDE_CONFIG_DIR/.claude.json`
/// or `~/.claude.json`).
fn agent_user_config() -> PathBuf {
    match std::env::var_os("CLAUDE_CONFIG_DIR") {
        Some(d) => PathBuf::from(d).join(".claude.json"),
        None => wyrd_cell::launch::home_dir()
            .unwrap_or_default()
            .join(".claude.json"),
    }
}

fn write_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    use std::io::Write as _;
    use std::os::unix::fs::OpenOptionsExt as _;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(path)
        .and_then(|mut f| f.write_all(bytes))
        .with_context(|| format!("writing {}", path.display()))
}

/// Bind the intent socket and start serving it for `session`.
pub fn start(data: &DataDir, session: &Session, wyrd: &Path) -> anyhow::Result<Capture> {
    let rec = session.record();
    let socket = socket_path(session.dir().root(), &rec.id);
    let listener = server::bind(&socket)
        .with_context(|| format!("binding the intent socket {}", socket.display()))?;
    let ledger = Ledger::open(session.dir().ledger())?;
    let turn = last_turn(&ledger)?;
    let checkpoints =
        (rec.mode == CaptureMode::Cell).then(|| Checkpointer::start(data.clone(), rec.id.clone()));
    let ingest = Arc::new(Ingest::new(
        LedgerSink {
            ledger: Mutex::new(ledger),
            checkpoints: checkpoints.clone(),
        },
        rec.mode,
        StaticPolicy {
            deny_tools: rec.deny_tools.clone(),
        },
        turn,
    ));
    std::thread::Builder::new()
        .name("wyrd-intent".into())
        .spawn(move || server::serve(&listener, &ingest))
        .context("starting the intent socket server")?;
    let mode = match rec.mode {
        CaptureMode::Cell => "cell",
        CaptureMode::Observe => "observe",
    };
    let env = vec![
        (
            wyrd_adapters::SOCKET_ENV.into(),
            socket.display().to_string(),
        ),
        (wyrd_adapters::MODE_ENV.into(), mode.into()),
        (wyrd_adapters::BIN_ENV.into(), wyrd.display().to_string()),
    ];
    let mut mcp_config = None;
    let command = if rec.agent == claude_code::NAME {
        let file = session.dir().root().join("claude-code-settings.json");
        std::fs::write(
            &file,
            serde_json::to_vec_pretty(&claude_code::settings(wyrd))?,
        )
        .with_context(|| format!("writing {}", file.display()))?;
        let mut command = claude_code::inject_settings(&rec.command, &file);
        let servers = claude_code::mcp_servers(
            read_json(&agent_user_config()).as_ref(),
            read_json(&rec.workspace.join(".mcp.json")).as_ref(),
            &rec.workspace,
        );
        if !servers.is_empty() {
            let file = session.dir().root().join("claude-code-mcp.json");
            write_private(
                &file,
                &serde_json::to_vec_pretty(&claude_code::wrap_mcp_servers(&servers, wyrd))?,
            )?;
            command = claude_code::inject_mcp_config(&command, &file);
            mcp_config = Some(file);
        }
        Some(command)
    } else {
        if matches!(
            rec.agent.as_str(),
            wyrd_adapters::CODEX | wyrd_adapters::gemini_cli::NAME
        ) {
            eprintln!(
                "wyrd: hooks are not injected automatically for {}; register `wyrd hook {}` \
                 in its hook configuration (docs/architecture/adapters.md)",
                rec.agent, rec.agent
            );
        }
        None
    };
    Ok(Capture {
        socket,
        checkpoints,
        env,
        command,
        mcp_config,
    })
}

/// Adapter implied by the agent program, when `--agent` is not given.
pub fn detect_agent(command: &[String]) -> String {
    let prog = command
        .first()
        .and_then(|p| Path::new(p).file_name())
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    match prog.as_str() {
        "claude" => claude_code::NAME.into(),
        "codex" => wyrd_adapters::CODEX.into(),
        "gemini" => wyrd_adapters::gemini_cli::NAME.into(),
        "wyrd-fakeagent" => "fakeagent".into(),
        _ => "generic".into(),
    }
}
