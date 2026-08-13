//! Connecteur Personnes (doc Connecteurs §4) — V1 hors biométrie.
//! Import initial (onboarding) + apprentissage progressif (demande groupée).
//! Graphe social de tiers non-consentants → strictement local, chiffré, effaçable.

use crate::db::{new_id, now, Db};
use crate::error::Result;
use rusqlite::params;
use serde_json::{json, Value};

/// Aperçu via l'API Contacts officielle : aucune lecture directe de la base privée.
pub fn os_contacts_preview() -> Result<Vec<Value>> {
    let mut out = crate::connectors::native::contacts()?;
    out.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    out.dedup_by(|a, b| a["name"] == b["name"]);
    Ok(out)
}

pub fn import_person(
    db: &Db,
    name: &str,
    relationship: Option<&str>,
    email: Option<&str>,
    phone: Option<&str>,
    birthday: Option<&str>,
) -> Result<String> {
    let id = crate::memory::find_or_create_person(db, name, email, phone)?;
    db.with(|c| {
        if let Some(rel) = relationship {
            c.execute(
                "UPDATE people SET relationship=?2 WHERE id=?1",
                params![id, rel],
            )?;
        }
        if let Some(b) = birthday {
            c.execute("UPDATE people SET birthday=?2 WHERE id=?1", params![id, b])?;
        }
        Ok(())
    })?;
    crate::security::log_access(db, "people", "import", Some(name));
    Ok(id)
}

pub fn list_people(db: &Db) -> Result<Vec<Value>> {
    db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT id, name, relationship, comm_channels, last_interaction, birthday FROM people ORDER BY name LIMIT 500",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "relationship": r.get::<_, Option<String>>(2)?,
                "comm_channels": r.get::<_, Option<String>>(3)?
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok()),
                "last_interaction": r.get::<_, Option<i64>>(4)?,
                "birthday": r.get::<_, Option<String>>(5)?,
            }))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

/// `people.context` : contexte disponible via les canaux accessibles UNIQUEMENT.
/// Alimente un brouillon, jamais un envoi automatique (plancher).
pub fn context(db: &Db, name: &str) -> Result<Value> {
    db.with(|c| {
        let person: Option<(String, String, Option<String>, Option<String>)> = c
            .query_row(
                "SELECT id, name, relationship, comm_channels FROM people
                 WHERE lower(name) LIKE '%'||lower(?1)||'%'
                    OR lower(COALESCE(relationship,'')) = lower(?1) LIMIT 1",
                params![name],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .ok();
        let Some((id, pname, relationship, channels)) = person else {
            return Ok(json!({"found": false, "note": format!("Personne « {name} » inconnue. L'utilisateur peut l'ajouter dans Apprendre à Syn.")}));
        };
        let mut linked: Vec<Value> = vec![];
        let mut stmt = c.prepare(
            "SELECT i.title, i.type, i.source_ref, i.mtime FROM person_links pl
             JOIN items i ON i.id = pl.item_id WHERE pl.person_id = ?1 AND i.status='active'
             ORDER BY i.mtime DESC LIMIT 10",
        )?;
        let rows = stmt.query_map(params![id], |r| {
            Ok(json!({
                "title": r.get::<_, Option<String>>(0)?,
                "type": r.get::<_, String>(1)?,
                "source_ref": r.get::<_, String>(2)?,
                "mtime": r.get::<_, Option<i64>>(3)?,
            }))
        })?;
        for r in rows {
            linked.push(r?);
        }
        Ok(json!({
            "found": true,
            "name": pname,
            "relationship": relationship,
            "comm_channels": channels.and_then(|s| serde_json::from_str::<Value>(&s).ok()),
            "linked_items": linked,
        }))
    })
}

pub fn pending_unknowns(db: &Db) -> Result<Vec<Value>> {
    db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT id, name, context, source_ref, created_at FROM unknown_names
             WHERE status='pending' ORDER BY created_at DESC LIMIT 20",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "name": r.get::<_, String>(1)?,
                "context": r.get::<_, Option<String>>(2)?,
                "source_ref": r.get::<_, Option<String>>(3)?,
                "created_at": r.get::<_, i64>(4)?,
            }))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

pub fn label_unknown(
    db: &Db,
    unknown_id: &str,
    name: &str,
    relationship: Option<&str>,
) -> Result<()> {
    let pid = crate::memory::find_or_create_person(db, name, None, None)?;
    db.with(|c| {
        if let Some(rel) = relationship {
            c.execute(
                "UPDATE people SET relationship=?2 WHERE id=?1",
                params![pid, rel],
            )?;
        }
        c.execute(
            "UPDATE unknown_names SET status='labeled' WHERE id=?1",
            params![unknown_id],
        )?;
        Ok(())
    })
}

pub fn add_fact_about(db: &Db, text: &str) -> Result<String> {
    let id = new_id();
    db.with(|c| {
        c.execute(
            "INSERT INTO items (id, source, source_ref, type, title, body, ingested_at, status)
             VALUES (?1, 'conversation', ?2, 'fact', ?3, ?4, ?5, 'active')",
            params![
                id,
                format!("fact:{id}"),
                text.chars().take(80).collect::<String>(),
                text,
                now()
            ],
        )?;
        Ok(())
    })?;
    Ok(id)
}
