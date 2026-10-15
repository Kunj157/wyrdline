//! Raw kernel events → `wyrd-schema` effect events.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use wyrd_common::{AT_FDCWD, RawEvent, flag, kind};
use wyrd_schema::{
    EventBody, FileChmod, FileOpenWrite, FilePath, FileRename, FileTruncate, FileUnlink,
    NetConnect, NetVerdict, ProcExec, ProcExit, ProcFork, ProcessRef,
};

const O_WRONLY: i64 = 0o1;
const O_RDWR: i64 = 0o2;
const O_CREAT: i64 = 0o100;
const O_TRUNC: i64 = 0o1000;
const O_APPEND: i64 = 0o2000;
const AT_REMOVEDIR: i64 = 0x200;

/// One decoded effect.
#[derive(Debug, Clone, PartialEq)]
pub struct Decoded {
    pub cgroup_id: u64,
    pub ts_mono_ns: u64,
    pub process: ProcessRef,
    pub body: EventBody,
}

/// Stateful decoder: tracks exit codes, parents, start times, and working
/// directories so later events can be completed.
#[derive(Debug, Default)]
pub struct Decoder {
    /// Root for `/proc` lookups (tests use a fake tree).
    proc_root: PathBuf,
    /// Workspace per registered cgroup, for `in_cell`.
    workspaces: HashMap<u64, PathBuf>,
    exit_codes: HashMap<u32, i32>,
    parents: HashMap<u32, u32>,
    starts: HashMap<u32, u64>,
    /// Working directory per process, followed through chdir/fchdir and
    /// inherited at fork.
    cwds: HashMap<u32, PathBuf>,
    /// Open file descriptors per process (path at open time), for
    /// `*at(dirfd, …)` resolution; inherited at fork.
    fds: HashMap<(u32, i64), PathBuf>,
}

fn bytes(buf: &[u8], len: u16) -> &[u8] {
    let n = usize::from(len).min(buf.len());
    let s = &buf[..n];
    s.strip_suffix(&[0]).unwrap_or(s)
}

fn text(buf: &[u8], len: u16) -> String {
    String::from_utf8_lossy(bytes(buf, len)).into_owned()
}

fn cstr(buf: &[u8]) -> String {
    let n = buf.iter().position(|&b| b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..n]).into_owned()
}

impl Decoder {
    pub fn new() -> Self {
        Decoder {
            proc_root: PathBuf::from("/proc"),
            ..Default::default()
        }
    }

    pub fn with_proc_root(root: impl Into<PathBuf>) -> Self {
        Decoder {
            proc_root: root.into(),
            ..Default::default()
        }
    }

    pub fn set_workspace(&mut self, cgroup_id: u64, workspace: PathBuf) {
        self.workspaces.insert(cgroup_id, workspace);
    }

    pub fn forget_cgroup(&mut self, cgroup_id: u64) {
        self.workspaces.remove(&cgroup_id);
    }

    fn proc_link(&self, tgid: u32, rel: &str) -> Option<PathBuf> {
        std::fs::read_link(self.proc_root.join(tgid.to_string()).join(rel)).ok()
    }

    /// Current directory: tracked, else live `/proc`, else the session's workspace.
    fn cwd(&mut self, tgid: u32, cgroup_id: u64) -> Option<PathBuf> {
        if let Some(c) = self.cwds.get(&tgid) {
            return Some(c.clone());
        }
        let c = self
            .proc_link(tgid, "cwd")
            .or_else(|| self.workspaces.get(&cgroup_id).cloned())?;
        self.cwds.insert(tgid, c.clone());
        Some(c)
    }

    fn ppid(&self, tgid: u32) -> u32 {
        if let Some(p) = self.parents.get(&tgid) {
            return *p;
        }
        std::fs::read_to_string(self.proc_root.join(tgid.to_string()).join("stat"))
            .ok()
            .and_then(|s| {
                // pid (comm) state ppid …  — comm may contain spaces/parens.
                let rest = &s[s.rfind(')')? + 2..];
                rest.split_whitespace().nth(1)?.parse().ok()
            })
            .unwrap_or(0)
    }

    /// Resolve a path relative to `dfd` of process `tgid`.
    fn resolve(&mut self, tgid: u32, cgroup_id: u64, dfd: i64, raw: &str) -> (String, bool) {
        if raw.starts_with('/') {
            return (normalize(Path::new(raw)), true);
        }
        let base = if dfd == AT_FDCWD {
            self.cwd(tgid, cgroup_id)
        } else {
            self.fds
                .get(&(tgid, dfd))
                .cloned()
                .or_else(|| self.proc_link(tgid, &format!("fd/{dfd}")))
        };
        match base {
            Some(b) => (normalize(&b.join(raw)), true),
            None => (raw.to_string(), false),
        }
    }

    fn file(&mut self, ev: &RawEvent, dfd: i64, second: bool) -> FilePath {
        let (raw, trunc_flag) = if second {
            (text(&ev.s1, ev.len1), flag::S1_TRUNCATED)
        } else {
            (text(&ev.s0, ev.len0), flag::S0_TRUNCATED)
        };
        let (path, absolute) = self.resolve(ev.tgid, ev.cgroup_id, dfd, &raw);
        let in_cell = absolute
            && self
                .workspaces
                .get(&ev.cgroup_id)
                .is_some_and(|w| Path::new(&path).starts_with(w));
        FilePath {
            path,
            in_cell,
            truncated: ev.flags & trunc_flag != 0,
        }
    }

    #[allow(clippy::too_many_lines)] // one arm per kind keeps the mapping readable
    /// Decode one raw event. Returns `None` for events that only update
    /// state (exit codes) or are filtered (non-sensitive reads).
    pub fn decode(&mut self, ev: &RawEvent) -> Option<Decoded> {
        if ev.start_ns != 0 {
            self.starts.entry(ev.tgid).or_insert(ev.start_ns);
        }
        let body = match ev.kind {
            kind::EXIT_GROUP => {
                self.exit_codes
                    .insert(ev.tgid, i32::try_from(ev.a0).unwrap_or(-1));
                return None;
            }
            kind::EXEC => {
                let n_args = usize::from(ev.argc).min(ev.argv.len());
                let argv = ev.argv[..n_args].iter().map(|a| cstr(a)).collect();
                let cwd = self.cwd(ev.tgid, ev.cgroup_id);
                let (filename, _) =
                    self.resolve(ev.tgid, ev.cgroup_id, AT_FDCWD, &text(&ev.s0, ev.len0));
                EventBody::ProcExec(ProcExec {
                    filename,
                    argv,
                    cwd: cwd.map(|c| c.display().to_string()),
                    truncated: ev.flags & (flag::S0_TRUNCATED | flag::ARGV_TRUNCATED) != 0,
                })
            }
            kind::FORK => {
                let child = u32::try_from(ev.a0).unwrap_or(0);
                self.parents.insert(child, ev.tgid);
                self.starts.insert(child, ev.ts_ns);
                if let Some(c) = self.cwd(ev.tgid, ev.cgroup_id) {
                    self.cwds.insert(child, c);
                }
                let inherited: Vec<(i64, PathBuf)> = self
                    .fds
                    .iter()
                    .filter(|((t, _), _)| *t == ev.tgid)
                    .map(|((_, fd), p)| (*fd, p.clone()))
                    .collect();
                for (fd, p) in inherited {
                    self.fds.insert((child, fd), p);
                }
                EventBody::ProcFork(ProcFork {
                    child_pid: child,
                    child_start_ns: ev.ts_ns,
                })
            }
            kind::EXIT => {
                let code = self.exit_codes.remove(&ev.tgid);
                self.cwds.remove(&ev.tgid);
                self.fds.retain(|(t, _), _| *t != ev.tgid);
                EventBody::ProcExit(ProcExit {
                    exit_code: code.unwrap_or(-1),
                    // Without exit_group the process was killed; the signal is
                    // not observable from these tracepoints.
                    signal: None,
                })
            }
            kind::CHDIR => {
                let (dir, ok) =
                    self.resolve(ev.tgid, ev.cgroup_id, AT_FDCWD, &text(&ev.s0, ev.len0));
                if ok {
                    self.cwds.insert(ev.tgid, PathBuf::from(dir));
                }
                return None;
            }
            kind::FCHDIR => {
                if let Some(dir) = self.fds.get(&(ev.tgid, ev.a0)).cloned() {
                    self.cwds.insert(ev.tgid, dir);
                }
                return None;
            }
            kind::OPEN => {
                let file = self.file(ev, ev.a0, false);
                if file.path.starts_with('/') {
                    self.fds
                        .insert((ev.tgid, ev.ret), PathBuf::from(&file.path));
                }
                if ev.a1 & (O_WRONLY | O_RDWR | O_CREAT | O_TRUNC | O_APPEND) != 0 {
                    EventBody::FileOpenWrite(FileOpenWrite {
                        file,
                        flags: u32::try_from(ev.a1 & 0xffff_ffff).unwrap_or(0),
                    })
                } else if wyrd_bundle::redact::is_secret_path(&file.path) {
                    EventBody::FileReadSensitive(file)
                } else {
                    return None;
                }
            }
            kind::UNLINK => {
                let file = self.file(ev, ev.a0, false);
                EventBody::FileUnlink(FileUnlink {
                    file,
                    is_dir: ev.a1 & AT_REMOVEDIR != 0,
                })
            }
            kind::RENAME => {
                let from = self.file(ev, ev.a0, false);
                let to = self.file(ev, ev.a1, true);
                EventBody::FileRename(FileRename { from, to })
            }
            kind::MKDIR => EventBody::FileMkdir(self.file(ev, ev.a0, false)),
            kind::CHMOD => {
                let file = self.file(ev, ev.a0, false);
                EventBody::FileChmod(FileChmod {
                    file,
                    mode: u32::try_from(ev.a1 & 0o7777).unwrap_or(0),
                })
            }
            kind::TRUNCATE => {
                let file = self.file(ev, AT_FDCWD, false);
                EventBody::FileTruncate(FileTruncate {
                    file,
                    size: u64::try_from(ev.a0).unwrap_or(0),
                })
            }
            kind::CONNECT => {
                let (family, addr) = if ev.a0 == 2 {
                    let b = &ev.s0[..4];
                    (
                        4,
                        std::net::Ipv4Addr::new(b[0], b[1], b[2], b[3]).to_string(),
                    )
                } else {
                    let mut a = [0u8; 16];
                    a.copy_from_slice(&ev.s0[..16]);
                    (6, std::net::Ipv6Addr::from(a).to_string())
                };
                EventBody::NetConnect(NetConnect {
                    family,
                    addr,
                    port: u16::try_from(ev.a1).unwrap_or(0),
                    dns_name: None,
                    verdict: NetVerdict::Allowed,
                })
            }
            _ => return None,
        };
        let process = ProcessRef {
            pid: ev.tgid,
            tid: ev.tid,
            ppid: if ev.ppid != 0 {
                ev.ppid
            } else {
                self.ppid(ev.tgid)
            },
            start_ns: self.starts.get(&ev.tgid).copied().unwrap_or(ev.start_ns),
            cgroup_id: ev.cgroup_id,
        };
        if ev.kind == kind::EXIT {
            // Forget the process only after its exit event carries parent and start.
            self.parents.remove(&ev.tgid);
            self.starts.remove(&ev.tgid);
        }
        Some(Decoded {
            cgroup_id: ev.cgroup_id,
            ts_mono_ns: ev.ts_ns,
            process,
            body,
        })
    }
}

/// Lexically normalise `.` and `..` (no symlink resolution).
pub fn normalize(p: &Path) -> String {
    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    for c in p.components() {
        match c {
            std::path::Component::ParentDir => {
                parts.pop();
            }
            std::path::Component::Normal(n) => parts.push(n),
            _ => {}
        }
    }
    let mut s = String::new();
    for part in parts {
        s.push('/');
        s.push_str(&part.to_string_lossy());
    }
    if s.is_empty() { "/".into() } else { s }
}
