//! Claims: what the agent said it did, as checkable statements.
//!
//! The regex extractor runs first and covers common phrasings; an LLM
//! extractor can add recall. Every claim's `quote` is a
//! verbatim substring of its turn ([`verify_quote`]); claims that fail the
//! check are dropped, whoever produced them.

use std::sync::LazyLock;

use regex::Regex;
use wyrd_schema::{ClaimExtracted, ClaimType, Event, EventBody, Extractor, Role};

/// One assistant message: its turn, ledger seq, and text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Turn {
    pub turn: u32,
    pub seq: u64,
    pub text: String,
}

/// Assistant messages of a session, in ledger order.
pub fn assistant_turns(events: &[Event]) -> Vec<Turn> {
    events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::Message(m) if m.role == Role::Assistant => Some(Turn {
                turn: m.turn,
                seq: e.seq,
                text: m.text.clone(),
            }),
            _ => None,
        })
        .collect()
}

/// True when `quote` is a non-empty verbatim substring of an assistant
/// message of `turn`.
pub fn verify_quote(turns: &[Turn], turn: u32, quote: &str) -> bool {
    !quote.trim().is_empty()
        && turns
            .iter()
            .any(|t| t.turn == turn && t.text.contains(quote))
}

/// The assistant message a claim was taken from.
pub fn claim_seq(turns: &[Turn], claim: &ClaimExtracted) -> Option<u64> {
    turns
        .iter()
        .find(|t| t.turn == claim.turn && t.text.contains(&claim.quote))
        .map(|t| t.seq)
}

/// Deterministic claim id from its turn, type, and quote.
pub fn claim_id(turn: u32, claim_type: ClaimType, quote: &str) -> String {
    let h = blake3::hash(format!("{turn}\0{claim_type:?}\0{quote}").as_bytes());
    format!("c-{turn}-{}", &hex::encode(h.as_bytes())[..10])
}

/// Sentences of `text` as (start, end) byte ranges, trimmed. Breaks on
/// `.`, `!`, `?` followed by whitespace, on newlines, and on list bullets.
fn sentences(text: &str) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    let bytes = text.as_bytes();
    let mut start = 0;
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        let end_here = c == b'\n'
            || (matches!(c, b'.' | b'!' | b'?')
                && bytes.get(i + 1).is_none_or(u8::is_ascii_whitespace)
                // keep "e.g. " and version numbers together
                && !text[start..i].ends_with("e.g")
                && !text[start..i].ends_with("i.e"));
        if end_here {
            let stop = if c == b'\n' { i } else { i + 1 };
            push_trimmed(text, start, stop, &mut out);
            start = i + 1;
        }
        i += 1;
    }
    push_trimmed(text, start, text.len(), &mut out);
    out
}

fn push_trimmed(text: &str, start: usize, end: usize, out: &mut Vec<(usize, usize)>) {
    let s = &text[start..end];
    let lead = s.len()
        - s.trim_start_matches([' ', '\t', '\r', '-', '*', '•', '>'])
            .len();
    let trail = s.len() - s.trim_end().len();
    if start + lead < end - trail {
        out.push((start + lead, end - trail));
    }
}

macro_rules! re {
    ($s:expr) => {
        LazyLock::new(|| {
            // Constant patterns, compiled once and covered by tests.
            #[allow(clippy::expect_used)]
            Regex::new($s).expect("claim pattern compiles")
        })
    };
}

/// Sentences that are not assertions about what happened.
static NOT_A_CLAIM: LazyLock<Regex> = re!(r"(?ix)
    \b(?:not|never|no\s+longer|still\s+(?:fail\w*|broken|red)|(?:tests?|specs?|build|suite)\s+(?:still\s+)?fail\w*
      |unable|cannot|if|once|until|unless|whether|should|would|might|may|could|will|let's|let\s+me|going\s+to
      |need\s+to|needs\s+to|try|trying|make\s+sure|ensure|verify\s+that|check\s+(?:if|whether|that)|please
      |expect(?:ed)?\s+to|so\s+that|in\s+order\s+to|to\s+confirm|to\s+see\s+if|hopefully|probably|i\s+think|seems?)\b
    |n't\b
    |\?\s*$");

/// Imperative instructions to the user ("Run `pnpm test` to …").
static IMPERATIVE: LazyLock<Regex> =
    re!(r"(?i)^(run|try|use|execute|type|now\s+run|you\s+can|you\s+should|to\s+run)\b");

struct Pattern {
    claim_type: ClaimType,
    re: LazyLock<Regex>,
    /// Capture group holding the subject, if any.
    subject_group: Option<usize>,
    default_subject: &'static str,
}

static PATTERNS: [Pattern; 11] = [
    Pattern {
        claim_type: ClaimType::TestsPass,
        re: re!(r"(?ix)
            \b(?:all\s+(?:the\s+)?(?:\d+\s+)?|the\s+|every\s+|each\s+)?((?:[\w./-]+\s+){0,2}?)(?:unit\s+|e2e\s+|integration\s+)?
              (?:tests?|specs?|test\s+suite|suite|test\s+cases?)\s+(?:now\s+|all\s+|both\s+)?
              (?:pass(?:es|ed)?|(?:are|is)\s+(?:now\s+)?(?:passing|green)|succeed(?:s|ed)?|went\s+green|run\s+green)\b"),
        subject_group: Some(1),
        default_subject: "tests",
    },
    Pattern {
        claim_type: ClaimType::TestsPass,
        re: re!(r"(?ix)
            \b(pytest|vitest|jest|cargo\s+test|go\s+test|pnpm\s+test|npm\s+test|yarn\s+test|mocha|rspec|the\s+test\s+run)\s+
              (?:now\s+)?(?:pass(?:es|ed)?|succeed(?:s|ed)?|(?:is|are)\s+green|reports?\s+(?:all\s+)?(?:\d+\s+)?pass(?:ed|ing)?)\b"),
        subject_group: Some(1),
        default_subject: "tests",
    },
    Pattern {
        claim_type: ClaimType::TestsPass,
        re: re!(
            r"(?ix)\b(\d+\s+(?:of\s+\d+\s+)?tests?\s+pass(?:ed|ing)?|\d+\s+passed(?:,\s+0\s+failed)?|0\s+fail(?:ed|ures))\b"
        ),
        subject_group: None,
        default_subject: "tests",
    },
    Pattern {
        claim_type: ClaimType::BuildOk,
        re: re!(r"(?ix)
            \b(?:the\s+)?((?:[\w./-]+\s+)?)(?:build|compilation|type[-\s]?check|typecheck|project)\s+(?:now\s+)?
              (?:succeeds|succeeded|passes|passed|is\s+(?:now\s+)?(?:green|clean|successful)|works|completes|completed|went\s+through)\b"),
        subject_group: Some(1),
        default_subject: "build",
    },
    Pattern {
        claim_type: ClaimType::BuildOk,
        re: re!(
            r"(?ix)\b(it|everything|the\s+code|the\s+crate|the\s+project|the\s+app)\s+(?:now\s+)?(?:builds|compiles)\b(?:\s+(?:cleanly|successfully|fine|without\s+(?:errors|warnings)))?"
        ),
        subject_group: Some(1),
        default_subject: "build",
    },
    Pattern {
        claim_type: ClaimType::RanCommand,
        re: re!(
            r"(?ix)\b(?:i\s+|i've\s+|i\s+have\s+|i\s+also\s+|and\s+)?(?:ran|executed|re-?ran)\s+`([^`]{1,200})`"
        ),
        subject_group: Some(1),
        default_subject: "command",
    },
    Pattern {
        claim_type: ClaimType::ChangedOnly,
        re: re!(r"(?ix)
            \b(?:i\s+)?(?:only\s+(?:changed|modified|edited|touched|updated)|(?:changed|modified|edited|touched|updated)\s+only)\s+
              (`[^`]{1,200}`|[\w./-]+(?:\s+and\s+[\w./-]+)?)"),
        subject_group: Some(1),
        default_subject: "",
    },
    Pattern {
        claim_type: ClaimType::ChangedOnly,
        re: re!(
            r"(?ix)\bthe\s+only\s+(?:file\s+)?(?:change|file)\s+(?:i\s+made\s+)?(?:is|was)\s+(?:in|to)?\s*(`[^`]{1,200}`|[\w./-]+)"
        ),
        subject_group: Some(1),
        default_subject: "",
    },
    Pattern {
        claim_type: ClaimType::DidNotChange,
        re: re!(r"(?ix)
            \b(?:i\s+)?(?:did\s+not|didn't|have\s+not|haven't|never)\s+(?:change|modify|touch|edit)(?:d)?\s+(?:the\s+|any\s+)?
              (`[^`]{1,200}`|[\w./-]+)|\bleft\s+(`[^`]{1,200}`|[\w./-]+)\s+(?:unchanged|untouched|as\s+is)"),
        subject_group: Some(1),
        default_subject: "",
    },
    Pattern {
        claim_type: ClaimType::NoNetwork,
        re: re!(
            r"(?ix)\b(?:without\s+(?:any\s+)?(?:network|internet)\s+access|no\s+(?:network|internet|external)\s+(?:access|calls|requests|connections)\s+(?:were|was)\s+made|(?:did\s+not|didn't)\s+(?:access|use|touch|call|contact)\s+(?:the\s+)?(?:network|internet|any\s+external))"
        ),
        subject_group: None,
        default_subject: "network",
    },
    Pattern {
        claim_type: ClaimType::FixedIssue,
        re: re!(r"(?ix)
            \b(?:i(?:'ve|\s+have)?\s+)?(?:fixed|resolved|solved|repaired)\s+(?:the\s+|this\s+|that\s+)?((?:[\w./-]+\s+){0,2}?(?:bug|issue|problem|error|failure|regression|test|crash))\b
            |\b(?:the\s+)?((?:[\w./-]+\s+){0,2}?(?:bug|issue|problem|error|failure|regression))\s+(?:is|has\s+been)\s+(?:now\s+)?(?:fixed|resolved|solved)\b"),
        subject_group: Some(1),
        default_subject: "issue",
    },
];

/// Words that disqualify the negation filter: "no failures", "0 failed".
static NEGATION_OK: LazyLock<Regex> = re!(
    r"(?i)\b(no\s+failures|0\s+fail(ed|ures)|without\s+(errors|warnings|failures)|none\s+failed)\b"
);

/// Regex claims of one assistant message.
pub fn extract_regex(turn: &Turn) -> Vec<ClaimExtracted> {
    let mut out: Vec<ClaimExtracted> = Vec::new();
    for (s, e) in sentences(&turn.text) {
        let sentence = &turn.text[s..e];
        if IMPERATIVE.is_match(sentence) {
            continue;
        }
        let negated = {
            let stripped = NEGATION_OK.replace_all(sentence, "");
            NOT_A_CLAIM.is_match(&stripped)
        };
        for p in &PATTERNS {
            // A negated sentence still states what was not done.
            let allowed =
                !negated || matches!(p.claim_type, ClaimType::DidNotChange | ClaimType::NoNetwork);
            if !allowed {
                continue;
            }
            let Some(caps) = p.re.captures(sentence) else {
                continue;
            };
            if out
                .iter()
                .any(|c| c.claim_type == p.claim_type && c.quote == sentence)
            {
                continue;
            }
            let subject = p
                .subject_group
                .and_then(|g| caps.get(g).or_else(|| caps.get(g + 1)))
                .map(|m| {
                    m.as_str()
                        .trim()
                        .trim_end_matches(['.', ',', ';', ':', '!'])
                        .trim_matches('`')
                        .to_string()
                })
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| p.default_subject.to_string());
            out.push(ClaimExtracted {
                claim_id: claim_id(turn.turn, p.claim_type, sentence),
                turn: turn.turn,
                claim_type: p.claim_type,
                subject,
                quote: sentence.to_string(),
                extractor: Extractor::Rule,
            });
        }
    }
    out
}

/// Regex claims of every assistant message of a session.
pub fn extract_all(turns: &[Turn]) -> Vec<ClaimExtracted> {
    turns.iter().flat_map(extract_regex).collect()
}

/// Merge claims from several extractors: drop claims whose quote is not
/// verbatim in their turn, then duplicates (same turn, type, and quote,
/// or an LLM claim whose quote overlaps a rule claim of the same type).
pub fn merge(
    turns: &[Turn],
    claims: impl IntoIterator<Item = ClaimExtracted>,
) -> Vec<ClaimExtracted> {
    let mut out: Vec<ClaimExtracted> = Vec::new();
    for c in claims {
        if !verify_quote(turns, c.turn, &c.quote) {
            continue;
        }
        let dup = out.iter().any(|o| {
            o.turn == c.turn
                && o.claim_type == c.claim_type
                && (o.quote == c.quote || o.quote.contains(&c.quote) || c.quote.contains(&o.quote))
        });
        if !dup {
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claims(text: &str) -> Vec<(ClaimType, String)> {
        extract_regex(&Turn {
            turn: 1,
            seq: 1,
            text: text.into(),
        })
        .into_iter()
        .map(|c| (c.claim_type, c.quote))
        .collect()
    }

    fn types(text: &str) -> Vec<ClaimType> {
        claims(text).into_iter().map(|c| c.0).collect()
    }

    #[test]
    fn tests_pass_phrasings() {
        for t in [
            "All tests pass.",
            "All 42 tests passed.",
            "The cart tests now pass.",
            "Tests are passing.",
            "The test suite is green.",
            "pytest passes.",
            "cargo test succeeds",
            "12 passed, 0 failed",
        ] {
            assert_eq!(types(t), vec![ClaimType::TestsPass], "{t}");
        }
    }

    #[test]
    fn hedges_negations_and_instructions_are_not_claims() {
        for t in [
            "Tests do not pass yet.",
            "The tests still fail on CI.",
            "Once the tests pass, I will commit.",
            "Run `pnpm test` to check that all tests pass.",
            "Do all tests pass?",
            "This should make the tests pass.",
            "Let me check whether the build succeeds.",
            "I couldn't get the build to pass.",
        ] {
            assert!(types(t).is_empty(), "{t}: {:?}", types(t));
        }
    }

    #[test]
    fn quotes_are_verbatim_sentences() {
        let text = "I updated the cart total.\n- All tests pass.\nI ran `pnpm lint` too.";
        let c = claims(text);
        assert_eq!(
            c,
            vec![
                (ClaimType::TestsPass, "All tests pass.".to_string()),
                (ClaimType::RanCommand, "I ran `pnpm lint` too.".to_string()),
            ]
        );
        for (_, q) in &c {
            assert!(text.contains(q.as_str()));
        }
    }

    #[test]
    fn subjects() {
        let t = Turn {
            turn: 2,
            seq: 9,
            text: "I only changed src/cart.ts. I didn't touch `package.json`. I ran `make check`."
                .into(),
        };
        let c = extract_regex(&t);
        let by = |ty| {
            c.iter()
                .find(|c| c.claim_type == ty)
                .unwrap()
                .subject
                .clone()
        };
        assert_eq!(by(ClaimType::ChangedOnly), "src/cart.ts");
        assert_eq!(by(ClaimType::DidNotChange), "package.json");
        assert_eq!(by(ClaimType::RanCommand), "make check");
    }

    #[test]
    fn merge_drops_unverifiable_and_duplicate_claims() {
        let turns = vec![Turn {
            turn: 3,
            seq: 5,
            text: "All tests pass.".into(),
        }];
        let rule = extract_all(&turns);
        let mut llm = rule[0].clone();
        llm.extractor = Extractor::Llm;
        let mut fake = rule[0].clone();
        fake.quote = "All tests pass and the build is green.".into();
        let mut wrong_turn = rule[0].clone();
        wrong_turn.turn = 4;
        let merged = merge(&turns, rule.into_iter().chain([llm, fake, wrong_turn]));
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].extractor, Extractor::Rule);
    }
}
