//! Contrat commun des connecteurs (doc maître §9, aligné MCP) :
//! permission explicite, révocable, tracée dans `access_log`.

pub mod calendar;
pub mod external;
pub mod files;
pub mod mail;
pub mod messages;
pub mod native;
pub mod oauth;
pub mod people;
pub mod reminders;
pub mod screen;
pub mod system;

use crate::db::{now, Db};
use crate::error::Result;
use rusqlite::params;
use serde::Serialize;
use serde_json::Value;

#[derive(Debug, Clone, Serialize)]
pub struct ConnectorInfo {
    pub id: String,
    pub r#type: String,
    pub status: String,
    pub scopes: Option<String>,
    pub last_sync: Option<i64>,
    pub detail: Option<String>,
    pub last_error: Option<String>,
    pub sync_summary: Option<String>,
}

/// Catalogue des connecteurs connus (V1) + statut persisté.
pub fn list(db: &Db) -> Result<Vec<ConnectorInfo>> {
    let mut known: Vec<(&str, &str, &str)> = vec![
        // (id, type, statut par défaut)
        ("files", "files", "connected"), // accès effectif contrôlé par macOS
        ("google", "google", "needs_configuration"),
        ("microsoft", "microsoft", "needs_configuration"),
        ("slack", "slack", "needs_configuration"),
        ("github", "github", "needs_configuration"),
        ("system", "system", "connected"),
        ("screen", "screen", "disconnected"),
    ];
    if cfg!(target_os = "macos") {
        known.insert(1, ("apple", "apple", "connected"));
        known.insert(2, ("messages", "messages", "disconnected"));
    }
    db.read(|c| {
        let mut out = vec![];
        for (id, ty, default_status) in known {
            let row: Option<(String, Option<String>, Option<i64>, Option<String>)> = c
                .query_row(
                    "SELECT status, scopes, last_sync, config FROM connectors WHERE id = ?1",
                    params![id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
                )
                .ok();
            let (mut status, scopes, last_sync, config) =
                row.unwrap_or((default_status.to_string(), None, None, None));
            let config: Value = config
                .as_deref()
                .and_then(|raw| serde_json::from_str(raw).ok())
                .unwrap_or(Value::Null);
            // Apple est une capacité native du Mac, pas un compte à connecter.
            if id == "apple" && cfg!(target_os = "macos") {
                status = "connected".into();
            }
            if matches!(id, "google" | "microsoft" | "slack" | "github") {
                status = if oauth::has_token(id) {
                    if matches!(status.as_str(), "connected" | "syncing" | "needs_reauth") {
                        status
                    } else {
                        "authorized_only".into()
                    }
                } else if status == "needs_reauth" {
                    status
                } else if oauth::is_configured(id) {
                    "disconnected".into()
                } else {
                    "needs_configuration".into()
                };
            }
            let detail = match ty {
                "google" if status == "connected" => Some("Gmail, Google Agenda et Google Drive synchronisés localement.".into()),
                "microsoft" if status == "connected" => Some("Outlook, Calendrier Microsoft et OneDrive synchronisés localement.".into()),
                "google" => Some(oauth::configuration_detail("google")),
                "microsoft" => Some(oauth::configuration_detail("microsoft")),
                "slack" => Some(oauth::configuration_detail("slack")),
                "github" => Some(oauth::configuration_detail("github")),
                "apple" => Some(
                    "Intégré à ce Mac. Chaque service reste soumis à son autorisation macOS propre.".to_string(),
                ),
                "messages" => Some(
                    "Lecture locale de l'historique Messages (iMessage/SMS). Nécessite l'Accès complet au disque.".to_string(),
                ),
                _ => None,
            };
            out.push(ConnectorInfo {
                id: id.to_string(),
                r#type: ty.to_string(),
                status,
                scopes,
                last_sync,
                detail,
                last_error: config["last_error"].as_str().map(str::to_string),
                sync_summary: config["sync_summary"].as_str().map(str::to_string),
            });
        }
        Ok(out)
    })
}

pub fn set_diagnostic(db: &Db, id: &str, error: Option<&str>, summary: Option<&str>) -> Result<()> {
    db.with(|c| {
        let current: Option<String> = c
            .query_row("SELECT config FROM connectors WHERE id=?1", [id], |row| {
                row.get(0)
            })
            .unwrap_or(None);
        let mut value: Value = current
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        if let Some(object) = value.as_object_mut() {
            match error {
                Some(message) => {
                    object.insert("last_error".into(), serde_json::json!(message));
                }
                None => {
                    object.remove("last_error");
                }
            }
            if let Some(message) = summary {
                object.insert("sync_summary".into(), serde_json::json!(message));
            }
        }
        c.execute(
            "UPDATE connectors SET config=?2 WHERE id=?1",
            params![id, value.to_string()],
        )?;
        Ok(())
    })
}

pub fn set_status(db: &Db, id: &str, ty: &str, status: &str) -> Result<()> {
    db.with(|c| {
        c.execute(
            "INSERT INTO connectors (id, type, status, last_sync) VALUES (?1,?2,?3,?4)
             ON CONFLICT(id) DO UPDATE SET status = excluded.status, last_sync = excluded.last_sync",
            params![id, ty, status, now()],
        )?;
        Ok(())
    })
}

pub fn is_connected(db: &Db, id: &str) -> bool {
    if id == "apple" && cfg!(target_os = "macos") {
        return true;
    }
    db.read(|c| {
        Ok(c.query_row(
            "SELECT status FROM connectors WHERE id=?1",
            params![id],
            |r| r.get::<_, String>(0),
        )
        .map(|s| s == "connected" || s == "syncing")
        .unwrap_or(matches!(id, "files" | "system")))
    })
    .unwrap_or(false)
}
