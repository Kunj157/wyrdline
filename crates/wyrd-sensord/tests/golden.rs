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
    assert!(client.attached >= 40, "attached {}", client.attached);
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
        if let Ok(Some(ServerMsg::Drops { total })) = client.recv() {
            drops = total;
        }
    }
    assert!(
        drops > 0,
        "a 4 KiB ring buffer must overflow under 3000 creates"
    );
}
