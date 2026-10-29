-- Back to the schema before the opt-in. Owner-wide rules, disabled rules and
-- ref-pinned rules cannot be represented there (the old schema would trust
-- them as exact or owner-wide rules), so they are removed with the access
-- they granted.
DELETE FROM ci_trust_rules WHERE wildcard OR NOT enabled OR git_ref IS NOT NULL;
DELETE FROM memberships m USING users u
 WHERE m.user_id = u.id AND u.subject LIKE 'repo:%'
   AND NOT EXISTS (
     SELECT 1 FROM ci_trust_rules r WHERE r.org_id = m.org_id
       AND lower(r.repository) = substr(u.subject, 6));
DELETE FROM api_tokens t WHERE t.scope = 'upload' AND NOT EXISTS (
    SELECT 1 FROM memberships m WHERE m.org_id = t.org_id AND m.user_id = t.user_id);
ALTER TABLE ci_trust_rules DROP CONSTRAINT ci_trust_wildcard_is_owner_wide;
ALTER TABLE ci_trust_rules DROP COLUMN git_ref, DROP COLUMN enabled, DROP COLUMN wildcard;
