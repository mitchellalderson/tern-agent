-- Proactive help (docs/innbot-learnings.md P1, P2, P5, P6, P7). One generic
-- dismissible card record every proactive feature writes, the approvals the
-- user answered "Allow once" (P7 learns from them), and a small key/value
-- table for what the host last did (heartbeat day, last seen). Self-contained,
-- so a merge can renumber it.
CREATE TABLE proactive_cards (
  id INTEGER PRIMARY KEY,
  kind TEXT NOT NULL,             -- heartbeat | routine_fix | routine_suggestion | away_brief | ...
  subject TEXT NOT NULL DEFAULT '', -- what it is about (a routine id, a day): caps count per subject
  agent_id TEXT NOT NULL,         -- whose DM it is posted in
  title TEXT NOT NULL,
  body TEXT NOT NULL DEFAULT '',
  accept_label TEXT,              -- NULL: the card can only be dismissed
  accept_message TEXT,            -- what accepting tells the agent, as the user
  state TEXT NOT NULL DEFAULT 'pending',  -- pending | accepted | dismissed
  post_id INTEGER,                -- the card's post in the agent's DM
  created_at INTEGER NOT NULL,
  decided_at INTEGER
);
CREATE INDEX proactive_cards_kind ON proactive_cards(kind, subject, created_at);
CREATE INDEX proactive_cards_post ON proactive_cards(post_id);

CREATE TABLE approval_taps (
  id INTEGER PRIMARY KEY,
  agent_id TEXT NOT NULL,
  tool TEXT NOT NULL,
  target TEXT NOT NULL,           -- the grant key or command pattern the answer was for
  at INTEGER NOT NULL
);
CREATE INDEX approval_taps_key ON approval_taps(agent_id, tool, target, at);

CREATE TABLE proactive_state (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  updated_at INTEGER NOT NULL
);
