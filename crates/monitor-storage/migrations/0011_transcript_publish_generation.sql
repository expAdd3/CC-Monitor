CREATE TABLE transcript_publish_generation (
    transcript_path TEXT PRIMARY KEY NOT NULL,
    generation INTEGER NOT NULL CHECK (generation > 0)
) STRICT;

CREATE INDEX idx_transcript_event_stage_cleanup
ON transcript_event_stage(staged_at_ms);

CREATE INDEX idx_transcript_usage_stage_cleanup
ON transcript_usage_stage(staged_at_ms);
