-- Where a run's final answer is sent when it finishes (e.g. telegram:<chat_id>),
-- so a run resumed after a crash still delivers.
ALTER TABLE runs ADD COLUMN deliver_to TEXT;

-- One-time codes from `tern gateway pair`.
CREATE TABLE pairing_codes (
  code TEXT PRIMARY KEY, expires_at INTEGER NOT NULL
);

-- Chat session lookup for gateways.
CREATE INDEX sessions_ref_updated ON sessions(source, external_ref, updated_at);
