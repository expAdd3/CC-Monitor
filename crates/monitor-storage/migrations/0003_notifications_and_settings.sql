CREATE TABLE notification_outbox (
    id TEXT PRIMARY KEY NOT NULL,
    agent_kind TEXT NOT NULL,
    session_id TEXT NOT NULL,
    turn_id TEXT,
    projection_revision INTEGER NOT NULL CHECK (projection_revision > 0),
    kind TEXT NOT NULL CHECK (kind IN ('needs_input', 'done', 'failed')),
    provider TEXT NOT NULL CHECK (provider IN ('desktop', 'ntfy')),
    status TEXT NOT NULL CHECK (status IN ('pending', 'inflight', 'sent', 'failed', 'suppressed')),
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL,
    next_attempt_at_ms INTEGER,
    attempt_count INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    sent_at_ms INTEGER,
    last_error TEXT,
    UNIQUE (agent_kind, session_id, projection_revision, kind, provider)
) STRICT;

CREATE INDEX idx_notification_outbox_dispatch
    ON notification_outbox(status, next_attempt_at_ms, created_at_ms)
    WHERE status IN ('pending', 'failed');

CREATE TABLE settings (
    key TEXT PRIMARY KEY NOT NULL,
    value_json TEXT NOT NULL CHECK (json_valid(value_json)),
    updated_at_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE price_overrides (
    model_id TEXT PRIMARY KEY NOT NULL,
    input_pico_usd_per_million INTEGER,
    output_pico_usd_per_million INTEGER,
    cache_write_pico_usd_per_million INTEGER,
    cache_read_pico_usd_per_million INTEGER,
    updated_at_ms INTEGER NOT NULL
) STRICT;

CREATE TABLE installation (
    singleton INTEGER PRIMARY KEY NOT NULL DEFAULT 1 CHECK (singleton = 1),
    installation_id TEXT NOT NULL UNIQUE,
    hook_path TEXT NOT NULL,
    installed_at_ms INTEGER NOT NULL,
    hook_version TEXT NOT NULL
) STRICT;
