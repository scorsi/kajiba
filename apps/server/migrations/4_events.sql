CREATE TABLE event (
    id INTEGER PRIMARY KEY,
    public_id TEXT NOT NULL UNIQUE CHECK (length(public_id) = 26 AND length(CAST(public_id AS BLOB)) = 26 AND public_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*' AND substr(public_id, 1, 1) BETWEEN '0' AND '7'),
    kind TEXT NOT NULL,
    version INTEGER NOT NULL,
    occurred_at INTEGER NOT NULL,
    principal_kind TEXT NOT NULL CHECK (principal_kind IN ('user', 'runner', 'system')),
    principal_id INTEGER,
    repo_id INTEGER,
    correlation_id TEXT NOT NULL,
    payload TEXT NOT NULL
);

CREATE INDEX event_repo_id_id ON event (repo_id, id);

CREATE TABLE handler_checkpoint (
    handler TEXT NOT NULL PRIMARY KEY,
    last_event_id INTEGER NOT NULL,
    updated_at INTEGER NOT NULL
);

CREATE TABLE pending_push (
    id INTEGER PRIMARY KEY,
    repo_id INTEGER NOT NULL UNIQUE REFERENCES repo(id) ON DELETE CASCADE,
    principal_id INTEGER NOT NULL REFERENCES user(id),
    correlation_id TEXT NOT NULL,
    updates TEXT NOT NULL,
    created_at INTEGER NOT NULL
);
