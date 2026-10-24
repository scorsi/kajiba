CREATE TABLE migration_probe (value INTEGER NOT NULL);
INSERT INTO migration_probe VALUES (1);
UPDATE preserved SET value = 'changed';
THIS IS INVALID SQL;
