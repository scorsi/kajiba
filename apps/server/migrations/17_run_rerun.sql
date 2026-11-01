-- ADR 0163: retain the public link even after worktree retention removes its source.
ALTER TABLE run ADD COLUMN rerun_of TEXT;
