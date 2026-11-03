-- A compaction summary replaces every message with id <= summary_of
-- when building model context.
ALTER TABLE messages ADD COLUMN summary_of INTEGER;
