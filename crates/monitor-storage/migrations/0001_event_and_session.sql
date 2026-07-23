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
    process_error TEXT
) STRICT;

CREATE INDEX idx_raw_events_unprocessed
    ON raw_events(received_at_ms, id)
    WHERE processed_at_ms IS NULL;
CREATE INDEX idx_raw_events_session_time
    ON raw_events(agent_kind, session_id, occurred_at_ms, received_at_ms);

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
