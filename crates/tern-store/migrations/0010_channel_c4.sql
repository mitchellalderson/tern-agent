-- T2 C4 (docs/channels-design.md §6): sub-agents that post, and "needs
-- you" items with pointers in Home. Self-contained so a merge can
-- renumber it.

-- A sub-agent spawned with `"post": true`: may use channel_post, and its
-- report is posted when it finishes.
ALTER TABLE runs ADD COLUMN can_post INTEGER NOT NULL DEFAULT 0;

-- A `needs_you` post (and the `pointer` to it in Home) stays open until
-- the user marks it done.
ALTER TABLE posts ADD COLUMN resolved_at INTEGER;
CREATE INDEX posts_open ON posts(channel_id, kind, resolved_at);
