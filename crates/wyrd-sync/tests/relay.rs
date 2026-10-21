//! The relay against real Automerge clients, without a transport.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;

use automerge::sync::{Message, State, SyncDoc};
use automerge::transaction::Transactable;
use automerge::{AutoCommit, ROOT, ReadDoc};
use proptest::prelude::*;
use wyrd_sync::{
    Connection, DirStore, DocStore, JoinError, Limits, MemoryStore, Reject, Role, SyncHub,
};

/// A client: its document, its side of the sync state, and a connection
/// to the relay while online.
struct Client<S: DocStore> {
    doc: AutoCommit,
    sync: State,
    conn: Option<Connection<S>>,
    /// Messages sent but not yet delivered to the relay.
    up: Vec<Vec<u8>>,
}

impl<S: DocStore> Client<S> {
    fn new() -> Self {
        Self {
            doc: AutoCommit::new(),
            sync: State::new(),
            conn: None,
            up: Vec::new(),
        }
    }

    async fn connect(&mut self, hub: &Arc<SyncHub<S>>, role: Role) {
        self.sync = State::new();
        self.up.clear();
        self.conn = Some(hub.join("doc", role).await.unwrap());
    }

    fn disconnect(&mut self) {
        self.conn = None;
        self.up.clear();
    }

    fn send(&mut self) {
        if self.conn.is_some()
            && let Some(m) = self.doc.sync().generate_sync_message(&mut self.sync)
        {
            self.up.push(m.encode());
        }
    }

    async fn deliver_up(&mut self) -> Result<bool, Reject> {
        let Some(conn) = &self.conn else {
            return Ok(false);
        };
        if self.up.is_empty() {
            return Ok(false);
        }
        let m = self.up.remove(0);
        conn.receive(&m).await.map(|()| true)
    }

    fn deliver_down(&mut self) -> bool {
        let Some(m) = self.conn.as_mut().and_then(Connection::try_next) else {
            return false;
        };
        let m = Message::decode(&m).unwrap();
        self.doc
            .sync()
            .receive_sync_message(&mut self.sync, m)
            .unwrap();
        true
    }

    fn put(&mut self, key: &str, value: i64) {
        self.doc.put(ROOT, key, value).unwrap();
    }

    fn keys(&self) -> Vec<(String, i64)> {
        self.doc
            .keys(ROOT)
            .map(|k| {
                let v = self.doc.get(ROOT, &k).unwrap().unwrap().0.as_i64().unwrap();
                (k, v)
            })
            .collect()
    }
}

/// Pump every online client until nobody has anything left to say.
async fn settle<S: DocStore>(clients: &mut [Client<S>]) {
    for _ in 0..200 {
        let mut busy = false;
        for c in clients.iter_mut() {
            c.send();
            while c.deliver_up().await.unwrap() || c.deliver_down() {
                busy = true;
            }
        }
        if !busy {
            return;
        }
    }
    panic!("sync did not settle");
}

fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
}

#[derive(Debug, Clone)]
enum Step {
    Edit(usize, u8, i64),
    Delete(usize, u8),
    Send(usize),
    Up(usize),
    Down(usize),
    Offline(usize),
    Online(usize),
}

fn step() -> impl Strategy<Value = Step> {
    let c = 0..3usize;
    prop_oneof![
        3 => (c.clone(), 0..6u8, any::<i64>()).prop_map(|(c, k, v)| Step::Edit(c, k, v)),
        1 => (c.clone(), 0..6u8).prop_map(|(c, k)| Step::Delete(c, k)),
        3 => c.clone().prop_map(Step::Send),
        3 => c.clone().prop_map(Step::Up),
        3 => c.clone().prop_map(Step::Down),
        1 => c.clone().prop_map(Step::Offline),
        1 => c.prop_map(Step::Online),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(96))]

    /// Three clients edit the same document through the relay; messages
    /// are delivered in any interleaving with edits, and clients go offline
    /// (losing what was in flight), keep editing and reconnect. Once all
    /// are back and the links are quiet, every client, the relay and the
    /// stored document agree.
    #[test]
    fn concurrent_and_offline_edits_converge(steps in proptest::collection::vec(step(), 0..120)) {
        rt().block_on(async {
            let hub = SyncHub::new(MemoryStore::default(), Limits::default());
            let mut clients: Vec<Client<MemoryStore>> = (0..3).map(|_| Client::new()).collect();
            for c in &mut clients {
                c.connect(&hub, Role::Member).await;
            }
            for s in steps {
                match s {
                    Step::Edit(i, k, v) => clients[i].put(&format!("k{k}"), v),
                    Step::Delete(i, k) => {
                        let _ = clients[i].doc.delete(ROOT, format!("k{k}"));
                    }
                    Step::Send(i) => clients[i].send(),
                    Step::Up(i) => {
                        clients[i].deliver_up().await.unwrap();
                    }
                    Step::Down(i) => {
                        clients[i].deliver_down();
                    }
                    Step::Offline(i) => clients[i].disconnect(),
                    Step::Online(i) => {
                        if clients[i].conn.is_none() {
                            clients[i].connect(&hub, Role::Member).await;
                        }
                    }
                }
            }
            for c in &mut clients {
                if c.conn.is_none() {
                    c.connect(&hub, Role::Member).await;
                }
            }
            settle(&mut clients).await;

            let heads = |d: &mut AutoCommit| {
                let mut h = d.get_heads();
                h.sort();
                h
            };
            let first = heads(&mut clients[0].doc);
            let content = clients[0].keys();
            for c in &mut clients[1..] {
                assert_eq!(heads(&mut c.doc), first);
                assert_eq!(c.keys(), content);
            }
            // A fresh client gets the same document from the relay.
            let mut fresh = [Client::new()];
            fresh[0].connect(&hub, Role::Viewer).await;
            settle(&mut fresh).await;
            assert_eq!(fresh[0].keys(), content);
        });
    }
}

#[tokio::test]
async fn viewers_follow_but_cannot_change_the_document() {
    let hub = SyncHub::new(MemoryStore::default(), Limits::default());
    let mut member = [Client::new()];
    member[0].connect(&hub, Role::Member).await;
    member[0].put("seen", 1);
    settle(&mut member).await;

    let mut viewer = [Client::new()];
    viewer[0].connect(&hub, Role::Viewer).await;
    settle(&mut viewer).await;
    assert_eq!(viewer[0].keys(), vec![("seen".to_owned(), 1)]);

    viewer[0].put("forged", 2);
    viewer[0].send();
    let mut rejected = None;
    for _ in 0..10 {
        if let Err(r) = viewer[0].deliver_up().await {
            rejected = Some(r);
            break;
        }
        viewer[0].deliver_down();
        viewer[0].send();
    }
    assert_eq!(rejected, Some(Reject::ReadOnly));
    // Nobody else ever sees the viewer's edit.
    let mut late = [Client::new()];
    late[0].connect(&hub, Role::Member).await;
    settle(&mut late).await;
    assert_eq!(late[0].keys(), vec![("seen".to_owned(), 1)]);
    assert_eq!(hub.metrics().await.rejected, 1);
}

#[tokio::test]
async fn oversized_and_malformed_messages_are_rejected() {
    let limits = Limits {
        max_message: 64,
        ..Limits::default()
    };
    let hub = SyncHub::new(MemoryStore::default(), limits);
    let conn = hub.join("doc", Role::Member).await.unwrap();
    assert_eq!(conn.receive(&[0x42; 65]).await, Err(Reject::TooLarge));
    assert_eq!(conn.receive(b"not automerge").await, Err(Reject::Invalid));
}

#[tokio::test]
async fn a_document_cannot_grow_past_its_limit() {
    let limits = Limits {
        max_document: 600,
        ..Limits::default()
    };
    let hub = SyncHub::new(MemoryStore::default(), limits);
    let mut c = [Client::new()];
    c[0].connect(&hub, Role::Member).await;
    // Incompressible text: change chunks are deflated.
    let noise: String = (0..2000u32)
        .map(|i| char::from(b'a' + u8::try_from(i.wrapping_mul(2_654_435_761) >> 27).unwrap() % 26))
        .collect();
    c[0].doc.put(ROOT, "big", noise).unwrap();
    c[0].send();
    let mut result = Ok(true);
    for _ in 0..10 {
        result = c[0].deliver_up().await;
        if result.is_err() {
            break;
        }
        c[0].deliver_down();
        c[0].send();
    }
    assert_eq!(result, Err(Reject::TooLarge));
}

#[tokio::test]
async fn a_document_holds_a_bounded_number_of_clients() {
    let limits = Limits {
        max_peers: 1,
        ..Limits::default()
    };
    let hub = SyncHub::new(MemoryStore::default(), limits);
    let first = hub.join("doc", Role::Member).await.unwrap();
    assert!(matches!(
        hub.join("doc", Role::Member).await,
        Err(JoinError::Full)
    ));
    drop(first);
    assert!(hub.join("doc", Role::Member).await.is_ok());
}

#[tokio::test]
async fn documents_persist_across_relay_restarts() {
    let dir = tempfile::tempdir().unwrap();
    {
        let hub = SyncHub::new(DirStore::new(dir.path()), Limits::default());
        let mut c = [Client::new()];
        c[0].connect(&hub, Role::Member).await;
        c[0].put("kept", 7);
        settle(&mut c).await;
    }
    assert!(dir.path().join("doc.automerge").is_file());
    let hub = SyncHub::new(DirStore::new(dir.path()), Limits::default());
    let mut c = [Client::new()];
    c[0].connect(&hub, Role::Viewer).await;
    settle(&mut c).await;
    assert_eq!(c[0].keys(), vec![("kept".to_owned(), 7)]);

    // A corrupt stored document refuses joins instead of being replaced.
    std::fs::write(dir.path().join("other.automerge"), b"junk").unwrap();
    assert!(matches!(
        hub.join("other", Role::Member).await,
        Err(JoinError::Storage(_))
    ));
}

#[test]
fn doc_ids_are_path_safe() {
    assert!(wyrd_sync::valid_doc_id("01JABCDEF_x-y"));
    for bad in ["", "../x", "a/b", "a.b", &"a".repeat(129)] {
        assert!(!wyrd_sync::valid_doc_id(bad), "{bad}");
    }
}

#[tokio::test]
async fn changes_are_pushed_to_idle_clients() {
    let hub = SyncHub::new(MemoryStore::default(), Limits::default());
    let mut a = [Client::new()];
    let mut b = [Client::new()];
    a[0].connect(&hub, Role::Member).await;
    b[0].connect(&hub, Role::Member).await;
    settle(&mut a).await;
    settle(&mut b).await;
    a[0].put("live", 1);
    settle(&mut a).await;
    // B sends nothing; the relay pushes the change to it.
    while b[0].deliver_down() {}
    assert_eq!(b[0].keys(), vec![("live".to_owned(), 1)]);
}
