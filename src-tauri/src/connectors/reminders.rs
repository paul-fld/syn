//! Connecteur Rappels (EventKit Reminders) : miroir bidirectionnel léger.
//! Lecture : les rappels ouverts alimentent la table `tasks` (source='reminders').
//! Écriture : une tâche créée dans Syn devient un vrai rappel macOS quand
//! l'autorisation est accordée — le rappel natif est la référence (`external_ref`).

use crate::db::{new_id, Db};
use crate::error::Result;
use rusqlite::params;

pub fn available() -> bool {
    cfg!(target_os = "macos")
        && matches!(
            crate::connectors::native::permission_status("reminders"),
            "granted" | "authorized"
        )
}

/// Miroir Rappels → `tasks`. Idempotent (upsert par `external_ref`).
pub fn sync_native_to_db(db: &Db) -> Result<usize> {
    if !available() {
        return Ok(0);
    }
    let reminders = crate::connectors::native::reminders_list()?;
    let mut count = 0usize;
    let ids: Vec<String> = reminders
        .iter()
        .filter_map(|r| r["id"].as_str().map(str::to_string))
        .collect();
    db.with(|c| {
        for r in &reminders {
            let native_id = r["id"].as_str().unwrap_or("");
            let title = r["title"].as_str().unwrap_or("");
            if native_id.is_empty() || title.is_empty() {
                continue;
            }
            let due = r["due"].as_f64().map(|f| f as i64);
            let updated = c.execute(
                "UPDATE tasks SET title=?2, due=?3, status='open' WHERE external_ref=?1",
                params![native_id, title, due],
            )?;
            if updated == 0 {
                c.execute(
                    "INSERT INTO tasks (id, source, title, due, status, external_ref)
                     VALUES (?1, 'reminders', ?2, ?3, 'open', ?4)",
                    params![new_id(), title, due, native_id],
                )?;
            }
            count += 1;
        }
        // Un rappel complété/supprimé côté macOS ferme la tâche miroir.
        if !ids.is_empty() {
            let placeholders = (0..ids.len())
                .map(|i| format!("?{}", i + 1))
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                "UPDATE tasks SET status='done' WHERE source='reminders' AND status='open'
                 AND external_ref IS NOT NULL AND external_ref NOT IN ({placeholders})"
            );
            let params_vec: Vec<&dyn rusqlite::ToSql> =
                ids.iter().map(|k| k as &dyn rusqlite::ToSql).collect();
            c.execute(&sql, params_vec.as_slice())?;
        }
        Ok(())
    })?;
    crate::security::log_access(db, "reminders", "sync", None);
    Ok(count)
}

/// Crée le rappel natif correspondant à une tâche Syn (best-effort) et
/// renvoie l'identifiant natif si la création a réussi.
pub fn create_native(title: &str, due: Option<i64>) -> Option<String> {
    if !available() {
        return None;
    }
    crate::connectors::native::reminder_create(title, due)
        .ok()
        .and_then(|v| v["id"].as_str().map(str::to_string))
}

/// Complète le rappel natif lié (best-effort).
pub fn complete_native(external_ref: &str) {
    if available() {
        let _ = crate::connectors::native::reminder_complete(external_ref);
    }
}
