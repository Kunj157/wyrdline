//! Effect-time checks: ledger effects as the policy's `effect` record, and
//! the exec deny list matched by name and by content (a renamed binary is
//! still the same binary).

use std::path::{Path, PathBuf};

use wyrd_schema::EventBody;

use crate::Effect;

/// The policy view of a kernel effect; `None` for effects a policy is not
/// asked about (forks, exits, directory changes).
pub fn effect_of(body: &EventBody) -> Option<Effect> {
    let path = |p: &str| Some(p.to_string());
    Some(match body {
        EventBody::ProcExec(x) => Effect {
            kind: "exec".into(),
            path: path(&x.filename),
            exe: path(&x.filename),
            ..Effect::default()
        },
        EventBody::FileReadSensitive(f) => Effect {
            kind: "file.read_sensitive".into(),
            path: path(&f.path),
            ..Effect::default()
        },
        EventBody::FileOpenWrite(x) => Effect {
            kind: "file.open_write".into(),
            path: path(&x.file.path),
            ..Effect::default()
        },
        EventBody::FileUnlink(x) => Effect {
            kind: "file.unlink".into(),
            path: path(&x.file.path),
            ..Effect::default()
        },
        EventBody::FileRename(x) => Effect {
            kind: "file.rename".into(),
            path: path(&x.to.path),
            ..Effect::default()
        },
        EventBody::NetConnect(c) => Effect {
            kind: "net.connect".into(),
            host: Some(c.dns_name.clone().unwrap_or_else(|| c.addr.clone())),
            port: Some(c.port),
            ..Effect::default()
        },
        _ => return None,
    })
}

/// One program that may not run: its name and, when found on `PATH`, the
/// size and hash of its binary.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Denied {
    name: String,
    binary: Option<(u64, blake3::Hash)>,
}

/// The exec deny list of a session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ExecDeny {
    denied: Vec<Denied>,
}

impl ExecDeny {
    /// Resolve every name on `path_env` (`PATH`) and hash its binary.
    pub fn resolve(names: &[String], path_env: &str) -> Self {
        let denied = names
            .iter()
            .map(|name| {
                let binary = path_env
                    .split(':')
                    .map(|d| Path::new(d).join(name))
                    .find(|p| p.is_file())
                    .and_then(|p| std::fs::read(p).ok())
                    .map(|b| (b.len() as u64, blake3::hash(&b)));
                Denied {
                    name: name.clone(),
                    binary,
                }
            })
            .collect();
        ExecDeny { denied }
    }

    pub fn is_empty(&self) -> bool {
        self.denied.is_empty()
    }

    /// Why executing `exe` is denied, if it is. `open` maps the path the
    /// agent saw to a readable host path (the Cell's overlay); the content
    /// is read only when its size matches a denied binary.
    pub fn check(&self, exe: &str, open: &dyn Fn(&str) -> Option<PathBuf>) -> Option<String> {
        let base = exe.rsplit('/').next().unwrap_or(exe);
        if let Some(d) = self.denied.iter().find(|d| d.name == base) {
            return Some(format!("executed {}, which the policy forbids", d.name));
        }
        let host = open(exe)?;
        let size = std::fs::metadata(&host).ok()?.len();
        if !self
            .denied
            .iter()
            .any(|d| d.binary.is_some_and(|(s, _)| s == size))
        {
            return None;
        }
        let hash = blake3::hash(&std::fs::read(&host).ok()?);
        self.denied
            .iter()
            .find(|d| d.binary.is_some_and(|(s, h)| s == size && h == hash))
            .map(|d| {
                format!(
                    "executed {exe}, a copy of {}, which the policy forbids",
                    d.name
                )
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wyrd_schema::{FilePath, NetConnect, NetVerdict, ProcExec};

    #[test]
    fn effects_map_to_policy_records() {
        let e = effect_of(&EventBody::ProcExec(ProcExec {
            filename: "/usr/bin/curl".into(),
            argv: vec!["curl".into()],
            cwd: None,
            truncated: false,
        }))
        .unwrap();
        assert_eq!(
            (e.kind.as_str(), e.exe.as_deref()),
            ("exec", Some("/usr/bin/curl"))
        );
        let e = effect_of(&EventBody::NetConnect(NetConnect {
            family: 4,
            addr: "203.0.113.9".into(),
            port: 443,
            dns_name: None,
            verdict: NetVerdict::Denied,
        }))
        .unwrap();
        assert_eq!(
            (e.host.as_deref(), e.port),
            (Some("203.0.113.9"), Some(443))
        );
        let e = effect_of(&EventBody::FileReadSensitive(FilePath {
            path: "/w/.env".into(),
            in_cell: true,
            truncated: false,
        }))
        .unwrap();
        assert_eq!(e.kind, "file.read_sensitive");
    }

    #[test]
    fn denied_programs_match_by_name_and_by_content() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("bin");
        std::fs::create_dir(&bin).unwrap();
        std::fs::write(bin.join("curl"), b"pretend curl binary").unwrap();
        std::fs::write(bin.join("ls"), b"pretend ls binary!!").unwrap();
        let deny = ExecDeny::resolve(&["curl".into(), "wget".into()], &bin.display().to_string());
        let host = |p: &str| Some(PathBuf::from(p));
        assert!(deny.check("/usr/bin/curl", &host).is_some());
        assert!(deny.check("wget", &host).is_some());
        // Same bytes under another name: caught by content.
        let copy = dir.path().join("x");
        std::fs::write(&copy, b"pretend curl binary").unwrap();
        let why = deny.check(&copy.display().to_string(), &host).unwrap();
        assert!(why.contains("a copy of curl"), "{why}");
        // Same size, other bytes: allowed.
        assert_eq!(
            deny.check(&bin.join("ls").display().to_string(), &host),
            None
        );
        assert_eq!(deny.check("/nonexistent/tool", &host), None);
    }
}
