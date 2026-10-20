-- Login state, browser sessions, and API tokens. Secrets are stored only
-- as SHA-256 hashes (they are 256-bit random values, so a fast hash is
-- enough).

-- One pending OIDC login (state, nonce, PKCE verifier), bound to the
-- browser that started it.
CREATE TABLE oidc_logins (
    state_hash   text        PRIMARY KEY,
    browser_hash text        NOT NULL,
    nonce        text        NOT NULL,
    pkce_verifier text       NOT NULL,
    return_to    text        NOT NULL,
    expires_at   timestamptz NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE sessions (
    id_hash      text        PRIMARY KEY,
    user_id      text        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    expires_at   timestamptz NOT NULL,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX sessions_user ON sessions (user_id);

CREATE TABLE api_tokens (
    id           text        PRIMARY KEY,
    token_hash   text        NOT NULL UNIQUE,
    user_id      text        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    -- When set, the token acts only in this organisation.
    org_id       text        REFERENCES orgs (id) ON DELETE CASCADE,
    name         text        NOT NULL CHECK (length(name) BETWEEN 1 AND 100),
    expires_at   timestamptz,
    last_used_at timestamptz,
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX api_tokens_user ON api_tokens (user_id);
