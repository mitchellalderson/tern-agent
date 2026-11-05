-- Workspaces as data (docs/alpha-workspaces.md §3). Additive: every new
-- column is NULL for existing rows and NULL means "inherit", so a migrated
-- install resolves everything to the default workspace, exactly as before.

CREATE TABLE workspaces (
  id TEXT PRIMARY KEY,               -- 'default' | 'ws_<uuid7>'
  name TEXT NOT NULL,
  path TEXT NOT NULL,                -- canonical (realpath) at last validation, UTF-8
  dev INTEGER, ino INTEGER,          -- identity of the directory when it was chosen
  fs_flags TEXT,                     -- JSON {case_insensitive, normalizes_unicode}
  status TEXT NOT NULL DEFAULT 'ok', -- ok | missing | replaced | denied | invalid
  status_detail TEXT,
  network TEXT,                      -- NULL inherit | 'off' (only ever stricter than global)
  settings TEXT,                     -- JSON, namespaced; {"auto": {...}} is auto mode's, opaque here
  created_by TEXT NOT NULL,          -- 'migration' | 'user' | 'default'
  archived_at INTEGER,
  created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
);
CREATE UNIQUE INDEX workspaces_path ON workspaces(path) WHERE archived_at IS NULL;

-- Extra folders (or single files): read-only, added by the user.
CREATE TABLE workspace_roots (
  workspace_id TEXT NOT NULL REFERENCES workspaces(id),
  path TEXT NOT NULL, dev INTEGER, ino INTEGER,
  kind TEXT NOT NULL,                -- 'user' | 'git_dir'
  is_file INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (workspace_id, path)
);

ALTER TABLE channels  ADD COLUMN workspace_id TEXT REFERENCES workspaces(id);
ALTER TABLE sessions  ADD COLUMN workspace_id TEXT REFERENCES workspaces(id);
ALTER TABLE agents    ADD COLUMN workspace_id TEXT REFERENCES workspaces(id);
-- A routine's frozen binding (and the subdirectory it was narrowed to).
ALTER TABLE schedules ADD COLUMN workspace_id TEXT REFERENCES workspaces(id);
ALTER TABLE schedules ADD COLUMN workspace_subdir TEXT;
-- What a run actually had: no FK, history outlives an archived workspace.
ALTER TABLE runs ADD COLUMN workspace_id TEXT;
ALTER TABLE runs ADD COLUMN workspace_root TEXT;
ALTER TABLE runs ADD COLUMN workspace_subdir TEXT;
