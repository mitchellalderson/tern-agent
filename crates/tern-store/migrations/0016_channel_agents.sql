-- Agents in topic channels (docs/agent-profiles-design.md §5, AP2). Uses
-- AP0's `channel_agents`, `runs.woken_by` and `posts.wake_depth`; this adds
-- each channel's wake settings, a member's own main thread there, and the
-- agents a post mentions. Self-contained, so a merge can renumber it.

-- §5.3: agents' @mentions wake nobody unless the channel allows it.
ALTER TABLE channels ADD COLUMN agent_wakes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE channels ADD COLUMN max_hops INTEGER NOT NULL DEFAULT 2;
ALTER TABLE channels ADD COLUMN wakes_per_hour INTEGER NOT NULL DEFAULT 6;

-- A member's main thread in the channel (the lead's is
-- `channels.main_session_id`).
ALTER TABLE channel_agents ADD COLUMN main_session_id TEXT;

-- JSON list of the agent ids a post @mentions; NULL = none.
ALTER TABLE posts ADD COLUMN mentions TEXT;

CREATE INDEX runs_woken_by ON runs(woken_by) WHERE woken_by IS NOT NULL;
