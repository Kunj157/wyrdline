-- Judge runs (LLM claim extraction: model, tokens, cost) are kept next
-- to the local ledger, not in the bundle, so the uploader sends them
-- with the bundle and the Hub keeps them with its row.
ALTER TABLE bundles ADD COLUMN judge_runs jsonb NOT NULL DEFAULT '[]'::jsonb;
