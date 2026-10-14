# Redaction

Secrets must never reach the ledger, a bundle, a prompt, or a log
(plan §17.6, trust boundaries B4 and B5). `wyrd_bundle::redact` is the
single implementation used at every exit point.

| Level | Where | What is replaced |
|---|---|---|
| `standard` | ingest, before an event is appended | private-key blocks; AWS, GitHub, Anthropic, OpenAI, Slack, Google, Stripe keys; JWTs; `Authorization`/`Cookie`/API-key headers; URL passwords; `password=`-style assignments; high-entropy tokens (mixed case + digits, ≥ 32 chars, not hex) |
| `strict` | bundle export (Phase 11) | standard + home-directory user names (`/home/alice` → `~`) |
| `off` | tests only | nothing |

A secret becomes `[REDACTED:<kind>:<hash8>]`, where `hash8` is the first
eight hex digits of BLAKE3 of the secret, so two occurrences of the same
secret stay recognisable as the same value without revealing it.
Redaction is idempotent: markers are never redacted again.

In JSON (tool arguments, MCP params), every string is redacted and values
under secret-named keys (`password`, `token`, `api_key`, `authorization`,
…) are replaced whole.

`is_secret_path` classifies paths whose *contents* are never captured
(`.env*` except `.env.example`, SSH/GPG/AWS/gh/kube/docker credentials,
`*.pem`, `*.key`, `*.p12`, `.netrc`, `.npmrc`, `.pypirc`). Reads of such
paths are recorded as `effect.file.read_sensitive` with the path only.

## Tests

`crates/wyrd-bundle/tests/redaction.rs` is the redaction corpus. Every
secret in it is assembled at runtime, so the repository contains no
secret-shaped literal (gitleaks stays clean). It checks each format in
several contexts, JSON nesting, idempotence (property test), and that
ordinary developer text — commit hashes, digests, paths, commands — is
left untouched.
