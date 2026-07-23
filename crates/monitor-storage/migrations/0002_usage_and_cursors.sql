CREATE TABLE usage_records (
    id TEXT PRIMARY KEY NOT NULL,
    agent_kind TEXT NOT NULL,
    session_id TEXT NOT NULL,
    transcript_path TEXT NOT NULL,
    source_location TEXT NOT NULL,
    request_id TEXT,
    message_id TEXT,
    model_id TEXT NOT NULL,
    local_day TEXT NOT NULL,
    input_tokens INTEGER NOT NULL DEFAULT 0 CHECK (input_tokens >= 0),
    output_tokens INTEGER NOT NULL DEFAULT 0 CHECK (output_tokens >= 0),
    cache_write_tokens INTEGER NOT NULL DEFAULT 0 CHECK (cache_write_tokens >= 0),
    cache_read_tokens INTEGER NOT NULL DEFAULT 0 CHECK (cache_read_tokens >= 0),
    cost_pico_usd INTEGER NOT NULL DEFAULT 0 CHECK (cost_pico_usd >= 0),
    cost_known INTEGER NOT NULL DEFAULT 1 CHECK (cost_known IN (0, 1)),
    dedupe_key TEXT NOT NULL UNIQUE,
    observed_at_ms INTEGER NOT NULL
) STRICT;

CREATE INDEX idx_usage_records_session
    ON usage_records(agent_kind, session_id, local_day);
CREATE INDEX idx_usage_records_day_model
    ON usage_records(local_day, model_id);

CREATE TABLE daily_usage (
    local_day TEXT NOT NULL,
    agent_kind TEXT NOT NULL,
    session_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    input_tokens INTEGER NOT NULL DEFAULT 0 CHECK (input_tokens >= 0),
    output_tokens INTEGER NOT NULL DEFAULT 0 CHECK (output_tokens >= 0),
    cache_write_tokens INTEGER NOT NULL DEFAULT 0 CHECK (cache_write_tokens >= 0),
    cache_read_tokens INTEGER NOT NULL DEFAULT 0 CHECK (cache_read_tokens >= 0),
    cost_pico_usd INTEGER NOT NULL DEFAULT 0 CHECK (cost_pico_usd >= 0),
    cost_known INTEGER NOT NULL DEFAULT 1 CHECK (cost_known IN (0, 1)),
    updated_at_ms INTEGER NOT NULL,
    PRIMARY KEY (local_day, agent_kind, session_id, model_id)
) STRICT;

CREATE TABLE transcript_cursors (
    transcript_path TEXT PRIMARY KEY NOT NULL,
    file_identity TEXT,
    byte_offset INTEGER NOT NULL DEFAULT 0 CHECK (byte_offset >= 0),
    file_size INTEGER NOT NULL DEFAULT 0 CHECK (file_size >= 0),
    modified_at_ms INTEGER,
    partial_line BLOB,
    last_scanned_at_ms INTEGER NOT NULL,
    last_error TEXT
) STRICT;
