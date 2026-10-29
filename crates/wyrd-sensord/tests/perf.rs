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
            Ok(Some(ServerMsg::Drops { total, .. })) => seen.drops = total,
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

/// Event loss at the plan's rate: the generator opens files at a fixed
/// 50 k/s (busy-wait pacing, so the schedule does not drift with a slow
/// open) and every one must arrive.
#[test]
#[ignore = "release measurement; make test-perf"]
fn throughput_paced_50k_events_per_second_without_loss() {
    const RATE: f64 = 50_000.0;
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let d = daemon(tmp.path());
    let n: u64 = 200_000;
    let script = format!(
        "cd {}; exec python3 -c 'import os,time\nt0=time.perf_counter()\nfor i in range({n}):\n while time.perf_counter() < t0 + i / {RATE}: pass\n os.close(os.open(\"f%d\" % (i % 64), os.O_WRONLY | os.O_CREAT, 0o644))'",
        ws.display()
    );
    let (elapsed, seen) = captured_run(tmp.path(), "paced", &script, &d.socket, &ws);
    let rate = seen.writes as f64 / elapsed.as_secs_f64();
    eprintln!(
        "sensor paced throughput: {} events in {elapsed:.2?} = {rate:.0} events/s (target {RATE:.0}), drops {}",
        seen.writes, seen.drops
    );
    assert_eq!(seen.drops, 0, "no event may be lost at 50 k/s");
    assert_eq!(seen.writes, n, "every open is accounted for");
    assert!(rate >= 0.9 * RATE, "generator ran at {rate:.0} events/s");
}

fn median(mut v: Vec<Duration>) -> Duration {
    v.sort();
    v[v.len() / 2]
}

/// A syscall-dense workload: a generated source tree committed to git,
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

/// Medians of an interleaved measurement: baseline, sensor loaded but the
/// workload not registered, and workload captured; plus events per run.
struct Overhead {
    base: Duration,
    unregistered: Duration,
    captured: Duration,
    events: u64,
}

impl Overhead {
    fn pct(&self, x: Duration) -> f64 {
        (x.as_secs_f64() / self.base.as_secs_f64() - 1.0) * 100.0
    }

    fn report(&self, what: &str, rounds: usize) {
        eprintln!(
            "{what} overhead (median of {rounds}): baseline {:.2?}, sensor loaded but process not registered {:.2?} ({:+.1}%), \
             captured {:.2?} ({:+.1}%), {} events per run",
            self.base,
            self.unregistered,
            self.pct(self.unregistered),
            self.captured,
            self.pct(self.captured),
            self.events
        );
    }
}

/// Run `workload(ws, round)` in the three configurations, interleaved per round
/// so load changes on a shared machine affect all of them alike. `cleanup`
/// runs after every round.
fn measure_overhead(
    rounds: usize,
    workload: impl Fn(&Path, usize) -> String,
    cleanup: impl Fn(&Path, usize),
) -> Overhead {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let (mut base, mut unregistered, mut captured) = (Vec::new(), Vec::new(), Vec::new());
    let mut events = 0u64;
    for r in 0..rounds {
        base.push(run_scoped(
            tmp.path(),
            &format!("base{r}"),
            &workload(&ws, r),
            |_| {},
        ));
        let d = daemon(tmp.path());
        unregistered.push(run_scoped(
            tmp.path(),
            &format!("unreg{r}"),
            &workload(&ws, 100 + r),
            |_| {},
        ));
        let (t, seen) = captured_run(
            tmp.path(),
            &format!("cap{r}"),
            &workload(&ws, 200 + r),
            &d.socket,
            &ws,
        );
        captured.push(t);
        events += seen.events;
        drop(d);
        for k in [r, 100 + r, 200 + r] {
            cleanup(&ws, k);
        }
    }
    Overhead {
        base: median(base),
        unregistered: median(unregistered),
        captured: median(captured),
        events: events / rounds as u64,
    }
}

/// The plan's overhead target (§18.2, Phase 5): a debug `cargo build` of a
/// mid-size crate with its dependencies (`wyrd-correlator`: serde, syn,
/// schemars, ciborium, ts-rs), offline, into a fresh target directory.
#[test]
#[ignore = "release measurement; make test-perf"]
fn overhead_of_capture_on_cargo_build() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let targets = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let cargo = env!("CARGO");
    let rounds = 5;
    let o = measure_overhead(
        rounds,
        |_, r| {
            format!(
                "cd {}; exec {cargo} build -q --offline --locked -j 4 -p wyrd-correlator --target-dir {}/t{r}",
                root.display(),
                targets.path().display()
            )
        },
        |_, k| {
            let _ = std::fs::remove_dir_all(targets.path().join(format!("t{k}")));
        },
    );
    o.report("cargo build", rounds);
    assert!(
        o.pct(o.captured) <= 5.0,
        "capture overhead {:.1}% exceeds 5%",
        o.pct(o.captured)
    );
    assert!(
        o.pct(o.unregistered) <= 5.0,
        "attached-but-filtered overhead {:.1}% exceeds 5%",
        o.pct(o.unregistered)
    );
}

/// The plan's second overhead workload (§18.2): the test script of the
/// demo repository, `pnpm test` in `demo/victim-shop` (a `node --test` run
/// over `src/*.test.ts`). The command is the script body, run directly so
/// the measurement does not include pnpm's own startup.
#[test]
#[ignore = "release measurement; make test-perf"]
fn overhead_of_capture_on_victim_shop_tests() {
    let shop = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../demo/victim-shop");
    let rounds = 5;
    let o = measure_overhead(
        rounds,
        |_, _| {
            // The demo suite fails one test on purpose (demo/victim-shop
            // README), so exit status 1 (test failures) is accepted; a
            // missing node (127) still fails the measurement.
            format!(
                "cd {}; node --experimental-strip-types --test src/*.test.ts > /dev/null 2>&1; [ $? -le 1 ]",
                shop.display()
            )
        },
        |_, _| {},
    );
    o.report("victim-shop tests", rounds);
    assert!(
        o.pct(o.captured) <= 5.0,
        "capture overhead {:.1}% exceeds 5%",
        o.pct(o.captured)
    );
    assert!(
        o.pct(o.unregistered) <= 5.0,
        "attached-but-filtered overhead {:.1}% exceeds 5%",
        o.pct(o.unregistered)
    );
}

/// Worst case, not the plan's target workload: a syscall-dense script
/// (~20 k effects/s, several times a real build). The cost every
/// unregistered process pays stays within the 5 % budget. Captured runs are
/// held to a regression bound of 12 % — today's design costs ~9 % here (see
/// `docs/results/perf-sensor.md`) — so a slower kernel path fails the job.
#[test]
#[ignore = "release measurement; make test-perf"]
fn overhead_of_capture_on_a_syscall_dense_workload() {
    let rounds = 9;
    let o = measure_overhead(rounds, build_like, |ws, k| {
        // Keep the disk small: each round leaves a generated tree behind.
        let _ = std::fs::remove_dir_all(ws.join(format!("r{k}")));
        let _ = std::fs::remove_file(ws.join(format!("r{k}.tgz")));
    });
    o.report("syscall-dense", rounds);
    assert!(
        o.pct(o.unregistered) <= 5.0,
        "attached-but-filtered overhead {:.1}% exceeds 5%",
        o.pct(o.unregistered)
    );
    assert!(
        o.pct(o.captured) <= 12.0,
        "capture overhead {:.1}% exceeds the 12% regression bound",
        o.pct(o.captured)
    );
}

/// Times `n` non-blocking connects to `ip:port` and prints the p50 and p99
/// of the syscall in nanoseconds.
const CONNECT_TIMER_C: &str = r#"
#include <arpa/inet.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <time.h>
#include <unistd.h>

static int cmp(const void *a, const void *b) {
    long x = *(const long *)a, y = *(const long *)b;
    return (x > y) - (x < y);
}

int main(int argc, char **argv) {
    int n = atoi(argv[3]);
    long *t = malloc(sizeof(long) * n);
    struct sockaddr_in a = { .sin_family = AF_INET, .sin_port = htons(atoi(argv[2])) };
    inet_pton(AF_INET, argv[1], &a.sin_addr);
    for (int i = 0; i < n; i++) {
        int s = socket(AF_INET, SOCK_STREAM | SOCK_NONBLOCK, 0);
        struct timespec t0, t1;
        clock_gettime(CLOCK_MONOTONIC, &t0);
        connect(s, (struct sockaddr *)&a, sizeof a);
        clock_gettime(CLOCK_MONOTONIC, &t1);
        close(s);
        t[i] = (t1.tv_sec - t0.tv_sec) * 1000000000L + (t1.tv_nsec - t0.tv_nsec);
    }
    qsort(t, n, sizeof(long), cmp);
    printf("%ld %ld\n", t[n / 2], t[n * 99 / 100]);
    return 0;
}
"#;

/// (p50, p99) in µs of connects from a captured scope, optionally enforced.
fn connect_latency(
    dir: &Path,
    socket: &Path,
    timer: &Path,
    name: &str,
    target: &str,
    egress: Option<wyrd_sensor::EgressRules>,
) -> (f64, f64) {
    let out = dir.join(format!("{name}.txt"));
    let ws = dir.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let mut client = None;
    run_scoped(
        dir,
        name,
        &format!("{} {target} 1 20000 > {}", timer.display(), out.display()),
        |cg| {
            let c = SensorClient::connect_with(socket, cg, &ws, egress).unwrap();
            assert_eq!(c.egress_error, None);
            client = Some(c);
        },
    );
    drop(client);
    let text = std::fs::read_to_string(&out).unwrap();
    let mut it = text
        .split_whitespace()
        .map(|v| v.parse::<f64>().unwrap() / 1000.0);
    (it.next().unwrap(), it.next().unwrap())
}

#[test]
#[ignore = "release measurement; make test-perf"]
fn latency_of_connect_enforcement() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let d = daemon(tmp.path());
    let c = tmp.path().join("timer.c");
    let timer = tmp.path().join("timer");
    std::fs::write(&c, CONNECT_TIMER_C).unwrap();
    assert!(
        Command::new("cc")
            .args(["-O2", "-o"])
            .arg(&timer)
            .arg(&c)
            .status()
            .unwrap()
            .success()
    );
    // A realistic allow-list: loopback plus a thousand host addresses.
    let mut cidrs = vec!["127.0.0.0/8".to_string()];
    cidrs.extend((0..1000).map(|i| format!("198.51.{}.{}/32", i / 250, i % 250 + 1)));
    let rules = wyrd_sensor::EgressRules {
        cidrs,
        restrict_hosts: true,
        ports: Vec::new(),
    };
    let mut rounds = Vec::new();
    for round in 0..3 {
        let base = connect_latency(
            tmp.path(),
            &d.socket,
            &timer,
            &format!("base{round}"),
            "127.0.0.1",
            None,
        );
        let allowed = connect_latency(
            tmp.path(),
            &d.socket,
            &timer,
            &format!("allowed{round}"),
            "127.0.0.1",
            Some(rules.clone()),
        );
        let denied = connect_latency(
            tmp.path(),
            &d.socket,
            &timer,
            &format!("denied{round}"),
            "10.255.255.1",
            Some(rules.clone()),
        );
        eprintln!(
            "round {round}: captured, not enforced p50 {:.1} p99 {:.1} µs; enforced, allowed p50 {:.1} p99 {:.1} µs; enforced, denied p50 {:.1} p99 {:.1} µs",
            base.0, base.1, allowed.0, allowed.1, denied.0, denied.1
        );
        rounds.push((base, allowed, denied));
    }
    let med = |f: &dyn Fn(&((f64, f64), (f64, f64), (f64, f64))) -> f64| {
        let mut v: Vec<f64> = rounds.iter().map(f).collect();
        v.sort_by(f64::total_cmp);
        v[1]
    };
    let added_allowed = med(&|r| r.1.1 - r.0.1);
    let added_denied = med(&|r| r.2.1 - r.0.1);
    eprintln!(
        "median p99 added: allowed {added_allowed:.1} µs, denied {added_denied:.1} µs (target ≤ 50 µs)"
    );
    assert!(
        added_allowed <= 50.0,
        "allowed connect adds {added_allowed:.1} µs at p99"
    );
    assert!(
        added_denied <= 50.0,
        "denied connect adds {added_denied:.1} µs at p99"
    );
}
