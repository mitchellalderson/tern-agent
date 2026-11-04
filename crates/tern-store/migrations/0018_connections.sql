-- Connections: authenticated accounts of a connector or MCP server
-- (docs/personal-agent-tooling-spec.md §6.8). Self-contained, so a merge can
-- renumber it. A target with no rows has one implicit `default` connection
-- using its plain secret names, so nothing existing changes.
CREATE TABLE connections (
    id            TEXT PRIMARY KEY,
    kind          TEXT NOT NULL,              -- 'connector' | 'mcp'
    target        TEXT NOT NULL,              -- the connector's or server's name
    slug          TEXT NOT NULL,              -- [a-z0-9_-]{1,16}, unique per target
    label         TEXT,                       -- the user's name for it ("work")
    account       TEXT,                       -- who signed in (email, login)
    auth_method   TEXT NOT NULL,              -- 'oauth' | 'key' | 'none'
    auth          TEXT,                       -- JSON: OAuth endpoints and client id, or key names; never a secret
    is_default    INTEGER NOT NULL DEFAULT 0,
    status        TEXT NOT NULL DEFAULT 'ok', -- 'ok' | 'needs_reauth' | 'error'
    last_error    TEXT,
    -- Multiplayer later (spec §15.4): whose account this is.
    owner_member  TEXT NOT NULL DEFAULT 'owner',
    created_at    INTEGER NOT NULL,
    last_used_at  INTEGER,
    UNIQUE (kind, target, slug)
);
CREATE INDEX connections_target ON connections (kind, target);
