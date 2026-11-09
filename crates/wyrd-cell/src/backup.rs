//! `wyrd backup`: a consistent copy of the data directory, and the check that
//! proves a copy (a backup, or a tree about to be restored) is whole. The
//! format is `wyrdline.backup.v1` (`docs/backup-format.md`).
//!
//! The order is the whole design. The store lock is taken shared for the whole
//! copy, so `gc` cannot sweep a chunk a copied record refers to. Each session
//! is copied under its own session lock, which no checkpoint or close can
//! hold at the same time, so a session's ledger, record and baseline are one
//! moment. The content store is copied last: every chunk a copied record
//! names was committed before that record was saved, so it is in the store by
//! the time the store is copied. A copied tree is verified before it gets its
//! manifest, and it gets its final name only after that.

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use wyrd_cas::Cas;
use wyrd_ledger::{HostKey, Ledger};
use wyrd_schema::SessionState;

use crate::cgroup::Cgroup;
use crate::{CellError, DataDir, Result, gc, io_err, now_ms, session, state};

/// Format identifier in the manifest.
pub const FORMAT_ID: &str = "wyrdline.backup.v1";
/// The manifest file at the root of a backup, written last.
pub const MANIFEST: &str = "BACKUP.json";

/// Session files copied beside the ledger snapshot, when present. Overlay
/// `work/` directories, `session.lock`, the restored markers, control tokens
/// and logs are not copied; `branches/*/upper` is not copied either (see
/// `docs/backup-format.md`: uncheckpointed workspace edits are not backed up).
const SESSION_FILES: &[&str] = &["baseline.cbor", "judge-runs.jsonl", "mcp-pins.json"];

/// One session as the backup (or a restored tree) holds it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionEntry {
    pub id: String,
    pub status: SessionState,
    pub events: u64,
    pub head_hash: String,
    pub sealed: bool,
}

/// The manifest at the root of a backup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Manifest {
    pub format: String,
    pub created_ms: i64,
    pub wyrd_version: String,
    /// Public half of the host key, lowercase hex. `None` without a host key.
    pub host_public_key: Option<String>,
    pub host_fingerprint: Option<String>,
    /// Whether the private seed (`keys/host.key`) is in the backup.
    pub host_key_included: bool,
    pub sessions: Vec<SessionEntry>,
    /// Checkpoint manifests reachable from the sessions.
    pub roots: usize,
    /// Distinct chunks reachable from those roots, all read back and checked.
    pub chunks: usize,
}

/// What [`verify_tree`] found in a copy of the data directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeReport {
    pub sessions: Vec<SessionEntry>,
    pub roots: usize,
    pub chunks: usize,
}

/// Check a copy of the data directory: every session record reads, every
/// ledger verifies (chain and seal), and every chunk reachable from a
/// checkpoint reads back with its hash. Nothing is repaired or deleted. Run it
/// only on a tree this process owns (a staging copy): opening the store may
/// create its directories and sweep its staging files.
pub fn verify_tree(root: &Path) -> Result<TreeReport> {
    let data = DataDir::new(root);
    let mut sessions = Vec::new();
    for id in session_ids(&data)? {
        let s = data.session(&id);
        let rec = state::load(&s).map_err(|e| {
            CellError::Invalid(format!("session {id}: its record cannot be read ({e})"))
        })?;
        if rec.id != id {
            return Err(CellError::Invalid(format!(
                "session {id}: its record names session {}",
                rec.id
            )));
        }
        let v = wyrd_ledger::verify(&s.ledger())?;
        if let Some(p) = &v.problem {
            return Err(CellError::Invalid(format!(
                "session {id}: its ledger does not verify ({p:?})"
            )));
        }
        sessions.push(SessionEntry {
            id,
            status: rec.status,
            events: v.events,
            head_hash: v.head_hash,
            sealed: v.sealed,
        });
    }
    let roots = gc::roots(&data)?;
    let mut chunks = HashSet::new();
    if !roots.is_empty() {
        if !data.cas().join("FORMAT").is_file() {
            return Err(CellError::Invalid(format!(
                "{} has checkpoints but no content store",
                root.display()
            )));
        }
        let cas = Cas::open(data.cas())?;
        for root_hash in &roots {
            let manifest = cas.get_manifest(*root_hash)?;
            chunks.insert(*root_hash);
            for entry in &manifest.entries {
                for c in &entry.chunks {
                    cas.get(*c)?;
                    chunks.insert(*c);
                }
            }
        }
    }
    Ok(TreeReport {
        sessions,
        roots: roots.len(),
        chunks: chunks.len(),
    })
}

/// Restore a backup into the data directory, which must be absent or empty.
/// The tree is copied beside the data directory, verified against the manifest,
/// and renamed into place only then. Nothing is written into the data directory
/// until that check has passed. A backup holds no branch overlays, so a
/// restored session keeps its ledger, record, checkpoints and baseline, but
/// not the uncheckpointed edits of its branches (see `docs/backup-format.md`).
pub fn restore(source: &Path, data: &DataDir) -> Result<Manifest> {
    let manifest = read_manifest(source)?;
    check_target(source, data)?;
    let target = data.root();
    let parent = parent_of(target);
    fs::create_dir_all(parent).map_err(io_err(parent))?;
    let name = target
        .file_name()
        .ok_or_else(|| CellError::Invalid(format!("{} has no name", target.display())))?;
    let stage = parent.join(format!(
        ".{}.restore-{}",
        name.to_string_lossy(),
        std::process::id()
    ));
    if stage.exists() {
        fs::remove_dir_all(&stage).map_err(io_err(&stage))?;
    }
    fs::create_dir(&stage).map_err(io_err(&stage))?;
    set_mode(&stage, 0o700)?;
    let ready = copy_in(source, &stage)
        .and_then(|()| rebuild_all(&stage))
        .and_then(|()| check_copy(&stage, &manifest))
        .and_then(|()| wyrd_cas::sync_fs_of(&stage).map_err(CellError::from));
    if let Err(e) = ready {
        let _ = fs::remove_dir_all(&stage);
        return Err(e);
    }
    publish(&stage, target)?;
    Ok(manifest)
}

/// The data directory must be absent or empty, and must not be inside the
/// backup (or the backup inside it).
fn check_target(source: &Path, data: &DataDir) -> Result<()> {
    let target = data.root();
    if target.exists() {
        let empty = target.is_dir()
            && fs::read_dir(target)
                .map_err(io_err(target))?
                .next()
                .is_none();
        if !empty {
            return Err(CellError::Occupied(target.to_path_buf()));
        }
    }
    let abs_target = resolve(target)?;
    let abs_source = resolve(source)?;
    if abs_target.starts_with(&abs_source) || abs_source.starts_with(&abs_target) {
        return Err(CellError::Invalid(format!(
            "the backup {} and the data directory {} must not contain each other",
            abs_source.display(),
            abs_target.display()
        )));
    }
    Ok(())
}

/// Entries of one session directory in a backup: the ledger snapshot and the
/// files `backup` writes beside it.
const SESSION_ENTRIES: &[&str] = &[
    "ledger.wyrdledger",
    "session.json",
    "baseline.cbor",
    "import.json",
    "launch.json",
    "judge-runs.jsonl",
    "mcp-pins.json",
];

/// Copy the parts of a backup that a data directory holds. Every entry is
/// checked against what `backup` writes; anything else is refused, so an
/// unexpected entry cannot land in the data directory unchecked.
fn copy_in(source: &Path, stage: &Path) -> Result<()> {
    for e in read_entries(source)? {
        match e.name.as_str() {
            MANIFEST => {}
            "sessions" => copy_sessions_in(&e.path, &stage.join("sessions"))?,
            "cas" => copy_store_in(&e.path, &stage.join("cas"))?,
            "policies" => copy_tree(&e.path, &stage.join("policies"))?,
            "keys" => copy_keys_in(&e.path, &stage.join("keys"))?,
            other => return Err(unexpected(other)),
        }
    }
    Ok(())
}

fn unexpected(entry: &str) -> CellError {
    CellError::Invalid(format!(
        "unexpected entry {entry} in the backup; it is not a wyrd backup"
    ))
}

/// The entries of a directory: name and path, each with its file type checked
/// to be a regular file or a directory.
struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
}

fn read_entries(dir: &Path) -> Result<Vec<Entry>> {
    let mut out = Vec::new();
    for e in fs::read_dir(dir).map_err(io_err(dir))? {
        let e = e.map_err(io_err(dir))?;
        let path = e.path();
        let kind = fs::symlink_metadata(&path)
            .map_err(io_err(&path))?
            .file_type();
        if !(kind.is_dir() || kind.is_file()) {
            return Err(CellError::Invalid(format!(
                "{} is not a regular file or directory",
                path.display()
            )));
        }
        out.push(Entry {
            name: e.file_name().to_string_lossy().into_owned(),
            path,
            is_dir: kind.is_dir(),
        });
    }
    Ok(out)
}

fn copy_sessions_in(src: &Path, dst: &Path) -> Result<()> {
    for s in read_entries(src)? {
        if !s.is_dir || s.name.starts_with('.') {
            return Err(unexpected(&format!("sessions/{}", s.name)));
        }
        let to = dst.join(&s.name);
        fs::create_dir_all(&to).map_err(io_err(&to))?;
        for f in read_entries(&s.path)? {
            if f.is_dir || !SESSION_ENTRIES.contains(&f.name.as_str()) {
                return Err(unexpected(&format!("sessions/{}/{}", s.name, f.name)));
            }
            copy_file_if_present(&f.path, &to.join(&f.name))?;
        }
    }
    Ok(())
}

fn copy_store_in(src: &Path, dst: &Path) -> Result<()> {
    fs::create_dir_all(dst).map_err(io_err(dst))?;
    for e in read_entries(src)? {
        match (e.name.as_str(), e.is_dir) {
            ("FORMAT", false) => copy_file_if_present(&e.path, &dst.join("FORMAT"))?,
            ("chunks", true) => copy_tree(&e.path, &dst.join("chunks"))?,
            _ => return Err(unexpected(&format!("cas/{}", e.name))),
        }
    }
    Ok(())
}

fn copy_keys_in(src: &Path, dst: &Path) -> Result<()> {
    for e in read_entries(src)? {
        if e.is_dir || e.name != "host.key" {
            return Err(unexpected(&format!("keys/{}", e.name)));
        }
    }
    let key = src.join("host.key");
    if key.exists() {
        fs::create_dir_all(dst).map_err(io_err(dst))?;
        set_mode(dst, 0o700)?;
        let to = dst.join("host.key");
        copy_file_if_present(&key, &to)?;
        set_mode(&to, 0o600)?;
    }
    Ok(())
}

/// Rebuild the branch overlays of every session in a copied tree.
fn rebuild_all(stage: &Path) -> Result<()> {
    let stage_data = DataDir::new(stage);
    for id in session_ids(&stage_data)? {
        session::rebuild_overlays(&stage_data, &id)?;
    }
    Ok(())
}

/// The copied tree must match the manifest: the same sessions, with the same
/// events and head, and the same checkpoints and chunks.
fn check_copy(stage: &Path, manifest: &Manifest) -> Result<()> {
    let report = verify_tree(stage)?;
    if report.sessions != manifest.sessions
        || report.roots != manifest.roots
        || report.chunks != manifest.chunks
    {
        return Err(CellError::Invalid(
            "the backup does not match its manifest (sessions, checkpoints or chunks differ)"
                .into(),
        ));
    }
    Ok(())
}

/// Read and check a backup's manifest.
pub fn read_manifest(dir: &Path) -> Result<Manifest> {
    let path = dir.join(MANIFEST);
    let bytes = fs::read(&path).map_err(io_err(&path))?;
    let m: Manifest = serde_json::from_slice(&bytes)
        .map_err(|e| CellError::Invalid(format!("{}: {e}", path.display())))?;
    if m.format != FORMAT_ID {
        return Err(CellError::Invalid(format!(
            "{}: unsupported backup format {}",
            path.display(),
            m.format
        )));
    }
    Ok(m)
}

/// Copy the data directory to `dest`, which must not exist or be an empty
/// directory outside the data directory. The copy is built beside `dest`,
/// verified, and renamed into place. Sessions that are running (or whose
/// session lock is held) make the backup fail, naming them. The host private
/// key is copied only with `include_key`.
pub fn backup(data: &DataDir, dest: &Path, include_key: bool) -> Result<Manifest> {
    if !data.root().is_dir() {
        return Err(CellError::Invalid(format!(
            "no data directory at {}",
            data.root().display()
        )));
    }
    // Everything that can refuse runs before anything is created, so a refused
    // backup leaves the data directory and the destination's parent as they were.
    check_dest(data, dest)?;
    let cas = Cas::open(data.cas())?;
    // Held to the end of the copy: no collection can remove a chunk a copied
    // record refers to.
    let _store = cas.lock_shared()?;
    refuse_imports(data)?;
    let stage = stage_dir(dest)?;
    let built = build(data, &stage, include_key);
    let manifest = match built {
        Ok(m) => m,
        Err(e) => {
            let _ = fs::remove_dir_all(&stage);
            return Err(e);
        }
    };
    publish(&stage, dest)?;
    Ok(manifest)
}

/// Rename a finished staging directory to its final name, and sync the
/// parent. A failure removes the staging directory: nothing is left half made.
fn publish(stage: &Path, dest: &Path) -> Result<()> {
    let step = || -> Result<()> {
        if dest.exists() {
            fs::remove_dir(dest).map_err(io_err(dest))?;
        }
        fs::rename(stage, dest).map_err(io_err(dest))
    };
    if let Err(e) = step() {
        let _ = fs::remove_dir_all(stage);
        return Err(e);
    }
    wyrd_cas::sync_dir(parent_of(dest)).map_err(CellError::from)
}

fn build(data: &DataDir, stage: &Path, include_key: bool) -> Result<Manifest> {
    let ids = session_ids(data)?;
    refuse_running(data, &ids)?;
    for id in &ids {
        copy_session(data, id, stage)?;
    }
    copy_tree(&data.policies(), &stage.join("policies"))?;
    let host = copy_host_key(data, stage, include_key)?;
    copy_store(data, stage)?;
    wyrd_cas::sync_fs_of(stage)?;
    let report = verify_tree(stage)?;
    // Opening the copied store (for the check above) created its staging
    // directory; a backup holds none of the store's own staging state.
    let _ = fs::remove_dir(stage.join("cas/tmp"));
    let manifest = Manifest {
        format: FORMAT_ID.into(),
        created_ms: now_ms(),
        wyrd_version: env!("CARGO_PKG_VERSION").into(),
        host_public_key: host.as_ref().map(|k| hex_lower(&k.public_key())),
        host_fingerprint: host.as_ref().map(HostKey::fingerprint),
        host_key_included: include_key && host.is_some(),
        sessions: report.sessions,
        roots: report.roots,
        chunks: report.chunks,
    };
    let bytes =
        serde_json::to_vec_pretty(&manifest).map_err(|e| CellError::Invalid(e.to_string()))?;
    wyrd_cas::write_durable(&stage.join(MANIFEST), &bytes)?;
    Ok(manifest)
}

fn hex_lower(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
            let _ = write!(s, "{b:02x}");
            s
        })
}

/// The directory that holds `dest`.
fn parent_of(dest: &Path) -> &Path {
    dest.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."))
}

/// Refuse a destination that is occupied, or that is inside the data
/// directory or contains it. Creates nothing: the containment test resolves
/// the deepest existing ancestor, so a path under `sessions/` cannot leave a
/// half-made session directory behind.
fn check_dest(data: &DataDir, dest: &Path) -> Result<()> {
    if dest.file_name().is_none() {
        return Err(CellError::Invalid(format!(
            "{} has no name",
            dest.display()
        )));
    }
    if dest.exists() {
        let empty = dest.is_dir() && fs::read_dir(dest).map_err(io_err(dest))?.next().is_none();
        if !empty {
            return Err(CellError::Occupied(dest.to_path_buf()));
        }
    }
    let abs = resolve(dest)?;
    let root = fs::canonicalize(data.root()).map_err(io_err(data.root()))?;
    if abs.starts_with(&root) || root.starts_with(&abs) {
        return Err(CellError::Invalid(format!(
            "the backup {} and the data directory {} must not contain each other",
            abs.display(),
            root.display()
        )));
    }
    Ok(())
}

/// The absolute path of `p`, with symbolic links resolved for the part that
/// exists. Missing components are kept as they are, and nothing is created.
fn resolve(p: &Path) -> Result<PathBuf> {
    let abs = std::path::absolute(p).map_err(io_err(p))?;
    let mut missing = Vec::new();
    let mut cur = abs.as_path();
    loop {
        if let Ok(found) = fs::canonicalize(cur) {
            return Ok(missing.iter().rev().fold(found, |acc, name| acc.join(name)));
        }
        match (cur.file_name(), cur.parent()) {
            (Some(name), Some(parent)) => {
                missing.push(name.to_os_string());
                cur = parent;
            }
            _ => {
                return Err(CellError::Invalid(format!(
                    "cannot resolve {}",
                    p.display()
                )));
            }
        }
    }
}

/// Create the destination's parents and the staging directory beside the
/// destination, private. Called only after every refusal has passed.
fn stage_dir(dest: &Path) -> Result<PathBuf> {
    let name = dest
        .file_name()
        .ok_or_else(|| CellError::Invalid(format!("{} has no name", dest.display())))?;
    let parent = parent_of(dest);
    fs::create_dir_all(parent).map_err(io_err(parent))?;
    let stage = parent.join(format!(
        ".{}.partial-{}",
        name.to_string_lossy(),
        std::process::id()
    ));
    if stage.exists() {
        fs::remove_dir_all(&stage).map_err(io_err(&stage))?;
    }
    fs::create_dir(&stage).map_err(io_err(&stage))?;
    set_mode(&stage, 0o700)?;
    Ok(stage)
}

fn refuse_imports(data: &DataDir) -> Result<()> {
    let dir = data.sessions();
    let Ok(rd) = fs::read_dir(&dir) else {
        return Ok(());
    };
    for e in rd.flatten() {
        if e.file_name().to_string_lossy().starts_with(".import-") {
            return Err(CellError::Busy(format!(
                "an import is in progress ({}); wait for it, then back up again",
                e.file_name().to_string_lossy()
            )));
        }
    }
    Ok(())
}

/// Session directories, by name. A session is one directory of `sessions/`.
fn session_ids(data: &DataDir) -> Result<Vec<String>> {
    let dir = data.sessions();
    let Ok(rd) = fs::read_dir(&dir) else {
        return Ok(Vec::new());
    };
    let mut ids = Vec::new();
    for e in rd {
        let e = e.map_err(io_err(&dir))?;
        let name = e.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !e.file_type().map_err(io_err(&e.path()))?.is_dir() {
            continue;
        }
        ids.push(name);
    }
    ids.sort();
    Ok(ids)
}

/// Whether a session has a live agent. A Cell's cgroup, when the session has
/// one, is populated while it runs; the recorded pid covers the rest (observe
/// mode, and non-root runs without a cgroup), and a pid that no longer exists
/// does not count. A reused pid counts as running: that refuses, which is safe.
fn is_running(rec: &state::SessionRecord) -> bool {
    let in_cgroup = rec
        .cgroup
        .as_ref()
        .is_some_and(|c| Cgroup::at(c).is_populated());
    let pid_alive = rec
        .pid
        .and_then(|p| i32::try_from(p).ok())
        .and_then(rustix::process::Pid::from_raw)
        .is_some_and(|p| match rustix::process::test_kill_process(p) {
            Ok(()) => true,
            // EPERM: the process exists, owned by someone else.
            Err(e) => e == rustix::io::Errno::PERM,
        });
    in_cgroup || pid_alive
}

/// Refuse, naming every running or busy session, before anything is copied.
fn refuse_running(data: &DataDir, ids: &[String]) -> Result<()> {
    let mut busy = Vec::new();
    for id in ids {
        let s = data.session(id);
        let Some(_lock) = state::try_lock(&s.lock())? else {
            busy.push(id.clone());
            continue;
        };
        if is_running(&state::load(&s)?) {
            busy.push(id.clone());
        }
    }
    if busy.is_empty() {
        Ok(())
    } else {
        Err(CellError::Busy(format!(
            "sessions running or busy: {}; stop them (`wyrd stop <id>`) or wait, then back up again",
            busy.join(", ")
        )))
    }
}

/// Copy one session under its session lock. A session that is running, or
/// whose lock is taken in the meantime, makes the backup fail.
fn copy_session(data: &DataDir, id: &str, stage: &Path) -> Result<()> {
    let s = data.session(id);
    let Some(_lock) = state::try_lock(&s.lock())? else {
        return Err(CellError::Busy(format!(
            "session {id} became busy during the backup; back up again"
        )));
    };
    let rec = state::load(&s).map_err(|e| {
        CellError::Invalid(format!("session {id}: its record cannot be read ({e})"))
    })?;
    if is_running(&rec) {
        return Err(CellError::Busy(format!(
            "session {id} started running during the backup; back up again"
        )));
    }
    let dst = stage.join("sessions").join(id);
    fs::create_dir_all(&dst).map_err(io_err(&dst))?;
    Ledger::open_read_only(s.ledger())
        .map_err(|e| CellError::Invalid(format!("session {id}: its ledger cannot be read ({e})")))?
        .snapshot_to(dst.join("ledger.wyrdledger"))?;
    copy_file_if_present(&s.state(), &dst.join("session.json"))?;
    copy_file_if_present(&s.import(), &dst.join("import.json"))?;
    copy_file_if_present(&s.spec(), &dst.join("launch.json"))?;
    for name in SESSION_FILES {
        copy_file_if_present(&s.root().join(name), &dst.join(name))?;
    }
    Ok(())
}

/// The host key: its public half is recorded in the manifest; its private seed
/// is copied only when asked, mode 0600 in a 0700 directory.
fn copy_host_key(data: &DataDir, stage: &Path, include_key: bool) -> Result<Option<HostKey>> {
    let src = data.host_key();
    if !src.exists() {
        return Ok(None);
    }
    let key = HostKey::load(&src)?;
    if include_key {
        let dir = stage.join("keys");
        fs::create_dir_all(&dir).map_err(io_err(&dir))?;
        set_mode(&dir, 0o700)?;
        let dst = dir.join("host.key");
        fs::copy(&src, &dst).map_err(io_err(&dst))?;
        set_mode(&dst, 0o600)?;
    }
    Ok(Some(key))
}

fn copy_store(data: &DataDir, stage: &Path) -> Result<()> {
    let cas_dst = stage.join("cas");
    fs::create_dir_all(&cas_dst).map_err(io_err(&cas_dst))?;
    copy_file_if_present(&data.cas().join("FORMAT"), &cas_dst.join("FORMAT"))?;
    copy_tree(&data.cas().join("chunks"), &cas_dst.join("chunks"))
}

fn copy_file_if_present(src: &Path, dst: &Path) -> Result<()> {
    match fs::symlink_metadata(src) {
        Ok(m) if m.is_file() => {
            fs::copy(src, dst).map_err(io_err(dst))?;
            Ok(())
        }
        Ok(_) => Err(CellError::Invalid(format!(
            "{} is not a regular file; a backup copies only files",
            src.display()
        ))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(io_err(src)(e)),
    }
}

/// Copy a directory tree of regular files and directories. A missing source is
/// an empty tree; a symbolic link or other special file is an error.
pub(crate) fn copy_tree(src: &Path, dst: &Path) -> Result<()> {
    let Ok(rd) = fs::read_dir(src) else {
        return Ok(());
    };
    fs::create_dir_all(dst).map_err(io_err(dst))?;
    for e in rd {
        let e = e.map_err(io_err(src))?;
        let from = e.path();
        let to = dst.join(e.file_name());
        let kind = fs::symlink_metadata(&from)
            .map_err(io_err(&from))?
            .file_type();
        if kind.is_dir() {
            copy_tree(&from, &to)?;
        } else if kind.is_file() {
            fs::copy(&from, &to).map_err(io_err(&to))?;
        } else {
            return Err(CellError::Invalid(format!(
                "{} is not a regular file or directory; a backup copies only those",
                from.display()
            )));
        }
    }
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).map_err(io_err(path))
}
