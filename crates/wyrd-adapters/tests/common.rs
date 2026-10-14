#![allow(dead_code, clippy::unwrap_used)]

use std::sync::Mutex;

use wyrd_adapters::Sink;
use wyrd_schema::EventBody;

/// In-memory sink recording events and checkpoint triggers.
#[derive(Debug, Default)]
pub struct MemSink {
    pub events: Mutex<Vec<EventBody>>,
    pub log: Mutex<Vec<String>>,
    pub fail: bool,
}

impl Sink for MemSink {
    fn append(&self, bodies: Vec<EventBody>) -> Result<(), String> {
        if self.fail {
            return Err("disk full".into());
        }
        let mut log = self.log.lock().unwrap();
        for b in &bodies {
            log.push(b.kind().to_string());
        }
        self.events.lock().unwrap().extend(bodies);
        Ok(())
    }
    fn before_tool_call(&self) {
        self.log.lock().unwrap().push("before_tool_call".into());
    }
    fn after_tool_call(&self, call_id: &str) {
        self.log
            .lock()
            .unwrap()
            .push(format!("after_tool_call {call_id}"));
    }
}

pub fn fixtures() -> Vec<(String, serde_json::Value)> {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/claude-code");
    let mut files: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    files.sort();
    files
        .into_iter()
        .map(|p| {
            let v = serde_json::from_str(&std::fs::read_to_string(&p).unwrap()).unwrap();
            (p.file_name().unwrap().to_string_lossy().into_owned(), v)
        })
        .collect()
}
