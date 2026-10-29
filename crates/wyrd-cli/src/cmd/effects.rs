//! Kernel effect capture for `wyrd run`: registers the session cgroup with
//! `wyrd-sensord` before the agent is released from the start gate, then
//! appends the streamed effects to the session ledger.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use wyrd_cell::{DataDir, Session};
use wyrd_ledger::Ledger;
use wyrd_schema::{Event, EventBody, SensorLost, SensorResumed};
use wyrd_sensor::{SensorClient, ServerMsg};

/// The policy's egress rules for the kernel: fixed prefixes, host names
/// resolved now and every [`REFRESH`], and ports.
#[derive(Debug, Clone, Default)]
pub struct EgressPlan {
    pub cidrs: Vec<String>,
    pub hosts: Vec<String>,
    pub ports: Vec<u16>,
    /// Whether destinations are restricted at all (else ports only).
    pub restrict_hosts: bool,
    pub policy_id: String,
    pub policy_version: String,
}

/// How often allowed host names are resolved again; addresses they no
/// longer resolve to are removed from the allow-list.
pub const REFRESH: Duration = Duration::from_secs(60);

impl EgressPlan {
    /// Current rules: the fixed prefixes plus every address the host names
    /// resolve to (names that do not resolve contribute nothing).
    pub fn rules(
        &self,
        resolve: &dyn Fn(&str) -> Vec<std::net::IpAddr>,
    ) -> wyrd_sensor::EgressRules {
        let mut cidrs = self.cidrs.clone();
        for h in &self.hosts {
            for ip in resolve(h) {
                cidrs.push(match ip {
                    std::net::IpAddr::V4(a) => format!("{a}/32"),
                    std::net::IpAddr::V6(a) => format!("{a}/128"),
                });
            }
        }
        cidrs.sort();
        cidrs.dedup();
        wyrd_sensor::EgressRules {
            cidrs,
            restrict_hosts: self.restrict_hosts,
            ports: self.ports.clone(),
        }
    }
}

/// Resolve with the system resolver (the same one the agent uses).
pub fn resolve(host: &str) -> Vec<std::net::IpAddr> {
    use std::net::ToSocketAddrs as _;
    (host, 443)
        .to_socket_addrs()
        .map(|it| it.map(|a| a.ip()).collect())
        .unwrap_or_default()
}

/// Effect-time checks: the policy's `on-effect` and the exec deny list. A
/// deny can only stop the agent (freeze and kill the Cell): the effect has
/// already happened.
#[derive(Debug)]
pub struct EffectGuard {
    pub policy: Option<Arc<wyrd_policy::Policy>>,
    pub ctx: wyrd_policy::SessionCtx,
    pub exec_deny: wyrd_policy::effects::ExecDeny,
    /// Stop the agent on a deny (Cell mode); observe mode only records.
    pub enforce: bool,
    pub workspace: PathBuf,
    /// The Cell's upper layer: files the agent created live here.
    pub upper: PathBuf,
    pub policy_id: String,
    pub policy_version: String,
}

impl EffectGuard {
    /// A path the agent saw, as readable from outside its mount namespace.
    fn host_path(&self, p: &str) -> PathBuf {
        let path = Path::new(p);
        match path.strip_prefix(&self.workspace) {
            Ok(rel) => {
                let up = self.upper.join(rel);
                if up.exists() { up } else { path.to_path_buf() }
            }
            Err(_) => path.to_path_buf(),
        }
    }

    /// Why this effect must stop the session, if it must.
    pub fn check(&self, body: &EventBody) -> Option<String> {
        if let EventBody::ProcExec(x) = body
            && let Some(why) = self
                .exec_deny
                .check(&x.filename, &|p| Some(self.host_path(p)))
        {
            return Some(why);
        }
        let policy = self.policy.as_ref()?;
        let effect = wyrd_policy::effects::effect_of(body)?;
        let out = policy.on_effect(&effect, &self.ctx);
        match out.value {
            wyrd_policy::Verdict::Allow => None,
            // An effect cannot wait for a human: ask stops it like deny.
            wyrd_policy::Verdict::Deny(r) | wyrd_policy::Verdict::Ask(r) => Some(r),
        }
    }
}

const BATCH: usize = 500;
const FLUSH_EVERY: Duration = Duration::from_millis(50);
const RECONNECT_EVERY: Duration = Duration::from_secs(1);
/// After the agent exits, wait at most this long for the sensor to confirm
/// that every queued event was delivered.
const DRAIN_BOUND: Duration = Duration::from_secs(5);

/// The end of capture: a flush barrier sent to the sensor when the agent
/// exited, answered once every event queued before it has been delivered.
#[derive(Debug)]
struct Drain {
    token: u64,
    since: Instant,
    /// Events that arrived after the agent exited.
    received: u64,
}

impl Drain {
    fn confirmed_by(&self, msg: &ServerMsg) -> bool {
        let done = matches!(msg, ServerMsg::Flushed { token } if *token == self.token);
        if done {
            tracing::info!(events = self.received, "sensor confirmed delivery");
        }
        done
    }

    fn expired(&self, now: Instant) -> bool {
        now.duration_since(self.since) >= DRAIN_BOUND
    }
}

/// Loss counters seen on the capture connection, and loss not yet attributed
/// to an event (it goes out as the next event's `drops_since_last`).
#[derive(Debug, Default, PartialEq, Eq)]
struct DropCount {
    /// Last ring-buffer total the daemon reported (daemon-wide).
    ring: Option<u64>,
    /// Last count of events this connection's queue refused.
    queue: u64,
    pending: u64,
}

/// Which loss counters grew in one `drops` report.
#[derive(Debug, PartialEq, Eq)]
struct Grew {
    ring: bool,
    queue: bool,
}

impl DropCount {
    /// Fold one `drops` report in.
    fn observe(&mut self, total: u64, queue_dropped: u64) -> Grew {
        let ring_prev = *self.ring.get_or_insert(total);
        self.pending += total.saturating_sub(ring_prev);
        self.ring = Some(total);
        self.pending += queue_dropped.saturating_sub(self.queue);
        let queue = queue_dropped > self.queue;
        self.queue = queue_dropped;
        Grew {
            ring: total > ring_prev,
            queue,
        }
    }

    /// The daemon counts each connection's queue from zero.
    fn new_connection(&mut self) {
        self.queue = 0;
    }
}

/// Handle to the capture thread.
#[derive(Debug)]
pub struct EffectCapture {
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
    /// Why capture is not (fully) working, for the user.
    pub warning: Arc<Mutex<Option<String>>>,
}

impl EffectCapture {
    /// Stop after draining in-flight events.
    pub fn finish(mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn event(
    ts_mono_ns: u64,
    process: Option<wyrd_schema::ProcessRef>,
    drops: u32,
    body: EventBody,
) -> Event {
    Event {
        seq: 0,
        ts_mono_ns,
        ts_wall_ms: wyrd_cell::now_ms(),
        process,
        drops_since_last: drops,
        body,
    }
}

fn meta(body: EventBody) -> Event {
    event(wyrd_cell::mono_ns(), None, 0, body)
}

/// Start capture for a launched Cell and return once the cgroup is
/// registered (or capture is known to be unavailable). The agent is still
/// held at the start gate while this runs.
pub fn start(
    data: &DataDir,
    session_id: &str,
    ledger: PathBuf,
    cgroup: Option<&Path>,
    workspace: &Path,
    egress: Option<EgressPlan>,
    guard: Option<EffectGuard>,
) -> EffectCapture {
    let socket = wyrd_sensor::client::socket_path();
    let stop = Arc::new(AtomicBool::new(false));
    let warning = Arc::new(Mutex::new(None));
    let (ready_tx, ready_rx) = mpsc::sync_channel::<()>(1);
    let ctx = Ctx {
        data: data.clone(),
        session_id: session_id.to_string(),
        ledger,
        cgroup: cgroup.map(Path::to_path_buf),
        workspace: workspace.to_path_buf(),
        socket,
        stop: stop.clone(),
        warning: warning.clone(),
        egress,
        guard,
        stopped: AtomicBool::new(false),
    };
    let thread = std::thread::Builder::new()
        .name("effects".into())
        .spawn(move || ctx.run(&ready_tx))
        .ok();
    // Registration is acknowledged by the daemon only once the cgroup is in
    // the kernel map; wait for that (bounded) before releasing the agent.
    let _ = ready_rx.recv_timeout(Duration::from_secs(5));
    EffectCapture {
        stop,
        thread,
        warning,
    }
}

struct Ctx {
    data: DataDir,
    session_id: String,
    ledger: PathBuf,
    cgroup: Option<PathBuf>,
    workspace: PathBuf,
    socket: PathBuf,
    stop: Arc<AtomicBool>,
    warning: Arc<Mutex<Option<String>>>,
    egress: Option<EgressPlan>,
    guard: Option<EffectGuard>,
    stopped: AtomicBool,
}

impl Ctx {
    fn append(&self, events: Vec<Event>) {
        if events.is_empty() {
            return;
        }
        match Ledger::open(&self.ledger).and_then(|mut l| l.append(events)) {
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "could not append kernel effects"),
        }
    }

    /// Record that some effects were lost, without marking the session
    /// degraded (the loss is counted in `drops_since_last`).
    fn warn_lost(&self, msg: &str) {
        if let Ok(mut w) = self.warning.lock() {
            w.get_or_insert_with(|| msg.into());
        }
    }

    /// Fold a `drops` report in, warning when loss grew. The queue loss means
    /// this client fell behind the sensor.
    fn on_drops(&self, seen: &mut DropCount, total: u64, queue_dropped: u64) {
        let grew = seen.observe(total, queue_dropped);
        if grew.ring {
            self.warn_lost("the sensor's ring buffer overflowed; some effects were lost");
        }
        if grew.queue {
            self.warn_lost("wyrd could not keep up with the sensor; some effects were lost");
        }
    }

    fn lost(&self, reason: &str) {
        let reason = if self.egress.is_some() {
            format!("{reason}; the network policy is not enforced in the kernel")
        } else {
            reason.to_string()
        };
        let reason = reason.as_str();
        self.append(vec![meta(EventBody::SensorLost(SensorLost {
            reason: reason.to_string(),
        }))]);
        let _ = Session::mark_degraded(&self.data, &self.session_id, reason);
        if let Ok(mut w) = self.warning.lock() {
            w.get_or_insert_with(|| reason.to_string());
        }
    }

    fn connect(&self) -> Result<SensorClient, String> {
        let cgroup = self
            .cgroup
            .as_deref()
            .ok_or("the Cell has no cgroup of its own (no systemd user manager)")?;
        if !self.socket.exists() {
            return Err(format!(
                "wyrd-sensord is not running (no socket at {}); kernel effects are not captured",
                self.socket.display()
            ));
        }
        let rules = self.egress.as_ref().map(|e| e.rules(&resolve));
        let client = SensorClient::connect_with(&self.socket, cgroup, &self.workspace, rules)
            .map_err(|e| e.to_string())?;
        if let Some(why) = &client.egress_error {
            let reason = format!("the network policy is not enforced in the kernel: {why}");
            let _ = Session::mark_degraded(&self.data, &self.session_id, &reason);
            if let Ok(mut w) = self.warning.lock() {
                w.get_or_insert(reason);
            }
        }
        Ok(client)
    }

    /// Record an effect-time deny right after its effect (appending the
    /// pending batch first, so the decision can cite the effect's seq), and
    /// stop the agent once in Cell mode.
    fn effect_denied(&self, batch: &mut Vec<Event>, reason: &str) {
        let Some(g) = &self.guard else {
            return;
        };
        let seq = match Ledger::open(&self.ledger).and_then(|mut l| l.append(std::mem::take(batch)))
        {
            Ok(last) => Some(last),
            Err(e) => {
                tracing::error!(error = %e, "could not append kernel effects");
                None
            }
        };
        let reason = if g.enforce {
            format!("{reason}; the agent was stopped (the effect itself cannot be undone)")
        } else {
            format!("{reason}; observe mode: not stopped")
        };
        self.append(vec![meta(EventBody::GuardDecision(
            wyrd_schema::GuardDecision {
                stage: wyrd_schema::GuardStage::Effect,
                decision: wyrd_schema::Decision::Deny,
                policy_id: g.policy_id.clone(),
                policy_version: g.policy_version.clone(),
                reason: reason.clone(),
                call_id: None,
                subject_seq: seq,
            },
        ))]);
        if g.enforce && !self.stopped.swap(true, Ordering::SeqCst) {
            let stopped = Session::open(&self.data, &self.session_id).and_then(|mut s| s.stop());
            if let Err(e) = stopped {
                tracing::error!(error = %e, "could not stop the agent after an effect-time deny");
            }
            if let Ok(mut w) = self.warning.lock() {
                w.get_or_insert(format!("policy stopped the agent: {reason}"));
            }
        }
    }

    /// Re-resolve the allowed host names every [`REFRESH`] and send the
    /// rules again when they changed.
    fn refresh_egress(
        &self,
        client: Option<&SensorClient>,
        refreshed: &mut Instant,
        current: &mut Option<wyrd_sensor::EgressRules>,
    ) {
        let (Some(plan), Some(c)) = (&self.egress, client) else {
            return;
        };
        if refreshed.elapsed() < REFRESH {
            return;
        }
        *refreshed = Instant::now();
        let next = plan.rules(&resolve);
        if current.as_ref() != Some(&next) {
            if let Err(e) = c.send_egress(&next) {
                tracing::warn!(error = %e, "could not update the egress rules");
            }
            *current = Some(next);
        }
    }

    /// A kernel-stage decision next to every connect the kernel refused.
    fn guard_for(&self, body: &EventBody) -> Option<EventBody> {
        let (EventBody::NetConnect(c), Some(plan)) = (body, &self.egress) else {
            return None;
        };
        (c.verdict == wyrd_schema::NetVerdict::Denied).then(|| {
            EventBody::GuardDecision(wyrd_schema::GuardDecision {
                stage: wyrd_schema::GuardStage::Kernel,
                decision: wyrd_schema::Decision::Deny,
                policy_id: plan.policy_id.clone(),
                policy_version: plan.policy_version.clone(),
                reason: format!(
                    "connect to {} port {} refused in the kernel: not on the egress allow-list",
                    c.addr, c.port
                ),
                call_id: None,
                subject_seq: None,
            })
        })
    }

    /// The agent exited: ask the sensor for a delivery barrier.
    fn start_drain(&self, client: &mut Option<SensorClient>, batch: &mut Vec<Event>) -> Drain {
        let d = Drain {
            token: wyrd_cell::mono_ns(),
            since: Instant::now(),
            received: 0,
        };
        if let Some(c) = client.as_ref()
            && let Err(e) = c.send_flush(d.token)
        {
            self.append(std::mem::take(batch));
            self.lost(&format!("lost the sensor connection: {e}"));
            *client = None;
        }
        d
    }

    /// The bound passed without the barrier's answer: say so, never silently.
    fn drain_unconfirmed(&self, d: &Drain, batch: &mut Vec<Event>) {
        tracing::warn!(events = d.received, "sensor did not confirm delivery");
        self.append(std::mem::take(batch));
        self.lost(&format!(
            "wyrd-sensord did not confirm delivery within {} s after the agent exited; \
             its last effects may be missing",
            DRAIN_BOUND.as_secs()
        ));
    }

    fn reconnect(&self) -> Option<SensorClient> {
        let c = self.connect().ok()?;
        self.append(vec![meta(EventBody::SensorResumed(SensorResumed {
            dropped: 0,
        }))]);
        let _ = Session::mark_recovered(&self.data, &self.session_id);
        Some(c)
    }

    fn run(self, ready: &mpsc::SyncSender<()>) {
        let mut client = match self.connect() {
            Ok(c) => Some(c),
            Err(reason) => {
                self.lost(&reason);
                None
            }
        };
        let _ = ready.send(());
        if self.cgroup.is_none() {
            return;
        }
        let mut seen = DropCount::default();
        let mut batch: Vec<Event> = Vec::with_capacity(BATCH);
        let mut last_flush = Instant::now();
        let mut last_attempt = Instant::now();
        let mut drain: Option<Drain> = None;
        let mut refreshed = Instant::now();
        let mut current = self.egress.as_ref().map(|e| e.rules(&resolve));
        loop {
            self.refresh_egress(client.as_ref(), &mut refreshed, &mut current);
            if drain.is_none() && self.stop.load(Ordering::SeqCst) {
                drain = Some(self.start_drain(&mut client, &mut batch));
            }
            if let Some(d) = drain.as_ref().filter(|d| d.expired(Instant::now())) {
                self.drain_unconfirmed(d, &mut batch);
                break;
            }
            let Some(c) = client.as_mut() else {
                if drain.is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
                if last_attempt.elapsed() >= RECONNECT_EVERY {
                    last_attempt = Instant::now();
                    client = self.reconnect();
                    seen.new_connection();
                }
                continue;
            };
            let _ = c.set_timeout(Some(Duration::from_millis(50)));
            match c.recv() {
                Ok(Some(ServerMsg::Event {
                    ts_mono_ns,
                    process,
                    body,
                })) => {
                    drain = drain.map(|d| Drain {
                        received: d.received + 1,
                        ..d
                    });
                    let drops =
                        u32::try_from(std::mem::take(&mut seen.pending)).unwrap_or(u32::MAX);
                    let guard = self.guard_for(&body);
                    let stop = self.guard.as_ref().and_then(|g| g.check(&body));
                    batch.push(event(ts_mono_ns, Some(process), drops, body));
                    if let Some(g) = guard {
                        batch.push(event(ts_mono_ns, Some(process), 0, g));
                    }
                    if let Some(reason) = stop {
                        self.effect_denied(&mut batch, &reason);
                    }
                }
                Ok(Some(ServerMsg::Drops {
                    total,
                    queue_dropped,
                })) => self.on_drops(&mut seen, total, queue_dropped),
                Ok(Some(msg)) if drain.as_ref().is_some_and(|d| d.confirmed_by(&msg)) => break,
                Ok(Some(_)) => {}
                Ok(None) => {
                    self.append(std::mem::take(&mut batch));
                    self.lost("wyrd-sensord closed the connection");
                    client = None;
                    last_attempt = Instant::now();
                }
                Err(wyrd_sensor::client::ClientError::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => {
                    self.append(std::mem::take(&mut batch));
                    self.lost(&format!("lost the sensor connection: {e}"));
                    client = None;
                    last_attempt = Instant::now();
                }
            }
            if batch.len() >= BATCH || (!batch.is_empty() && last_flush.elapsed() >= FLUSH_EVERY) {
                self.append(std::mem::take(&mut batch));
                last_flush = Instant::now();
            }
        }
        self.append(batch);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drop_counts_add_up_loss_across_reports_and_reconnects() {
        let mut seen = DropCount::default();
        // The first report is the baseline: loss before it was not seen here.
        assert_eq!(
            seen.observe(4, 0),
            Grew {
                ring: false,
                queue: false,
            }
        );
        assert_eq!(seen.pending, 0);
        // Ring-buffer loss grows daemon-wide; queue loss grows this connection.
        assert_eq!(
            seen.observe(6, 5),
            Grew {
                ring: true,
                queue: true,
            }
        );
        assert_eq!(seen.pending, 2 + 5);
        // Unchanged counts add nothing.
        assert_eq!(
            seen.observe(6, 5),
            Grew {
                ring: false,
                queue: false,
            }
        );
        assert_eq!(seen.pending, 7);
        // A reconnect starts the daemon's queue count again from zero, so
        // its first losses must count, even below the old total.
        seen.new_connection();
        seen.observe(6, 0);
        seen.observe(6, 3);
        assert_eq!(seen.pending, 10);
    }

    #[test]
    fn drain_ends_on_its_own_flush_answer_or_the_bound() {
        let since = Instant::now();
        let d = Drain {
            token: 7,
            since,
            received: 0,
        };
        assert!(d.confirmed_by(&ServerMsg::Flushed { token: 7 }));
        assert!(!d.confirmed_by(&ServerMsg::Flushed { token: 8 }));
        assert!(!d.confirmed_by(&ServerMsg::Drops {
            total: 0,
            queue_dropped: 0,
        }));
        assert!(!d.expired(since + DRAIN_BOUND.saturating_sub(Duration::from_millis(1))));
        assert!(d.expired(since + DRAIN_BOUND));
    }

    #[test]
    fn egress_rules_combine_prefixes_with_resolved_hosts() {
        let plan = EgressPlan {
            cidrs: vec!["127.0.0.0/8".into()],
            hosts: vec!["api.example".into(), "gone.example".into()],
            ports: vec![443],
            restrict_hosts: true,
            policy_id: "p".into(),
            policy_version: "b3:0".into(),
        };
        let fake = |h: &str| -> Vec<std::net::IpAddr> {
            match h {
                "api.example" => vec![
                    "203.0.113.7".parse().unwrap(),
                    "2001:db8::7".parse().unwrap(),
                ],
                _ => Vec::new(),
            }
        };
        let r = plan.rules(&fake);
        assert_eq!(
            r.cidrs,
            ["127.0.0.0/8", "2001:db8::7/128", "203.0.113.7/32"]
        );
        assert!(r.restrict_hosts);
        assert_eq!(r.ports, [443]);
        // A changed resolution changes the rules (and so triggers an update).
        let moved = |_: &str| vec!["198.51.100.1".parse().unwrap()];
        assert_ne!(plan.rules(&moved), r);
    }
}
