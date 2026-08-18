//! Syn — assistant de vie numérique local-first.
//! Backend Rust (daemon + logique + données) ; frontend SolidJS (vue).
//! Invariants (doc maître §1) : local par défaut, escalade cloud opt-in OFF,
//! desktop-only, l'utilisateur possède ses données, zéro télémétrie,
//! plancher humain, proactivité rare et explicable.

pub mod actions;
pub mod bus;
pub mod connectors;
pub mod db;
pub mod error;
pub mod ingestion;
pub mod ipc;
pub mod lifecycle;
pub mod llm;
pub mod memory;
pub mod proactivity;
pub mod retrieval;
pub mod router;
pub mod rules;
pub mod security;
pub mod settings;
pub mod state;
pub mod tools;

use state::AppState;
use tauri::Manager;

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_notification::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .setup(|app| {
            let data_dir = app.path().app_data_dir().expect("dossier de données");
            app.manage(AppState::new(data_dir));

            let handle = app.handle().clone();
            lifecycle::setup_tray(&handle)?;
            lifecycle::setup_shortcut(&handle);
            lifecycle::forward_bus(&handle);
            lifecycle::spawn_background_loops(&handle);
            Ok(())
        })
        .on_window_event(|window, event| {
            // Fermer la fenêtre principale ne quitte pas : le daemon vit dans le tray.
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    api.prevent_close();
                    let _ = window.hide();
                }
            }
            if window.label() == "bar" {
                if let tauri::WindowEvent::Focused(false) = event {
                    let _ = window.hide();
                }
            }
        })
        .invoke_handler(tauri::generate_handler![
            // session & sécurité
            ipc::app_status,
            ipc::runtime_ready,
            ipc::dictation_status,
            ipc::dictation_request_permission,
            ipc::dictation_start,
            ipc::dictation_transcript,
            ipc::dictation_stop,
            ipc::setup_master,
            ipc::unlock,
            ipc::unlock_with_keychain,
            ipc::unlock_with_recovery,
            ipc::lock,
            ipc::change_master_password,
            ipc::regenerate_recovery,
            ipc::set_keychain,
            // conversation & briefs
            ipc::query,
            ipc::list_sessions,
            ipc::get_conversation,
            ipc::rename_session,
            ipc::delete_session,
            ipc::list_projects,
            ipc::create_project,
            ipc::move_session_to_project,
            ipc::choose_mail_account,
            ipc::get_startup_brief,
            ipc::get_daily_wrap,
            // actions
            ipc::list_pending_actions,
            ipc::list_actions,
            ipc::confirm_action,
            ipc::reject_action,
            ipc::undo_action,
            // connecteurs
            ipc::connector_status,
            ipc::native_permissions,
            ipc::request_native_permission,
            ipc::open_native_settings,
            ipc::connector_connect,
            ipc::connector_sync,
            ipc::connector_disconnect,
            ipc::screen_context,
            // réglages & modèle
            ipc::get_settings,
            ipc::set_settings,
            ipc::hardware_info,
            ipc::llm_status,
            ipc::model_pull,
            // files
            ipc::files_add_folder,
            ipc::files_request_full_access,
            ipc::files_activate_full_access,
            ipc::files_remove_folder,
            ipc::files_reindex,
            ipc::files_index_status,
            ipc::files_search,
            ipc::search_memory,
            // connaissances
            ipc::knowledge_stats,
            ipc::knowledge_file_groups,
            ipc::list_knowledge,
            ipc::forget_item,
            // personnes
            ipc::get_person_context,
            ipc::people_list,
            ipc::people_os_preview,
            ipc::people_add,
            ipc::unknowns_pending,
            ipc::unknown_label,
            ipc::unknown_ignore,
            ipc::add_fact,
            // règles
            ipc::rules_add,
            ipc::rules_edit,
            ipc::rules_delete,
            ipc::rules_list,
            ipc::rules_set_priority,
            // proactivité & système
            ipc::list_surfacings,
            ipc::dismiss_surfacing,
            ipc::list_triggers,
            ipc::trigger_toggle,
            ipc::system_snapshot,
            ipc::access_log_list,
            // calendrier & tâches
            ipc::calendar_today,
            ipc::tasks_quick_add,
            // données & divers
            ipc::storage_stats,
            ipc::data_dir_path,
            ipc::purge_all_data,
            ipc::onboarding_complete,
            ipc::open_source,
            ipc::show_main_window,
            ipc::hide_bar,
            ipc::speak_text,
            ipc::stop_speaking,
        ])
        .run(tauri::generate_context!())
        .expect("erreur au lancement de Syn");
}
