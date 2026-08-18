//! État applicatif : le backend démarre verrouillé ; le déverrouillage (mot de
//! passe maître / trousseau) ouvre la base chiffrée et démarre les boucles.

use crate::bus::Bus;
use crate::connectors::files::Indexer;
use crate::db::Db;
use crate::error::{AppError, Result};
use crate::llm::{ollama::OllamaClient, LlmClient};
use crate::security::egress::EgressGuard;
use crate::security::keys::KeyStore;
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use zeroize::Zeroize;

pub struct Core {
    pub db: Db,
    pub llm: Arc<dyn LlmClient>,
    pub bus: Bus,
    pub indexer: Arc<Indexer>,
    pub key_hex: Arc<std::sync::Mutex<String>>,
}

pub struct AppState {
    pub keystore: KeyStore,
    pub bus: Bus,
    pub egress: Arc<EgressGuard>,
    pub data_dir: PathBuf,
    /// Les modèles sont-ils chargés en mémoire ? L'écran de démarrage attend
    /// ce drapeau plutôt qu'un délai arbitraire : on ne fait patienter
    /// l'utilisateur que le temps réellement nécessaire.
    pub runtime_ready: Arc<std::sync::atomic::AtomicBool>,
    core: RwLock<Option<Arc<Core>>>,
}

impl AppState {
    pub fn new(data_dir: PathBuf) -> Self {
        std::fs::create_dir_all(&data_dir).ok();
        AppState {
            keystore: KeyStore::new(&data_dir),
            bus: Bus::new(),
            egress: Arc::new(EgressGuard::new()),
            data_dir,
            runtime_ready: Arc::new(std::sync::atomic::AtomicBool::new(false)),
            core: RwLock::new(None),
        }
    }

    pub fn db_path(&self) -> PathBuf {
        self.data_dir.join("syn.db")
    }

    pub fn is_unlocked(&self) -> bool {
        self.core.read().unwrap().is_some()
    }

    pub fn core(&self) -> Result<Arc<Core>> {
        self.core
            .read()
            .unwrap()
            .clone()
            .ok_or_else(AppError::locked)
    }

    /// Ouvre la base avec la clé, construit le LlmClient selon les réglages,
    /// démarre l'indexeur. Idempotent.
    pub fn unlock_with_key(&self, key_hex: &str) -> Result<Arc<Core>> {
        if let Some(core) = self.core.read().unwrap().clone() {
            return Ok(core);
        }
        let db = Db::open(&self.db_path(), key_hex)?;
        let settings = crate::settings::load(&db)?;
        if cfg!(target_os = "macos")
            && settings.files_full_access_requested
            && crate::connectors::files::full_disk_access_granted()
        {
            crate::settings::set_key(&db, "sensitive_consent", &serde_json::Value::Bool(true))?;
            let _ = crate::connectors::files::ensure_full_access_scope(&db)?;
        }
        let llm: Arc<dyn LlmClient> = Arc::new(OllamaClient::new(
            &settings.ollama_url,
            &settings.chat_model,
            &settings.embed_model,
            self.egress.clone(),
        ));
        let indexer = Indexer::start(
            db.clone(),
            llm.clone(),
            self.bus.clone(),
            settings.embed_model.clone(),
        );
        indexer.paused.store(
            settings.indexing_paused,
            std::sync::atomic::Ordering::SeqCst,
        );
        let core = Arc::new(Core {
            db,
            llm,
            bus: self.bus.clone(),
            indexer,
            key_hex: Arc::new(std::sync::Mutex::new(key_hex.to_string())),
        });
        *self.core.write().unwrap() = Some(core.clone());
        // Chargement des modèles dès le déverrouillage, en tâche de fond. Sans
        // cela, c'est la première question de l'utilisateur qui attend que les
        // poids montent en mémoire — le pire moment possible.
        let warming = core.llm.clone();
        let ready_flag = self.runtime_ready.clone();
        let ready_bus = self.bus.clone();
        ready_flag.store(false, std::sync::atomic::Ordering::SeqCst);
        tauri::async_runtime::spawn(async move {
            warming.warm_up().await;
            ready_flag.store(true, std::sync::atomic::Ordering::SeqCst);
            ready_bus.emit(crate::bus::BusEvent::RuntimeReady);
            // Puis le prompt de compréhension d'intention : ses consignes et ses
            // exemples coûtent une vingtaine de secondes à évaluer la première
            // fois. Les traiter maintenant, plutôt que sur la première question
            // de l'utilisateur, est la différence entre une demande comprise et
            // une demande devinée par mots-clés.
            crate::router::intent::preheat(&warming).await;
        });
        // Aucun rescan au démarrage : Indexer rejoue FSEvents depuis le point
        // de contrôle persistant et ne demande un catalogue de secours que si
        // macOS déclare l'historique purgé ou invalide.
        // Sur macOS, Apple est intégré : si l'utilisateur a déjà accordé l'accès
        // à Mail et terminé l'onboarding, la synchronisation reprend sans faux login.
        if cfg!(target_os = "macos")
            && settings.onboarding_done
            && crate::connectors::mail::native_available()
        {
            let db = core.db.clone();
            let llm = core.llm.clone();
            let bus = core.bus.clone();
            let embed_model = settings.embed_model.clone();
            tauri::async_runtime::spawn(async move {
                let _ = crate::connectors::mail::sync_native(&db, &llm, &bus, &embed_model).await;
            });
        }
        // Sens supplémentaires au déverrouillage : agenda (miroir proactivité),
        // Rappels et Messages — tous best-effort, jamais bloquants.
        if cfg!(target_os = "macos") && settings.onboarding_done {
            let db = core.db.clone();
            let llm = core.llm.clone();
            let bus = core.bus.clone();
            let embed_model = settings.embed_model.clone();
            tauri::async_runtime::spawn(async move {
                let db2 = db.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    let _ = crate::connectors::calendar::sync_native_to_db(&db2);
                    let _ = crate::connectors::reminders::sync_native_to_db(&db2);
                })
                .await;
                let _ = crate::connectors::messages::sync(&db, &llm, &bus, &embed_model).await;
            });
        }
        Ok(core)
    }

    /// Reconstruit le LlmClient (changement de modèle/runtime dans les réglages).
    pub fn rebuild_llm(&self) -> Result<()> {
        let core = self.core()?;
        let settings = crate::settings::load(&core.db)?;
        let llm: Arc<dyn LlmClient> = Arc::new(OllamaClient::new(
            &settings.ollama_url,
            &settings.chat_model,
            &settings.embed_model,
            self.egress.clone(),
        ));
        let new_core = Arc::new(Core {
            db: core.db.clone(),
            llm,
            bus: core.bus.clone(),
            indexer: core.indexer.clone(),
            key_hex: core.key_hex.clone(),
        });
        *self.core.write().unwrap() = Some(new_core);
        Ok(())
    }

    /// Verrouille (Déconnexion) : la clé et la base quittent la mémoire.
    pub async fn lock(&self) {
        let core = self.core.write().unwrap().take();
        if let Some(core) = core {
            core.indexer.stop_and_wait().await;
            if let Ok(mut key) = core.key_hex.lock() {
                // Écrasement best-effort avant libération de la chaîne.
                key.zeroize();
            }
        }
    }
}
