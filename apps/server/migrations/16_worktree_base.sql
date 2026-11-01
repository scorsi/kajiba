-- ADR 0151: display-only base commit; older runs have no recorded base.
ALTER TABLE run ADD COLUMN base_sha TEXT;
