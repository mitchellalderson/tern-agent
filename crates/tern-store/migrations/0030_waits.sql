-- Waits: a one-shot "wait until" tied to a conversation. The daemon polls
-- the check with no model call; when its condition matches (or it times
-- out) it starts one run in `session_id` and deletes the row.

CREATE TABLE waits (
  id TEXT PRIMARY KEY,
  -- The conversation the continuation lands in, and the run that made it.
  session_id TEXT NOT NULL,
  run_id TEXT NOT NULL,
  agent_id TEXT NOT NULL,
  note TEXT NOT NULL,
  -- `connector`, `url` or `time`, and its JSON arguments.
  check_kind TEXT NOT NULL,
  check_spec TEXT NOT NULL,
  -- JSON `{path, equals | in}`; NULL for a time wait.
  until_spec TEXT,
  every_secs INTEGER NOT NULL,
  next_check_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  -- The creating run had read untrusted content: the continuation starts
  -- tainted for the same reason.
  tainted INTEGER NOT NULL DEFAULT 0,
  -- A chat to send the continuation's answer to, as for routines.
  deliver_to TEXT,
  created_at INTEGER NOT NULL,
  checks INTEGER NOT NULL DEFAULT 0,
  last_checked_at INTEGER,
  -- What the last check found, short, for the list and the continuation.
  last_check TEXT,
  last_value TEXT
);
CREATE INDEX waits_due ON waits (next_check_at);
CREATE INDEX waits_agent ON waits (agent_id);
