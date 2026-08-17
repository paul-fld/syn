//! Mémoire (Intelligence §7) : working (conversations), épisodique (events, tasks,
//! commitments, actions), sémantique (items + embeddings). Les faits personnels
//! vivent ici, jamais dans les poids du modèle.

use crate::db::{new_id, now, Db};
use crate::error::Result;
use rusqlite::params;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Item {
    pub id: String,
    pub source: String,
    pub source_ref: String,
    pub r#type: String,
    pub title: Option<String>,
    pub body: Option<String>,
    pub created_at: Option<i64>,
    pub ingested_at: i64,
    pub hash: Option<String>,
    pub path: Option<String>,
    pub mime: Option<String>,
    pub size: Option<i64>,
    pub mtime: Option<i64>,
    pub status: String,
}

/// Upsert d'un item par (source, source_ref). Renvoie (id, contenu_changé).
pub fn upsert_item(db: &Db, it: &Item) -> Result<(String, bool)> {
    db.with(|c| {
        let existing: Option<(String, Option<String>)> = c
            .query_row(
                "SELECT id, hash FROM items WHERE source = ?1 AND source_ref = ?2",
                params![it.source, it.source_ref],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .or_else(|e| {
                if e == rusqlite::Error::QueryReturnedNoRows {
                    Ok(None)
                } else {
                    Err(e)
                }
            })?;

        match existing {
            Some((id, old_hash)) => {
                let changed = old_hash != it.hash;
                c.execute(
                    "UPDATE items SET type=?2, title=?3, body=?4, created_at=?5, ingested_at=?6,
                     hash=?7, path=?8, mime=?9, size=?10, mtime=?11, status='active' WHERE id=?1",
                    params![
                        id,
                        it.r#type,
                        it.title,
                        it.body,
                        it.created_at,
                        it.ingested_at,
                        it.hash,
                        it.path,
                        it.mime,
                        it.size,
                        it.mtime
                    ],
                )?;
                Ok((id, changed))
            }
            None => {
                let id = new_id();
                c.execute(
                    "INSERT INTO items (id, source, source_ref, type, title, body, created_at,
                     ingested_at, hash, path, mime, size, mtime, status)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,'active')",
                    params![
                        id,
                        it.source,
                        it.source_ref,
                        it.r#type,
                        it.title,
                        it.body,
                        it.created_at,
                        it.ingested_at,
                        it.hash,
                        it.path,
                        it.mime,
                        it.size,
                        it.mtime
                    ],
                )?;
                Ok((id, true))
            }
        }
    })
}

pub fn item_hash(db: &Db, source: &str, source_ref: &str) -> Result<Option<String>> {
    db.read(|c| {
        c.query_row(
            "SELECT hash FROM items WHERE source=?1 AND source_ref=?2 AND status='active'",
            params![source, source_ref],
            |r| r.get::<_, Option<String>>(0),
        )
        .or_else(|e| {
            if e == rusqlite::Error::QueryReturnedNoRows {
                Ok(None)
            } else {
                Err(e)
            }
        })
        .map_err(Into::into)
    })
}

pub fn mark_removed(db: &Db, source: &str, source_ref: &str) -> Result<()> {
    db.with(|c| {
        c.execute(
            "UPDATE items SET status='removed' WHERE source=?1 AND source_ref=?2",
            params![source, source_ref],
        )?;
        Ok(())
    })
}

/// Renommage/déplacement : mise à jour du source_ref sans réembedder (Files §5).
pub fn rename_item(
    db: &Db,
    source: &str,
    old_ref: &str,
    new_ref: &str,
    new_path: &str,
) -> Result<bool> {
    db.with(|c| {
        let n = c.execute(
            "UPDATE items SET source_ref=?3, path=?4 WHERE source=?1 AND source_ref=?2",
            params![source, old_ref, new_ref, new_path],
        )?;
        Ok(n > 0)
    })
}

pub fn replace_embeddings(
    db: &Db,
    item_id: &str,
    model: &str,
    chunks: &[(String, Option<Vec<u8>>)],
) -> Result<()> {
    db.with(|c| {
        c.execute("DELETE FROM embeddings WHERE item_id = ?1", params![item_id])?;
        let mut stmt = c.prepare(
            "INSERT INTO embeddings (item_id, model, chunk_index, text, vector) VALUES (?1,?2,?3,?4,?5)",
        )?;
        for (i, (text, vec)) in chunks.iter().enumerate() {
            stmt.execute(params![item_id, model, i as i64, text, vec])?;
        }
        Ok(())
    })
}

// ————— Conversations (working memory) —————

pub fn ensure_session(db: &Db, session_id: &str, first_msg: &str) -> Result<()> {
    db.with(|c| {
        let title: String = first_msg.chars().take(64).collect();
        c.execute(
            "INSERT INTO sessions (id, title, created_at, updated_at) VALUES (?1,?2,?3,?3)
             ON CONFLICT(id) DO UPDATE SET updated_at = ?3",
            params![session_id, title, now()],
        )?;
        Ok(())
    })
}

pub fn persist_turn(db: &Db, session_id: &str, role: &str, content: &str) -> Result<()> {
    db.with(|c| {
        let next: i64 = c.query_row(
            "SELECT COALESCE(MAX(turn), -1) + 1 FROM conversations WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )?;
        c.execute(
            "INSERT INTO conversations (session_id, turn, role, content, created_at) VALUES (?1,?2,?3,?4,?5)",
            params![session_id, next, role, content, now()],
        )?;
        Ok(())
    })
}

/// Résumé de long terme d'une session : les tours trop anciens pour tenir dans
/// la fenêtre sont condensés une fois pour toutes (mémoire de travail, doc §13).
pub fn session_summary(db: &Db, session_id: &str) -> Result<Option<String>> {
    db.read(|c| {
        Ok(c.query_row(
            "SELECT summary FROM sessions WHERE id=?1",
            params![session_id],
            |r| r.get(0),
        )
        .unwrap_or(None))
    })
}

pub fn set_session_summary(db: &Db, session_id: &str, summary: &str) -> Result<()> {
    db.with(|c| {
        c.execute(
            "UPDATE sessions SET summary=?2 WHERE id=?1",
            params![session_id, summary],
        )?;
        Ok(())
    })
}

pub fn turn_count(db: &Db, session_id: &str) -> Result<i64> {
    db.read(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM conversations WHERE session_id=?1 AND role IN ('user','assistant')",
            params![session_id],
            |r| r.get(0),
        )
        .unwrap_or(0))
    })
}

/// Les tours ANTÉRIEURS à la fenêtre récente (matière du résumé), bornés.
pub fn older_turns(
    db: &Db,
    session_id: &str,
    skip_recent: usize,
    cap: usize,
) -> Result<Vec<(String, String)>> {
    db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT role, content FROM (
               SELECT role, content, turn FROM conversations
               WHERE session_id = ?1 AND role IN ('user','assistant')
               ORDER BY turn DESC LIMIT ?2 OFFSET ?3
             ) ORDER BY turn ASC",
        )?;
        let rows = stmt.query_map(params![session_id, cap as i64, skip_recent as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

pub fn recent_turns(db: &Db, session_id: &str, limit: usize) -> Result<Vec<(String, String)>> {
    db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT role, content FROM (
               SELECT role, content, turn FROM conversations
               WHERE session_id = ?1 AND role IN ('user','assistant')
               ORDER BY turn DESC LIMIT ?2
             ) ORDER BY turn ASC",
        )?;
        let rows = stmt.query_map(params![session_id, limit as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

/// Historique des autres conversations du même projet. Ce texte sera injecté
/// sous provenance non fiable : il apporte la continuité sans rejouer d'anciennes consignes.
pub fn project_context(
    db: &Db,
    session_id: &str,
    limit: usize,
) -> Result<Option<(String, String, String)>> {
    db.read(|c| {
        let project: Option<(String, String)> = c
            .query_row(
                "SELECT p.id, p.name FROM sessions s JOIN projects p ON p.id=s.project_id
                 WHERE s.id=?1",
                [session_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map(Some)
            .or_else(|e| {
                if e == rusqlite::Error::QueryReturnedNoRows {
                    Ok(None)
                } else {
                    Err(e)
                }
            })?;
        let Some((project_id, project_name)) = project else {
            return Ok(None);
        };
        let mut stmt = c.prepare(
            "SELECT COALESCE(s.title, 'Sans titre'), c.role, c.content
             FROM conversations c JOIN sessions s ON s.id=c.session_id
             WHERE s.project_id=?1 AND s.id<>?2 AND c.role IN ('user','assistant')
             ORDER BY c.created_at DESC LIMIT ?3",
        )?;
        let rows = stmt.query_map(params![project_id, session_id, limit as i64], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?;
        let mut turns = vec![];
        for row in rows {
            turns.push(row?);
        }
        if turns.is_empty() {
            return Ok(None);
        }
        turns.reverse();
        let mut text = String::new();
        for (title, role, content) in turns {
            let speaker = if role == "user" { "Utilisateur" } else { "Syn" };
            let line = format!("[Conversation : {title}] {speaker} : {content}\n");
            if text.len() + line.len() > 7_000 {
                break;
            }
            text.push_str(&line);
        }
        Ok(Some((project_id, project_name, text)))
    })
}

// ————— Tâches —————

pub fn create_task(
    db: &Db,
    title: &str,
    due: Option<i64>,
    priority: Option<&str>,
    source: &str,
) -> Result<String> {
    let id = new_id();
    db.with(|c| {
        c.execute(
            "INSERT INTO tasks (id, title, due, status, priority, source) VALUES (?1,?2,?3,'open',?4,?5)",
            params![id, title, due, priority, source],
        )?;
        Ok(())
    })?;
    Ok(id)
}

// ————— Personnes —————

pub fn find_or_create_person(
    db: &Db,
    name: &str,
    email: Option<&str>,
    phone: Option<&str>,
) -> Result<String> {
    db.with(|c| {
        let found: Option<String> = c
            .query_row(
                "SELECT id FROM people WHERE lower(name) = lower(?1)
                 OR (comm_channels IS NOT NULL AND ?2 IS NOT NULL AND comm_channels LIKE '%' || lower(?2) || '%')
                 LIMIT 1",
                params![name, email],
                |r| r.get(0),
            )
            .or_else(|e| if e == rusqlite::Error::QueryReturnedNoRows { Ok(None) } else { Err(e) })
            .map(|x: Option<String>| x)
            .unwrap_or(None);
        if let Some(id) = found {
            if email.is_some() || phone.is_some() {
                let channels: Option<String> = c
                    .query_row("SELECT comm_channels FROM people WHERE id=?1", params![&id], |r| r.get(0))
                    .unwrap_or(None);
                let mut v: Value = channels
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_else(|| serde_json::json!({"emails": [], "phones": []}));
                // Un JSON valide mais mal formé (emails/phones non-tableaux)
                // ne doit pas faire paniquer l'app (audit §3) : on répare.
                for key in ["emails", "phones"] {
                    if !v[key].is_array() {
                        v[key] = serde_json::json!([]);
                    }
                }
                if let (Some(e), Some(arr)) = (email, v["emails"].as_array_mut()) {
                    let e = e.to_lowercase();
                    if !arr.iter().any(|x| x.as_str() == Some(&e)) {
                        arr.push(Value::String(e));
                    }
                }
                if let (Some(p), Some(arr)) = (phone, v["phones"].as_array_mut()) {
                    if !arr.iter().any(|x| x.as_str() == Some(p)) {
                        arr.push(Value::String(p.to_string()));
                    }
                }
                c.execute("UPDATE people SET comm_channels=?2 WHERE id=?1", params![&id, v.to_string()])?;
            }
            return Ok(id);
        }
        let id = new_id();
        let channels = serde_json::json!({
            "emails": email.map(|e| vec![e.to_lowercase()]).unwrap_or_default(),
            "phones": phone.map(|p| vec![p.to_string()]).unwrap_or_default(),
        });
        c.execute(
            "INSERT INTO people (id, name, comm_channels, last_interaction) VALUES (?1,?2,?3,?4)",
            params![id, name, channels.to_string(), now()],
        )?;
        Ok(id)
    })
}

/// Retrouve une personne par canal de communication (email exact, ou numéro
/// de téléphone comparé sur ses 9 derniers chiffres pour absorber les formats
/// « +33 6… » vs « 06… »).
pub fn find_person_by_channel(db: &Db, handle: &str) -> Result<Option<String>> {
    let handle_lower = handle.to_lowercase();
    let handle_digits: String = handle.chars().filter(|c| c.is_ascii_digit()).collect();
    let handle_tail: String = handle_digits
        .chars()
        .rev()
        .take(9)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT id, COALESCE(comm_channels,'') FROM people WHERE comm_channels IS NOT NULL",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows.flatten() {
            let (id, channels) = row;
            let v: Value = serde_json::from_str(&channels).unwrap_or(Value::Null);
            let emails = v["emails"].as_array().cloned().unwrap_or_default();
            if emails
                .iter()
                .any(|e| e.as_str().map(|s| s.to_lowercase()) == Some(handle_lower.clone()))
            {
                return Ok(Some(id));
            }
            if !handle_tail.is_empty() && handle_tail.len() >= 6 {
                let phones = v["phones"].as_array().cloned().unwrap_or_default();
                for p in phones {
                    let digits: String = p
                        .as_str()
                        .unwrap_or("")
                        .chars()
                        .filter(|c| c.is_ascii_digit())
                        .collect();
                    if !digits.is_empty()
                        && (digits.ends_with(&handle_tail)
                            || handle_digits.ends_with(
                                &digits
                                    .chars()
                                    .rev()
                                    .take(9)
                                    .collect::<Vec<_>>()
                                    .into_iter()
                                    .rev()
                                    .collect::<String>(),
                            ))
                    {
                        return Ok(Some(id));
                    }
                }
            }
        }
        Ok(None)
    })
}

pub fn link_person(db: &Db, item_id: &str, person_id: &str) -> Result<()> {
    db.with(|c| {
        c.execute(
            "INSERT OR IGNORE INTO person_links (item_id, person_id) VALUES (?1,?2)",
            params![item_id, person_id],
        )?;
        Ok(())
    })
}

/// Nom inconnu rencontré → file d'apprentissage progressif (demande groupée).
pub fn queue_unknown_name(db: &Db, name: &str, context: &str, source_ref: &str) -> Result<()> {
    db.with(|c| {
        let known: bool = c
            .query_row(
                "SELECT 1 FROM people WHERE lower(name)=lower(?1)",
                params![name],
                |_| Ok(true),
            )
            .unwrap_or(false);
        let queued: bool = c
            .query_row(
                "SELECT 1 FROM unknown_names WHERE lower(name)=lower(?1) AND status='pending'",
                params![name],
                |_| Ok(true),
            )
            .unwrap_or(false);
        if !known && !queued {
            c.execute(
                "INSERT INTO unknown_names (id, name, context, source_ref, status, created_at)
                 VALUES (?1,?2,?3,?4,'pending',?5)",
                params![new_id(), name, context, source_ref, now()],
            )?;
        }
        Ok(())
    })
}
