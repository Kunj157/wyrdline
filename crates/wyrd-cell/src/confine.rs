//! Landlock confinement for the agent process: writes only beneath an
//! allow-list, no reads beneath a deny-list, and optionally TCP connects
//! only to an allow-list of ports.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use landlock::{
    ABI, AccessFs, AccessNet, NetPort, RestrictionStatus, Ruleset, RulesetAttr, RulesetCreatedAttr,
    RulesetStatus, path_beneath_rules,
};
use serde::{Deserialize, Serialize};

use crate::{CellError, Result};

/// Highest Landlock ABI whose rights we request.
const TARGET_ABI: ABI = ABI::V5;

/// How completely the ruleset is enforced by the running kernel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Enforcement {
    Full,
    Partial,
    None,
}

/// What the agent's process tree may touch.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Confinement {
    /// Writable hierarchies (the workspace, temporary and agent state dirs).
    pub write_allow: Vec<PathBuf>,
    /// Hierarchies the agent may not read. Landlock grants rights and never
    /// denies, so these are enforced as their complement: read rights on
    /// every sibling along the path to each denied entry, and only
    /// directory listing on the path itself.
    #[serde(default)]
    pub read_deny: Vec<PathBuf>,
    /// TCP ports the agent may connect to; `None` leaves TCP unrestricted.
    #[serde(default)]
    pub tcp_connect_ports: Option<Vec<u16>>,
}

/// Default writable locations besides the workspace: temporary files,
/// device nodes such as `/dev/null`, and the dot-directories coding agents
/// keep their own state in. Writes there escape the Cell and are recorded
/// as irreversible.
pub fn default_write_allow(workspace: &Path, home: Option<&Path>) -> Vec<PathBuf> {
    let mut v = vec![
        workspace.to_path_buf(),
        PathBuf::from("/tmp"),
        PathBuf::from("/var/tmp"),
        PathBuf::from("/dev"),
        PathBuf::from("/proc/self"),
    ];
    if let Some(h) = home {
        for d in [
            ".cache",
            ".claude",
            ".claude.json",
            ".codex",
            ".gemini",
            ".config",
            ".npm",
        ] {
            v.push(h.join(d));
        }
    }
    v
}

/// The read rules that implement `deny`: `(full, listing_only)`.
/// `full` gets every read right; `listing_only` (the ancestors of denied
/// entries) may be listed but nothing beneath them read unless it is in
/// `full`. `children` lists a directory (empty for files or errors).
/// Entries created later inside an ancestor are not readable.
pub fn read_complement(
    deny: &[PathBuf],
    children: &dyn Fn(&Path) -> Vec<PathBuf>,
) -> (Vec<PathBuf>, Vec<PathBuf>) {
    let deny: BTreeSet<&Path> = deny.iter().map(PathBuf::as_path).collect();
    // Every ancestor of a denied entry must be expanded, not granted.
    let mut ancestors: BTreeSet<PathBuf> = BTreeSet::new();
    for d in &deny {
        let mut p = d.parent();
        while let Some(a) = p {
            ancestors.insert(a.to_path_buf());
            p = a.parent();
        }
    }
    let mut full = Vec::new();
    let mut listing = Vec::new();
    let mut stack = vec![PathBuf::from("/")];
    while let Some(dir) = stack.pop() {
        if !ancestors.contains(&dir) {
            continue;
        }
        listing.push(dir.clone());
        for child in children(&dir) {
            if deny.contains(child.as_path()) {
                continue;
            }
            if ancestors.contains(&child) {
                stack.push(child);
            } else {
                full.push(child);
            }
        }
    }
    full.sort();
    listing.sort();
    (full, listing)
}

fn list_dir(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|it| it.filter_map(|e| e.ok().map(|e| e.path())).collect())
        .unwrap_or_default()
}

fn ll(e: impl std::fmt::Display) -> CellError {
    CellError::Sys(format!("landlock: {e}"))
}

/// Restrict the calling thread (and every later child). Missing paths are
/// ignored.
pub fn restrict(c: &Confinement) -> Result<Enforcement> {
    let write = AccessFs::from_write(TARGET_ABI);
    let read = AccessFs::from_read(TARGET_ABI);
    let restrict_reads = !c.read_deny.is_empty();
    let handled = if restrict_reads { write | read } else { write };
    let mut ruleset = Ruleset::default().handle_access(handled).map_err(ll)?;
    if c.tcp_connect_ports.is_some() {
        ruleset = ruleset.handle_access(AccessNet::ConnectTcp).map_err(ll)?;
    }
    let existing = |v: &[PathBuf]| v.iter().filter(|p| p.exists()).cloned().collect::<Vec<_>>();
    let writable = existing(&c.write_allow);
    let mut created = ruleset
        .create()
        .map_err(ll)?
        .add_rules(path_beneath_rules(&writable, handled))
        .map_err(ll)?;
    if restrict_reads {
        let deny: Vec<PathBuf> = existing(&c.read_deny);
        let (full, listing) = read_complement(&deny, &list_dir);
        created = created
            .add_rules(path_beneath_rules(&full, read))
            .map_err(ll)?
            .add_rules(path_beneath_rules(&listing, AccessFs::ReadDir))
            .map_err(ll)?;
    }
    if let Some(ports) = &c.tcp_connect_ports {
        created =
            created
                .add_rules(ports.iter().map(|p| {
                    Ok::<_, landlock::RulesetError>(NetPort::new(*p, AccessNet::ConnectTcp))
                }))
                .map_err(ll)?;
    }
    let status: RestrictionStatus = created.restrict_self().map_err(ll)?;
    Ok(match status.ruleset {
        RulesetStatus::FullyEnforced => Enforcement::Full,
        RulesetStatus::PartiallyEnforced => Enforcement::Partial,
        RulesetStatus::NotEnforced => Enforcement::None,
    })
}

/// What a kernel with Landlock ABI `abi` cannot enforce of `c` (empty when
/// everything is enforced), for the session record.
pub fn unenforced(c: &Confinement, abi: Option<u32>) -> Vec<String> {
    let mut out = Vec::new();
    match abi {
        None | Some(0) => out.push("Landlock is not available: no filesystem confinement".into()),
        Some(abi) => {
            if abi < 3 {
                out.push("truncation is not restricted (Landlock ABI < 3)".into());
            }
            if abi < 4 && c.tcp_connect_ports.is_some() {
                out.push("TCP port rules need Landlock ABI 4".into());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree(dir: &Path) -> Vec<PathBuf> {
        let kids: &[&str] = match dir.to_str().unwrap_or_default() {
            "/" => &["/etc", "/home", "/tmp", "/usr"],
            "/home" => &["/home/u", "/home/v"],
            "/home/u" => &[
                "/home/u/.ssh",
                "/home/u/.aws",
                "/home/u/proj",
                "/home/u/.bashrc",
            ],
            _ => &[],
        };
        kids.iter().map(PathBuf::from).collect()
    }

    #[test]
    fn complement_grants_siblings_and_lists_ancestors() {
        let deny = vec![PathBuf::from("/home/u/.ssh"), PathBuf::from("/home/u/.aws")];
        let (full, listing) = read_complement(&deny, &tree);
        let p = |v: &[&str]| v.iter().map(PathBuf::from).collect::<Vec<_>>();
        assert_eq!(
            full,
            p(&[
                "/etc",
                "/home/u/.bashrc",
                "/home/u/proj",
                "/home/v",
                "/tmp",
                "/usr"
            ])
        );
        assert_eq!(listing, p(&["/", "/home", "/home/u"]));
    }

    #[test]
    fn no_denials_means_no_rules() {
        assert_eq!(read_complement(&[], &tree), (Vec::new(), Vec::new()));
    }

    #[test]
    fn denying_a_whole_top_level_dir() {
        let (full, listing) = read_complement(&[PathBuf::from("/home")], &tree);
        assert_eq!(full.len(), 3);
        assert!(!full.contains(&PathBuf::from("/home")));
        assert_eq!(listing, vec![PathBuf::from("/")]);
    }

    #[test]
    fn unenforced_rights_per_abi() {
        let ports = Confinement {
            tcp_connect_ports: Some(vec![443]),
            ..Confinement::default()
        };
        assert_eq!(unenforced(&ports, Some(5)), Vec::<String>::new());
        assert_eq!(unenforced(&ports, Some(3)).len(), 1);
        assert_eq!(unenforced(&Confinement::default(), Some(2)).len(), 1);
        assert_eq!(unenforced(&Confinement::default(), None).len(), 1);
    }
}
