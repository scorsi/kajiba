CREATE TABLE repo (
    id INTEGER PRIMARY KEY,
    public_id TEXT NOT NULL UNIQUE CHECK (length(public_id) = 26 AND length(CAST(public_id AS BLOB)) = 26 AND public_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*' AND substr(public_id, 1, 1) BETWEEN '0' AND '7'),
    owner_id INTEGER NOT NULL REFERENCES user(id) ON DELETE RESTRICT,
    name TEXT NOT NULL CHECK (name = lower(name)),
    visibility TEXT NOT NULL CHECK (visibility IN ('public')),
    ci_enabled INTEGER NOT NULL CHECK (ci_enabled IN (0, 1)),
    default_branch TEXT NOT NULL,
    description TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    UNIQUE (owner_id, name)
);

CREATE TABLE repo_redirect (
    id INTEGER PRIMARY KEY,
    owner_name TEXT NOT NULL CHECK (owner_name = lower(owner_name)),
    name TEXT NOT NULL CHECK (name = lower(name)),
    repo_id INTEGER NOT NULL REFERENCES repo(id) ON DELETE CASCADE,
    created_at INTEGER NOT NULL,
    UNIQUE (owner_name, name)
);

CREATE TABLE repo_permission (
    user_id INTEGER NOT NULL REFERENCES user(id) ON DELETE CASCADE,
    repo_id INTEGER NOT NULL REFERENCES repo(id) ON DELETE CASCADE,
    role TEXT NOT NULL CHECK (role IN ('read', 'write', 'admin')),
    PRIMARY KEY (user_id, repo_id)
);

CREATE TABLE mirror (
    id INTEGER PRIMARY KEY,
    public_id TEXT NOT NULL UNIQUE CHECK (length(public_id) = 26 AND length(CAST(public_id AS BLOB)) = 26 AND public_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*' AND substr(public_id, 1, 1) BETWEEN '0' AND '7'),
    repo_id INTEGER NOT NULL REFERENCES repo(id) ON DELETE CASCADE,
    remote_url TEXT NOT NULL,
    credential TEXT NOT NULL,
    enabled INTEGER NOT NULL CHECK (enabled IN (0, 1)),
    state TEXT NOT NULL CHECK (state IN ('idle', 'queued', 'running')),
    redo INTEGER NOT NULL CHECK (redo IN (0, 1)),
    attempts INTEGER NOT NULL,
    next_attempt_at INTEGER,
    last_success_at INTEGER,
    last_error TEXT,
    last_error_at INTEGER,
    UNIQUE (repo_id, remote_url)
);

CREATE INDEX mirror_state_next_attempt_at ON mirror (state, next_attempt_at);
