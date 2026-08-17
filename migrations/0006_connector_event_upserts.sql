-- Les UPSERT Google/Microsoft ciblent `source_ref`. L'ancien index était
-- partiel (`WHERE source_ref IS NOT NULL`) et SQLite ne pouvait donc pas
-- l'inférer avec `ON CONFLICT(source_ref)`.
DELETE FROM events
WHERE source_ref IS NOT NULL
  AND rowid NOT IN (
    SELECT MAX(rowid) FROM events
    WHERE source_ref IS NOT NULL
    GROUP BY source_ref
  );

DROP INDEX IF EXISTS idx_events_source_ref;
CREATE UNIQUE INDEX idx_events_source_ref ON events(source_ref);
