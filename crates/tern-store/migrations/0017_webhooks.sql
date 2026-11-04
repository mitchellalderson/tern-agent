-- T1c: webhook triggers (docs/personal-agent-tooling-spec.md §3.1, W6). A
-- webhook routine is a `schedules` row with `trigger_kind = 'webhook'`; its
-- secret lives here, out of every `schedules` read. Self-contained, so a
-- merge can renumber it.

CREATE TABLE routine_webhooks (
  routine_id TEXT PRIMARY KEY REFERENCES schedules(id) ON DELETE CASCADE,
  -- `hmac` (a signature over the body) or `bearer` (the secret itself in
  -- `Authorization`).
  auth TEXT NOT NULL,
  -- `tern_whk_…`. Kept as is, not hashed: it is the HMAC key. NULL once
  -- revoked, so the URL stops working until it is rotated again.
  secret TEXT,
  created_at INTEGER NOT NULL,
  rotated_at INTEGER,
  revoked_at INTEGER,
  -- The last delivery that passed its signature check.
  last_delivery_at INTEGER
);
