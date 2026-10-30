-- ADR 0026: a runner removed from the configuration keeps its row, disabled.
-- `hostname` is reported at connection, informational only (0107).
ALTER TABLE runner ADD COLUMN disabled_at INTEGER;
ALTER TABLE runner ADD COLUMN hostname TEXT;
