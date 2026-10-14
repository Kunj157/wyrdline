# Codex CLI

Codex hooks use the same payload and decision format as Claude Code's.
Register `wyrd hook codex` for `PreToolUse`, `PostToolUse`,
`UserPromptSubmit`, and `Stop` in Codex's hook configuration. `wyrd run`
does not do this for you because Codex reads hooks only from user or
project configuration, which Wyrdline never modifies.

Status: field names checked against Codex 0.160; not yet verified in a
live hook run. See [docs/architecture/adapters.md](../../docs/architecture/adapters.md).
