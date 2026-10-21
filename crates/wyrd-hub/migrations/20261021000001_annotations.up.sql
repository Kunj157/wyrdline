-- The annotation document of a bundle (Automerge, saved form), kept by the
-- sync relay at /api/v1/sync; gone with its bundle.
CREATE TABLE bundle_annotations (
    bundle_id  text        PRIMARY KEY REFERENCES bundles (id) ON DELETE CASCADE,
    doc        bytea       NOT NULL,
    updated_at timestamptz NOT NULL DEFAULT now()
);
