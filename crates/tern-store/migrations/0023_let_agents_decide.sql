-- "Let agents decide" (docs/agent-profiles-design.md §5.2, AP3): a topic
-- channel setting. On, a post that mentions no one is routed by one call on
-- the gate model to the members who should answer, instead of always going
-- to the lead. Off (0) by default.
ALTER TABLE channels ADD COLUMN let_agents_decide INTEGER NOT NULL DEFAULT 0;
