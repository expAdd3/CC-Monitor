CREATE TABLE background_task_health (
    task TEXT PRIMARY KEY NOT NULL
         CHECK (task IN (
             'incremental_index',
             'engine_processing',
             'engine_reconciliation'
         )),
    success_count INTEGER NOT NULL DEFAULT 0 CHECK (success_count >= 0),
    failure_count INTEGER NOT NULL DEFAULT 0 CHECK (failure_count >= 0),
    consecutive_failures INTEGER NOT NULL DEFAULT 0
                         CHECK (consecutive_failures >= 0),
    last_error_code TEXT,
    last_succeeded_at_ms INTEGER,
    last_failed_at_ms INTEGER,
    recovered_at_ms INTEGER
) STRICT;
