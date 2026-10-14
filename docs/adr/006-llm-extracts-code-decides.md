# ADR-006: The LLM extracts claims, code decides verdicts

- Status: Accepted
- Date: 2026-10-14

## Context

Divergence detection compares what an agent claims with what the ledger
shows. Asking an LLM to judge the whole session would be non-deterministic,
costly, hard to evaluate, and itself open to prompt injection from the
transcript.

## Decision

- An LLM is used only to **extract** claims from assistant messages into a
  strict JSON Schema (structured outputs): type, subject, verbatim quote,
  turn.
- Code verifies each quote is a verbatim substring of the referenced turn;
  otherwise the claim is dropped.
- Deterministic rules (D001–D010) decide divergences using ledger evidence;
  every divergence cites event sequence numbers.
- A regex extractor runs first; `provider = none` gives a rules-only mode
  that must still pass every eval that does not need the LLM.
- Prompts and schemas are versioned; token usage and cost are recorded per
  run and capped per session.

## Consequences

- Verdicts are reproducible and testable without a network.
- Recall of unusual claim phrasings depends on the model; measured by the
  claims eval set.
- Prompt injection in the transcript can at worst add or hide a claim, never
  fabricate evidence.

## Alternatives considered

- **LLM-as-judge for the full verdict** — rejected for the reasons above.
- **Rules only** — kept as a supported mode, but misses paraphrased claims.
