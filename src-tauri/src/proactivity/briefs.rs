//! Brief de démarrage & débrief (Proactivité §5–6). 100 % local, déterministe
//! (donc explicable), état vide gracieux, surface non-intrusive.

use crate::bus::{Bus, BusEvent};
use crate::connectors::calendar;
use crate::db::{now, Db};
use crate::error::Result;
use rusqlite::params;
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Clone, Serialize)]
pub struct BriefItem {
    pub icon: String, // message | calendar | gmail | mail | clock | flag | gauge
    pub text: String,
    pub sub: Option<String>,
    pub source_ref: Option<String>,
    pub kind: String, // mail | event | task | commitment | system
}

#[derive(Debug, Clone, Serialize)]
pub struct BriefChip {
    pub icon: String, // cake | file
    pub text: String,
    pub source_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Brief {
    pub greeting: String,
    pub items: Vec<BriefItem>,
    pub chips: Vec<BriefChip>,
    pub empty: bool,
    pub generated_at: i64,
}

fn greeting(db: &Db) -> String {
    let settings = crate::settings::load(db).unwrap_or_default();
    match &settings.voice.address_form {
        Some(addr) => format!("Bonjour {addr},"),
        None => "Bonjour,".to_string(),
    }
}

/// Construit le brief (sections configurables ; chaque ligne est explicable).
pub fn build_brief(db: &Db) -> Result<Brief> {
    let settings = crate::settings::load(db)?;
    if !settings.startup_brief_enabled {
        return Ok(Brief {
            greeting: greeting(db),
            items: vec![],
            chips: vec![],
            empty: true,
            generated_at: now(),
        });
    }
    let sections = &settings.brief_sections;
    let mut items: Vec<BriefItem> = vec![];
    let mut chips: Vec<BriefChip> = vec![];

    // Événements du jour.
    if sections.iter().any(|s| s == "events") {
        for ev in calendar::today_events(db)? {
            let start = ev["start"].as_i64().unwrap_or(0);
            let time = chrono::DateTime::from_timestamp(start, 0)
                .map(|dt| dt.with_timezone(&chrono::Local).format("%Hh%M").to_string())
                .unwrap_or_default();
            items.push(BriefItem {
                icon: "calendar".into(),
                text: format!(
                    "Aujourd'hui vous avez {} à {}",
                    ev["title"].as_str().unwrap_or("?"),
                    time
                ),
                sub: ev["location"].as_str().map(String::from),
                source_ref: ev["id"].as_str().map(String::from),
                kind: "event".into(),
            });
        }
    }

    // Mails récents non traités (dernières 24 h).
    if sections.iter().any(|s| s == "mails") {
        let mails: Vec<(String, String, String)> = db.with(|c| {
            let mut stmt = c.prepare(
                "SELECT title, source_ref, COALESCE(substr(body, 1, 200),'') FROM items
                 WHERE source='mail' AND type='email' AND status='active' AND created_at >= ?1
                 ORDER BY created_at DESC LIMIT 3",
            )?;
            let rows = stmt.query_map(params![now() - 86_400], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?;
            let mut out = vec![];
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })?;
        for (title, source_ref, body) in mails {
            let from = body
                .lines()
                .find(|l| l.starts_with("De :"))
                .map(|l| l.trim_start_matches("De :").trim().to_string());
            items.push(BriefItem {
                icon: "gmail".into(),
                text: format!("Mail concernant « {title} »"),
                sub: from,
                source_ref: Some(source_ref),
                kind: "mail".into(),
            });
        }
    }

    // Tâches dues aujourd'hui ou en retard.
    if sections.iter().any(|s| s == "tasks") {
        let tasks: Vec<(String, String)> = db.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, title FROM tasks WHERE status='open' AND due IS NOT NULL AND due <= ?1 ORDER BY due LIMIT 4",
            )?;
            let rows = stmt.query_map(params![now() + 86_400], |r| Ok((r.get(0)?, r.get(1)?)))?;
            let mut out = vec![];
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })?;
        for (id, title) in tasks {
            items.push(BriefItem {
                icon: "clock".into(),
                text: format!("Tâche à faire : {title}"),
                sub: None,
                source_ref: Some(id),
                kind: "task".into(),
            });
        }
    }

    // Engagements ouverts.
    if sections.iter().any(|s| s == "commitments") {
        let commitments: Vec<(String, String)> = db.with(|c| {
            let mut stmt = c.prepare(
                "SELECT id, text FROM commitments WHERE status='open' ORDER BY due IS NULL, due LIMIT 3",
            )?;
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
            let mut out = vec![];
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })?;
        for (id, text) in commitments {
            items.push(BriefItem {
                icon: "flag".into(),
                text: format!("Engagement en cours : {text}"),
                sub: None,
                source_ref: Some(id),
                kind: "commitment".into(),
            });
        }
    }

    // Anniversaires (personnes connues).
    if sections.iter().any(|s| s == "birthdays") {
        let today = chrono::Local::now().format("%m-%d").to_string();
        let people: Vec<String> = db.with(|c| {
            let mut stmt = c.prepare(
                "SELECT name FROM people WHERE birthday IS NOT NULL AND (birthday = ?1 OR substr(birthday, 6) = ?1)",
            )?;
            let rows = stmt.query_map(params![today], |r| r.get(0))?;
            let mut out = vec![];
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })?;
        for name in people {
            chips.push(BriefChip {
                icon: "cake".into(),
                text: format!("C'est l'anniversaire de {name} aujourd'hui"),
                source_ref: None,
            });
        }
    }

    // Reprendre le travail : document le plus récemment modifié.
    if sections.iter().any(|s| s == "continue") {
        let recent: Option<(String, String)> = db.with(|c| {
            Ok(c.query_row(
                "SELECT title, source_ref FROM items
                 WHERE source='files' AND type='document' AND status='active' AND mtime >= ?1
                 ORDER BY mtime DESC LIMIT 1",
                params![now() - 3 * 86_400],
                |r| {
                    Ok((
                        r.get::<_, Option<String>>(0)?.unwrap_or_default(),
                        r.get(1)?,
                    ))
                },
            )
            .ok())
        })?;
        if let Some((title, source_ref)) = recent {
            if !title.is_empty() {
                chips.push(BriefChip {
                    icon: "file".into(),
                    text: format!("Continuer de travailler sur “{title}”"),
                    source_ref: Some(source_ref),
                });
            }
        }
    }

    // Note système éventuelle.
    if sections.iter().any(|s| s == "system") {
        let snapshot = crate::connectors::system::snapshot();
        let diag = crate::connectors::system::diagnose(&snapshot);
        if !diag.starts_with("Rien d'anormal") {
            items.push(BriefItem {
                icon: "gauge".into(),
                text: diag,
                sub: None,
                source_ref: None,
                kind: "system".into(),
            });
        }
    }

    let empty = items.is_empty() && chips.is_empty();
    Ok(Brief {
        greeting: greeting(db),
        items,
        chips,
        empty,
        generated_at: now(),
    })
}

/// Gate du brief de démarrage (Proactivité §5) :
/// jour différent + pas d'activité significative + après l'heure-plancher.
pub async fn maybe_generate_startup_brief(db: &Db, bus: &Bus) -> Result<()> {
    let mut settings = crate::settings::load(db)?;
    if !settings.startup_brief_enabled {
        return Ok(());
    }
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    if settings.last_brief_date == today {
        return Ok(());
    }
    use chrono::Timelike;
    if chrono::Local::now().hour() < settings.brief_floor_hour as u32 {
        return Ok(()); // évite le brief à 2 h du matin
    }
    // « Aucune activité significative aujourd'hui » : pas de tour de conversation.
    let today_start = super::today_start_ts();
    let active: bool = db.with(|c| {
        Ok(c.query_row(
            "SELECT 1 FROM conversations WHERE created_at >= ?1 AND role='user' LIMIT 1",
            params![today_start],
            |_| Ok(true),
        )
        .unwrap_or(false))
    })?;
    if active {
        // L'utilisateur a déjà commencé sa journée avec Syn : pas de brief rétroactif.
        settings.last_brief_date = today;
        crate::settings::save(db, &settings)?;
        return Ok(());
    }

    settings.last_brief_date = today;
    crate::settings::save(db, &settings)?;
    db.with(|c| {
        c.execute(
            "INSERT INTO proactive_log (id, kind, reason, body, priority, surfaced_at)
             VALUES (?1, 'brief', 'Nouveau jour, première ouverture après l''heure-plancher', 'Brief de démarrage', 'info', ?2)",
            params![crate::db::new_id(), now()],
        )?;
        Ok(())
    })?;
    bus.emit(BusEvent::BriefReady);
    Ok(())
}

/// Débrief de fin de journée : bouclé / glissé / promesses non tenues.
pub fn build_daily_wrap(db: &Db) -> Result<Value> {
    let today_start = super::today_start_ts();
    let done: Vec<String> = db.with(|c| {
        let mut stmt =
            c.prepare("SELECT title FROM tasks WHERE status='done' ORDER BY rowid DESC LIMIT 10")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;
    let pending: Vec<String> = db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT title FROM tasks WHERE status='open' ORDER BY due IS NULL, due LIMIT 10",
        )?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;
    let commitments: Vec<String> = db.with(|c| {
        let mut stmt = c.prepare("SELECT text FROM commitments WHERE status='open' LIMIT 10")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;
    let actions_today: i64 = db.with(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM actions_log WHERE created_at >= ?1 AND status='executed'",
            params![today_start],
            |r| r.get(0),
        )?)
    })?;
    Ok(json!({
        "greeting": greeting(db),
        "done_tasks": done,
        "pending_tasks": pending,
        "open_commitments": commitments,
        "actions_executed_today": actions_today,
        "generated_at": now(),
    }))
}

/// Produit réellement le débrief à l'heure choisie, une seule fois par jour.
pub fn maybe_generate_daily_wrap(db: &Db, bus: &Bus) -> Result<()> {
    let mut settings = crate::settings::load(db)?;
    if !settings.daily_wrap_enabled {
        return Ok(());
    }
    use chrono::Timelike;
    let now_local = chrono::Local::now();
    let today = now_local.format("%Y-%m-%d").to_string();
    if settings.last_wrap_date == today || now_local.hour() < settings.daily_wrap_hour as u32 {
        return Ok(());
    }
    let wrap = build_daily_wrap(db)?;
    let done = wrap["done_tasks"].as_array().map(|a| a.len()).unwrap_or(0);
    let pending = wrap["pending_tasks"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    let commitments = wrap["open_commitments"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    let body = format!("Débrief : {done} tâche(s) terminée(s), {pending} en cours, {commitments} engagement(s) ouvert(s).");
    let surfaced = super::arbitrate(
        db,
        bus,
        super::Candidate {
            trigger_id: None,
            kind: "daily_wrap".into(),
            reason: format!(
                "Heure du débrief quotidien ({} h)",
                settings.daily_wrap_hour
            ),
            body,
            priority: "info".into(),
        },
    )?;
    if surfaced {
        settings.last_wrap_date = today;
        crate::settings::save(db, &settings)?;
    }
    Ok(())
}
