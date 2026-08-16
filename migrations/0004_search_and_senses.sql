-- 0004 — Recherche, mémoire longue et nouveaux sens.
-- 1) Résumé de long terme par session (les tours anciens sont condensés).
ALTER TABLE sessions ADD COLUMN summary TEXT;

-- 2) Rattachement des tâches à un rappel natif (EventKit Reminders).
ALTER TABLE tasks ADD COLUMN external_ref TEXT;

-- 3) Miroir agenda : une ligne par événement natif, dédupliquée par identifiant.
CREATE UNIQUE INDEX IF NOT EXISTS idx_events_source_ref
  ON events(source_ref) WHERE source_ref IS NOT NULL;

-- 4) Index alignés sur les requêtes réelles (audit 14/08/2026).
CREATE INDEX IF NOT EXISTS idx_items_source_status ON items(source, status, ingested_at DESC);
CREATE INDEX IF NOT EXISTS idx_items_mtime ON items(mtime DESC);
CREATE INDEX IF NOT EXISTS idx_embeddings_pending ON embeddings(item_id) WHERE vector IS NULL;
CREATE INDEX IF NOT EXISTS idx_actions_log_status ON actions_log(status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_actions_log_session ON actions_log(session_id);
CREATE INDEX IF NOT EXISTS idx_tasks_status_due ON tasks(status, due);
CREATE INDEX IF NOT EXISTS idx_tasks_external_ref ON tasks(external_ref);
CREATE INDEX IF NOT EXISTS idx_commitments_status_due ON commitments(status, due);
CREATE INDEX IF NOT EXISTS idx_commitments_source_text ON commitments(source_ref, text);
CREATE INDEX IF NOT EXISTS idx_proactive_log_reason ON proactive_log(reason, surfaced_at);
CREATE INDEX IF NOT EXISTS idx_access_log_created ON access_log(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_unknown_names_status ON unknown_names(status, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_person_links_person ON person_links(person_id);
CREATE INDEX IF NOT EXISTS idx_folders_status ON folders(status);
