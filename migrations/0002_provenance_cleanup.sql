-- Les contenus ingérés sont du contexte, jamais une source autonome de tâches.
-- Nettoie les engagements produits historiquement par l'ancien extracteur de fichiers.
DELETE FROM commitments
WHERE source_ref IN (SELECT source_ref FROM items WHERE source = 'files');

-- Les bases/caches applicatifs n'ont pas vocation à apparaître comme documents récents.
UPDATE items
SET status = 'removed'
WHERE source = 'files'
  AND (
    lower(path) LIKE '%.musicdb'
    OR lower(path) LIKE '%.sqlite'
    OR lower(path) LIKE '%.sqlite-wal'
    OR lower(path) LIKE '%.sqlite-shm'
    OR lower(path) LIKE '%.db'
    OR lower(path) LIKE '%.db-wal'
    OR lower(path) LIKE '%.db-shm'
  );

CREATE TABLE IF NOT EXISTS ignored_items (
  source TEXT NOT NULL,
  source_ref TEXT NOT NULL,
  ignored_at INTEGER NOT NULL,
  PRIMARY KEY (source, source_ref)
);

CREATE TABLE IF NOT EXISTS reorganize_plans (
  id TEXT PRIMARY KEY,
  plan TEXT NOT NULL,
  status TEXT NOT NULL DEFAULT 'pending',
  created_at INTEGER NOT NULL
);

ALTER TABLE actions_log ADD COLUMN session_id TEXT;
