CREATE TABLE usage_session_aggregates (
    agent_kind TEXT NOT NULL CHECK (agent_kind IN ('claude')),
    session_id TEXT NOT NULL,
    input_tokens INTEGER NOT NULL CHECK (input_tokens >= 0),
    output_tokens INTEGER NOT NULL CHECK (output_tokens >= 0),
    cache_write_tokens INTEGER NOT NULL CHECK (cache_write_tokens >= 0),
    cache_read_tokens INTEGER NOT NULL CHECK (cache_read_tokens >= 0),
    cost_pico_usd INTEGER NOT NULL CHECK (cost_pico_usd >= 0),
    cost_known INTEGER NOT NULL CHECK (cost_known IN (0, 1)),
    unpriced_tokens INTEGER NOT NULL CHECK (unpriced_tokens >= 0),
    PRIMARY KEY (agent_kind, session_id)
) STRICT;

CREATE TABLE usage_model_aggregates (
    agent_kind TEXT NOT NULL CHECK (agent_kind IN ('claude')),
    session_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    input_tokens INTEGER NOT NULL CHECK (input_tokens >= 0),
    output_tokens INTEGER NOT NULL CHECK (output_tokens >= 0),
    cache_write_tokens INTEGER NOT NULL CHECK (cache_write_tokens >= 0),
    cache_read_tokens INTEGER NOT NULL CHECK (cache_read_tokens >= 0),
    cost_pico_usd INTEGER NOT NULL CHECK (cost_pico_usd >= 0),
    cost_known INTEGER NOT NULL CHECK (cost_known IN (0, 1)),
    unpriced_tokens INTEGER NOT NULL CHECK (unpriced_tokens >= 0),
    PRIMARY KEY (agent_kind, session_id, model_id)
) STRICT;

CREATE TABLE usage_daily_aggregates (
    agent_kind TEXT NOT NULL CHECK (agent_kind IN ('claude')),
    local_day TEXT NOT NULL,
    input_tokens INTEGER NOT NULL CHECK (input_tokens >= 0),
    output_tokens INTEGER NOT NULL CHECK (output_tokens >= 0),
    cache_write_tokens INTEGER NOT NULL CHECK (cache_write_tokens >= 0),
    cache_read_tokens INTEGER NOT NULL CHECK (cache_read_tokens >= 0),
    cost_pico_usd INTEGER NOT NULL CHECK (cost_pico_usd >= 0),
    cost_known INTEGER NOT NULL CHECK (cost_known IN (0, 1)),
    unpriced_tokens INTEGER NOT NULL CHECK (unpriced_tokens >= 0),
    PRIMARY KEY (agent_kind, local_day)
) STRICT;

CREATE TABLE usage_aggregate_state (
    singleton INTEGER PRIMARY KEY NOT NULL CHECK (singleton = 1),
    is_current INTEGER NOT NULL CHECK (is_current IN (0, 1))
) STRICT;

INSERT INTO usage_aggregate_state(singleton, is_current) VALUES (1, 0);

CREATE INDEX idx_usage_records_session_model_v2
    ON usage_records(agent_kind, session_id, model_id, id);
CREATE INDEX idx_usage_records_day_v2
    ON usage_records(agent_kind, local_day, id);
