//! Connecteur Calendrier : EventKit sur macOS sous consentement explicite ;
//! stockage Syn local en repli sur les plateformes sans pont natif.

use crate::db::{new_id, Db};
use crate::error::{AppError, Result};
use crate::tools::parse_iso;
use rusqlite::params;
use serde_json::{json, Value};

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
                "EventKit ne permet pas à Syn d’ajouter des invités. Connecte Google ou Microsoft pour envoyer des invitations.".into(),
            ));
        }
        return crate::connectors::native::calendar_create(
            title,
            start,
            end.unwrap_or(start + 3600),
            args["location"].as_str().unwrap_or(""),
        );
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
