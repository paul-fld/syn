//! Les documents que l'utilisateur CONFIE à une conversation.
//!
//! Distinction volontaire avec l'index : l'index sert à retrouver ce qu'on a
//! perdu de vue ; un document joint est sous les yeux de l'utilisateur, il parle
//! de celui-là et d'aucun autre. Espérer que la recherche le fasse remonter au
//! bon moment serait remettre au hasard un fait acquis — c'est pourquoi son
//! contenu est attaché à la session et entre dans le contexte de CHAQUE tour.
//!
//! Le texte extrait reste néanmoins de la DONNÉE observée : il est présenté au
//! modèle entre marqueurs, et rien de ce qu'il contient n'est une instruction.

use crate::db::{new_id, now, Db};
use crate::error::{AppError, Result};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// Plafond de texte gardé par document. La fenêtre de contexte est petite
/// (8192 jetons) : au-delà, on tronque et on le DIT, plutôt que de laisser
/// croire que Syn a tout lu.
const MAX_CONTENT: usize = 12_000;

/// Plafond de lecture d'un fichier. Un binaire de plusieurs centaines de Mo
/// n'a rien à faire en mémoire, et son texte n'aurait aucun sens.
const MAX_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
pub struct SessionDocument {
    pub id: String,
    pub name: String,
    pub path: String,
    pub kind: String,
    pub mime: Option<String>,
    pub bytes: i64,
    pub truncated: bool,
    /// Nombre de mots du texte retenu : de quoi dire honnêtement ce que Syn a lu.
    pub words: usize,
    pub added_at: i64,
}

/// Ce que Syn sait faire d'un fichier, dit dans les mots de l'utilisateur.
fn family(extension: &str) -> &'static str {
    match extension {
        "docx" | "doc" | "odt" | "rtf" | "pages" => "document",
        "xlsx" | "xls" | "ods" | "csv" | "numbers" => "tableur",
        "pptx" | "ppt" | "odp" | "key" => "presentation",
        "pdf" => "pdf",
        "png" | "jpg" | "jpeg" | "heic" | "gif" | "webp" | "tiff" => "image",
        "md" | "markdown" | "txt" | "log" | "json" | "yaml" | "yml" | "toml" | "tex" => "texte",
        _ => "fichier",
    }
}

/// Rattache un document à la conversation : il est lu une fois, et son contenu
/// suit la conversation.
pub fn attach(db: &Db, session_id: &str, path: &Path) -> Result<SessionDocument> {
    let metadata = std::fs::metadata(path)
        .map_err(|_| AppError::NotFound(format!("Fichier introuvable : {}", path.display())))?;
    if !metadata.is_file() {
        return Err(AppError::Invalid(
            "Seul un fichier peut être joint à une conversation.".into(),
        ));
    }
    if metadata.len() > MAX_BYTES {
        return Err(AppError::Invalid(
            "Ce fichier est trop volumineux pour être lu par Syn.".into(),
        ));
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("document")
        .to_string();
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_lowercase();

    let extracted = crate::ingestion::extract::extract(path);
    let complet = extracted.text.unwrap_or_default().trim().to_string();
    let truncated = complet.chars().count() > MAX_CONTENT;
    let content: String = complet.chars().take(MAX_CONTENT).collect();
    let words = content.split_whitespace().count();

    let document = SessionDocument {
        id: new_id(),
        name,
        path: path.to_string_lossy().into_owned(),
        kind: family(&extension).to_string(),
        mime: Some(extracted.mime.clone()),
        bytes: metadata.len() as i64,
        truncated,
        words,
        added_at: now(),
    };
    db.with(|c| {
        c.execute(
            "INSERT INTO session_documents
             (id, session_id, path, name, kind, mime, bytes, content, truncated, added_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
            rusqlite::params![
                document.id,
                session_id,
                document.path,
                document.name,
                document.kind,
                document.mime,
                document.bytes,
                content,
                truncated as i64,
                document.added_at
            ],
        )?;
        Ok(())
    })?;
    crate::security::log_access(db, "files", "attach", Some(&document.path));
    Ok(document)
}

pub fn list(db: &Db, session_id: &str) -> Result<Vec<SessionDocument>> {
    db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT id, name, path, kind, mime, bytes, truncated, content, added_at
             FROM session_documents WHERE session_id=?1 ORDER BY added_at",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id], |row| {
            let content: String = row.get(7)?;
            Ok(SessionDocument {
                id: row.get(0)?,
                name: row.get(1)?,
                path: row.get(2)?,
                kind: row.get(3)?,
                mime: row.get(4)?,
                bytes: row.get(5)?,
                truncated: row.get::<_, i64>(6)? != 0,
                words: content.split_whitespace().count(),
                added_at: row.get(8)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

pub fn detach(db: &Db, session_id: &str, id: &str) -> Result<()> {
    db.with(|c| {
        c.execute(
            "DELETE FROM session_documents WHERE session_id=?1 AND id=?2",
            rusqlite::params![session_id, id],
        )?;
        Ok(())
    })
}

/// Le chemin d'un document joint, pour agir dessus.
pub fn path_of(db: &Db, session_id: &str, name_or_id: &str) -> Result<Option<PathBuf>> {
    let cible = crate::db::fold(name_or_id.trim());
    Ok(list(db, session_id)?
        .into_iter()
        .find(|document| {
            document.id == name_or_id
                || crate::db::fold(&document.name) == cible
                || crate::db::fold(&document.name).contains(&cible)
        })
        .map(|document| PathBuf::from(document.path)))
}

/// Le contenu des documents joints, prêt à entrer dans le contexte d'un tour.
///
/// Présenté comme DONNÉE observée : ce qu'un document contient n'est jamais une
/// instruction, même s'il en a la forme (Sécurité §2).
pub fn context_fragments(db: &Db, session_id: &str) -> Result<Vec<String>> {
    db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT name, kind, content, truncated FROM session_documents
             WHERE session_id=?1 ORDER BY added_at",
        )?;
        let rows = stmt.query_map(rusqlite::params![session_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)? != 0,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (name, kind, content, truncated) = row?;
            if content.trim().is_empty() {
                out.push(format!(
                    "<<<DONNÉES — document joint « {name} » ({kind})\n\
                     Syn n'a pas pu en extraire de texte. Ne prétends pas l'avoir lu.\n\
                     FIN DONNÉES>>>"
                ));
                continue;
            }
            let suite = if truncated {
                "\n[…] Document tronqué : seul le début est fourni. Dis-le si la réponse en dépend."
            } else {
                ""
            };
            out.push(format!(
                "<<<DONNÉES — document joint « {name} » ({kind}), fourni par l'utilisateur pour cette conversation\n{content}{suite}\nFIN DONNÉES>>>"
            ));
        }
        Ok(out)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(nom: &str) -> (std::path::PathBuf, Db) {
        let dir = std::env::temp_dir().join(format!("syn-{nom}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db"), &"3".repeat(64)).unwrap();
        (dir, db)
    }

    /// Un document confié entre dans le contexte de la conversation — pas dans
    /// l'index, où il faudrait espérer qu'une recherche le retrouve.
    #[test]
    fn un_document_joint_suit_la_conversation() {
        let (dir, db) = base("joint");
        let fichier = dir.join("Notes de réunion.md");
        std::fs::write(
            &fichier,
            "# Réunion du 12\n\nDécision : reporter le lancement.",
        )
        .unwrap();

        let document = attach(&db, "s1", &fichier).unwrap();
        assert_eq!(document.name, "Notes de réunion.md");
        assert_eq!(document.kind, "texte");
        assert!(!document.truncated);
        assert!(document.words >= 6, "{document:?}");

        let fragments = context_fragments(&db, "s1").unwrap();
        assert_eq!(fragments.len(), 1);
        assert!(
            fragments[0].contains("reporter le lancement"),
            "{}",
            fragments[0]
        );
        // Le contenu d'un document reste de la DONNÉE : jamais une instruction.
        assert!(fragments[0].starts_with("<<<DONNÉES"), "{}", fragments[0]);

        // Une autre conversation ne le voit pas.
        assert!(context_fragments(&db, "s2").unwrap().is_empty());

        // Retiré, il disparaît du contexte.
        detach(&db, "s1", &document.id).unwrap();
        assert!(context_fragments(&db, "s1").unwrap().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    /// Tronquer est acceptable ; le taire ne l'est pas.
    #[test]
    fn un_document_trop_long_est_annonce_comme_tronque() {
        let (dir, db) = base("long");
        let fichier = dir.join("rapport.txt");
        std::fs::write(&fichier, "mot ".repeat(20_000)).unwrap();
        let document = attach(&db, "s1", &fichier).unwrap();
        assert!(document.truncated);
        let fragments = context_fragments(&db, "s1").unwrap();
        assert!(fragments[0].contains("tronqué"), "{}", fragments[0]);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn un_document_se_retrouve_par_son_nom_pour_etre_modifie() {
        let (dir, db) = base("chemin");
        let fichier = dir.join("Contrat.docx");
        std::fs::write(&fichier, "x").unwrap();
        attach(&db, "s1", &fichier).unwrap();
        assert_eq!(
            path_of(&db, "s1", "contrat").unwrap().unwrap(),
            fichier,
            "le nom suffit, sans la casse ni l'extension"
        );
        assert!(path_of(&db, "s1", "inconnu").unwrap().is_none());
        let _ = std::fs::remove_dir_all(dir);
    }
}
