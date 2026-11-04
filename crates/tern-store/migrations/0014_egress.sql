-- The tainted host limit's setting (docs/personal-agent-tooling-spec.md
-- §5.3): `ask` (NULL) or `allow`. Self-contained, so a merge can renumber it.
-- Frozen on each run when it starts, like its autonomy; sub-agents copy it.
ALTER TABLE runs ADD COLUMN egress TEXT;
-- An agent's own setting; NULL = the configured `[taint] egress`.
ALTER TABLE agents ADD COLUMN egress TEXT;
