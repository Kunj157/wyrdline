#![cfg(feature = "privileged-tests")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The escape suite (plan §19 Phase 9): an agent in a Cell under the
//! `escape` fixture policy tries to get around each enforcement layer.
//! Every attempt must be denied by the kernel (Landlock, the cgroup
//! connect programs) or detected and stopped (the exec observer). Run
//! with `make test-escape` (also part of `make test-integration`); fails,
//! never skips, when the machine cannot run Cells, the sensor, or `cc`.
//!
//! The fixture (`policies/examples/escape`): no reads of `~/.ssh`; writes
//! only to the Cell defaults; egress to 10.0.0.0/8 and loopback on TCP
//! port 443; `curl` may not run. A changed DNS answer for an allowed host
//! is covered at the sensor level
//! (`privileged_egress_update_drops_addresses_a_host_no_longer_has` in
//! `wyrd-sensord`) and in the rule builder's unit tests.

#[path = "../../../crates/wyrd-cli/tests/support/mod.rs"]
mod support;

use std::fs;
use std::path::Path;
use std::process::Command;

use support::*;

fn policy() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../policies/fixtures/escape.wasm")
        .canonicalize()
        .unwrap()
        .display()
        .to_string()
}

/// A library whose constructor tries to write to `$HOME` and to connect to
/// a denied address, from inside whatever process preloads it.
const PRELOAD_C: &str = r#"
#include <arpa/inet.h>
#include <errno.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <unistd.h>

__attribute__((constructor)) static void escape(void) {
    char path[4096];
    snprintf(path, sizeof path, "%s/preloaded.txt", getenv("HOME"));
    int fd = open(path, O_WRONLY | O_CREAT, 0644);
    printf("preload-home-write=%s\n", fd >= 0 ? "ok" : (errno == EACCES ? "denied" : strerror(errno)));
    int s = socket(AF_INET, SOCK_STREAM, 0);
    struct sockaddr_in a = { .sin_family = AF_INET, .sin_port = htons(443) };
    inet_pton(AF_INET, "203.0.113.9", &a.sin_addr);
    int r = connect(s, (struct sockaddr *)&a, sizeof a);
    printf("preload-connect=%s\n", r == 0 ? "ok" : (errno == EPERM ? "denied" : "not-denied"));
    fflush(stdout);
}
"#;

/// Each attempt prints `name=denied` when the kernel refused it.
const ATTEMPTS: &str = r#"
try() { name=$1; shift; if "$@" >/dev/null 2>&1; then echo "$name=ok"; else echo "$name=denied"; fi; }
{
try home-write sh -c 'echo x > "$HOME/escaped.txt"'
try ssh-read cat "$HOME/.ssh/id_test"
ln -s "$HOME/.ssh/id_test" key-link
try ssh-via-symlink cat key-link
ln -s "$HOME/.ssh" ssh-dir-link
try ssh-via-dir-symlink cat ssh-dir-link/id_test
ln -s "$HOME" home-link
try home-write-via-symlink sh -c 'echo x > home-link/escaped2.txt'
try home-write-via-proc-fd sh -c 'exec 3< "$HOME/notes"; echo x > /proc/self/fd/3'
try workspace-write sh -c 'echo x > inside.txt'
try notes-read cat "$HOME/notes"
python3 -c "
import socket
def attempt(name, fam, addr):
    s = socket.socket(fam)
    s.settimeout(0.5)
    try:
        s.connect(addr)
        r = 'ok'
    except PermissionError:
        r = 'denied'
    except OSError:
        r = 'not-denied'
    print(f'{name}={r}')
attempt('connect-by-ip', socket.AF_INET, ('203.0.113.9', 443))
attempt('connect-ipv6', socket.AF_INET6, ('2001:db8::1', 443))
attempt('connect-v4-mapped', socket.AF_INET6, ('::ffff:203.0.113.9', 443))
attempt('connect-wrong-port', socket.AF_INET, ('127.0.0.1', 22))
attempt('connect-allowed', socket.AF_INET, ('10.255.255.1', 443))
"
LD_PRELOAD="$PRELOAD" /bin/true
} > results.txt
"#;

#[test]
fn privileged_escape_attempts_are_denied_in_the_kernel() {
    let e = env();
    let d = sensord(&e);
    let home = e.outside.join("home");
    fs::create_dir_all(home.join(".ssh")).unwrap();
    fs::write(home.join(".ssh/id_test"), "secret key\n").unwrap();
    fs::write(home.join("notes"), "fine\n").unwrap();
    let so = e.outside.join("preload.so");
    let c = e.outside.join("preload.c");
    fs::write(&c, PRELOAD_C).unwrap();
    let cc = Command::new("cc")
        .args(["-shared", "-fPIC", "-o"])
        .arg(&so)
        .arg(&c)
        .status()
        .expect("a C compiler (cc) is needed for the LD_PRELOAD case");
    assert!(cc.success());
    let agent = script(
        &e,
        "agent.sh",
        &format!("export PRELOAD={}\n{ATTEMPTS}", so.display()),
    );
    let out = Command::new(env!("CARGO_BIN_EXE_wyrd"))
        .args(["run", "--policy", &policy(), "--", &agent])
        .current_dir(&e.ws)
        .env("WYRD_DATA_DIR", &e.data)
        .env("WYRD_PRIVILEGED_HELPER", "sudo")
        .env("WYRD_SENSORD_SOCKET", &d.socket)
        .env("HOME", &home)
        .output()
        .unwrap();
    eprintln!(
        "{}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(out.status.code(), Some(0));
    let sid = only_session(&e);
    let results = fs::read_to_string(
        e.data
            .join("sessions")
            .join(&sid)
            .join("branches/a/upper/results.txt"),
    )
    .unwrap();
    eprintln!("{results}");
    let got: Vec<&str> = results.lines().collect();
    assert_eq!(
        got,
        [
            "home-write=denied",
            "ssh-read=denied",
            "ssh-via-symlink=denied",
            "ssh-via-dir-symlink=denied",
            "home-write-via-symlink=denied",
            "home-write-via-proc-fd=denied",
            "workspace-write=ok",
            "notes-read=ok",
            "connect-by-ip=denied",
            "connect-ipv6=denied",
            "connect-v4-mapped=denied",
            "connect-wrong-port=denied",
            "connect-allowed=not-denied",
            "preload-home-write=denied",
            "preload-connect=denied",
        ]
    );
    assert!(!home.join("escaped.txt").exists());
    assert!(!home.join("escaped2.txt").exists());
    assert!(!home.join("preloaded.txt").exists());
    // The kernel denials are in the ledger, each with its decision.
    let events = ledger_events(&e, &sid);
    let denied = events
        .iter()
        .filter(|b| matches!(b, wyrd_schema::EventBody::NetConnect(c) if c.verdict == wyrd_schema::NetVerdict::Denied))
        .count();
    let decisions = events
        .iter()
        .filter(|b| matches!(b, wyrd_schema::EventBody::GuardDecision(g) if g.stage == wyrd_schema::GuardStage::Kernel && g.decision == wyrd_schema::Decision::Deny))
        .count();
    // By IP, IPv6, v4-mapped, wrong port (Landlock answers first, with
    // EACCES), and the preloaded library's connect.
    assert_eq!(denied, 5, "denied connects");
    assert_eq!(decisions, denied);
}

#[test]
fn privileged_escape_renamed_binary_is_detected_and_stopped() {
    use wyrd_schema::EventBody as B;
    let e = env();
    let d = sensord(&e);
    // curl is on the exec deny list; a copy under another name is still
    // curl: the observer matches the binary's content.
    let agent = script(
        &e,
        "agent.sh",
        r#"cp "$(command -v curl)" ./fetcher
./fetcher --version > /dev/null
sleep 5
echo survived > late.txt"#,
    );
    let t = std::time::Instant::now();
    let out = wyrd_with_sensor(&e, &d.socket, &["run", "--policy", &policy(), "--", &agent]);
    assert!(
        t.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        t.elapsed()
    );
    assert_ne!(out.status.code(), Some(0));
    let sid = only_session(&e);
    let upper = e.data.join("sessions").join(&sid).join("branches/a/upper");
    assert!(upper.join("fetcher").exists());
    assert!(
        !upper.join("late.txt").exists(),
        "the agent was not stopped"
    );
    let stop = ledger_events(&e, &sid)
        .into_iter()
        .find_map(|b| match b {
            B::GuardDecision(g) if g.stage == wyrd_schema::GuardStage::Effect => Some(g),
            _ => None,
        })
        .expect("effect-stage decision");
    assert_eq!(stop.decision, wyrd_schema::Decision::Deny);
    assert!(stop.reason.contains("a copy of curl"), "{}", stop.reason);
    assert!(stop.reason.contains("stopped"), "{}", stop.reason);
    assert!(stop.subject_seq.is_some());
}
