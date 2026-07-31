ALTER TABLE raw_events ADD COLUMN transcript_path TEXT;

CREATE INDEX idx_raw_events_transcript_path
    ON raw_events(transcript_path)
    WHERE source = 'transcript';
