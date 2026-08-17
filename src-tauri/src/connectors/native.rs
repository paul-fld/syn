use crate::error::{AppError, Result};
use serde_json::Value;
use std::ffi::{CStr, CString};

#[cfg(target_os = "macos")]
unsafe extern "C" {
    fn syn_native_permission_status(service: *const std::os::raw::c_char) -> i32;
    fn syn_native_request_permission(service: *const std::os::raw::c_char) -> i32;
    fn syn_native_contacts_json() -> *mut std::os::raw::c_char;
    fn syn_native_calendar_events(from: f64, to: f64) -> *mut std::os::raw::c_char;
    fn syn_native_calendar_create(
        title: *const std::os::raw::c_char,
        start: f64,
        end: f64,
        location: *const std::os::raw::c_char,
    ) -> *mut std::os::raw::c_char;
    fn syn_native_calendar_delete(identifier: *const std::os::raw::c_char) -> i32;
    fn syn_native_reminders_json() -> *mut std::os::raw::c_char;
    fn syn_native_reminder_create(
        title: *const std::os::raw::c_char,
        due: f64,
    ) -> *mut std::os::raw::c_char;
    fn syn_native_reminder_complete(identifier: *const std::os::raw::c_char) -> i32;
    fn syn_native_ocr_image_json(path: *const std::os::raw::c_char) -> *mut std::os::raw::c_char;
    fn syn_native_frontmost_context_json() -> *mut std::os::raw::c_char;
    fn syn_native_free(value: *mut std::os::raw::c_char);
    fn syn_native_idle_seconds() -> f64;
    fn syn_native_fsevents_current_id() -> u64;
    fn syn_native_fsevents_replay_json(
        root: *const std::os::raw::c_char,
        since_id: u64,
    ) -> *mut std::os::raw::c_char;
}

pub fn idle_seconds() -> f64 {
    #[cfg(target_os = "macos")]
    {
        return unsafe { syn_native_idle_seconds() };
    }
    #[allow(unreachable_code)]
    60.0
}

pub fn fsevents_current_id() -> u64 {
    #[cfg(target_os = "macos")]
    {
        return unsafe { syn_native_fsevents_current_id() };
    }
    #[allow(unreachable_code)]
    0
}

pub fn fsevents_replay(root: &str, since_id: u64) -> Result<Value> {
    #[cfg(target_os = "macos")]
    {
        let root =
            CString::new(root).map_err(|_| AppError::Invalid("racine FSEvents invalide".into()))?;
        return take_json(
            unsafe { syn_native_fsevents_replay_json(root.as_ptr(), since_id) },
            "Historique FSEvents indisponible.",
        );
    }
    #[allow(unreachable_code)]
    Ok(serde_json::json!({"valid":false,"current_id":0,"events":[]}))
}

pub fn ocr_image(path: &std::path::Path) -> Result<Vec<Value>> {
    #[cfg(target_os = "macos")]
    {
        let path = CString::new(path.to_string_lossy().as_bytes())
            .map_err(|_| AppError::Invalid("chemin de capture invalide".into()))?;
        let value = take_json(
            unsafe { syn_native_ocr_image_json(path.as_ptr()) },
            "La reconnaissance du contenu affiché a échoué.",
        )?;
        return Ok(value.as_array().cloned().unwrap_or_default());
    }
    #[allow(unreachable_code)]
    Err(AppError::Invalid("OCR natif indisponible".into()))
}

pub fn frontmost_context() -> Value {
    #[cfg(target_os = "macos")]
    {
        take_json(
            unsafe { syn_native_frontmost_context_json() },
            "Impossible d’identifier l’application visible.",
        )
        .unwrap_or_else(|_| serde_json::json!({"available": true, "app": "", "window": ""}))
    }
    #[cfg(not(target_os = "macos"))]
    {
        serde_json::json!({"available": false, "app": "", "window": ""})
    }
}

pub fn permission_status(service: &str) -> &'static str {
    #[cfg(target_os = "macos")]
    {
        let Ok(service) = CString::new(service) else {
            return "unavailable";
        };
        // Le pont ne conserve pas le pointeur et ne modifie aucune mémoire Rust.
        return status_label(unsafe { syn_native_permission_status(service.as_ptr()) });
    }
    #[allow(unreachable_code)]
    "unavailable"
}

pub fn request_permission(service: &str) -> Result<&'static str> {
    #[cfg(target_os = "macos")]
    {
        let service = CString::new(service)
            .map_err(|_| AppError::Invalid("service natif invalide".into()))?;
        // L’appel est exécuté dans spawn_blocking par l’IPC et le pont copie la chaîne.
        return Ok(status_label(unsafe {
            syn_native_request_permission(service.as_ptr())
        }));
    }
    #[allow(unreachable_code)]
    Err(AppError::Invalid(
        "autorisations natives indisponibles sur cet OS".into(),
    ))
}

fn status_label(code: i32) -> &'static str {
    match code {
        1 => "granted",
        2 => "denied",
        3 => "restricted",
        4 => "limited",
        0 => "needs_permission",
        _ => "unavailable",
    }
}

#[cfg(target_os = "macos")]
fn take_json(raw: *mut std::os::raw::c_char, unavailable: &str) -> Result<Value> {
    if raw.is_null() {
        return Err(AppError::Security(unavailable.into()));
    }
    // Le pont alloue une chaîne UTF-8 avec strdup ; elle est copiée puis libérée ici.
    let text = unsafe { CStr::from_ptr(raw) }
        .to_string_lossy()
        .into_owned();
    unsafe { syn_native_free(raw) };
    serde_json::from_str(&text)
        .map_err(|e| AppError::Other(format!("Réponse native invalide : {e}")))
}

pub fn contacts() -> Result<Vec<Value>> {
    #[cfg(target_os = "macos")]
    {
        let value = take_json(
            unsafe { syn_native_contacts_json() },
            "Autorise Contacts pour Syn dans Réglages système.",
        )?;
        return Ok(value.as_array().cloned().unwrap_or_default());
    }
    #[allow(unreachable_code)]
    Ok(vec![])
}

pub fn calendar_events(from: i64, to: i64) -> Result<Vec<Value>> {
    #[cfg(target_os = "macos")]
    {
        let value = take_json(
            unsafe { syn_native_calendar_events(from as f64, to as f64) },
            "Autorise Calendrier pour Syn dans Réglages système.",
        )?;
        return Ok(value.as_array().cloned().unwrap_or_default());
    }
    #[allow(unreachable_code)]
    Ok(vec![])
}

pub fn reminders_list() -> Result<Vec<Value>> {
    #[cfg(target_os = "macos")]
    {
        let value = take_json(
            unsafe { syn_native_reminders_json() },
            "Autorise Rappels pour Syn dans Réglages système.",
        )?;
        return Ok(value.as_array().cloned().unwrap_or_default());
    }
    #[allow(unreachable_code)]
    Ok(vec![])
}

pub fn reminder_create(title: &str, due: Option<i64>) -> Result<Value> {
    #[cfg(target_os = "macos")]
    {
        let title = CString::new(title).map_err(|_| AppError::Invalid("titre invalide".into()))?;
        return take_json(
            unsafe { syn_native_reminder_create(title.as_ptr(), due.unwrap_or(0) as f64) },
            "Impossible de créer le rappel.",
        );
    }
    #[allow(unreachable_code)]
    {
        let _ = (title, due);
        Err(AppError::Invalid("Rappels natifs indisponibles".into()))
    }
}

pub fn reminder_complete(identifier: &str) -> Result<bool> {
    #[cfg(target_os = "macos")]
    {
        let identifier = CString::new(identifier)
            .map_err(|_| AppError::Invalid("identifiant de rappel invalide".into()))?;
        return Ok(unsafe { syn_native_reminder_complete(identifier.as_ptr()) } == 1);
    }
    #[allow(unreachable_code)]
    {
        let _ = identifier;
        Ok(false)
    }
}

pub fn calendar_create(title: &str, start: i64, end: i64, location: &str) -> Result<Value> {
    #[cfg(target_os = "macos")]
    {
        let title = CString::new(title).map_err(|_| AppError::Invalid("titre invalide".into()))?;
        let location =
            CString::new(location).map_err(|_| AppError::Invalid("lieu invalide".into()))?;
        return take_json(
            unsafe {
                syn_native_calendar_create(
                    title.as_ptr(),
                    start as f64,
                    end as f64,
                    location.as_ptr(),
                )
            },
            "Impossible de créer l’événement dans Calendrier.",
        );
    }
    #[allow(unreachable_code)]
    Err(AppError::Invalid("Calendrier natif indisponible".into()))
}

pub fn calendar_delete(identifier: &str) -> Result<()> {
    #[cfg(target_os = "macos")]
    {
        let identifier = CString::new(identifier)
            .map_err(|_| AppError::Invalid("identifiant d’événement invalide".into()))?;
        if unsafe { syn_native_calendar_delete(identifier.as_ptr()) } == 1 {
            return Ok(());
        }
        return Err(AppError::Other(
            "Impossible de supprimer l’événement Apple Calendar.".into(),
        ));
    }
    #[allow(unreachable_code)]
    Err(AppError::Invalid("Calendrier natif indisponible".into()))
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::*;

    #[test]
    fn lit_les_statuts_tcc_sans_declencher_de_demande() {
        assert!([
            "granted",
            "limited",
            "denied",
            "restricted",
            "needs_permission",
        ]
        .contains(&permission_status("contacts")));
        assert_eq!(permission_status("service-inconnu"), "unavailable");
    }
}
