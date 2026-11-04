-- T1a: quiet scheduled runs and per-schedule budgets.

-- NULL = the [schedules] default; else "HH:MM-HH:MM" or "none".
ALTER TABLE schedules ADD COLUMN quiet_hours TEXT;
-- Delivers even inside quiet hours.
ALTER TABLE schedules ADD COLUMN urgent INTEGER NOT NULL DEFAULT 0;
-- Caps; NULL = the [schedules] default.
ALTER TABLE schedules ADD COLUMN runs_per_day INTEGER;
ALTER TABLE schedules ADD COLUMN usd_per_day REAL;
ALTER TABLE schedules ADD COLUMN tokens_per_run INTEGER;
-- Set when a daily cap paused the schedule: until when (unix ms), and why.
ALTER TABLE schedules ADD COLUMN paused_until INTEGER;
ALTER TABLE schedules ADD COLUMN paused_reason TEXT;
-- When the user last resumed it: daily caps count from here or from the
-- start of the day, whichever is later.
ALTER TABLE schedules ADD COLUMN budget_since INTEGER;

-- A scheduled run whose final answer was NO_REPLY: completed, delivered nothing.
ALTER TABLE runs ADD COLUMN quiet INTEGER NOT NULL DEFAULT 0;

-- Deliveries held during quiet hours, sent (combined per target) at release_at.
CREATE TABLE held_deliveries (
  id INTEGER PRIMARY KEY, target TEXT NOT NULL, run_id TEXT,
  label TEXT, text TEXT NOT NULL,
  release_at INTEGER NOT NULL, created_at INTEGER NOT NULL
);
CREATE INDEX held_deliveries_release ON held_deliveries(release_at);
