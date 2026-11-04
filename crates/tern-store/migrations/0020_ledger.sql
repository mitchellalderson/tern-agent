-- T6b: the activity ledger (docs/personal-agent-tooling-spec.md §8.1).
-- Self-contained, so a merge can renumber it.
--
-- Append-only and hash-chained: `hash = sha256(prev_hash || row)`, where
-- `row` is the canonical JSON of every other column (see
-- `tern_store::ledger_repo`). `prev_hash` is unique, so two writers can
-- never fork the chain; `tern ledger verify` recomputes it. No foreign
-- keys: an entry outlives the run, channel or routine it names.
CREATE TABLE ledger (
  seq INTEGER PRIMARY KEY,
  at INTEGER NOT NULL,
  kind TEXT NOT NULL,          -- approval, effect, grant.add, outbox.approve, …
  actor TEXT NOT NULL,         -- 'owner' (you), 'agent:<id>', 'tern' (the host)
  subject TEXT NOT NULL,       -- the tool, connector, connection or routine
  summary TEXT NOT NULL,
  detail TEXT,                 -- JSON
  run_id TEXT,
  session_id TEXT,
  channel_id TEXT,
  routine_id TEXT,
  connector TEXT,
  prev_hash TEXT NOT NULL UNIQUE,
  hash TEXT NOT NULL
);
CREATE INDEX ledger_kind ON ledger(kind, seq);
CREATE INDEX ledger_channel ON ledger(channel_id, seq);
CREATE INDEX ledger_routine ON ledger(routine_id, seq);
CREATE INDEX ledger_connector ON ledger(connector, seq);

-- Defence in depth: the chain detects edits, these refuse them.
CREATE TRIGGER ledger_no_update BEFORE UPDATE ON ledger
BEGIN SELECT RAISE(ABORT, 'the ledger is append-only'); END;
CREATE TRIGGER ledger_no_delete BEFORE DELETE ON ledger
BEGIN SELECT RAISE(ABORT, 'the ledger is append-only'); END;
