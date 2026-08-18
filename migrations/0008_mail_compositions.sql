-- Un envoi de mail se construit sur plusieurs tours : destinataire, contenu,
-- compte d'envoi arrivent rarement dans le même message. Sans état persistant,
-- Syn redemandait à chaque tour une information déjà donnée — et finissait par
-- perdre le fil complètement.
--
-- Ce qui est stocké ici n'est PAS du langage interprété : ce sont les arguments
-- structurés des appels d'outil, accumulés tour après tour.
CREATE TABLE IF NOT EXISTS mail_compositions (
  session_id TEXT PRIMARY KEY,
  recipient  TEXT,
  subject    TEXT,
  body       TEXT,
  via        TEXT,
  updated_at INTEGER NOT NULL
);
