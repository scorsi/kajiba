CREATE TABLE user (
    id INTEGER PRIMARY KEY,
    public_id TEXT NOT NULL UNIQUE CHECK (length(public_id) = 26 AND length(CAST(public_id AS BLOB)) = 26 AND public_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*' AND substr(public_id, 1, 1) BETWEEN '0' AND '7'),
    name TEXT NOT NULL UNIQUE,
    kind TEXT NOT NULL CHECK (kind IN ('local', 'external')),
    is_admin INTEGER NOT NULL CHECK (is_admin IN (0, 1)),
    source TEXT NOT NULL CHECK (source IN ('config', 'runtime')),
    created_at INTEGER NOT NULL,
    disabled_at INTEGER,
    CHECK (kind != 'external' OR is_admin = 0)
);

CREATE TABLE ssh_key (
    id INTEGER PRIMARY KEY,
    public_id TEXT NOT NULL UNIQUE CHECK (length(public_id) = 26 AND length(CAST(public_id AS BLOB)) = 26 AND public_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*' AND substr(public_id, 1, 1) BETWEEN '0' AND '7'),
    user_id INTEGER NOT NULL REFERENCES user(id) ON DELETE CASCADE,
    usage TEXT NOT NULL CHECK (usage IN ('auth', 'signing')),
    key_type TEXT NOT NULL,
    public_key TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('config', 'runtime')),
    created_at INTEGER NOT NULL,
    last_used_at INTEGER,
    UNIQUE (fingerprint, usage)
);

CREATE TABLE api_token (
    id INTEGER PRIMARY KEY,
    public_id TEXT NOT NULL UNIQUE CHECK (length(public_id) = 26 AND length(CAST(public_id AS BLOB)) = 26 AND public_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*' AND substr(public_id, 1, 1) BETWEEN '0' AND '7'),
    user_id INTEGER NOT NULL REFERENCES user(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    token_hash BLOB NOT NULL UNIQUE,
    scopes TEXT NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('config', 'runtime')),
    created_at INTEGER NOT NULL,
    expires_at INTEGER,
    last_used_at INTEGER,
    revoked_at INTEGER,
    UNIQUE (user_id, name)
);

CREATE TABLE webauthn_credential (
    id INTEGER PRIMARY KEY,
    public_id TEXT NOT NULL UNIQUE CHECK (length(public_id) = 26 AND length(CAST(public_id AS BLOB)) = 26 AND public_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*' AND substr(public_id, 1, 1) BETWEEN '0' AND '7'),
    user_id INTEGER NOT NULL REFERENCES user(id) ON DELETE CASCADE,
    credential_id BLOB NOT NULL UNIQUE,
    passkey TEXT NOT NULL,
    name TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    last_used_at INTEGER
);

CREATE TABLE session (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL REFERENCES user(id) ON DELETE CASCADE,
    token_hash BLOB NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL
);

CREATE INDEX session_expires_at ON session (expires_at);

CREATE TABLE enrollment_token (
    id INTEGER PRIMARY KEY,
    user_id INTEGER NOT NULL UNIQUE REFERENCES user(id) ON DELETE CASCADE,
    token_hash BLOB NOT NULL UNIQUE,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    consumed_at INTEGER
);
