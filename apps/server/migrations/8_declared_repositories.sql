-- docs/schema.md, ADR 0026: a repository's stable declared key and its archival time. The
-- existing UNIQUE(owner_id, name) covers archived rows too, so `repo` is not rebuilt; the
-- code never writes `config_key` empty (the default only satisfies ADD COLUMN).
ALTER TABLE repo ADD COLUMN config_key TEXT NOT NULL DEFAULT '';
ALTER TABLE repo ADD COLUMN archived_at INTEGER;
CREATE UNIQUE INDEX repo_config_key ON repo (config_key);

-- A NULL principal is `system` (admin import). Changing nullability needs a rebuild; it is
-- safe here because no table references `pending_push`, so its DROP cascades nowhere.
CREATE TABLE pending_push_new (
    id INTEGER PRIMARY KEY,
    repo_id INTEGER NOT NULL UNIQUE REFERENCES repo(id) ON DELETE CASCADE,
    principal_id INTEGER REFERENCES user(id),
    correlation_id TEXT NOT NULL,
    updates TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
INSERT INTO pending_push_new SELECT id, repo_id, principal_id, correlation_id, updates, created_at FROM pending_push;
DROP TABLE pending_push;
ALTER TABLE pending_push_new RENAME TO pending_push;
