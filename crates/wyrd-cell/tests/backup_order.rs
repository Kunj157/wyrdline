#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The order of a backup's durable steps (#318). This is its own test binary:
//! step recording is process-wide, so no other test may run beside it.

use std::collections::BTreeMap;

use wyrd_cas::{Step, record_steps};
use wyrd_cell::session::CreateOptions;
use wyrd_cell::{DataDir, Session, backup};

#[test]
fn a_backup_takes_the_store_lock_shared_before_anything_and_releases_it_last() {
    let tmp = tempfile::tempdir().unwrap();
    let data = DataDir::new(tmp.path().join("data"));
    let ws = tmp.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let mut s = Session::create(
        &data,
        CreateOptions {
            workspace: ws,
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
    .unwrap();
    std::fs::write(s.dir().upper("a").join("f.txt"), "x").unwrap();
    s.checkpoint(None).unwrap();

    let dest = tmp.path().join("out").join("backup");
    let (r, steps) = record_steps(|| backup::backup(&data, &dest, false));
    r.unwrap();
    assert_eq!(steps.first(), Some(&Step::Lock { exclusive: false }));
    assert_eq!(steps.last(), Some(&Step::Unlock));
    let locks = steps
        .iter()
        .filter(|s| matches!(s, Step::Lock { .. }))
        .count();
    assert_eq!(locks, 1, "one lock for the whole copy: {steps:?}");
}
