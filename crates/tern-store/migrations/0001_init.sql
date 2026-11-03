CREATE TABLE sessions (
  id TEXT PRIMARY KEY, title TEXT, source TEXT NOT NULL,
  external_ref TEXT, model TEXT,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);
CREATE INDEX sessions_source_ref ON sessions(source, external_ref);

CREATE TABLE runs (
  id TEXT PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id),
  status TEXT NOT NULL,            -- running | completed | failed | interrupted
  trigger TEXT NOT NULL,           -- user | cron | resume
  started_at INTEGER NOT NULL, updated_at INTEGER NOT NULL,
  finished_at INTEGER, error TEXT
);
CREATE INDEX runs_session ON runs(session_id);
CREATE INDEX runs_status ON runs(status);

CREATE TABLE steps (
  run_id TEXT NOT NULL REFERENCES runs(id), idx INTEGER NOT NULL,
  kind TEXT NOT NULL, input_hash TEXT NOT NULL,
  input_json TEXT NOT NULL, output_json TEXT,
  status TEXT NOT NULL,            -- started | completed | failed
  usage_json TEXT, error TEXT,
  started_at INTEGER NOT NULL, finished_at INTEGER,
  PRIMARY KEY (run_id, idx)
);

CREATE TABLE messages (
  id INTEGER PRIMARY KEY, session_id TEXT NOT NULL REFERENCES sessions(id),
  run_id TEXT, role TEXT NOT NULL, content_json TEXT NOT NULL,
  text TEXT NOT NULL,              -- plain text for FTS
  created_at INTEGER NOT NULL
);
CREATE INDEX messages_session ON messages(session_id, id);
CREATE VIRTUAL TABLE messages_fts USING fts5(text, content='messages', content_rowid='id');
CREATE TRIGGER messages_ai AFTER INSERT ON messages BEGIN
  INSERT INTO messages_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER messages_ad AFTER DELETE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;
CREATE TRIGGER messages_au AFTER UPDATE ON messages BEGIN
  INSERT INTO messages_fts(messages_fts, rowid, text) VALUES ('delete', old.id, old.text);
  INSERT INTO messages_fts(rowid, text) VALUES (new.id, new.text);
END;

CREATE TABLE memories (
  id INTEGER PRIMARY KEY, content TEXT NOT NULL, tags TEXT,
  created_at INTEGER NOT NULL
);
CREATE VIRTUAL TABLE memories_fts USING fts5(content, tags, content='memories', content_rowid='id');
CREATE TRIGGER memories_ai AFTER INSERT ON memories BEGIN
  INSERT INTO memories_fts(rowid, content, tags) VALUES (new.id, new.content, new.tags);
END;
CREATE TRIGGER memories_ad AFTER DELETE ON memories BEGIN
  INSERT INTO memories_fts(memories_fts, rowid, content, tags) VALUES ('delete', old.id, old.content, old.tags);
END;
CREATE TRIGGER memories_au AFTER UPDATE ON memories BEGIN
  INSERT INTO memories_fts(memories_fts, rowid, content, tags) VALUES ('delete', old.id, old.content, old.tags);
  INSERT INTO memories_fts(rowid, content, tags) VALUES (new.id, new.content, new.tags);
END;

CREATE TABLE schedules (
  id TEXT PRIMARY KEY, cron TEXT NOT NULL, prompt TEXT NOT NULL,
  session_id TEXT NOT NULL, deliver_to TEXT,
  enabled INTEGER NOT NULL DEFAULT 1,
  next_run_at INTEGER NOT NULL, last_run_at INTEGER
);
CREATE INDEX schedules_due ON schedules(enabled, next_run_at);

CREATE TABLE kv (
  namespace TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL,
  updated_at INTEGER NOT NULL, PRIMARY KEY (namespace, key)
);

CREATE TABLE approval_allowlist (
  id INTEGER PRIMARY KEY, tool TEXT NOT NULL, pattern TEXT NOT NULL,
  created_at INTEGER NOT NULL
);

CREATE TABLE telegram_pairings (
  user_id INTEGER PRIMARY KEY, paired_at INTEGER NOT NULL
);
