ALTER TABLE versions ALTER COLUMN features DROP NOT NULL;
ALTER TABLE versions ALTER COLUMN features DROP DEFAULT;
ALTER TABLE versions ALTER COLUMN features SET DATA TYPE text;
