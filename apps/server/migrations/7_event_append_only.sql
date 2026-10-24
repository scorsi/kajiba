-- docs/schema.md: the event journal is append-only. Ids have no AUTOINCREMENT, so a
-- deleted newest row would have its id reused and skipped by handlers checkpointed
-- past it; enforce the invariant in the database, not only in the code.
CREATE TRIGGER event_no_update BEFORE UPDATE ON event
BEGIN
    SELECT RAISE(ABORT, 'event is append-only');
END;

CREATE TRIGGER event_no_delete BEFORE DELETE ON event
BEGIN
    SELECT RAISE(ABORT, 'event is append-only');
END;
