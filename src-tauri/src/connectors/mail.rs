//! Connecteur Mail (doc Connecteurs §1).
//! Voie A [ce build] : boîte native Apple Mail, lecture seule du stock local
//! (`~/Library/Mail`, .emlx) sous « Accès complet au disque ». Ne voit que le
//! synchronisé ; pas d'envoi par cette voie.
//! Voie B [🔎 config OAuth requise] : Gmail/Graph/IMAP — vérification d'app
//! fournisseur + audit CASA à anticiper. Corps de mail = donnée non fiable.

use crate::bus::Bus;
use crate::db::{now, Db};
use rusqlite::params;
use crate::error::{AppError, Result};
use crate::ingestion;
use crate::llm::LlmClient;
use crate::memory::{self, Item};
use std::collections::HashSet;
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

/// Canaux d'envoi réellement utilisables, avec leur libellé.
///
/// Le défaut historique était « Apple Mail », choisi sans vérifier qu'il soit
/// disponible : sur une application non signée, ou sans autorisation
/// d'automatisation, l'envoi échouait alors que des comptes Google et Microsoft
/// étaient connectés et prêts. Une adresse d'expéditeur n'est pas un détail
/// technique — c'est l'utilisateur qui la choisit.
pub fn available_channels(db: &Db) -> Vec<(&'static str, &'static str)> {
    let mut channels = Vec::new();
    if cfg!(target_os = "macos") && native_available() {
        channels.push(("apple", "Apple Mail"));
    }
    if super::is_connected(db, "google") {
        channels.push(("google", "Gmail"));
    }
    if super::is_connected(db, "microsoft") {
        channels.push(("microsoft", "Outlook"));
    }
    channels
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
    // Ne pas tronquer ici : si les 800 plus récents sont déjà connus, une
    // troncature empêche à vie l'indexation des messages plus anciens.
    let known: HashSet<String> = db.with(|c| {
        let mut stmt =
            c.prepare("SELECT source_ref FROM items WHERE source='mail' AND status='active'")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        let mut out = HashSet::new();
        for row in rows {
            out.insert(row?);
        }
        Ok(out)
    })?;

    let mut count = 0usize;
    for (path, mtime) in emlx {
        if !crate::connectors::is_connected(db, "apple") {
            break;
        }
        let source_ref = path.to_string_lossy().to_string();
        // Incrémental : déjà connu → skip.
        if known.contains(&source_ref) {
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
        // Le contenu d'un mail est une donnée externe non fiable. Il peut
        // enrichir la recherche et les liens vers des personnes, mais ne doit
        // jamais créer silencieusement un engagement « dû par moi » à partir
        // d'une phrase reçue ou d'une signature transférée.
        count += 1;
        if count >= MAX_MAILS_PER_SYNC {
            break;
        }
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
    let mut in_style_or_script = false;
    let mut chars = html.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '<' {
            let mut tag = String::new();
            for tag_ch in chars.by_ref() {
                if tag_ch == '>' {
                    break;
                }
                if tag.len() < 32 {
                    tag.push(tag_ch);
                }
            }
            let tag = tag.trim().to_lowercase();
            if tag.starts_with("style") || tag.starts_with("script") {
                in_style_or_script = true;
            } else if tag.starts_with("/style") || tag.starts_with("/script") {
                in_style_or_script = false;
            } else if !in_style_or_script
                && (tag.starts_with("br")
                    || tag.starts_with("/p")
                    || tag.starts_with("/div")
                    || tag.starts_with("/li"))
            {
                out.push('\n');
            }
        } else if !in_style_or_script {
            out.push(ch);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn le_nettoyage_html_preserve_lutf8_et_ignore_les_scripts() {
        let html = "<p>Échéance : décembre</p><script>faux TODO</script><div>À bientôt 👋</div>";
        assert_eq!(strip_html(html), "Échéance : décembre À bientôt 👋");
    }
}

/// Envoi en cours de construction, accumulé sur plusieurs tours.
///
/// Le modèle oublie : après avoir demandé le compte d'envoi, il redemandait le
/// contenu que l'utilisateur venait de donner deux tours plus tôt. Ce n'est pas
/// une faiblesse qu'on corrige par une consigne — on la corrige en tenant l'état
/// hors du modèle. Seuls des ARGUMENTS D'OUTIL structurés entrent ici, jamais
/// une phrase interprétée.
#[derive(Debug, Clone, PartialEq)]
pub struct Composition {
    pub recipient: String,
    pub subject: String,
    pub body: String,
    pub via: String,
    /// « validated » quand le texte vient de l'utilisateur ou qu'il l'a relu,
    /// « draft » quand Syn vient de le rédiger et attend son accord.
    pub body_state: String,
    /// « resolved » quand l'adresse sort du carnet d'adresses de l'utilisateur,
    /// « model » quand elle vient d'un appel d'outil — donc à vérifier.
    pub recipient_source: String,
}

impl Default for Composition {
    fn default() -> Self {
        Composition {
            recipient: String::new(),
            subject: String::new(),
            body: String::new(),
            via: String::new(),
            body_state: "validated".into(),
            recipient_source: "model".into(),
        }
    }
}

impl Composition {
    pub fn is_empty(&self) -> bool {
        self.recipient.is_empty()
            && self.subject.is_empty()
            && self.body.is_empty()
            && self.via.is_empty()
    }

    /// Le texte proposé attend-il la relecture de l'utilisateur ?
    pub fn awaits_approval(&self) -> bool {
        !self.body.is_empty() && self.body_state == "draft"
    }

    /// L'adresse a-t-elle été trouvée par Syn dans le carnet de l'utilisateur ?
    /// Elle est alors légitime par construction : elle ne vient ni du modèle,
    /// ni d'un contenu observé.
    pub fn recipient_is_resolved(&self) -> bool {
        !self.recipient.is_empty() && self.recipient_source == "resolved"
    }

    /// Ce qu'il reste à savoir avant de pouvoir proposer l'envoi.
    pub fn missing(&self) -> Vec<&'static str> {
        let mut missing = Vec::new();
        if self.recipient.is_empty() {
            missing.push("destinataire");
        }
        if self.body.is_empty() {
            missing.push("contenu");
        }
        missing
    }
}

pub fn composition(db: &Db, session_id: &str) -> Result<Composition> {
    db.read(|c| {
        Ok(c.query_row(
            "SELECT COALESCE(recipient,''),COALESCE(subject,''),COALESCE(body,''),COALESCE(via,''),
                    COALESCE(body_state,'validated'),COALESCE(recipient_source,'model')
             FROM mail_compositions WHERE session_id=?1",
            params![session_id],
            |row| {
                Ok(Composition {
                    recipient: row.get(0)?,
                    subject: row.get(1)?,
                    body: row.get(2)?,
                    via: row.get(3)?,
                    body_state: row.get(4)?,
                    recipient_source: row.get(5)?,
                })
            },
        )
        .unwrap_or_default())
    })
}

/// Combien de messages sont déjà indexés localement (Apple Mail, messages déjà
/// vus). Sert à savoir si une recherche a la moindre chance d'aboutir hors
/// connecteur — et donc à ne promettre que ce que Syn peut tenir.
pub fn indexed_count(db: &Db) -> Result<i64> {
    db.read(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM items WHERE source='mail' AND status='active'",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0))
    })
}

/// Libellé d'un compte d'envoi, tel que l'utilisateur le nomme.
pub fn channel_label(via: &str) -> &'static str {
    match via {
        "google" => "Gmail",
        "microsoft" => "Outlook",
        _ => "Apple Mail",
    }
}

/// Le texte proposé vient d'être relu et accepté : plus rien ne le retient.
pub fn approve_body(db: &Db, session_id: &str) -> Result<Composition> {
    db.with(|c| {
        c.execute(
            "UPDATE mail_compositions SET body_state='validated', updated_at=?2 WHERE session_id=?1",
            params![session_id, now()],
        )?;
        Ok(())
    })?;
    composition(db, session_id)
}

/// Fusionne les champs non vides d'un appel d'outil dans l'état de la session.
/// Un champ vide n'efface jamais un champ déjà connu : le modèle qui rappelle
/// `mail.send` sans le corps ne doit pas faire disparaître le corps.
pub fn remember_composition(db: &Db, session_id: &str, args: &serde_json::Value) -> Result<Composition> {
    let current = composition(db, session_id)?;
    let field = |key: &str, previous: &str| -> String {
        let value = args[key].as_str().unwrap_or("").trim();
        if value.is_empty() {
            previous.to_string()
        } else {
            value.to_string()
        }
    };
    let body = field("body", &current.body);
    // Un texte que Syn vient d'écrire n'a pas encore été lu par l'utilisateur :
    // il attend sa relecture. Tant que le corps ne bouge pas, l'accord déjà
    // donné tient — sinon la question « tu valides ? » se reposerait sans fin.
    let body_state = if body == current.body {
        current.body_state.clone()
    } else {
        "draft".to_string()
    };
    let recipient = field("to", &current.recipient);
    // Une adresse réécrite par un appel d'outil retombe sous contrôle : seule
    // celle que Syn a lui-même résolue garde sa provenance de confiance.
    let recipient_source = if recipient.eq_ignore_ascii_case(&current.recipient) {
        current.recipient_source.clone()
    } else {
        "model".to_string()
    };
    let merged = Composition {
        recipient,
        subject: field("subject", &current.subject),
        body,
        via: field("via", &current.via),
        body_state,
        recipient_source,
    };
    if merged == current {
        return Ok(merged);
    }
    save(db, session_id, &merged)?;
    Ok(merged)
}

/// L'adresse trouvée par `people.resolve_email` dans le carnet de l'utilisateur.
///
/// Elle entre par une porte différente de celle des arguments d'outil, et c'est
/// tout l'intérêt : elle ne vient ni du modèle, ni d'un contenu observé, mais
/// des données de l'utilisateur, pour un nom qu'il a lui-même écrit.
pub fn remember_resolved_recipient(db: &Db, session_id: &str, email: &str) -> Result<Composition> {
    let email = email.trim();
    if !(email.contains('@') && email.contains('.')) {
        return composition(db, session_id);
    }
    let current = composition(db, session_id)?;
    let merged = Composition {
        recipient: email.to_string(),
        recipient_source: "resolved".into(),
        ..current.clone()
    };
    if merged == current {
        return Ok(merged);
    }
    save(db, session_id, &merged)?;
    Ok(merged)
}

fn save(db: &Db, session_id: &str, state: &Composition) -> Result<()> {
    db.with(|c| {
        c.execute(
            "INSERT INTO mail_compositions(session_id,recipient,subject,body,via,body_state,recipient_source,updated_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(session_id) DO UPDATE SET recipient=excluded.recipient,
               subject=excluded.subject, body=excluded.body, via=excluded.via,
               body_state=excluded.body_state, recipient_source=excluded.recipient_source,
               updated_at=excluded.updated_at",
            params![
                session_id,
                state.recipient,
                state.subject,
                state.body,
                state.via,
                state.body_state,
                state.recipient_source,
                now()
            ],
        )?;
        Ok(())
    })
}

/// Le mail est parti : l'état n'a plus lieu d'être. Le garder ferait resurgir
/// un vieux brouillon à la demande suivante.
pub fn clear_composition(db: &Db, session_id: &str) -> Result<()> {
    db.with(|c| {
        c.execute(
            "DELETE FROM mail_compositions WHERE session_id=?1",
            params![session_id],
        )?;
        Ok(())
    })
}
