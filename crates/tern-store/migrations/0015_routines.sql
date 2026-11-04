-- T1b: schedules become routines (docs/personal-agent-tooling-spec.md §3,
-- §4.1). A routine is a `schedules` row with a trigger; every existing row
-- is a routine with a cron trigger. Self-contained, so a merge can
-- renumber it.

-- `cron` (the `cron` column), `once` (at `next_run_at`, then disabled) or
-- `watch` (a connector's watch, polled every `every_secs`).
ALTER TABLE schedules ADD COLUMN trigger_kind TEXT NOT NULL DEFAULT 'cron';
-- `<connector>.<watch>` for a watch trigger.
ALTER TABLE schedules ADD COLUMN watch TEXT;
ALTER TABLE schedules ADD COLUMN every_secs INTEGER;
-- The watch's opaque JSON cursor; saved with the events it found, in one
-- transaction. NULL until the first poll, which is a baseline.
ALTER TABLE schedules ADD COLUMN watch_cursor TEXT;
-- Start runs for the first poll's events instead of only recording them.
ALTER TABLE schedules ADD COLUMN backfill INTEGER NOT NULL DEFAULT 0;
-- Keep events the watch marks as caused by tern itself.
ALTER TABLE schedules ADD COLUMN include_self INTEGER NOT NULL DEFAULT 0;
-- Luau run before a cron or once slot; false skips the run.
ALTER TABLE schedules ADD COLUMN gate TEXT;
-- The active connector whose capabilities (and tools) the gate gets.
ALTER TABLE schedules ADD COLUMN gate_connector TEXT;
-- silent | digest | notify | interrupt; NULL = `[schedules] initiative`.
ALTER TABLE schedules ADD COLUMN initiative TEXT;
-- What the last poll or gate found ("3 new events", "gate: skipped",
-- "watch failed: …"), and when.
ALTER TABLE schedules ADD COLUMN last_check TEXT;
ALTER TABLE schedules ADD COLUMN last_check_at INTEGER;

-- Schedules from before routines pushed every result (subject to quiet
-- hours), which is `interrupt`. New routines start at the default.
UPDATE schedules SET initiative = 'interrupt';

-- Events a watch reported, deduplicated per routine: a re-delivered or
-- overlapping event can't start a second run.
CREATE TABLE routine_events (
  routine_id TEXT NOT NULL REFERENCES schedules(id) ON DELETE CASCADE,
  event_key TEXT NOT NULL,
  -- The event as the watch returned it (JSON); emptied once a run has it.
  payload TEXT NOT NULL,
  -- pending → started; batch → started (overflow, one run for all);
  -- baseline (the first poll) and self (caused by tern) never run.
  state TEXT NOT NULL,
  run_id TEXT,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (routine_id, event_key)
);
CREATE INDEX routine_events_waiting ON routine_events(state, routine_id);

-- Why a delivery is held: `quiet` (quiet hours) or `digest` (an initiative
-- digest, sent at the next digest time).
ALTER TABLE held_deliveries ADD COLUMN kind TEXT NOT NULL DEFAULT 'quiet';
