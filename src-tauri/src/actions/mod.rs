//! Actions & autonomie (doc maître §11) + Sécurité §3.2 :
//! la porte d'action déterministe est LE contrôle réel — pas le modèle.
//! Plancher dur : irréversible / vers une personne réelle / financier-administratif
//! → TOUJOURS confirmé, quel que soit le niveau d'autonomie. Jamais désactivable.

use crate::db::{new_id, now, Db};
use crate::error::{AppError, Result};
use crate::settings::Autonomy;
use rusqlite::params;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskClass {
    Read,
    ReversibleLocal,
    ReversibleExternal,
    Floor,
}

impl RiskClass {
    pub fn as_str(&self) -> &'static str {
        match self {
            RiskClass::Read => "read",
            RiskClass::ReversibleLocal => "reversible_local",
            RiskClass::ReversibleExternal => "reversible_external",
            RiskClass::Floor => "floor",
        }
    }
}

/// Classement sur deux axes : réversibilité × rayon d'impact.
pub fn classify(tool: &str, args: &Value) -> RiskClass {
    match tool {
        // Lectures
        "memory.query" | "files.search" | "mail.search" | "calendar.list" | "tasks.list"
        | "commitments.list" | "people.context" | "photos.search" | "system.diagnose"
        | "files.reorganize" /* dry-run : produit un PLAN, ne déplace rien */ => RiskClass::Read,

        // Réversible local (preview + undo > demander à chaque fois)
        "mail.draft"
        | "tasks.create"
        | "tasks.complete"
        | "memory.remember"
        | "files.move"
        | "files.create_folder_and_move" => {
            RiskClass::ReversibleLocal
        }

        // Calendrier : privé = réversible quasi local ; avec invités = touche des
        // personnes externes → plancher (doc Connecteurs §2.2).
        "calendar.create" | "calendar.update" => {
            let has_attendees = args
                .get("attendees")
                .and_then(|a| a.as_array())
                .map(|a| !a.is_empty())
                .unwrap_or(false);
            if has_attendees {
                RiskClass::Floor
            } else {
                RiskClass::ReversibleLocal
            }
        }
        "calendar.delete" => RiskClass::ReversibleExternal,

        // Exécution d'un plan de rangement : réversible local (undo global),
        // mais soumis à la revue unique (B6) — voir needs_confirmation.
        "files.apply_reorganize_plan" => RiskClass::ReversibleLocal,

        // Plancher : envoi vers une personne réelle.
        "mail.send" => RiskClass::Floor,

        // Inconnu → prudence maximale.
        _ => RiskClass::Floor,
    }
}

/// Le point d'arrêt DANS la boucle (pas une couche UI par-dessus).
pub fn needs_confirmation(
    risk: RiskClass,
    autonomy: &Autonomy,
    derived_from_untrusted: bool,
    tool: &str,
) -> bool {
    // Suspicion renforcée : arguments dérivés de contenu non fiable (Sécurité §3.4).
    if derived_from_untrusted && risk != RiskClass::Read {
        return true;
    }
    // Le rangement suit le modèle « confiance → plan → revue unique » (Média §B6).
    if tool == "files.apply_reorganize_plan" {
        return true;
    }
    match risk {
        RiskClass::Floor => true, // plancher : jamais dissous, par personne
        RiskClass::Read => false,
        RiskClass::ReversibleExternal => !matches!(autonomy, Autonomy::Autonome),
        RiskClass::ReversibleLocal => matches!(autonomy, Autonomy::Prudent),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct PendingAction {
    pub id: String,
    pub tool: String,
    pub input: Value,
    pub risk_class: String,
    pub status: String,
    pub preview: String,
    pub result: Option<String>,
    pub created_at: i64,
    pub derived_from_untrusted: bool,
    pub session_id: Option<String>,
    pub undoable: bool,
}

pub struct ActionRecord {
    pub tool: String,
    pub input: Value,
    pub status: String,
    pub undo: Option<Value>,
    pub session_id: Option<String>,
}

/// Met une action en attente de confirmation (machine à états : proposer → classer
/// → confirmer → exécuter → journaliser + undo → réinjecter).
pub fn queue_pending(
    db: &Db,
    tool: &str,
    input: &Value,
    risk: RiskClass,
    preview: &str,
    untrusted: bool,
    session_id: Option<&str>,
) -> Result<String> {
    let id = new_id();
    db.with(|c| {
        c.execute(
            "INSERT INTO actions_log (id, tool, input, risk_class, status, preview, created_at, derived_from_untrusted, session_id)
             VALUES (?1,?2,?3,?4,'awaiting_confirmation',?5,?6,?7,?8)",
            params![id, tool, input.to_string(), risk.as_str(), preview, now(), untrusted as i64, session_id],
        )?;
        Ok(())
    })?;
    Ok(id)
}

#[allow(clippy::too_many_arguments)]
pub fn log_executed(
    db: &Db,
    tool: &str,
    input: &Value,
    risk: RiskClass,
    preview: &str,
    result: &str,
    undo_data: Option<&Value>,
    untrusted: bool,
) -> Result<String> {
    let id = new_id();
    db.with(|c| {
        c.execute(
            "INSERT INTO actions_log (id, tool, input, risk_class, status, preview, result, undo_data, created_at, derived_from_untrusted)
             VALUES (?1,?2,?3,?4,'executed',?5,?6,?7,?8,?9)",
            params![
                id,
                tool,
                input.to_string(),
                risk.as_str(),
                preview,
                result,
                undo_data.map(|u| u.to_string()),
                now(),
                untrusted as i64
            ],
        )?;
        Ok(())
    })?;
    Ok(id)
}

pub fn get_action(db: &Db, id: &str) -> Result<ActionRecord> {
    db.with(|c| {
        c.query_row(
            "SELECT tool, input, status, undo_data, session_id FROM actions_log WHERE id = ?1",
            params![id],
            |r| {
                Ok(ActionRecord {
                    tool: r.get::<_, String>(0)?,
                    input: serde_json::from_str(&r.get::<_, String>(1)?).unwrap_or(Value::Null),
                    status: r.get::<_, String>(2)?,
                    undo: r
                        .get::<_, Option<String>>(3)?
                        .and_then(|s| serde_json::from_str(&s).ok()),
                    session_id: r.get::<_, Option<String>>(4)?,
                })
            },
        )
        .map_err(|_| AppError::NotFound("action introuvable".into()))
    })
}

pub fn set_action_result(
    db: &Db,
    id: &str,
    status: &str,
    result: Option<&str>,
    undo: Option<&Value>,
) -> Result<()> {
    db.with(|c| {
        c.execute(
            "UPDATE actions_log SET status=?2, result=COALESCE(?3, result), undo_data=COALESCE(?4, undo_data) WHERE id=?1",
            params![id, status, result, undo.map(|u| u.to_string())],
        )?;
        Ok(())
    })
}

pub fn list_pending(db: &Db) -> Result<Vec<PendingAction>> {
    list_actions(db, Some("awaiting_confirmation"), 50)
}

pub fn list_actions(db: &Db, status: Option<&str>, limit: usize) -> Result<Vec<PendingAction>> {
    db.with(|c| {
        let sql = match status {
            Some(_) => {
                "SELECT id, tool, input, risk_class, status, preview, result, created_at, derived_from_untrusted, session_id,
                        undo_data IS NOT NULL
                 FROM actions_log WHERE status = ?1 ORDER BY created_at DESC LIMIT ?2"
            }
            None => {
                "SELECT id, tool, input, risk_class, status, preview, result, created_at, derived_from_untrusted, session_id,
                        undo_data IS NOT NULL
                 FROM actions_log WHERE ?1 IS NULL ORDER BY created_at DESC LIMIT ?2"
            }
        };
        let mut stmt = c.prepare(sql)?;
        let rows = stmt.query_map(params![status, limit as i64], |r| {
            Ok(PendingAction {
                id: r.get(0)?,
                tool: r.get(1)?,
                input: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or(Value::Null),
                risk_class: r.get(3)?,
                status: r.get(4)?,
                preview: r.get::<_, Option<String>>(5)?.unwrap_or_default(),
                result: r.get(6)?,
                created_at: r.get(7)?,
                derived_from_untrusted: r.get::<_, i64>(8)? != 0,
                session_id: r.get(9)?,
                undoable: r.get::<_, i64>(10)? != 0,
            })
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        drop(stmt);
        // Compatibilité avec les plans créés avant que leur aperçu complet ne
        // soit embarqué dans l'action : Archives et la carte de confirmation
        // peuvent ainsi afficher aussi les rangements déjà effectués.
        for action in &mut out {
            if action.tool != "files.apply_reorganize_plan" || !action.input["plan"].is_null() {
                continue;
            }
            let Some(plan_id) = action.input["plan_id"].as_str() else {
                continue;
            };
            let plan: Option<String> = c
                .query_row(
                    "SELECT plan FROM reorganize_plans WHERE id=?1",
                    params![plan_id],
                    |row| row.get(0),
                )
                .ok();
            if let Some(plan) = plan.and_then(|raw| serde_json::from_str(&raw).ok()) {
                action.input["plan"] = plan;
            }
        }
        Ok(out)
    })
}

/// Annulation générique à partir du journal d'undo.
pub fn apply_undo(db: &Db, undo: &Value) -> Result<String> {
    let kind = undo["kind"].as_str().unwrap_or("");
    match kind {
        "file_moves" => {
            // Rejouer les déplacements à l'envers.
            let mut restored = 0;
            if let Some(moves) = undo["moves"].as_array() {
                for m in moves.iter().rev() {
                    let (from, to) = (
                        m["from"].as_str().unwrap_or(""),
                        m["to"].as_str().unwrap_or(""),
                    );
                    if !to.is_empty() && std::path::Path::new(to).exists() {
                        if let Some(parent) = std::path::Path::new(from).parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        if std::fs::rename(to, from).is_ok() {
                            restored += 1;
                        }
                    }
                }
            }
            if let Some(dirs) = undo["created_dirs"].as_array() {
                for d in dirs.iter().rev() {
                    if let Some(p) = d.as_str() {
                        let _ = std::fs::remove_dir(p); // seulement si vide
                    }
                }
            }
            Ok(format!(
                "{restored} fichier(s) restauré(s) à leur emplacement d'origine."
            ))
        }
        "delete_task" => {
            db.with(|c| {
                c.execute(
                    "DELETE FROM tasks WHERE id = ?1",
                    params![undo["id"].as_str()],
                )?;
                Ok(())
            })?;
            Ok("Tâche supprimée.".into())
        }
        "reopen_task" => {
            db.with(|c| {
                c.execute(
                    "UPDATE tasks SET status='open' WHERE id = ?1",
                    params![undo["id"].as_str()],
                )?;
                Ok(())
            })?;
            Ok("Tâche rouverte.".into())
        }
        "delete_event" => {
            if cfg!(target_os = "macos") {
                crate::connectors::native::calendar_delete(undo["id"].as_str().unwrap_or(""))?;
                return Ok("Événement supprimé d’Apple Calendar.".into());
            }
            db.with(|c| {
                c.execute(
                    "DELETE FROM events WHERE id = ?1",
                    params![undo["id"].as_str()],
                )?;
                Ok(())
            })?;
            Ok("Événement supprimé.".into())
        }
        "delete_item" => {
            db.with(|c| {
                c.execute(
                    "DELETE FROM items WHERE id = ?1",
                    params![undo["id"].as_str()],
                )?;
                c.execute(
                    "DELETE FROM embeddings WHERE item_id = ?1",
                    params![undo["id"].as_str()],
                )?;
                Ok(())
            })?;
            Ok("Élément supprimé.".into())
        }
        _ => Err(AppError::Invalid(
            "cette action ne peut pas être annulée".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn le_plancher_tient_quel_que_soit_le_niveau() {
        // Invariant 6 : aucune action grave silencieuse.
        for autonomy in [Autonomy::Prudent, Autonomy::Assiste, Autonomy::Autonome] {
            assert!(needs_confirmation(
                RiskClass::Floor,
                &autonomy,
                false,
                "mail.send"
            ));
        }
    }

    #[test]
    fn evenement_avec_invites_touche_le_plancher() {
        let with = json!({"title": "Réunion", "attendees": ["a@b.fr"]});
        let without = json!({"title": "Sport", "attendees": []});
        assert_eq!(classify("calendar.create", &with), RiskClass::Floor);
        assert_eq!(
            classify("calendar.create", &without),
            RiskClass::ReversibleLocal
        );
    }

    #[test]
    fn derive_untrusted_force_la_confirmation() {
        // Sécurité §3.4 : même une action anodine dérivée d'untrusted est confirmée.
        assert!(needs_confirmation(
            RiskClass::ReversibleLocal,
            &Autonomy::Autonome,
            true,
            "mail.draft"
        ));
    }

    #[test]
    fn outil_inconnu_est_plancher() {
        assert_eq!(classify("outil.mystere", &json!({})), RiskClass::Floor);
    }

    #[test]
    fn lecture_jamais_confirmee() {
        assert!(!needs_confirmation(
            RiskClass::Read,
            &Autonomy::Prudent,
            false,
            "files.search"
        ));
    }
}
