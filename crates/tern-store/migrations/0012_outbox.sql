-- T3: the outbox, deferred approvals and autonomy profiles
-- (docs/personal-agent-tooling-spec.md §5.2, §5.4). Self-contained, so a
-- merge can renumber it.

-- The run's autonomy profile, frozen when it starts (like its model), so a
-- resumed run asks exactly where it asked before. NULL = supervised.
ALTER TABLE runs ADD COLUMN autonomy TEXT;
-- A routine's own profile; NULL = the configured `autonomy`.
ALTER TABLE schedules ADD COLUMN autonomy TEXT;

-- A write nobody could approve in time, waiting for the user.
CREATE TABLE outbox (
  id INTEGER PRIMARY KEY,
  run_id TEXT NOT NULL REFERENCES runs(id),
  session_id TEXT NOT NULL,
  call_id TEXT NOT NULL,
  tool TEXT NOT NULL,
  input_json TEXT NOT NULL,
  summary TEXT NOT NULL,
  effect TEXT NOT NULL,
  taint TEXT,                       -- why the run was tainted, if it was
  local_only INTEGER NOT NULL DEFAULT 0,
  -- pending → approved → done | failed; pending → rejected | expired
  state TEXT NOT NULL DEFAULT 'pending',
  created_at INTEGER NOT NULL,
  expires_at INTEGER NOT NULL,
  decided_at INTEGER,
  note TEXT,                        -- the user's note on rejecting
  edited_input_json TEXT,           -- arguments the user changed before approving
  exec_run_id TEXT,                 -- the system run that executes it, exactly once
  result_json TEXT,                 -- the tool's result once executed
  UNIQUE (run_id, call_id)
);
CREATE INDEX outbox_state ON outbox(state, expires_at);
CREATE UNIQUE INDEX outbox_exec_run ON outbox(exec_run_id) WHERE exec_run_id IS NOT NULL;
