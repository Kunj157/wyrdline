DELETE FROM api_tokens WHERE scope = 'upload';
DROP TABLE ci_trust_rules;
ALTER TABLE api_tokens DROP COLUMN scope;
