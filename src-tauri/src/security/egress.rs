//! Contrôle d'egress (Sécurité §3.5) : aucune connexion sortante hors des
//! connecteurs explicitement autorisés. Par défaut, seul le loopback (Ollama dev).

use crate::error::{AppError, Result};
use std::collections::HashSet;
use std::sync::RwLock;

pub struct EgressGuard {
    allowed_hosts: RwLock<HashSet<String>>,
}

impl Default for EgressGuard {
    fn default() -> Self {
        Self::new()
    }
}

impl EgressGuard {
    pub fn new() -> Self {
        let mut set = HashSet::new();
        // Runtime d'inférence local uniquement.
        set.insert("127.0.0.1".to_string());
        set.insert("localhost".to_string());
        EgressGuard {
            allowed_hosts: RwLock::new(set),
        }
    }

    pub fn allow(&self, host: &str) {
        self.allowed_hosts
            .write()
            .unwrap()
            .insert(host.to_lowercase());
    }

    pub fn revoke(&self, host: &str) {
        self.allowed_hosts
            .write()
            .unwrap()
            .remove(&host.to_lowercase());
    }

    /// À appeler avant TOUTE requête réseau. Refuse les cibles non autorisées —
    /// notamment toute cible suggérée par du contenu non fiable.
    pub fn check(&self, url: &str) -> Result<()> {
        let parsed = reqwest::Url::parse(url)
            .map_err(|_| AppError::Security(format!("URL invalide : {url}")))?;
        let host = parsed.host_str().unwrap_or("").to_lowercase();
        if self.allowed_hosts.read().unwrap().contains(&host) {
            Ok(())
        } else {
            Err(AppError::Security(format!(
                "Sortie réseau refusée vers « {host} » : hôte non autorisé par un connecteur actif."
            )))
        }
    }

    pub fn allowed(&self) -> Vec<String> {
        self.allowed_hosts.read().unwrap().iter().cloned().collect()
    }
}
