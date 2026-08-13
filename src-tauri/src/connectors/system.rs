//! Connecteur Système (doc Connecteurs §3) : des « capteurs » sur la machine.
//! Lecture libre ; toute action système passerait par la porte d'action.
//! 🔎 APIs thermiques par OS : sysinfo expose les sondes disponibles ; macOS
//! batterie via `pmset` (IOKit indirect). Limite honnête : bon sur les cas
//! courants, pas un diagnostic hardware expert.

use serde::Serialize;
use serde_json::{json, Value};
use sysinfo::{Components, Disks, System};

#[derive(Debug, Clone, Serialize)]
pub struct SystemSnapshot {
    pub os: String,
    pub cpu_pct: f32,
    pub cpu_count: usize,
    pub mem_total_gb: f64,
    pub mem_used_gb: f64,
    pub disks: Vec<Value>,
    pub temps: Vec<Value>,
    pub battery: Option<Value>,
    pub top_processes: Vec<Value>,
    pub uptime_secs: u64,
}

pub fn snapshot() -> SystemSnapshot {
    let mut sys = System::new_all();
    sys.refresh_cpu();
    std::thread::sleep(std::time::Duration::from_millis(200));
    sys.refresh_cpu();
    sys.refresh_memory();
    sys.refresh_processes();

    let cpu_pct = sys.global_cpu_info().cpu_usage();
    let mem_total_gb = sys.total_memory() as f64 / 1e9;
    let mem_used_gb = sys.used_memory() as f64 / 1e9;

    let disks: Vec<Value> = Disks::new_with_refreshed_list()
        .iter()
        .map(|d| {
            json!({
                "mount": d.mount_point().to_string_lossy(),
                "total_gb": (d.total_space() as f64 / 1e9 * 10.0).round() / 10.0,
                "free_gb": (d.available_space() as f64 / 1e9 * 10.0).round() / 10.0,
            })
        })
        .collect();

    let temps: Vec<Value> = Components::new_with_refreshed_list()
        .iter()
        .filter(|c| c.temperature() > 0.0)
        .take(8)
        .map(|c| json!({"label": c.label(), "celsius": (c.temperature() * 10.0).round() / 10.0}))
        .collect();

    let mut procs: Vec<(String, f32, u64)> = sys
        .processes()
        .values()
        .map(|p| (p.name().to_string(), p.cpu_usage(), p.memory()))
        .collect();
    procs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    let top_processes: Vec<Value> = procs
        .into_iter()
        .take(6)
        .map(|(name, cpu, mem)| {
            json!({"name": name, "cpu_pct": (cpu * 10.0).round() / 10.0, "mem_mb": mem / 1_000_000})
        })
        .collect();

    SystemSnapshot {
        os: format!(
            "{} {}",
            System::name().unwrap_or_default(),
            System::os_version().unwrap_or_default()
        ),
        cpu_pct,
        cpu_count: sys.cpus().len(),
        mem_total_gb: (mem_total_gb * 10.0).round() / 10.0,
        mem_used_gb: (mem_used_gb * 10.0).round() / 10.0,
        disks,
        temps,
        battery: battery_info(),
        top_processes,
        uptime_secs: System::uptime(),
    }
}

/// Batterie via pmset (macOS) — absent ailleurs (dégradation gracieuse).
fn battery_info() -> Option<Value> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let out = std::process::Command::new("pmset")
        .args(["-g", "batt"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let pct = text
        .split('%')
        .next()?
        .rsplit(|c: char| !c.is_ascii_digit())
        .next()?
        .parse::<u8>()
        .ok()?;
    let charging = text.contains("AC Power") || text.contains("charging");
    Some(json!({"pct": pct, "charging": charging}))
}

/// Corrèle les signaux et EXPLIQUE (jamais « Syn a deviné »).
pub fn diagnose(s: &SystemSnapshot) -> String {
    let mut findings: Vec<String> = vec![];
    if s.cpu_pct > 80.0 {
        let culprit = s.top_processes.first().map(|p| {
            format!(
                " Le processus « {} » y contribue le plus ({} % CPU).",
                p["name"].as_str().unwrap_or("?"),
                p["cpu_pct"]
            )
        });
        findings.push(format!(
            "Le CPU est très sollicité ({:.0} %).{}",
            s.cpu_pct,
            culprit.unwrap_or_default()
        ));
    }
    let mem_pct = if s.mem_total_gb > 0.0 {
        s.mem_used_gb / s.mem_total_gb * 100.0
    } else {
        0.0
    };
    if mem_pct > 85.0 {
        findings.push(format!(
            "La mémoire est presque saturée ({:.1} sur {:.1} Go) — les ralentissements viennent souvent de là.",
            s.mem_used_gb, s.mem_total_gb
        ));
    }
    for d in &s.disks {
        let (total, free) = (
            d["total_gb"].as_f64().unwrap_or(1.0),
            d["free_gb"].as_f64().unwrap_or(0.0),
        );
        if total > 0.0 && free / total < 0.07 {
            findings.push(format!(
                "Le disque {} est presque plein ({:.0} Go libres sur {:.0}).",
                d["mount"].as_str().unwrap_or("?"),
                free,
                total
            ));
        }
    }
    for t in &s.temps {
        if t["celsius"].as_f64().unwrap_or(0.0) > 85.0 {
            findings.push(format!(
                "La sonde {} relève {} °C — température élevée, probablement en conséquence de la charge CPU.",
                t["label"].as_str().unwrap_or("?"),
                t["celsius"]
            ));
        }
    }
    if let Some(b) = &s.battery {
        if b["pct"].as_u64().unwrap_or(100) < 15 && !b["charging"].as_bool().unwrap_or(false) {
            findings.push(format!(
                "Batterie faible ({} %) — le mode économie peut limiter Syn à l'essentiel.",
                b["pct"]
            ));
        }
    }
    if findings.is_empty() {
        "Rien d'anormal : charge CPU, mémoire, disque et température sont dans les valeurs normales.".into()
    } else {
        findings.join(" ")
    }
}
