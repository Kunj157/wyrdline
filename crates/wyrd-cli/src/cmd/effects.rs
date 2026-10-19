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

const BATCH: usize = 500;
const FLUSH_EVERY: Duration = Duration::from_millis(50);
const RECONNECT_EVERY: Duration = Duration::from_secs(1);
/// After the agent exits, keep reading this long for in-flight events.
const DRAIN: Duration = Duration::from_millis(300);

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
        let mut drops_total: Option<u64> = None;
        let mut pending_drops = 0u64;
        let mut batch: Vec<Event> = Vec::with_capacity(BATCH);
        let mut last_flush = Instant::now();
        let mut last_attempt = Instant::now();
        let mut stopping: Option<Instant> = None;
        let mut refreshed = Instant::now();
        let mut current = self.egress.as_ref().map(|e| e.rules(&resolve));
        loop {
            self.refresh_egress(client.as_ref(), &mut refreshed, &mut current);
            if stopping.is_none() && self.stop.load(Ordering::SeqCst) {
                stopping = Some(Instant::now());
            }
            if stopping.is_some_and(|t| t.elapsed() >= DRAIN) {
                break;
            }
            let Some(c) = client.as_mut() else {
                if stopping.is_some() {
                    break;
                }
                std::thread::sleep(Duration::from_millis(100));
                if last_attempt.elapsed() >= RECONNECT_EVERY {
                    last_attempt = Instant::now();
                    if let Ok(c) = self.connect() {
                        self.append(vec![meta(EventBody::SensorResumed(SensorResumed {
                            dropped: 0,
                        }))]);
                        let _ = Session::mark_recovered(&self.data, &self.session_id);
                        client = Some(c);
                    }
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
                    let drops =
                        u32::try_from(std::mem::take(&mut pending_drops)).unwrap_or(u32::MAX);
                    let guard = self.guard_for(&body);
                    batch.push(event(ts_mono_ns, Some(process), drops, body));
                    if let Some(g) = guard {
                        batch.push(event(ts_mono_ns, Some(process), 0, g));
                    }
                }
                Ok(Some(ServerMsg::Drops { total })) => {
                    let prev = *drops_total.get_or_insert(total);
                    if total > prev {
                        pending_drops += total - prev;
                        if let Ok(mut w) = self.warning.lock() {
                            w.get_or_insert_with(|| {
                                "the sensor's ring buffer overflowed; some effects were lost".into()
                            });
                        }
                    }
                    drops_total = Some(total);
                }
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
