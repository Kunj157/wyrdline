#![allow(
    clippy::unwrap_used,
    clippy::panic,
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::many_single_char_names
)]

use std::path::PathBuf;

use wyrd_common::{AT_FDCWD, RawEvent, flag, kind};
use wyrd_schema::EventBody;
use wyrd_sensor::tracefs::{field_offset, offsets_from};
use wyrd_sensor::{Decoder, decode::normalize};

const NEWTASK_617: &str = "name: task_newtask
format:
	field:unsigned short common_type;	offset:0;	size:2;	signed:0;
	field:int common_pid;	offset:4;	size:4;	signed:1;

	field:pid_t pid;	offset:8;	size:4;	signed:1;
	field:char comm[16];	offset:12;	size:16;	signed:0;
	field:unsigned long clone_flags;	offset:32;	size:8;	signed:0;
	field:short oom_score_adj;	offset:40;	size:2;	signed:1;
";

const EXIT_617: &str = "name: sched_process_exit
	field:int common_pid;	offset:4;	size:4;	signed:1;
	field:char comm[16];	offset:8;	size:16;	signed:0;
	field:pid_t pid;	offset:24;	size:4;	signed:1;
	field:int prio;	offset:28;	size:4;	signed:1;
	field:bool group_dead;	offset:32;	size:1;	signed:0;
";

const EXIT_OLD: &str = "name: sched_process_exit
	field:char comm[16];	offset:8;	size:16;	signed:0;
	field:pid_t pid;	offset:24;	size:4;	signed:1;
	field:int prio;	offset:28;	size:4;	signed:1;
";

#[test]
fn tracefs_offsets_for_current_and_old_layouts() {
    let o = offsets_from(NEWTASK_617, EXIT_617).unwrap();
    assert_eq!(
        (
            o.newtask_pid,
            o.newtask_clone_flags,
            o.exit_pid,
            o.exit_group_dead
        ),
        (8, 32, 24, 32)
    );
    let old = offsets_from(NEWTASK_617, EXIT_OLD).unwrap();
    assert_eq!(
        old.exit_group_dead,
        u32::MAX,
        "missing group_dead falls back"
    );
    assert_eq!(field_offset(NEWTASK_617, "comm"), Some(12), "array fields");
    assert_eq!(field_offset(NEWTASK_617, "common_pid"), Some(4));
    assert_eq!(field_offset(NEWTASK_617, "nope"), None);
    assert!(offsets_from("garbage", EXIT_617).is_err());
}

fn set(buf: &mut [u8], s: &str) -> u16 {
    buf[..s.len()].copy_from_slice(s.as_bytes());
    buf[s.len()] = 0;
    (s.len() + 1) as u16
}

const CG: u64 = 77;

fn ev(k: u32, tgid: u32) -> RawEvent {
    let mut e = RawEvent::ZERO;
    e.kind = k;
    e.tgid = tgid;
    e.tid = tgid;
    e.cgroup_id = CG;
    e.ts_ns = 1000;
    e
}

fn path_ev(k: u32, tgid: u32, dfd: i64, p: &str) -> RawEvent {
    let mut e = ev(k, tgid);
    e.a0 = dfd;
    e.len0 = set(&mut e.s0, p);
    e
}

fn decoder() -> Decoder {
    // Empty proc root: nothing can be looked up, everything must be tracked.
    let mut d = Decoder::with_proc_root("/nonexistent-proc");
    d.set_workspace(CG, PathBuf::from("/w"));
    d
}

fn body(d: &mut Decoder, e: &RawEvent) -> Option<EventBody> {
    d.decode(e).map(|x| x.body)
}

#[test]
fn relative_paths_follow_chdir_fork_and_dirfds() {
    let mut d = decoder();
    // Process 10 starts in the workspace (default), creates a file.
    let mut open = path_ev(kind::OPEN, 10, AT_FDCWD, "a.txt");
    open.a1 = 0o1101;
    open.ret = 3;
    match body(&mut d, &open).unwrap() {
        EventBody::FileOpenWrite(f) => {
            assert_eq!(f.file.path, "/w/a.txt");
            assert!(f.file.in_cell);
        }
        other => panic!("{other:?}"),
    }
    // chdir into src, then a relative mkdir resolves there.
    assert!(body(&mut d, &path_ev(kind::CHDIR, 10, AT_FDCWD, "src/../src")).is_none());
    match body(&mut d, &path_ev(kind::MKDIR, 10, AT_FDCWD, "gen")).unwrap() {
        EventBody::FileMkdir(f) => assert_eq!(f.path, "/w/src/gen"),
        other => panic!("{other:?}"),
    }
    // Directory fd opened read-only: not reported, but tracked.
    let mut dir = path_ev(kind::OPEN, 10, AT_FDCWD, "/w/build");
    dir.ret = 5;
    assert!(body(&mut d, &dir).is_none());
    // Fork: child inherits cwd and fds; unlinkat(5, "x") in the child.
    let mut fork = ev(kind::FORK, 10);
    fork.a0 = 11;
    match body(&mut d, &fork).unwrap() {
        EventBody::ProcFork(f) => assert_eq!(f.child_pid, 11),
        other => panic!("{other:?}"),
    }
    match body(&mut d, &path_ev(kind::UNLINK, 11, 5, "x.o")).unwrap() {
        EventBody::FileUnlink(f) => assert_eq!(f.file.path, "/w/build/x.o"),
        other => panic!("{other:?}"),
    }
    let mut exec = path_ev(kind::EXEC, 11, AT_FDCWD, "./tool");
    exec.argc = 1;
    set(&mut exec.argv[0], "tool");
    match body(&mut d, &exec).unwrap() {
        EventBody::ProcExec(x) => {
            assert_eq!(x.filename, "/w/src/tool");
            assert_eq!(x.cwd.as_deref(), Some("/w/src"));
            assert_eq!(x.argv, vec!["tool"]);
        }
        other => panic!("{other:?}"),
    }
    // Paths outside the workspace are not in the Cell.
    match body(&mut d, &path_ev(kind::UNLINK, 11, AT_FDCWD, "/home/u/x")).unwrap() {
        EventBody::FileUnlink(f) => assert!(!f.file.in_cell),
        other => panic!("{other:?}"),
    }
}

#[test]
fn exit_codes_parents_and_start_times() {
    let mut d = decoder();
    let mut fork = ev(kind::FORK, 1);
    fork.a0 = 2;
    fork.ts_ns = 555;
    d.decode(&fork).unwrap();
    let mut code = ev(kind::EXIT_GROUP, 2);
    code.a0 = 3;
    assert!(d.decode(&code).is_none());
    let exit = d.decode(&ev(kind::EXIT, 2)).unwrap();
    assert_eq!(exit.process.ppid, 1);
    assert_eq!(exit.process.start_ns, 555);
    match exit.body {
        EventBody::ProcExit(x) => assert_eq!(x.exit_code, 3),
        other => panic!("{other:?}"),
    }
    // Killed without exit_group: unknown code.
    match d.decode(&ev(kind::EXIT, 9)).unwrap().body {
        EventBody::ProcExit(x) => assert_eq!(x.exit_code, -1),
        other => panic!("{other:?}"),
    }
}

#[test]
fn reads_are_dropped_unless_sensitive() {
    let mut d = decoder();
    let mut read = path_ev(kind::OPEN, 1, AT_FDCWD, "src/main.rs");
    read.ret = 3;
    assert!(d.decode(&read).is_none());
    let mut secret = path_ev(kind::OPEN, 1, AT_FDCWD, ".env");
    secret.ret = 4;
    match body(&mut d, &secret).unwrap() {
        EventBody::FileReadSensitive(f) => assert_eq!(f.path, "/w/.env"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn rename_chmod_truncate_connect_and_truncation_flags() {
    let mut d = decoder();
    let mut r = path_ev(kind::RENAME, 1, AT_FDCWD, "old");
    r.a1 = AT_FDCWD;
    r.len1 = set(&mut r.s1, "dir/new");
    match body(&mut d, &r).unwrap() {
        EventBody::FileRename(x) => assert_eq!(
            (x.from.path.as_str(), x.to.path.as_str()),
            ("/w/old", "/w/dir/new")
        ),
        other => panic!("{other:?}"),
    }
    let mut c = path_ev(kind::CHMOD, 1, AT_FDCWD, "run.sh");
    c.a1 = 0o100_755;
    match body(&mut d, &c).unwrap() {
        EventBody::FileChmod(x) => assert_eq!(x.mode, 0o755),
        other => panic!("{other:?}"),
    }
    let mut t = path_ev(kind::TRUNCATE, 1, 0, "log");
    t.a0 = 42;
    match body(&mut d, &t).unwrap() {
        EventBody::FileTruncate(x) => assert_eq!((x.file.path.as_str(), x.size), ("/w/log", 42)),
        other => panic!("{other:?}"),
    }
    let mut n = ev(kind::CONNECT, 1);
    n.a0 = 2;
    n.a1 = 443;
    n.s0[..4].copy_from_slice(&[93, 184, 216, 34]);
    match body(&mut d, &n).unwrap() {
        EventBody::NetConnect(x) => assert_eq!(
            (x.family, x.addr.as_str(), x.port),
            (4, "93.184.216.34", 443)
        ),
        other => panic!("{other:?}"),
    }
    let mut n6 = ev(kind::CONNECT, 1);
    n6.a0 = 10;
    n6.a1 = 53;
    n6.s0[15] = 1;
    match body(&mut d, &n6).unwrap() {
        EventBody::NetConnect(x) => assert_eq!((x.family, x.addr.as_str()), (6, "::1")),
        other => panic!("{other:?}"),
    }
    let mut long = path_ev(kind::UNLINK, 1, AT_FDCWD, "/w/very/long");
    long.flags = flag::S0_TRUNCATED;
    match body(&mut d, &long).unwrap() {
        EventBody::FileUnlink(x) => assert!(x.file.truncated),
        other => panic!("{other:?}"),
    }
}

#[test]
fn normalisation() {
    use std::path::Path;
    assert_eq!(normalize(Path::new("/a/./b/../c")), "/a/c");
    assert_eq!(normalize(Path::new("/../..")), "/");
    assert_eq!(normalize(Path::new("/w/src/../src/x")), "/w/src/x");
}

#[test]
fn the_ebpf_object_is_embedded() {
    assert!(wyrd_sensor::EBPF_OBJECT.starts_with(b"\x7fELF"));
    assert!(wyrd_sensor::loader::PROGRAMS.len() >= 20);
}
