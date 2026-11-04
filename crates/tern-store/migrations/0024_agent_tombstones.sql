-- Deleting an agent (docs/agent-profiles-design.md §2, DECISIONS.md "Editing
-- and deleting"): the row stays as a tombstone, so every session, run, post
-- and ledger entry that names it still resolves to its name. A deleted agent
-- is invisible to lookups by slug or id and to every list; its slug and DM
-- slug are freed. Self-contained, so a merge can renumber it.
ALTER TABLE agents ADD COLUMN deleted_at INTEGER;
