#![cfg(feature = "privileged-tests")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::cast_precision_loss,
    clippy::print_stderr
)]

//! Sensor measurements (release build, `make test-perf`). Results are
//! printed and asserted against the plan's targets (§18.2).

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use wyrd_schema::EventBody;
use wyrd_sensor::{SensorClient, ServerMsg};

struct Daemon {
    child: Child,
    socket: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        drop(self.child.stdin.take());
        let _ = self.child.wait();
    }
}

fn daemon(dir: &Path) -> Daemon {
    let socket = dir.join("perf.sock");
    let child = Command::new("sudo")
        .args([
            "-n",
            "--",
            env!("CARGO_BIN_EXE_wyrd-sensord"),
            "--exit-on-stdin-eof",
            "--socket",
        ])
        .arg(&socket)
        .stdin(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !socket.exists() {
        assert!(start.elapsed() < Duration::from_secs(20));
        std::thread::sleep(Duration::from_millis(50));
    }
    Daemon { child, socket }
}

/// Run `script` in a fresh user scope, released through a FIFO once
/// `before_release` (given the scope's cgroup) has run. Returns wall time.
fn run_scoped(
    dir: &Path,
    name: &str,
    script: &str,
    before_release: impl FnOnce(&Path),
) -> Duration {
    let fifo = dir.join(format!("{name}.fifo"));
    let _ = std::fs::remove_file(&fifo);
    Command::new("mkfifo").arg(&fifo).status().unwrap();
    let unit = format!("wyrd-perf-{}-{name}", std::process::id());
    let mut child = Command::new("systemd-run")
        .args([
            "--user",
            "--scope",
            "--quiet",
            "--collect",
            "--unit",
            &unit,
            "--",
            "sh",
            "-c",
        ])
        .arg(format!("read go < {}; {script}", fifo.display()))
        .spawn()
        .unwrap();
    let want = format!("{unit}.scope");
    let cgroup = loop {
        let text =
            std::fs::read_to_string(format!("/proc/{}/cgroup", child.id())).unwrap_or_default();
        if let Some(p) = text.lines().find_map(|l| l.strip_prefix("0::"))
            && p.trim().ends_with(&want)
        {
            break PathBuf::from("/sys/fs/cgroup").join(p.trim().trim_start_matches('/'));
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    before_release(&cgroup);
    let start = Instant::now();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&fifo)
        .unwrap()
        .write_all(b"go\n")
        .unwrap();
    assert!(child.wait().unwrap().success());
    start.elapsed()
}

/// What a reader saw.
#[derive(Debug, Default, Clone, Copy)]
struct Seen {
    events: u64,
    writes: u64,
    drops: u64,
}

/// Read until the workload has finished (`done`) and the stream has been
/// idle for 500 ms, or a 120 s deadline passes. Drops heartbeats keep the
/// connection busy, so read timeouts alone cannot end the loop.
fn read_all(mut client: SensorClient, done: &AtomicBool) -> Seen {
    client
        .set_timeout(Some(Duration::from_millis(200)))
        .unwrap();
    let mut seen = Seen::default();
    let mut last_event = Instant::now();
    let deadline = Instant::now() + Duration::from_secs(120);
    while Instant::now() < deadline {
        if done.load(Ordering::SeqCst) && last_event.elapsed() > Duration::from_millis(500) {
            break;
        }
        match client.recv() {
            Ok(Some(ServerMsg::Event { body, .. })) => {
                seen.events += 1;
                if matches!(body, EventBody::FileOpenWrite(_)) {
                    seen.writes += 1;
                }
                last_event = Instant::now();
            }
            Ok(Some(ServerMsg::Drops { total })) => seen.drops = total,
            Ok(Some(_)) | Err(_) => {}
            Ok(None) => break,
        }
    }
    seen
}

/// Run `script` in a scope with capture; returns wall time and what the
/// reader saw.
fn captured_run(
    dir: &Path,
    name: &str,
    script: &str,
    socket: &Path,
    ws: &Path,
) -> (Duration, Seen) {
    let done = Arc::new(AtomicBool::new(false));
    let mut reader = None;
    let t = run_scoped(dir, name, script, |cg| {
        let client = SensorClient::connect(socket, cg, ws).unwrap();
        let done = done.clone();
        reader = Some(std::thread::spawn(move || read_all(client, &done)));
    });
    done.store(true, Ordering::SeqCst);
    (t, reader.unwrap().join().unwrap())
}

#[test]
#[ignore = "release measurement; make test-perf"]
fn throughput_50k_events_per_second_without_loss() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let d = daemon(tmp.path());
    let n: u64 = 200_000;
    let script = format!(
        "cd {}; exec python3 -c 'import os\nfor i in range({n}):\n os.close(os.open(\"f%d\" % (i % 64), os.O_WRONLY | os.O_CREAT, 0o644))'",
        ws.display()
    );
    let (elapsed, seen) = captured_run(tmp.path(), "tput", &script, &d.socket, &ws);
    let rate = seen.writes as f64 / elapsed.as_secs_f64();
    eprintln!(
        "sensor throughput: {} events in {elapsed:.2?} = {rate:.0} events/s, drops {}",
        seen.writes, seen.drops
    );
    assert_eq!(seen.drops, 0, "no event may be lost");
    assert_eq!(seen.writes, n, "every open is accounted for");
    assert!(rate >= 50_000.0, "{rate:.0} events/s");
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

/// A build-like workload: a generated source tree committed to git,
/// archived, extracted, and grepped — process spawns, many small files,
/// real CPU work.
fn build_like(ws: &Path, round: usize) -> String {
    format!(
        "set -e; d={ws}/r{round}; mkdir -p $d; cd $d; \
         python3 -c 'import os\nfor i in range(1500):\n os.makedirs(\"src/m%d\" % (i % 50), exist_ok=True)\n open(\"src/m%d/f%d.rs\" % (i % 50, i), \"w\").write(\"fn f%d() {{ let x = %d; }}\\n\" % (i, i) * 40)'; \
         git init -q; git add -A; git -c user.name=p -c user.email=p@p commit -qm init; \
         tar czf ../r{round}.tgz src; mkdir out; tar xzf ../r{round}.tgz -C out; grep -rc fn out > /dev/null; \
         for i in $(seq 1 150); do /bin/true; done",
        ws = ws.display()
    )
}

#[test]
#[ignore = "release measurement; make test-perf"]
fn overhead_of_capture_on_a_build_like_workload() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let rounds = 9;
    let (mut base, mut unregistered, mut captured) = (Vec::new(), Vec::new(), Vec::new());
    let mut events = 0u64;
    // Interleaved per round so load changes on a shared machine affect all
    // three configurations alike.
    for r in 0..rounds {
        base.push(run_scoped(
            tmp.path(),
            &format!("base{r}"),
            &build_like(&ws, r),
            |_| {},
        ));
        let d = daemon(tmp.path());
        unregistered.push(run_scoped(
            tmp.path(),
            &format!("unreg{r}"),
            &build_like(&ws, 100 + r),
            |_| {},
        ));
        let (t, seen) = captured_run(
            tmp.path(),
            &format!("cap{r}"),
            &build_like(&ws, 200 + r),
            &d.socket,
            &ws,
        );
        captured.push(t);
        events += seen.events;
        drop(d);
        // Keep the disk small: each round leaves a generated tree behind.
        for k in [r, 100 + r, 200 + r] {
            let _ = std::fs::remove_dir_all(ws.join(format!("r{k}")));
            let _ = std::fs::remove_file(ws.join(format!("r{k}.tgz")));
        }
    }
    let (b, u, c) = (median(base), median(unregistered), median(captured));
    let pct = |x: Duration| (x.as_secs_f64() / b.as_secs_f64() - 1.0) * 100.0;
    eprintln!(
        "overhead (median of {rounds}): baseline {b:.2?}, sensor loaded but process not registered {u:.2?} ({:+.1}%), \
         captured {c:.2?} ({:+.1}%), {} events per run",
        pct(u),
        pct(c),
        events / rounds as u64
    );
    assert!(pct(c) <= 5.0, "capture overhead {:.1}% exceeds 5%", pct(c));
    assert!(
        pct(u) <= 5.0,
        "attached-but-filtered overhead {:.1}% exceeds 5%",
        pct(u)
    );
}
