-- A reserved import has a session id but no generation until OpenSession.
-- Clearing the session id records exit durably, including retained/archived rows.
ALTER TABLE pending_push ADD COLUMN git_session_id TEXT;
ALTER TABLE pending_push ADD COLUMN git_generation TEXT;
CREATE UNIQUE INDEX pending_push_git_session ON pending_push(git_session_id);
