-- T5b: skill capture (docs/personal-agent-tooling-spec.md §7). After a long,
-- clean run tern offers once to save it as a skill; one row per run, so a
-- run is never offered twice. Self-contained, so a merge can renumber it.
CREATE TABLE skill_offers (
  id INTEGER PRIMARY KEY,
  run_id TEXT NOT NULL UNIQUE,
  session_id TEXT NOT NULL,       -- the run's conversation
  tool_calls INTEGER NOT NULL,
  title TEXT,                     -- the conversation's title when offered
  state TEXT NOT NULL DEFAULT 'pending',  -- pending | accepted | dismissed
  skill TEXT,                     -- the draft's name once accepted
  created_at INTEGER NOT NULL,
  decided_at INTEGER
);
CREATE INDEX skill_offers_session ON skill_offers(session_id, state);
