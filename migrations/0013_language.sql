-- 0013 — La langue de la conversation.
--
-- Syn répond dans la langue de son utilisateur, détectée sur ses propres
-- phrases. Mais une réponse courte n'a pas de langue (« gmail », « ok », « le
-- deuxième ») : sans mémoire, la conversation basculerait d'une langue à
-- l'autre au fil des tours. La langue est donc retenue par conversation, et
-- seule une phrase franche la fait changer.
ALTER TABLE sessions ADD COLUMN lang TEXT;
