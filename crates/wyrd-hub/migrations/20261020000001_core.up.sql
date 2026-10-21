-- Hub core schema: organisations, users, memberships, bundles, audit log.
-- Identifiers are ULIDs (text); times are timestamptz.

CREATE TABLE orgs (
    id             text        PRIMARY KEY,
    slug           text        NOT NULL UNIQUE
                               CHECK (slug ~ '^[a-z0-9][a-z0-9-]{0,62}$'),
    name           text        NOT NULL,
    -- Quotas: total stored bundle bytes and bundle count.
    quota_bytes    bigint      NOT NULL DEFAULT 10737418240 CHECK (quota_bytes >= 0),
    quota_bundles  bigint      NOT NULL DEFAULT 10000 CHECK (quota_bundles >= 0),
    -- Bundles older than this many days are deleted; NULL keeps them.
    retention_days integer     CHECK (retention_days IS NULL OR retention_days > 0),
    created_at     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE users (
    id         text        PRIMARY KEY,
    -- The OIDC issuer and subject identify a person; email is display only.
    issuer     text        NOT NULL,
    subject    text        NOT NULL,
    email      text,
    name       text,
    created_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (issuer, subject)
);

CREATE TABLE memberships (
    org_id     text        NOT NULL REFERENCES orgs (id) ON DELETE CASCADE,
    user_id    text        NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    role       text        NOT NULL CHECK (role IN ('viewer', 'member', 'admin')),
    created_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (org_id, user_id)
);
CREATE INDEX memberships_user ON memberships (user_id);

CREATE TABLE bundles (
    id                 text        PRIMARY KEY,
    org_id             text        NOT NULL REFERENCES orgs (id) ON DELETE CASCADE,
    uploaded_by        text        REFERENCES users (id) ON DELETE SET NULL,
    -- From the verified manifest.
    manifest_hash      text        NOT NULL,
    signer             text        NOT NULL,
    session_id         text        NOT NULL,
    session_name       text,
    agent              text        NOT NULL,
    session_created_at timestamptz,
    events             bigint      NOT NULL CHECK (events >= 0),
    -- Supplied by the uploader (the bundle does not name its repository).
    repo               text,
    -- Divergence rule ids raised in the session, e.g. {D001,D003}.
    rules              text[]      NOT NULL DEFAULT '{}',
    size_bytes         bigint      NOT NULL CHECK (size_bytes >= 0),
    storage_key        text        NOT NULL UNIQUE,
    created_at         timestamptz NOT NULL DEFAULT now(),
    UNIQUE (org_id, manifest_hash)
);
CREATE INDEX bundles_org_created ON bundles (org_id, created_at DESC);
CREATE INDEX bundles_org_repo ON bundles (org_id, repo);
CREATE INDEX bundles_rules ON bundles USING gin (rules);

-- Append-only record of every mutation. Not tied to orgs by a foreign key
-- so history outlives what it describes.
CREATE TABLE audit_logs (
    id         bigserial   PRIMARY KEY,
    org_id     text,
    -- A user id, or 'system' for jobs such as retention.
    actor      text        NOT NULL,
    action     text        NOT NULL,
    target     text,
    request_id text,
    details    jsonb       NOT NULL DEFAULT '{}'::jsonb,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX audit_logs_org_time ON audit_logs (org_id, created_at DESC);

CREATE FUNCTION audit_logs_append_only() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'audit_logs is append-only';
END;
$$;

CREATE TRIGGER audit_logs_no_update BEFORE UPDATE OR DELETE ON audit_logs
    FOR EACH ROW EXECUTE FUNCTION audit_logs_append_only();
CREATE TRIGGER audit_logs_no_truncate BEFORE TRUNCATE ON audit_logs
    FOR EACH STATEMENT EXECUTE FUNCTION audit_logs_append_only();
