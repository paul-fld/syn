//! Connecteur Mail (doc Connecteurs §1).
//! Voie A [ce build] : boîte native Apple Mail, lecture seule du stock local
//! (`~/Library/Mail`, .emlx) sous « Accès complet au disque ». Ne voit que le
//! synchronisé ; pas d'envoi par cette voie.
//! Voie B [🔎 config OAuth requise] : Gmail/Graph/IMAP — vérification d'app
//! fournisseur + audit CASA à anticiper. Corps de mail = donnée non fiable.

use crate::bus::Bus;
use crate::db::{now, Db};
use crate::error::{AppError, Result};
use crate::ingestion;
use crate::llm::LlmClient;
use crate::memory::{self, Item};
use std::path::PathBuf;
use std::sync::Arc;

const MAX_MAILS_PER_SYNC: usize = 800;

pub fn mail_dir() -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let dir = dirs::home_dir()?.join("Library/Mail");
    dir.exists().then_some(dir)
}

/// La permission est-elle accordée ? (échec de lecture = TCC refuse)
pub fn native_available() -> bool {
    match mail_dir() {
        Some(dir) => std::fs::read_dir(&dir).is_ok(),
        None => false,
    }
}

/// Parse un .emlx : première ligne = longueur du message RFC822, puis le message,
/// puis un plist de métadonnées (ignoré).
fn parse_emlx(bytes: &[u8]) -> Option<mailparse::ParsedMail<'_>> {
    let newline = bytes.iter().position(|&b| b == b'\n')?;
    let count: usize = std::str::from_utf8(&bytes[..newline])
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let start = newline + 1;
    let end = (start + count).min(bytes.len());
    mailparse::parse_mail(&bytes[start..end]).ok()
}

/// Synchronisation de la boîte native : incrémental (mtime), skip+log, jamais de chute.
pub async fn sync_native(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    bus: &Bus,
    embed_model: &str,
) -> Result<usize> {
    let dir = mail_dir().ok_or(AppError::NotFound(
        "Apple Mail introuvable sur cette machine.".into(),
    ))?;
    if !native_available() {
        return Err(AppError::Security(
            "Lecture impossible : autorise « Accès complet au disque » pour Syn (Réglages système → Confidentialité et sécurité).".into(),
        ));
    }
    crate::security::log_access(db, "mail", "sync_native", None);

    // Collecte des .emlx les plus récents.
    let mut emlx: Vec<(PathBuf, i64)> = vec![];
    for entry in walkdir::WalkDir::new(&dir)
        .max_depth(8)
        .into_iter()
        .filter_entry(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .flatten()
    {
        if entry.path().extension().and_then(|e| e.to_str()) == Some("emlx") {
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            emlx.push((entry.into_path(), mtime));
        }
    }
    emlx.sort_by(|a, b| b.1.cmp(&a.1));
    emlx.truncate(MAX_MAILS_PER_SYNC);

    let mut count = 0usize;
    for (path, mtime) in emlx {
        if !crate::connectors::is_connected(db, "apple") {
            break;
        }
        let source_ref = path.to_string_lossy().to_string();
        // Incrémental : déjà connu → skip.
        if memory::item_hash(db, "mail", &source_ref)?.is_some() {
            continue;
        }
        let bytes = match std::fs::read(&path) {
            Ok(b) => b,
            Err(_) => continue, // verrouillé/disparu → skip
        };
        let Some(parsed) = parse_emlx(&bytes) else {
            continue;
        };

        let get_header = |name: &str| -> String {
            parsed
                .headers
                .iter()
                .find(|h| h.get_key().eq_ignore_ascii_case(name))
                .map(|h| h.get_value())
                .unwrap_or_default()
        };
        let subject = get_header("Subject");
        let from = get_header("From");
        let to = get_header("To");
        let date = get_header("Date");
        let body = extract_body(&parsed).unwrap_or_default();
        let text = format!("De : {from}\nÀ : {to}\nDate : {date}\nObjet : {subject}\n\n{body}");

        let item = Item {
            id: String::new(),
            source: "mail".into(),
            source_ref: source_ref.clone(),
            r#type: "email".into(),
            title: Some(if subject.is_empty() {
                "(sans objet)".into()
            } else {
                subject.clone()
            }),
            body: Some(text.clone()),
            created_at: mailparse::dateparse(&date).ok(),
            ingested_at: now(),
            hash: Some(blake3::hash(&bytes).to_hex().to_string()),
            path: Some(source_ref.clone()),
            mime: Some("message/rfc822".into()),
            size: Some(bytes.len() as i64),
            mtime: Some(mtime),
            status: "active".into(),
        };
        let id = ingestion::ingest_item(db, llm, bus, embed_model, item, Some(&text)).await?;

        // Rattachement aux personnes + apprentissage progressif des inconnus.
        if let Some((name, email)) = parse_address(&from) {
            let known: bool = db
                .with(|c| {
                    Ok(c.query_row(
                        "SELECT 1 FROM people WHERE lower(name)=lower(?1) OR comm_channels LIKE '%'||lower(?2)||'%'",
                        rusqlite::params![name, email],
                        |_| Ok(true),
                    )
                    .unwrap_or(false))
                })
                .unwrap_or(false);
            if known {
                let pid = memory::find_or_create_person(db, &name, Some(&email), None)?;
                memory::link_person(db, &id, &pid)?;
            } else if !name.is_empty() && !name.contains('@') {
                memory::queue_unknown_name(
                    db,
                    &name,
                    &format!("expéditeur du mail « {subject} »"),
                    &source_ref,
                )?;
            }
        }
        ingestion::extract_entities(db, &id, &source_ref, &body);
        count += 1;
    }
    crate::connectors::set_status(db, "apple", "apple", "connected")?;
    Ok(count)
}

fn extract_body(mail: &mailparse::ParsedMail) -> Option<String> {
    if mail.subparts.is_empty() {
        let ctype = mail.ctype.mimetype.to_lowercase();
        if ctype.starts_with("text/plain") || ctype.starts_with("text/html") {
            let mut body = mail.get_body().ok()?;
            if ctype.starts_with("text/html") {
                body = strip_html(&body);
            }
            return Some(body.chars().take(20_000).collect());
        }
        return None;
    }
    // multipart : préférer text/plain.
    for part in &mail.subparts {
        if part.ctype.mimetype.starts_with("text/plain") {
            if let Some(b) = extract_body(part) {
                return Some(b);
            }
        }
    }
    for part in &mail.subparts {
        if let Some(b) = extract_body(part) {
            return Some(b);
        }
    }
    None
}

fn strip_html(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut in_tag = false;
    let mut in_style_or_script = false;
    let lower = html.to_lowercase();
    let mut i = 0;
    let bytes = html.as_bytes();
    while i < bytes.len() {
        if !in_tag && bytes[i] == b'<' {
            in_tag = true;
            if lower[i..].starts_with("<style") || lower[i..].starts_with("<script") {
                in_style_or_script = true;
            } else if lower[i..].starts_with("</style") || lower[i..].starts_with("</script") {
                in_style_or_script = false;
            }
        } else if in_tag && bytes[i] == b'>' {
            in_tag = false;
        } else if !in_tag && !in_style_or_script {
            out.push(bytes[i] as char);
        }
        i += 1;
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn parse_address(raw: &str) -> Option<(String, String)> {
    // « Prénom Nom <adresse@x.fr> » ou « adresse@x.fr »
    if let Some(open) = raw.find('<') {
        let name = raw[..open].trim().trim_matches('"').to_string();
        let email = raw[open + 1..].trim_end_matches('>').trim().to_lowercase();
        Some((if name.is_empty() { email.clone() } else { name }, email))
    } else {
        let email = raw.trim().to_lowercase();
        if email.contains('@') {
            Some((email.clone(), email))
        } else {
            None
        }
    }
}
