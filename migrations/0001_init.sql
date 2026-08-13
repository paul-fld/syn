-- Syn — schéma consolidé (doc maître §8, normatif : ne pas renommer les tables)
-- L'index est dérivé : la vérité est dans les fichiers/comptes → reconstructible.

CREATE TABLE IF NOT EXISTS items (
  id          TEXT PRIMARY KEY,
  source      TEXT NOT NULL,             -- files | mail | calendar | system | people | screen | conversation
  source_ref  TEXT NOT NULL,             -- chemin de fichier, id fournisseur, URI locale…
  type        TEXT NOT NULL,             -- document | email | photo | code_project | note | fact…
  title       TEXT,
  body        TEXT,
  created_at  INTEGER,
  ingested_at INTEGER NOT NULL,
  hash        TEXT,
  path        TEXT,
  mime        TEXT,
  size        INTEGER,
  mtime       INTEGER,
  status      TEXT NOT NULL DEFAULT 'active'  -- active | removed (suppression FS : on ne casse pas les citations)
);
CREATE INDEX IF NOT EXISTS idx_items_source_ref ON items(source, source_ref);
CREATE INDEX IF NOT EXISTS idx_items_path ON items(path);
CREATE INDEX IF NOT EXISTS idx_items_type ON items(type);

CREATE TABLE IF NOT EXISTS embeddings (
  item_id     TEXT NOT NULL,
  model       TEXT NOT NULL,
  chunk_index INTEGER NOT NULL DEFAULT 0,
  text        TEXT NOT NULL,             -- le fragment embeddé (pour l'assemblage sourcé)
  vector      BLOB,                      -- f32 little-endian ; NULL = embedding en attente (mode dégradé)
  PRIMARY KEY (item_id, model, chunk_index)
);

CREATE TABLE IF NOT EXISTS events (
  id         TEXT PRIMARY KEY,
  source     TEXT NOT NULL,
  source_ref TEXT,
  title      TEXT NOT NULL,
  "start"    INTEGER NOT NULL,           -- UTC epoch (fuseaux normalisés en stockage)
  "end"      INTEGER,
  location   TEXT,
  attendees  TEXT,                       -- JSON [] — présence d'invités ⇒ plancher à la création
  notes      TEXT                        -- donnée non fiable (vecteur d'injection)
);
CREATE INDEX IF NOT EXISTS idx_events_start ON events("start");

CREATE TABLE IF NOT EXISTS tasks (
  id         TEXT PRIMARY KEY,
  title      TEXT NOT NULL,
  due        INTEGER,
  status     TEXT NOT NULL DEFAULT 'open',   -- open | done | dropped
  priority   TEXT,
  project    TEXT,
  source     TEXT,
  source_ref TEXT
);

CREATE TABLE IF NOT EXISTS commitments (
  id         TEXT PRIMARY KEY,
  text       TEXT NOT NULL,
  person_id  TEXT,
  direction  TEXT,                        -- owed_by_me | owed_to_me
  due        INTEGER,
  status     TEXT NOT NULL DEFAULT 'open',
  source_ref TEXT
);

CREATE TABLE IF NOT EXISTS people (
  id               TEXT PRIMARY KEY,
  name             TEXT NOT NULL,
  aliases          TEXT,                  -- JSON []
  relationship     TEXT,
  notes            TEXT,
  last_interaction INTEGER,
  face_embeddings  BLOB,                  -- [V2] biométrie — locale, chiffrée, jamais transmise
  comm_channels    TEXT,                  -- JSON { emails: [], phones: [] }
  consent_flags    TEXT,
  birthday         TEXT                   -- 'MM-DD' ou 'YYYY-MM-DD'
);

CREATE TABLE IF NOT EXISTS person_links (
  item_id   TEXT NOT NULL,
  person_id TEXT NOT NULL,
  PRIMARY KEY (item_id, person_id)
);

CREATE TABLE IF NOT EXISTS conversations (
  session_id TEXT NOT NULL,
  turn       INTEGER NOT NULL,
  role       TEXT NOT NULL,               -- user | assistant | tool
  content    TEXT NOT NULL,
  created_at INTEGER NOT NULL,
  PRIMARY KEY (session_id, turn)
);

CREATE TABLE IF NOT EXISTS sessions (
  id         TEXT PRIMARY KEY,
  title      TEXT,
  created_at INTEGER NOT NULL,
  updated_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS actions_log (
  id         TEXT PRIMARY KEY,
  tool       TEXT NOT NULL,
  input      TEXT NOT NULL,               -- JSON des arguments
  risk_class TEXT NOT NULL,               -- read | reversible_local | reversible_external | floor
  status     TEXT NOT NULL,               -- awaiting_confirmation | executed | rejected | undone | failed
  preview    TEXT,                        -- ce que l'utilisateur voit avant de confirmer
  result     TEXT,
  undo_data  TEXT,                        -- JSON pour annulation
  created_at INTEGER NOT NULL,
  derived_from_untrusted INTEGER NOT NULL DEFAULT 0  -- suspicion renforcée (Sécurité §3.4)
);

CREATE TABLE IF NOT EXISTS access_log (
  id         TEXT PRIMARY KEY,
  connector  TEXT NOT NULL,
  operation  TEXT NOT NULL,
  item_ref   TEXT,
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS connectors (
  id        TEXT PRIMARY KEY,
  type      TEXT NOT NULL,                -- files | mail_native | google | microsoft | apple | slack | github | system | people | screen
  status    TEXT NOT NULL,                -- connected | disconnected | needs_reauth | needs_permission | unavailable
  config    TEXT,                         -- chiffré au niveau base (SQLCipher)
  scopes    TEXT,
  last_sync INTEGER
);

CREATE TABLE IF NOT EXISTS settings (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS rules (
  id         TEXT PRIMARY KEY,
  text       TEXT NOT NULL,
  kind       TEXT,                        -- style | standing | action_modifier
  status     TEXT NOT NULL,               -- active | refused | conflict
  priority   INTEGER NOT NULL DEFAULT 0,
  params     TEXT,                        -- extraction structurée (profil de voix…)
  reason     TEXT,                        -- explication en cas de refus/conflit
  created_at INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS llm_profiles (
  model         TEXT PRIMARY KEY,
  prompt_format TEXT NOT NULL,
  params        TEXT
);

-- Périmètre du connecteur Files (moindre privilège : uniquement les dossiers choisis)
CREATE TABLE IF NOT EXISTS folders (
  path     TEXT PRIMARY KEY,
  added_at INTEGER NOT NULL,
  status   TEXT NOT NULL DEFAULT 'active',
  last_indexed INTEGER
);

-- Déclencheurs de proactivité (doc Proactivité §2) ; source=rule ⇐ Règles « tâche de fond »
CREATE TABLE IF NOT EXISTS triggers (
  id              TEXT PRIMARY KEY,
  type            TEXT NOT NULL,          -- time | event | threshold | context
  condition       TEXT NOT NULL,
  priority        TEXT NOT NULL,          -- urgent | important | info
  reason_template TEXT NOT NULL,
  action          TEXT NOT NULL,          -- notify | brief | suggest
  source          TEXT NOT NULL,          -- system | rule
  rule_id         TEXT,
  enabled         INTEGER NOT NULL DEFAULT 1,
  last_fired      INTEGER
);

-- Journal des surfaçages (budget de rareté + anti-répétition)
CREATE TABLE IF NOT EXISTS proactive_log (
  id          TEXT PRIMARY KEY,
  trigger_id  TEXT,
  kind        TEXT NOT NULL,
  reason      TEXT NOT NULL,              -- explicabilité : « Syn a vu X et Y, donc Z »
  body        TEXT,
  priority    TEXT NOT NULL,
  surfaced_at INTEGER NOT NULL,
  dismissed   INTEGER NOT NULL DEFAULT 0
);

-- Apprentissage progressif : inconnus en file (demande groupée, jamais intrusive)
CREATE TABLE IF NOT EXISTS unknown_names (
  id         TEXT PRIMARY KEY,
  name       TEXT NOT NULL,
  context    TEXT,
  source_ref TEXT,
  status     TEXT NOT NULL DEFAULT 'pending',  -- pending | labeled | ignored
  created_at INTEGER NOT NULL
);
