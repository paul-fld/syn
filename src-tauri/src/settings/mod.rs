//! Réglages typés (doc Onboarding/Réglages partie B), persistés dans `settings`.
//! Le plancher humain n'est PAS un réglage : il n'est jamais désactivable.

use crate::db::Db;
use crate::error::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum Autonomy {
    Prudent,
    Assiste,
    Autonome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VoiceProfile {
    pub formality: String,            // "tu" | "vous"
    pub address_form: Option<String>, // « Monsieur », prénom… ou rien
    pub extras: Vec<String>,          // consignes de style (parole de Syn uniquement)
}

impl VoiceProfile {
    /// La forme d'adresse choisie par l'utilisateur (Personnalisation) ou
    /// imposée par une de ses règles — `recompute_voice_profile` fusionne les
    /// deux dans ce profil. Toute phrase que Syn adresse à l'utilisateur, y
    /// compris celles écrites en dur côté Rust, passe par ici.
    pub fn vouvoie(&self) -> bool {
        self.formality == "vous"
    }

    /// Choisit entre la variante tutoyée et la variante vouvoyée.
    pub fn pick<'a>(&self, tu: &'a str, vous: &'a str) -> &'a str {
        if self.vouvoie() {
            vous
        } else {
            tu
        }
    }
}

impl Default for VoiceProfile {
    fn default() -> Self {
        // Défaut aligné sur les maquettes (vouvoiement).
        VoiceProfile {
            formality: "vous".into(),
            address_form: None,
            extras: vec![],
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    // Général
    pub autonomy: Autonomy,
    pub startup_brief_enabled: bool,
    pub brief_floor_hour: u8,
    pub brief_sections: Vec<String>, // events | tasks | commitments | mails | system | birthdays | continue
    pub daily_wrap_enabled: bool,
    pub daily_wrap_hour: u8,
    pub autostart: bool,
    // Personnalisation
    pub voice: VoiceProfile,
    pub theme: String, // "dark" (V1)
    /// Langue des réponses : "auto" (celle de l'utilisateur, détectée), "fr", "en".
    /// La langue de TRAVAIL de Syn (consignes internes, requêtes aux services)
    /// est l'anglais quoi qu'il arrive : ce réglage ne concerne que ce que
    /// l'utilisateur lit.
    pub answer_language: String,
    // Accessibilité
    pub voice_input_enabled: bool,
    pub voice_output_enabled: bool,
    pub bar_shortcut: String,
    pub reduce_motion: bool,
    pub large_text: bool,
    // Notifications
    pub notifications_enabled: bool,
    pub notifications_muted: bool,
    pub notification_sound: bool,
    pub notification_min_priority: String, // info | important | urgent
    pub notify_briefs: bool,
    pub notify_events: bool,
    pub notify_commitments: bool,
    pub notify_system: bool,
    pub notify_rules: bool,
    /// Interrupteur général des réflexes (messages sans réponse, réunions à
    /// préparer…). Chaque réflexe reste débrayable individuellement dans
    /// « Mes programmations ».
    pub notify_reflexes: bool,
    pub work_notification_policy: String, // urgent | relevant
    // Confidentialité
    pub cloud_escalation: bool,  // opt-in, OFF par défaut (invariant 2)
    pub sensitive_consent: bool, // gate de lecture des documents sensibles (Média §B8)
    pub files_full_access_requested: bool,
    pub rarity_budget: u32,    // plafond de surfaçages proactifs / jour
    pub guardian_disk_pct: u8, // alerte si espace libre < N %
    pub guardian_temp_c: u8,
    // Modes
    pub work_mode: bool,
    pub eco_mode: bool,
    pub indexing_paused: bool,
    // Intelligence
    pub tier: String, // leger | standard | costaud
    pub chat_model: String,
    pub embed_model: String,
    pub ollama_url: String,
    // Onboarding
    pub onboarding_done: bool,
    pub last_brief_date: String,
    pub last_wrap_date: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            autonomy: Autonomy::Assiste,
            startup_brief_enabled: true,
            brief_floor_hour: 7,
            brief_sections: vec![
                "events".into(),
                "tasks".into(),
                "commitments".into(),
                "mails".into(),
                "birthdays".into(),
                "continue".into(),
            ],
            daily_wrap_enabled: true,
            daily_wrap_hour: 18,
            autostart: false,
            voice: VoiceProfile::default(),
            theme: "dark".into(),
            answer_language: "auto".into(),
            voice_input_enabled: false,
            voice_output_enabled: false,
            // 🔎 tranché au build : Option+Espace, sans conflit avec Spotlight (Cmd+Espace).
            bar_shortcut: "Alt+Space".into(),
            reduce_motion: false,
            large_text: false,
            notifications_enabled: true,
            notifications_muted: false,
            notification_sound: true,
            notification_min_priority: "info".into(),
            notify_briefs: true,
            notify_events: true,
            notify_commitments: true,
            notify_system: true,
            notify_rules: true,
            notify_reflexes: true,
            work_notification_policy: "urgent".into(),
            cloud_escalation: false,
            // Décision produit (14/08/2026) : Syn lit tout par défaut — la
            // frustration d'autoriser fichier par fichier bloquait l'usage.
            // Le toggle Réglages ▸ Confidentialité reste le levier d'opt-out.
            sensitive_consent: true,
            files_full_access_requested: false,
            rarity_budget: 5,
            guardian_disk_pct: 5,
            guardian_temp_c: 90,
            work_mode: false,
            eco_mode: false,
            indexing_paused: false,
            tier: "standard".into(),
            chat_model: "llama3.1:latest".into(),
            embed_model: "nomic-embed-text".into(),
            ollama_url: "http://127.0.0.1:11434".into(),
            onboarding_done: false,
            last_brief_date: String::new(),
            last_wrap_date: String::new(),
        }
    }
}

pub fn load(db: &Db) -> Result<Settings> {
    let mut base = serde_json::to_value(Settings::default())?;
    db.read(|c| {
        let mut stmt = c.prepare("SELECT key, value FROM settings")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
        for row in rows {
            let (k, v) = row?;
            if let (Some(obj), Ok(val)) = (base.as_object_mut(), serde_json::from_str::<Value>(&v))
            {
                obj.insert(k, val);
            }
        }
        Ok(())
    })?;
    Ok(serde_json::from_value(base)?)
}

pub fn save(db: &Db, s: &Settings) -> Result<()> {
    let val = serde_json::to_value(s)?;
    db.with(|c| {
        for (k, v) in val.as_object().unwrap() {
            c.execute(
                "INSERT INTO settings (key, value) VALUES (?1, ?2)
                 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                rusqlite::params![k, serde_json::to_string(v).unwrap()],
            )?;
        }
        Ok(())
    })
}

pub fn set_key(db: &Db, key: &str, value: &Value) -> Result<()> {
    db.with(|c| {
        c.execute(
            "INSERT INTO settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![key, serde_json::to_string(value)?],
        )?;
        Ok(())
    })
}
