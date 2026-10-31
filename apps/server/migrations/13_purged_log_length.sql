ALTER TABLE job_attempt ADD COLUMN purged_log_length INTEGER NOT NULL DEFAULT 0 CHECK (purged_log_length >= 0);
