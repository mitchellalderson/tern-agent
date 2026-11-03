-- Paired remote devices (deployment spec §6.2). Tokens are stored as
-- blake3 hashes; the token itself is shown once, at pairing.
CREATE TABLE devices (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  platform TEXT,
  scope TEXT NOT NULL,
  token_hash TEXT NOT NULL UNIQUE,
  created_at INTEGER NOT NULL,
  last_seen_at INTEGER,
  last_ip TEXT,
  revoked_at INTEGER,
  push_json TEXT
);

-- One-time codes from `tern devices pair`. Separate from the Telegram
-- codes, so a code only pairs what it was made for.
CREATE TABLE device_pairing_codes (
  code TEXT PRIMARY KEY,
  scope TEXT NOT NULL,
  name TEXT,
  expires_at INTEGER NOT NULL
);
