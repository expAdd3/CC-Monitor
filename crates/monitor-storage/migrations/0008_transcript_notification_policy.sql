ALTER TABLE raw_events
ADD COLUMN notifications_allowed INTEGER NOT NULL DEFAULT 1
CHECK (notifications_allowed IN (0, 1));
