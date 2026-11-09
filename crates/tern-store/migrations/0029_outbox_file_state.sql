-- A queued file edit remembers the file it was meant for: the resolved path
-- and a content hash (`absent` when the file didn't exist), so approving it
-- later can tell the file changed in between. NULL for every other tool.
-- State `stale`: it was skipped because the file changed; Discard clears it.
ALTER TABLE outbox ADD COLUMN file_path TEXT;
ALTER TABLE outbox ADD COLUMN file_hash TEXT;
