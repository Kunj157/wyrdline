# Development Workflow — Solo Developer (Kunj157)

Solo-developer GitHub workflow, commit/authorship rules, and the headless
iterative testing loop for Wyrdline (Rust core + eBPF sensor + Wasm policies +
React/WebGPU Studio + Hub). Applies to every task in this repo, for humans
and agents alike.

This is a **one-person project**. There is no team, no reviewer, no approver.
Every rule below is written for a single developer who is also their own
reviewer. Follow it on every task, without being asked.

`DEVELOPMENT_PLAN.md` (a local file, not committed) says **what** to build
and in which order. This file says **how** every change gets made. If the two
disagree on process, this file wins; if they disagree on scope, the plan wins.

---

## 0. Non-negotiables (read first)

1. **Never commit directly to `main` or `dev`.** All work happens on a
   `feature/*`, `bugfix/*`, `infra/*`, `docs/*`, `release/*`, or `hotfix/*`
   branch. The only exception is the one-time repository bootstrap commit
   (§1.1).
2. **Never add a co-author.** No `Co-Authored-By:` trailer for Cursor, Claude,
   Copilot, ChatGPT, or any other tool or person. No "Generated with …" footer.
   No AI attribution anywhere in the commit message, PR title, or PR body.
   The sole author of every commit is **Kunj157 `<kunjr157@gmail.com>`**.
3. **Never ship untested behavior.** Every capture path, Cell operation,
   policy decision, API endpoint, and UI flow is driven by an automated test —
   cargo nextest (unit, property, privileged), kernel-matrix VMs, fakeagent
   scenarios, Vitest, Playwright (via the Playwright MCP server while
   iterating) — run headless and iterated on until correct, before the PR is
   opened (§6).
4. **Never commit secrets.** No real values in `.env*` files except
   `.env.example` placeholders. No secret-shaped literals even in tests —
   build them at runtime in the redaction corpus. Gitleaks must be clean.
5. **Never weaken a safety boundary silently.** Anything that touches
   `wyrd-sensord` capabilities, Landlock rules, cgroup BPF programs, the
   escape suite, the redaction corpus, or ledger integrity needs an ADR or an
   explicit note under "Security implications" in the PR.
6. **Ask before**: changing branch/release strategy, adding a heavy dependency
   or new infrastructure component, raising `wyrd-sensord` privileges,
   moving something between in-scope and excluded scope
   (`DEVELOPMENT_PLAN.md` §3), or starting a later phase before the current
   phase's exit criteria are met.

---

## 1. Branch model

| Branch | Purpose | Rules |
|---|---|---|
| `main` | Always stable, releasable | Protected. No direct pushes. Only PRs from `dev`, `release/*`, or `hotfix/*`. Tagged releases (`v0.1.0`, `v0.2.0`, …). |
| `dev` | Integration branch (default) | Latest development code. Only PRs from work branches. Must pass CI. |
| `feature/<short-desc>` | One task / one feature | Branched from `dev`. Merged back into `dev` via PR. |
| `bugfix/<short-desc>` | Non-urgent bug | Same lifecycle as `feature/*`. |
| `infra/<short-desc>` | CI, release pipeline, Helm, Docker, systemd/AppArmor units | Same lifecycle as `feature/*`. |
| `docs/<short-desc>` | Documentation, ADRs, format specs, threat model | Same lifecycle as `feature/*`. |
| `release/<short-desc>` | Pre-release stabilization | Branched from `dev` when `dev` is ready. Only final testing and bug fixes. PR → `main`, then tag `vX.Y.Z`. Example: `release/ledger-and-cell`. |
| `hotfix/<short-desc>` | Urgent fix on a release | Branched from **`main`**. PR → `main` **and** `dev`. Tag a new patch release. |

Branch names are short, lowercase, hyphenated descriptions and **contain no
numbers** — no issue number, no username: `feature/ledger-hash-chain`,
not `feature/12-ledger-hash-chain`. The issue is linked from the PR
(`Closes #<n>`), not from the branch name. Versions are carried by git tags
and GitHub releases (`v0.2.0`), never by release or hotfix branch names.
Never use the word "bot" in a branch name.

### 1.1 Repository bootstrap (one time only)

A brand-new repository has no `dev` to branch from, so the very first commit
(workflow, README, contributing guide, agent instructions, templates,
`.gitignore`, license) goes directly on `main`. Immediately after: create
`dev` from `main`, push both, set `dev` as the default branch, and enable
branch protection (§7). From then on §0.1 applies without exception.

### 1.2 Releases map to plan phases

Tag a minor release when a phase's exit criteria are met, e.g. `v0.1.0` =
Phases 0–1 complete, `v0.2.0` = Phase 2 complete. `v1.0.0` = the full demo
scenario (`DEVELOPMENT_PLAN.md` §2) works end to end.

---

## 2. The loop, every single task

1. **Open a GitHub issue** describing the task, using the plan's Step 1
   "Define" fields (`DEVELOPMENT_PLAN.md` §9): user story, acceptance
   criteria, failure cases, security risks, observability needs. Assign it
   to its phase milestone. One issue = one unit of work.
2. `git checkout dev && git pull origin dev` — always start from the latest
   `dev`.
3. `git checkout -b feature/<short-desc>`.
4. **Design before code** (plan §9 Step 2): API/CLI contract, event schema or
   ledger/bundle format changes (with version bumps), kernel requirements and
   fallback, test plan, rollback.
5. **Test first → implement smallest version → commit** in small,
   single-purpose commits. Never bulk-commit a whole feature at the end.
6. Run the full local gate before pushing (§5). It must be green.
7. Run the **iterative test loop** (§6) for anything touching capture, the
   Cell, policies, the API, the Studio, or the LLM path. It must be green,
   warnings included.
8. `git push -u origin feature/<short-desc>`.
9. **Open a PR into `dev`** using the template in §4. Link the issue with
   `Closes #<n>`.
10. **Self-review the PR diff in full** on GitHub and wait for CI to pass.
    Both are required.
11. Squash-or-merge into `dev` (keep history readable). Delete the branch.
12. When a phase's exit criteria are met → `release/<short-desc>` → full
    system test (plan §23, as far as built) → PR to `main` → merge → tag.
13. For urgent fixes on a release → `hotfix/<short-desc>` from `main` → fix →
    PR to `main` **and** back-merge to `dev` → tag patch release.

---

## 3. Commits and authorship

### 3.1 Authorship — hard rule

Every commit is authored **only** by `Kunj157 <kunjr157@gmail.com>`.
Concretely:

- Commit with the repository's configured identity. Do **not** pass
  `--author`, do **not** set `GIT_AUTHOR_*` / `GIT_COMMITTER_*` overrides,
  and do **not** add any trailer naming a tool or assistant.
- **Forbidden strings** anywhere in a commit message, PR title, or PR body:
  `Co-Authored-By:`, `Co-authored-by:`, `Generated with`, `Claude`,
  `Cursor`, `Copilot`, `ChatGPT`, `🤖`.
  (Filenames such as `CLAUDE.md` may exist in the tree, and the product
  integrates with Claude Code; in commit messages refer to it as "agent
  instructions" or "the claude-code adapter" — use the scope `adapters`
  and the directory name, never the product name in prose.)
- If a commit template or tool tries to append attribution, strip it before
  committing. Verify after every commit:
  ```
  git log -1 --format='%an <%ae>%n%b'
  ```
  The author line must read `Kunj157 <kunjr157@gmail.com>` and the body must
  contain no co-author trailer.

### 3.2 Message format — Conventional Commits

```
<type>(<scope>): <subject>

<body>

<footer>
```

- **Types:** `feat`, `fix`, `docs`, `style`, `refactor`, `test`, `chore`,
  `ci`, `build`, `perf`. (No `infra` type — use `build(infra)`.)
- **Scopes:** `ebpf`, `sensor`, `ledger`, `cas`, `cell`, `adapters`,
  `gateway`, `otlp`, `correlator`, `policy`, `divergence`, `bundle`, `mcp`,
  `daemon`, `cli`, `wasm`, `studio`, `ui`, `hub`, `action`, `evals`,
  `infra`, `helm`, `ci`, `release`, `docs`, `deps`.
- **Subject:** imperative, lowercase, no trailing period, ≤ 72 chars.
- **Body:** what changed **and why**. Wrap at 72 columns. Not optional for
  anything non-trivial.
- **Footer:** `Closes #12`, `Refs #9`, `BREAKING CHANGE: …` (any ledger,
  bundle, event-schema, or WIT incompatibility is a breaking change).

Examples:

```
feat(ledger): add blake3 hash chain with signed session head

Every event row now carries the hash of its predecessor so any
modification is detectable at the exact sequence number. The head is
signed with the host ed25519 key when the session closes, and an
SQLite trigger rejects UPDATE and DELETE on the events table.

Closes #14
```

```
fix(cell): restore overlay whiteouts when rewinding deletes
```

One logical change per commit. If the subject needs an "and", split it.

---

## 4. Self-review (replaces team code review)

There is no second reviewer, so the discipline has to come from the process.

- **Small PRs.** One issue = one PR. If the diff exceeds ~400 changed lines
  (excluding lockfiles, generated schemas/clients, and test fixtures), split
  it.
- **Link the issue.** Every PR closes or references one.
- **Read the entire diff on GitHub** before merging — not in the editor.
- **PR description template** (`.github/PULL_REQUEST_TEMPLATE.md`):
  ```
  ## Summary
  ## Reason for change
  ## How it was tested        (commands run, kernels, scenarios, flows, results)
  ## Screenshots              (Studio changes)
  ## Security implications
  ## Migration notes          (format/schema version bumps, config keys, kernel reqs)
  ## Rollback procedure
  ## Definition of done       (checklist)
  Closes #<n>
  ```
- **Self-review checklist, every PR:**
  - [ ] No debug prints, `dbg!`, `console.log`, commented-out code, or `TODO`
        without an issue number.
  - [ ] No `unwrap()`/`expect()` on untrusted input paths; no new `unsafe`
        outside `wyrd-ebpf`/`wyrd-sensor` without a `// SAFETY:` comment.
  - [ ] No secrets, tokens, absolute local paths, or real transcripts.
  - [ ] Tests added/updated first, and passing, for the new behavior.
  - [ ] Isolation tested: socket UID, token auth, cgroup ownership, Hub roles
        and cross-org access, as applicable.
  - [ ] State changes write ledger events; logs and metrics emitted.
  - [ ] Capture/Cell/Guard changes: escape suite + kernel matrix green.
  - [ ] Anything that stores or exports data: redaction corpus green.
  - [ ] Format changes (ledger, CAS manifest, bundle, event schema, WIT):
        version bumped, spec in `docs/` updated, old-version read test added.
  - [ ] Studio flows covered by a committed Playwright spec or Vitest test,
        green headless (§6); WebGPU and fallback paths both exercised.
  - [ ] No browser console errors, React warnings, or hydration warnings.
  - [ ] LLM changes: structured output validated, quotes verified, eval cases
        added (`tests/evals/`), prompt version bumped.
  - [ ] Docs / README / ADR updated if behavior or setup changed.
  - [ ] No dead code, no duplication introduced.
  - [ ] Commit messages follow §3, no AI co-author trailers.
- **Never merge red CI.** Never merge with "will fix in the next PR".

---

## 5. Local gate before every push

Run from the repo root, in this order; all must pass. The `make` targets are
defined in the root `Makefile` (plan §10.3); until a target exists, run the
underlying command directly and add the target in the same PR. Always
`. ~/kunj-personal/env.sh` first.

1. **Format** — `make fmt` (cargo fmt, prettier) → no diff left behind.
2. **Lint** — `make lint` (clippy `-D warnings`, eslint) → zero warnings.
3. **Type check** — `make typecheck` (`cargo check --all-targets`,
   `tsc --noEmit`).
4. **Unit + property tests** — `make test` (cargo nextest, Vitest).
5. **Privileged integration** — `make test-integration` (sensor + Cell with
   sudo; Testcontainers for the Hub). Must **fail**, not skip, when
   capabilities or Docker are missing.
6. **Kernel matrix** — `make test-kernels` for anything touching
   `wyrd-ebpf`, `wyrd-sensor`, `wyrd-cell`, or enforcement.
7. **Scenarios** — `make test-scenarios` (fakeagent full pipeline).
8. **E2E** — `make test-e2e` (Playwright, headless) for anything touching
   the Studio or the daemon API.
9. **Evals** — `make test-evals` for anything touching correlation,
   divergence rules, claim extraction, or prompts.
10. **Build** — `make build` (release crates, eBPF object, Wasm, policies,
    Studio).
11. **Supply chain + secrets** — `make deny` and `make secrets`.

CI (`.github/workflows/`) enforces the same gate plus Semgrep, fuzz smoke,
Trivy, actionlint, and Helm lint (plan §20). Local-first is not optional: CI
is the backstop, not the first run.

---

## 6. Testing — driven, iterative, headless

### 6.1 Pick the right driver

| Surface | Driver |
|---|---|
| Pure logic (correlator, rules, policy compiler, redaction, manifests, hashing) | `cargo nextest` unit + `proptest`, no I/O, no root |
| Untrusted parsers (bundle, CBOR, hook payloads, MCP, OTLP) | `cargo-fuzz` targets + seed corpora |
| Ledger / CAS | proptest tamper + crash-safety tests |
| eBPF sensor | privileged golden tests on the dev box + `vmtest` kernel matrix |
| Cell (namespaces, overlay, Landlock, checkpoints) | privileged integration + randomised lifecycle state-machine test |
| Guard / enforcement | escape suite (`tests/security/escape/`) + Wasm guest conformance |
| Full pipeline | `wyrd-fakeagent` scenarios (`tests/scenarios/`) |
| Daemon API | `reqwest`/`hyper` against the real Axum app; contract check against OpenAPI |
| Studio components | Vitest + Testing Library |
| Studio flows | **Playwright MCP server** while iterating → committed spec in `tests/e2e/` |
| Causal River | Playwright: selection, keyboard, WebGPU off → Canvas2D → table, frame-time budget |
| LLM claim extraction | recorded-response tests; live runs with `EVAL_LIVE=1` before releases |
| Hub | Testcontainers Postgres + MinIO; authz matrix; two-context Playwright sync test |
| Performance | `criterion`, `hyperfine` overhead runs in `tests/perf/` |
| Packaging | `helm lint`, kubeconform, kind install, release smoke install |

### 6.2 Design for testability first

Most bugs should be catchable without root, a kernel, a browser, or an LLM.
Keep logic in pure Rust functions (the correlator is a pure function of the
event list — that is why it can run in Wasm); keep eBPF programs, the Axum
handlers, React components, and the LLM client thin. Drive the pipeline with
`wyrd-fakeagent` instead of a real agent wherever possible. Then the bulk of
coverage is fast unit/property tests, and privileged/kernel/browser/LLM tests
only cover genuine integration behavior.

### 6.3 The iterative loop

Do not stop at "it built" or "the program loaded". Loop until the behavior is
right:

1. **Write the failing test first** for the next smallest behavior (red →
   green → refactor).
2. **Run it headless** — `make test` / `make test-integration` /
   `make test-scenarios` / `pnpm --filter studio exec playwright test`.
3. **Drive the real flow** — run `wyrd run` with the fakeagent *and* with a
   real agent for capture changes; for Studio, use the Playwright MCP server:
   navigate, snapshot the accessibility tree, click, type, rewind, fork. Not
   just the happy path: also the capability-missing path
   (`WYRD_DOCTOR_MASK`), sensor/daemon restart, disk full, malicious input,
   SSE disconnect.
4. **Assert on observable behavior** — ledger events and hashes, file-tree
   hashes after rewind, errno values, exit codes, HTTP status and body,
   rendered accessible text — not implementation details. No sleeps; poll
   with deadlines.
5. **Watch the output** — daemon logs, sensor drop counters, kernel verifier
   log, browser console, network tab. A test that passes while dropping events,
   leaking mounts/cgroups, or emitting React warnings is not green.
6. **Look at it** for anything visual — screenshot at 1440 / 1024 / 768 /
   390 px (plan §16.5). If it diverges from intent → fix → back to step 2.
7. Once a flow is verified via MCP, **commit it as a Playwright spec** so CI
   protects it.
8. **Record the outcome** in the PR description: kernels and scenarios run,
   flows exercised, screenshots, and anything deliberately left untested.

Report the loop honestly. If a step failed or was skipped, say so — never
describe an untested change as verified.

### 6.4 What must exist before a PR merges

- **Unit/property tests** for every new behavior, written before the
  implementation.
- **Isolation/authz tests** for every new or changed endpoint or socket.
- **Privileged + kernel-matrix tests** for every capture, Cell, or
  enforcement change.
- **Scenario tests** for every cross-component flow (capture → ledger →
  correlate → checkpoint → guard → divergence → bundle).
- **Fuzz targets** for every new parser of untrusted input.
- **Playwright specs** for every new or changed Studio flow, committed to
  `tests/e2e/`.
- **Eval cases** for every new rule, claim type, or prompt version.
- **Visual snapshots** only for key screens — keep them few.
- All of it wired into `make` targets so CI runs it headless on every PR.
- Coverage of pure-logic and security-critical crates must not drop.

---

## 7. Branch protection (configure once on GitHub)

- Protect `main` and `dev`.
- Require status checks to pass before merging (add each CI job as a
  required check once it exists).
- Require branches to be up to date before merging.
- Restrict direct pushes; changes land only via PR.
- Solo exception: required approvals = `0`, since you cannot approve your
  own PR — the §4 self-review checklist substitutes for it. Everything else
  stays on.

---

## 8. Project structure

Follow `DEVELOPMENT_PLAN.md` §7 exactly. Do not create top-level directories
or crates that are not listed there without asking (§0.6). `docs/` is
committed — architecture, ADRs, format specs, threat model, and results are
deliverables.

---

## 9. General practices

- Pull the latest `dev` before starting any work.
- Track everything in GitHub Issues — a task without an issue does not exist.
- One GitHub milestone per plan phase; a GitHub Projects board shows what is
  in flight. As a solo dev, keep **at most one** item in "In Progress".
- Work the plan in order (§34 starting checklist, §35 core rule). Do not
  start excluded-scope work (§3.2).
- Update docs, README, format specs, and ADRs in the same PR as the behavior
  change.
- Keep dependencies current by updating them yourself on an `infra/*` or
  `feature/*` branch. No automated bot pull requests or bot branches.
- Pin GitHub Actions by commit SHA with the version in a comment.
- Tag every release. Write release notes from the merged PR titles.
- Clean code, clear commits, honest status reports. Performance, accuracy,
  and resume numbers come only from committed reports of real runs.

---

## 10. Agent conduct in this repo

Any coding agent working in this repository:

- Reads `DEVELOPMENT_PLAN.md` and this file before starting.
- Sources `~/kunj-personal/env.sh` and uses `ghk` (never plain `gh`, which is
  a different account) for GitHub operations on `Kunj157/wyrdline`.
- Follows this workflow end to end without being reminded.
- Never commits or pushes unless explicitly asked; and when asked, obeys
  §3.1 authorship rules exactly and verifies with `git log -1`.
- Never opens a PR to `main` from a work branch.
- Never runs privileged commands (sudo, sensord install, AppArmor changes)
  without saying so first.
- Never claims a change is tested unless it ran the §6 loop; if a step was
  skipped, says which one and why.
- Surfaces a rule conflict instead of silently working around it.
