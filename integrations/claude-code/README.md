# Claude Code

`wyrd run -- claude …` injects these hooks automatically with
`--settings` (nothing in your user or project settings changes). Use
`settings.json` here only to register the hooks permanently, e.g. by
merging it into `.claude/settings.json`: outside a `wyrd run` session
`wyrd hook` is a silent no-op. `wyrd` must be on `PATH`.

Coverage and decisions: [docs/architecture/adapters.md](../../docs/architecture/adapters.md).
