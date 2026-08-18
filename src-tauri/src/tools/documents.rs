//! Création, écriture et ouverture de documents (doc maître §10 : « ajouter une
//! capacité = ajouter un outil »).
//!
//! Un document produit par Syn est un fichier réel, ouvrable dans l'application
//! de l'utilisateur, indexé immédiatement — pas un texte affiché dans le fil de
//! conversation. Les formats sont écrits localement, sans dépendre d'un service
//! distant : `docx` est un vrai paquet OOXML, lisible par Word, Pages et Google
//! Docs.

use crate::bus::Bus;
use crate::db::Db;
use crate::error::{AppError, Result};
use crate::llm::LlmClient;
use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Formats que Syn sait écrire seul.
pub fn normalized_format(format: &str) -> Result<&'static str> {
    let lowered = format.trim().to_lowercase();
    match lowered.trim_start_matches('.') {
        "" | "md" | "markdown" => Ok("md"),
        "txt" | "text" | "texte" => Ok("txt"),
        "csv" => Ok("csv"),
        "docx" | "doc" | "word" => Ok("docx"),
        other => Err(AppError::Invalid(format!(
            "Format « {other} » non pris en charge : Syn écrit md, txt, csv ou docx."
        ))),
    }
}

/// Nom de fichier sûr tiré du titre demandé. On conserve les accents et les
/// espaces — c'est le nom que l'utilisateur cherchera plus tard.
pub fn safe_file_name(title: &str) -> String {
    let cleaned: String = title
        .chars()
        .map(|character| {
            if character.is_alphanumeric()
                || matches!(character, ' ' | '-' | '_' | '\'' | '(' | ')' | '&' | ',')
            {
                character
            } else {
                ' '
            }
        })
        .collect();
    let collapsed = cleaned.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim_matches(|character: char| {
        character == '-' || character == '.' || character.is_whitespace()
    });
    if trimmed.is_empty() {
        "Document Syn".to_string()
    } else {
        trimmed.chars().take(90).collect()
    }
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

const DOCX_CONTENT_TYPES: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;

const DOCX_RELS: &str = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

fn docx_document_xml(content: &str) -> String {
    let paragraphs = content
        .lines()
        .map(|line| {
            format!(
                "<w:p><w:r><w:t xml:space=\"preserve\">{}</w:t></w:r></w:p>",
                xml_escape(line)
            )
        })
        .collect::<String>();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\" standalone=\"yes\"?>\
         <w:document xmlns:w=\"http://schemas.openxmlformats.org/wordprocessingml/2006/main\">\
         <w:body>{paragraphs}<w:sectPr/></w:body></w:document>"
    )
}

/// Paquet OOXML minimal mais valide (trois parties obligatoires).
pub fn docx_bytes(content: &str) -> Result<Vec<u8>> {
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut archive = zip::ZipWriter::new(&mut buffer);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        let zip_error = |error: zip::result::ZipError| {
            AppError::Other(format!("écriture du document Word impossible : {error}"))
        };
        for (name, part) in [
            ("[Content_Types].xml", DOCX_CONTENT_TYPES.to_string()),
            ("_rels/.rels", DOCX_RELS.to_string()),
            ("word/document.xml", docx_document_xml(content)),
        ] {
            archive.start_file(name, options).map_err(zip_error)?;
            archive
                .write_all(part.as_bytes())
                .map_err(|error| AppError::Other(error.to_string()))?;
        }
        archive.finish().map_err(zip_error)?;
    }
    Ok(buffer.into_inner())
}

/// Octets d'un document dans le format demandé.
pub fn document_bytes(format: &str, title: &str, content: &str) -> Result<Vec<u8>> {
    match normalized_format(format)? {
        "docx" => docx_bytes(content),
        "md" => Ok(format!("# {title}\n\n{content}\n").into_bytes()),
        _ => Ok(format!("{content}\n").into_bytes()),
    }
}

/// Dossier de destination par défaut : celui que l'utilisateur associe à ses
/// documents. On ne crée jamais rien dans une zone système.
fn default_folder() -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| AppError::Other("dossier personnel introuvable".into()))?;
    let documents = home.join("Documents");
    if documents.is_dir() {
        Ok(documents)
    } else {
        Ok(home)
    }
}

fn resolve_target_folder(db: &Db, folder: Option<&str>) -> Result<PathBuf> {
    let home =
        dirs::home_dir().ok_or_else(|| AppError::Other("dossier personnel introuvable".into()))?;
    let directory = match folder.map(str::trim).filter(|value| !value.is_empty()) {
        Some(folder) => super::reorganize::resolve_location(db, folder)?,
        None => default_folder()?,
    };
    if !directory.is_dir() {
        return Err(AppError::Invalid(format!(
            "« {} » n'est pas un dossier.",
            directory.display()
        )));
    }
    if super::reorganize::is_protected_target(&directory, &home) {
        return Err(AppError::Security(format!(
            "Syn n'écrit pas dans « {} » : cet emplacement est protégé.",
            directory.display()
        )));
    }
    Ok(directory)
}

pub struct LocalDocument {
    pub path: PathBuf,
    pub created: bool,
}

/// Crée un document local et l'indexe aussitôt : il doit être retrouvable par
/// une recherche dans la seconde qui suit, sans attendre le prochain balayage.
pub async fn create_local(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    bus: &Bus,
    embed_model: &str,
    title: &str,
    content: &str,
    format: &str,
    folder: Option<&str>,
) -> Result<LocalDocument> {
    let extension = normalized_format(format)?;
    let directory = resolve_target_folder(db, folder)?;
    let bytes = document_bytes(extension, title, content)?;
    let path = super::reorganize::unique_destination(
        directory.join(format!("{}.{extension}", safe_file_name(title))),
    );
    std::fs::write(&path, &bytes).map_err(|error| {
        AppError::Other(format!(
            "création de {} impossible : {error}",
            path.display()
        ))
    })?;
    crate::connectors::files::index_file(db, llm, bus, embed_model, &path).await?;
    Ok(LocalDocument {
        path,
        created: true,
    })
}

/// Retrouve un document local à écrire ou à ouvrir : chemin exact, sinon nom
/// dans l'index, sinon nom dans les dossiers autorisés.
pub fn locate_local(db: &Db, target: &str) -> Result<PathBuf> {
    let direct = PathBuf::from(target);
    if direct.is_file() {
        return Ok(direct);
    }
    if let Some(relative) = target.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            let expanded = home.join(relative);
            if expanded.is_file() {
                return Ok(expanded);
            }
        }
    }
    let folded = crate::db::fold(target);
    let candidates: Vec<String> = db.with(|connection| {
        let mut statement = connection.prepare(
            "SELECT path FROM items
             WHERE source='files' AND status='active' AND path IS NOT NULL
               AND (syn_fold(COALESCE(title,'')) LIKE '%'||?1||'%'
                    OR syn_fold(path) LIKE '%'||?1||'%')
             ORDER BY COALESCE(mtime,0) DESC LIMIT 8",
        )?;
        let rows = statement.query_map([&folded], |row| row.get::<_, String>(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;
    match candidates
        .into_iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
    {
        Some(path) => Ok(path),
        None => Err(AppError::NotFound(format!(
            "Aucun document « {target} » dans les emplacements suivis par Syn."
        ))),
    }
}

/// Écrit dans un document local existant. `append` conserve le contenu, ce qui
/// laisse le geste réversible ; `replace` sauvegarde d'abord l'ancienne version.
/// Retouche un document Word existant, en place, avec sauvegarde.
///
/// Contrairement à `write_local`, qui réécrit du texte brut, la mise en forme,
/// les images et les styles sont préservés : seule la partie principale du
/// paquet OOXML est modifiée, et uniquement là où une opération s'applique.
pub fn edit_local(
    db: &Db,
    target: &str,
    operations: &[super::docx_edit::Operation],
) -> Result<(Value, Value)> {
    let path = locate_local(db, target)?;
    let home =
        dirs::home_dir().ok_or_else(|| AppError::Other("dossier personnel introuvable".into()))?;
    if super::reorganize::is_protected_target(&path, &home) {
        return Err(AppError::Security(format!(
            "« {} » se trouve dans une zone protégée.",
            path.display()
        )));
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_lowercase();
    if extension != "docx" {
        return Err(AppError::Invalid(format!(
            "Syn ne sait retoucher en place que les documents Word (.docx). « {} » est un {extension} — \
             pour un fichier texte, je peux le réécrire ; pour les autres formats, je ne sais pas encore.",
            path.display()
        )));
    }

    let (bytes, report) = super::docx_edit::apply(&path, operations)?;
    if report.is_empty() {
        return Err(AppError::Invalid(
            "Aucun passage du document ne correspond à ce que tu demandes. Précise ce qu'il faut viser.".into(),
        ));
    }
    // La sauvegarde est prise AVANT l'écriture : c'est elle qui rend la
    // retouche annulable, et donc acceptable.
    let backup = super::reorganize::unique_destination(
        path.with_extension(format!("docx.syn-avant-{}", crate::db::now())),
    );
    std::fs::copy(&path, &backup).map_err(|error| {
        AppError::Other(format!(
            "sauvegarde de la version précédente impossible : {error}"
        ))
    })?;
    std::fs::write(&path, &bytes).map_err(|error| {
        AppError::Other(format!(
            "écriture de {} impossible : {error}",
            path.display()
        ))
    })?;

    let mut faits: Vec<String> = Vec::new();
    if report.paragraphs_touched > 0 {
        faits.push(format!(
            "{} paragraphe(s) mis en forme",
            report.paragraphs_touched
        ));
    }
    if report.replacements > 0 {
        faits.push(format!("{} remplacement(s)", report.replacements));
    }
    if report.paragraphs_added > 0 {
        faits.push(format!(
            "{} paragraphe(s) ajouté(s)",
            report.paragraphs_added
        ));
    }
    if report.placeholders_added > 0 {
        faits.push(format!(
            "{} emplacement(s) d'image réservé(s) — Syn ne crée pas d'images, l'encadré indique où déposer la vôtre",
            report.placeholders_added
        ));
    }
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("document");
    Ok((
        json!({
            "path": path.to_string_lossy(),
            "report": format!("« {name} » retouché : {}. La version précédente est conservée à côté.", faits.join(", ")),
            "details": report,
        }),
        json!({
            "kind": "restore_binary_file",
            "path": path.to_string_lossy(),
            "backup": backup.to_string_lossy(),
        }),
    ))
}

pub fn write_local(db: &Db, target: &str, content: &str, mode: &str) -> Result<(Value, Value)> {
    let path = locate_local(db, target)?;
    let home =
        dirs::home_dir().ok_or_else(|| AppError::Other("dossier personnel introuvable".into()))?;
    if super::reorganize::is_protected_target(&path, &home) {
        return Err(AppError::Security(format!(
            "« {} » se trouve dans une zone protégée.",
            path.display()
        )));
    }
    let extension = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_lowercase();
    if !matches!(extension.as_str(), "md" | "markdown" | "txt" | "csv") {
        return Err(AppError::Invalid(format!(
            "Syn ne réécrit que du texte (md, txt, csv). « {} » est un {extension} : \
             je peux créer un nouveau document à côté.",
            path.display()
        )));
    }
    let previous = std::fs::read_to_string(&path).unwrap_or_default();
    let backup = super::reorganize::unique_destination(
        path.with_extension(format!("{extension}.syn-avant-{}", crate::db::now())),
    );
    let replaces = matches!(
        mode.trim().to_lowercase().as_str(),
        "replace" | "remplace" | "remplacer"
    );
    let updated = if replaces {
        std::fs::copy(&path, &backup).map_err(|error| {
            AppError::Other(format!(
                "sauvegarde de la version précédente impossible : {error}"
            ))
        })?;
        format!("{content}\n")
    } else if previous.trim().is_empty() {
        format!("{content}\n")
    } else {
        format!("{}\n\n{content}\n", previous.trim_end())
    };
    std::fs::write(&path, updated.as_bytes()).map_err(|error| {
        AppError::Other(format!(
            "écriture de {} impossible : {error}",
            path.display()
        ))
    })?;
    let undo = json!({
        "kind": "restore_text_file",
        "path": path.to_string_lossy(),
        "previous": previous,
        "backup": backup.to_string_lossy(),
    });
    Ok((
        json!({
            "path": path.to_string_lossy(),
            "mode": if replaces { "remplacé" } else { "complété" },
            "taille": updated.len(),
        }),
        undo,
    ))
}

/// Ouvre un document dans l'application de l'utilisateur. Accepte un chemin, un
/// nom indexé ou un lien cloud déjà connu de Syn.
pub fn open_target(db: &Db, target: &str) -> Result<Value> {
    let trimmed = target.trim();
    let opened = if trimmed.starts_with("https://") || trimmed.starts_with("http://") {
        // Garde de périmètre : on n'ouvre une URL que si elle vient d'un objet
        // indexé, jamais une adresse fabriquée dans une réponse de modèle.
        let known: bool = db.with(|connection| {
            Ok(connection
                .query_row(
                    "SELECT 1 FROM items WHERE path=?1 AND status='active' LIMIT 1",
                    rusqlite::params![trimmed],
                    |_| Ok(true),
                )
                .unwrap_or(false))
        })?;
        if !known {
            return Err(AppError::Security(
                "ce lien ne provient pas d'un document connu de Syn".into(),
            ));
        }
        trimmed.to_string()
    } else {
        locate_local(db, trimmed)?.to_string_lossy().to_string()
    };
    if !cfg!(target_os = "macos") {
        return Err(AppError::Invalid(
            "l'ouverture de documents n'est disponible que sur macOS".into(),
        ));
    }
    let status = std::process::Command::new("/usr/bin/open")
        .arg(&opened)
        .status()
        .map_err(|error| AppError::Other(format!("ouverture impossible : {error}")))?;
    if !status.success() {
        return Err(AppError::Other(format!("ouverture refusée pour {opened}")));
    }
    Ok(json!({"ouvert": opened}))
}

/// Restaure un fichier texte après un `document.write` (chemin d'annulation).
pub fn restore_text_file(path: &Path, previous: &str, backup: Option<&Path>) -> Result<()> {
    std::fs::write(path, previous.as_bytes()).map_err(|error| {
        AppError::Other(format!(
            "restauration de {} impossible : {error}",
            path.display()
        ))
    })?;
    if let Some(backup) = backup {
        let _ = std::fs::remove_file(backup);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_docx_minimal_est_une_archive_ooxml_valide() {
        let bytes = docx_bytes("Bonjour Paul\nDeuxième ligne").unwrap();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let names = archive.file_names().map(str::to_string).collect::<Vec<_>>();
        for part in ["[Content_Types].xml", "_rels/.rels", "word/document.xml"] {
            assert!(names.contains(&part.to_string()), "{names:?}");
        }
        let mut document = String::new();
        std::io::Read::read_to_string(
            &mut archive.by_name("word/document.xml").unwrap(),
            &mut document,
        )
        .unwrap();
        assert!(document.contains("Deuxième ligne"), "{document}");
    }

    #[test]
    fn le_nom_de_fichier_reste_lisible_et_sans_separateur() {
        assert_eq!(
            safe_file_name("Compte rendu : réunion du 12/03 ?"),
            "Compte rendu réunion du 12 03"
        );
        assert_eq!(safe_file_name("///"), "Document Syn");
    }

    #[test]
    fn le_contenu_est_echappe_avant_dentrer_dans_le_xml() {
        let xml = docx_document_xml("a < b & c");
        assert!(xml.contains("a &lt; b &amp; c"), "{xml}");
    }

    #[test]
    fn ecrire_dans_un_document_reste_annulable() {
        let root = std::env::temp_dir().join(format!("syn-doc-write-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = crate::db::Db::open(&root.join("t.db"), &"4".repeat(64)).unwrap();
        let note = root.join("notes.md");
        std::fs::write(&note, "# Notes\n\nPremière ligne.\n").unwrap();
        let target = note.to_string_lossy().to_string();

        let (_, undo) = write_local(&db, &target, "Ligne ajoutée.", "append").unwrap();
        let after_append = std::fs::read_to_string(&note).unwrap();
        assert!(after_append.contains("Première ligne."), "{after_append}");
        assert!(after_append.contains("Ligne ajoutée."), "{after_append}");

        let (report, undo_replace) = write_local(&db, &target, "Tout neuf.", "replace").unwrap();
        assert_eq!(report["mode"], "remplacé");
        assert_eq!(std::fs::read_to_string(&note).unwrap(), "Tout neuf.\n");

        // L'annulation doit rendre l'état d'avant le remplacement, puis d'avant
        // l'ajout — c'est ce qui autorise le classement « réversible local ».
        crate::actions::apply_undo(&db, &undo_replace).unwrap();
        assert_eq!(std::fs::read_to_string(&note).unwrap(), after_append);
        crate::actions::apply_undo(&db, &undo).unwrap();
        assert_eq!(
            std::fs::read_to_string(&note).unwrap(),
            "# Notes\n\nPremière ligne.\n"
        );

        // Un binaire Word n'est pas réécrit à l'aveugle.
        let word = root.join("rapport.docx");
        std::fs::write(&word, docx_bytes("x").unwrap()).unwrap();
        assert!(write_local(&db, &word.to_string_lossy(), "…", "append").is_err());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn un_format_inconnu_est_refuse_explicitement() {
        assert!(normalized_format("pages").is_err());
        assert_eq!(normalized_format("Word").unwrap(), "docx");
        assert_eq!(normalized_format("").unwrap(), "md");
    }
}
