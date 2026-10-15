//! `wyrd-sensord`: the only privileged Wyrdline process. It loads the eBPF
//! programs and streams each registered cgroup's events to the user who
//! owns that cgroup. No network, no persistent storage.

use std::collections::HashMap;
use std::io::{BufRead as _, BufReader, Read as _, Write as _};
use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, SyncSender};
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
        workspace: PathBuf,
        tx: SyncSender<ServerMsg>,
        ack: SyncSender<Result<(), String>>,
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
    let mut next_conn = 0u64;
    for stream in listener.incoming() {
        let Ok(stream) = stream else { continue };
        next_conn += 1;
        let conn = next_conn;
        let cmd_tx = cmd_tx.clone();
        let skipped = skipped.clone();
        let _ = std::thread::Builder::new()
            .name(format!("conn-{conn}"))
            .spawn(move || serve(stream, conn, &cmd_tx, attached, skipped));
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
    let (cgroup, workspace) = match serde_json::from_str::<ClientMsg>(&line) {
        Ok(ClientMsg::Register { cgroup, workspace }) => (cgroup, workspace),
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
    let (tx, rx) = mpsc::sync_channel(CLIENT_QUEUE);
    let (ack_tx, ack_rx) = mpsc::sync_channel(1);
    if cmd_tx
        .send(Cmd::Register {
            conn,
            cgroup_id,
            workspace,
            tx,
            ack: ack_tx,
        })
        .is_err()
    {
        return;
    }
    match ack_rx.recv_timeout(Duration::from_secs(5)) {
        Ok(Ok(())) => {}
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
    }
    tracing::info!(peer_uid, cgroup = %canon.display(), cgroup_id, conn, "registered");
    if send(
        &mut stream,
        &ServerMsg::Registered {
            cgroup_id,
            attached,
            skipped,
        },
    )
    .is_ok()
    {
        forward(&mut stream, &rx);
    }
    let _ = cmd_tx.send(Cmd::Unregister { conn, cgroup_id });
    tracing::info!(cgroup_id, conn, "unregistered");
}

/// Write queued messages until the client goes away. Periodic `drops`
/// messages double as a liveness check.
fn forward(stream: &mut UnixStream, rx: &Receiver<ServerMsg>) {
    loop {
        match rx.recv_timeout(Duration::from_secs(5)) {
            Ok(msg) => {
                if send(stream, &msg).is_err() {
                    return;
                }
            }
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return,
        }
    }
}

type Subscribers = HashMap<u64, Vec<(u64, SyncSender<ServerMsg>)>>;

fn sensor_loop(mut sensor: Sensor, cmds: &Receiver<Cmd>, pause: Duration) {
    let mut decoder = Decoder::new();
    let mut subs: Subscribers = HashMap::new();
    let mut last_drops = Instant::now();
    loop {
        while let Ok(cmd) = cmds.try_recv() {
            match cmd {
                Cmd::Register {
                    conn,
                    cgroup_id,
                    workspace,
                    tx,
                    ack,
                } => {
                    if let Err(e) = sensor.register(cgroup_id, 1) {
                        let _ = ack.try_send(Err(e.to_string()));
                        continue;
                    }
                    decoder.set_workspace(cgroup_id, workspace);
                    subs.entry(cgroup_id).or_default().push((conn, tx));
                    let _ = ack.try_send(Ok(()));
                }
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
            if let Some(list) = subs.get(&d.cgroup_id) {
                for (_, tx) in list {
                    // A full queue means a stalled client: count it as a drop
                    // rather than blocking every session.
                    let _ = tx.try_send(msg.clone());
                }
            }
        }
        if last_drops.elapsed() >= Duration::from_secs(1) {
            last_drops = Instant::now();
            let total = sensor.drops().unwrap_or(0);
            for list in subs.values() {
                for (_, tx) in list {
                    let _ = tx.try_send(ServerMsg::Drops { total });
                }
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
        list.retain(|(c, _)| *c != conn);
        if list.is_empty() {
            subs.remove(&cgroup_id);
            let _ = sensor.unregister(cgroup_id);
            decoder.forget_cgroup(cgroup_id);
        }
    }
}
