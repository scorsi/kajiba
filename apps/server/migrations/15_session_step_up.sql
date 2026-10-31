-- docs/schema.md, ADR 0143: the instant this session last re-authenticated with one of
-- its user's passkeys; adding or removing a passkey needs it less than five minutes old.
ALTER TABLE session ADD COLUMN step_up_at INTEGER;
