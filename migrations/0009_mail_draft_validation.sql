-- Le parcours d'envoi validé avec les maquettes comporte une étape que l'état
-- précédent ne savait pas représenter : quand c'est SYN qui rédige le message,
-- l'utilisateur relit le texte proposé et le valide AVANT qu'on lui demande le
-- compte d'envoi. Sans cette mémoire, la question « tu valides ? » se reposait
-- à chaque tour, ou pire, disparaissait.
--
-- `body_state` :
--   'validated' — texte donné par l'utilisateur, ou déjà approuvé par lui ;
--   'draft'     — rédigé par Syn, en attente de sa relecture.
ALTER TABLE mail_compositions ADD COLUMN body_state TEXT NOT NULL DEFAULT 'validated';
