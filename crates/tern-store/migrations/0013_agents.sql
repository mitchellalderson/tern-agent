-- Agent profiles (docs/agent-profiles-design.md §2): several configurable
-- agents, each with its own DM. Additive: every new column is NULL for
-- existing rows, which the code reads as the default agent, `tern`.

CREATE TABLE agents (
  id TEXT PRIMARY KEY,              -- 'tern' (default) | 'tern:<slug at creation>'
  slug TEXT NOT NULL UNIQUE,        -- [a-z0-9][a-z0-9-]{0,31}
  name TEXT NOT NULL,
  avatar TEXT,                      -- JSON {glyph, color}; NULL = generated from id
  model TEXT,                       -- ModelChain (string or JSON list); NULL = default_model
  subagent_model TEXT,
  tools TEXT,                       -- JSON scope (§1.2); NULL = all
  memory TEXT NOT NULL DEFAULT 'private',   -- shared | private
  telegram TEXT,                    -- JSON {bot_token_secret} (AP3)
  delegates TEXT,                   -- JSON list of agent ids (AP3)
  archived_at INTEGER,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);
INSERT INTO agents (id, slug, name, memory, created_at, updated_at)
VALUES ('tern', 'tern', 'tern', 'shared',
        CAST(strftime('%s', 'now') AS INTEGER) * 1000,
        CAST(strftime('%s', 'now') AS INTEGER) * 1000);

ALTER TABLE sessions  ADD COLUMN agent_id TEXT REFERENCES agents(id);   -- NULL read as 'tern'
ALTER TABLE schedules ADD COLUMN agent_id TEXT REFERENCES agents(id);
ALTER TABLE channels  ADD COLUMN agent_id TEXT REFERENCES agents(id);   -- main: whose DM; topic: the lead
ALTER TABLE memories  ADD COLUMN agent_id TEXT REFERENCES agents(id);   -- NULL = shared

-- The memory scope frozen when the run started: NULL = shared, else
-- 'private'. A run's agent is its session's agent.
ALTER TABLE runs ADD COLUMN memory_scope TEXT;

CREATE TABLE channel_agents (       -- topic channel membership (§5, AP2)
  channel_id TEXT NOT NULL REFERENCES channels(id),
  agent_id TEXT NOT NULL REFERENCES agents(id),
  added_at INTEGER NOT NULL,
  PRIMARY KEY (channel_id, agent_id)
);

-- Loop guards (§5.3, AP2): which post woke a run, and how far down a wake
-- chain it is.
ALTER TABLE runs  ADD COLUMN woken_by INTEGER REFERENCES posts(id);
ALTER TABLE posts ADD COLUMN wake_depth INTEGER NOT NULL DEFAULT 0;
CREATE INDEX sessions_agent ON sessions(agent_id, updated_at);
CREATE INDEX posts_author ON posts(channel_id, author_id);
CREATE INDEX memories_agent ON memories(agent_id);

-- Home is the default agent's DM.
UPDATE channels SET agent_id = 'tern' WHERE id = 'home';
