//! Capture ponctuelle du contenu visible. L'image reste locale et est supprimée
//! immédiatement après l'OCR. Le texte observé est toujours traité comme non fiable.

use crate::error::{AppError, Result};
use serde_json::{json, Value};
use std::process::Command;

pub fn capture_context() -> Result<Value> {
    if !cfg!(target_os = "macos") {
        return Err(AppError::Invalid(
            "Contexte d'écran non pris en charge sur cet OS.".into(),
        ));
    }
    if crate::connectors::native::permission_status("screen") != "granted" {
        return Err(AppError::Security(
            "Autorise l’enregistrement de l’écran pour Syn dans Réglages système → Confidentialité et sécurité → Enregistrement de l’écran.".into(),
        ));
    }

    // La fenêtre Syn est masquée par le client avant cet appel : l'app et la fenêtre
    // détectées sont donc celles que l'utilisateur veut montrer.
    std::thread::sleep(std::time::Duration::from_millis(180));
    let front = crate::connectors::native::frontmost_context();
    let path = std::env::temp_dir().join(format!("syn-screen-{}.png", uuid::Uuid::new_v4()));
    let output = Command::new("/usr/sbin/screencapture")
        .arg("-x")
        .arg("-m")
        .arg(&path)
        .output()
        .map_err(|e| AppError::Other(format!("Capture de l’écran impossible : {e}")))?;
    if !output.status.success() || !path.exists() {
        return Err(AppError::Security(
            "macOS n’a pas permis la capture. Vérifie l’autorisation Enregistrement de l’écran puis relance Syn.".into(),
        ));
    }

    let observations = crate::connectors::native::ocr_image(&path);
    let _ = std::fs::remove_file(&path);
    let observations = observations?;
    let visible_text = format_observations(&observations);
    Ok(json!({
        "available": true,
        "app": front["app"],
        "window": front["window"],
        "captured_at": chrono::Utc::now().timestamp(),
        "source": "capture_locale_ocr",
        "text": visible_text,
        "observations": observations,
    }))
}

fn format_observations(items: &[Value]) -> String {
    let mut lines: Vec<(f64, f64, String)> = items
        .iter()
        .filter_map(|item| {
            let text = item["text"].as_str()?.trim();
            if text.is_empty() {
                return None;
            }
            Some((
                item["y"].as_f64().unwrap_or(0.0),
                item["x"].as_f64().unwrap_or(0.0),
                text.to_string(),
            ))
        })
        .collect();
    lines.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.total_cmp(&b.1)));
    let mut out = String::new();
    for (y, x, text) in lines {
        let vertical = if y > 0.66 {
            "haut"
        } else if y < 0.33 {
            "bas"
        } else {
            "milieu"
        };
        let horizontal = if x < 0.33 {
            "gauche"
        } else if x > 0.66 {
            "droite"
        } else {
            "centre"
        };
        let line = format!("[{vertical} {horizontal}] {text}\n");
        if out.len() + line.len() > 16_000 {
            break;
        }
        out.push_str(&line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ordonne_et_localise_le_texte_visible() {
        let input = vec![
            json!({"text":"Bas", "x":0.8, "y":0.1}),
            json!({"text":"Haut", "x":0.1, "y":0.9}),
        ];
        assert_eq!(
            format_observations(&input),
            "[haut gauche] Haut\n[bas droite] Bas\n"
        );
    }
}
