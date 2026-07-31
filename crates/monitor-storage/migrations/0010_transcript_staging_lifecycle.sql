ALTER TABLE transcript_event_stage
ADD COLUMN staged_at_ms INTEGER NOT NULL DEFAULT 0;

ALTER TABLE transcript_usage_stage
ADD COLUMN staged_at_ms INTEGER NOT NULL DEFAULT 0;

ALTER TABLE transcript_usage_stage
ADD COLUMN is_sidechain INTEGER NOT NULL DEFAULT 0
CHECK (is_sidechain IN (0, 1));

ALTER TABLE transcript_usage_stage
ADD COLUMN final_message INTEGER NOT NULL DEFAULT 0
CHECK (final_message IN (0, 1));

ALTER TABLE usage_records
ADD COLUMN is_sidechain INTEGER NOT NULL DEFAULT 0
CHECK (is_sidechain IN (0, 1));

ALTER TABLE usage_records
ADD COLUMN final_message INTEGER NOT NULL DEFAULT 0
CHECK (final_message IN (0, 1));
