//! API interne (doc maître §28) — commandes Tauri. Les noms du contrat stable
//! sont réutilisés tels quels (query, get_startup_brief, confirm_action, …).

use crate::actions;
use crate::bus::BusEvent;
use crate::connectors::{self, calendar, files, mail, people, screen, system};
use crate::db::new_id;
use crate::error::{AppError, Result};
use crate::proactivity::{self, briefs};
use crate::rules;
use crate::settings::Settings;
use crate::state::AppState;
use rusqlite::params;
use serde_json::{json, Value};
use tauri::State;

// ————————————————— Session & sécurité —————————————————

#[tauri::command]
pub fn app_status(state: State<'_, AppState>) -> Value {
    let initialized = state.keystore.exists();
    let email = state.keystore.meta().ok().and_then(|m| m.email);
    let keychain = state.keystore.meta().map(|m| m.keychain).unwrap_or(false);
    let onboarding_done = state
        .core()
        .ok()
        .and_then(|c| crate::settings::load(&c.db).ok())
        .map(|s| s.onboarding_done)
        .unwrap_or(false);
    json!({
        "initialized": initialized,
        "unlocked": state.is_unlocked(),
        "onboarding_done": onboarding_done,
        "email": email,
        "keychain": keychain,
        "keychain_available": state.keystore.keychain_load().is_some(),
    })
}

#[tauri::command]
pub fn setup_master(
    state: State<'_, AppState>,
    email: Option<String>,
    password: String,
) -> Result<Value> {
    let (key_hex, phrase) = state.keystore.setup(email, &password)?;
    state.unlock_with_key(&key_hex)?;
    Ok(json!({ "recovery_phrase": phrase }))
}

#[tauri::command]
pub fn unlock(state: State<'_, AppState>, password: String) -> Result<()> {
    let key = state.keystore.unlock_password(&password)?;
    state.unlock_with_key(&key)?;
    Ok(())
}

#[tauri::command]
pub fn unlock_with_keychain(state: State<'_, AppState>) -> Result<()> {
    let key = state
        .keystore
        .keychain_load()
        .ok_or_else(|| AppError::Security("Aucune clé dans le trousseau OS.".into()))?;
    state.unlock_with_key(&key)?;
    Ok(())
}

#[tauri::command]
pub fn unlock_with_recovery(state: State<'_, AppState>, phrase: String) -> Result<()> {
    let key = state.keystore.unlock_phrase(&phrase)?;
    state.unlock_with_key(&key)?;
    Ok(())
}

#[tauri::command]
pub async fn lock(state: State<'_, AppState>) -> Result<()> {
    state.lock().await;
    Ok(())
}

#[tauri::command]
pub fn change_master_password(
    state: State<'_, AppState>,
    current: String,
    new_password: String,
) -> Result<()> {
    let key = state.keystore.unlock_password(&current)?;
    state.keystore.change_password(&key, &new_password)
}

#[tauri::command]
pub fn regenerate_recovery(state: State<'_, AppState>, password: String) -> Result<String> {
    let key = state.keystore.unlock_password(&password)?;
    state.keystore.regenerate_phrase(&key)
}

#[tauri::command]
pub fn set_keychain(state: State<'_, AppState>, enabled: bool) -> Result<()> {
    if enabled {
        let core = state.core()?;
        let key = core
            .key_hex
            .lock()
            .map_err(|_| AppError::Other("clé indisponible".into()))?;
        state.keystore.keychain_store(&key)
    } else {
        state.keystore.keychain_clear()
    }
}

// ————————————————— Conversation —————————————————

#[tauri::command]
pub async fn query(
    state: State<'_, AppState>,
    session_id: Option<String>,
    text: String,
) -> Result<crate::router::Answer> {
    let core = state.core()?;
    let sid = session_id.unwrap_or_else(new_id);
    crate::router::handle_query(&core, &sid, &text).await
}

#[tauri::command]
pub fn list_sessions(state: State<'_, AppState>) -> Result<Vec<Value>> {
    let core = state.core()?;
    core.db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT id, title, created_at, updated_at FROM sessions ORDER BY updated_at DESC LIMIT 100",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "title": r.get::<_, Option<String>>(1)?,
                "created_at": r.get::<_, i64>(2)?,
                "updated_at": r.get::<_, i64>(3)?,
            }))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

#[tauri::command]
pub fn get_conversation(state: State<'_, AppState>, session_id: String) -> Result<Vec<Value>> {
    let core = state.core()?;
    core.db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT role, content, created_at FROM conversations
             WHERE session_id = ?1 AND role IN ('user','assistant') ORDER BY turn",
        )?;
        let rows = stmt.query_map(params![session_id], |r| {
            Ok(json!({
                "role": r.get::<_, String>(0)?,
                "content": r.get::<_, String>(1)?,
                "created_at": r.get::<_, i64>(2)?,
            }))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

// ————————————————— Briefs —————————————————

#[tauri::command]
pub fn get_startup_brief(state: State<'_, AppState>) -> Result<briefs::Brief> {
    let core = state.core()?;
    briefs::build_brief(&core.db)
}

#[tauri::command]
pub fn get_daily_wrap(state: State<'_, AppState>) -> Result<Value> {
    let core = state.core()?;
    briefs::build_daily_wrap(&core.db)
}

// ————————————————— Actions (porte + plancher) —————————————————

#[tauri::command]
pub fn list_pending_actions(state: State<'_, AppState>) -> Result<Vec<actions::PendingAction>> {
    let core = state.core()?;
    actions::list_pending(&core.db)
}

#[tauri::command]
pub fn list_actions(
    state: State<'_, AppState>,
    status: Option<String>,
    limit: Option<usize>,
) -> Result<Vec<actions::PendingAction>> {
    let core = state.core()?;
    actions::list_actions(&core.db, status.as_deref(), limit.unwrap_or(100))
}

#[tauri::command]
pub async fn confirm_action(state: State<'_, AppState>, action_id: String) -> Result<Value> {
    let core = state.core()?;
    let action = actions::get_action(&core.db, &action_id)?;
    if action.status != "awaiting_confirmation" {
        return Err(AppError::Invalid(
            "cette action n'attend plus de confirmation".into(),
        ));
    }
    let settings = crate::settings::load(&core.db)?;
    let ctx = crate::tools::ToolCtx {
        db: core.db.clone(),
        llm: core.llm.clone(),
        bus: core.bus.clone(),
        settings,
    };
    if let Some(sid) = &action.session_id {
        core.bus.emit(BusEvent::AgentProgress {
            session_id: sid.clone(),
            stage: "execute".into(),
            title: crate::tools::preview_for(&action.tool, &action.input),
            detail: Some("Action confirmée, exécution locale en cours".into()),
            current: 4,
            total: 5,
            status: "running".into(),
        });
    }
    match crate::tools::execute(&ctx, &action.tool, &action.input).await {
        Ok(outcome) => {
            let result_text = outcome.result.to_string();
            actions::set_action_result(
                &core.db,
                &action_id,
                "executed",
                Some(
                    &outcome
                        .result
                        .to_string()
                        .chars()
                        .take(800)
                        .collect::<String>(),
                ),
                outcome.undo.as_ref(),
            )?;
            core.bus.emit(BusEvent::ActionResolved {
                action_id,
                status: "executed".into(),
            });
            if let Some(sid) = &action.session_id {
                let human_result = outcome.result["report"].as_str().unwrap_or(&result_text);
                crate::memory::persist_turn(&core.db, sid, "assistant", human_result)?;
                core.bus.emit(BusEvent::AgentProgress {
                    session_id: sid.clone(),
                    stage: "complete".into(),
                    title: "Action terminée et vérifiée".into(),
                    detail: Some(human_result.into()),
                    current: 5,
                    total: 5,
                    status: "done".into(),
                });
            }
            Ok(outcome.result)
        }
        Err(e) => {
            actions::set_action_result(&core.db, &action_id, "failed", Some(&e.to_string()), None)?;
            core.bus.emit(BusEvent::ActionResolved {
                action_id,
                status: "failed".into(),
            });
            Err(e)
        }
    }
}

#[tauri::command]
pub fn reject_action(state: State<'_, AppState>, action_id: String) -> Result<()> {
    let core = state.core()?;
    actions::set_action_result(&core.db, &action_id, "rejected", None, None)?;
    core.bus.emit(BusEvent::ActionResolved {
        action_id,
        status: "rejected".into(),
    });
    Ok(())
}

#[tauri::command]
pub fn undo_action(state: State<'_, AppState>, action_id: String) -> Result<String> {
    let core = state.core()?;
    let action = actions::get_action(&core.db, &action_id)?;
    if action.status != "executed" {
        return Err(AppError::Invalid(
            "seule une action exécutée peut être annulée".into(),
        ));
    }
    let undo = action
        .undo
        .ok_or_else(|| AppError::Invalid("cette action n'a pas de journal d'annulation".into()))?;
    let report = actions::apply_undo(&core.db, &undo)?;
    actions::set_action_result(&core.db, &action_id, "undone", Some(&report), None)?;
    core.bus.emit(BusEvent::ActionResolved {
        action_id,
        status: "undone".into(),
    });
    Ok(report)
}

// ————————————————— Connecteurs —————————————————

#[tauri::command]
pub fn connector_status(state: State<'_, AppState>) -> Result<Vec<connectors::ConnectorInfo>> {
    let core = state.core()?;
    connectors::list(&core.db)
}

#[tauri::command]
pub fn native_permissions(state: State<'_, AppState>) -> Result<Value> {
    let core = state.core()?;
    #[cfg(target_os = "macos")]
    {
        let folders = files::folder_paths(&core.db)?;
        let screen_status = connectors::is_connected(&core.db, "screen");
        return Ok(json!({
            "platform": "macos",
            "provider": "Apple",
            "services": [
                {"id":"files", "label":"Fichiers et dossiers", "status": if folders.is_empty() {"needs_selection"} else {"granted"}, "detail": format!("{} dossier(s) autorisé(s)", folders.len()), "settings":"files", "operational":true},
                {"id":"mail", "label":"Apple Mail", "status": if mail::native_available() {"granted"} else {"needs_permission"}, "detail":"Lecture locale des messages synchronisés", "settings":"all_files", "operational":true},
                {"id":"contacts", "label":"Contacts", "status": connectors::native::permission_status("contacts"), "detail":"API Contacts native", "settings":"contacts", "operational":true},
                {"id":"calendar", "label":"Calendrier", "status": connectors::native::permission_status("calendar"), "detail":"EventKit natif en lecture et écriture", "settings":"calendars", "operational":true},
                {"id":"reminders", "label":"Rappels", "status": connectors::native::permission_status("reminders"), "detail":"Autorisation prête ; synchronisation des tâches à finaliser", "settings":"reminders", "operational":false},
                {"id":"photos", "label":"Photos", "status": connectors::native::permission_status("photos"), "detail":"Autorisation prête ; recherche PhotoKit à finaliser", "settings":"photos", "operational":false},
                {"id":"screen", "label":"Contexte d’écran", "status": if screen_status || connectors::native::permission_status("screen") == "granted" {"granted"} else {"needs_permission"}, "detail":"Application et fenêtre actives", "settings":"accessibility", "operational":true}
            ]
        }));
    }
    #[cfg(target_os = "windows")]
    {
        return Ok(json!({"platform":"windows", "provider":"Windows", "services":[]}));
    }
    #[allow(unreachable_code)]
    Ok(json!({"platform":"other", "provider":"Système", "services":[]}))
}

#[tauri::command]
pub async fn request_native_permission(
    state: State<'_, AppState>,
    service: String,
) -> Result<Value> {
    let core = state.core()?;
    let requested = service.clone();
    let status =
        tokio::task::spawn_blocking(move || connectors::native::request_permission(&requested))
            .await
            .map_err(|e| AppError::Other(format!("Demande d’autorisation interrompue : {e}")))??;
    if service == "screen" && status == "granted" {
        connectors::set_status(&core.db, "screen", "screen", "connected")?;
    }
    Ok(json!({"service": service, "status": status}))
}

#[tauri::command]
pub fn open_native_settings(section: String) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let pane = match section.as_str() {
            "all_files" => "Privacy_AllFiles",
            "contacts" => "Privacy_Contacts",
            "calendars" => "Privacy_Calendars",
            "reminders" => "Privacy_Reminders",
            "photos" => "Privacy_Photos",
            "accessibility" => "Privacy_Accessibility",
            _ => "Privacy",
        };
        std::process::Command::new("open")
            .arg(format!(
                "x-apple.systempreferences:com.apple.preference.security?{pane}"
            ))
            .spawn()
            .map_err(|e| AppError::Other(format!("Impossible d’ouvrir les réglages : {e}")))?;
    }
    Ok(())
}

#[tauri::command]
pub async fn connector_connect(state: State<'_, AppState>, id: String) -> Result<Value> {
    let core = state.core()?;
    match id.as_str() {
        "apple" => {
            if !mail::native_available() {
                connectors::set_status(&core.db, "apple", "apple", "needs_permission")?;
                return Ok(json!({
                    "status": "needs_permission",
                    "message": "Autorise « Accès complet au disque » pour Syn dans Réglages système → Confidentialité et sécurité, puis réessaie."
                }));
            }
            connectors::set_status(&core.db, "apple", "apple", "syncing")?;
            let settings = crate::settings::load(&core.db)?;
            let db = core.db.clone();
            let llm = core.llm.clone();
            let bus = core.bus.clone();
            tauri::async_runtime::spawn(async move {
                bus.emit(BusEvent::SyncProgress {
                    connector: "apple".into(),
                    pct: 0.0,
                    message: Some("Lecture de la boîte Apple Mail locale…".into()),
                });
                match mail::sync_native(&db, &llm, &bus, &settings.embed_model).await {
                    Ok(n) => bus.emit(BusEvent::SyncProgress {
                        connector: "apple".into(),
                        pct: 100.0,
                        message: Some(format!("{n} mail(s) ingérés.")),
                    }),
                    Err(e) => {
                        let _ = connectors::set_status(&db, "apple", "apple", "unavailable");
                        bus.emit(BusEvent::SyncProgress {
                            connector: "apple".into(),
                            pct: 100.0,
                            message: Some(format!("Erreur : {e}")),
                        })
                    }
                }
            });
            Ok(
                json!({"status": "connected", "message": "Synchronisation de la boîte locale lancée."}),
            )
        }
        "screen" => {
            let ctx = screen::frontmost_context();
            if ctx["available"].as_bool() == Some(true) {
                connectors::set_status(&core.db, "screen", "screen", "connected")?;
                Ok(json!({"status": "connected"}))
            } else {
                connectors::set_status(&core.db, "screen", "screen", "needs_permission")?;
                Ok(json!({"status": "needs_permission", "message": ctx["reason"]}))
            }
        }
        "google" | "microsoft" | "slack" | "github" => Ok(json!({
            "status": "needs_configuration",
            "message": "Ce connecteur exige l'enregistrement OAuth de l'application auprès du fournisseur (scopes sensibles : vérification d'app, audit CASA côté Google). À configurer avant activation."
        })),
        "files" | "system" => {
            connectors::set_status(&core.db, &id, &id, "connected")?;
            Ok(json!({"status": "connected"}))
        }
        _ => Err(AppError::NotFound(format!("connecteur inconnu : {id}"))),
    }
}

#[tauri::command]
pub fn connector_disconnect(state: State<'_, AppState>, id: String) -> Result<()> {
    let core = state.core()?;
    if id == "apple" && cfg!(target_os = "macos") {
        return Err(AppError::Invalid(
            "Apple est intégré à macOS : révoque séparément les autorisations dans Réglages système.".into(),
        ));
    }
    connectors::set_status(&core.db, &id, &id, "disconnected")?;
    if id == "apple" {
        core.db.with(|c| {
            c.execute("UPDATE items SET status='removed' WHERE source='mail'", [])?;
            Ok(())
        })?;
    }
    crate::security::log_access(&core.db, &id, "disconnect", None);
    Ok(())
}

#[tauri::command]
pub fn screen_context(state: State<'_, AppState>) -> Result<Value> {
    let core = state.core()?;
    if !connectors::is_connected(&core.db, "screen") {
        return Err(AppError::Security(
            "Le connecteur Contexte d'écran n'est pas activé.".into(),
        ));
    }
    crate::security::log_access(&core.db, "screen", "frontmost", None);
    Ok(screen::frontmost_context())
}

// ————————————————— Réglages —————————————————

#[tauri::command]
pub fn get_settings(state: State<'_, AppState>) -> Result<Settings> {
    let core = state.core()?;
    crate::settings::load(&core.db)
}

#[tauri::command]
pub fn set_settings(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    patch: Value,
) -> Result<Settings> {
    let core = state.core()?;
    let current = crate::settings::load(&core.db)?;
    let mut merged = serde_json::to_value(&current)?;
    let rebuild_llm = ["chat_model", "embed_model", "ollama_url"]
        .iter()
        .any(|k| patch.get(k).is_some());
    if let (Some(obj), Some(p)) = (merged.as_object_mut(), patch.as_object()) {
        for (k, v) in p {
            obj.insert(k.clone(), v.clone());
        }
    }
    let new_settings: Settings = serde_json::from_value(merged)
        .map_err(|e| AppError::Invalid(format!("réglage invalide : {e}")))?;
    if patch.get("autostart").is_some() {
        use tauri_plugin_autostart::ManagerExt;
        if new_settings.autostart {
            app.autolaunch()
                .enable()
                .map_err(|e| AppError::Other(format!("démarrage automatique : {e}")))?;
        } else {
            app.autolaunch()
                .disable()
                .map_err(|e| AppError::Other(format!("démarrage automatique : {e}")))?;
        }
    }
    crate::settings::save(&core.db, &new_settings)?;
    if current.sensitive_consent && !new_settings.sensitive_consent {
        let sensitive_ids: Vec<String> = core.db.with(|c| {
            let mut stmt =
                c.prepare("SELECT id, path FROM items WHERE source='files' AND status='active'")?;
            let rows = stmt.query_map([], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<String>>(1)?))
            })?;
            let mut ids = vec![];
            for row in rows {
                let (id, path) = row?;
                if path
                    .as_deref()
                    .map(std::path::Path::new)
                    .map(files::looks_sensitive)
                    .unwrap_or(false)
                {
                    ids.push(id);
                }
            }
            Ok(ids)
        })?;
        core.db.with(|c| {
            for id in &sensitive_ids {
                c.execute("DELETE FROM embeddings WHERE item_id=?1", params![id])?;
                c.execute(
                    "UPDATE items SET body=NULL, type='sensible_non_lu' WHERE id=?1",
                    params![id],
                )?;
            }
            Ok(())
        })?;
    } else if !current.sensitive_consent && new_settings.sensitive_consent {
        let _ = core.indexer.tx.send(files::IndexJob::FullScan(None));
    }
    core.indexer.paused.store(
        new_settings.indexing_paused || new_settings.eco_mode,
        std::sync::atomic::Ordering::SeqCst,
    );
    if rebuild_llm {
        state.rebuild_llm()?;
    }
    Ok(new_settings)
}

#[tauri::command]
pub fn hardware_info() -> crate::llm::profiles::HardwareProfile {
    crate::llm::profiles::detect()
}

#[tauri::command]
pub async fn llm_status(state: State<'_, AppState>) -> Result<crate::llm::LlmStatus> {
    let core = state.core()?;
    Ok(core.llm.status().await)
}

#[tauri::command]
pub async fn model_pull(state: State<'_, AppState>, model: String) -> Result<()> {
    let core = state.core()?;
    let (tx, mut rx) = tokio::sync::mpsc::channel::<(f32, String)>(32);
    let bus = core.bus.clone();
    let model_name = model.clone();
    tauri::async_runtime::spawn(async move {
        while let Some((pct, status)) = rx.recv().await {
            bus.emit(BusEvent::ModelPullProgress {
                model: model_name.clone(),
                pct,
                status,
            });
        }
    });
    let llm = core.llm.clone();
    let bus2 = core.bus.clone();
    tauri::async_runtime::spawn(async move {
        match llm.pull(&model, tx).await {
            Ok(_) => bus2.emit(BusEvent::ModelPullProgress {
                model,
                pct: 100.0,
                status: "terminé".into(),
            }),
            Err(e) => bus2.emit(BusEvent::ModelPullProgress {
                model,
                pct: -1.0,
                status: format!("erreur : {e}"),
            }),
        }
    });
    Ok(())
}

// ————————————————— Files —————————————————

#[tauri::command]
pub fn files_add_folder(state: State<'_, AppState>, path: String) -> Result<()> {
    let core = state.core()?;
    let canonical = std::path::Path::new(&path)
        .canonicalize()
        .map_err(|_| AppError::Invalid("dossier introuvable".into()))?;
    if !canonical.is_dir() {
        return Err(AppError::Invalid("dossier introuvable".into()));
    }
    let path = canonical.to_string_lossy().to_string();
    core.db.with(|c| {
        c.execute(
            "INSERT INTO folders (path, added_at, status) VALUES (?1, ?2, 'active')
             ON CONFLICT(path) DO UPDATE SET status='active'",
            params![path, crate::db::now()],
        )?;
        Ok(())
    })?;
    core.indexer.watch_folder(std::path::Path::new(&path));
    let _ = core
        .indexer
        .tx
        .send(files::IndexJob::FullScan(Some(path.into())));
    Ok(())
}

#[tauri::command]
pub fn files_remove_folder(state: State<'_, AppState>, path: String) -> Result<()> {
    let core = state.core()?;
    let canonical = std::path::Path::new(&path)
        .canonicalize()
        .unwrap_or_else(|_| path.clone().into());
    let path = canonical.to_string_lossy().to_string();
    let prefix = format!(
        "{}{}%",
        path.trim_end_matches(std::path::MAIN_SEPARATOR),
        std::path::MAIN_SEPARATOR
    );
    core.db.with(|c| {
        c.execute(
            "UPDATE folders SET status='removed' WHERE path=?1",
            params![path],
        )?;
        c.execute(
            "UPDATE items SET status='removed' WHERE source='files' AND (path=?1 OR path LIKE ?2)",
            params![path, prefix],
        )?;
        Ok(())
    })?;
    core.indexer.unwatch_folder(std::path::Path::new(&path));
    Ok(())
}

#[tauri::command]
pub fn files_reindex(state: State<'_, AppState>, path: Option<String>) -> Result<()> {
    let core = state.core()?;
    let _ = core
        .indexer
        .tx
        .send(files::IndexJob::FullScan(path.map(Into::into)));
    Ok(())
}

#[tauri::command]
pub fn files_index_status(state: State<'_, AppState>) -> Result<files::IndexStatus> {
    let core = state.core()?;
    core.indexer.status(&core.db)
}

#[tauri::command]
pub async fn files_search(
    state: State<'_, AppState>,
    query_text: String,
) -> Result<Vec<crate::retrieval::Retrieved>> {
    let core = state.core()?;
    let mut results = crate::retrieval::search(&core.db, &core.llm, &query_text, 20).await?;
    results.retain(|r| r.source == "files");
    Ok(results)
}

#[tauri::command]
pub async fn search_memory(
    state: State<'_, AppState>,
    query_text: String,
) -> Result<Vec<crate::retrieval::Retrieved>> {
    let core = state.core()?;
    crate::retrieval::search(&core.db, &core.llm, &query_text, 20).await
}

// ————————————————— Connaissances —————————————————

#[tauri::command]
pub fn knowledge_stats(state: State<'_, AppState>) -> Result<Value> {
    let core = state.core()?;
    core.db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT source, type, COUNT(*) FROM items WHERE status='active' GROUP BY source, type",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(json!({"source": r.get::<_, String>(0)?, "type": r.get::<_, String>(1)?, "count": r.get::<_, i64>(2)?}))
        })?;
        let mut by = vec![];
        for r in rows {
            by.push(r?);
        }
        let people: i64 = c.query_row("SELECT COUNT(*) FROM people", [], |r| r.get(0))?;
        let embeddings: i64 = c.query_row("SELECT COUNT(*) FROM embeddings WHERE vector IS NOT NULL", [], |r| r.get(0))?;
        let facts: i64 = c.query_row("SELECT COUNT(*) FROM items WHERE type='fact' AND status='active'", [], |r| r.get(0))?;
        Ok(json!({"by_type": by, "people": people, "embeddings": embeddings, "facts": facts}))
    })
}

#[tauri::command]
pub fn list_knowledge(
    state: State<'_, AppState>,
    source: Option<String>,
    filter: Option<String>,
    limit: Option<i64>,
) -> Result<Vec<Value>> {
    let core = state.core()?;
    core.db.with(|c| {
        let sql = "SELECT id, source, source_ref, type, title, path, size, mtime, ingested_at FROM items
                   WHERE status='active'
                   AND (?1 IS NULL OR source = ?1)
                   AND (?2 IS NULL OR lower(COALESCE(title,'') || ' ' || COALESCE(path,'')) LIKE '%'||lower(?2)||'%')
                   ORDER BY ingested_at DESC LIMIT ?3";
        let mut stmt = c.prepare(sql)?;
        let rows = stmt.query_map(params![source, filter, limit.unwrap_or(200)], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "source": r.get::<_, String>(1)?,
                "source_ref": r.get::<_, String>(2)?,
                "type": r.get::<_, String>(3)?,
                "title": r.get::<_, Option<String>>(4)?,
                "path": r.get::<_, Option<String>>(5)?,
                "size": r.get::<_, Option<i64>>(6)?,
                "mtime": r.get::<_, Option<i64>>(7)?,
                "ingested_at": r.get::<_, i64>(8)?,
            }))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

/// « Gérer ce que Syn a appris » : oubli d'un élément (droit de l'utilisateur).
#[tauri::command]
pub fn forget_item(state: State<'_, AppState>, item_id: String) -> Result<()> {
    let core = state.core()?;
    core.db.with(|c| {
        c.execute(
            "INSERT OR IGNORE INTO ignored_items (source, source_ref, ignored_at)
             SELECT source, source_ref, strftime('%s','now') FROM items WHERE id=?1",
            params![item_id],
        )?;
        c.execute(
            "DELETE FROM embeddings WHERE item_id = ?1",
            params![item_id],
        )?;
        c.execute(
            "DELETE FROM person_links WHERE item_id = ?1",
            params![item_id],
        )?;
        c.execute("DELETE FROM items WHERE id = ?1", params![item_id])?;
        Ok(())
    })?;
    crate::security::log_access(&core.db, "memory", "forget", Some(&item_id));
    Ok(())
}

// ————————————————— Personnes & apprentissage —————————————————

#[tauri::command]
pub fn get_person_context(state: State<'_, AppState>, name: String) -> Result<Value> {
    let core = state.core()?;
    people::context(&core.db, &name)
}

#[tauri::command]
pub fn people_list(state: State<'_, AppState>) -> Result<Vec<Value>> {
    let core = state.core()?;
    people::list_people(&core.db)
}

#[tauri::command]
pub fn people_os_preview(state: State<'_, AppState>) -> Result<Vec<Value>> {
    let _ = state.core()?;
    people::os_contacts_preview()
}

#[tauri::command]
pub fn people_add(
    state: State<'_, AppState>,
    name: String,
    relationship: Option<String>,
    email: Option<String>,
    phone: Option<String>,
    birthday: Option<String>,
) -> Result<String> {
    let core = state.core()?;
    people::import_person(
        &core.db,
        &name,
        relationship.as_deref(),
        email.as_deref(),
        phone.as_deref(),
        birthday.as_deref(),
    )
}

#[tauri::command]
pub fn unknowns_pending(state: State<'_, AppState>) -> Result<Vec<Value>> {
    let core = state.core()?;
    people::pending_unknowns(&core.db)
}

#[tauri::command]
pub fn unknown_label(
    state: State<'_, AppState>,
    unknown_id: String,
    name: String,
    relationship: Option<String>,
) -> Result<()> {
    let core = state.core()?;
    people::label_unknown(&core.db, &unknown_id, &name, relationship.as_deref())
}

#[tauri::command]
pub fn unknown_ignore(state: State<'_, AppState>, unknown_id: String) -> Result<()> {
    let core = state.core()?;
    core.db.with(|c| {
        c.execute(
            "UPDATE unknown_names SET status='ignored' WHERE id=?1",
            params![unknown_id],
        )?;
        Ok(())
    })
}

/// « Apprendre à Syn » : fait durable saisi par l'utilisateur (canal de confiance).
#[tauri::command]
pub async fn add_fact(state: State<'_, AppState>, text: String) -> Result<String> {
    let core = state.core()?;
    let id = people::add_fact_about(&core.db, &text)?;
    if let Ok(vecs) = core.llm.embed(std::slice::from_ref(&text)).await {
        if let Some(v) = vecs.first() {
            let settings = crate::settings::load(&core.db)?;
            crate::memory::replace_embeddings(
                &core.db,
                &id,
                &settings.embed_model,
                &[(text, Some(crate::llm::vec_to_blob(v)))],
            )?;
        }
    }
    Ok(id)
}

// ————————————————— Règles —————————————————

#[tauri::command]
pub async fn rules_add(state: State<'_, AppState>, text: String) -> Result<rules::RuleOutcome> {
    let core = state.core()?;
    rules::add_rule(&core.db, &core.llm, &core.bus, &text).await
}

#[tauri::command]
pub async fn rules_edit(
    state: State<'_, AppState>,
    id: String,
    text: String,
) -> Result<rules::RuleOutcome> {
    let core = state.core()?;
    rules::edit_rule(&core.db, &core.llm, &core.bus, &id, &text).await
}

#[tauri::command]
pub fn rules_delete(state: State<'_, AppState>, id: String) -> Result<()> {
    let core = state.core()?;
    rules::delete_rule(&core.db, &core.bus, &id)
}

#[tauri::command]
pub fn rules_list(state: State<'_, AppState>) -> Result<Vec<rules::Rule>> {
    let core = state.core()?;
    rules::list_rules(&core.db)
}

#[tauri::command]
pub fn rules_set_priority(state: State<'_, AppState>, id: String, over_id: String) -> Result<()> {
    let core = state.core()?;
    rules::set_priority(&core.db, &core.bus, &id, &over_id)
}

// ————————————————— Proactivité, système, archives —————————————————

#[tauri::command]
pub fn list_surfacings(state: State<'_, AppState>, limit: Option<usize>) -> Result<Vec<Value>> {
    let core = state.core()?;
    proactivity::list_surfacings(&core.db, limit.unwrap_or(50))
}

#[tauri::command]
pub fn dismiss_surfacing(state: State<'_, AppState>, id: String) -> Result<()> {
    let core = state.core()?;
    core.db.with(|c| {
        c.execute(
            "UPDATE proactive_log SET dismissed=1 WHERE id=?1",
            params![id],
        )?;
        Ok(())
    })
}

#[tauri::command]
pub fn list_triggers(state: State<'_, AppState>) -> Result<Vec<Value>> {
    let core = state.core()?;
    core.db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT t.id, t.type, t.condition, t.priority, t.reason_template, t.action, t.source, t.enabled, t.last_fired, r.text
             FROM triggers t LEFT JOIN rules r ON r.id = t.rule_id ORDER BY t.rowid DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "type": r.get::<_, String>(1)?,
                "condition": r.get::<_, String>(2)?,
                "priority": r.get::<_, String>(3)?,
                "reason_template": r.get::<_, String>(4)?,
                "action": r.get::<_, String>(5)?,
                "source": r.get::<_, String>(6)?,
                "enabled": r.get::<_, i64>(7)? != 0,
                "last_fired": r.get::<_, Option<i64>>(8)?,
                "rule_text": r.get::<_, Option<String>>(9)?,
            }))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

#[tauri::command]
pub fn trigger_toggle(state: State<'_, AppState>, id: String, enabled: bool) -> Result<()> {
    let core = state.core()?;
    core.db.with(|c| {
        c.execute(
            "UPDATE triggers SET enabled=?2 WHERE id=?1",
            params![id, enabled as i64],
        )?;
        Ok(())
    })
}

#[tauri::command]
pub fn system_snapshot(state: State<'_, AppState>) -> Result<Value> {
    let core = state.core()?;
    if !connectors::is_connected(&core.db, "system") {
        return Err(AppError::Security(
            "Le connecteur Système n'est pas activé.".into(),
        ));
    }
    let snap = system::snapshot();
    let diag = system::diagnose(&snap);
    Ok(json!({"snapshot": snap, "explanation": diag}))
}

#[tauri::command]
pub fn access_log_list(state: State<'_, AppState>, limit: Option<usize>) -> Result<Vec<Value>> {
    let core = state.core()?;
    core.db.with(|c| {
        let mut stmt = c.prepare(
            "SELECT connector, operation, item_ref, created_at FROM access_log ORDER BY created_at DESC LIMIT ?1",
        )?;
        let rows = stmt.query_map([limit.unwrap_or(200) as i64], |r| {
            Ok(json!({
                "connector": r.get::<_, String>(0)?,
                "operation": r.get::<_, String>(1)?,
                "item_ref": r.get::<_, Option<String>>(2)?,
                "created_at": r.get::<_, i64>(3)?,
            }))
        })?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

// ————————————————— Calendrier & tâches (UI directe) —————————————————

#[tauri::command]
pub fn calendar_today(state: State<'_, AppState>) -> Result<Vec<Value>> {
    let core = state.core()?;
    calendar::today_events(&core.db)
}

#[tauri::command]
pub fn tasks_quick_add(
    state: State<'_, AppState>,
    title: String,
    due: Option<String>,
) -> Result<String> {
    let core = state.core()?;
    let due_ts = due.as_deref().and_then(crate::tools::parse_iso);
    crate::memory::create_task(&core.db, &title, due_ts, None, "ui")
}

// ————————————————— Données / stockage —————————————————

#[tauri::command]
pub fn storage_stats(state: State<'_, AppState>) -> Result<Value> {
    let core = state.core()?;
    let db_size = std::fs::metadata(state.db_path())
        .map(|m| m.len())
        .unwrap_or(0);
    let (items, embeddings): (i64, i64) = core.db.with(|c| {
        Ok((
            c.query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))?,
            c.query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))?,
        ))
    })?;
    Ok(json!({
        "db_bytes": db_size,
        "items": items,
        "embeddings": embeddings,
        "data_dir": state.data_dir.to_string_lossy(),
    }))
}

/// Export : révèle le dossier de données (base chiffrée + meta) — l'utilisateur
/// possède ses données, il peut les copier telles quelles.
#[tauri::command]
pub fn data_dir_path(state: State<'_, AppState>) -> String {
    state.data_dir.to_string_lossy().to_string()
}

/// Purge complète : désinstallation = suppression (invariant 4).
#[tauri::command]
pub async fn purge_all_data(state: State<'_, AppState>, password: String) -> Result<()> {
    state.keystore.unlock_password(&password)?; // preuve de possession
                                                // Le trousseau doit être nettoyé tant que le fichier de métadonnées existe.
    state.keystore.keychain_clear()?;
    state.lock().await;
    // Laisse à l'indexeur le temps d'observer son signal d'arrêt.
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    for path in [
        state.db_path(),
        state.data_dir.join("syn.db-wal"),
        state.data_dir.join("syn.db-shm"),
        state.data_dir.join("syn-meta.json"),
    ] {
        if path.exists() {
            std::fs::remove_file(&path).map_err(|e| {
                AppError::Other(format!(
                    "suppression de {} impossible : {e}",
                    path.display()
                ))
            })?;
        }
    }
    Ok(())
}

#[tauri::command]
pub fn onboarding_complete(state: State<'_, AppState>) -> Result<()> {
    let core = state.core()?;
    let mut s = crate::settings::load(&core.db)?;
    s.onboarding_done = true;
    crate::settings::save(&core.db, &s)
}

#[tauri::command]
pub fn open_source(
    app: tauri::AppHandle,
    state: State<'_, AppState>,
    source_ref: String,
) -> Result<()> {
    let _ = state.core()?;
    if std::path::Path::new(&source_ref).exists() {
        use tauri_plugin_opener::OpenerExt;
        app.opener()
            .open_path(source_ref, None::<String>)
            .map_err(|e| AppError::Other(e.to_string()))?;
        Ok(())
    } else {
        Err(AppError::NotFound(
            "cette source n'est pas un fichier ouvrable".into(),
        ))
    }
}

#[tauri::command]
pub fn show_main_window(app: tauri::AppHandle) {
    crate::lifecycle::show_main(&app);
}

#[tauri::command]
pub fn hide_bar(app: tauri::AppHandle) {
    use tauri::Manager;
    if let Some(bar) = app.get_webview_window("bar") {
        let _ = bar.hide();
    }
}
