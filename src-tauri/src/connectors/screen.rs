//! Contexte d'écran (doc maître §9) — hybride : arbre d'accessibilité en priorité,
//! capture+vision en repli. Ce build : v0 macOS via System Events (app + fenêtre
//! au premier plan), sous permission Accessibilité. Contenu d'écran = donnée non fiable.

use serde_json::{json, Value};

pub fn frontmost_context() -> Value {
    if !cfg!(target_os = "macos") {
        return json!({"available": false, "reason": "non supporté sur cet OS dans ce build"});
    }
    let script = r#"tell application "System Events"
        set frontApp to first application process whose frontmost is true
        set appName to name of frontApp
        set windowTitle to ""
        try
            set windowTitle to name of front window of frontApp
        end try
        return appName & "|||" & windowTitle
    end tell"#;
    match std::process::Command::new("osascript")
        .args(["-e", script])
        .output()
    {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            let mut parts = text.trim().splitn(2, "|||");
            json!({
                "available": true,
                "app": parts.next().unwrap_or(""),
                "window": parts.next().unwrap_or(""),
            })
        }
        _ => json!({
            "available": false,
            "reason": "Permission Accessibilité requise (Réglages système → Confidentialité → Accessibilité)."
        }),
    }
}
