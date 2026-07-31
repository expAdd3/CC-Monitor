ALTER TABLE background_task_health RENAME TO background_task_health_v1;

CREATE TABLE background_task_health (
    task TEXT PRIMARY KEY NOT NULL
         CHECK (task IN (
             'incremental_index',
             'engine_processing',
             'engine_reconciliation',
             'startup_reconciliation',
             'retention_cleanup'
         )),
    success_count INTEGER NOT NULL DEFAULT 0 CHECK (success_count >= 0),
    failure_count INTEGER NOT NULL DEFAULT 0 CHECK (failure_count >= 0),
    consecutive_failures INTEGER NOT NULL DEFAULT 0
                         CHECK (consecutive_failures >= 0),
    last_error_code TEXT,
    last_succeeded_at_ms INTEGER,
    last_failed_at_ms INTEGER,
    recovered_at_ms INTEGER,
    last_transition_at_ms INTEGER
) STRICT;

INSERT INTO background_task_health (
    task, success_count, failure_count, consecutive_failures, last_error_code,
    last_succeeded_at_ms, last_failed_at_ms, recovered_at_ms
)
SELECT
    task, success_count, failure_count, consecutive_failures, last_error_code,
    last_succeeded_at_ms, last_failed_at_ms, recovered_at_ms
FROM background_task_health_v1;

DROP TABLE background_task_health_v1;
