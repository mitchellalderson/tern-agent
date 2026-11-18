-- Proactive memory, open loops, new-topic hints and the weekly tidy-up
-- (docs/innbot-learnings.md P3, P4, P9, P11). A card can sit in a chosen
-- channel (the "New topic?" hint lives where the user is talking), a wait
-- can be a follow-up the agent promised, and every memory tidy-up change is
-- logged with what it replaced so it can be undone.
ALTER TABLE proactive_cards ADD COLUMN channel_id TEXT;
ALTER TABLE waits ADD COLUMN followup INTEGER NOT NULL DEFAULT 0;

CREATE TABLE memory_tidy (
  id INTEGER PRIMARY KEY,
  at INTEGER NOT NULL,
  action TEXT NOT NULL,                 -- merge | conflict
  namespace TEXT NOT NULL,              -- 'shared' or an agent id
  note TEXT NOT NULL DEFAULT '',        -- one line for the list
  before TEXT NOT NULL,                 -- JSON: the rows as they were
  kept_id INTEGER,                      -- merge: the memory that stayed
  state TEXT NOT NULL DEFAULT 'applied' -- applied | undone | dismissed
);
CREATE INDEX memory_tidy_at ON memory_tidy(at);
