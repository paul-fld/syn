-- Un document confié à Syn dans une conversation.
--
-- Il ne passe PAS par l'index : l'index sert à retrouver ce qu'on a perdu de
-- vue, alors qu'un document joint est sous les yeux de l'utilisateur et doit
-- être présent à CHAQUE tour de cette conversation. Espérer que la recherche le
-- fasse remonter serait remettre au hasard ce qui est un fait acquis.
--
-- Le texte extrait est stocké ici pour que la conversation reste intelligible
-- même si le fichier bouge ou disparaît ; le chemin reste noté pour pouvoir le
-- rouvrir et le modifier.
CREATE TABLE IF NOT EXISTS session_documents (
  id         TEXT PRIMARY KEY,
  session_id TEXT NOT NULL,
  path       TEXT NOT NULL,
  name       TEXT NOT NULL,
  kind       TEXT NOT NULL,           -- document | tableur | presentation | image…
  mime       TEXT,
  bytes      INTEGER NOT NULL DEFAULT 0,
  content    TEXT NOT NULL DEFAULT '',
  truncated  INTEGER NOT NULL DEFAULT 0,
  added_at   INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_session_documents_session
  ON session_documents(session_id, added_at DESC);
