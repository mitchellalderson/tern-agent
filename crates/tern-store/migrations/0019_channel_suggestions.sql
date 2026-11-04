-- Channel suggestions (docs/channels-design.md C5, tooling spec §4.2): the
-- agent proposes a channel with `channel_suggest`; the user accepts or
-- declines a card. Self-contained, so a merge can renumber it.
CREATE TABLE channel_suggestions (
  id INTEGER PRIMARY KEY,
  slug TEXT NOT NULL,
  title TEXT NOT NULL,
  why TEXT NOT NULL,
  routines TEXT,                  -- JSON list of schedule ids it would move
  run_id TEXT,                    -- the run that suggested it
  session_id TEXT,                -- that run's conversation (its tree's root session)
  post_id INTEGER,                -- its card (a post)
  state TEXT NOT NULL DEFAULT 'pending',  -- pending | accepted | declined
  channel_id TEXT,                -- the channel accepting created
  created_at INTEGER NOT NULL,
  decided_at INTEGER
);
CREATE INDEX channel_suggestions_post ON channel_suggestions(post_id);
