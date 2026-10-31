-- ADR 0097: a third run trigger, `worktree`, and the run's selected checks (NULL = the
-- whole `nix flake check`; else a JSON array of check names, opaque to the schema).
-- Widening `run.trigger`'s CHECK needs a rebuild of `run`. sqlx runs SQLite migrations
-- in a transaction, where foreign keys cannot be disabled, so dropping `run` would
-- cascade to its jobs; the CI tables under `run` are copied, dropped leaf first,
-- recreated unchanged and refilled around the rebuild.
CREATE TABLE copy_job AS SELECT * FROM job;
CREATE TABLE copy_job_attempt AS SELECT * FROM job_attempt;
CREATE TABLE copy_step AS SELECT * FROM step;
CREATE TABLE copy_log_chunk AS SELECT * FROM log_chunk;
CREATE TABLE copy_artifact AS SELECT * FROM artifact;
DROP TABLE artifact;
DROP TABLE log_chunk;
DROP TABLE step;
DROP TABLE job_attempt;
DROP TABLE job;

CREATE TABLE new_run (
    id INTEGER PRIMARY KEY,
    public_id TEXT NOT NULL UNIQUE CHECK (length(public_id) = 26 AND length(CAST(public_id AS BLOB)) = 26 AND public_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*' AND substr(public_id, 1, 1) BETWEEN '0' AND '7'),
    repo_id INTEGER NOT NULL REFERENCES repo(id) ON DELETE CASCADE,
    sha TEXT NOT NULL,
    ref TEXT,
    trigger TEXT NOT NULL CHECK (trigger IN ('push', 'manual', 'worktree')),
    event_id INTEGER UNIQUE REFERENCES event(id),
    triggered_by INTEGER REFERENCES user(id),
    state TEXT NOT NULL CHECK (state IN ('queued', 'running', 'succeeded', 'failed', 'cancelled')),
    created_at INTEGER NOT NULL,
    started_at INTEGER,
    finished_at INTEGER,
    failure_check TEXT,
    failure_reason TEXT,
    checks TEXT
);
INSERT INTO new_run (id, public_id, repo_id, sha, ref, trigger, event_id, triggered_by, state, created_at, started_at, finished_at, failure_check, failure_reason)
SELECT id, public_id, repo_id, sha, ref, trigger, event_id, triggered_by, state, created_at, started_at, finished_at, failure_check, failure_reason FROM run;
DROP TABLE run;
ALTER TABLE new_run RENAME TO run;
CREATE INDEX run_repo_id_id ON run (repo_id, id);

CREATE TABLE job (
    id INTEGER PRIMARY KEY,
    public_id TEXT NOT NULL UNIQUE CHECK (length(public_id) = 26 AND length(CAST(public_id AS BLOB)) = 26 AND public_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*' AND substr(public_id, 1, 1) BETWEEN '0' AND '7'),
    run_id INTEGER NOT NULL REFERENCES run(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    requirements TEXT NOT NULL,
    replayable INTEGER NOT NULL CHECK (replayable IN (0, 1)),
    max_attempts INTEGER NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'assigned', 'running', 'succeeded', 'failed', 'cancelled', 'lost')),
    pending_reason TEXT,
    created_at INTEGER NOT NULL,
    finished_at INTEGER,
    UNIQUE (run_id, name)
);

CREATE INDEX job_state ON job (state);

CREATE TABLE job_attempt (
    id INTEGER PRIMARY KEY,
    public_id TEXT NOT NULL UNIQUE CHECK (length(public_id) = 26 AND length(CAST(public_id AS BLOB)) = 26 AND public_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*' AND substr(public_id, 1, 1) BETWEEN '0' AND '7'),
    job_id INTEGER NOT NULL REFERENCES job(id) ON DELETE CASCADE,
    number INTEGER NOT NULL,
    runner_id INTEGER NOT NULL REFERENCES runner(id) ON DELETE RESTRICT,
    state TEXT NOT NULL CHECK (state IN ('running', 'succeeded', 'failed', 'cancelled', 'lost')),
    lease_expires_at INTEGER NOT NULL,
    exit_code INTEGER,
    started_at INTEGER NOT NULL,
    finished_at INTEGER,
    UNIQUE (job_id, number)
);

CREATE INDEX job_attempt_state_lease_expires_at ON job_attempt (state, lease_expires_at);

CREATE TABLE step (
    id INTEGER PRIMARY KEY,
    attempt_id INTEGER NOT NULL REFERENCES job_attempt(id) ON DELETE CASCADE,
    idx INTEGER NOT NULL,
    name TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('running', 'succeeded', 'failed', 'skipped')),
    exit_code INTEGER,
    started_at INTEGER,
    finished_at INTEGER,
    UNIQUE (attempt_id, idx)
);

CREATE TABLE log_chunk (
    attempt_id INTEGER NOT NULL REFERENCES job_attempt(id) ON DELETE CASCADE,
    seq INTEGER NOT NULL,
    step_idx INTEGER NOT NULL,
    offset INTEGER NOT NULL,
    length INTEGER NOT NULL,
    PRIMARY KEY (attempt_id, seq)
);

CREATE TABLE artifact (
    id INTEGER PRIMARY KEY,
    public_id TEXT NOT NULL UNIQUE CHECK (length(public_id) = 26 AND length(CAST(public_id AS BLOB)) = 26 AND public_id NOT GLOB '*[^0123456789ABCDEFGHJKMNPQRSTVWXYZ]*' AND substr(public_id, 1, 1) BETWEEN '0' AND '7'),
    attempt_id INTEGER NOT NULL REFERENCES job_attempt(id) ON DELETE CASCADE,
    name TEXT NOT NULL,
    size INTEGER NOT NULL,
    sha256 TEXT NOT NULL,
    created_at INTEGER NOT NULL,
    expires_at INTEGER NOT NULL,
    UNIQUE (attempt_id, name)
);

INSERT INTO job SELECT * FROM copy_job;
INSERT INTO job_attempt SELECT * FROM copy_job_attempt;
INSERT INTO step SELECT * FROM copy_step;
INSERT INTO log_chunk SELECT * FROM copy_log_chunk;
INSERT INTO artifact SELECT * FROM copy_artifact;
DROP TABLE copy_job;
DROP TABLE copy_job_attempt;
DROP TABLE copy_step;
DROP TABLE copy_log_chunk;
DROP TABLE copy_artifact;
