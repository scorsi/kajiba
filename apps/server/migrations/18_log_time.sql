ALTER TABLE log_chunk ADD COLUMN produced_at_ms INTEGER NOT NULL DEFAULT 0;
CREATE INDEX log_chunk_offset ON log_chunk (attempt_id, offset);
