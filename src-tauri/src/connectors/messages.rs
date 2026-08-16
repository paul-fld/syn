//! Connecteur Messages (iMessage/SMS) : lecture seule de la base locale
//! `~/Library/Messages/chat.db` (nécessite l'Accès complet au disque, déjà
//! au cœur du produit). La base est copiée avant lecture pour ne jamais
//! verrouiller celle de Messages. Les messages sont regroupés par
//! correspondant et par mois en items compacts — assez fins pour le
//! retrieval, assez gros pour ne pas noyer les embeddings.

use crate::bus::Bus;
use crate::db::{now, Db};
use crate::error::Result;
use crate::llm::LlmClient;
use crate::memory::{self, Item};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

const APPLE_EPOCH_OFFSET: i64 = 978_307_200; // 2001-01-01 en epoch Unix
const MAX_MESSAGES: usize = 4000;
const MAX_GROUP_CHARS: usize = 7000;

struct TempChatCopy(PathBuf);

impl Drop for TempChatCopy {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn chat_db_path() -> Option<PathBuf> {
    let p = dirs::home_dir()?.join("Library/Messages/chat.db");
    p.exists().then_some(p)
}

pub fn available() -> bool {
    cfg!(target_os = "macos")
        && chat_db_path()
            .map(|p| std::fs::File::open(p).is_ok())
            .unwrap_or(false)
}

fn to_unix(raw: i64) -> i64 {
    // Selon la version de macOS, `message.date` est en secondes ou en nanosecondes
    // depuis 2001. Au-delà de 10^12, c'est forcément des nanosecondes.
    if raw > 1_000_000_000_000 {
        raw / 1_000_000_000 + APPLE_EPOCH_OFFSET
    } else {
        raw + APPLE_EPOCH_OFFSET
    }
}

/// Synchronisation : derniers messages → items `source='messages'`,
/// un item par (correspondant, mois), rattaché à la personne connue si possible.
pub async fn sync(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    bus: &Bus,
    embed_model: &str,
) -> Result<usize> {
    let Some(src) = chat_db_path() else {
        return Ok(0);
    };
    // Copie de travail : jamais d'accès concurrent à la vraie base de Messages.
    let tmp = std::env::temp_dir().join(format!("syn-chatdb-{}", uuid::Uuid::new_v4()));
    if std::fs::create_dir(&tmp).is_err() {
        return Ok(0);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o700));
    }
    let _cleanup = TempChatCopy(tmp.clone());
    let copy = tmp.join("chat.db");
    if std::fs::copy(&src, &copy).is_err() {
        return Ok(0); // FDA absent ou fichier verrouillé : silencieux, on réessaiera
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&copy, std::fs::Permissions::from_mode(0o600));
    }
    for suffix in ["-wal", "-shm"] {
        let extra = src.with_file_name(format!("chat.db{suffix}"));
        if extra.exists() {
            let _ = std::fs::copy(&extra, tmp.join(format!("chat.db{suffix}")));
        }
    }

    // (correspondant, mois) → lignes de conversation datées.
    let mut groups: BTreeMap<(String, String), Vec<String>> = BTreeMap::new();
    {
        let conn = match rusqlite::Connection::open_with_flags(
            &copy,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        ) {
            Ok(c) => c,
            Err(_) => return Ok(0),
        };
        let mut stmt = match conn.prepare(
            "SELECT COALESCE(h.id, 'inconnu'), m.text, m.date, m.is_from_me
             FROM message m LEFT JOIN handle h ON m.handle_id = h.ROWID
             WHERE m.text IS NOT NULL AND length(m.text) > 0
             ORDER BY m.date DESC LIMIT ?1",
        ) {
            Ok(s) => s,
            Err(_) => return Ok(0),
        };
        let rows = stmt
            .query_map([MAX_MESSAGES as i64], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                ))
            })
            .map_err(|e| crate::error::AppError::Other(format!("chat.db : {e}")))?;
        for row in rows.flatten() {
            let (handle, text, raw_date, from_me) = row;
            let ts = to_unix(raw_date);
            let month = chrono::DateTime::from_timestamp(ts, 0)
                .map(|d| d.format("%Y-%m").to_string())
                .unwrap_or_else(|| "inconnu".into());
            let day = chrono::DateTime::from_timestamp(ts, 0)
                .map(|d| d.format("%d/%m %H:%M").to_string())
                .unwrap_or_default();
            let who = if from_me == 1 { "moi" } else { "eux" };
            groups
                .entry((handle, month))
                .or_default()
                .push(format!("[{day}] {who} : {text}"));
        }
    }
    let mut count = 0usize;
    for ((handle, month), mut lines) in groups {
        lines.reverse(); // ordre chronologique
        let mut body = String::new();
        for l in &lines {
            if body.len() + l.len() + 1 > MAX_GROUP_CHARS {
                break;
            }
            body.push_str(l);
            body.push('\n');
        }
        if body.is_empty() {
            continue;
        }
        let source_ref = format!("messages://{handle}/{month}");
        let item = Item {
            id: String::new(),
            source: "messages".into(),
            source_ref: source_ref.clone(),
            r#type: "message".into(),
            title: Some(format!("Messages avec {handle} — {month}")),
            body: Some(body.clone()),
            created_at: None,
            ingested_at: now(),
            hash: Some(blake3::hash(body.as_bytes()).to_hex().to_string()),
            path: None,
            mime: Some("text/plain".into()),
            size: Some(body.len() as i64),
            mtime: None,
            status: "active".into(),
        };
        // Incrémental : même contenu → même hash → skip.
        if memory::item_hash(db, "messages", &source_ref)?.as_deref() == item.hash.as_deref() {
            continue;
        }
        let item_id =
            crate::ingestion::ingest_item(db, llm, bus, embed_model, item, Some(&body)).await?;
        // Rattachement à une personne connue par numéro/email.
        if let Some(person_id) = memory::find_person_by_channel(db, &handle)? {
            memory::link_person(db, &item_id, &person_id)?;
        }
        count += 1;
    }
    if count > 0 {
        crate::connectors::set_status(db, "messages", "messages", "connected")?;
    }
    crate::security::log_access(db, "messages", "sync", None);
    Ok(count)
}
