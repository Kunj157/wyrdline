-- CI upload tokens (issue #161): which GitHub repositories may upload to
-- an organisation, and token scopes.

-- An API token is either a user's full token or an upload-only token
-- minted from a GitHub Actions OIDC token.
ALTER TABLE api_tokens
    ADD COLUMN scope text NOT NULL DEFAULT 'full'
    CHECK (scope IN ('full', 'upload'));

-- Admin-configured trust: workflows of `repository` (owner/name), or of
-- every repository of `repository_owner` when `repository` is NULL, may
-- exchange their OIDC token for an upload token of the organisation.
CREATE TABLE ci_trust_rules (
    id               text        PRIMARY KEY,
    org_id           text        NOT NULL REFERENCES orgs (id) ON DELETE CASCADE,
    repository_owner text        NOT NULL,
    repository       text,
    created_by       text        REFERENCES users (id) ON DELETE SET NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    UNIQUE NULLS NOT DISTINCT (org_id, repository_owner, repository)
);
CREATE INDEX ci_trust_rules_org ON ci_trust_rules (org_id);
