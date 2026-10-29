#![allow(clippy::unwrap_used, clippy::panic)]

//! A session that is answering when it is told to stop must finish the
//! answer first: `drain` waits for connections still being served.

mod common;

use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;
use wyrd_adapters::protocol::call;
use wyrd_adapters::server;
use wyrd_adapters::{Direction, Ingest, Request, Response, Sink, StaticPolicy};
use wyrd_schema::{CaptureMode, EventBody};

/// A sink that holds every append until the test releases it.
struct Gate {
    entered: Mutex<mpsc::Sender<()>>,
    release: Mutex<mpsc::Receiver<()>>,
}

impl Sink for Gate {
    fn append(&self, _: Vec<EventBody>) -> Result<(), String> {
        let _ = self.entered.lock().unwrap().send(());
        let _ = self.release.lock().unwrap().recv();
        Ok(())
    }
}

#[test]
fn drain_waits_for_an_answer_that_is_still_being_written() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("s.sock");
    let listener = server::bind(&path).unwrap();
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let ingest = Arc::new(Ingest::new(
        Gate {
            entered: Mutex::new(entered_tx),
            release: Mutex::new(release_rx),
        },
        CaptureMode::Cell,
        StaticPolicy::default(),
        0,
    ));
    let served = ingest.clone();
    std::thread::spawn(move || server::serve(&listener, &served));

    let client_path = path.clone();
    let client = std::thread::spawn(move || {
        call(
            &client_path,
            &Request::Mcp {
                server: "toy".into(),
                direction: Direction::ToServer,
                message: json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
            },
            Duration::from_secs(10),
        )
    });

    // The handler is inside the sink: its answer is not written yet.
    entered_rx.recv_timeout(Duration::from_secs(10)).unwrap();
    assert!(
        !server::drain(Duration::from_millis(100)),
        "drain must not return while an answer is being written"
    );
    release_tx.send(()).unwrap();
    assert!(
        server::drain(Duration::from_secs(5)),
        "once the answer is written the server is quiet"
    );
    assert_eq!(client.join().unwrap().unwrap(), Response::Ok);
}
