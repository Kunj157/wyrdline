//! Path globs (`*`, `**`, `?`) compiled to anchored regular expressions.

use regex::Regex;

/// A set of globs matched against workspace-relative paths.
#[derive(Debug, Clone)]
pub struct GlobSet {
    patterns: Vec<String>,
    re: Option<Regex>,
}

impl GlobSet {
    /// Compile `patterns`; an empty list matches nothing.
    pub fn new<S: AsRef<str>>(patterns: &[S]) -> Result<Self, regex::Error> {
        let patterns: Vec<String> = patterns.iter().map(|p| p.as_ref().to_string()).collect();
        let re = if patterns.is_empty() {
            None
        } else {
            let alts: Vec<String> = patterns.iter().map(|p| translate(p)).collect();
            Some(Regex::new(&format!("^(?:{})$", alts.join("|")))?)
        };
        Ok(Self { patterns, re })
    }

    pub fn is_match(&self, path: &str) -> bool {
        self.re.as_ref().is_some_and(|r| r.is_match(path))
    }

    pub fn is_empty(&self) -> bool {
        self.patterns.is_empty()
    }

    pub fn patterns(&self) -> &[String] {
        &self.patterns
    }
}

/// `**/` matches zero or more directories, `**` anything, `*` and `?`
/// stay inside one path component. A pattern without a slash matches
/// the file name in any directory (gitignore style).
fn translate(glob: &str) -> String {
    let glob = glob.trim_start_matches("./");
    let anchored = glob.contains('/');
    let mut out = String::new();
    if !anchored {
        out.push_str("(?:.*/)?");
    }
    let chars: Vec<char> = glob.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            '*' if chars.get(i + 1) == Some(&'*') => {
                if chars.get(i + 2) == Some(&'/') {
                    out.push_str("(?:.*/)?");
                    i += 3;
                } else {
                    out.push_str(".*");
                    i += 2;
                }
            }
            '*' => {
                out.push_str("[^/]*");
                i += 1;
            }
            '?' => {
                out.push_str("[^/]");
                i += 1;
            }
            c => {
                out.push_str(&regex::escape(&c.to_string()));
                i += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::GlobSet;

    fn m(p: &str, path: &str) -> bool {
        GlobSet::new(&[p]).unwrap().is_match(path)
    }

    #[test]
    fn globs() {
        assert!(m("src/**", "src/a/b.ts"));
        assert!(!m("src/**", "srcx/a.ts"));
        assert!(m("**/*.test.ts", "cart.test.ts"));
        assert!(m("**/*.test.ts", "a/b/cart.test.ts"));
        assert!(m("*.test.ts", "a/cart.test.ts"));
        assert!(!m("src/*.ts", "src/a/b.ts"));
        assert!(m(".github/**", ".github/workflows/ci.yml"));
        assert!(m(".env*", ".env.local"));
        assert!(m("node_modules", "a/node_modules"));
        assert!(m("a?c", "abc"));
        assert!(m("/tmp/**", "/tmp/x/y"));
        assert!(!m("/tmp/**", "tmp/x"));
        assert!(!m("a.c", "abc"), "dots are literal");
        assert!(!GlobSet::new::<&str>(&[]).unwrap().is_match("x"));
    }
}
