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
        // On ne LIT jamais la clé du trousseau ici : app_status est appelé en
        // boucle, écran verrouillé compris (audit §2). Le drapeau opt-in suffit.
        "keychain_available": keychain,
    })
}

/// Les modèles locaux sont-ils chargés ? L'écran de démarrage s'appuie dessus.
/// Répond aussi `true` quand le moteur est absent : mieux vaut entrer dans une
/// application dégradée que rester bloqué sur un écran d'attente.
#[tauri::command]
pub fn runtime_ready(state: State<'_, AppState>) -> bool {
    state.runtime_ready.load(std::sync::atomic::Ordering::SeqCst)
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
) -> Result<Value> {
    // Rotation complète (audit §2) : changer le mot de passe sans re-chiffrer
    // la base laissait l'ancienne clé valable à vie. Ici : nouvelle clé K',
    // rekey SQLCipher, nouvelles enveloppes, nouvelle phrase de récupération.
    let old_key = state.keystore.unlock_password(&current)?;
    let core = state.core()?;
    let new_key = crate::security::keys::KeyStore::generate_key_hex();
    core.db.rekey(&new_key)?;
    let phrase = match state.keystore.rotate(&new_key, &new_password) {
        Ok(p) => p,
        Err(e) => {
            // La base est déjà re-chiffrée : on tente de revenir à l'ancienne
            // clé pour ne jamais laisser meta et base désynchronisés.
            let _ = core.db.rekey(&old_key);
            return Err(e);
        }
    };
    if let Ok(mut k) = core.key_hex.lock() {
        *k = new_key.clone();
    }
    if state.keystore.meta().map(|m| m.keychain).unwrap_or(false) {
        let _ = state.keystore.keychain_store(&new_key);
    }
    Ok(json!({ "recovery_phrase": phrase }))
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
    screen_context: Option<Value>,
) -> Result<crate::router::Answer> {
    let core = state.core()?;
    let sid = session_id.unwrap_or_else(new_id);
    crate::router::handle_query_with_context(&core, &sid, &text, screen_context.as_ref()).await
}

/// Confie un document à une conversation : Syn le lit une fois, et son contenu
/// suit la conversation à chaque tour.
#[tauri::command]
pub fn attach_document(
    state: State<'_, AppState>,
    session_id: String,
    path: String,
) -> Result<crate::tools::attachments::SessionDocument> {
    let core = state.core()?;
    crate::tools::attachments::attach(&core.db, &session_id, std::path::Path::new(&path))
}

#[tauri::command]
pub fn session_documents(
    state: State<'_, AppState>,
    session_id: String,
) -> Result<Vec<crate::tools::attachments::SessionDocument>> {
    let core = state.core()?;
    crate::tools::attachments::list(&core.db, &session_id)
}

#[tauri::command]
pub fn detach_document(
    state: State<'_, AppState>,
    session_id: String,
    document_id: String,
) -> Result<()> {
    let core = state.core()?;
    crate::tools::attachments::detach(&core.db, &session_id, &document_id)
}

/// Le compte d'envoi choisi d'un clic sur la proposition affichée dans le fil.
/// Aucun appel au modèle : le choix est un fait, pas une phrase à interpréter.
#[tauri::command]
pub fn choose_mail_account(
    state: State<'_, AppState>,
    session_id: String,
    via: String,
) -> Result<crate::router::Answer> {
    let core = state.core()?;
    let settings = crate::settings::load(&core.db)?;
    crate::router::mail_flow::choose_account(&core, &session_id, &via, &settings)
}

#[tauri::command]
pub fn list_sessions(state: State<'_, AppState>) -> Result<Vec<Value>> {
    let core = state.core()?;
    core.db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT s.id, s.title, s.created_at, s.updated_at, s.project_id, p.name
             FROM sessions s LEFT JOIN projects p ON p.id = s.project_id
             ORDER BY s.updated_at DESC LIMIT 100",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?,
                "title": r.get::<_, Option<String>>(1)?,
                "created_at": r.get::<_, i64>(2)?,
                "updated_at": r.get::<_, i64>(3)?,
                "project_id": r.get::<_, Option<String>>(4)?,
                "project_name": r.get::<_, Option<String>>(5)?,
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
pub fn rename_session(state: State<'_, AppState>, session_id: String, title: String) -> Result<()> {
    let core = state.core()?;
    let title = title.trim();
    if title.is_empty() || title.chars().count() > 120 {
        return Err(AppError::Invalid(
            "Le titre doit contenir entre 1 et 120 caractères.".into(),
        ));
    }
    core.db.with(|c| {
        let changed = c.execute(
            "UPDATE sessions SET title=?2, updated_at=?3 WHERE id=?1",
            params![session_id, title, crate::db::now()],
        )?;
        if changed == 0 {
            return Err(AppError::Invalid("Conversation introuvable.".into()));
        }
        Ok(())
    })
}

#[tauri::command]
pub fn delete_session(state: State<'_, AppState>, session_id: String) -> Result<()> {
    let core = state.core()?;
    core.db.with(|c| {
        c.execute(
            "DELETE FROM conversations WHERE session_id=?1",
            [&session_id],
        )?;
        c.execute(
            "UPDATE actions_log SET status='rejected', result='Conversation supprimée avant validation'
             WHERE session_id=?1 AND status='awaiting_confirmation'",
            [&session_id],
        )?;
        c.execute(
            "UPDATE actions_log SET session_id=NULL WHERE session_id=?1",
            [&session_id],
        )?;
        c.execute("DELETE FROM sessions WHERE id=?1", [&session_id])?;
        Ok(())
    })
}

#[tauri::command]
pub fn list_projects(state: State<'_, AppState>) -> Result<Vec<Value>> {
    let core = state.core()?;
    core.db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT p.id, p.name, p.created_at, p.updated_at, COUNT(s.id)
             FROM projects p LEFT JOIN sessions s ON s.project_id=p.id
             GROUP BY p.id ORDER BY p.updated_at DESC, p.name COLLATE NOCASE",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(json!({
                "id": r.get::<_, String>(0)?, "name": r.get::<_, String>(1)?,
                "created_at": r.get::<_, i64>(2)?, "updated_at": r.get::<_, i64>(3)?,
                "conversation_count": r.get::<_, i64>(4)?,
            }))
        })?;
        let mut out = vec![];
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })
}

#[tauri::command]
pub fn create_project(state: State<'_, AppState>, name: String) -> Result<Value> {
    let core = state.core()?;
    let name = name.trim();
    if name.is_empty() || name.chars().count() > 80 {
        return Err(AppError::Invalid(
            "Le nom du projet doit contenir entre 1 et 80 caractères.".into(),
        ));
    }
    let id = new_id();
    let now = crate::db::now();
    core.db.with(|c| {
        let duplicate = c
            .query_row("SELECT 1 FROM projects WHERE name=?1 COLLATE NOCASE", [name], |_| Ok(()))
            .is_ok();
        if duplicate {
            return Err(AppError::Invalid("Un projet porte déjà ce nom.".into()));
        }
        c.execute(
            "INSERT INTO projects (id, name, created_at, updated_at) VALUES (?1,?2,?3,?3)",
            params![id, name, now],
        )?;
        Ok(json!({"id": id, "name": name, "created_at": now, "updated_at": now, "conversation_count": 0}))
    })
}

#[tauri::command]
pub fn move_session_to_project(
    state: State<'_, AppState>,
    session_id: String,
    project_id: Option<String>,
) -> Result<()> {
    let core = state.core()?;
    core.db.with(|c| {
        if let Some(id) = &project_id {
            let exists = c
                .query_row("SELECT 1 FROM projects WHERE id=?1", [id], |_| Ok(()))
                .is_ok();
            if !exists {
                return Err(AppError::Invalid("Projet introuvable.".into()));
            }
        }
        let changed = c.execute(
            "UPDATE sessions SET project_id=?2, updated_at=?3 WHERE id=?1",
            params![session_id, project_id, crate::db::now()],
        )?;
        if changed == 0 {
            return Err(AppError::Invalid("Conversation introuvable.".into()));
        }
        if let Some(id) = project_id {
            c.execute(
                "UPDATE projects SET updated_at=?2 WHERE id=?1",
                params![id, crate::db::now()],
            )?;
        }
        Ok(())
    })
}

#[tauri::command]
pub fn get_conversation(state: State<'_, AppState>, session_id: String) -> Result<Vec<Value>> {
    let core = state.core()?;
    core.db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT role, content, created_at FROM conversations
             WHERE session_id = ?1 AND role IN ('user','assistant','note') ORDER BY turn",
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
        settings: settings.clone(),
    };
    // Accusé discret avant l'exécution : dans le fil, l'utilisateur voit que
    // c'est bien LUI qui a déclenché l'envoi.
    if action.tool == "mail.send" {
        if let Some(sid) = &action.session_id {
            crate::router::mail_flow::note(
                &core,
                sid,
                settings
                    .voice
                    .pick("Tu as confirmé l'envoi", "Vous avez confirmé l'envoi"),
            )?;
        }
    }
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
            if action.tool == "mail.send" {
                if let Some(sid) = &action.session_id {
                    crate::connectors::mail::clear_composition(&core.db, sid)?;
                }
            }
            if let Some(sid) = &action.session_id {
                let human_result = crate::tools::outcome_summary(
                    &action.tool,
                    &outcome.result,
                    settings.voice.vouvoie(),
                );
                crate::memory::persist_turn(&core.db, sid, "assistant", &human_result)?;
                core.bus.emit(BusEvent::AgentProgress {
                    session_id: sid.clone(),
                    stage: "complete".into(),
                    title: "Action terminée et vérifiée".into(),
                    detail: Some(human_result.clone()),
                    current: 5,
                    total: 5,
                    status: "done".into(),
                });
            }
            // Émis après persistance : l'interface peut recharger le fil sans
            // course et afficher immédiatement le compte rendu de l'action.
            core.bus.emit(BusEvent::ActionResolved {
                action_id,
                status: "executed".into(),
            });
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
    let action = actions::get_action(&core.db, &action_id)?;
    actions::set_action_result(&core.db, &action_id, "rejected", None, None)?;
    if action.tool == "mail.cleanup.apply" {
        if let Some(plan_id) = action.input["plan_id"].as_str() {
            core.db.with(|connection| {
                connection.execute(
                    "UPDATE mail_cleanup_plans SET status='rejected' WHERE id=?1 AND status='pending'",
                    [plan_id],
                )?;
                Ok(())
            })?;
        }
    }
    // Un envoi refusé est un envoi abandonné : sans cet effacement, le parcours
    // reproposait la même carte au tour suivant.
    if action.tool == "mail.send" {
        if let Some(sid) = &action.session_id {
            crate::connectors::mail::clear_composition(&core.db, sid)?;
            let settings = crate::settings::load(&core.db)?;
            crate::router::mail_flow::note(
                &core,
                sid,
                settings
                    .voice
                    .pick("Tu as refusé l'envoi", "Vous avez refusé l'envoi"),
            )?;
        }
    }
    core.bus.emit(BusEvent::ActionResolved {
        action_id,
        status: "rejected".into(),
    });
    Ok(())
}

#[tauri::command]
pub async fn undo_action(state: State<'_, AppState>, action_id: String) -> Result<String> {
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
    let report = if undo["kind"].as_str() == Some("mail_cleanup") {
        let report = crate::connectors::external::undo_mail_cleanup(&undo).await?;
        crate::tools::mail_cleanup::mark_local_after_undo(&core.db, &undo)?;
        report
    } else {
        actions::apply_undo(&core.db, &undo)?
    };
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
        let full_files = files::full_disk_access_granted();
        return Ok(json!({
            "platform": "macos",
            "provider": "Apple",
            "services": [
                {"id":"files", "label":"Fichiers de ce Mac", "status": if full_files {"granted"} else {"needs_permission"}, "detail": if full_files {"Accès aux fichiers personnels autorisé ; éléments système et techniques exclus".to_string()} else {format!("Accès complet au disque non accordé ({} ancien(s) périmètre(s) enregistré(s))", folders.len())}, "settings":"all_files", "operational":true},
                {"id":"mail", "label":"Apple Mail", "status": if mail::native_available() {"granted"} else {"needs_permission"}, "detail":"Lecture locale des messages synchronisés", "settings":"all_files", "operational":true},
                {"id":"contacts", "label":"Contacts", "status": connectors::native::permission_status("contacts"), "detail":"API Contacts native", "settings":"contacts", "operational":true},
                {"id":"calendar", "label":"Calendrier", "status": connectors::native::permission_status("calendar"), "detail":"EventKit natif en lecture et écriture", "settings":"calendars", "operational":true},
                {"id":"reminders", "label":"Rappels", "status": connectors::native::permission_status("reminders"), "detail":"Synchronisé avec les tâches de Syn (les rappels ouverts apparaissent dans les briefs)", "settings":"reminders", "operational":true},
                {"id":"photos", "label":"Photos", "status": connectors::native::permission_status("photos"), "detail":"Autorisation prête ; recherche PhotoKit à finaliser", "settings":"photos", "operational":false},
                {"id":"screen", "label":"Contexte d’écran", "status": connectors::native::permission_status("screen"), "detail":"Capture ponctuelle locale, OCR et disposition visuelle ; l’image est supprimée aussitôt", "settings":"screen_recording", "operational":true}
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
            "screen_recording" => "Privacy_ScreenCapture",
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
            if connectors::native::permission_status("screen") == "granted" {
                connectors::set_status(&core.db, "screen", "screen", "connected")?;
                Ok(json!({"status": "connected"}))
            } else {
                connectors::set_status(&core.db, "screen", "screen", "needs_permission")?;
                Ok(
                    json!({"status": "needs_permission", "message": "Autorise Enregistrement de l’écran dans les réglages macOS."}),
                )
            }
        }
        "google" | "microsoft" | "slack" | "github" => {
            let embed = crate::settings::load(&core.db)?.embed_model;
            connectors::oauth::start(&core.db, &core.llm, &core.bus, &embed, &id).await
        }
        "files" | "system" => {
            connectors::set_status(&core.db, &id, &id, "connected")?;
            Ok(json!({"status": "connected"}))
        }
        _ => Err(AppError::NotFound(format!("connecteur inconnu : {id}"))),
    }
}

#[tauri::command]
pub async fn connector_sync(state: State<'_, AppState>, id: String) -> Result<Value> {
    if !matches!(id.as_str(), "google" | "microsoft") {
        return Err(AppError::Invalid(
            "Ce connecteur ne propose pas cette synchronisation.".into(),
        ));
    }
    let core = state.core()?;
    connectors::set_status(&core.db, &id, &id, "syncing")?;
    connectors::set_diagnostic(&core.db, &id, None, None)?;
    let embed = crate::settings::load(&core.db)?.embed_model;
    match connectors::external::sync(&id, &core.db, &core.llm, &core.bus, &embed).await {
        Ok(result) => Ok(result),
        Err(error) => {
            let error_text = error.to_string();
            let status = if error_text.contains("reconnect")
                || error_text.contains("Réautorisation")
                || error_text.contains("401")
                || error_text.contains("403")
                || error_text.contains("insufficient")
            {
                "needs_reauth"
            } else {
                "authorized_only"
            };
            let _ = connectors::set_status(&core.db, &id, &id, status);
            let _ = connectors::set_diagnostic(&core.db, &id, Some(&error_text), None);
            core.bus.emit(BusEvent::SyncProgress {
                connector: id.clone(),
                pct: 100.0,
                message: Some(format!("Échec de la synchronisation : {error}")),
            });
            Err(error)
        }
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
    if matches!(id.as_str(), "google" | "microsoft" | "slack" | "github") {
        connectors::oauth::revoke_local(&id);
    }
    connectors::set_status(&core.db, &id, &id, "disconnected")?;
    if id == "google" || id == "microsoft" {
        let prefix = format!("{id}:%");
        core.db.with(|c| {
            c.execute(
                "UPDATE items SET status='removed' WHERE source_ref LIKE ?1",
                [&prefix],
            )?;
            c.execute("DELETE FROM events WHERE source=?1", [&id])?;
            Ok(())
        })?;
    } else if id == "apple" {
        core.db.with(|c| {
            c.execute("UPDATE items SET status='removed' WHERE source='mail'", [])?;
            Ok(())
        })?;
    }
    crate::security::log_access(&core.db, &id, "disconnect", None);
    Ok(())
}

#[tauri::command]
pub async fn screen_context(state: State<'_, AppState>) -> Result<Value> {
    let core = state.core()?;
    crate::security::log_access(&core.db, "screen", "capture_ocr", None);
    tokio::task::spawn_blocking(screen::capture_context)
        .await
        .map_err(|e| AppError::Other(format!("Capture interrompue : {e}")))?
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
        let sensitive_ids: Vec<String> = core.db.read(|c| {
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
        let paths = core.db.read(|c| {
            let mut statement = c.prepare(
                "SELECT source_ref FROM items WHERE source='files'
                 AND type='sensible_non_lu' AND status='active'",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            let mut paths = Vec::new();
            for row in rows {
                paths.push(std::path::PathBuf::from(row?));
            }
            Ok(paths)
        })?;
        let _ = core.indexer.tx.send(files::IndexJob::Demand(paths));
    }
    core.indexer.paused.store(
        new_settings.indexing_paused,
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
pub fn files_request_full_access(state: State<'_, AppState>) -> Result<Value> {
    let core = state.core()?;
    crate::settings::set_key(&core.db, "files_full_access_requested", &Value::Bool(true))?;
    if files::full_disk_access_granted() {
        crate::settings::set_key(&core.db, "sensitive_consent", &Value::Bool(true))?;
        let (root, started) = files::ensure_full_access_scope(&core.db)?;
        if started {
            core.indexer.watch_folder(std::path::Path::new(&root));
            let _ = core
                .indexer
                .tx
                .send(files::IndexJob::FullScan(Some(root.clone().into())));
        }
        return Ok(
            json!({"status":"granted", "message":"Accès vérifié. L’indexation automatique a commencé.", "root":root}),
        );
    }
    #[cfg(target_os = "macos")]
    std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        .spawn()
        .map_err(|e| AppError::Other(format!("Impossible d’ouvrir les réglages : {e}")))?;
    Ok(json!({
        "status":"needs_permission",
        "message":"Ajoute ou active Syn dans Accès complet au disque. macOS peut demander de relancer l’application."
    }))
}

#[tauri::command]
pub fn files_activate_full_access(state: State<'_, AppState>) -> Result<Value> {
    let core = state.core()?;
    if !files::full_disk_access_granted() {
        return Ok(json!({"status":"needs_permission"}));
    }
    crate::settings::set_key(&core.db, "files_full_access_requested", &Value::Bool(true))?;
    let sensitive_was_disabled = !crate::settings::load(&core.db)?.sensitive_consent;
    crate::settings::set_key(&core.db, "sensitive_consent", &Value::Bool(true))?;
    let (root, started) = files::ensure_full_access_scope(&core.db)?;
    if started {
        core.indexer.watch_folder(std::path::Path::new(&root));
        let _ = core
            .indexer
            .tx
            .send(files::IndexJob::FullScan(Some(root.clone().into())));
    } else if sensitive_was_disabled {
        let paths = core.db.read(|c| {
            let mut statement = c.prepare(
                "SELECT source_ref FROM items WHERE source='files'
                 AND type='sensible_non_lu' AND status='active'",
            )?;
            let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
            let mut paths = Vec::new();
            for row in rows {
                paths.push(std::path::PathBuf::from(row?));
            }
            Ok(paths)
        })?;
        let _ = core.indexer.tx.send(files::IndexJob::Demand(paths));
    }
    Ok(json!({"status":"granted", "root":root, "started":started || sensitive_was_disabled}))
}

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
    crate::retrieval::search_lexical_source(&core.db, &query_text, 20, "files").await
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
    core.db.read(|c| {
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

// ————————————————— La toile, la ligne de temps, les habitudes —————————————————

/// Ce que Syn a tissé : nombre de liens, principaux correspondants, et les
/// adresses qu'il croit être celles de l'utilisateur (à confirmer).
///
/// Tout est montrable : une mémoire qu'on ne peut pas inspecter est une mémoire
/// qu'on ne peut pas corriger.
#[tauri::command]
pub fn memory_graph(state: State<'_, AppState>) -> Result<Value> {
    let core = state.core()?;
    Ok(json!({
        "stats": crate::memory::graph::stats(&core.db)?,
        "correspondants": crate::memory::graph::top_correspondents(&core.db, 12)?,
        "identites": crate::memory::graph::list_identity_candidates(&core.db)?,
        "identites_retenues": crate::memory::graph::self_addresses(&core.db),
    }))
}

#[tauri::command]
pub fn memory_relations(state: State<'_, AppState>, nom: String) -> Result<Value> {
    let core = state.core()?;
    crate::memory::graph::lookup(&core.db, &nom)
}

#[tauri::command]
pub fn memory_timeline(
    state: State<'_, AppState>,
    jours: Option<i64>,
    sujet: Option<String>,
    limite: Option<usize>,
) -> Result<Value> {
    let core = state.core()?;
    let mut window =
        crate::memory::timeline::Window::last_days(jours.unwrap_or(14).clamp(1, 400), limite.unwrap_or(60));
    window.about = sujet.map(|s| s.trim().to_string()).filter(|s| !s.is_empty());
    crate::memory::timeline::grouped(&core.db, &window)
}

/// L'utilisateur confirme (ou corrige) une adresse qui est la sienne. Sans
/// cela, Syn ne peut pas distinguer un message reçu d'un message envoyé.
#[tauri::command]
pub fn memory_set_identity(state: State<'_, AppState>, address: String, mine: bool) -> Result<()> {
    let core = state.core()?;
    crate::memory::graph::set_self_address(&core.db, &address, mine)
}

/// Les habitudes observées : celles qui attendent un avis, et celles déjà
/// confirmées. Chacune porte ce dont Syn l'a déduite.
#[tauri::command]
pub fn habits_list(state: State<'_, AppState>) -> Result<Vec<Value>> {
    let core = state.core()?;
    let mut list = crate::memory::habits::list_all(&core.db)?;
    for habit in list.iter_mut() {
        let phrase = crate::memory::habits::describe(habit);
        if let Some(object) = habit.as_object_mut() {
            object.insert("phrase".into(), json!(phrase));
        }
    }
    Ok(list)
}

/// L'utilisateur tranche : une habitude confirmée entre dans le comportement de
/// Syn, une habitude rejetée n'y revient jamais par la porte des observations.
#[tauri::command]
pub fn habits_decide(state: State<'_, AppState>, id: String, accepte: bool) -> Result<()> {
    let core = state.core()?;
    crate::memory::habits::decide(&core.db, &id, accepte)
}

/// Reconstruit la toile de zéro. Elle est dérivée des sources déjà indexées :
/// la jeter ne perd rien, et c'est le bon geste après une correction d'identité.
#[tauri::command]
pub async fn memory_rebuild(state: State<'_, AppState>) -> Result<Value> {
    let core = state.core()?;
    let db = core.db.clone();
    let resultat = tauri::async_runtime::spawn_blocking(move || {
        crate::memory::graph::rebuild(&db)?;
        let liens = crate::memory::graph::build(&db, 5_000)?;
        let habitudes = crate::memory::habits::learn(&db, 2_000)?;
        crate::error::Result::Ok(json!({"elements_relus": liens, "habitudes": habitudes}))
    })
    .await
    .map_err(|e| crate::error::AppError::Other(format!("reconstruction interrompue : {e}")))??;
    Ok(resultat)
}

/// Vue compacte des fichiers connus : l'interface n'a pas à afficher des
/// milliers de lignes pour expliquer ce que Syn sait rechercher.
#[tauri::command]
pub fn knowledge_file_groups(state: State<'_, AppState>) -> Result<Vec<Value>> {
    use std::collections::{BTreeMap, HashMap};

    let core = state.core()?;
    let home = dirs::home_dir();
    core.db.read(|c| {
        let mut stmt = c.prepare(
            "SELECT type, title, path, ingested_at FROM items
             WHERE source='files' AND status='active' ORDER BY ingested_at DESC",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        #[derive(Default)]
        struct Group {
            count: i64,
            latest: i64,
            types: HashMap<String, i64>,
            examples: Vec<Value>,
        }
        let mut groups: BTreeMap<String, Group> = BTreeMap::new();
        for row in rows {
            let (kind, title, path, ingested_at) = row?;
            let category = if kind == "code_project" {
                "Projets".to_string()
            } else {
                path.as_deref()
                    .and_then(|raw| home.as_ref().and_then(|root| std::path::Path::new(raw).strip_prefix(root).ok()))
                    .and_then(|relative| relative.components().next())
                    .map(|component| match component.as_os_str().to_string_lossy().as_ref() {
                        "Desktop" => "Bureau".to_string(),
                        "Documents" => "Documents".to_string(),
                        "Downloads" => "Téléchargements".to_string(),
                        "Pictures" => "Images".to_string(),
                        "Movies" => "Vidéos".to_string(),
                        "Music" => "Musique".to_string(),
                        name => name.to_string(),
                    })
                    .unwrap_or_else(|| "Autres fichiers".to_string())
            };
            let group = groups.entry(category).or_default();
            group.count += 1;
            group.latest = group.latest.max(ingested_at);
            *group.types.entry(kind.clone()).or_insert(0) += 1;
            if group.examples.len() < 5 {
                group.examples.push(json!({
                    "title": title.or_else(|| path.as_ref().and_then(|value| std::path::Path::new(value).file_name().map(|name| name.to_string_lossy().to_string()))),
                    "path": path,
                    "type": kind
                }));
            }
        }
        let mut result: Vec<Value> = groups
            .into_iter()
            .map(|(name, group)| json!({
                "name": name,
                "count": group.count,
                "latest": group.latest,
                "types": group.types,
                "examples": group.examples
            }))
            .collect();
        result.sort_by(|a, b| b["count"].as_i64().cmp(&a["count"].as_i64()));
        Ok(result)
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
    core.db.read(|c| {
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
    core.db.read(|c| {
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
    core.db.read(|c| {
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
    let (items, embeddings): (i64, i64) = core.db.read(|c| {
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
    let core = state.core()?;
    // Garde de périmètre (audit §2) : on n'ouvre que ce que Syn connaît —
    // un chemin indexé ou couvert par un dossier suivi. source_ref provient
    // de l'UI, donc indirectement du contenu indexé.
    let known: bool = core.db.read(|c| {
        Ok(c.query_row(
            "SELECT 1 FROM items WHERE source_ref=?1 OR path=?1 LIMIT 1",
            rusqlite::params![source_ref],
            |_| Ok(true),
        )
        .unwrap_or(false))
    })?;

    use tauri_plugin_opener::OpenerExt;
    // Un résultat Google Drive ou OneDrive s'ouvre dans le navigateur : son
    // « chemin » est une URL, jamais un fichier local. Sans ce cas, cliquer le
    // nom d'un document cloud échouait en silence.
    if source_ref.starts_with("https://") || source_ref.starts_with("http://") {
        if !known {
            return Err(AppError::Security(
                "ce lien ne provient pas d'un document connu de Syn".into(),
            ));
        }
        return app
            .opener()
            .open_url(source_ref, None::<String>)
            .map_err(|e| AppError::Other(e.to_string()));
    }

    let path = std::path::Path::new(&source_ref);
    if !path.exists() {
        return Err(AppError::NotFound(
            "cette source n'est pas un fichier ouvrable".into(),
        ));
    }
    if !known && !files::is_path_in_active_scope(&core.db, path)? {
        return Err(AppError::Security(
            "ce chemin est hors du périmètre suivi par Syn".into(),
        ));
    }
    app.opener()
        .open_path(source_ref, None::<String>)
        .map_err(|e| AppError::Other(e.to_string()))?;
    Ok(())
}

// ————————————————— Dictée et commande vocale —————————————————

/// État de la dictée : autorisation et écoute en cours. L'interface s'appuie
/// dessus pour savoir si le micro peut être proposé.
#[tauri::command]
pub fn dictation_status() -> Value {
    json!({
        "authorization": connectors::native::speech::authorization(),
        "listening": connectors::native::speech::running(),
        "supported": cfg!(target_os = "macos"),
    })
}

#[tauri::command]
pub fn dictation_request_permission() -> Value {
    connectors::native::speech::request_authorization();
    json!({"authorization": connectors::native::speech::authorization()})
}

/// Démarre l'écoute. La transcription se lit ensuite par `dictation_transcript`.
#[tauri::command]
pub fn dictation_start(state: State<'_, AppState>, locale: Option<String>) -> Result<()> {
    let core = state.core()?;
    let settings = crate::settings::load(&core.db)?;
    if !settings.voice_input_enabled {
        return Err(AppError::Invalid(
            "La dictée est désactivée dans Réglages ▸ Général.".into(),
        ));
    }
    // La voix est une donnée personnelle : son usage se journalise comme les
    // autres accès, même si rien ne sort de la machine.
    crate::security::log_access(&core.db, "micro", "dictation_start", None);
    connectors::native::speech::start(locale.as_deref().unwrap_or("fr-FR"))
}

#[tauri::command]
pub fn dictation_transcript() -> Value {
    json!({
        "text": connectors::native::speech::transcript(),
        "listening": connectors::native::speech::running(),
    })
}

#[tauri::command]
pub fn dictation_stop() -> String {
    connectors::native::speech::stop()
}

#[tauri::command]
pub fn speak_text(state: State<'_, AppState>, text: String) -> Result<()> {
    let core = state.core()?;
    let settings = crate::settings::load(&core.db)?;
    if !settings.voice_output_enabled {
        return Err(AppError::Invalid(
            "La lecture à voix haute est désactivée dans Réglages ▸ Général.".into(),
        ));
    }
    if !cfg!(target_os = "macos") {
        return Err(AppError::Invalid(
            "Lecture vocale indisponible sur cet OS.".into(),
        ));
    }
    // Une seule lecture à la fois : on coupe la précédente.
    let _ = std::process::Command::new("/usr/bin/pkill")
        .args(["-x", "say"])
        .status();
    let capped: String = text.chars().take(4000).collect();
    std::process::Command::new("/usr/bin/say")
        .arg(capped)
        .spawn()
        .map_err(|e| AppError::Other(format!("synthèse vocale : {e}")))?;
    Ok(())
}

#[tauri::command]
pub fn stop_speaking() -> Result<()> {
    let _ = std::process::Command::new("/usr/bin/pkill")
        .args(["-x", "say"])
        .status();
    Ok(())
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
