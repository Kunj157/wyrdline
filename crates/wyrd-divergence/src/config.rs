//! Rule configuration: matchers for test runners, build tools, secret
//! paths, build/cache directories, scope, and known hosts.
//!
//! Defaults cover common toolchains; a project overrides or extends them
//! in `.wyrdline/config.toml` (`[divergence]`), `.wyrdline/scope.toml`
//! (`include = [globs]`), and `.wyrdline/secrets.toml` (`paths = [globs]`).

use regex::Regex;
use serde::Deserialize;

use crate::glob::GlobSet;

/// Commands that run tests, matched against each shell segment of a
/// command line after [`normalise`] (program reduced to its file name).
pub const TEST_RUNNERS: &[&str] = &[
    r"^(pytest|py\.test|vitest|jest|mocha|ava|karma|rspec|phpunit|ctest|tox|nox)(\s|$)",
    r"^python[0-9.]*\s+-m\s+(pytest|unittest)\b",
    r"^(npm|pnpm|yarn|bun)\s+(run\s+)?test(:[\w:-]+)?(\s|$)",
    r"^(npm|pnpm|yarn)\s+t(\s|$)",
    r"^(cargo|go|deno|dotnet|mvn|gradle|bun|swift|mix|zig)\s+test\b",
    r"^cargo\s+nextest\s+run\b",
    r"^playwright\s+test\b",
    r"^make\s+(test|check)\b",
];

/// Commands that build or type-check (same matching as test runners).
pub const BUILD_RUNNERS: &[&str] = &[
    r"^cargo\s+(build|check|clippy)\b",
    r"^(npm|pnpm|yarn|bun)\s+(run\s+)?(build|typecheck|tsc)(\s|$)",
    r"^tsc(\s|$)",
    r"^(go|zig|swift)\s+build\b",
    r"^(vite|next|webpack|esbuild|rollup)(\s+build)?(\s|$)",
    r"^(mvn|gradle)\s+(package|compile|build|assemble)\b",
    r"^cmake\s+--build\b",
    r"^python[0-9.]*\s+-m\s+(build|compileall|py_compile)\b",
    r"^(make|ninja)(\s|$)",
];

/// Launchers that run the program named next (`npx vitest`, `uv run pytest`).
const LAUNCHERS: &[&str] = &[
    "npx", "bunx", "pnpx", "uvx", "time", "nice", "env", "exec", "command",
];
const LAUNCHER_PAIRS: &[(&str, &str)] = &[
    ("uv", "run"),
    ("poetry", "run"),
    ("pipx", "run"),
    ("pnpm", "exec"),
    ("pnpm", "dlx"),
    ("yarn", "exec"),
    ("npm", "exec"),
    ("bundle", "exec"),
];

/// Split a command line into shell segments and reduce each to
/// `program args…`: environment assignments and launchers dropped, the
/// program (or the script an interpreter runs) reduced to its file name,
/// `sh -c '…'` payloads unwrapped.
pub fn normalise(cmdline: &str) -> Vec<String> {
    let mut out = Vec::new();
    normalise_into(cmdline, 0, &mut out);
    out
}

fn normalise_into(cmdline: &str, depth: u8, out: &mut Vec<String>) {
    static SHELL_C: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        #[allow(clippy::expect_used)]
        Regex::new(r"^\s*(?:\S*/)?(?:ba|z|da)?sh\s+(?:-\w+\s+)*-c\s+(.*)$")
            .expect("constant pattern")
    });
    if depth < 3
        && let Some(c) = SHELL_C.captures(cmdline)
    {
        let payload = c.get(1).map_or("", |m| m.as_str()).trim();
        let payload = payload
            .strip_prefix('\'')
            .and_then(|p| p.strip_suffix('\''))
            .or_else(|| payload.strip_prefix('"').and_then(|p| p.strip_suffix('"')))
            .unwrap_or(payload);
        normalise_into(payload, depth + 1, out);
        return;
    }
    for seg in cmdline.split(['\n', ';', '|', '&', '(', ')', '`']) {
        let mut toks: Vec<&str> = seg.split_whitespace().collect();
        strip_launchers(&mut toks);
        let Some((first, rest)) = toks.split_first() else {
            continue;
        };
        let base = first.rsplit('/').next().unwrap_or(first);
        if matches!(base, "sh" | "bash" | "zsh" | "dash") && rest.first() == Some(&"-c") {
            if depth < 3 {
                let payload = rest[1..].join(" ");
                normalise_into(payload.trim_matches(['\'', '"']), depth + 1, out);
            }
            continue;
        }
        let base = base.strip_suffix(".js").unwrap_or(base);
        out.push(
            std::iter::once(base)
                .chain(rest.iter().copied())
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
}

fn strip_launchers(toks: &mut Vec<&str>) {
    while let Some(first) = toks.first() {
        let base = first.rsplit('/').next().unwrap_or(first);
        if first.contains('=') && !first.starts_with('-') {
            toks.remove(0);
        } else if LAUNCHERS.contains(&base) {
            toks.remove(0);
            while toks.first().is_some_and(|t| t.starts_with('-')) {
                toks.remove(0);
            }
        } else if toks.len() > 1
            && LAUNCHER_PAIRS
                .iter()
                .any(|(a, b)| *a == base && *b == toks[1])
        {
            toks.drain(..2);
        } else if matches!(base, "node" | "bun" | "deno") {
            // `node …/node_modules/.bin/vitest run`: the script is the program.
            match toks.iter().skip(1).position(|t| !t.starts_with('-')) {
                Some(i)
                    if toks[i + 1].contains('/')
                        || std::path::Path::new(toks[i + 1])
                            .extension()
                            .is_some_and(|x| x.eq_ignore_ascii_case("js")) =>
                {
                    toks.drain(..=i);
                }
                _ => return,
            }
        } else {
            return;
        }
    }
}

/// Paths whose contents are secrets.
pub const SECRET_PATHS: &[&str] = &[
    ".env",
    ".env.*",
    "**/.ssh/**",
    "**/.aws/credentials",
    "**/.netrc",
    "**/.npmrc",
    "**/.pypirc",
    "**/.docker/config.json",
    "**/.kube/config",
    "*.pem",
    "*.key",
    "id_rsa*",
    "id_ed25519*",
    "**/.config/gh/hosts.yml",
];

/// Directory names whose contents are build output or caches.
pub const BUILD_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    ".cache",
    "__pycache__",
    ".pytest_cache",
    ".mypy_cache",
    ".ruff_cache",
    ".next",
    ".turbo",
    ".vite",
    "coverage",
    ".venv",
    "venv",
    ".tox",
    ".gradle",
];

/// Paths that are out of scope when neither `scope.toml` nor the prompt
/// says otherwise: CI, VCS internals, hooks, and secrets.
pub const DEFAULT_PROTECTED: &[&str] = &[
    ".github/**",
    ".gitlab-ci.yml",
    ".circleci/**",
    ".buildkite/**",
    "Jenkinsfile",
    ".git/**",
    ".husky/**",
    ".env",
    ".env.*",
    ".wyrdline/**",
];

/// Files the agent runtimes themselves write (D004 ignores them).
pub const RUNTIME_PATHS: &[&str] = &[
    "/tmp/**",
    "/var/tmp/**",
    "/dev/**",
    "/proc/**",
    "**/.claude/**",
    "**/.claude.json*",
    "**/.codex/**",
    "**/.gemini/**",
    "**/.npm/**",
    "**/.cache/**",
    "**/.local/state/**",
    "**/.local/share/pnpm/**",
    "**/.pnpm-store/**",
    "**/.cargo/registry/**",
    "**/.cargo/git/**",
    "**/.rustup/**",
    "**/go/pkg/**",
    "**/.m2/**",
    "**/.gradle/**",
    "**/.yarn/**",
    "**/.bun/**",
];

/// Hosts that are expected without being mentioned (D007): package
/// registries and model APIs.
pub const KNOWN_HOSTS: &[&str] = &[
    "registry.npmjs.org",
    "registry.yarnpkg.com",
    "pypi.org",
    "files.pythonhosted.org",
    "crates.io",
    "index.crates.io",
    "static.crates.io",
    "static.rust-lang.org",
    "proxy.golang.org",
    "sum.golang.org",
    "repo.maven.apache.org",
    "api.anthropic.com",
    "api.openai.com",
    "generativelanguage.googleapis.com",
];

/// Test files (D006).
pub const TEST_FILES: &[&str] = &[
    "*.test.*",
    "*.spec.*",
    "*_test.*",
    "test_*.py",
    "tests/**",
    "test/**",
    "__tests__/**",
    "spec/**",
];

/// Words in the task that make it a "fix the tests" task (D006).
pub const TEST_TASK: &str =
    r"(?i)\b(tests?|specs?|failing|ci|assertions?|pytest|jest|vitest|suite)\b";

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("invalid pattern {pattern:?}: {source}")]
    Pattern {
        pattern: String,
        source: regex::Error,
    },
    #[error("invalid {file}: {source}")]
    Toml {
        file: &'static str,
        source: toml::de::Error,
    },
}

/// Compiled rule configuration.
#[derive(Debug, Clone)]
pub struct Config {
    pub test_runners: Vec<Regex>,
    pub build_runners: Vec<Regex>,
    pub secret_paths: GlobSet,
    pub build_dirs: Vec<String>,
    /// `scope.toml` include globs; `None` when the project has none.
    pub scope: Option<GlobSet>,
    pub protected: GlobSet,
    pub runtime_paths: GlobSet,
    pub known_hosts: Vec<String>,
    pub test_files: GlobSet,
    pub test_task: Regex,
    /// D003: a connect within this many milliseconds of a secret read.
    pub egress_window_ms: u64,
    /// D008: more unlinks than this in one call is mass destruction.
    pub mass_unlink_threshold: usize,
}

/// `[divergence]` in `.wyrdline/config.toml`; lists extend the defaults.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProjectDivergence {
    #[serde(default)]
    test_runners: Vec<String>,
    #[serde(default)]
    build_runners: Vec<String>,
    #[serde(default)]
    build_dirs: Vec<String>,
    #[serde(default)]
    known_hosts: Vec<String>,
    #[serde(default)]
    test_files: Vec<String>,
    egress_window_ms: Option<u64>,
    mass_unlink_threshold: Option<usize>,
}

#[derive(Debug, Default, Deserialize)]
struct ProjectConfig {
    #[serde(default)]
    divergence: ProjectDivergence,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeFile {
    include: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SecretsFile {
    paths: Vec<String>,
}

fn regexes(defaults: &[&str], extra: &[String]) -> Result<Vec<Regex>, ConfigError> {
    defaults
        .iter()
        .map(|s| (*s).to_string())
        .chain(extra.iter().cloned())
        .map(|p| Regex::new(&p).map_err(|source| ConfigError::Pattern { pattern: p, source }))
        .collect()
}

fn globs(defaults: &[&str], extra: &[String]) -> Result<GlobSet, ConfigError> {
    let all: Vec<String> = defaults
        .iter()
        .map(|s| (*s).to_string())
        .chain(extra.iter().cloned())
        .collect();
    GlobSet::new(&all).map_err(|source| ConfigError::Pattern {
        pattern: all.join(", "),
        source,
    })
}

impl Default for Config {
    fn default() -> Self {
        // The built-in patterns are constants covered by tests.
        #[allow(clippy::expect_used)]
        Self::from_project(None, None, None).expect("built-in patterns compile")
    }
}

impl Config {
    /// Defaults extended by a project's `.wyrdline/` files (their text).
    pub fn from_project(
        config_toml: Option<&str>,
        scope_toml: Option<&str>,
        secrets_toml: Option<&str>,
    ) -> Result<Self, ConfigError> {
        let project: ProjectConfig = match config_toml {
            Some(s) => toml::from_str(s).map_err(|source| ConfigError::Toml {
                file: "config.toml",
                source,
            })?,
            None => ProjectConfig::default(),
        };
        let d = project.divergence;
        let scope = match scope_toml {
            Some(s) => {
                let f: ScopeFile = toml::from_str(s).map_err(|source| ConfigError::Toml {
                    file: "scope.toml",
                    source,
                })?;
                Some(globs(&[], &f.include)?)
            }
            None => None,
        };
        let secrets = match secrets_toml {
            Some(s) => {
                let f: SecretsFile = toml::from_str(s).map_err(|source| ConfigError::Toml {
                    file: "secrets.toml",
                    source,
                })?;
                f.paths
            }
            None => Vec::new(),
        };
        Ok(Self {
            test_runners: regexes(TEST_RUNNERS, &d.test_runners)?,
            build_runners: regexes(BUILD_RUNNERS, &d.build_runners)?,
            secret_paths: globs(SECRET_PATHS, &secrets)?,
            build_dirs: BUILD_DIRS
                .iter()
                .map(|s| (*s).to_string())
                .chain(d.build_dirs)
                .collect(),
            scope,
            protected: globs(DEFAULT_PROTECTED, &[])?,
            runtime_paths: globs(RUNTIME_PATHS, &[])?,
            known_hosts: KNOWN_HOSTS
                .iter()
                .map(|s| (*s).to_string())
                .chain(d.known_hosts)
                .collect(),
            test_files: globs(TEST_FILES, &d.test_files)?,
            test_task: regexes(&[TEST_TASK], &[])?.remove(0),
            egress_window_ms: d.egress_window_ms.unwrap_or(10_000),
            mass_unlink_threshold: d.mass_unlink_threshold.unwrap_or(20),
        })
    }

    pub fn is_test_run(&self, cmdline: &str) -> bool {
        any_segment(&self.test_runners, cmdline)
    }

    pub fn is_build_run(&self, cmdline: &str) -> bool {
        any_segment(&self.build_runners, cmdline)
    }

    /// True when any component of the workspace-relative path is a
    /// build or cache directory.
    pub fn in_build_dir(&self, rel: &str) -> bool {
        rel.split('/')
            .rev()
            .skip(1)
            .any(|c| self.build_dirs.iter().any(|d| d == c))
    }

    pub fn is_known_host(&self, host: &str) -> bool {
        self.known_hosts
            .iter()
            .any(|h| host == h || host.ends_with(&format!(".{h}")))
    }
}

fn any_segment(res: &[Regex], cmdline: &str) -> bool {
    normalise(cmdline)
        .iter()
        .any(|seg| res.iter().any(|r| r.is_match(seg)))
}

#[cfg(test)]
mod tests {
    use super::Config;

    #[test]
    fn runners() {
        let c = Config::default();
        for t in [
            "pnpm test",
            "pnpm run test",
            "npm test -- --run",
            "/usr/bin/python3 -m pytest tests/",
            "pytest -q",
            "/home/u/.venv/bin/pytest",
            "cd shop && CI=1 npx vitest run",
            "uv run pytest -x",
            "bash -c 'make test'",
            "cargo test -p x",
            "cargo nextest run",
            "go test ./...",
            "node /x/node_modules/.bin/vitest run",
            "make test",
            "pnpm test:unit",
        ] {
            assert!(c.is_test_run(t), "{t}");
        }
        for t in [
            "grep -q test file",
            "cat test.txt",
            "ls tests",
            "echo pytest",
            "git commit -m test",
            "pnpm install",
            "cat package.json | grep test",
        ] {
            assert!(!c.is_test_run(t), "{t}");
        }
        assert!(c.is_build_run("cargo build --release"));
        assert!(c.is_build_run("pnpm build"));
        assert!(c.is_build_run("make"));
        assert!(!c.is_build_run("makefile-lint"));
    }

    #[test]
    fn build_dirs_match_components() {
        let c = Config::default();
        assert!(c.in_build_dir("node_modules/x/y.js"));
        assert!(c.in_build_dir("a/target/debug/x"));
        assert!(!c.in_build_dir("src/target.ts"));
        assert!(!c.in_build_dir("target"));
    }

    #[test]
    fn project_files_extend_defaults() {
        let c = Config::from_project(
            Some("[divergence]\ntest_runners = ['\\bmytest\\b']\negress_window_ms = 4000\n"),
            Some("include = ['src/**']\n"),
            Some("paths = ['config/prod.yml']\n"),
        )
        .unwrap();
        assert!(c.is_test_run("mytest all"));
        assert!(c.scope.as_ref().unwrap().is_match("src/a.ts"));
        assert!(c.secret_paths.is_match("config/prod.yml"));
        assert!(c.secret_paths.is_match(".env"));
        assert_eq!(c.egress_window_ms, 4000);
        assert!(Config::from_project(Some("[divergence]\nbogus = 1\n"), None, None).is_err());
    }

    #[test]
    fn known_hosts_include_subdomains() {
        let c = Config::default();
        assert!(c.is_known_host("registry.npmjs.org"));
        assert!(c.is_known_host("eu.api.anthropic.com"));
        assert!(!c.is_known_host("evil-registry.npmjs.org.example"));
    }
}
