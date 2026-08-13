//! Catalogue d'outils (doc maître §10). Le routeur reçoit ce catalogue ;
//! ajouter une capacité = ajouter un outil. Chaque outil déclare son side_effect ;
//! la classe de risque est calculée par la porte d'action (actions::classify).

pub mod reorganize;

use crate::bus::Bus;
use crate::connectors::{calendar, people as people_conn, system as system_conn};
use crate::db::{new_id, now, Db};
use crate::error::{AppError, Result};
use crate::llm::{LlmClient, SideEffect, ToolSpec};
use crate::memory;
use crate::retrieval;
use crate::settings::Settings;
use serde_json::{json, Value};
use std::sync::Arc;

pub struct ToolCtx {
    pub db: Db,
    pub llm: Arc<dyn LlmClient>,
    pub bus: Bus,
    pub settings: Settings,
}

pub struct ToolResult {
    pub result: Value,
    pub undo: Option<Value>,
}

fn spec(
    name: &str,
    description: &str,
    props: Value,
    required: &[&str],
    side_effect: SideEffect,
) -> ToolSpec {
    ToolSpec {
        name: name.into(),
        description: description.into(),
        input_schema: json!({
            "type": "object",
            "properties": props,
            "required": required,
        }),
        side_effect,
    }
}

pub fn catalog() -> Vec<ToolSpec> {
    vec![
        spec(
            "memory.query",
            "Recherche dans la mémoire de Syn (documents, mails, notes, faits appris). À utiliser pour toute question sur la vie numérique de l'utilisateur.",
            json!({"query": {"type": "string", "description": "requête en langage naturel"}}),
            &["query"],
            SideEffect::Read,
        ),
        spec(
            "files.search",
            "Recherche parmi les fichiers indexés (contenu et métadonnées). Renvoie des chemins ouvrables.",
            json!({"query": {"type": "string"}}),
            &["query"],
            SideEffect::Read,
        ),
        spec(
            "files.reorganize",
            "Prépare un PLAN de rangement intelligent d'un fichier, dossier ou emplacement autorisé (simulation, rien n'est déplacé). Accepte un chemin exact ou un nom non ambigu. L'utilisateur revoit le plan une seule fois avant exécution.",
            json!({"target_dir": {"type": "string", "description": "fichier ou dossier cible, par nom ou chemin, dans le périmètre autorisé"}}),
            &["target_dir"],
            SideEffect::Read,
        ),
        spec(
            "files.move",
            "Déplace précisément un fichier ou dossier existant dans un dossier de destination existant. Utilise cet outil quand l'utilisateur dit naturellement « mets/déplace/range X dans Y ». Ne l'utilise PAS pour classer le contenu de X : dans ce cas utilise files.reorganize.",
            json!({
                "source": {"type": "string", "description": "nom ou chemin du fichier/dossier à déplacer"},
                "destination": {"type": "string", "description": "nom ou chemin du dossier dans lequel placer la source"}
            }),
            &["source", "destination"],
            SideEffect::WriteLocal,
        ),
        spec(
            "mail.search",
            "Recherche dans les mails ingérés.",
            json!({"query": {"type": "string"}}),
            &["query"],
            SideEffect::Read,
        ),
        spec(
            "mail.draft",
            "Prépare un BROUILLON de mail (local, rien n'est envoyé).",
            json!({
                "to": {"type": "string"},
                "subject": {"type": "string"},
                "body": {"type": "string"}
            }),
            &["to", "subject", "body"],
            SideEffect::WriteLocal,
        ),
        spec(
            "mail.send",
            "Envoie un mail. Action vers une personne réelle : TOUJOURS confirmée par l'utilisateur (plancher).",
            json!({
                "to": {"type": "string"},
                "subject": {"type": "string"},
                "body": {"type": "string"}
            }),
            &["to", "subject", "body"],
            SideEffect::WriteExternal,
        ),
        spec(
            "calendar.list",
            "Liste les événements du calendrier entre deux dates (ISO 8601).",
            json!({"from": {"type": "string"}, "to": {"type": "string"}}),
            &["from", "to"],
            SideEffect::Read,
        ),
        spec(
            "calendar.create",
            "Crée un événement. Avec invités : confirmation obligatoire (plancher).",
            json!({
                "title": {"type": "string"},
                "start": {"type": "string", "description": "ISO 8601"},
                "end": {"type": "string"},
                "location": {"type": "string"},
                "attendees": {"type": "array", "items": {"type": "string"}}
            }),
            &["title", "start"],
            SideEffect::WriteExternal,
        ),
        spec(
            "tasks.list",
            "Liste les tâches (statut open par défaut).",
            json!({"status": {"type": "string", "enum": ["open", "done", "all"]}}),
            &[],
            SideEffect::Read,
        ),
        spec(
            "tasks.create",
            "Crée une tâche locale.",
            json!({
                "title": {"type": "string"},
                "due": {"type": "string", "description": "ISO 8601, optionnel"},
                "priority": {"type": "string", "enum": ["haute", "normale", "basse"]}
            }),
            &["title"],
            SideEffect::WriteLocal,
        ),
        spec(
            "tasks.complete",
            "Marque une tâche comme faite.",
            json!({"id": {"type": "string"}}),
            &["id"],
            SideEffect::WriteLocal,
        ),
        spec(
            "commitments.list",
            "Liste les engagements pris ou reçus (promesses extraites des échanges).",
            json!({}),
            &[],
            SideEffect::Read,
        ),
        spec(
            "people.context",
            "Rassemble le contexte connu sur une personne (échanges, fichiers, événements liés).",
            json!({"name": {"type": "string"}}),
            &["name"],
            SideEffect::Read,
        ),
        spec(
            "photos.search",
            "Recherche de photos par métadonnées EXIF (date, lieu GPS) et nom. Renvoie des candidates à confirmer.",
            json!({
                "query": {"type": "string"},
                "from": {"type": "string", "description": "date ISO, optionnel"},
                "to": {"type": "string"}
            }),
            &["query"],
            SideEffect::Read,
        ),
        spec(
            "system.diagnose",
            "Diagnostique l'état de la machine (CPU, mémoire, disque, température, batterie) et explique les causes probables.",
            json!({}),
            &[],
            SideEffect::Read,
        ),
        spec(
            "memory.remember",
            "Mémorise un fait durable dit par l'utilisateur (ex. « mon checkup est mardi 15h »).",
            json!({"fact": {"type": "string"}}),
            &["fact"],
            SideEffect::WriteLocal,
        ),
    ]
}

/// Aperçu lisible pour la confirmation (les confirmations d'actions graves
/// doivent être claires, explicites, non pré-cochées — Sécurité §6).
pub fn preview_for(tool: &str, args: &Value) -> String {
    let s = |k: &str| {
        args.get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("?")
            .to_string()
    };
    match tool {
        "mail.send" => format!(
            "Envoyer un mail à {} — objet : « {} »",
            s("to"),
            s("subject")
        ),
        "mail.draft" => format!(
            "Créer un brouillon pour {} — objet : « {} »",
            s("to"),
            s("subject")
        ),
        "calendar.create" => {
            let attendees = args["attendees"].as_array().map(|a| a.len()).unwrap_or(0);
            if attendees > 0 {
                format!(
                    "Créer l'événement « {} » le {} avec {} invité(s)",
                    s("title"),
                    s("start"),
                    attendees
                )
            } else {
                format!("Créer l'événement « {} » le {}", s("title"), s("start"))
            }
        }
        "tasks.create" => format!("Créer la tâche « {} »", s("title")),
        "tasks.complete" => "Marquer une tâche comme faite".into(),
        "files.apply_reorganize_plan" => "Exécuter le plan de rangement validé".into(),
        "files.move" => format!("Déplacer « {} » dans « {} »", s("source"), s("destination")),
        "memory.remember" => format!("Mémoriser : « {} »", s("fact")),
        _ => format!("{tool} {args}"),
    }
}

/// Exécution effective (appelée pour les lectures, ou après passage de la porte).
pub async fn execute(ctx: &ToolCtx, tool: &str, args: &Value) -> Result<ToolResult> {
    let connector = match tool.split('.').next().unwrap_or("") {
        "mail" => Some("apple"),
        "system" => Some("system"),
        _ => None,
    };
    if let Some(id) = connector {
        if !crate::connectors::is_connected(&ctx.db, id) {
            return Err(AppError::Security(format!(
                "Le connecteur {id} n'est pas activé."
            )));
        }
    }
    if tool.starts_with("mail.") && !crate::connectors::mail::native_available() {
        return Err(AppError::Security(
            "Apple Mail n’est pas autorisé. Ouvre Connecteurs → Services Apple → Apple Mail."
                .into(),
        ));
    }
    match tool {
        "memory.query" | "files.search" | "mail.search" => {
            let query = args["query"].as_str().unwrap_or("");
            let mut results = retrieval::search(&ctx.db, &ctx.llm, query, 10).await?;
            if tool == "files.search" {
                results.retain(|r| r.source == "files");
            } else if tool == "mail.search" {
                results.retain(|r| r.source == "mail");
            }
            crate::security::log_access(
                &ctx.db,
                tool.split('.').next().unwrap_or("memory"),
                "search",
                Some(query),
            );
            Ok(ToolResult {
                result: json!({ "results": results }),
                undo: None,
            })
        }

        "files.reorganize" => {
            let target = args["target_dir"].as_str().unwrap_or("");
            let plan = reorganize::build_plan(&ctx.db, &ctx.llm, target).await?;
            let plan_id = new_id();
            ctx.db.with(|c| {
                c.execute(
                    "INSERT INTO reorganize_plans (id, plan, status, created_at) VALUES (?1,?2,'pending',?3)",
                    rusqlite::params![plan_id, serde_json::to_string(&plan)?, now()],
                )?;
                Ok(())
            })?;
            Ok(ToolResult {
                result: json!({"plan_id": plan_id, "plan": plan}),
                undo: None,
            })
        }

        "files.apply_reorganize_plan" => {
            let plan_id = args["plan_id"]
                .as_str()
                .ok_or(AppError::Invalid("plan_id requis".into()))?;
            let plan: reorganize::Plan = ctx.db.with(|c| {
                let raw: String = c
                    .query_row(
                        "SELECT plan FROM reorganize_plans WHERE id=?1 AND status='pending'",
                        rusqlite::params![plan_id],
                        |r| r.get(0),
                    )
                    .map_err(|_| AppError::NotFound("plan introuvable ou déjà exécuté".into()))?;
                serde_json::from_str(&raw)
                    .map_err(|_| AppError::Invalid("plan de rangement invalide".into()))
            })?;
            let (report, undo) = reorganize::execute_plan(&plan)?;
            ctx.db.with(|c| {
                c.execute(
                    "UPDATE reorganize_plans SET status='executed' WHERE id=?1",
                    rusqlite::params![plan_id],
                )?;
                Ok(())
            })?;
            Ok(ToolResult {
                result: json!({ "report": report }),
                undo: Some(undo),
            })
        }

        "files.move" => {
            let source = args["source"]
                .as_str()
                .ok_or_else(|| AppError::Invalid("source requise".into()))?;
            let destination = args["destination"]
                .as_str()
                .ok_or_else(|| AppError::Invalid("destination requise".into()))?;
            let (report, undo) = reorganize::move_location(&ctx.db, source, destination)?;
            crate::security::log_access(&ctx.db, "files", "move", Some(source));
            Ok(ToolResult {
                result: json!({"report": report}),
                undo: Some(undo),
            })
        }

        "mail.draft" => {
            let id = new_id();
            let (to, subject, body) = (
                args["to"].as_str().unwrap_or(""),
                args["subject"].as_str().unwrap_or(""),
                args["body"].as_str().unwrap_or(""),
            );
            ctx.db.with(|c| {
                c.execute(
                    "INSERT INTO items (id, source, source_ref, type, title, body, ingested_at, status)
                     VALUES (?1, 'mail', ?2, 'draft', ?3, ?4, ?5, 'active')",
                    rusqlite::params![id, format!("draft:{id}"), format!("Brouillon → {to} : {subject}"),
                        format!("À : {to}\nObjet : {subject}\n\n{body}"), now()],
                )?;
                Ok(())
            })?;
            Ok(ToolResult {
                result: json!({"status": "brouillon créé", "draft_id": id, "to": to, "subject": subject}),
                undo: Some(json!({"kind": "delete_item", "id": id})),
            })
        }

        "mail.send" => {
            // L'envoi exige un transport configuré (API/SMTP — doc Connecteurs §1.2).
            // Ce build n'embarque pas de transport : échec propre et honnête, après plancher.
            Err(AppError::Invalid(
                "L'envoi de mail n'est pas encore configuré (aucun compte d'envoi connecté). Le brouillon reste disponible.".into(),
            ))
        }

        "calendar.list" => {
            let (from, to) = (
                args["from"].as_str().unwrap_or(""),
                args["to"].as_str().unwrap_or(""),
            );
            let events = calendar::list_range(&ctx.db, from, to)?;
            Ok(ToolResult {
                result: json!({ "events": events }),
                undo: None,
            })
        }

        "calendar.create" => {
            let ev = calendar::create(&ctx.db, args)?;
            Ok(ToolResult {
                result: json!({"status": "événement créé", "event": ev.clone()}),
                undo: Some(json!({"kind": "delete_event", "id": ev["id"]})),
            })
        }

        "tasks.list" => {
            let status = args["status"].as_str().unwrap_or("open");
            let tasks = ctx.db.with(|c| {
                let sql = if status == "all" {
                    "SELECT id, title, due, status, priority FROM tasks ORDER BY due IS NULL, due LIMIT 100"
                } else {
                    "SELECT id, title, due, status, priority FROM tasks WHERE status = ?1 ORDER BY due IS NULL, due LIMIT 100"
                };
                let mut stmt = c.prepare(sql)?;
                let map = |r: &rusqlite::Row| -> rusqlite::Result<Value> {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "title": r.get::<_, String>(1)?,
                        "due": r.get::<_, Option<i64>>(2)?,
                        "status": r.get::<_, String>(3)?,
                        "priority": r.get::<_, Option<String>>(4)?,
                    }))
                };
                let mut out = vec![];
                if status == "all" {
                    let rows = stmt.query_map([], map)?;
                    for r in rows {
                        out.push(r?);
                    }
                } else {
                    let rows = stmt.query_map([status], map)?;
                    for r in rows {
                        out.push(r?);
                    }
                }
                Ok(out)
            })?;
            Ok(ToolResult {
                result: json!({ "tasks": tasks }),
                undo: None,
            })
        }

        "tasks.create" => {
            let title = args["title"]
                .as_str()
                .ok_or(AppError::Invalid("titre requis".into()))?;
            let due = args["due"].as_str().and_then(parse_iso);
            let id = memory::create_task(
                &ctx.db,
                title,
                due,
                args["priority"].as_str(),
                "conversation",
            )?;
            Ok(ToolResult {
                result: json!({"status": "tâche créée", "id": id, "title": title}),
                undo: Some(json!({"kind": "delete_task", "id": id})),
            })
        }

        "tasks.complete" => {
            let id = args["id"]
                .as_str()
                .ok_or(AppError::Invalid("id requis".into()))?;
            ctx.db.with(|c| {
                c.execute(
                    "UPDATE tasks SET status='done' WHERE id=?1",
                    rusqlite::params![id],
                )?;
                Ok(())
            })?;
            Ok(ToolResult {
                result: json!({"status": "tâche terminée"}),
                undo: Some(json!({"kind": "reopen_task", "id": id})),
            })
        }

        "commitments.list" => {
            let list = ctx.db.with(|c| {
                let mut stmt = c.prepare(
                    "SELECT co.id, co.text, co.direction, co.due, co.status, p.name
                     FROM commitments co LEFT JOIN people p ON p.id = co.person_id
                     WHERE co.status='open' ORDER BY co.due IS NULL, co.due LIMIT 50",
                )?;
                let rows = stmt.query_map([], |r| {
                    Ok(json!({
                        "id": r.get::<_, String>(0)?,
                        "text": r.get::<_, String>(1)?,
                        "direction": r.get::<_, Option<String>>(2)?,
                        "due": r.get::<_, Option<i64>>(3)?,
                        "status": r.get::<_, String>(4)?,
                        "person": r.get::<_, Option<String>>(5)?,
                    }))
                })?;
                let mut out = vec![];
                for r in rows {
                    out.push(r?);
                }
                Ok(out)
            })?;
            Ok(ToolResult {
                result: json!({ "commitments": list }),
                undo: None,
            })
        }

        "people.context" => {
            let name = args["name"].as_str().unwrap_or("");
            let context = people_conn::context(&ctx.db, name)?;
            crate::security::log_access(&ctx.db, "people", "context", Some(name));
            Ok(ToolResult {
                result: context,
                undo: None,
            })
        }

        "photos.search" => {
            let query = args["query"].as_str().unwrap_or("");
            let photos = photos_search(&ctx.db, query, args["from"].as_str(), args["to"].as_str())?;
            Ok(ToolResult {
                result: json!({ "candidates": photos, "note": "candidates classées — à confirmer visuellement" }),
                undo: None,
            })
        }

        "system.diagnose" => {
            let snapshot = system_conn::snapshot();
            let explanation = system_conn::diagnose(&snapshot);
            crate::security::log_access(&ctx.db, "system", "diagnose", None);
            Ok(ToolResult {
                result: json!({ "snapshot": snapshot, "explanation": explanation }),
                undo: None,
            })
        }

        "memory.remember" => {
            let fact = args["fact"]
                .as_str()
                .ok_or(AppError::Invalid("fait requis".into()))?;
            let id = new_id();
            ctx.db.with(|c| {
                c.execute(
                    "INSERT INTO items (id, source, source_ref, type, title, body, ingested_at, status)
                     VALUES (?1, 'conversation', ?2, 'fact', ?3, ?4, ?5, 'active')",
                    rusqlite::params![id, format!("fact:{id}"), fact.chars().take(80).collect::<String>(), fact, now()],
                )?;
                Ok(())
            })?;
            // Embedding pour le retrieval futur.
            if let Ok(vecs) = ctx.llm.embed(&[fact.to_string()]).await {
                if let Some(v) = vecs.first() {
                    memory::replace_embeddings(
                        &ctx.db,
                        &id,
                        &ctx.settings.embed_model,
                        &[(fact.to_string(), Some(crate::llm::vec_to_blob(v)))],
                    )?;
                }
            }
            Ok(ToolResult {
                result: json!({"status": "mémorisé", "id": id}),
                undo: Some(json!({"kind": "delete_item", "id": id})),
            })
        }

        _ => Err(AppError::Invalid(format!("outil inconnu : {tool}"))),
    }
}

pub fn parse_iso(s: &str) -> Option<i64> {
    if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(s) {
        return Some(dt.timestamp());
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        return d
            .and_hms_opt(0, 0, 0)
            .and_then(|dt| dt.and_local_timezone(chrono::Local).single())
            .map(|dt| dt.timestamp());
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M") {
        return dt
            .and_local_timezone(chrono::Local)
            .single()
            .map(|dt| dt.timestamp());
    }
    if let Ok(dt) = chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%d %H:%M") {
        return dt
            .and_local_timezone(chrono::Local)
            .single()
            .map(|dt| dt.timestamp());
    }
    None
}

fn photos_search(db: &Db, query: &str, from: Option<&str>, to: Option<&str>) -> Result<Vec<Value>> {
    let kws: Vec<String> = query
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(|w| w.to_lowercase())
        .collect();
    let from_ts = from.and_then(parse_iso);
    let to_ts = to.and_then(parse_iso);
    db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT id, title, path, body, mtime FROM items
             WHERE type='photo' AND status='active' ORDER BY mtime DESC LIMIT 2000",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, Option<i64>>(4)?,
            ))
        })?;
        let mut out = vec![];
        for r in rows {
            let (id, title, path, body, mtime) = r?;
            if let (Some(f), Some(m)) = (from_ts, mtime) {
                if m < f {
                    continue;
                }
            }
            if let (Some(t), Some(m)) = (to_ts, mtime) {
                if m > t {
                    continue;
                }
            }
            let haystack = format!(
                "{} {} {}",
                title.clone().unwrap_or_default(),
                path.clone().unwrap_or_default(),
                body.clone().unwrap_or_default()
            )
            .to_lowercase();
            let hits = kws.iter().filter(|k| haystack.contains(*k)).count();
            if kws.is_empty() || hits > 0 {
                out.push(json!({
                    "id": id, "title": title, "path": path, "exif": body, "mtime": mtime, "hits": hits
                }));
            }
            if out.len() >= 12 {
                break;
            }
        }
        Ok(out)
    })
}
