//! Connecteur Calendrier : EventKit sur macOS sous consentement explicite ;
//! stockage Syn local en repli sur les plateformes sans pont natif.

use crate::db::{new_id, Db};
use crate::error::{AppError, Result};
use crate::tools::parse_iso;
use rusqlite::params;
use serde_json::{json, Value};
use std::collections::HashSet;

/// Miroir local de l'agenda natif → table `events`, dédupliqué par identifiant.
/// Sans ce miroir, la proactivité « événement imminent » ne voyait jamais rien
/// sur macOS : la table restait vide à vie (audit §3).
pub fn sync_native_to_db(db: &Db) -> Result<usize> {
    if !cfg!(target_os = "macos") {
        return Ok(0);
    }
    let now = crate::db::now();
    let from = now - 86_400;
    let to = now + 30 * 86_400;
    let events = crate::connectors::native::calendar_events(from, to)?;
    let native_ids: HashSet<String> = events
        .iter()
        .filter_map(|event| event["id"].as_str().map(str::to_string))
        .collect();
    let mut count = 0usize;
    db.with(|c| {
        for ev in &events {
            let native_id = ev["id"].as_str().unwrap_or("");
            if native_id.is_empty() {
                continue;
            }
            let title = ev["title"].as_str().unwrap_or("");
            let start = ev["start"].as_f64().map(|f| f as i64);
            let end = ev["end"].as_f64().map(|f| f as i64);
            let location = ev["location"].as_str();
            c.execute(
                "INSERT INTO events (id, source, source_ref, title, \"start\", \"end\", location)
                 VALUES (?1, 'apple', ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(source_ref) DO UPDATE SET
                   title=excluded.title, \"start\"=excluded.\"start\",
                   \"end\"=excluded.\"end\", location=excluded.location",
                params![new_id(), native_id, title, start, end, location],
            )?;
            count += 1;
        }
        let mut stmt = c.prepare(
            "SELECT id, source_ref FROM events
             WHERE source='apple' AND \"start\">=?1 AND \"start\"<=?2 AND source_ref IS NOT NULL",
        )?;
        let mirrored = stmt
            .query_map(params![from, to], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        drop(stmt);
        for (id, source_ref) in mirrored {
            if !native_ids.contains(&source_ref) {
                c.execute("DELETE FROM events WHERE id=?1", [id])?;
            }
        }
        Ok(())
    })?;
    Ok(count)
}

pub fn list_range(db: &Db, from: &str, to: &str) -> Result<Vec<Value>> {
    let from_ts = parse_iso(from).unwrap_or(0);
    let to_ts = parse_iso(to).unwrap_or(i64::MAX);
    if cfg!(target_os = "macos") {
        return crate::connectors::native::calendar_events(from_ts, to_ts);
    }
    db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT id, title, \"start\", \"end\", location, attendees, notes, source
             FROM events WHERE \"start\" >= ?1 AND \"start\" <= ?2 ORDER BY \"start\" LIMIT 200",
        )?;
        let rows = stmt.query_map(params![from_ts, to_ts], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "title": r.get::<_, String>(1)?,
                "start": r.get::<_, i64>(2)?,
                "end": r.get::<_, Option<i64>>(3)?,
                "location": r.get::<_, Option<String>>(4)?,
                "attendees": r.get::<_, Option<String>>(5)?
                    .and_then(|s| serde_json::from_str::<Value>(&s).ok())
                    .unwrap_or(json!([])),
                // Notes d'événement = donnée non fiable (vecteur d'injection).
                "notes": r.get::<_, Option<String>>(6)?,
                "source": r.get::<_, String>(7)?,
            }))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

pub fn create(db: &Db, args: &Value) -> Result<Value> {
    let title = args["title"]
        .as_str()
        .ok_or(AppError::Invalid("titre requis".into()))?;
    let start = args["start"]
        .as_str()
        .and_then(parse_iso)
        .ok_or(AppError::Invalid(
            "date de début invalide (ISO 8601 attendu)".into(),
        ))?;
    let end = args["end"].as_str().and_then(parse_iso);
    let attendees = args.get("attendees").cloned().unwrap_or(json!([]));
    if cfg!(target_os = "macos") {
        if attendees
            .as_array()
            .is_some_and(|values| !values.is_empty())
        {
            return Err(AppError::Invalid(
                "Syn ne peut pas encore ajouter ni inviter des participants à un événement dans cette version.".into(),
            ));
        }
        let created = crate::connectors::native::calendar_create(
            title,
            start,
            end.unwrap_or(start + 3600),
            args["location"].as_str().unwrap_or(""),
        )?;
        // Miroir immédiat pour la proactivité (la passe périodique complétera).
        if let Some(native_id) = created["id"].as_str().filter(|s| !s.is_empty()) {
            let _ = db.with(|c| {
                c.execute(
                    "INSERT INTO events (id, source, source_ref, title, \"start\", \"end\", location)
                     VALUES (?1, 'apple', ?2, ?3, ?4, ?5, ?6)
                     ON CONFLICT(source_ref) DO NOTHING",
                    params![
                        new_id(),
                        native_id,
                        title,
                        start,
                        end.unwrap_or(start + 3600),
                        args["location"].as_str()
                    ],
                )?;
                Ok(())
            });
        }
        return Ok(created);
    }
    let id = new_id();
    db.with(|c| {
        c.execute(
            "INSERT INTO events (id, source, title, \"start\", \"end\", location, attendees, notes)
             VALUES (?1, 'local', ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                id,
                title,
                start,
                end,
                args["location"].as_str(),
                attendees.to_string(),
                args["notes"].as_str()
            ],
        )?;
        Ok(())
    })?;
    Ok(json!({"id": id, "title": title, "start": start, "end": end, "attendees": attendees}))
}

pub fn today_events(db: &Db) -> Result<Vec<Value>> {
    let now = chrono::Local::now();
    let start = now
        .date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_utc()
        .timestamp()
        - now.offset().local_minus_utc() as i64;
    let end = start + 86_400;
    if cfg!(target_os = "macos") {
        return crate::connectors::native::calendar_events(start, end);
    }
    db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT id, title, \"start\", location FROM events
             WHERE \"start\" >= ?1 AND \"start\" < ?2 ORDER BY \"start\"",
        )?;
        let rows = stmt.query_map(params![start, end], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "title": r.get::<_, String>(1)?,
                "start": r.get::<_, i64>(2)?,
                "location": r.get::<_, Option<String>>(3)?,
            }))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}
