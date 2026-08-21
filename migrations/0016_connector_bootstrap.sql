-- Reprise fine des catalogues cloud volumineux. Le curseur final du fournisseur
-- n'est publié qu'une fois le catalogue initial terminé ; cette table permet de
-- reprendre la page suivante après fermeture ou panne sans recommencer à zéro.
CREATE TABLE IF NOT EXISTS connector_bootstrap_state (
  provider     TEXT NOT NULL,
  resource     TEXT NOT NULL,
  continuation TEXT,
  watermark    TEXT,
  processed    INTEGER NOT NULL DEFAULT 0,
  total        INTEGER,
  updated_at   INTEGER NOT NULL,
  PRIMARY KEY(provider, resource)
);
