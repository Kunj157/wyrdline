#![cfg(feature = "privileged-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Golden tests against a real kernel. The daemon runs through `sudo -n`
//! on a temporary socket and exits when its stdin closes.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use wyrd_schema::EventBody;
use wyrd_sensor::{SensorClient, ServerMsg};

struct Daemon {
    child: Child,
    socket: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        drop(self.child.stdin.take()); // EOF → daemon exits
        let _ = self.child.wait();
    }
}

fn daemon(dir: &Path, extra: &[&str]) -> Daemon {
    let socket = dir.join("sensord.sock");
    let child = Command::new("sudo")
        .args([
            "-n",
            "--",
            env!("CARGO_BIN_EXE_wyrd-sensord"),
            "--exit-on-stdin-eof",
            "--socket",
        ])
        .arg(&socket)
        .args(extra)
        .stdin(Stdio::piped())
        .spawn()
        .unwrap();
    let start = Instant::now();
    while !socket.exists() {
        assert!(
            start.elapsed() < Duration::from_secs(20),
            "daemon did not start"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    Daemon { child, socket }
}

/// A shell in its own user scope that blocks on a FIFO until released.
struct Gated {
    child: Child,
    fifo: PathBuf,
    cgroup: PathBuf,
}

fn gated(dir: &Path, name: &str, script: &str) -> Gated {
    let fifo = dir.join(format!("{name}.fifo"));
    Command::new("mkfifo").arg(&fifo).status().unwrap();
    let unit = format!("wyrd-golden-{}-{name}", std::process::id());
    let child = Command::new("systemd-run")
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
    let start = Instant::now();
    let cgroup = loop {
        let text =
            std::fs::read_to_string(format!("/proc/{}/cgroup", child.id())).unwrap_or_default();
        if let Some(p) = text.lines().find_map(|l| l.strip_prefix("0::"))
            && p.trim().ends_with(&want)
        {
            break PathBuf::from("/sys/fs/cgroup").join(p.trim().trim_start_matches('/'));
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "scope not created"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    Gated {
        child,
        fifo,
        cgroup,
    }
}

impl Gated {
    fn release(&self) {
        std::fs::OpenOptions::new()
            .write(true)
            .open(&self.fifo)
            .unwrap()
            .write_all(b"go\n")
            .unwrap();
    }
}

/// Collect events until the root shell exits (or timeout).
fn collect(client: &mut SensorClient, root_pid: u32) -> Vec<(u32, EventBody)> {
    client.set_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut out = Vec::new();
    let start = Instant::now();
    while start.elapsed() < Duration::from_secs(15) {
        match client.recv() {
            Ok(Some(ServerMsg::Event { process, body, .. })) => {
                let done = process.pid == root_pid && matches!(body, EventBody::ProcExit(_));
                out.push((process.pid, body));
                if done {
                    break;
                }
            }
            Ok(Some(_)) => {}
            Ok(None) | Err(_) => break,
        }
    }
    out
}

fn summary(events: &[(u32, EventBody)], ws: &Path) -> Vec<String> {
    let rel = |p: &str| {
        p.strip_prefix(&format!("{}/", ws.display()))
            .unwrap_or(p)
            .to_string()
    };
    events
        .iter()
        .filter_map(|(_, b)| match b {
            EventBody::ProcExec(x) => Some(format!("exec {}", x.argv.join(" "))),
            EventBody::FileOpenWrite(x) if x.file.in_cell => {
                Some(format!("write {}", rel(&x.file.path)))
            }
            EventBody::FileMkdir(x) => Some(format!("mkdir {}", rel(&x.path))),
            EventBody::FileRename(x) => {
                Some(format!("rename {} {}", rel(&x.from.path), rel(&x.to.path)))
            }
            EventBody::FileChmod(x) => Some(format!("chmod {} {:o}", rel(&x.file.path), x.mode)),
            EventBody::FileUnlink(x) => Some(format!(
                "unlink{} {}",
                if x.is_dir { "dir" } else { "" },
                rel(&x.file.path)
            )),
            EventBody::FileReadSensitive(x) => Some(format!("secret {}", rel(&x.path))),
            EventBody::ProcExit(x) => Some(format!("exit {}", x.exit_code)),
            _ => None,
        })
        .collect()
}

#[test]
fn privileged_golden_effect_sequence_for_a_scripted_tree() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    std::fs::write(ws.join(".env"), "X=1\n").unwrap();
    let d = daemon(tmp.path(), &[]);
    let g = gated(
        tmp.path(),
        "golden",
        &format!(
            "cd {}; echo x > a.txt; mkdir d; mv a.txt d/b.txt; chmod 600 d/b.txt; cat .env > /dev/null; \
             rm d/b.txt; rmdir d; exit 3",
            ws.display()
        ),
    );
    let mut client = SensorClient::connect(&d.socket, &g.cgroup, &ws).unwrap();
    // task_newtask, sched_process_exit, raw sys_enter/sys_exit (+ connect4/6).
    assert!(client.attached >= 4, "attached {}", client.attached);
    g.release();
    let events = collect(&mut client, g.child.id());
    let got = summary(&events, &ws);
    let expected = vec![
        "write a.txt",
        "exec mkdir d",
        "mkdir d",
        "exit 0",
        "exec mv a.txt d/b.txt",
        "rename a.txt d/b.txt",
        "exit 0",
        "exec chmod 600 d/b.txt",
        "chmod d/b.txt 600",
        "exit 0",
        "exec cat .env",
        "secret .env",
        "exit 0",
        "exec rm d/b.txt",
        "unlink d/b.txt",
        "exit 0",
        "exec rmdir d",
        "unlinkdir d",
        "exit 0",
        "exit 3",
    ];
    assert_eq!(got, expected, "\nfull: {events:#?}");
    // Every event carries the right process identity and parentage.
    let root = g.child.id();
    for (pid, _) in &events {
        assert!(
            *pid == root
                || events
                    .iter()
                    .any(|(_, b)| matches!(b, EventBody::ProcFork(f) if f.child_pid == *pid))
        );
    }
}

#[test]
fn privileged_other_cgroups_are_invisible_and_foreign_cgroups_refused() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let d = daemon(tmp.path(), &[]);
    let watched = gated(
        tmp.path(),
        "watched",
        &format!("echo w > {}/watched.txt", ws.display()),
    );
    let other = gated(
        tmp.path(),
        "other",
        &format!("echo o > {}/other.txt", ws.display()),
    );
    let mut client = SensorClient::connect(&d.socket, &watched.cgroup, &ws).unwrap();
    other.release();
    watched.release();
    let events = collect(&mut client, watched.child.id());
    let pids: Vec<u32> = events.iter().map(|(p, _)| *p).collect();
    assert!(
        !pids.contains(&other.child.id()),
        "unregistered cgroup leaked: {events:#?}"
    );
    assert!(summary(&events, &ws).contains(&"write watched.txt".to_string()));

    // A root-owned cgroup cannot be registered by this user.
    let err = SensorClient::connect(&d.socket, Path::new("/sys/fs/cgroup/system.slice"), &ws)
        .unwrap_err();
    assert!(err.to_string().contains("not owned"), "{err}");
    // Paths outside the cgroup filesystem are refused.
    let err = SensorClient::connect(&d.socket, Path::new("/etc"), &ws).unwrap_err();
    assert!(err.to_string().contains("not a cgroup"), "{err}");
}

#[test]
fn privileged_drops_are_counted_with_a_tiny_ring_buffer() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let d = daemon(tmp.path(), &["--ringbuf-kb", "4", "--read-pause-ms", "300"]);
    let g = gated(
        tmp.path(),
        "flood",
        &format!(
            "cd {}; i=0; while [ $i -lt 3000 ]; do : > f$i; i=$((i+1)); done; sleep 2",
            ws.display()
        ),
    );
    let mut client = SensorClient::connect(&d.socket, &g.cgroup, &ws).unwrap();
    g.release();
    client.set_timeout(Some(Duration::from_secs(10))).unwrap();
    let start = Instant::now();
    let mut drops = 0;
    while start.elapsed() < Duration::from_secs(15) && drops == 0 {
        if let Ok(Some(ServerMsg::Drops { total, .. })) = client.recv() {
            drops = total;
        }
    }
    assert!(
        drops > 0,
        "a 4 KiB ring buffer must overflow under 3000 creates"
    );
}

#[test]
fn privileged_a_stalled_client_sees_its_own_queue_drops() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    // A 16-event client queue: a client that stops reading overflows it.
    let d = daemon(tmp.path(), &["--client-queue", "16"]);
    let g = gated(
        tmp.path(),
        "stall",
        &format!(
            "cd {}; i=0; while [ $i -lt 3000 ]; do : > f$i; i=$((i+1)); done; sleep 2",
            ws.display()
        ),
    );
    let mut client = SensorClient::connect(&d.socket, &g.cgroup, &ws).unwrap();
    g.release();
    // Stop reading while the workload runs, then read until the loss shows.
    std::thread::sleep(Duration::from_secs(3));
    client.set_timeout(Some(Duration::from_secs(10))).unwrap();
    let start = Instant::now();
    let mut lost = 0;
    while start.elapsed() < Duration::from_secs(20) && lost == 0 {
        if let Ok(Some(ServerMsg::Drops { queue_dropped, .. })) = client.recv() {
            lost = queue_dropped;
        }
    }
    assert!(
        lost > 0,
        "a client that stopped reading must see its own queue drops"
    );
}

#[test]
fn privileged_flush_is_answered_after_every_queued_event() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    // A lagging reader: events sit in the ring buffer well past the old
    // fixed 300 ms drain.
    let d = daemon(tmp.path(), &["--read-pause-ms", "800"]);
    let mut g = gated(
        tmp.path(),
        "flush",
        &format!("cd {}; echo x > last.txt; exit 5", ws.display()),
    );
    let mut client = SensorClient::connect(&d.socket, &g.cgroup, &ws).unwrap();
    g.release();
    g.child.wait().unwrap();
    client.send_flush(42).unwrap();
    client.set_timeout(Some(Duration::from_secs(10))).unwrap();
    let mut events = Vec::new();
    let start = Instant::now();
    loop {
        assert!(start.elapsed() < Duration::from_secs(15), "no flush answer");
        match client.recv().unwrap() {
            Some(ServerMsg::Event { process, body, .. }) => events.push((process.pid, body)),
            Some(ServerMsg::Flushed { token }) => {
                assert_eq!(token, 42);
                break;
            }
            Some(_) => {}
            None => panic!("daemon closed the connection"),
        }
    }
    let got = summary(&events, &ws);
    assert_eq!(got, vec!["write last.txt", "exit 5"], "\nfull: {events:#?}");
}

/// Connect outcomes printed by the workload, one `name=result` per line.
const CONNECTS: &str = r#"python3 -c "
import socket
def tryc(name, fam, addr):
    s = socket.socket(fam)
    s.settimeout(0.5)
    try:
        s.connect(addr)
        r = 'open'
    except PermissionError:
        r = 'denied'
    except socket.timeout:
        r = 'timeout'
    except OSError:
        r = 'refused'
    print(f'{name}={r}', flush=True)
tryc('v4-allowed', socket.AF_INET, ('127.0.0.1', 1))
tryc('v4-other', socket.AF_INET, ('10.255.255.1', 9))
tryc('v6-loopback', socket.AF_INET6, ('::1', 1))
tryc('mapped-allowed', socket.AF_INET6, ('::ffff:127.0.0.1', 1))
tryc('mapped-other', socket.AF_INET6, ('::ffff:10.255.255.1', 9))
tryc('v4-port-443', socket.AF_INET, ('127.0.0.1', 443))
""#;

#[test]
fn privileged_egress_is_enforced_in_the_kernel_and_reported() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let out = tmp.path().join("connects.txt");
    let d = daemon(tmp.path(), &[]);
    let g = gated(
        tmp.path(),
        "egress",
        &format!("{CONNECTS} > {}", out.display()),
    );
    // Loopback v4 only, and only ports 1 and 443.
    let rules = wyrd_sensor::EgressRules {
        cidrs: vec!["127.0.0.0/8".into()],
        restrict_hosts: true,
        ports: vec![1, 443],
    };
    let mut client = SensorClient::connect_with(&d.socket, &g.cgroup, &ws, Some(rules)).unwrap();
    assert_eq!(client.egress_error, None);
    // A later update replaces the rules: 10.255.255.1 and port 9 become
    // allowed (the connect then times out: the address is unroutable).
    let widened = wyrd_sensor::EgressRules {
        cidrs: vec!["127.0.0.0/8".into(), "10.255.255.1/32".into()],
        restrict_hosts: true,
        ports: vec![1, 9, 443],
    };
    client.send_egress(&widened).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    g.release();
    let events = collect(&mut client, g.child.id());
    let got = std::fs::read_to_string(&out).unwrap();
    assert_eq!(
        got.lines().collect::<Vec<_>>(),
        [
            "v4-allowed=refused",
            "v4-other=timeout",
            "v6-loopback=denied",
            "mapped-allowed=refused",
            "mapped-other=timeout",
            "v4-port-443=refused",
        ]
    );
    let verdicts: Vec<(String, u16, wyrd_schema::NetVerdict)> = events
        .iter()
        .filter_map(|(_, b)| match b {
            EventBody::NetConnect(c) => Some((c.addr.clone(), c.port, c.verdict)),
            _ => None,
        })
        .collect();
    use wyrd_schema::NetVerdict::{Allowed, Denied};
    assert_eq!(
        verdicts,
        [
            ("127.0.0.1".into(), 1, Allowed),
            ("10.255.255.1".into(), 9, Allowed),
            ("::1".into(), 1, Denied),
            ("::ffff:127.0.0.1".into(), 1, Allowed),
            ("::ffff:10.255.255.1".into(), 9, Allowed),
            ("127.0.0.1".into(), 443, Allowed),
        ],
        "\nfull: {events:#?}"
    );
}

#[test]
fn privileged_egress_rules_do_not_touch_other_cgroups() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let d = daemon(tmp.path(), &[]);
    let mut enforced = gated(tmp.path(), "enforced", "true");
    let _client = SensorClient::connect_with(
        &d.socket,
        &enforced.cgroup,
        &ws,
        Some(wyrd_sensor::EgressRules {
            cidrs: Vec::new(),
            restrict_hosts: true,
            ports: Vec::new(),
        }),
    )
    .unwrap();
    // A sibling scope, registered for capture only, connects freely.
    let out = tmp.path().join("free.txt");
    let free = gated(
        tmp.path(),
        "free",
        &format!("{CONNECTS} > {}", out.display()),
    );
    let mut c2 = SensorClient::connect(&d.socket, &free.cgroup, &ws).unwrap();
    free.release();
    let _ = collect(&mut c2, free.child.id());
    let got = std::fs::read_to_string(&out).unwrap();
    assert!(!got.contains("=denied"), "{got}");
    enforced.release();
    let _ = enforced.child.wait();
}

/// An allowed host name that now resolves elsewhere: `wyrd run` sends the
/// new address set, and the old address is refused from then on.
#[test]
fn privileged_egress_update_drops_addresses_a_host_no_longer_has() {
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let out = tmp.path().join("moved.txt");
    let d = daemon(tmp.path(), &[]);
    let g = gated(
        tmp.path(),
        "moved",
        &format!(
            r#"python3 -c "
import socket
for host in ('10.255.255.1', '10.255.255.2'):
    s = socket.socket()
    s.settimeout(0.5)
    try:
        s.connect((host, 9))
        r = 'open'
    except PermissionError:
        r = 'denied'
    except OSError:
        r = 'not-denied'
    print(host, r)
" > {}"#,
            out.display()
        ),
    );
    let before = wyrd_sensor::EgressRules {
        cidrs: vec!["10.255.255.1/32".into()],
        restrict_hosts: true,
        ports: Vec::new(),
    };
    let mut client = SensorClient::connect_with(&d.socket, &g.cgroup, &ws, Some(before)).unwrap();
    client
        .send_egress(&wyrd_sensor::EgressRules {
            cidrs: vec!["10.255.255.2/32".into()],
            restrict_hosts: true,
            ports: Vec::new(),
        })
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    g.release();
    let _ = collect(&mut client, g.child.id());
    assert_eq!(
        std::fs::read_to_string(&out).unwrap(),
        "10.255.255.1 denied\n10.255.255.2 not-denied\n"
    );
}

/// The daemon started by `daemon()` for `socket`. Matched on the program name
/// and on the socket path in its command line: `sudo` has another program
/// name, and the other tests in this file run their own daemons on their own
/// sockets, so a name match alone could pick the wrong process.
fn daemon_pid(socket: &Path) -> u32 {
    let wanted = socket.as_os_str().as_encoded_bytes();
    for entry in std::fs::read_dir("/proc").unwrap().flatten() {
        let Some(pid) = entry
            .file_name()
            .to_str()
            .and_then(|s| s.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline")) else {
            continue;
        };
        let argv0 = cmdline.split(|b| *b == 0).next().unwrap_or_default();
        let names_socket = cmdline.windows(wanted.len()).any(|w| w == wanted);
        if argv0.ends_with(b"wyrd-sensord") && names_socket {
            return pid;
        }
    }
    panic!("no wyrd-sensord process for {}", socket.display());
}

/// `(thread id, CapEff, CapPrm, CapInh)` of every thread of `pid`. Capability
/// sets are per thread, so a thread created before the drop would keep its
/// old set: every thread is checked, not only the leader.
fn thread_caps(pid: u32) -> Vec<(u32, u64, u64, u64)> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(format!("/proc/{pid}/task"))
        .unwrap()
        .flatten()
    {
        let tid: u32 = entry.file_name().to_str().unwrap().parse().unwrap();
        let status = std::fs::read_to_string(entry.path().join("status")).unwrap();
        out.push((
            tid,
            cap_mask(&status, "CapEff:"),
            cap_mask(&status, "CapPrm:"),
            cap_mask(&status, "CapInh:"),
        ));
    }
    out
}

/// The capability mask in a `/proc/<pid>/status` line such as `CapEff:`.
fn cap_mask(status: &str, field: &str) -> u64 {
    let line = status
        .lines()
        .find(|l| l.starts_with(field))
        .unwrap_or_else(|| panic!("{field} missing"));
    let hex = line.split_whitespace().nth(1).unwrap();
    u64::from_str_radix(hex, 16).unwrap()
}

#[test]
fn privileged_runtime_capabilities_are_bpf_only() {
    // CAP_BPF = 39 (linux/capability.h). The daemon keeps only what the
    // runtime needs: see RUNTIME_CAPS in wyrd-sensord.
    const RUNTIME: u64 = 1 << 39;
    let tmp = tempfile::tempdir_in(env!("CARGO_TARGET_TMPDIR")).unwrap();
    let d = daemon(tmp.path(), &[]);
    // Read after the daemon is listening, so the drop has run.
    let pid = daemon_pid(&d.socket);
    let threads = thread_caps(pid);
    assert!(!threads.is_empty());
    for (tid, eff, prm, inh) in &threads {
        assert_eq!(*eff, RUNTIME, "thread {tid}: effective set after attach");
        assert_eq!(*prm, RUNTIME, "thread {tid}: permitted set after attach");
        assert_eq!(*inh, 0, "thread {tid}: inheritable set after attach");
    }
    // Capture still works with the reduced set: a session is registered and
    // its scripted effects are reported (the golden sequence test covers the
    // full sequence; this proves the path is live).
    let ws = tmp.path().join("ws");
    std::fs::create_dir(&ws).unwrap();
    let g = gated(
        tmp.path(),
        "caps",
        &format!("cd {}; echo x > a.txt; exit 0", ws.display()),
    );
    let mut client = SensorClient::connect(&d.socket, &g.cgroup, &ws).unwrap();
    g.release();
    let events = collect(&mut client, g.child.id());
    assert!(
        summary(&events, &ws).iter().any(|s| s == "write a.txt"),
        "{events:#?}"
    );
}
