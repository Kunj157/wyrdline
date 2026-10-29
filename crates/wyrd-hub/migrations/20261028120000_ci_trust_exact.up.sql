-- CI trust rules name an exact repository by default (F-10, issue #225).
-- Owner-wide rules become an explicit, audited opt-in (`wildcard`), can be
-- switched off (`enabled`), and can be pinned to a git ref (`git_ref`).

ALTER TABLE ci_trust_rules
    ADD COLUMN wildcard boolean NOT NULL DEFAULT false,
    ADD COLUMN enabled  boolean NOT NULL DEFAULT true,
    ADD COLUMN git_ref  text
        CHECK (git_ref IS NULL OR (git_ref LIKE 'refs/%' AND char_length(git_ref) <= 255));

-- Owner-wide rules made before this change trusted every repository of the
-- owner without an opt-in. They are kept as records of that and disabled: an
-- admin deletes one and adds it again with `wildcard: true` to re-confirm.
UPDATE ci_trust_rules SET wildcard = true, enabled = false WHERE repository IS NULL;

ALTER TABLE ci_trust_rules
    ADD CONSTRAINT ci_trust_wildcard_is_owner_wide CHECK ((repository IS NULL) = wildcard);

-- What the disabled rules granted goes too: CI memberships no enabled rule
-- covers, and the upload tokens of users left without a membership.
DELETE FROM memberships m USING users u
 WHERE m.user_id = u.id AND u.subject LIKE 'repo:%'
   AND NOT EXISTS (
     SELECT 1 FROM ci_trust_rules r WHERE r.org_id = m.org_id AND r.enabled
       AND lower(r.repository_owner) = split_part(substr(u.subject, 6), '/', 1)
       AND (r.wildcard OR lower(r.repository) = substr(u.subject, 6)));
DELETE FROM api_tokens t WHERE t.scope = 'upload' AND NOT EXISTS (
    SELECT 1 FROM memberships m WHERE m.org_id = t.org_id AND m.user_id = t.user_id);
