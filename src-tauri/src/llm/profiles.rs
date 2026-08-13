//! Sélection par palier matériel (Intelligence §2.4).
//! 🔎 Modèles par défaut figés pour ce build (licences permissives — Llama Community /
//! Apache 2.0) ; remplaçables dans les réglages sans toucher au cœur.

use serde::Serialize;
use sysinfo::System;

#[derive(Debug, Clone, Serialize)]
pub struct HardwareProfile {
    pub tier: String, // leger | standard | costaud
    pub total_ram_gb: f64,
    pub cpu_arch: String,
    pub cpu_count: usize,
    pub chat_model: String,
    pub embed_model: String,
}

pub fn detect() -> HardwareProfile {
    let mut sys = System::new();
    sys.refresh_memory();
    sys.refresh_cpu();
    let total_ram_gb = sys.total_memory() as f64 / 1024.0 / 1024.0 / 1024.0;
    let cpu_arch = std::env::consts::ARCH.to_string();
    let cpu_count = sys.cpus().len().max(1);

    let tier = if total_ram_gb < 12.0 {
        "leger"
    } else if total_ram_gb < 32.0 {
        "standard"
    } else {
        "costaud"
    };

    let chat_model = match tier {
        "leger" => "llama3.2:3b",
        "costaud" => "qwen3:14b",
        _ => "llama3.1:latest",
    };

    HardwareProfile {
        tier: tier.to_string(),
        total_ram_gb: (total_ram_gb * 10.0).round() / 10.0,
        cpu_arch,
        cpu_count,
        chat_model: chat_model.to_string(),
        embed_model: "nomic-embed-text".to_string(),
    }
}
