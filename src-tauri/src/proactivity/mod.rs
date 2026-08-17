//! Proactivité (deep-dive Proactivité) : déclencheurs → file de candidats →
//! ARBITRE (budget de rareté + priorité) → surfaçage EXPLICABLE.
//! Un surfaçage sans raison affichée est un bug.

pub mod briefs;

use crate::bus::{Bus, BusEvent};
use crate::connectors::system as system_conn;
use crate::db::{new_id, now, Db};
use crate::error::Result;
use rusqlite::params;
use serde_json::Value;

pub struct Candidate {
    pub trigger_id: Option<String>,
    pub kind: String,
    pub reason: String, // « Syn a vu X et Y, donc Z »
    pub body: String,
    pub priority: String, // urgent | important | info
}

fn priority_level(priority: &str) -> u8 {
    match priority {
        "urgent" => 3,
        "important" => 2,
        _ => 1,
    }
}

fn kind_enabled(settings: &crate::settings::Settings, kind: &str) -> bool {
    match kind {
        "brief" | "daily_wrap" => settings.notify_briefs,
        "event" => settings.notify_events,
        "commitment" => settings.notify_commitments,
        "system" => settings.notify_system,
        "rule" => settings.notify_rules,
        _ => true,
    }
}

fn allowed_in_work_mode(settings: &crate::settings::Settings, candidate: &Candidate) -> bool {
    if !settings.work_mode || candidate.priority == "urgent" {
        return true;
    }
    settings.work_notification_policy == "relevant"
        && matches!(candidate.kind.as_str(), "event" | "commitment" | "rule")
}

fn notification_allowed(settings: &crate::settings::Settings, candidate: &Candidate) -> bool {
    settings.notifications_enabled
        && !settings.notifications_muted
        && kind_enabled(settings, &candidate.kind)
        && priority_level(&candidate.priority)
            >= priority_level(&settings.notification_min_priority)
        && allowed_in_work_mode(settings, candidate)
}

/// L'arbitre : point unique de décision (budget + priorité + fenêtres calmes + anti-répétition).
pub fn arbitrate(db: &Db, bus: &Bus, candidate: Candidate) -> Result<bool> {
    let settings = crate::settings::load(db)?;

    if !notification_allowed(&settings, &candidate) {
        return Ok(false);
    }
    // Heures raisonnables.
    let hour = chrono::Local::now().hour();
    if candidate.priority != "urgent" && !(8..22).contains(&hour) {
        return Ok(false);
    }
    // Anti-répétition : même raison déjà surfacée aujourd'hui.
    let today_start = today_start_ts();
    let repeated: bool = db.read(|c| {
        Ok(c.query_row(
            "SELECT 1 FROM proactive_log WHERE reason = ?1 AND surfaced_at >= ?2",
            params![candidate.reason, today_start],
            |_| Ok(true),
        )
        .unwrap_or(false))
    })?;
    if repeated {
        return Ok(false);
    }
    // Budget de rareté : plafond/jour ; `urgent` passe toujours ; `info` premier sacrifié.
    let surfaced_today: i64 = db.read(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM proactive_log WHERE surfaced_at >= ?1 AND kind != 'brief'",
            params![today_start],
            |r| r.get(0),
        )?)
    })?;
    if candidate.priority != "urgent" && surfaced_today >= settings.rarity_budget as i64 {
        return Ok(false); // au-delà : silence ; aucun stockage implicite trompeur
    }

    let id = new_id();
    db.with(|c| {
        c.execute(
            "INSERT INTO proactive_log (id, trigger_id, kind, reason, body, priority, surfaced_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![
                id,
                candidate.trigger_id,
                candidate.kind,
                candidate.reason,
                candidate.body,
                candidate.priority,
                now()
            ],
        )?;
        Ok(())
    })?;
    bus.emit(BusEvent::ProactiveAlert {
        id,
        kind: candidate.kind,
        reason: candidate.reason,
        body: candidate.body,
        priority: candidate.priority,
    });
    Ok(true)
}

use chrono::Timelike;

pub fn today_start_ts() -> i64 {
    let now = chrono::Local::now();
    now.date_naive()
        .and_hms_opt(0, 0, 0)
        .unwrap()
        .and_local_timezone(now.timezone())
        .unwrap()
        .timestamp()
}

/// Une passe d'évaluation (appelée par la boucle cadencée, ~60 s).
pub async fn evaluate_tick(db: &Db, bus: &Bus) -> Result<()> {
    let settings = crate::settings::load(db)?;

    // — Gardien système (déclencheurs threshold) —
    let snapshot = system_conn::snapshot();
    for d in &snapshot.disks {
        let (total, free) = (
            d["total_gb"].as_f64().unwrap_or(1.0),
            d["free_gb"].as_f64().unwrap_or(1.0),
        );
        if total > 0.0 && (free / total * 100.0) < settings.guardian_disk_pct as f64 {
            let _ = arbitrate(
                db,
                bus,
                Candidate {
                    trigger_id: None,
                    kind: "system".into(),
                    reason: "Espace de stockage faible".into(),
                    body: format!(
                        "Il reste {:.0} Go libres sur {:.0} Go pour le disque {}.",
                        free,
                        total,
                        d["mount"].as_str().unwrap_or("?")
                    ),
                    priority: "urgent".into(),
                },
            );
        }
    }
    for t in &snapshot.temps {
        if t["celsius"].as_f64().unwrap_or(0.0) > settings.guardian_temp_c as f64 {
            let culprit = snapshot
                .top_processes
                .first()
                .map(|p| {
                    format!(
                        " Processus le plus gourmand : {} ({} % CPU).",
                        p["name"].as_str().unwrap_or("?"),
                        p["cpu_pct"]
                    )
                })
                .unwrap_or_default();
            let _ = arbitrate(
                db,
                bus,
                Candidate {
                    trigger_id: None,
                    kind: "system".into(),
                    reason: "Température élevée".into(),
                    body: format!(
                        "{} relève {} °C.{}",
                        t["label"].as_str().unwrap_or("?"),
                        t["celsius"],
                        culprit
                    ),
                    priority: "important".into(),
                },
            );
        }
    }
    // Batterie faible → suggérer le mode économie (consultatif V1).
    if let Some(b) = &snapshot.battery {
        if b["pct"].as_u64().unwrap_or(100) < 15
            && !b["charging"].as_bool().unwrap_or(false)
            && !settings.eco_mode
        {
            let _ = arbitrate(
                db,
                bus,
                Candidate {
                    trigger_id: None,
                    kind: "system".into(),
                    reason: "Batterie faible".into(),
                    body: format!(
                        "Il reste {} %. Active le mode économie pour réduire l'activité de Syn.",
                        b["pct"]
                    ),
                    priority: "important".into(),
                },
            );
        }
    }

    // — Engagements arrivant à échéance (<24 h) —
    let soon = now() + 86_400;
    let commitments: Vec<(String, String)> = db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT id, text FROM commitments WHERE status='open' AND due IS NOT NULL AND due <= ?1 AND due >= ?2",
        )?;
        let rows = stmt.query_map(params![soon, now() - 86_400], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;
    for (_id, text) in commitments {
        let _ = arbitrate(
            db,
            bus,
            Candidate {
                trigger_id: None,
                kind: "commitment".into(),
                reason: "Échéance à venir".into(),
                body: text,
                priority: "important".into(),
            },
        );
    }

    // — Événements imminents (<30 min) —
    let events: Vec<Value> = db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT title, \"start\" FROM events WHERE \"start\" > ?1 AND \"start\" <= ?2",
        )?;
        let rows = stmt.query_map(params![now(), now() + 1800], |r| {
            Ok(serde_json::json!({"title": r.get::<_, String>(0)?, "start": r.get::<_, i64>(1)?}))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;
    for ev in events {
        let start = ev["start"].as_i64().unwrap_or(0);
        let mins = ((start - now()) / 60).max(0);
        let _ = arbitrate(
            db,
            bus,
            Candidate {
                trigger_id: None,
                kind: "event".into(),
                reason: "Événement à venir".into(),
                body: format!(
                    "« {} » commence dans {} min.",
                    ev["title"].as_str().unwrap_or("?"),
                    mins
                ),
                priority: "important".into(),
            },
        );
    }

    // — Déclencheurs issus des Règles (source=rule, threshold reconnus) —
    let triggers: Vec<(String, String, String, String)> = db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT id, condition, priority, reason_template FROM triggers WHERE enabled=1 AND type='threshold'",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })?;
    for (tid, condition, priority, reason_template) in triggers {
        let fired = match condition.as_str() {
            "cpu.pct>85" => snapshot.cpu_pct > 85.0,
            "disk.free_pct<10" => snapshot.disks.iter().any(|d| {
                let (t, f) = (
                    d["total_gb"].as_f64().unwrap_or(1.0),
                    d["free_gb"].as_f64().unwrap_or(1.0),
                );
                t > 0.0 && f / t * 100.0 < 10.0
            }),
            "battery.pct<20" => snapshot
                .battery
                .as_ref()
                .map(|b| b["pct"].as_u64().unwrap_or(100) < 20)
                .unwrap_or(false),
            _ => false,
        };
        if fired {
            let body = match condition.as_str() {
                "cpu.pct>85" => format!(
                    "CPU à {:.0} %. {}",
                    snapshot.cpu_pct,
                    system_conn::diagnose(&snapshot)
                ),
                _ => system_conn::diagnose(&snapshot),
            };
            let _ = arbitrate(
                db,
                bus,
                Candidate {
                    trigger_id: Some(tid.clone()),
                    kind: "rule".into(),
                    reason: reason_template.clone(),
                    body,
                    priority: priority.clone(),
                },
            );
            let _ = db.with(|c| {
                c.execute(
                    "UPDATE triggers SET last_fired=?2 WHERE id=?1",
                    params![tid, now()],
                )?;
                Ok(())
            });
        }
    }

    // — Brief de démarrage (gate jour + activité + heure-plancher) —
    briefs::maybe_generate_startup_brief(db, bus).await?;
    briefs::maybe_generate_daily_wrap(db, bus)?;

    Ok(())
}

pub fn list_surfacings(db: &Db, limit: usize) -> Result<Vec<Value>> {
    db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT id, kind, reason, body, priority, surfaced_at, dismissed FROM proactive_log
             ORDER BY surfaced_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit as i64], |r| {
            Ok(serde_json::json!({
                "id": r.get::<_, String>(0)?,
                "kind": r.get::<_, String>(1)?,
                "reason": r.get::<_, String>(2)?,
                "body": r.get::<_, Option<String>>(3)?,
                "priority": r.get::<_, String>(4)?,
                "surfaced_at": r.get::<_, i64>(5)?,
                "dismissed": r.get::<_, i64>(6)? != 0,
            }))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

#[cfg(test)]
mod notification_tests {
    use super::*;

    fn candidate(kind: &str, priority: &str) -> Candidate {
        Candidate {
            trigger_id: None,
            kind: kind.into(),
            reason: "Test".into(),
            body: "Test".into(),
            priority: priority.into(),
        }
    }

    #[test]
    fn la_sourdine_bloque_toutes_les_notifications() {
        let settings = crate::settings::Settings {
            notifications_muted: true,
            ..Default::default()
        };
        assert!(!notification_allowed(
            &settings,
            &candidate("system", "urgent")
        ));
    }

    #[test]
    fn le_filtre_de_priorite_est_applique() {
        let settings = crate::settings::Settings {
            notification_min_priority: "important".into(),
            ..Default::default()
        };
        assert!(!notification_allowed(
            &settings,
            &candidate("brief", "info")
        ));
        assert!(notification_allowed(
            &settings,
            &candidate("event", "important")
        ));
    }

    #[test]
    fn le_mode_travail_garde_lurgent_par_defaut() {
        let settings = crate::settings::Settings {
            work_mode: true,
            ..Default::default()
        };
        assert!(!notification_allowed(
            &settings,
            &candidate("event", "important")
        ));
        assert!(notification_allowed(
            &settings,
            &candidate("system", "urgent")
        ));
    }

    #[test]
    fn le_mode_travail_peut_garder_agenda_et_echeances() {
        let settings = crate::settings::Settings {
            work_mode: true,
            work_notification_policy: "relevant".into(),
            ..Default::default()
        };
        assert!(notification_allowed(
            &settings,
            &candidate("event", "important")
        ));
        assert!(notification_allowed(
            &settings,
            &candidate("commitment", "important")
        ));
        assert!(!notification_allowed(
            &settings,
            &candidate("brief", "info")
        ));
    }
}
