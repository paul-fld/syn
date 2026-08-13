//! Contrat commun des connecteurs (doc maître §9, aligné MCP) :
//! permission explicite, révocable, tracée dans `access_log`.

pub mod calendar;
pub mod files;
pub mod mail;
pub mod native;
pub mod people;
pub mod screen;
pub mod system;

use crate::db::{now, Db};
use crate::error::Result;
use rusqlite::params;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ConnectorInfo {
    pub id: String,
    pub r#type: String,
    pub status: String,
    pub scopes: Option<String>,
    pub last_sync: Option<i64>,
    pub detail: Option<String>,
}

/// Catalogue des connecteurs connus (V1) + statut persisté.
pub fn list(db: &Db) -> Result<Vec<ConnectorInfo>> {
    let mut known: Vec<(&str, &str, &str)> = vec![
        // (id, type, statut par défaut)
        ("files", "files", "connected"), // périmètre = dossiers choisis
        ("google", "google", "needs_configuration"),
        ("microsoft", "microsoft", "needs_configuration"),
        ("slack", "slack", "needs_configuration"),
        ("github", "github", "needs_configuration"),
        ("system", "system", "connected"),
        ("screen", "screen", "disconnected"),
    ];
    if cfg!(target_os = "macos") {
        known.insert(1, ("apple", "apple", "connected"));
    }
    db.with(|c| {
        let mut out = vec![];
        for (id, ty, default_status) in known {
            let row: Option<(String, Option<String>, Option<i64>)> = c
                .query_row(
                    "SELECT status, scopes, last_sync FROM connectors WHERE id = ?1",
                    params![id],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .ok();
            let (mut status, scopes, last_sync) =
                row.unwrap_or((default_status.to_string(), None, None));
            // Apple est une capacité native du Mac, pas un compte à connecter.
            if id == "apple" && cfg!(target_os = "macos") {
                status = "connected".into();
            }
            let detail = match ty {
                "google" => Some("Nécessite un client OAuth Google (PKCE), les API Gmail/Calendar/Drive et la validation des portées sensibles.".to_string()),
                "microsoft" => Some("Nécessite une application Microsoft Entra, OAuth PKCE et les permissions Graph déléguées.".to_string()),
                "slack" => Some("Nécessite une application Slack, OAuth v2 et des portées minimales par espace de travail.".to_string()),
                "github" => Some("Nécessite une GitHub App ou le Device Flow, avec des permissions dépôt minimales.".to_string()),
                "apple" => Some(
                    "Intégré à ce Mac. Chaque service reste soumis à son autorisation macOS propre.".to_string(),
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
            });
        }
        Ok(out)
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
    db.with(|c| {
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
