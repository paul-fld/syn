//! Cycle de vie du daemon (doc maître §6) : tray, fermeture → arrière-plan,
//! raccourci global de la barre d'interaction, détection de réveil, boucle de
//! proactivité cadencée.

use crate::bus::BusEvent;
use crate::state::AppState;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::TrayIconBuilder;
use tauri::{AppHandle, Manager};

pub fn setup_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Ouvrir Syn", true, None::<&str>)?;
    let bar = MenuItem::with_id(app, "bar", "Barre d'interaction", true, Some("Alt+Space"))?;
    let quit = MenuItem::with_id(app, "quit", "Quitter Syn", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &bar, &quit])?;

    TrayIconBuilder::with_id("syn-tray")
        .icon(app.default_window_icon().unwrap().clone())
        .menu(&menu)
        .show_menu_on_left_click(true)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => show_main(app),
            "bar" => toggle_bar(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .build(app)?;
    Ok(())
}

pub fn show_main(app: &AppHandle) {
    if let Some(win) = app.get_webview_window("main") {
        let _ = win.show();
        let _ = win.unminimize();
        let _ = win.set_focus();
    }
}

/// Barre d'interaction : pilule flottante en bas à droite (maquette dédiée).
pub fn toggle_bar(app: &AppHandle) {
    let Some(bar) = app.get_webview_window("bar") else {
        return;
    };
    if bar.is_visible().unwrap_or(false) {
        let _ = bar.hide();
        return;
    }
    if let Some(monitor) = bar.current_monitor().ok().flatten() {
        let size = monitor.size();
        let scale = monitor.scale_factor();
        let (w, h) = (560.0 * scale, 64.0 * scale);
        let x = size.width as f64 - w - 24.0 * scale;
        let y = size.height as f64 - h - 90.0 * scale; // au-dessus du Dock
        let _ = bar.set_position(tauri::PhysicalPosition::new(x as i32, y as i32));
    }
    let _ = bar.show();
    let _ = bar.set_focus();
    let _ = app.emit_to(tauri::EventTarget::webview_window("bar"), "bar_shown", ());
}

use tauri::Emitter;

/// Raccourci global (🔎 tranché : Option+Espace, sans conflit Spotlight).
pub fn setup_shortcut(app: &AppHandle) {
    use tauri_plugin_global_shortcut::{
        Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState,
    };
    let shortcut = Shortcut::new(Some(Modifiers::ALT), Code::Space);
    let handle = app.clone();
    let _ = app
        .global_shortcut()
        .on_shortcut(shortcut, move |_app, _sc, event| {
            if event.state() == ShortcutState::Pressed {
                toggle_bar(&handle);
            }
        });
}

/// Boucles de fond : proactivité (60 s), rattrapage d'embeddings, détection de
/// réveil (dérive horloge murale vs monotone → hook wake-from-sleep).
pub fn spawn_background_loops(app: &AppHandle) {
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        let mut last_instant = std::time::Instant::now();
        let mut last_wall = chrono::Utc::now().timestamp();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            let state = handle.state::<AppState>();

            // Détection de réveil : l'horloge murale a sauté plus que le monotone.
            let wall_delta = chrono::Utc::now().timestamp() - last_wall;
            let mono_delta = last_instant.elapsed().as_secs() as i64;
            if wall_delta - mono_delta > 120 {
                state.bus.emit(BusEvent::WakeFromSleep);
            }
            last_instant = std::time::Instant::now();
            last_wall = chrono::Utc::now().timestamp();

            let Ok(core) = state.core() else { continue };
            // L'interactif prime : la passe de proactivité est légère et espacée.
            if let Err(e) = crate::proactivity::evaluate_tick(&core.db, &core.bus).await {
                eprintln!("proactivité : {e}");
            }
            // Mode dégradé → nominal : rattraper les embeddings en attente.
            let _ = crate::ingestion::backfill_embeddings(&core.db, &core.llm, 64).await;
        }
    });
}

/// Relaye le bus interne vers le frontend (événements IPC, doc maître §28).
pub fn forward_bus(app: &AppHandle) {
    let state = app.state::<AppState>();
    let mut rx = state.bus.subscribe();
    let handle = app.clone();
    tauri::async_runtime::spawn(async move {
        while let Ok(ev) = rx.recv().await {
            let name = match &ev {
                BusEvent::ItemIngested { .. } => "item_ingested",
                BusEvent::IngestionStatus { .. } => "ingestion_status",
                BusEvent::FilesError { .. } => "files_error",
                BusEvent::SyncProgress { .. } => "sync_progress",
                BusEvent::BriefReady => "brief_ready",
                BusEvent::ProactiveAlert { .. } => "proactive_alert",
                BusEvent::ActionAwaitingConfirmation { .. } => "action_awaiting_confirmation",
                BusEvent::ActionResolved { .. } => "action_resolved",
                BusEvent::SystemAlert { .. } => "system_alert",
                BusEvent::ModelPullProgress { .. } => "model_pull_progress",
                BusEvent::AgentProgress { .. } => "agent_progress",
                BusEvent::VoiceProfileChanged => "voice_profile_changed",
                BusEvent::WakeFromSleep => "wake",
            };
            let _ = handle.emit(name, &ev);

            // Surface non-intrusive : notification OS pour l'urgent/important.
            if let BusEvent::ProactiveAlert {
                reason,
                body,
                priority,
                ..
            } = &ev
            {
                if priority != "info" {
                    use tauri_plugin_notification::NotificationExt;
                    let _ = handle
                        .notification()
                        .builder()
                        .title(format!("Syn — {reason}"))
                        .body(body.clone())
                        .show();
                }
            }
        }
    });
}
