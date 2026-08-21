-- 0012 — La toile : graphe personnel, ligne de temps, habitudes observées.
--
-- Jusqu'ici la mémoire de Syn savait retrouver des CHOSES qui se ressemblent.
-- Elle ne savait pas QUI est qui, QUAND les choses se sont passées, ni COMMENT
-- l'utilisateur aime que les choses soient faites. Ces trois manques sont
-- comblés par trois structures distinctes, volontairement explicites :
-- lisibles, datées, sourcées, effaçables — l'inverse d'un réseau de neurones.
--
-- Tout ce qui suit est DÉRIVÉ des sources déjà indexées : la table peut être
-- vidée et reconstruite sans perte, comme le reste de l'index (doc maître §8).

-- 1) Arêtes typées du graphe personnel.
--
-- Un lien vaut par son TYPE, sa DATE et sa PROVENANCE : « tout relier à tout »
-- ne produirait que du bruit. Chaque arête compte ses observations — c'est ce
-- qui distingue un correspondant quotidien d'un inconnu croisé une fois.
CREATE TABLE IF NOT EXISTS relations (
  id           TEXT PRIMARY KEY,
  src_kind     TEXT NOT NULL,          -- self | person | contact | item | event | project
  src_id       TEXT NOT NULL,
  kind         TEXT NOT NULL,          -- ecrit_a | auteur_de | apparait_dans | co_destinataire | reunit | classe_dans
  dst_kind     TEXT NOT NULL,
  dst_id       TEXT NOT NULL,
  observations INTEGER NOT NULL DEFAULT 1,
  first_seen   INTEGER NOT NULL,
  last_seen    INTEGER NOT NULL,
  origin       TEXT NOT NULL,          -- mail | calendar | files | conversation | utilisateur
  UNIQUE(src_kind, src_id, kind, dst_kind, dst_id)
);
CREATE INDEX IF NOT EXISTS idx_relations_src ON relations(src_kind, src_id, last_seen DESC);
CREATE INDEX IF NOT EXISTS idx_relations_dst ON relations(dst_kind, dst_id, last_seen DESC);
CREATE INDEX IF NOT EXISTS idx_relations_kind ON relations(kind, observations DESC);

-- 2) Les adresses de l'utilisateur lui-même.
--
-- Sans elles, impossible de distinguer « il m'a écrit » de « je lui ai écrit » —
-- donc impossible de savoir qu'un message attend une réponse. Elles sont
-- DÉDUITES des en-têtes déjà ingérés (une adresse qui apparaît à la fois comme
-- destinataire et comme expéditeur est la sienne), jamais demandées deux fois.
CREATE TABLE IF NOT EXISTS self_identities (
  address      TEXT PRIMARY KEY,
  observations INTEGER NOT NULL DEFAULT 0,
  confirmed    INTEGER NOT NULL DEFAULT 0,   -- 1 = l'utilisateur l'a validée
  updated_at   INTEGER NOT NULL
);

-- 2 bis) Les correspondants rencontrés dans les en-têtes.
--
-- Tout le monde n'est pas « une personne » du carnet : la plupart des adresses
-- croisées sont des correspondants occasionnels. Les inscrire d'office dans
-- `people` polluerait le carnet de l'utilisateur ; les oublier obligeait Syn à
-- relire les corps de mails à chaque question. Ils vivent donc ici, avec leur
-- nom affiché et leur fréquence — et deviennent des personnes le jour où
-- l'utilisateur le décide.
CREATE TABLE IF NOT EXISTS contacts (
  address      TEXT PRIMARY KEY,
  display_name TEXT,
  person_id    TEXT,                   -- rempli quand le correspondant est devenu une personne connue
  observations INTEGER NOT NULL DEFAULT 1,
  first_seen   INTEGER NOT NULL,
  last_seen    INTEGER NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_contacts_person ON contacts(person_id);
CREATE INDEX IF NOT EXISTS idx_contacts_seen ON contacts(observations DESC, last_seen DESC);

-- 3) Mémoire procédurale : comment l'utilisateur aime que les choses soient faites.
--
-- Rien ici n'est appliqué en silence. Une habitude est OBSERVÉE (comptée), puis
-- proposée à l'utilisateur, qui la confirme ou la rejette. Une habitude rejetée
-- reste en base avec son statut : la réobserver ne doit pas la ressusciter.
CREATE TABLE IF NOT EXISTS preferences (
  id           TEXT PRIMARY KEY,
  topic        TEXT NOT NULL,          -- mail.compte | mail.ouverture | mail.cloture | rythme.heures | rangement.destination
  subject      TEXT NOT NULL DEFAULT '',  -- ce que l'habitude qualifie (un destinataire, un type de fichier…)
  value        TEXT NOT NULL,
  observations INTEGER NOT NULL DEFAULT 1,
  first_seen   INTEGER NOT NULL,
  last_seen    INTEGER NOT NULL,
  status       TEXT NOT NULL DEFAULT 'observed',  -- observed | confirmed | rejected
  evidence     TEXT,                   -- de quoi Syn l'a déduit (explicabilité)
  UNIQUE(topic, subject, value)
);
CREATE INDEX IF NOT EXISTS idx_preferences_topic
  ON preferences(topic, status, observations DESC);

-- 4) Curseurs des passes de construction (incrémental, reprise après coupure).
CREATE TABLE IF NOT EXISTS memory_state (
  key   TEXT PRIMARY KEY,
  value TEXT NOT NULL
);

-- 5) La ligne de temps interroge les tables existantes par leur date : sans ces
-- index, chaque question « que s'est-il passé la semaine dernière ? » balayait
-- tout le corpus.
CREATE INDEX IF NOT EXISTS idx_items_created_at ON items(created_at DESC);
CREATE INDEX IF NOT EXISTS idx_conversations_created ON conversations(created_at DESC);
