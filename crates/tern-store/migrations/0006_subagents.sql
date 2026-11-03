-- Sub-agents (docs/subagents-design.md §1.2): a run started by another
-- run's agent_spawn call.
ALTER TABLE runs ADD COLUMN parent_run_id TEXT REFERENCES runs(id);
ALTER TABLE runs ADD COLUMN parent_call_id TEXT;     -- the tool call that spawned it
ALTER TABLE runs ADD COLUMN label TEXT;              -- unique per parent run
ALTER TABLE runs ADD COLUMN depth INTEGER NOT NULL DEFAULT 0;
ALTER TABLE runs ADD COLUMN tool_scope TEXT;         -- JSON array of tool names; NULL = all
ALTER TABLE runs ADD COLUMN budget_tokens INTEGER;   -- NULL = unlimited
ALTER TABLE runs ADD COLUMN max_iterations INTEGER;  -- NULL = config default
-- Makes agent_spawn idempotent: re-executing the step finds the same child.
CREATE UNIQUE INDEX runs_parent_call ON runs(parent_run_id, parent_call_id);
CREATE UNIQUE INDEX runs_parent_label ON runs(parent_run_id, label);
CREATE INDEX runs_parent ON runs(parent_run_id);

-- Messages between runs of one tree, consumed by journal steps.
CREATE TABLE agent_mail (
  id INTEGER PRIMARY KEY,
  to_run_id TEXT NOT NULL REFERENCES runs(id),
  from_run_id TEXT,                 -- NULL = the user
  kind TEXT NOT NULL,               -- finished | question | answer | message
  body TEXT NOT NULL,               -- JSON
  dedupe_key TEXT NOT NULL UNIQUE,  -- e.g. finished:<child>
  created_at INTEGER NOT NULL,
  consumed_run_id TEXT,             -- set in the consuming step's transaction
  consumed_step INTEGER
);
CREATE INDEX agent_mail_pending ON agent_mail(to_run_id, consumed_step);
