-- 0005 — Index plein texte local, chiffré avec le reste de la base.
-- Évite de recalculer syn_fold sur le corps de chaque fichier à chaque requête.
CREATE VIRTUAL TABLE IF NOT EXISTS items_fts USING fts5(
  item_id UNINDEXED,
  title,
  body,
  path,
  tokenize = 'unicode61 remove_diacritics 2'
);

DELETE FROM items_fts;
INSERT INTO items_fts(item_id, title, body, path)
SELECT id, COALESCE(title, ''), COALESCE(body, ''), COALESCE(path, '')
FROM items WHERE status = 'active';

CREATE TRIGGER IF NOT EXISTS items_fts_after_insert
AFTER INSERT ON items WHEN NEW.status = 'active'
BEGIN
  INSERT INTO items_fts(item_id, title, body, path)
  VALUES (NEW.id, COALESCE(NEW.title, ''), COALESCE(NEW.body, ''), COALESCE(NEW.path, ''));
END;

CREATE TRIGGER IF NOT EXISTS items_fts_after_update
AFTER UPDATE OF title, body, path, status ON items
BEGIN
  DELETE FROM items_fts WHERE item_id = OLD.id;
  INSERT INTO items_fts(item_id, title, body, path)
  SELECT NEW.id, COALESCE(NEW.title, ''), COALESCE(NEW.body, ''), COALESCE(NEW.path, '')
  WHERE NEW.status = 'active';
END;

CREATE TRIGGER IF NOT EXISTS items_fts_after_delete
AFTER DELETE ON items
BEGIN
  DELETE FROM items_fts WHERE item_id = OLD.id;
END;
