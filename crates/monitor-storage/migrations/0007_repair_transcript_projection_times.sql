UPDATE session_projection
SET
    last_observed_at_ms = (
        SELECT MAX(e.occurred_at_ms)
        FROM raw_events e
        WHERE e.agent_kind = session_projection.agent_kind
          AND e.session_id = session_projection.session_id
          AND e.source = 'transcript'
          AND e.occurred_at_ms > 0
    ),
    changed_at_ms = CASE
        WHEN state_reason = 'transcript_idle' THEN (
            SELECT MAX(e.occurred_at_ms) + 30001
            FROM raw_events e
            WHERE e.agent_kind = session_projection.agent_kind
              AND e.session_id = session_projection.session_id
              AND e.source = 'transcript'
              AND e.occurred_at_ms > 0
        )
        ELSE (
            SELECT MAX(e.occurred_at_ms)
            FROM raw_events e
            WHERE e.agent_kind = session_projection.agent_kind
              AND e.session_id = session_projection.session_id
              AND e.source = 'transcript'
              AND e.occurred_at_ms > 0
        )
    END
WHERE state_source = 'transcript'
  AND EXISTS (
      SELECT 1
      FROM raw_events e
      WHERE e.agent_kind = session_projection.agent_kind
        AND e.session_id = session_projection.session_id
        AND e.source = 'transcript'
        AND e.occurred_at_ms > 0
  );
