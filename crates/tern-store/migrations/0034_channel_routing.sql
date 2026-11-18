-- Who replies in a topic channel (docs/innbot-learnings.md B1, B2, B4).
-- An agent's one-line description is what the router reads, never its
-- persona; empty = written once from its role. `mention_only` makes an
-- agent answer only when @mentioned ("Reply when relevant" off). A channel routes to `one`
-- agent (ties go to the lead) or `all` relevant ones, and may carry a
-- plain-English reply rule.
ALTER TABLE agents ADD COLUMN description TEXT;
ALTER TABLE agents ADD COLUMN mention_only INTEGER NOT NULL DEFAULT 0;
ALTER TABLE channels ADD COLUMN route_mode TEXT NOT NULL DEFAULT 'one';
ALTER TABLE channels ADD COLUMN reply_rule TEXT;
