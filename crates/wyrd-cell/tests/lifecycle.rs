#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Unprivileged tests: the overlay's upper layer is simulated by writing
//! directly into the branch directory, which is exactly what overlayfs
//! would leave there (files, whiteouts, opaque markers).

use std::collections::BTreeMap;
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};

use proptest::prelude::*;
use wyrd_cell::session::CreateOptions;
use wyrd_cell::{CellError, DataDir, Session};

struct Env {
    _tmp: tempfile::TempDir,
    data: DataDir,
    ws: PathBuf,
}

fn env() -> Env {
    let tmp = tempfile::tempdir().unwrap();
    let ws = tmp.path().join("ws");
    fs::create_dir(&ws).unwrap();
    fs::write(ws.join("keep.txt"), "original keep\n").unwrap();
    fs::write(ws.join("victim.txt"), "will be deleted\n").unwrap();
    fs::create_dir(ws.join("src")).unwrap();
    fs::write(ws.join("src/lib.rs"), "fn main() {}\n").unwrap();
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

fn upper(s: &Session) -> PathBuf {
    s.dir().upper(&s.record().current_branch)
}

fn whiteout(p: &Path) {
    nix::sys::stat::mknod(
        p,
        nix::sys::stat::SFlag::S_IFCHR,
        nix::sys::stat::Mode::empty(),
        0,
    )
    .unwrap();
}

/// Tree fingerprint including whiteouts and opaque markers.
fn fingerprint(dir: &Path) -> BTreeMap<Vec<u8>, String> {
    let mut out = BTreeMap::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in fs::read_dir(&d).unwrap() {
            let p = e.unwrap().path();
            let rel = p.strip_prefix(dir).unwrap().as_os_str().as_bytes().to_vec();
            let m = fs::symlink_metadata(&p).unwrap();
            let v = if m.file_type().is_char_device() {
                "whiteout".to_string()
            } else if m.is_dir() {
                stack.push(p.clone());
                let mut buf = [0u8; 4];
                let opaque = rustix::fs::lgetxattr(&p, "user.overlay.opaque", &mut buf[..]).is_ok();
                format!("dir:{:o}:{opaque}", m.permissions().mode() & 0o777)
            } else if m.is_symlink() {
                format!("link:{}", fs::read_link(&p).unwrap().display())
            } else {
                format!(
                    "file:{:o}:{}",
                    m.permissions().mode() & 0o777,
                    String::from_utf8_lossy(&fs::read(&p).unwrap())
                )
            };
            out.insert(rel, v);
        }
    }
    out
}

#[test]
fn checkpoint_rewind_and_fork_restore_exact_upper_layers() {
    let e = env();
    let mut s = create(&e);
    let up = upper(&s);
    fs::write(up.join("keep.txt"), "agent edit 1\n").unwrap();
    let c1 = s.checkpoint(Some("call-1".into())).unwrap();
    let fp1 = fingerprint(&up);
    whiteout(&up.join("victim.txt"));
    fs::create_dir(up.join("new")).unwrap();
    fs::write(up.join("new/x.txt"), "x").unwrap();
    std::os::unix::fs::symlink("../keep.txt", up.join("new/link")).unwrap();
    let c2 = s.checkpoint(None).unwrap();
    assert_eq!(c2.parent.as_deref(), Some(c1.id.as_str()));
    let fp2 = fingerprint(&up);

    let b = s.rewind(&c1.id, None).unwrap();
    assert_eq!(b.id, "b");
    assert_eq!(s.record().current_branch, "b");
    assert_eq!(fingerprint(&upper(&s)), fp1);

    let f = s.rewind(&c2.id, Some("experiment")).unwrap();
    assert_eq!(f.name, "experiment");
    assert_eq!(
        fingerprint(&upper(&s)),
        fp2,
        "whiteouts and symlinks restored"
    );

    // Old branches are untouched.
    assert_eq!(fingerprint(&s.dir().upper("a")), fp2);
    // Branch names resolve to their latest checkpoint.
    assert_eq!(s.resolve("a").unwrap().id, c2.id);
    assert!(matches!(
        s.rewind("nope", None),
        Err(CellError::NotFound(_))
    ));
    assert!(
        s.rewind(&c1.id, Some("experiment")).is_err(),
        "duplicate branch name"
    );
}

#[test]
fn opaque_directories_survive_rewind() {
    let e = env();
    let mut s = create(&e);
    let up = upper(&s);
    fs::create_dir(up.join("src")).unwrap();
    rustix::fs::setxattr(
        up.join("src"),
        "user.overlay.opaque",
        b"y",
        rustix::fs::XattrFlags::empty(),
    )
    .unwrap();
    fs::write(up.join("src/only.rs"), "replaced dir\n").unwrap();
    let c = s.checkpoint(None).unwrap();
    let fp = fingerprint(&up);
    s.rewind(&c.id, None).unwrap();
    assert_eq!(fingerprint(&upper(&s)), fp);
}

#[test]
fn diff_lists_changes_between_checkpoints() {
    let e = env();
    let mut s = create(&e);
    let up = upper(&s);
    fs::write(up.join("keep.txt"), "v1").unwrap();
    s.checkpoint(None).unwrap();
    fs::write(up.join("keep.txt"), "v2").unwrap();
    fs::write(up.join("added.txt"), "a").unwrap();
    whiteout(&up.join("victim.txt"));
    s.checkpoint(None).unwrap();
    let d: Vec<String> = s
        .diff("c1", "c2")
        .unwrap()
        .into_iter()
        .map(|c| format!("{:?} {} {}", c.kind, c.entry, c.path))
        .collect();
    assert_eq!(
        d,
        vec![
            "Added file added.txt",
            "Modified file keep.txt",
            "Added deleted victim.txt"
        ]
    );
}

#[test]
fn apply_writes_changes_and_detects_conflicts() {
    let e = env();
    let mut s = create(&e);
    let up = upper(&s);
    fs::write(up.join("keep.txt"), "agent version\n").unwrap();
    whiteout(&up.join("victim.txt"));
    fs::create_dir(up.join("docs")).unwrap();
    fs::write(up.join("docs/new.md"), "# new\n").unwrap();
    s.checkpoint(None).unwrap();

    let dry = s.apply("head", true, false).unwrap();
    assert!(!dry.applied);
    assert_eq!(dry.ops.len(), 4);
    assert_eq!(dry.conflicts.len(), 0);
    assert_eq!(
        fs::read_to_string(e.ws.join("keep.txt")).unwrap(),
        "original keep\n",
        "dry run"
    );

    // The user edits keep.txt in the real workspace meanwhile.
    std::thread::sleep(std::time::Duration::from_millis(20));
    fs::write(e.ws.join("keep.txt"), "user edit\n").unwrap();
    match s.apply("head", false, false) {
        Err(CellError::Conflicts(c)) => assert_eq!(c, vec!["keep.txt"]),
        other => panic!("expected conflict, got {other:?}"),
    }
    assert_eq!(
        fs::read_to_string(e.ws.join("keep.txt")).unwrap(),
        "user edit\n"
    );

    let r = s.apply("head", false, true).unwrap();
    assert!(r.applied);
    assert_eq!(
        fs::read_to_string(e.ws.join("keep.txt")).unwrap(),
        "agent version\n"
    );
    assert!(!e.ws.join("victim.txt").exists());
    assert_eq!(
        fs::read_to_string(e.ws.join("docs/new.md")).unwrap(),
        "# new\n"
    );
    assert_eq!(
        fs::read_to_string(e.ws.join("src/lib.rs")).unwrap(),
        "fn main() {}\n",
        "untouched"
    );
}

#[test]
fn ledger_records_every_change_and_seals() {
    let e = env();
    let mut s = create(&e);
    fs::write(upper(&s).join("keep.txt"), "x").unwrap();
    s.checkpoint(None).unwrap();
    s.rewind("c1", Some("try")).unwrap();
    s.apply("head", true, false).unwrap();
    s.close("done").unwrap();
    let v = wyrd_ledger::verify(&s.dir().ledger()).unwrap();
    assert!(v.ok(), "{v:?}");
    assert!(v.sealed);
    let l = wyrd_ledger::Ledger::open_read_only(s.dir().ledger()).unwrap();
    let kinds: Vec<&str> = l
        .events_after(0, 100)
        .unwrap()
        .iter()
        .map(wyrd_schema::Event::kind)
        .collect::<Vec<_>>()
        .into_iter()
        .collect();
    assert_eq!(
        kinds,
        vec![
            "meta.session.started",
            "state.checkpoint.created",
            "state.checkpoint.forked",
            "meta.session.state_changed",
            "meta.session.state_changed",
            "meta.session.closed",
            "meta.session.state_changed",
        ]
    );
}

#[test]
fn workspace_and_data_dir_must_not_nest() {
    let e = env();
    let r = Session::create(
        &DataDir::new(e.ws.join(".wyrd")),
        CreateOptions {
            workspace: e.ws.clone(),
            command: vec!["true".into()],
            observe: false,
            name: None,
            agent_kind: "test".into(),
            deny_tools: Vec::new(),
            policy: None,
            kernel_release: String::new(),
            capabilities: BTreeMap::new(),
        },
    );
    assert!(matches!(r, Err(CellError::Invalid(_))));
}

#[test]
fn sessions_open_by_prefix_and_list() {
    let e = env();
    let s = create(&e);
    let id = s.record().id.clone();
    let opened = Session::open(&e.data, &id[..8]).unwrap();
    assert_eq!(opened.record().id, id);
    assert_eq!(Session::list(&e.data).unwrap().len(), 1);
    assert!(Session::open(&e.data, "zzzz").is_err());
}

#[derive(Debug, Clone)]
enum Op {
    Write(u8, u8),
    Delete(u8),
    Checkpoint,
    Rewind(prop::sample::Index),
    Fork(prop::sample::Index),
}

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        3 => (0u8..6, any::<u8>()).prop_map(|(f, v)| Op::Write(f, v)),
        1 => (0u8..6).prop_map(Op::Delete),
        2 => Just(Op::Checkpoint),
        1 => any::<prop::sample::Index>().prop_map(Op::Rewind),
        1 => any::<prop::sample::Index>().prop_map(Op::Fork),
    ]
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(10))]

    /// Randomised lifecycle: after any sequence of edits, checkpoints,
    /// rewinds, and forks, a rewound branch's upper layer is byte-identical
    /// to the layer at that checkpoint. 10 cases × 100 operations.
    #[test]
    fn randomized_lifecycle(ops in prop::collection::vec(op(), 100)) {
        let e = env();
        let mut s = create(&e);
        let mut expected: BTreeMap<String, BTreeMap<Vec<u8>, String>> = BTreeMap::new();
        let mut forks = 0;
        for op in ops {
            let up = upper(&s);
            match op {
                Op::Write(f, v) => {
                    let p = up.join(format!("f{f}.txt"));
                    if fs::symlink_metadata(&p).is_ok_and(|m| m.file_type().is_char_device()) {
                        fs::remove_file(&p).unwrap();
                    }
                    fs::write(&p, [v]).unwrap();
                }
                Op::Delete(f) => {
                    let p = up.join(format!("f{f}.txt"));
                    let _ = fs::remove_file(&p);
                    whiteout(&p);
                }
                Op::Checkpoint => {
                    let c = s.checkpoint(None).unwrap();
                    expected.insert(c.id, fingerprint(&up));
                }
                Op::Rewind(i) | Op::Fork(i) if !expected.is_empty() => {
                    let id = expected.keys().nth(i.index(expected.len())).unwrap().clone();
                    let name = if matches!(op, Op::Fork(_)) { forks += 1; Some(format!("fork{forks}")) } else { None };
                    s.rewind(&id, name.as_deref()).unwrap();
                    prop_assert_eq!(&fingerprint(&upper(&s)), &expected[&id]);
                }
                Op::Rewind(_) | Op::Fork(_) => {}
            }
        }
        let v = wyrd_ledger::verify(&s.dir().ledger()).unwrap();
        prop_assert!(v.ok());
    }
}

#[test]
fn proc_cgroup_parsing() {
    use wyrd_cell::cgroup::parse_proc_cgroup;
    assert_eq!(
        parse_proc_cgroup("0::/user.slice/user-1000.slice/session-3.scope\n"),
        Some(PathBuf::from(
            "/sys/fs/cgroup/user.slice/user-1000.slice/session-3.scope"
        ))
    );
    assert_eq!(parse_proc_cgroup("12:memory:/foo\n"), None);
    assert_eq!(
        parse_proc_cgroup("0::/\n"),
        Some(PathBuf::from("/sys/fs/cgroup/"))
    );
}

#[test]
fn overlay_paths_with_separators_are_rejected() {
    use wyrd_cell::exec::check_overlay_path;
    assert!(check_overlay_path(Path::new("/home/u/repo")).is_ok());
    assert!(check_overlay_path(Path::new("/home/u/a,b")).is_err());
    assert!(check_overlay_path(Path::new("/home/u/a:b")).is_err());
}

#[test]
fn file_at_reads_the_checkpoint_the_workspace_or_nothing() {
    use wyrd_cell::session::FileSource;
    let e = env();
    let mut s = create(&e);
    let up = upper(&s);
    fs::write(up.join("keep.txt"), "agent edit\n").unwrap();
    whiteout(&up.join("victim.txt"));
    fs::create_dir(up.join("src")).unwrap();
    rustix::fs::setxattr(
        up.join("src"),
        "user.overlay.opaque",
        b"y",
        rustix::fs::XattrFlags::empty(),
    )
    .unwrap();
    fs::write(up.join("src/new.rs"), "fn new() {}\n").unwrap();
    let c = s.checkpoint(None).unwrap();

    let f = s.file_at(&c.id, "keep.txt", 1 << 20).unwrap();
    assert_eq!(f.source, FileSource::Checkpoint);
    assert_eq!(f.bytes.as_deref(), Some(&b"agent edit\n"[..]));

    let f = s.file_at(&c.id, "victim.txt", 1 << 20).unwrap();
    assert_eq!((f.source, f.bytes), (FileSource::Absent, None), "whiteout");

    let f = s.file_at(&c.id, "src/lib.rs", 1 << 20).unwrap();
    assert_eq!(f.source, FileSource::Absent, "hidden by the opaque dir");
    let f = s.file_at(&c.id, "src/new.rs", 1 << 20).unwrap();
    assert_eq!(f.source, FileSource::Checkpoint);

    fs::write(e.ws.join("untouched.txt"), "below the cell\n").unwrap();
    let f = s.file_at(&c.id, "untouched.txt", 5).unwrap();
    assert_eq!(f.source, FileSource::Workspace);
    assert_eq!(f.bytes.as_deref(), Some(&b"below"[..]));
    assert!(f.truncated);

    for bad in ["", "/etc/passwd", "../x", "a/../b", "a//b", "./a"] {
        assert!(s.file_at(&c.id, bad, 10).is_err(), "{bad:?}");
    }
}
