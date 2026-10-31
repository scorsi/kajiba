-- docs/schema.md, ADR 0133: the boolean `ci_enabled` becomes a three-valued CI mode; an
-- enabled repository keeps running on pushes. Startup reconciliation rewrites the column
-- from the configuration anyway (0026).
ALTER TABLE repo ADD COLUMN ci_mode TEXT NOT NULL DEFAULT 'off' CHECK (ci_mode IN ('off', 'manual', 'push'));
UPDATE repo SET ci_mode = 'push' WHERE ci_enabled = 1;
ALTER TABLE repo DROP COLUMN ci_enabled;
