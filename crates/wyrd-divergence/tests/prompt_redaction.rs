#![allow(clippy::unwrap_used)]

//! No secret reaches the model: prompts are built from turns and tool
//! arguments with planted secrets (assembled at runtime, so no
//! secret-shaped literal is in the repository) and scanned for them.

use wyrd_divergence::claims::Turn;
use wyrd_divergence::llm::{CallSummary, render_prompt, request};

fn mixed(n: usize) -> String {
    "aB3dE9fG2hJ7kL5mN8pQ1rS4tU6vW0xY"
        .chars()
        .cycle()
        .take(n)
        .collect()
}

fn corpus() -> Vec<String> {
    vec![
        format!("{}{}", "AKIA", "Q".repeat(16)),
        format!("{}{}", "ghp_", mixed(36)),
        format!("{}{}", "sk-ant-api03-", mixed(40)),
        format!("{}{}", "sk-proj-", mixed(40)),
        format!("{}{}", "xoxb-", mixed(24)),
        format!("{}{}", "AIza", mixed(35)),
        format!("eyJ{}.eyJ{}.{}", mixed(12), mixed(20), mixed(30)),
        mixed(40),
    ]
}

#[test]
fn planted_secrets_never_reach_the_prompt() {
    for secret in corpus() {
        let turns = vec![Turn {
            turn: 1,
            seq: 3,
            text: format!("I exported TOKEN={secret} and all tests pass."),
        }];
        let calls = vec![CallSummary {
            tool: "Bash".into(),
            args: format!("{{\"command\":\"curl -H 'Authorization: Bearer {secret}' https://x\"}}"),
        }];
        let body = request("claude-haiku-5-5", &render_prompt(&turns, &calls)).to_string();
        assert!(!body.contains(&secret), "secret leaked into the prompt");
    }
}
