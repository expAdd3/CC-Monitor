CREATE TABLE notification_provider_health (
    provider TEXT PRIMARY KEY NOT NULL CHECK (provider IN ('desktop', 'ntfy')),
    consecutive_failures INTEGER NOT NULL DEFAULT 0 CHECK (consecutive_failures >= 0),
    last_error TEXT,
    last_failed_at_ms INTEGER,
    recovered_at_ms INTEGER
) STRICT;
