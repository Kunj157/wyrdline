//! Single-writer task with batching.
//!
//! Producers call [`WriterHandle::submit`]; one blocking thread owns the
//! ledger and commits batches of up to [`MAX_BATCH`] events or whatever
//! arrived within [`MAX_BATCH_DELAY`].

use std::time::{Duration, Instant};

use tokio::sync::{mpsc, oneshot};
use wyrd_schema::Event;

use crate::{Ledger, LedgerError, Result};

pub const MAX_BATCH: usize = 1_000;
pub const MAX_BATCH_DELAY: Duration = Duration::from_millis(5);

enum Msg {
    Event(Box<Event>),
    Flush(oneshot::Sender<Result<u64>>),
    Close(oneshot::Sender<Ledger>),
}

/// Handle to the writer thread.
#[derive(Debug, Clone)]
pub struct WriterHandle {
    tx: mpsc::Sender<Msg>,
}

impl std::fmt::Debug for Msg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Msg")
    }
}

/// Spawn the writer thread. `capacity` bounds the queue (backpressure).
pub fn spawn_writer(ledger: Ledger, capacity: usize) -> WriterHandle {
    let (tx, rx) = mpsc::channel(capacity);
    // If the thread cannot be spawned, the receiver is dropped and every
    // call on the handle fails with `WriterGone`.
    if let Err(e) = std::thread::Builder::new()
        .name("wyrd-ledger-writer".into())
        .spawn(move || run(ledger, rx))
    {
        tracing::error!(error = %e, "failed to spawn ledger writer");
    }
    WriterHandle { tx }
}

impl WriterHandle {
    /// Queue an event; waits only when the queue is full.
    pub async fn submit(&self, event: Event) -> Result<()> {
        self.tx
            .send(Msg::Event(Box::new(event)))
            .await
            .map_err(|_| LedgerError::WriterGone)
    }

    /// Commit everything queued so far; returns the head sequence number.
    pub async fn flush(&self) -> Result<u64> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Msg::Flush(tx))
            .await
            .map_err(|_| LedgerError::WriterGone)?;
        rx.await.map_err(|_| LedgerError::WriterGone)?
    }

    /// Flush, stop the thread, and get the ledger back (e.g. to seal it).
    pub async fn close(self) -> Result<Ledger> {
        self.flush().await?;
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(Msg::Close(tx))
            .await
            .map_err(|_| LedgerError::WriterGone)?;
        rx.await.map_err(|_| LedgerError::WriterGone)
    }
}

fn commit(ledger: &mut Ledger, batch: &mut Vec<Event>, failed: &mut Option<String>) -> Result<u64> {
    if batch.is_empty() {
        return Ok(ledger.head().0);
    }
    let n = batch.len();
    ledger.append(std::mem::take(batch)).inspect_err(|e| {
        tracing::error!(error = %e, events = n, "ledger batch commit failed");
        failed.get_or_insert_with(|| format!("{n} events lost: {e}"));
    })
}

fn run(mut ledger: Ledger, mut rx: mpsc::Receiver<Msg>) {
    let mut batch: Vec<Event> = Vec::with_capacity(MAX_BATCH);
    let mut first_at: Option<Instant> = None;
    // A failed commit is reported on the next flush so producers learn about it.
    let mut failed: Option<String> = None;
    loop {
        let msg = if batch.is_empty() {
            match rx.blocking_recv() {
                Some(m) => m,
                None => break,
            }
        } else {
            match rx.try_recv() {
                Ok(m) => m,
                Err(mpsc::error::TryRecvError::Empty) => {
                    let waited = first_at.map_or(Duration::ZERO, |t| t.elapsed());
                    if waited >= MAX_BATCH_DELAY {
                        let _ = commit(&mut ledger, &mut batch, &mut failed);
                        first_at = None;
                    } else {
                        std::thread::sleep(Duration::from_micros(200));
                    }
                    continue;
                }
                Err(mpsc::error::TryRecvError::Disconnected) => break,
            }
        };
        match msg {
            Msg::Event(ev) => {
                if batch.is_empty() {
                    first_at = Some(Instant::now());
                }
                batch.push(*ev);
                if batch.len() >= MAX_BATCH {
                    let _ = commit(&mut ledger, &mut batch, &mut failed);
                    first_at = None;
                }
            }
            Msg::Flush(reply) => {
                let r = commit(&mut ledger, &mut batch, &mut failed);
                first_at = None;
                let r = match failed.take() {
                    Some(msg) => Err(LedgerError::Commit(msg)),
                    None => r,
                };
                let _ = reply.send(r);
            }
            Msg::Close(reply) => {
                let _ = reply.send(ledger);
                return;
            }
        }
    }
    let _ = commit(&mut ledger, &mut batch, &mut failed);
}
