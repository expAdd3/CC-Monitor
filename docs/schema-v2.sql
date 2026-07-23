-- Normative logical schema for the new Rust/Tauri database.
-- During implementation this file is split into immutable, ordered SQLx
-- migrations. Only the desktop application runs those migrations.

PRAGMA foreign_keys = ON;

CREATE TABLE raw_events (
    -- Hook IDs are per-invocation ingestion UUIDs (prefer UUIDv7), generated
    -- once and reused across DB retries. Transcript IDs use file identity,
    -- line start offset, and a content fingerprint.
    id                  TEXT PRIMARY KEY NOT NULL,
    agent_kind          TEXT NOT NULL CHECK (agent_kind IN ('claude')),
    session_id          TEXT NOT NULL,
    source              TEXT NOT NULL
                        CHECK (source IN ('hook', 'transcript', 'recovery')),
    source_event        TEXT NOT NULL,
    occurred_at_ms      INTEGER NOT NULL,
    received_at_ms      INTEGER NOT NULL,
    sequence_no         INTEGER,
    -- Hook key: source + ingestion UUID. Transcript key: stable line identity.
    dedupe_key          TEXT NOT NULL UNIQUE,
    payload_version     INTEGER NOT NULL DEFAULT 1,
    payload_json        TEXT NOT NULL CHECK (json_valid(payload_json)),
    processed_at_ms     INTEGER,
    process_error       TEXT
) STRICT;

CREATE INDEX idx_raw_events_unprocessed
    ON raw_events(received_at_ms, id)
    WHERE processed_at_ms IS NULL;

CREATE INDEX idx_raw_events_session_time
    ON raw_events(agent_kind, session_id, occurred_at_ms, received_at_ms);

CREATE TABLE session_projection (
    agent_kind          TEXT NOT NULL CHECK (agent_kind IN ('claude')),
    session_id          TEXT NOT NULL,
    lifecycle           TEXT NOT NULL
                        CHECK (lifecycle IN ('active', 'ended')),
    turn_state          TEXT
                        CHECK (turn_state IN
                               ('running', 'waiting', 'needs_input', 'failed')),
    state_reason        TEXT NOT NULL,
    state_source        TEXT NOT NULL
                        CHECK (state_source IN ('hook', 'transcript', 'recovery')),
    confidence          TEXT NOT NULL
                        CHECK (confidence IN ('definitive', 'inferred')),
    revision            INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    project_name        TEXT,
    cwd                 TEXT,
    transcript_path     TEXT,
    client_bundle_id    TEXT,
    started_at_ms       INTEGER,
    changed_at_ms       INTEGER NOT NULL,
    last_observed_at_ms INTEGER NOT NULL,
    ended_at_ms         INTEGER,
    current_turn_id     TEXT,
    PRIMARY KEY (agent_kind, session_id)
) STRICT;

CREATE INDEX idx_session_projection_active
    ON session_projection(lifecycle, last_observed_at_ms DESC);

CREATE TABLE turns (
    id                  TEXT PRIMARY KEY NOT NULL,
    agent_kind          TEXT NOT NULL,
    session_id          TEXT NOT NULL,
    ordinal             INTEGER NOT NULL CHECK (ordinal > 0),
    state               TEXT NOT NULL
                        CHECK (state IN
                               ('running', 'waiting', 'needs_input', 'failed')),
    started_at_ms       INTEGER NOT NULL,
    changed_at_ms       INTEGER NOT NULL,
    finished_at_ms      INTEGER,
    UNIQUE (agent_kind, session_id, ordinal),
    FOREIGN KEY (agent_kind, session_id)
        REFERENCES session_projection(agent_kind, session_id)
        ON DELETE CASCADE
) STRICT;

CREATE TABLE usage_records (
    -- Dedupe identity is always scoped by agent_kind + session_id:
    -- both IDs => message_id + request_id; message only => message_id;
    -- request only => request_id; no IDs => one deterministic
    -- anonymous-latest slot per session.
    id                  TEXT PRIMARY KEY NOT NULL,
    agent_kind          TEXT NOT NULL,
    session_id          TEXT NOT NULL,
    transcript_path     TEXT NOT NULL,
    -- Normalized transcript-family-relative file identity + line-start offset.
    source_location     TEXT NOT NULL,
    request_id          TEXT,
    message_id          TEXT,
    model_id            TEXT NOT NULL,
    local_day           TEXT NOT NULL,
    input_tokens        INTEGER NOT NULL DEFAULT 0 CHECK (input_tokens >= 0),
    output_tokens       INTEGER NOT NULL DEFAULT 0 CHECK (output_tokens >= 0),
    cache_write_tokens  INTEGER NOT NULL DEFAULT 0
                        CHECK (cache_write_tokens >= 0),
    cache_read_tokens   INTEGER NOT NULL DEFAULT 0
                        CHECK (cache_read_tokens >= 0),
    cost_pico_usd       INTEGER NOT NULL DEFAULT 0 CHECK (cost_pico_usd >= 0),
    cost_known          INTEGER NOT NULL DEFAULT 1
                        CHECK (cost_known IN (0, 1)),
    dedupe_key          TEXT NOT NULL UNIQUE,
    observed_at_ms      INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_usage_records_session
    ON usage_records(agent_kind, session_id, local_day);

CREATE INDEX idx_usage_records_day_model
    ON usage_records(local_day, model_id);

CREATE TABLE daily_usage (
    local_day           TEXT NOT NULL,
    agent_kind          TEXT NOT NULL,
    session_id          TEXT NOT NULL,
    model_id            TEXT NOT NULL,
    input_tokens        INTEGER NOT NULL DEFAULT 0 CHECK (input_tokens >= 0),
    output_tokens       INTEGER NOT NULL DEFAULT 0 CHECK (output_tokens >= 0),
    cache_write_tokens  INTEGER NOT NULL DEFAULT 0
                        CHECK (cache_write_tokens >= 0),
    cache_read_tokens   INTEGER NOT NULL DEFAULT 0
                        CHECK (cache_read_tokens >= 0),
    cost_pico_usd       INTEGER NOT NULL DEFAULT 0 CHECK (cost_pico_usd >= 0),
    cost_known          INTEGER NOT NULL DEFAULT 1
                        CHECK (cost_known IN (0, 1)),
    updated_at_ms       INTEGER NOT NULL,
    PRIMARY KEY (local_day, agent_kind, session_id, model_id)
) STRICT;

CREATE TABLE transcript_cursors (
    transcript_path     TEXT PRIMARY KEY NOT NULL,
    file_identity       TEXT,
    byte_offset         INTEGER NOT NULL DEFAULT 0 CHECK (byte_offset >= 0),
    file_size           INTEGER NOT NULL DEFAULT 0 CHECK (file_size >= 0),
    modified_at_ms      INTEGER,
    partial_line        BLOB,
    last_scanned_at_ms  INTEGER NOT NULL,
    last_error          TEXT
) STRICT;

CREATE TABLE notification_outbox (
    id                  TEXT PRIMARY KEY NOT NULL,
    agent_kind          TEXT NOT NULL,
    session_id          TEXT NOT NULL,
    turn_id             TEXT,
    projection_revision INTEGER NOT NULL CHECK (projection_revision > 0),
    kind                TEXT NOT NULL
                        CHECK (kind IN ('needs_input', 'done', 'failed')),
    provider            TEXT NOT NULL CHECK (provider IN ('desktop', 'ntfy')),
    status              TEXT NOT NULL
                        CHECK (status IN
                               ('pending', 'inflight', 'sent', 'failed',
                                'suppressed')),
    title               TEXT NOT NULL,
    body                TEXT NOT NULL,
    created_at_ms       INTEGER NOT NULL,
    next_attempt_at_ms  INTEGER,
    attempt_count       INTEGER NOT NULL DEFAULT 0 CHECK (attempt_count >= 0),
    sent_at_ms          INTEGER,
    last_error          TEXT,
    UNIQUE (
        agent_kind,
        session_id,
        projection_revision,
        kind,
        provider
    )
) STRICT;

CREATE INDEX idx_notification_outbox_dispatch
    ON notification_outbox(status, next_attempt_at_ms, created_at_ms)
    WHERE status IN ('pending', 'failed');

CREATE TABLE settings (
    key                 TEXT PRIMARY KEY NOT NULL,
    value_json          TEXT NOT NULL CHECK (json_valid(value_json)),
    updated_at_ms       INTEGER NOT NULL
) STRICT;

CREATE TABLE price_overrides (
    model_id            TEXT PRIMARY KEY NOT NULL,
    input_pico_usd_per_million       INTEGER,
    output_pico_usd_per_million      INTEGER,
    cache_write_pico_usd_per_million INTEGER,
    cache_read_pico_usd_per_million  INTEGER,
    updated_at_ms       INTEGER NOT NULL
) STRICT;

CREATE TABLE installation (
    singleton           INTEGER PRIMARY KEY NOT NULL DEFAULT 1
                        CHECK (singleton = 1),
    installation_id     TEXT NOT NULL UNIQUE,
    hook_path           TEXT NOT NULL,
    installed_at_ms     INTEGER NOT NULL,
    hook_version        TEXT NOT NULL
) STRICT;
