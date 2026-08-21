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
    let speak = crate::i18n::ambient_speak(db, &settings);
    let bonjour = speak.either("Bonjour", "Hello");
    match &settings.voice.address_form {
        Some(addr) => format!("{bonjour} {addr},"),
        None => format!("{bonjour},"),
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
    let speak = crate::i18n::ambient_speak(db, &settings);
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
                text: if speak.is_en() {
                    format!("Today you have {} at {}", ev["title"].as_str().unwrap_or("?"), time)
                } else {
                    format!(
                        "Aujourd'hui {} {} à {}",
                        speak.pick("tu as", "vous avez", "you have"),
                        ev["title"].as_str().unwrap_or("?"),
                        time
                    )
                },
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
                text: if speak.is_en() {
                    format!("Email about « {title} »")
                } else {
                    format!("Mail concernant « {title} »")
                },
                sub: from,
                source_ref: Some(source_ref),
                kind: "mail".into(),
            });
        }
    }

    // Messages qui attendent une réponse. C'est la ligne la plus utile d'un
    // brief du matin, et elle manquait : Syn listait les mails récents sans
    // jamais dire lesquels attendaient quelque chose de l'utilisateur.
    if super::reflexes::est_actif(db, "sys.mail_sans_reponse") {
        for attente in super::reflexes::en_attente_de_reponse(db, 3)? {
            items.push(BriefItem {
                icon: "mail-open".into(),
                text: if speak.is_en() {
                    format!(
                        "{} is waiting for your reply about « {} »",
                        attente.qui, attente.objet
                    )
                } else {
                    format!(
                        "{} attend {} réponse au sujet de « {} »",
                        attente.qui,
                        speak.pick("ta", "votre", "your"),
                        attente.objet
                    )
                },
                sub: Some(if speak.is_en() {
                    format!("received {} days ago", attente.jours)
                } else {
                    format!("reçu il y a {} jours", attente.jours)
                }),
                source_ref: Some(attente.source_ref),
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
                text: format!("{} {title}", speak.either("Tâche à faire :", "To do:")),
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
                text: format!("{} {text}", speak.either("Engagement en cours :", "Open commitment:")),
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
                text: if speak.is_en() {
                    format!("It's {name}'s birthday today")
                } else {
                    format!("C'est l'anniversaire de {name} aujourd'hui")
                },
                source_ref: None,
            });
        }
    }

    // Reprendre le travail : document le plus récemment modifié.
    if sections.iter().any(|s| s == "continue") {
        let recent: Option<(String, String)> = db.with(|c| {
            Ok(c.query_row(
                "SELECT title, source_ref FROM items
                 WHERE source='files' AND type IN ('document','code_project','code') AND status='active' AND mtime >= ?1
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
                    text: if speak.is_en() {
                        format!("Keep working on “{title}”")
                    } else {
                        format!("Continuer de travailler sur “{title}”")
                    },
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
    bus.emit(BusEvent::BriefReady);
    let _ = super::arbitrate(
        db,
        bus,
        super::Candidate {
            trigger_id: None,
            kind: "brief".into(),
            reason: "Résumé du jour disponible".into(),
            body: "Consulte ton agenda, tes tâches et tes rappels sur l'accueil.".into(),
            priority: "info".into(),
        },
    )?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::new_id;

    /// La ligne la plus utile du matin : qui attend une réponse. Et l'interrupteur
    /// du réflexe la commande — couper l'un coupe l'autre.
    #[test]
    fn le_brief_dit_qui_attend_une_reponse() {
        let dir = std::env::temp_dir().join(format!("syn-brief-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db"), &"e".repeat(64)).unwrap();

        // Sans section « events » : l'agenda natif demanderait une permission
        // macOS que le test n'a pas — et ce n'est pas ce qu'on mesure ici.
        let settings = crate::settings::Settings {
            brief_sections: vec!["mails".into()],
            ..Default::default()
        };
        crate::settings::save(&db, &settings).unwrap();

        crate::memory::graph::set_self_address(&db, "paul@moi.fr", true).unwrap();
        for _ in 0..3 {
            crate::memory::graph::observe(
                &db,
                &crate::memory::graph::Node::new("contact", "julie@exemple.fr"),
                "ecrit_a",
                &crate::memory::graph::Node::moi(),
                now(),
                "mail",
            )
            .unwrap();
        }
        db.with(|c| {
            c.execute(
                "INSERT INTO items (id,source,source_ref,type,title,body,created_at,ingested_at,status)
                 VALUES (?1,'mail','ref','email','Devis toiture',
                         'De : Julie <julie@exemple.fr>' || char(10) || 'À : paul@moi.fr' || char(10) || 'Objet : Devis',
                         ?2,?2,'active')",
                params![new_id(), now() - 5 * 86_400],
            )?;
            Ok(())
        })
        .unwrap();

        let brief = build_brief(&db).unwrap();
        assert!(
            brief.items.iter().any(|item| item.text.contains("Julie")
                && item.text.contains("attend")
                && item.text.contains("réponse")),
            "le brief doit signaler le message en attente : {:?}",
            brief.items.iter().map(|i| &i.text).collect::<Vec<_>>()
        );

        // Le même brief, pour un utilisateur anglophone.
        let anglais = crate::settings::Settings {
            brief_sections: vec!["mails".into()],
            answer_language: "en".into(),
            ..Default::default()
        };
        crate::settings::save(&db, &anglais).unwrap();
        let brief = build_brief(&db).unwrap();
        assert!(
            brief.greeting.starts_with("Hello"),
            "{}",
            brief.greeting
        );
        assert!(
            brief.items.iter().any(|item| item
                .text
                .contains("is waiting for your reply")),
            "{:?}",
            brief.items.iter().map(|i| &i.text).collect::<Vec<_>>()
        );
        crate::settings::save(&db, &settings).unwrap();

        super::super::reflexes::ensure_registered(&db).unwrap();
        db.with(|c| {
            c.execute(
                "UPDATE triggers SET enabled=0 WHERE id='sys.mail_sans_reponse'",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        let brief = build_brief(&db).unwrap();
        assert!(
            !brief.items.iter().any(|item| item.text.contains("attend ta réponse")),
            "réflexe coupé : la ligne disparaît aussi du brief"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
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
    let body = format!(
        "{done} tâche(s) terminée(s), {pending} en cours et {commitments} échéance(s) ouverte(s)."
    );
    let surfaced = super::arbitrate(
        db,
        bus,
        super::Candidate {
            trigger_id: None,
            kind: "daily_wrap".into(),
            reason: "Bilan du jour disponible".into(),
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
