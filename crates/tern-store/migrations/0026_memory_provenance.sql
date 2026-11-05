-- Agent memory provenance (DECISIONS.md "Memory for read-only agents"):
-- where a memory came from, so a note saved in a run that had read
-- untrusted content is recalled as unverified, and which run wrote it.
-- Self-contained, so a merge can renumber it.

-- `agent` (saved in a clean run), `unverified` (saved in a tainted run) or
-- `user` (written or edited by the user in the app). Memories from before
-- this column were saved with the user's approval or in clean runs.
ALTER TABLE memories ADD COLUMN source TEXT NOT NULL DEFAULT 'agent';
-- The run that saved it; NULL for the user's own and for older rows.
ALTER TABLE memories ADD COLUMN run_id TEXT;
CREATE INDEX memories_run ON memories(run_id);
-- The ledger's per-run count (a run's memory writes are capped).
CREATE INDEX ledger_run ON ledger(run_id, seq);
