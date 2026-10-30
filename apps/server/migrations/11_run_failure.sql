-- Why a run failed (0008, 0108): the failing check when the backend knows it, and the
-- reason (the exit status, or `lease-expired`).
ALTER TABLE run ADD COLUMN failure_check TEXT;
ALTER TABLE run ADD COLUMN failure_reason TEXT;
