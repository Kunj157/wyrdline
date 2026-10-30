#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Which network effects a rewind lists as irreversible (issue #260).
//!
//! The Cell's sensor writes `effect.net.*` events in the real run; these tests
//! append them to the ledger directly, the way the sensor would, so the
//! ledger's order is exact.

use std::collections::BTreeMap;
use std::fs;
use std::path::PathBuf;

use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session};
use wyrd_schema::{Event, EventBody, NetConnect, NetVerdict};

struct Env {
    _tmp: tempfile::TempDir,
    data: DataDir,
    ws: PathBuf,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    fs::create_dir(&ws).unwrap();
    fs::write(ws.join("keep.txt"), "original\n").unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    Env {
        _tmp: tmp,
        data,
        ws,
    }
}

fn create(e: &Env) -> Session {
    Session::create(
        &e.data,
        CreateOptions {
            workspace: e.ws.clone(),
            command: vec!["true".into()],
            observe: false,
            name: None,
            agent_kind: "test".into(),
            deny_tools: Vec::new(),
            policy: None,
            kernel_release: "test".into(),
            capabilities: BTreeMap::new(),
        },
    )
    .unwrap()
}

/// Append one event to the session's ledger as the sensor would; returns its seq.
fn record(s: &Session, body: EventBody) -> u64 {
    let event = Event {
        seq: 0,
        ts_mono_ns: 0,
        ts_wall_ms: 0,
        process: None,
        drops_since_last: 0,
        body,
    };
    let mut ledger = wyrd_ledger::Ledger::open(s.dir().ledger()).unwrap();
    ledger.append(vec![event]).unwrap()
}

fn connect(addr: &str, verdict: NetVerdict) -> EventBody {
    EventBody::NetConnect(NetConnect {
        family: 4,
        addr: addr.into(),
        port: 80,
        dns_name: None,
        verdict,
    })
}

fn listed(s: &Session, branch_id: &str) -> Vec<u64> {
    s.rewind_effects(branch_id)
        .unwrap()
        .expect("a checkpoint.rewound for the branch")
        .effects
        .iter()
        .map(|e| e.seq)
        .collect()
}

#[test]
fn rewind_lists_nothing_without_network_effects_after_the_checkpoint() {
    let e = env();
    let mut s = create(&e);
    // A network effect before the checkpoint is not after it.
    record(&s, connect("198.51.100.1", NetVerdict::Allowed));
    let c1 = s.checkpoint(None).unwrap();
    let b = s.rewind(&c1.id, None).unwrap();
    let r = s.rewind_effects(&b.id).unwrap().unwrap();
    assert_eq!(r.checkpoint_id, c1.id);
    assert!(r.effects.is_empty(), "{:?}", r.effects);
}

#[test]
fn rewind_lists_every_network_effect_after_the_checkpoint_denied_included() {
    let e = env();
    let mut s = create(&e);
    record(&s, connect("198.51.100.1", NetVerdict::Allowed));
    let c1 = s.checkpoint(None).unwrap();
    let allowed = record(&s, connect("198.51.100.2", NetVerdict::Allowed));
    let denied = record(&s, connect("203.0.113.50", NetVerdict::Denied));
    let dns = record(
        &s,
        EventBody::DnsQuery(wyrd_schema::DnsQuery {
            name: "diagnostics.example".into(),
        }),
    );
    s.checkpoint(None).unwrap();
    // Effects after a later checkpoint are still after c1.
    let later = record(&s, connect("203.0.113.50", NetVerdict::Denied));

    let b = s.rewind(&c1.id, None).unwrap();
    assert_eq!(listed(&s, &b.id), vec![allowed, denied, dns, later]);
}

#[test]
fn the_rewound_event_carries_the_list() {
    let e = env();
    let mut s = create(&e);
    let c1 = s.checkpoint(None).unwrap();
    let denied = record(&s, connect("203.0.113.50", NetVerdict::Denied));
    let b = s.rewind(&c1.id, None).unwrap();

    let ledger = wyrd_ledger::Ledger::open_read_only(s.dir().ledger()).unwrap();
    let events = ledger.events_after(0, usize::MAX).unwrap();
    let rewound = events
        .iter()
        .find_map(|ev| match &ev.body {
            EventBody::CheckpointRewound(r) if r.branch_id == b.id => Some(r),
            _ => None,
        })
        .expect("checkpoint.rewound");
    assert_eq!(rewound.irreversible_seqs, vec![denied]);
}

#[test]
fn a_rewind_from_a_forked_branch_lists_only_the_branch_being_left() {
    let e = env();
    let mut s = create(&e);
    let c1 = s.checkpoint(None).unwrap();
    // On branch a, after c1: abandoned by the first rewind.
    let on_a = record(&s, connect("203.0.113.50", NetVerdict::Denied));
    let first = s.rewind(&c1.id, None).unwrap();
    assert_eq!(first.id, "b");
    assert_eq!(listed(&s, "b"), vec![on_a]);

    // On branch b: the effect the first rewind's list already reported is not repeated.
    let on_b = record(&s, connect("198.51.100.9", NetVerdict::Allowed));
    let second = s.rewind(&c1.id, None).unwrap();
    assert_eq!(listed(&s, &second.id), vec![on_b]);
}

#[test]
fn a_rewind_across_a_fork_boundary_lists_both_branches_in_ledger_order() {
    let e = env();
    let mut s = create(&e);
    let c1 = s.checkpoint(None).unwrap();
    let x = record(&s, connect("203.0.113.50", NetVerdict::Denied));
    let c2 = s.checkpoint(None).unwrap();
    // On a, after c2: already listed by the rewind to c2, not by the next one.
    record(&s, connect("198.51.100.3", NetVerdict::Allowed));
    let to_c2 = s.rewind(&c2.id, None).unwrap();
    assert_eq!(to_c2.id, "b");
    let z = record(&s, connect("198.51.100.4", NetVerdict::Allowed));

    // Back to c1 from b, which forked from c2 on a: b's own effects (z) and a's
    // effects between c1 and c2 (x), in ledger order.
    let back = s.rewind(&c1.id, None).unwrap();
    assert_eq!(back.id, "c");
    assert_eq!(listed(&s, &back.id), vec![x, z]);
}

#[test]
fn a_fork_records_no_list() {
    let e = env();
    let mut s = create(&e);
    let c1 = s.checkpoint(None).unwrap();
    record(&s, connect("203.0.113.50", NetVerdict::Denied));
    let f = s.rewind(&c1.id, Some("experiment")).unwrap();
    assert!(s.rewind_effects(&f.id).unwrap().is_none());
}
