//! `wyrd-sensord`: the only privileged Wyrdline process. It loads the eBPF
//! programs and streams each registered cgroup's events to the user who
//! owns that cgroup. No network, no persistent storage.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender, TrySendError};
use std::time::{Duration, Instant};

use clap::Parser;
use wyrd_sensor::client::{ClientMsg, MAX_REQUEST, ServerMsg};
use wyrd_sensor::{Decoder, Sensor, SensorConfig};

/// Privileged eBPF sensor daemon.
#[derive(Debug, Parser)]
#[command(name = "wyrd-sensord", version, about)]
struct Args {
    /// Unix socket to serve.
    #[arg(long, default_value = wyrd_sensor::client::DEFAULT_SOCKET)]
    socket: PathBuf,
    /// Ring buffer size in MiB (power of two).
    #[arg(long, default_value_t = 64)]
    ringbuf_mb: u32,
    /// Ring buffer size in KiB (overrides --ringbuf-mb; tests only).
    #[arg(long, hide = true)]
    ringbuf_kb: Option<u32>,
    /// Pause between ring-buffer reads, in milliseconds (tests only: makes
    /// overflow deterministic).
    #[arg(long, hide = true, default_value_t = 0)]
    read_pause_ms: u64,
    /// Events queued per client before its events are counted as dropped
    /// (tests only: makes a stalled client overflow quickly).
    #[arg(long, hide = true, default_value_t = CLIENT_QUEUE)]
    client_queue: usize,
    /// Exit when standard input reaches end of file (tests, supervisors).
    #[arg(long)]
    exit_on_stdin_eof: bool,
}

/// Commands from connection threads to the sensor thread.
enum Cmd {
    /// `ack` is answered once the cgroup is in the kernel map, so the client
    /// may start its workload without losing the first events.
    Register {
        conn: u64,
        cgroup_id: u64,
        level: u32,
        workspace: PathBuf,
        /// The cgroup directory and the rules to enforce in it.
        egress: Option<(PathBuf, wyrd_sensor::EgressRules)>,
        tx: Queue,
        /// `Ok(Some(why))`: registered, but the egress rules are not enforced.
        ack: SyncSender<Result<Option<String>, String>>,
    },
    Egress {
        cgroup_id: u64,
        dir: PathBuf,
        rules: wyrd_sensor::EgressRules,
    },
    /// Answer `Flushed { token }` on `conn` after the events already in the
    /// ring buffer have been dispatched.
    Flush {
        conn: u64,
        cgroup_id: u64,
        token: u64,
    },
    Unregister {
        conn: u64,
        cgroup_id: u64,
    },
}

const CGROUP_ROOT: &str = "/sys/fs/cgroup";
const CLIENT_QUEUE: usize = 65_536;

fn main() {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .init();
    let args = Args::parse();
    if let Err(e) = run(&args) {
        tracing::error!("{e}");
        std::process::exit(1);
    }
}

fn run(args: &Args) -> Result<(), String> {
    let bytes = args
        .ringbuf_kb
        .map_or(args.ringbuf_mb.saturating_mul(1 << 20), |kb| {
            kb.saturating_mul(1024)
        });
    let (sensor, report) = Sensor::load(&SensorConfig {
        ringbuf_bytes: bytes,
    })
    .map_err(|e| e.to_string())?;
    tracing::info!(attached = report.attached.len(), skipped = ?report.skipped, "sensor loaded");
    let skipped: Vec<String> = report.skipped.iter().map(|(n, _)| n.clone()).collect();

    if let Some(dir) = args.socket.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    let _ = std::fs::remove_file(&args.socket);
    let listener =
        UnixListener::bind(&args.socket).map_err(|e| format!("{}: {e}", args.socket.display()))?;
    // Any local user may connect; authorisation is per cgroup (SO_PEERCRED).
    std::fs::set_permissions(&args.socket, std::fs::Permissions::from_mode(0o666))
        .map_err(|e| e.to_string())?;
    tracing::info!(socket = %args.socket.display(), "listening");

    let (cmd_tx, cmd_rx) = mpsc::channel::<Cmd>();
    std::thread::Builder::new()
        .name("sensor".into())
        .spawn({
            let pause = Duration::from_millis(args.read_pause_ms);
            move || sensor_loop(sensor, &cmd_rx, pause)
        })
        .map_err(|e| e.to_string())?;

    if args.exit_on_stdin_eof {
        let socket = args.socket.clone();
        std::thread::spawn(move || {
            let mut sink = Vec::new();
            let _ = std::io::stdin().read_to_end(&mut sink);
            let _ = std::fs::remove_file(&socket);
            std::process::exit(0);
        });
    }

    let attached = report.attached.len();
    let queue = args.client_queue;
    let mut next_conn = 0u64;
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        next_conn += 1;
        let conn = next_conn;
        let cmd_tx = cmd_tx.clone();
        let skipped = skipped.clone();
        let _ = std::thread::Builder::new()
            .name(format!("conn-{conn}"))
            .spawn(move || serve(stream, conn, &cmd_tx, attached, skipped, queue));
    }
    Ok(())
}

/// Validate a registration: the cgroup must be a real cgroup directory and
/// owned by the peer (root may register any).
fn authorise(cgroup: &Path, peer_uid: u32) -> Result<(PathBuf, u64), String> {
    let canon = std::fs::canonicalize(cgroup).map_err(|e| format!("{}: {e}", cgroup.display()))?;
    if !canon.starts_with(CGROUP_ROOT) || !canon.join("cgroup.procs").exists() {
        return Err(format!("{} is not a cgroup v2 directory", canon.display()));
    }
    let meta = std::fs::metadata(&canon).map_err(|e| e.to_string())?;
    if peer_uid != 0 && meta.uid() != peer_uid {
        return Err(format!(
            "{} is not owned by uid {peer_uid}",
            canon.display()
        ));
    }
    Ok((canon, meta.ino()))
}

fn send(stream: &mut UnixStream, msg: &ServerMsg) -> std::io::Result<()> {
    let mut line = serde_json::to_vec(msg).map_err(std::io::Error::other)?;
    line.push(b'\n');
    stream.write_all(&line)
}

fn serve(
    mut stream: UnixStream,
    conn: u64,
    cmd_tx: &Sender<Cmd>,
    attached: usize,
    skipped: Vec<String>,
    queue: usize,
) {
    let peer_uid = match rustix::net::sockopt::socket_peercred(&stream) {
        Ok(c) => c.uid.as_raw(),
        Err(_) => return,
    };
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let mut line = String::new();
    let read = BufReader::new(&stream)
        .take(MAX_REQUEST as u64)
        .read_line(&mut line);
    if read.is_err() || line.is_empty() {
        return;
    }
    let (cgroup, workspace, egress) = match serde_json::from_str::<ClientMsg>(&line) {
        Ok(ClientMsg::Register {
            cgroup,
            workspace,
            egress,
        }) => (cgroup, workspace, egress),
        Ok(ClientMsg::Egress { .. } | ClientMsg::Flush { .. }) => {
            let _ = send(
                &mut stream,
                &ServerMsg::Error {
                    message: "register first".into(),
                },
            );
            return;
        }
        Err(e) => {
            let _ = send(
                &mut stream,
                &ServerMsg::Error {
                    message: format!("bad request: {e}"),
                },
            );
            return;
        }
    };
    let (canon, cgroup_id) = match authorise(&cgroup, peer_uid) {
        Ok(v) => v,
        Err(message) => {
            tracing::warn!(peer_uid, %message, "registration refused");
            let _ = send(&mut stream, &ServerMsg::Error { message });
            return;
        }
    };
    let inflight = Arc::new(AtomicUsize::new(0));
    let (tx, rx) = mpsc::sync_channel(queue + CONTROL_RESERVE);
    let tx = Queue {
        tx,
        inflight: inflight.clone(),
        events: queue,
    };
    let (ack_tx, ack_rx) = mpsc::sync_channel(1);
    if cmd_tx
        .send(Cmd::Register {
            conn,
            cgroup_id,
            level: wyrd_sensor::loader::cgroup_level(&canon),
            workspace,
            egress: egress.map(|r| (canon.clone(), r)),
            tx,
            ack: ack_tx,
        })
        .is_err()
    {
        return;
    }
    let egress_error = match ack_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(why)) => why,
        Ok(Err(message)) => {
            let _ = send(&mut stream, &ServerMsg::Error { message });
            return;
        }
        Err(_) => {
            let _ = send(
                &mut stream,
                &ServerMsg::Error {
                    message: "sensor thread unavailable".into(),
                },
            );
            return;
        }
    };
    tracing::info!(peer_uid, cgroup = %canon.display(), cgroup_id, conn, "registered");
    if send(
        &mut stream,
        &ServerMsg::Registered {
            cgroup_id,
            attached,
            skipped,
            egress_error,
        },
    )
    .is_ok()
    {
        updates(&stream, cmd_tx, conn, cgroup_id, &canon);
        forward(&mut stream, &rx, &inflight);
    }
    let _ = cmd_tx.send(Cmd::Unregister { conn, cgroup_id });
    tracing::info!(cgroup_id, conn, "unregistered");
}

/// Egress rule updates and flush requests from the client, on their own
/// thread; they can only ever address the cgroup this connection registered.
fn updates(stream: &UnixStream, cmd_tx: &Sender<Cmd>, conn: u64, cgroup_id: u64, dir: &Path) {
    let Ok(read) = stream.try_clone() else {
        return;
    };
    let _ = read.set_read_timeout(None);
    let cmd_tx = cmd_tx.clone();
    let dir = dir.to_path_buf();
    let _ = std::thread::Builder::new()
        .name("sensord-updates".into())
        .spawn(move || {
            let mut reader = BufReader::new(read);
            loop {
                let mut line = String::new();
                match (&mut reader).take(MAX_REQUEST as u64).read_line(&mut line) {
                    Ok(0) | Err(_) => return,
                    Ok(_) => {}
                }
                match serde_json::from_str::<ClientMsg>(&line) {
                    Ok(ClientMsg::Egress { rules }) => {
                        let _ = cmd_tx.send(Cmd::Egress {
                            cgroup_id,
                            dir: dir.clone(),
                            rules,
                        });
                    }
                    Ok(ClientMsg::Flush { token }) => {
                        let _ = cmd_tx.send(Cmd::Flush {
                            conn,
                            cgroup_id,
                            token,
                        });
                    }
                    Ok(ClientMsg::Register { .. }) | Err(_) => {
                        tracing::warn!(cgroup_id, "ignored an invalid update");
                    }
                }
            }
        });
}

/// Write queued messages until the client goes away. Periodic `drops`
/// messages double as a liveness check.
fn forward(stream: &mut UnixStream, rx: &Receiver<ServerMsg>, inflight: &AtomicUsize) {
    loop {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(msg) => {
                inflight.fetch_sub(1, Ordering::SeqCst);
                if send(stream, &msg).is_err() {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

/// Extra slots kept for control messages (loss reports and flush answers),
/// so a full event queue cannot lose the flush barrier.
const CONTROL_RESERVE: usize = 16;

/// The sensor side of one client's queue. The channel holds `events` event
/// slots plus `CONTROL_RESERVE`. `inflight` counts messages not yet written
/// out, and events are admitted only while below `events`.
struct Queue {
    tx: SyncSender<ServerMsg>,
    inflight: Arc<AtomicUsize>,
    events: usize,
}

/// Outcome of offering a message to a client's queue.
enum Offer {
    Sent,
    /// No room under the admission limit.
    Full,
    /// The client is gone; `remove` unregisters it.
    Gone,
}

impl Queue {
    /// Queue `msg` if fewer than `limit` messages are waiting. Never blocks.
    /// Only the sensor thread offers, so the count cannot race upward.
    fn offer(&self, msg: ServerMsg, limit: usize) -> Offer {
        if self.inflight.fetch_add(1, Ordering::SeqCst) >= limit {
            self.inflight.fetch_sub(1, Ordering::SeqCst);
            return Offer::Full;
        }
        match self.tx.try_send(msg) {
            Ok(()) => Offer::Sent,
            Err(e) => {
                self.inflight.fetch_sub(1, Ordering::SeqCst);
                match e {
                    TrySendError::Full(_) => Offer::Full,
                    TrySendError::Disconnected(_) => Offer::Gone,
                }
            }
        }
    }
}

/// One connection's share of a cgroup's events.
struct Sub {
    conn: u64,
    queue: Queue,
    /// Events this connection's queue refused because the client was not
    /// reading fast enough. Cumulative; every `drops` message carries it.
    queue_dropped: u64,
}

type Subscribers = HashMap<u64, Vec<Sub>>;

/// Queue `msg` for every subscriber of a cgroup. A full queue means a
/// stalled client: that client's drop count goes up, and the other sessions
/// are not blocked. A disconnected queue is left to `remove`.
fn fan_out(subs: &mut [Sub], msg: &ServerMsg) {
    for sub in subs {
        match sub.queue.offer(msg.clone(), sub.queue.events) {
            Offer::Sent | Offer::Gone => {}
            Offer::Full => {
                if sub.queue_dropped == 0 {
                    tracing::warn!(
                        conn = sub.conn,
                        "client is not reading fast enough; dropping its events"
                    );
                }
                sub.queue_dropped += 1;
            }
        }
    }
}

impl Sub {
    /// The loss report: ring-buffer total plus this connection's queue
    /// count. Heartbeats pass the event limit and never use the reserve; the
    /// next report carries the same cumulative numbers.
    fn report(&self, total: u64, limit: usize) -> Offer {
        self.queue.offer(
            ServerMsg::Drops {
                total,
                queue_dropped: self.queue_dropped,
            },
            limit,
        )
    }
}

/// Periodic liveness and loss report to every connection of a cgroup.
fn heartbeat(subs: &[Sub], total: u64) {
    for sub in subs {
        sub.report(total, sub.queue.events);
    }
}

/// Answer a flush request: the loss report, then `Flushed`, both through the
/// control reserve, so a full event queue cannot lose them. Never blocks.
fn answer_flush(sub: &Sub, token: u64, total: u64) {
    let limit = sub.queue.events + CONTROL_RESERVE;
    sub.report(total, limit);
    let _ = sub.queue.offer(ServerMsg::Flushed { token }, limit);
}

fn sensor_loop(mut sensor: Sensor, cmds: &Receiver<Cmd>, pause: Duration) {
    let mut decoder = Decoder::new();
    let mut subs: Subscribers = HashMap::new();
    let mut last_drops = Instant::now();
    let mut flushes: Vec<(u64, u64, u64)> = Vec::new();
    loop {
        while let Ok(cmd) = cmds.try_recv() {
            match cmd {
                Cmd::Register {
                    conn,
                    cgroup_id,
                    level,
                    workspace,
                    egress,
                    tx,
                    ack,
                } => {
                    if let Err(e) = sensor.register(cgroup_id, level) {
                        let _ = ack.try_send(Err(e.to_string()));
                        continue;
                    }
                    let not_enforced = egress.and_then(|(dir, rules)| {
                        sensor.enforce(cgroup_id, &dir, &rules).err().map(|e| {
                            tracing::error!(cgroup_id, error = %e, "egress not enforced");
                            e.to_string()
                        })
                    });
                    decoder.set_workspace(cgroup_id, workspace);
                    subs.entry(cgroup_id).or_default().push(Sub {
                        conn,
                        queue: tx,
                        queue_dropped: 0,
                    });
                    let _ = ack.try_send(Ok(not_enforced));
                }
                Cmd::Egress {
                    cgroup_id,
                    dir,
                    rules,
                } => {
                    if subs.contains_key(&cgroup_id)
                        && let Err(e) = sensor.enforce(cgroup_id, &dir, &rules)
                    {
                        tracing::error!(cgroup_id, error = %e, "egress update failed");
                    }
                }
                Cmd::Flush {
                    conn,
                    cgroup_id,
                    token,
                } => flushes.push((conn, cgroup_id, token)),
                Cmd::Unregister { conn, cgroup_id } => {
                    remove(&mut sensor, &mut decoder, &mut subs, cgroup_id, conn);
                }
            }
        }
        if !pause.is_zero() {
            std::thread::sleep(pause);
        }
        let events = match sensor.poll(Duration::from_millis(10)) {
            Ok(e) => e,
            Err(e) => {
                tracing::error!("ring buffer: {e}");
                std::thread::sleep(Duration::from_millis(100));
                continue;
            }
        };
        for raw in &events {
            let Some(d) = decoder.decode(raw) else {
                continue;
            };
            let msg = ServerMsg::Event {
                ts_mono_ns: d.ts_mono_ns,
                process: d.process,
                body: d.body,
            };
            if let Some(list) = subs.get_mut(&d.cgroup_id) {
                fan_out(list, &msg);
            }
        }
        // `poll` drained the ring buffer, so everything the kernel queued
        // before these requests arrived is now ahead of the answer in the
        // connection's queue.
        // The loss report goes first, so the client sees every drop known at
        // the barrier before it stops reading.
        for (conn, cgroup_id, token) in flushes.drain(..) {
            if let Some(list) = subs.get(&cgroup_id) {
                for sub in list.iter().filter(|s| s.conn == conn) {
                    answer_flush(sub, token, sensor.drops().unwrap_or(0));
                }
            }
        }
        if last_drops.elapsed() >= Duration::from_secs(1) {
            last_drops = Instant::now();
            let total = sensor.drops().unwrap_or(0);
            for list in subs.values() {
                heartbeat(list, total);
            }
        }
    }
}

fn remove(
    sensor: &mut Sensor,
    decoder: &mut Decoder,
    subs: &mut Subscribers,
    cgroup_id: u64,
    conn: u64,
) {
    if let Some(list) = subs.get_mut(&cgroup_id) {
        for sub in list
            .iter()
            .filter(|s| s.conn == conn && s.queue_dropped > 0)
        {
            tracing::warn!(
                conn,
                cgroup_id,
                dropped = sub.queue_dropped,
                "client dropped events it did not read in time"
            );
        }
        list.retain(|s| s.conn != conn);
        if list.is_empty() {
            subs.remove(&cgroup_id);
            let _ = sensor.unregister(cgroup_id);
            decoder.forget_cgroup(cgroup_id);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub(conn: u64, events: usize) -> (Sub, Receiver<ServerMsg>, Arc<AtomicUsize>) {
        let inflight = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = mpsc::sync_channel(events + CONTROL_RESERVE);
        let queue = Queue {
            tx,
            inflight: inflight.clone(),
            events,
        };
        (
            Sub {
                conn,
                queue,
                queue_dropped: 0,
            },
            rx,
            inflight,
        )
    }

    /// What the writer thread does: take the next message, freeing its slot.
    fn take(rx: &Receiver<ServerMsg>, inflight: &AtomicUsize) -> Option<ServerMsg> {
        let msg = rx.try_recv().ok()?;
        inflight.fetch_sub(1, Ordering::SeqCst);
        Some(msg)
    }

    fn drain(rx: &Receiver<ServerMsg>, inflight: &AtomicUsize) -> Vec<ServerMsg> {
        std::iter::from_fn(|| take(rx, inflight)).collect()
    }

    fn msg(token: u64) -> ServerMsg {
        ServerMsg::Flushed { token }
    }

    #[test]
    fn a_client_that_stops_reading_has_its_drops_counted_and_reported() {
        let (s, rx, inflight) = sub(1, 2);
        let mut subs = vec![s];
        for token in 0..5 {
            fan_out(&mut subs, &msg(token));
        }
        // Two fit in the queue; the other three were refused and counted.
        assert_eq!(subs[0].queue_dropped, 3);
        // The client reads again: the two queued messages arrive first, then
        // the next heartbeat reports the loss.
        assert_eq!(take(&rx, &inflight), Some(msg(0)));
        assert_eq!(take(&rx, &inflight), Some(msg(1)));
        heartbeat(&subs, 7);
        assert_eq!(
            take(&rx, &inflight),
            Some(ServerMsg::Drops {
                total: 7,
                queue_dropped: 3,
            })
        );
    }

    #[test]
    fn a_client_that_keeps_up_has_no_drops() {
        let (s, rx, inflight) = sub(1, 2);
        let mut subs = vec![s];
        for token in 0..10 {
            fan_out(&mut subs, &msg(token));
            assert_eq!(take(&rx, &inflight), Some(msg(token)));
        }
        assert_eq!(subs[0].queue_dropped, 0);
    }

    #[test]
    fn a_stalled_client_does_not_drop_events_for_a_reading_client() {
        let (slow, _slow_rx, _) = sub(1, 1);
        let (fast, fast_rx, fast_inflight) = sub(2, 8);
        let mut subs = vec![slow, fast];
        for token in 0..4 {
            fan_out(&mut subs, &msg(token));
            assert_eq!(take(&fast_rx, &fast_inflight), Some(msg(token)));
        }
        assert_eq!(subs[0].queue_dropped, 3);
        assert_eq!(subs[1].queue_dropped, 0);
    }

    #[test]
    fn a_gone_client_is_not_counted_as_dropping() {
        let (s, rx, _) = sub(1, 1);
        drop(rx);
        let mut subs = vec![s];
        fan_out(&mut subs, &msg(0));
        assert_eq!(subs[0].queue_dropped, 0);
    }

    #[test]
    fn a_full_queue_still_gets_the_flush_barrier_and_the_final_count() {
        let (s, rx, inflight) = sub(1, 2);
        let mut subs = vec![s];
        for token in 0..5 {
            fan_out(&mut subs, &msg(token));
        }
        assert_eq!(subs[0].queue_dropped, 3);
        // The queue is full: a heartbeat is refused, the barrier is not.
        heartbeat(&subs, 42);
        assert_eq!(subs[0].queue.inflight.load(Ordering::SeqCst), 2);
        answer_flush(&subs[0], 9, 42);
        assert_eq!(
            drain(&rx, &inflight),
            vec![
                msg(0),
                msg(1),
                ServerMsg::Drops {
                    total: 42,
                    queue_dropped: 3,
                },
                msg(9),
            ]
        );
    }
}
