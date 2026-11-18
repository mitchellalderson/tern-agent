-- Documents (docs/documents-design.md): a record per document, an append-only
-- revision log, and content-addressed text. The file in the workspace is the
-- user's copy; the latest revision is what the app shows.
CREATE TABLE document_blobs (
  hash TEXT PRIMARY KEY,                 -- sha256 hex of the content
  content TEXT NOT NULL
);

CREATE TABLE documents (
  id TEXT PRIMARY KEY,                   -- 'd_<uuid7>'
  kind TEXT NOT NULL,                    -- markdown | text | code | csv
  title TEXT NOT NULL,
  workspace_id TEXT NOT NULL,            -- the workspace the file lives in
  path TEXT NOT NULL,                    -- relative to that workspace
  current_rev INTEGER NOT NULL,
  content_hash TEXT NOT NULL,
  size INTEGER NOT NULL,
  tainted INTEGER NOT NULL DEFAULT 0,    -- any revision since the last review came from a tainted run
  agent_id TEXT,
  session_id TEXT,                       -- origin
  run_id TEXT,
  owner_user_id TEXT,                    -- teams seam: NULL reads as 'owner'
  visibility TEXT NOT NULL DEFAULT 'private',
  created_by TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL,
  deleted_at INTEGER
);
CREATE UNIQUE INDEX documents_path ON documents(workspace_id, path) WHERE deleted_at IS NULL;
CREATE INDEX documents_updated ON documents(updated_at DESC);
CREATE INDEX documents_session ON documents(session_id);

CREATE TABLE document_revisions (
  doc_id TEXT NOT NULL REFERENCES documents(id),
  rev INTEGER NOT NULL,
  parent_rev INTEGER,
  author_kind TEXT NOT NULL,             -- agent | user | external | system
  author_id TEXT NOT NULL,
  run_id TEXT,
  tool_call_id TEXT,
  blob_hash TEXT NOT NULL REFERENCES document_blobs(hash),
  size INTEGER NOT NULL,
  added INTEGER NOT NULL DEFAULT 0,      -- lines
  removed INTEGER NOT NULL DEFAULT 0,
  summary TEXT,
  tainted INTEGER NOT NULL DEFAULT 0,
  provenance TEXT,                       -- why it is tainted (the run's taint reason)
  rebased INTEGER NOT NULL DEFAULT 0,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (doc_id, rev)
);
