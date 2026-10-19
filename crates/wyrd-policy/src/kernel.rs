//! Kernel rule compiler: a policy's `kernel-rules` into what the kernel
//! can enforce — Landlock paths and ports for the Cell, a CIDR and host
//! allow-list for the cgroup connect programs, and an exec deny list for
//! the effect-time observer. Pure; the caller supplies home and workspace.

use std::net::IpAddr;
use std::path::{Component, Path, PathBuf};

use crate::KernelRules;

/// One network prefix of the egress allow-list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    pub addr: IpAddr,
    pub prefix: u8,
}

impl std::fmt::Display for Cidr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

/// Parse `10.0.0.0/8`, `::1/128`, or a bare address (full-length prefix).
pub fn parse_cidr(s: &str) -> Option<Cidr> {
    let (a, p) = s.split_once('/').unwrap_or((s, ""));
    let addr: IpAddr = a.trim().parse().ok()?;
    let max = if addr.is_ipv4() { 32 } else { 128 };
    let prefix = if p.is_empty() {
        max
    } else {
        p.trim().parse().ok()?
    };
    (prefix <= max).then_some(Cidr { addr, prefix })
}

/// What the kernel will enforce for a session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Compiled {
    /// Extra writable paths (absolute).
    pub write_allow: Vec<PathBuf>,
    /// Paths the agent may not read (absolute).
    pub read_deny: Vec<PathBuf>,
    /// `None`: every TCP port.
    pub tcp_connect_ports: Option<Vec<u16>>,
    /// `None`: no egress rules (everything allowed).
    pub egress_cidrs: Option<Vec<Cidr>>,
    pub egress_hosts: Vec<String>,
    pub exec_deny: Vec<String>,
    /// Rules the kernel cannot express, with why; left to tool-call and
    /// effect-time checks.
    pub not_expressible: Vec<String>,
}

fn has_glob(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// `~` → home, relative → workspace; `None` (with a reason) when the path
/// cannot become one fixed absolute path.
fn expand(p: &str, home: Option<&Path>, workspace: &Path) -> Result<PathBuf, String> {
    if has_glob(p) {
        return Err(format!("{p}: patterns cannot be enforced by Landlock"));
    }
    let path = if p == "~" || p.starts_with("~/") {
        let home = home.ok_or_else(|| format!("{p}: no home directory"))?;
        home.join(p.trim_start_matches('~').trim_start_matches('/'))
    } else if p.starts_with('/') {
        PathBuf::from(p)
    } else {
        workspace.join(p)
    };
    if path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(format!("{p}: `..` is not allowed"));
    }
    Ok(path)
}

pub fn compile(rules: &KernelRules, home: Option<&Path>, workspace: &Path) -> Compiled {
    let mut out = Compiled::default();
    let paths = |list: &[String], into: &mut Vec<PathBuf>, bad: &mut Vec<String>| {
        for p in list {
            match expand(p, home, workspace) {
                Ok(path) => into.push(path),
                Err(why) => bad.push(why),
            }
        }
        into.sort();
        into.dedup();
    };
    paths(
        &rules.fs_write_allow,
        &mut out.write_allow,
        &mut out.not_expressible,
    );
    paths(
        &rules.fs_read_deny,
        &mut out.read_deny,
        &mut out.not_expressible,
    );
    if !rules.egress_allow_ports.is_empty() {
        let mut ports = rules.egress_allow_ports.clone();
        ports.sort_unstable();
        ports.dedup();
        out.tcp_connect_ports = Some(ports);
    }
    if !rules.egress_allow_cidrs.is_empty() || !rules.egress_allow_hosts.is_empty() {
        let mut cidrs = Vec::new();
        for c in &rules.egress_allow_cidrs {
            match parse_cidr(c) {
                Some(c) => cidrs.push(c),
                None => out.not_expressible.push(format!("{c}: not a CIDR")),
            }
        }
        out.egress_cidrs = Some(cidrs);
        out.egress_hosts.clone_from(&rules.egress_allow_hosts);
    }
    out.exec_deny.clone_from(&rules.exec_deny);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_expanded_and_patterns_reported() {
        let rules = KernelRules {
            fs_read_deny: vec![
                "~/.ssh".into(),
                "~/.ssh".into(),
                "/etc/shadow".into(),
                "**/.env*".into(),
            ],
            fs_write_allow: vec!["build".into(), "../x".into()],
            ..KernelRules::default()
        };
        let c = compile(&rules, Some(Path::new("/home/u")), Path::new("/work/shop"));
        assert_eq!(
            c.read_deny,
            vec![PathBuf::from("/etc/shadow"), PathBuf::from("/home/u/.ssh")]
        );
        assert_eq!(c.write_allow, vec![PathBuf::from("/work/shop/build")]);
        assert_eq!(c.not_expressible.len(), 2);
        assert!(c.not_expressible.iter().any(|w| w.starts_with("**/.env*")));
        assert_eq!(c.tcp_connect_ports, None);
        assert_eq!(c.egress_cidrs, None);
    }

    #[test]
    fn home_is_needed_for_tilde() {
        let rules = KernelRules {
            fs_read_deny: vec!["~/.aws".into()],
            ..KernelRules::default()
        };
        let c = compile(&rules, None, Path::new("/w"));
        assert_eq!(c.read_deny, Vec::<PathBuf>::new());
        assert_eq!(c.not_expressible, vec!["~/.aws: no home directory"]);
    }

    #[test]
    fn egress_rules() {
        let rules = KernelRules {
            egress_allow_cidrs: vec![
                "127.0.0.0/8".into(),
                "::1/128".into(),
                "10.1.2.3".into(),
                "300.0.0.0/8".into(),
                "10.0.0.0/33".into(),
            ],
            egress_allow_hosts: vec!["api.anthropic.com".into()],
            egress_allow_ports: vec![443, 80, 443],
            ..KernelRules::default()
        };
        let c = compile(&rules, None, Path::new("/w"));
        let cidrs: Vec<String> = c
            .egress_cidrs
            .unwrap()
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(cidrs, vec!["127.0.0.0/8", "::1/128", "10.1.2.3/32"]);
        assert_eq!(c.not_expressible.len(), 2);
        assert_eq!(c.tcp_connect_ports, Some(vec![80, 443]));
        assert_eq!(c.egress_hosts, vec!["api.anthropic.com"]);
    }

    #[test]
    fn hosts_alone_still_restrict_egress() {
        let rules = KernelRules {
            egress_allow_hosts: vec!["example.org".into()],
            ..KernelRules::default()
        };
        assert_eq!(
            compile(&rules, None, Path::new("/w")).egress_cidrs,
            Some(Vec::new())
        );
    }
}
