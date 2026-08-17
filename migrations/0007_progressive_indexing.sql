-- File persistante : le catalogue/FTS est disponible indépendamment de la
-- couverture sémantique, qui converge ensuite en arrière-plan.
CREATE TABLE IF NOT EXISTS enrichment_queue (
  item_id            TEXT PRIMARY KEY,
  source             TEXT NOT NULL,
  source_ref         TEXT NOT NULL,
  state              TEXT NOT NULL DEFAULT 'pending',
  base_priority      REAL NOT NULL DEFAULT 0,
  access_count       INTEGER NOT NULL DEFAULT 0,
  last_accessed      INTEGER,
  lexical_ready      INTEGER NOT NULL DEFAULT 0,
  embedding_ready    INTEGER NOT NULL DEFAULT 0,
  extractor_version  TEXT,
  attempts           INTEGER NOT NULL DEFAULT 0,
  last_error         TEXT,
  updated_at         INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_enrichment_pending
  ON enrichment_queue(state, base_priority DESC, last_accessed DESC);
CREATE INDEX IF NOT EXISTS idx_enrichment_source
  ON enrichment_queue(source, state);

-- Curseurs opaques fournis par les APIs. Les compteurs constituent une preuve
-- testable qu'un redémarrage utilise les deltas et non un re-listing complet.
CREATE TABLE IF NOT EXISTS connector_cursors (
  provider        TEXT NOT NULL,
  resource        TEXT NOT NULL,
  cursor          TEXT NOT NULL,
  updated_at      INTEGER NOT NULL,
  full_sync_count INTEGER NOT NULL DEFAULT 0,
  delta_sync_count INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY(provider, resource)
);

-- Reprise macOS et instrumentation du fallback catalogue.
CREATE TABLE IF NOT EXISTS fs_journal_state (
  root              TEXT PRIMARY KEY,
  last_event_id     INTEGER NOT NULL DEFAULT 0,
  history_valid     INTEGER NOT NULL DEFAULT 1,
  replay_count      INTEGER NOT NULL DEFAULT 0,
  replayed_events   INTEGER NOT NULL DEFAULT 0,
  fallback_count    INTEGER NOT NULL DEFAULT 0,
  full_scan_count   INTEGER NOT NULL DEFAULT 0,
  updated_at        INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS index_metric_log (
  id               INTEGER PRIMARY KEY AUTOINCREMENT,
  recorded_at      INTEGER NOT NULL,
  eligible_count   INTEGER NOT NULL,
  embedded_count   INTEGER NOT NULL,
  lexical_count    INTEGER NOT NULL,
  coverage_pct     REAL NOT NULL,
  high_water_pct   REAL NOT NULL,
  reason           TEXT NOT NULL
);
