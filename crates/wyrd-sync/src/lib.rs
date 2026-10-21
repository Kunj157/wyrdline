//! Automerge sync relay for annotation documents (plan §19 Phase 12).
//!
//! Every annotation document (one per session or bundle) lives in a
//! [`SyncHub`] room while clients are connected. Each client speaks the
//! Automerge sync protocol with the room's copy; whatever one client adds
//! reaches every other client in the room and is persisted through a
//! [`DocStore`]. The relay is transport-independent: [`Connection`] takes
//! and yields raw sync messages, and the WebSocket endpoint that wyrdd and
//! the Hub mount sits on top of it.
//!
//! Sync messages are untrusted input: each is size-limited, a document may
//! not grow past [`Limits::max_document`], and a viewer's messages may not
//! carry changes.

use std::collections::HashMap;
use std::io;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError, Weak};
use std::time::Duration;

use automerge::Automerge;
use automerge::sync::{Message, State, SyncDoc};
use serde::Serialize;
use tokio::sync::mpsc;

/// What a connected client may do with a document.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Reads and follows the document; its edits are rejected.
    Viewer,
    /// Reads and annotates.
    Member,
    Admin,
}

impl Role {
    #[must_use]
    pub fn can_write(self) -> bool {
        !matches!(self, Role::Viewer)
    }
}

/// Bounds on what a client can make the relay hold.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    /// Largest sync message accepted, in bytes.
    pub max_message: usize,
    /// Largest a document may grow to (sum of its stored changes), in bytes.
    pub max_document: usize,
    /// Clients connected to one document at once.
    pub max_peers: usize,
    /// Outgoing messages queued per client; a client that falls this far
    /// behind is disconnected and resyncs when it reconnects.
    pub queue: usize,
    /// How long a new connection may take to authenticate.
    pub auth_timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_message: 1 << 20,
            max_document: 8 << 20,
            max_peers: 64,
            queue: 256,
            auth_timeout: Duration::from_secs(5),
        }
    }
}

/// Document ids are path- and URL-safe: `[A-Za-z0-9_-]{1,128}`.
#[must_use]
pub fn valid_doc_id(id: &str) -> bool {
    (1..=128).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Where documents are persisted (a directory for wyrdd, Postgres for the
/// Hub).
pub trait DocStore: Send + Sync + 'static {
    fn load(&self, doc: &str) -> impl Future<Output = io::Result<Option<Vec<u8>>>> + Send;
    fn save(&self, doc: &str, bytes: Vec<u8>) -> impl Future<Output = io::Result<()>> + Send;
}

/// One `<id>.automerge` file per document, replaced atomically.
#[derive(Debug, Clone)]
pub struct DirStore {
    dir: PathBuf,
}

impl DirStore {
    #[must_use]
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    fn path(&self, doc: &str) -> PathBuf {
        self.dir.join(format!("{doc}.automerge"))
    }
}

impl DocStore for DirStore {
    async fn load(&self, doc: &str) -> io::Result<Option<Vec<u8>>> {
        match tokio::fs::read(self.path(doc)).await {
            Ok(b) => Ok(Some(b)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    async fn save(&self, doc: &str, bytes: Vec<u8>) -> io::Result<()> {
        tokio::fs::create_dir_all(&self.dir).await?;
        let tmp = self.dir.join(format!(".{doc}.automerge.tmp"));
        tokio::fs::write(&tmp, bytes).await?;
        tokio::fs::rename(&tmp, self.path(doc)).await
    }
}

/// Documents in memory (tests).
#[derive(Debug, Default)]
pub struct MemoryStore(Mutex<HashMap<String, Vec<u8>>>);

impl DocStore for MemoryStore {
    fn load(&self, doc: &str) -> impl Future<Output = io::Result<Option<Vec<u8>>>> + Send {
        std::future::ready(Ok(lock(&self.0).get(doc).cloned()))
    }

    fn save(&self, doc: &str, bytes: Vec<u8>) -> impl Future<Output = io::Result<()>> + Send {
        lock(&self.0).insert(doc.to_owned(), bytes);
        std::future::ready(Ok(()))
    }
}

/// Why a client's sync message was refused. The connection should be
/// closed afterwards; the client rolls back and resyncs on reconnect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum Reject {
    #[error("viewers cannot change annotations")]
    ReadOnly,
    #[error("the message or the document would exceed its size limit")]
    TooLarge,
    #[error("not a valid sync message")]
    Invalid,
}

impl Reject {
    /// Stable code sent to the client.
    #[must_use]
    pub fn code(self) -> &'static str {
        match self {
            Reject::ReadOnly => "read_only",
            Reject::TooLarge => "too_large",
            Reject::Invalid => "invalid",
        }
    }
}

/// Why a client could not join a document.
#[derive(Debug, thiserror::Error)]
pub enum JoinError {
    #[error("too many clients on this document")]
    Full,
    #[error("the stored document could not be loaded: {0}")]
    Storage(String),
}

/// Counters for `/metrics`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct SyncMetrics {
    pub connected: u64,
    pub documents: u64,
    pub messages_in: u64,
    pub messages_out: u64,
    pub rejected: u64,
    /// Size of the largest open document, in bytes of stored changes.
    pub largest_document_bytes: u64,
}

#[derive(Debug, Default)]
struct Counters {
    connected: AtomicU64,
    messages_in: AtomicU64,
    messages_out: AtomicU64,
    rejected: AtomicU64,
}

#[derive(Debug)]
struct Peer {
    sync: State,
    tx: mpsc::Sender<Vec<u8>>,
}

#[derive(Debug)]
struct RoomState {
    doc: Automerge,
    /// Bytes of changes the document holds (bounded by `max_document`).
    bytes: usize,
    /// Bumped on every change, so saves can be ordered.
    version: u64,
    peers: HashMap<u64, Peer>,
}

#[derive(Debug)]
struct Room {
    id: String,
    state: Mutex<RoomState>,
    /// The last version written to the store.
    saved: tokio::sync::Mutex<u64>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    // A panic while holding the lock leaves plain data behind; keep going.
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl RoomState {
    /// Send every peer whatever it is missing; a peer whose queue is full
    /// is dropped (its receiver then ends and the transport closes).
    fn fan_out(&mut self, counters: &Counters) {
        let RoomState { doc, peers, .. } = self;
        peers.retain(|_, p| match doc.generate_sync_message(&mut p.sync) {
            None => true,
            Some(m) => {
                counters.messages_out.fetch_add(1, Ordering::Relaxed);
                p.tx.try_send(m.encode()).is_ok()
            }
        });
    }
}

/// The relay: one room per open document.
#[derive(Debug)]
pub struct SyncHub<S> {
    store: S,
    limits: Limits,
    rooms: tokio::sync::Mutex<HashMap<String, Weak<Room>>>,
    next_peer: AtomicU64,
    counters: Arc<Counters>,
}

impl<S: DocStore> SyncHub<S> {
    pub fn new(store: S, limits: Limits) -> Arc<Self> {
        Arc::new(Self {
            store,
            limits,
            rooms: tokio::sync::Mutex::default(),
            next_peer: AtomicU64::new(1),
            counters: Arc::default(),
        })
    }

    #[must_use]
    pub fn limits(&self) -> Limits {
        self.limits
    }

    /// Connect a client with `role` to document `doc` (the caller checked
    /// that the client may see it). The first message for the client is
    /// queued right away.
    pub async fn join(self: &Arc<Self>, doc: &str, role: Role) -> Result<Connection<S>, JoinError> {
        let room = self.room(doc).await?;
        let (tx, rx) = mpsc::channel(self.limits.queue);
        let id = self.next_peer.fetch_add(1, Ordering::Relaxed);
        {
            let mut st = lock(&room.state);
            if st.peers.len() >= self.limits.max_peers {
                return Err(JoinError::Full);
            }
            // Viewers get a full sync state too: if they push changes, the
            // changes are rejected and the client is told, rather than the
            // edits silently never leaving the viewer's device.
            let sync = State::new();
            st.peers.insert(id, Peer { sync, tx });
            st.fan_out(&self.counters);
        }
        self.counters.connected.fetch_add(1, Ordering::Relaxed);
        tracing::info!(doc, ?role, peer = id, "annotation sync client joined");
        Ok(Connection {
            hub: Arc::clone(self),
            room,
            id,
            role,
            rx,
        })
    }

    async fn room(&self, doc: &str) -> Result<Arc<Room>, JoinError> {
        // Held across the load so two first joins cannot open two rooms.
        let mut rooms = self.rooms.lock().await;
        rooms.retain(|_, r| r.strong_count() > 0);
        if let Some(room) = rooms.get(doc).and_then(Weak::upgrade) {
            return Ok(room);
        }
        let saved = self
            .store
            .load(doc)
            .await
            .map_err(|e| JoinError::Storage(e.to_string()))?;
        let (automerge, bytes) = match saved {
            None => (Automerge::new(), 0),
            Some(b) => (
                Automerge::load(&b).map_err(|e| JoinError::Storage(e.to_string()))?,
                b.len(),
            ),
        };
        let room = Arc::new(Room {
            id: doc.to_owned(),
            state: Mutex::new(RoomState {
                doc: automerge,
                bytes,
                version: 0,
                peers: HashMap::new(),
            }),
            saved: tokio::sync::Mutex::new(0),
        });
        rooms.insert(doc.to_owned(), Arc::downgrade(&room));
        Ok(room)
    }

    pub async fn metrics(&self) -> SyncMetrics {
        let rooms: Vec<Arc<Room>> = self
            .rooms
            .lock()
            .await
            .values()
            .filter_map(Weak::upgrade)
            .collect();
        let largest = rooms
            .iter()
            .map(|r| lock(&r.state).bytes)
            .max()
            .unwrap_or(0);
        let c = &self.counters;
        SyncMetrics {
            connected: c.connected.load(Ordering::Relaxed),
            documents: rooms.len() as u64,
            messages_in: c.messages_in.load(Ordering::Relaxed),
            messages_out: c.messages_out.load(Ordering::Relaxed),
            rejected: c.rejected.load(Ordering::Relaxed),
            largest_document_bytes: largest as u64,
        }
    }
}

/// One client's side of a room.
#[derive(Debug)]
pub struct Connection<S: DocStore> {
    hub: Arc<SyncHub<S>>,
    room: Arc<Room>,
    id: u64,
    role: Role,
    rx: mpsc::Receiver<Vec<u8>>,
}

impl<S: DocStore> Connection<S> {
    #[must_use]
    pub fn role(&self) -> Role {
        self.role
    }

    /// The next sync message for this client; `None` once the relay has
    /// dropped it (it fell too far behind).
    pub async fn next(&mut self) -> Option<Vec<u8>> {
        self.rx.recv().await
    }

    /// A queued message for this client, without waiting.
    pub fn try_next(&mut self) -> Option<Vec<u8>> {
        self.rx.try_recv().ok()
    }

    /// Apply a sync message from this client, pass any new changes on to
    /// the other clients and persist the document.
    pub async fn receive(&self, raw: &[u8]) -> Result<(), Reject> {
        let result = self.apply(raw);
        let counters = &self.hub.counters;
        counters.messages_in.fetch_add(1, Ordering::Relaxed);
        match result {
            Err(r) => {
                counters.rejected.fetch_add(1, Ordering::Relaxed);
                tracing::warn!(doc = %self.room.id, peer = self.id, reason = r.code(), "annotation sync message rejected");
                Err(r)
            }
            Ok(None) => Ok(()),
            Ok(Some((version, bytes))) => {
                self.persist(version, bytes).await;
                Ok(())
            }
        }
    }

    /// Returns the version and saved bytes when the document changed.
    fn apply(&self, raw: &[u8]) -> Result<Option<(u64, Vec<u8>)>, Reject> {
        let limits = self.hub.limits;
        if raw.len() > limits.max_message {
            return Err(Reject::TooLarge);
        }
        let msg = Message::decode(raw).map_err(|_| Reject::Invalid)?;
        if !self.role.can_write() && !msg.changes.is_empty() {
            return Err(Reject::ReadOnly);
        }
        let incoming: usize = msg.changes.iter().map(<[u8]>::len).sum();
        let mut st = lock(&self.room.state);
        if st.bytes.saturating_add(incoming) > limits.max_document {
            return Err(Reject::TooLarge);
        }
        let before = st.doc.get_heads();
        {
            let RoomState { doc, peers, .. } = &mut *st;
            let Some(peer) = peers.get_mut(&self.id) else {
                // Dropped for falling behind; the transport is closing.
                return Ok(None);
            };
            doc.receive_sync_message(&mut peer.sync, msg)
                .map_err(|_| Reject::Invalid)?;
        }
        let changed = st.doc.get_heads() != before;
        if changed {
            st.bytes += incoming;
            st.version += 1;
        }
        st.fan_out(&self.hub.counters);
        Ok(changed.then(|| (st.version, st.doc.save())))
    }

    async fn persist(&self, version: u64, bytes: Vec<u8>) {
        let mut saved = self.room.saved.lock().await;
        // A newer version may already be on disk.
        if version <= *saved {
            return;
        }
        match self.hub.store.save(&self.room.id, bytes).await {
            Ok(()) => *saved = version,
            Err(e) => {
                tracing::warn!(doc = %self.room.id, error = %e, "annotation document not saved");
            }
        }
    }
}

impl<S: DocStore> Drop for Connection<S> {
    fn drop(&mut self) {
        lock(&self.room.state).peers.remove(&self.id);
        self.hub.counters.connected.fetch_sub(1, Ordering::Relaxed);
        tracing::info!(doc = %self.room.id, peer = self.id, "annotation sync client left");
    }
}
