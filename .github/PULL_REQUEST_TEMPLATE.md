## Summary

<!-- What changed, in one or two sentences. -->

## Reason for change

## How it was tested

<!-- Commands run, kernels tested, scenarios / Playwright flows driven, results. -->

## Screenshots

<!-- UI changes only: 1440 / 1024 / 768 / 390 px. -->

## Security implications

## Migration notes

<!-- Ledger/bundle/event schema version bumps, projection migrations, new config keys, kernel requirement changes. "None" if none. -->

## Rollback procedure

## Definition of done

- [ ] Acceptance criteria from the linked issue are met
- [ ] Tests written first and passing (unit / property / privileged / kernel matrix / scenario / e2e / evals as applicable)
- [ ] Escape suite and redaction corpus still green (capture, Cell, Guard, bundle changes)
- [ ] Authorization and isolation tested (token, socket UID, cgroup ownership, Hub roles)
- [ ] State changes emit ledger events; logs and metrics emitted
- [ ] No console errors, React warnings, or 5xx responses in the test run
- [ ] Local gate green (`DEVELOPMENT_WORKFLOW.md` §5)
- [ ] Docs / README / ADRs updated
- [ ] No secrets, debug output, or untracked TODOs
- [ ] Commits follow Conventional Commits, no co-author trailers
- [ ] Full diff self-reviewed on GitHub

Closes #
