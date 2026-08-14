-- Readable reference for the CC Monitor v1 SQLite schema.
-- The immutable file in crates/monitor-storage/migrations is authoritative.
-- Only the desktop application runs migrations.

PRAGMA foreign_keys = ON;

CREATE TABLE raw_events (
    id TEXT PRIMARY KEY NOT NULL,
    agent_kind TEXT NOT NULL CHECK (agent_kind IN ('claude')),
    session_id TEXT NOT NULL,
    source TEXT NOT NULL CHECK (source IN ('hook', 'transcript', 'recovery')),
    source_event TEXT NOT NULL,
    occurred_at_ms INTEGER NOT NULL,
    received_at_ms INTEGER NOT NULL,
    sequence_no INTEGER,
    dedupe_key TEXT NOT NULL UNIQUE,
    payload_version INTEGER NOT NULL DEFAULT 1,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    processed_at_ms INTEGER,
    process_error TEXT,
    transcript_path TEXT,
    notifications_allowed INTEGER NOT NULL DEFAULT 1
        CHECK (notifications_allowed IN (0, 1))
) STRICT;

CREATE INDEX idx_raw_events_unprocessed
    ON raw_events(received_at_ms, id) WHERE processed_at_ms IS NULL;
CREATE INDEX idx_raw_events_session_time
    ON raw_events(agent_kind, session_id, occurred_at_ms, received_at_ms);
CREATE INDEX idx_raw_events_transcript_path
    ON raw_events(transcript_path) WHERE source = 'transcript';

CREATE TABLE session_projection (
    agent_kind TEXT NOT NULL CHECK (agent_kind IN ('claude')),
    session_id TEXT NOT NULL,
    lifecycle TEXT NOT NULL CHECK (lifecycle IN ('active', 'ended')),
    turn_state TEXT CHECK (turn_state IN ('running', 'waiting', 'needs_input', 'failed')),
    state_reason TEXT NOT NULL,
    state_source TEXT NOT NULL CHECK (state_source IN ('hook', 'transcript', 'recovery')),
    confidence TEXT NOT NULL CHECK (confidence IN ('definitive', 'inferred')),
    revision INTEGER NOT NULL DEFAULT 1 CHECK (revision > 0),
    project_name TEXT,
    cwd TEXT,
    transcript_path TEXT,
    client_bundle_id TEXT,
    started_at_ms INTEGER,
    changed_at_ms INTEGER NOT NULL,
    last_observed_at_ms INTEGER NOT NULL,
    ended_at_ms INTEGER,
    current_turn_id TEXT,
    PRIMARY KEY (agent_kind, session_id)
) STRICT;

CREATE INDEX idx_session_projection_active
    ON session_projection(lifecycle, last_observed_at_ms DESC);

CREATE TABLE turns (
    id TEXT PRIMARY KEY NOT NULL,
    agent_kind TEXT NOT NULL,
    session_id TEXT NOT NULL,
    ordinal INTEGER NOT NULL CHECK (ordinal > 0),
    state TEXT NOT NULL CHECK (state IN ('running', 'waiting', 'needs_input', 'failed')),
    started_at_ms INTEGER NOT NULL,
    changed_at_ms INTEGER NOT NULL,
    finished_at_ms INTEGER,
    UNIQUE (agent_kind, session_id, ordinal),
    FOREIGN KEY (agent_kind, session_id)
        REFERENCES session_projection(agent_kind, session_id) ON DELETE CASCADE
) STRICT;

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
    observed_at_ms INTEGER NOT NULL,
    is_sidechain INTEGER NOT NULL DEFAULT 0 CHECK (is_sidechain IN (0, 1)),
    final_message INTEGER NOT NULL DEFAULT 0 CHECK (final_message IN (0, 1))
) STRICT;

CREATE INDEX idx_usage_records_session
    ON usage_records(agent_kind, session_id, local_day);
CREATE INDEX idx_usage_records_day_model
    ON usage_records(local_day, model_id);

CREATE TABLE transcript_cursors (
    transcript_path TEXT PRIMARY KEY NOT NULL,
    file_identity TEXT,
    byte_offset INTEGER NOT NULL DEFAULT 0 CHECK (byte_offset >= 0),
    file_size INTEGER NOT NULL DEFAULT 0 CHECK (file_size >= 0),
    modified_at_ms INTEGER,
    partial_line BLOB,
    last_scanned_at_ms INTEGER NOT NULL,
    last_error TEXT,
    content_anchor TEXT
) STRICT;

CREATE TABLE notification_outbox (
    id TEXT PRIMARY KEY NOT NULL,
    agent_kind TEXT NOT NULL,
    session_id TEXT NOT NULL,
    turn_id TEXT,
    projection_revision INTEGER NOT NULL CHECK (projection_revision > 0),
    kind TEXT NOT NULL CHECK (kind IN ('needs_input', 'done', 'failed')),
    provider TEXT NOT NULL CHECK (provider IN ('desktop', 'ntfy')),
    status TEXT NOT NULL
        CHECK (status IN ('pending', 'inflight', 'sent', 'failed', 'suppressed')),
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
    updated_at_ms INTEGER NOT NULL,
    disabled INTEGER NOT NULL DEFAULT 0 CHECK (disabled IN (0, 1))
) STRICT;

CREATE TABLE installation (
    singleton INTEGER PRIMARY KEY NOT NULL DEFAULT 1 CHECK (singleton = 1),
    installation_id TEXT NOT NULL UNIQUE,
    hook_path TEXT NOT NULL,
    installed_at_ms INTEGER NOT NULL,
    hook_version TEXT NOT NULL
) STRICT;

CREATE TABLE notification_provider_health (
    provider TEXT PRIMARY KEY NOT NULL CHECK (provider IN ('desktop', 'ntfy')),
    consecutive_failures INTEGER NOT NULL DEFAULT 0 CHECK (consecutive_failures >= 0),
    last_error TEXT,
    last_failed_at_ms INTEGER,
    recovered_at_ms INTEGER
) STRICT;

CREATE TABLE transcript_event_stage (
    ingest_id TEXT NOT NULL,
    transcript_path TEXT NOT NULL,
    id TEXT NOT NULL,
    agent_kind TEXT NOT NULL,
    session_id TEXT NOT NULL,
    source_event TEXT NOT NULL,
    occurred_at_ms INTEGER NOT NULL,
    received_at_ms INTEGER NOT NULL,
    sequence_no INTEGER,
    dedupe_key TEXT NOT NULL,
    payload_version INTEGER NOT NULL,
    payload_json TEXT NOT NULL CHECK (json_valid(payload_json)),
    notifications_allowed INTEGER NOT NULL CHECK (notifications_allowed IN (0, 1)),
    staged_at_ms INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (ingest_id, id)
) STRICT;

CREATE INDEX idx_transcript_event_stage_path
    ON transcript_event_stage(transcript_path);
CREATE INDEX idx_transcript_event_stage_cleanup
    ON transcript_event_stage(staged_at_ms);

CREATE TABLE transcript_usage_stage (
    ingest_id TEXT NOT NULL,
    transcript_path TEXT NOT NULL,
    id TEXT NOT NULL,
    session_id TEXT NOT NULL,
    source_location TEXT NOT NULL,
    request_id TEXT,
    message_id TEXT,
    model_id TEXT NOT NULL,
    local_day TEXT NOT NULL,
    input_tokens INTEGER NOT NULL CHECK (input_tokens >= 0),
    output_tokens INTEGER NOT NULL CHECK (output_tokens >= 0),
    cache_write_tokens INTEGER NOT NULL CHECK (cache_write_tokens >= 0),
    cache_read_tokens INTEGER NOT NULL CHECK (cache_read_tokens >= 0),
    cost_pico_usd INTEGER NOT NULL CHECK (cost_pico_usd >= 0),
    cost_known INTEGER NOT NULL CHECK (cost_known IN (0, 1)),
    dedupe_key TEXT NOT NULL,
    observed_at_ms INTEGER NOT NULL,
    staged_at_ms INTEGER NOT NULL DEFAULT 0,
    is_sidechain INTEGER NOT NULL DEFAULT 0 CHECK (is_sidechain IN (0, 1)),
    final_message INTEGER NOT NULL DEFAULT 0 CHECK (final_message IN (0, 1)),
    PRIMARY KEY (ingest_id, id)
) STRICT;

CREATE INDEX idx_transcript_usage_stage_path
    ON transcript_usage_stage(transcript_path);
CREATE INDEX idx_transcript_usage_stage_cleanup
    ON transcript_usage_stage(staged_at_ms);

CREATE TABLE background_task_health (
    task TEXT PRIMARY KEY NOT NULL CHECK (task IN (
        'incremental_index',
        'engine_processing',
        'engine_reconciliation',
        'startup_reconciliation',
        'retention_cleanup'
    )),
    success_count INTEGER NOT NULL DEFAULT 0 CHECK (success_count >= 0),
    failure_count INTEGER NOT NULL DEFAULT 0 CHECK (failure_count >= 0),
    consecutive_failures INTEGER NOT NULL DEFAULT 0 CHECK (consecutive_failures >= 0),
    last_error_code TEXT,
    last_succeeded_at_ms INTEGER,
    last_failed_at_ms INTEGER,
    recovered_at_ms INTEGER,
    last_transition_at_ms INTEGER
) STRICT;
