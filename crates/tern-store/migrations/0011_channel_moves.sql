-- Moving conversations into channels (DECISIONS.md "T2 moves"):
-- where a thread came from. Self-contained so a merge can renumber it.
ALTER TABLE sessions ADD COLUMN moved_from TEXT;   -- the channel it was in
ALTER TABLE sessions ADD COLUMN moved_at INTEGER;
