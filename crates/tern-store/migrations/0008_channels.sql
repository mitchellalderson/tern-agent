-- T2 channels (docs/channels-design.md §2.2): channels over sessions, and
-- posts as each channel's public timeline.

CREATE TABLE channels (
  id TEXT PRIMARY KEY,
  slug TEXT NOT NULL UNIQUE,        -- [a-z0-9][a-z0-9-]{0,31}
  title TEXT NOT NULL,
  kind TEXT NOT NULL,               -- main | topic
  instructions TEXT,
  main_session_id TEXT REFERENCES sessions(id),  -- the current main thread; Home: the newest segment
  created_by TEXT NOT NULL,         -- user | agent (an accepted channel_suggest)
  archived_at INTEGER,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);

ALTER TABLE sessions ADD COLUMN channel_id TEXT REFERENCES channels(id);
-- The newest post this session's agent has been shown (the §3 digest cursor, C3).
ALTER TABLE sessions ADD COLUMN posts_seen INTEGER NOT NULL DEFAULT 0;
-- Home's stream (§2.6): the segment this one continues, and why it was cut.
ALTER TABLE sessions ADD COLUMN prev_session_id TEXT REFERENCES sessions(id);
ALTER TABLE sessions ADD COLUMN segment_reason TEXT;   -- idle | day | new_topic | migrated
-- A side thread: the message or post it branched from.
ALTER TABLE sessions ADD COLUMN branched_from TEXT;    -- 'message:<id>' | 'post:<id>'
CREATE INDEX sessions_channel ON sessions(channel_id, updated_at);
CREATE INDEX sessions_prev ON sessions(prev_session_id);

-- Who wrote a message, as a stable id that becomes a member id with
-- multiplayer (spec §15): NULL = the owner for user messages, the main
-- agent for assistant ones.
ALTER TABLE messages ADD COLUMN author_id TEXT;

CREATE TABLE posts (
  id INTEGER PRIMARY KEY,
  channel_id TEXT NOT NULL REFERENCES channels(id),
  session_id TEXT REFERENCES sessions(id),   -- the thread it came from; NULL for pointers
  run_id TEXT REFERENCES runs(id),
  author_kind TEXT NOT NULL,        -- user | agent | subagent | routine
  -- Stable identity: 'owner' (later a member id), 'tern', 'tern/<label>',
  -- 'routine:<schedule id>'. `author` is only what is shown.
  author_id TEXT NOT NULL,
  author TEXT NOT NULL,             -- 'you', 'tern', 'tern/research-1', 'reply-sweep'
  kind TEXT NOT NULL,               -- message | answer | update | needs_you | pointer
  text TEXT NOT NULL,
  reply_to INTEGER REFERENCES posts(id),
  points_to INTEGER REFERENCES posts(id),    -- kind = pointer: the post in another channel
  tainted INTEGER NOT NULL DEFAULT 0,        -- written by a run that read untrusted content (C3)
  dedupe_key TEXT UNIQUE,           -- e.g. answer:<run>, send:<run>
  created_at INTEGER NOT NULL
);
CREATE INDEX posts_channel ON posts(channel_id, id);
CREATE INDEX posts_reply ON posts(reply_to);
CREATE VIRTUAL TABLE posts_fts USING fts5(text, content='posts', content_rowid='id');
CREATE TRIGGER posts_ai AFTER INSERT ON posts BEGIN
  INSERT INTO posts_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER posts_ad AFTER DELETE ON posts BEGIN
  INSERT INTO posts_fts(posts_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
CREATE TRIGGER posts_au AFTER UPDATE ON posts BEGIN
  INSERT INTO posts_fts(posts_fts, rowid, text) VALUES ('delete', old.id, old.text);
  INSERT INTO posts_fts(rowid, text) VALUES (new.id, new.text);
END;

ALTER TABLE schedules ADD COLUMN channel_id TEXT REFERENCES channels(id);

-- Backfill: Home exists, and everything that was there before belongs to it.
INSERT INTO channels (id, slug, title, kind, created_by, created_at, updated_at)
VALUES ('home', 'home', 'Home', 'main', 'user',
        CAST(strftime('%s', 'now') AS INTEGER) * 1000,
        CAST(strftime('%s', 'now') AS INTEGER) * 1000);
-- Sub-agent sessions take their parent's channel; every parent is in Home.
UPDATE sessions SET channel_id = 'home';
UPDATE schedules SET channel_id = 'home';
-- The newest desktop conversation becomes Home's first segment. Every other
-- one stays as it is and shows up in History.
UPDATE sessions SET segment_reason = 'migrated'
WHERE id = (SELECT id FROM sessions WHERE source = 'api' ORDER BY updated_at DESC, rowid DESC LIMIT 1);
UPDATE channels SET main_session_id =
  (SELECT id FROM sessions WHERE segment_reason = 'migrated')
WHERE id = 'home';
