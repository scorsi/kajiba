-- docs/schema.md: user names are stored lowercase, as repo and runner names. SQLite
-- cannot add a CHECK without rebuilding `user`, and the rebuild's DROP TABLE would
-- cascade to every child row because sqlx runs SQLite migrations in a transaction,
-- where foreign keys cannot be disabled; these triggers enforce the same CHECK.
CREATE TRIGGER user_name_lowercase_insert BEFORE INSERT ON user
WHEN NEW.name != lower(NEW.name)
BEGIN
    SELECT RAISE(ABORT, 'CHECK constraint failed: user.name = lower(user.name)');
END;

CREATE TRIGGER user_name_lowercase_update BEFORE UPDATE OF name ON user
WHEN NEW.name != lower(NEW.name)
BEGIN
    SELECT RAISE(ABORT, 'CHECK constraint failed: user.name = lower(user.name)');
END;
