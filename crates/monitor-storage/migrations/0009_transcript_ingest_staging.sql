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
    PRIMARY KEY (ingest_id, id)
) STRICT;

CREATE INDEX idx_transcript_event_stage_path
ON transcript_event_stage(transcript_path);

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
    PRIMARY KEY (ingest_id, id)
) STRICT;

CREATE INDEX idx_transcript_usage_stage_path
ON transcript_usage_stage(transcript_path);
