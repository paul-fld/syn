//! Connecteur Files (deep-dive dédié — la brique de la Phase 1).
//! Lit, extrait, indexe, surveille. N'organise pas (le rangement est un outil).
//! Périmètre utilisateur autorisé par macOS, avec exclusions strictes des zones
//! système et techniques. Tout échec = skip + log.

use crate::bus::{Bus, BusEvent};
use crate::db::{now, Db};
use crate::error::Result;
use crate::ingestion::{self, extract};
use crate::llm::LlmClient;
use crate::memory::{self, Item};
use notify::RecursiveMode;
use notify_debouncer_mini::{new_debouncer, DebouncedEventKind};
use rusqlite::params;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Système d'exclusion (Média §B3 — le fix du désastre Minecraft),
/// appliqué AUSSI à l'indexation (Files §2).
const EXCLUDED_DIRS: &[&str] = &[
    "node_modules",
    "target",
    "dist",
    "build",
    "out",
    "__pycache__",
    "venv",
    ".venv",
    "env",
    "library",
    "application support",
    "caches",
    "cache",
    "logs",
    "tmp",
    "temp",
    "minecraft",
    ".minecraft",
    "steamapps",
    "resourcepacks",
    "texturepacks",
    "shaderpacks",
    "saves",
    "appdata",
    "program files",
    "windows",
];
const EXCLUDED_BUNDLES: &[&str] = &[
    ".app",
    ".bundle",
    ".framework",
    ".photoslibrary",
    ".imovielibrary",
];
const PROJECT_MARKERS: &[&str] = &[
    ".git",
    "package.json",
    "Cargo.toml",
    "pyproject.toml",
    "go.mod",
    "pom.xml",
    "Makefile",
];
const SENSITIVE_HINTS: &[&str] = &[
    "santé",
    "sante",
    "medical",
    "médical",
    "ordonnance",
    "mutuelle",
    "impot",
    "impôt",
    "impots",
    "banque",
    "iban",
    "rib",
    "salaire",
    "bulletin",
    "paie",
    "paye",
    "passeport",
    "passport",
    "cni",
    "identité",
    "identite",
    "carte_identite",
    "fiche_de_paie",
    "avis-imposition",
];
const MAX_FILE_SIZE: u64 = 200 * 1024 * 1024;
const EXTRACTION_VERSION_ID: &str = "files-v4-progressive-fts";
const EXTRACTION_VERSION: &[u8] = EXTRACTION_VERSION_ID.as_bytes();
const TECHNICAL_FILE_NAMES: &[&str] = &[
    ".ds_store",
    "thumbs.db",
    "desktop.ini",
    "package-lock.json",
    "cargo.lock",
];
const TECHNICAL_EXTENSIONS: &[&str] = &[
    "db",
    "db-shm",
    "db-wal",
    "sqlite",
    "sqlite-shm",
    "sqlite-wal",
    "sqlite3",
    "musicdb",
    "lock",
    "tmp",
    "cache",
];
const INDEXABLE_EXTENSIONS: &[&str] = &[
    "txt", "md", "markdown", "csv", "log", "json", "yaml", "yml", "toml", "tex", "rtf", "pdf",
    "doc", "docx", "odt", "ppt", "pptx", "key", "xls", "xlsx", "ods", "numbers", "pages", "jpg",
    "jpeg", "png", "heic", "tiff", "gif", "webp", "bmp", "mp4", "mov", "avi", "mkv", "mp3", "wav",
    "m4a", "flac", "py", "js", "ts", "tsx", "jsx", "rs", "go", "java", "kt", "c", "cpp", "h",
    "swift", "m", "mm", "rb", "php", "sh", "vue", "svelte", "css", "scss", "html", "sql",
];

pub fn is_excluded_dir(name: &str) -> bool {
    let lower = name.to_lowercase();
    if lower.starts_with('.') {
        return true; // dossiers cachés (conventions par OS)
    }
    if EXCLUDED_BUNDLES.iter().any(|b| lower.ends_with(b)) {
        return true;
    }
    EXCLUDED_DIRS.iter().any(|d| lower == *d)
}

pub fn is_project_root(dir: &Path) -> bool {
    PROJECT_MARKERS.iter().any(|m| dir.join(m).exists())
}

/// Vrai pour un fichier situé dans un projet de développement. Sert à éviter
/// qu'un README contenant les mots d'un scénario de test soit présenté comme
/// un document personnel (par exemple une quittance).
pub fn is_project_content(path: &Path) -> bool {
    path.ancestors().skip(1).take(12).any(is_project_root)
}

pub fn looks_sensitive(path: &Path) -> bool {
    let s = path.to_string_lossy().to_lowercase();
    SENSITIVE_HINTS.iter().any(|h| s.contains(h))
}

pub fn is_technical_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_lowercase();
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    TECHNICAL_FILE_NAMES.contains(&name.as_str()) || TECHNICAL_EXTENSIONS.contains(&ext.as_str())
}

fn is_indexable_file(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| INDEXABLE_EXTENSIONS.contains(&extension.to_ascii_lowercase().as_str()))
        .unwrap_or(false)
}

fn document_index_priority(path: &Path) -> u8 {
    let extension = path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "pdf" | "doc" | "docx" | "odt" | "rtf" | "txt" | "md" | "markdown" | "csv" | "xls"
        | "xlsx" | "ods" | "ppt" | "pptx" | "key" | "pages" | "numbers" => 0,
        "jpg" | "jpeg" | "png" | "heic" | "tiff" | "gif" | "webp" | "bmp" => 1,
        _ => 2,
    }
}

/// Recherche immédiate sur les noms et chemins du périmètre autorisé. Elle
/// complète l'index de contenu pendant sa construction : l'utilisateur ne doit
/// jamais attendre la fin d'un scan de plusieurs dizaines de milliers de
/// fichiers pour retrouver un document dont le nom ou le dossier est parlant.
pub fn live_metadata_search(
    roots: &[String],
    keywords: &[String],
    limit: usize,
) -> Vec<crate::retrieval::Retrieved> {
    if keywords.is_empty() {
        return vec![];
    }
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(1800);
    let mut results = Vec::new();
    let mut visited = 0usize;
    for root in roots {
        for entry in walkdir::WalkDir::new(root)
            .max_depth(20)
            .follow_links(false)
            .into_iter()
            .filter_entry(|entry| {
                if entry.depth() == 0 || !entry.file_type().is_dir() {
                    return true;
                }
                let name = entry.file_name().to_string_lossy();
                !is_excluded_dir(&name) && !is_project_root(entry.path())
            })
            .flatten()
        {
            visited += 1;
            if visited > 250_000 || std::time::Instant::now() >= deadline {
                break;
            }
            if !entry.file_type().is_file()
                || is_technical_file(entry.path())
                || entry
                    .metadata()
                    .is_ok_and(|metadata| metadata.len() > MAX_FILE_SIZE)
            {
                continue;
            }
            let path = entry.path().to_string_lossy().to_string();
            let folded = crate::db::fold(&path);
            let hits = keywords
                .iter()
                .filter(|keyword| folded.contains(keyword.as_str()))
                .count();
            if hits == 0 {
                continue;
            }
            let coverage = hits as f32 / keywords.len().max(1) as f32;
            let title = entry.file_name().to_string_lossy().to_string();
            results.push(crate::retrieval::Retrieved {
                item_id: format!("live:{}", blake3::hash(path.as_bytes()).to_hex()),
                source: "files".into(),
                source_ref: path.clone(),
                title,
                path: Some(path),
                snippet: "Correspondance directe dans le nom ou le chemin du fichier.".into(),
                score: 1.60 + 0.80 * coverage - 0.10 * document_index_priority(entry.path()) as f32,
            });
        }
    }
    results.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.source_ref.cmp(&b.source_ref))
    });
    results.truncate(limit.max(1));
    results
}

#[derive(Debug)]
pub enum IndexJob {
    FullScan(Option<PathBuf>),
    Paths(Vec<PathBuf>),
    Replay {
        paths: Vec<PathBuf>,
        root: String,
        through_event_id: u64,
    },
    Demand(Vec<PathBuf>),
    Drain(usize),
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct CloudBootstrapStatus {
    pub provider: String,
    pub resource: String,
    pub processed: i64,
    pub total: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct IndexStatus {
    pub running: bool,
    pub phase: String,
    pub catalog_ready: bool,
    pub done: u64,
    pub total: u64,
    pub current: Option<String>,
    pub items_count: i64,
    pub pending_embeddings: i64,
    pub sensitive_skipped: i64,
    pub unreadable_files: i64,
    pub eligible_count: i64,
    pub embedded_count: i64,
    pub lexical_count: i64,
    pub coverage_pct: f64,
    pub coverage_high_water_pct: f64,
    pub replay_count: i64,
    pub replayed_events: i64,
    pub fallback_count: i64,
    pub full_scan_count: i64,
    pub folders: Vec<FolderStatus>,
    pub cloud_bootstraps: Vec<CloudBootstrapStatus>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FolderStatus {
    pub path: String,
    pub last_indexed: Option<i64>,
}

pub struct Indexer {
    pub tx: mpsc::UnboundedSender<IndexJob>,
    pub running: Arc<AtomicBool>,
    pub paused: Arc<AtomicBool>,
    pub stopping: Arc<AtomicBool>,
    pub stopped: Arc<AtomicBool>,
    pub done: Arc<AtomicU64>,
    pub total: Arc<AtomicU64>,
    pub current: Arc<std::sync::Mutex<Option<String>>>,
    _watcher: std::sync::Mutex<Option<notify_debouncer_mini::Debouncer<notify::FsEventWatcher>>>,
    worker: std::sync::Mutex<Option<tauri::async_runtime::JoinHandle<()>>>,
}

impl Indexer {
    /// Démarre la boucle d'ingestion (fond, basse priorité, throttlée — Files §6)
    /// et la surveillance FS (debounce 2 s — Files §5).
    pub fn start(db: Db, llm: Arc<dyn LlmClient>, bus: Bus, embed_model: String) -> Arc<Indexer> {
        let (tx, mut rx) = mpsc::unbounded_channel::<IndexJob>();
        let indexer = Arc::new(Indexer {
            tx: tx.clone(),
            running: Arc::new(AtomicBool::new(false)),
            paused: Arc::new(AtomicBool::new(false)),
            stopping: Arc::new(AtomicBool::new(false)),
            stopped: Arc::new(AtomicBool::new(false)),
            done: Arc::new(AtomicU64::new(0)),
            total: Arc::new(AtomicU64::new(0)),
            current: Arc::new(std::sync::Mutex::new(None)),
            _watcher: std::sync::Mutex::new(None),
            worker: std::sync::Mutex::new(None),
        });

        // Watcher FS → jobs incrémentaux.
        {
            let tx_watch = tx.clone();
            let mut debouncer = new_debouncer(
                std::time::Duration::from_secs(2),
                move |events: notify_debouncer_mini::DebounceEventResult| {
                    if let Ok(events) = events {
                        let paths: Vec<PathBuf> = events
                            .into_iter()
                            .filter(|e| matches!(e.kind, DebouncedEventKind::Any))
                            .map(|e| e.path)
                            .collect();
                        if !paths.is_empty() {
                            let _ = tx_watch.send(IndexJob::Paths(paths));
                        }
                    }
                },
            )
            .ok();
            if let (Some(deb), Ok(folders)) = (debouncer.as_mut(), folder_paths(&db)) {
                for f in &folders {
                    let _ = deb.watcher().watch(Path::new(f), RecursiveMode::Recursive);
                }
            }
            *indexer._watcher.lock().unwrap() = debouncer;
        }

        // Boucle d'ingestion.
        let ix = indexer.clone();
        let startup_db = db.clone();
        let worker = tauri::async_runtime::spawn(async move {
            while let Some(job) = rx.recv().await {
                if ix.stopping.load(Ordering::SeqCst) || matches!(job, IndexJob::Shutdown) {
                    break;
                }
                ix.running.store(true, Ordering::SeqCst);
                let mut replay_checkpoint = None;
                let result = match job {
                    IndexJob::FullScan(one) => {
                        ix.full_scan(&db, &llm, &bus, &embed_model, one).await
                    }
                    IndexJob::Paths(paths) => {
                        ix.incremental(&db, &llm, &bus, &embed_model, paths).await
                    }
                    IndexJob::Replay {
                        paths,
                        root,
                        through_event_id,
                    } => {
                        replay_checkpoint = Some((root, through_event_id));
                        ix.incremental(&db, &llm, &bus, &embed_model, paths).await
                    }
                    IndexJob::Demand(paths) => {
                        ix.demand(&db, &llm, &bus, &embed_model, paths).await
                    }
                    IndexJob::Drain(limit) => {
                        ix.drain_queue(&db, &llm, &bus, &embed_model, limit).await
                    }
                    IndexJob::Shutdown => break,
                };
                if let Err(e) = result {
                    bus.emit(BusEvent::FilesError {
                        path: String::new(),
                        reason: e.to_string(),
                    });
                } else if let Some((root, event_id)) = replay_checkpoint {
                    // Le curseur n'avance qu'après écriture du lot rejoué.
                    let _ = set_fsevents_checkpoint(&db, &root, event_id);
                }
                ix.running.store(false, Ordering::SeqCst);
                bus.emit(BusEvent::IngestionStatus {
                    state: "idle".into(),
                    current: None,
                    done: ix.done.load(Ordering::SeqCst),
                    total: ix.total.load(Ordering::SeqCst),
                });
            }
            ix.running.store(false, Ordering::SeqCst);
            ix.stopped.store(true, Ordering::SeqCst);
        });
        *indexer.worker.lock().unwrap() = Some(worker);
        // Le replay est lancé après le worker afin de ne jamais bloquer le
        // déverrouillage. Un scan catalogue n'est envoyé que si macOS déclare
        // explicitement l'historique invalide/purgé.
        #[cfg(target_os = "macos")]
        {
            let replay_db = startup_db.clone();
            let checkpoint_db = startup_db;
            let replay_tx = tx;
            tauri::async_runtime::spawn(async move {
                let recovered = tokio::task::spawn_blocking(move || replay_fsevents(&replay_db))
                    .await
                    .unwrap_or_default();
                for recovery in recovered {
                    match recovery {
                        FsRecovery::Delta {
                            paths,
                            root,
                            current,
                        } if !paths.is_empty() => {
                            let _ = replay_tx.send(IndexJob::Replay {
                                paths,
                                root,
                                through_event_id: current,
                            });
                        }
                        FsRecovery::Delta { root, current, .. } => {
                            let _ = set_fsevents_checkpoint(&checkpoint_db, &root, current);
                        }
                        FsRecovery::Fallback(root) => {
                            let _ = replay_tx.send(IndexJob::FullScan(Some(root)));
                        }
                    }
                }
            });
        }
        indexer
    }

    pub fn watch_folder(&self, path: &Path) {
        if let Some(deb) = self._watcher.lock().unwrap().as_mut() {
            let _ = deb.watcher().watch(path, RecursiveMode::Recursive);
        }
    }

    pub fn unwatch_folder(&self, path: &Path) {
        if let Some(deb) = self._watcher.lock().unwrap().as_mut() {
            let _ = deb.watcher().unwatch(path);
        }
    }

    /// Coupe immédiatement le watcher et demande l'arrêt de la boucle. Les scans
    /// vérifient aussi ce drapeau entre chaque fichier afin qu'un verrouillage ne
    /// laisse pas l'indexation continuer avec une base déjà ouverte.
    pub fn stop(&self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.paused.store(false, Ordering::SeqCst);
        *self._watcher.lock().unwrap() = None;
        let _ = self.tx.send(IndexJob::Shutdown);
    }

    pub async fn stop_and_wait(&self) {
        self.stop();
        let worker = self.worker.lock().unwrap().take();
        if let Some(worker) = worker {
            let _ = worker.await;
        }
    }

    async fn full_scan(
        &self,
        db: &Db,
        _llm: &Arc<dyn LlmClient>,
        bus: &Bus,
        _embed_model: &str,
        only: Option<PathBuf>,
    ) -> Result<()> {
        // Point de départ conservateur : tout événement arrivé pendant le scan
        // restera rejouable au prochain lancement.
        let scan_baseline = crate::connectors::native::fsevents_current_id();
        let folders = match only {
            Some(p) => vec![p.to_string_lossy().to_string()],
            None => folder_paths(db)?,
        };
        crate::security::log_access(db, "files", "full_scan", None);
        self.done.store(0, Ordering::SeqCst);
        self.total.store(0, Ordering::SeqCst);
        *self.current.lock().unwrap() = Some("Préparation du catalogue…".into());

        // 1. Énumération (walk + exclusions + projets atomiques).
        let mut files: Vec<PathBuf> = vec![];
        let mut projects: Vec<PathBuf> = vec![];
        for folder in &folders {
            walk_collect(
                Path::new(folder),
                &mut files,
                &mut projects,
                0,
                &self.stopping,
            );
        }
        self.total
            .store((files.len() + projects.len()) as u64, Ordering::SeqCst);
        self.done.store(0, Ordering::SeqCst);

        // Phase 1 : un catalogue léger (nom, chemin, taille, date) rend tous les
        // fichiers trouvables immédiatement, sans lire leurs octets ni appeler
        // le moteur d'embeddings. Les contenus sont enrichis paresseusement.
        files.sort_by_key(|path| document_index_priority(path));
        catalog_metadata(db, &files).await?;
        self.total.store(files.len() as u64, Ordering::SeqCst);
        self.done.store(files.len() as u64, Ordering::SeqCst);
        bus.emit(BusEvent::IngestionStatus {
            state: "ready".into(),
            current: None,
            done: files.len() as u64,
            total: files.len() as u64,
        });

        // Les projets déjà connus restent dans l'index. Les nouveaux sont
        // découverts à la demande ; leur analyse ne bloque pas le catalogue.
        let _ = projects;

        for folder in &folders {
            db.with(|c| {
                c.execute(
                    "UPDATE folders SET last_indexed=?2 WHERE path=?1",
                    params![folder, now()],
                )?;
                c.execute(
                    "INSERT INTO fs_journal_state(root,last_event_id,full_scan_count,updated_at)
                     VALUES (?1,?2,1,?3) ON CONFLICT(root) DO UPDATE SET
                     last_event_id=excluded.last_event_id,
                     full_scan_count=full_scan_count+1,updated_at=excluded.updated_at",
                    params![folder, scan_baseline as i64, now()],
                )?;
                Ok(())
            })?;
        }
        // Le watcher peut manquer une suppression survenue pendant que Syn
        // était fermé. Après un scan complet terminé, retirer de la recherche
        // les chemins qui n'existent réellement plus sur le disque.
        if !self.stopping.load(Ordering::SeqCst) {
            reconcile_missing_files(db, &folders)?;
        }
        *self.current.lock().unwrap() = None;

        // Aucun plafond artificiel : la file persistante contient tout le
        // corpus éligible et sera drainée par les périodes idle + secteur.
        record_coverage(db, "catalog_ready")?;
        Ok(())
    }

    async fn incremental(
        &self,
        db: &Db,
        _llm: &Arc<dyn LlmClient>,
        _bus: &Bus,
        _embed_model: &str,
        paths: Vec<PathBuf>,
    ) -> Result<()> {
        self.total.store(paths.len() as u64, Ordering::SeqCst);
        self.done.store(0, Ordering::SeqCst);
        for path in paths {
            if self.stopping.load(Ordering::SeqCst) {
                break;
            }
            if !is_path_in_active_scope(db, &path)? {
                continue;
            }
            if path
                .components()
                .any(|c| is_excluded_dir(&c.as_os_str().to_string_lossy()))
            {
                continue;
            }
            if !path.exists() {
                // Suppression : marquer retiré, ne pas casser les citations passées.
                let _ = memory::mark_removed(db, "files", &path.to_string_lossy());
                let _ = db.with(|connection| {
                    connection.execute(
                        "UPDATE enrichment_queue SET state='removed',updated_at=?2
                         WHERE source='files' AND source_ref=?1",
                        params![path.to_string_lossy(), now()],
                    )?;
                    Ok(())
                });
                continue;
            }
            if path.is_dir() {
                continue;
            }
            catalog_metadata(db, std::slice::from_ref(&path)).await?;
        }
        Ok(())
    }

    async fn demand(
        &self,
        db: &Db,
        llm: &Arc<dyn LlmClient>,
        bus: &Bus,
        embed_model: &str,
        paths: Vec<PathBuf>,
    ) -> Result<()> {
        self.incremental(db, llm, bus, embed_model, paths.clone())
            .await?;
        prioritize_paths(db, &paths)?;
        // Le travail reste derrière la réponse de recherche, dans ce worker.
        self.drain_queue(db, llm, bus, embed_model, paths.len().min(8))
            .await
    }

    async fn drain_queue(
        &self,
        db: &Db,
        llm: &Arc<dyn LlmClient>,
        bus: &Bus,
        embed_model: &str,
        limit: usize,
    ) -> Result<()> {
        let jobs = next_enrichment_jobs(db, limit)?;
        self.total.store(jobs.len() as u64, Ordering::SeqCst);
        self.done.store(0, Ordering::SeqCst);
        for (item_id, source, source_ref) in jobs {
            if self.stopping.load(Ordering::SeqCst) {
                break;
            }
            mark_enrichment_started(db, &item_id)?;
            let result = if source == "files" {
                let path = PathBuf::from(&source_ref);
                self.set_current(&path);
                index_file(db, llm, bus, embed_model, &path).await
            } else {
                crate::connectors::external::enrich_item(
                    &source,
                    &item_id,
                    &source_ref,
                    db,
                    llm,
                    bus,
                    embed_model,
                )
                .await
            };
            finish_enrichment(db, &item_id, result.as_ref().err())?;
            if let Err(error) = result {
                bus.emit(BusEvent::FilesError {
                    path: source_ref,
                    reason: error.to_string(),
                });
            }
            self.tick(bus).await;
        }
        record_coverage(db, "background_batch")?;
        Ok(())
    }

    fn set_current(&self, p: &Path) {
        *self.current.lock().unwrap() = Some(p.to_string_lossy().to_string());
    }

    /// Throttle + pause (mode économie / batterie faible) + progression.
    async fn tick(&self, bus: &Bus) {
        let done = self.done.fetch_add(1, Ordering::SeqCst) + 1;
        let total = self.total.load(Ordering::SeqCst);
        if done.is_multiple_of(10) || done == total {
            bus.emit(BusEvent::IngestionStatus {
                state: "indexing".into(),
                current: self.current.lock().unwrap().clone(),
                done,
                total,
            });
        }
        while self.paused.load(Ordering::SeqCst) {
            if self.stopping.load(Ordering::SeqCst) {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        }
        // Basse priorité : l'interactif prime toujours.
        tokio::time::sleep(std::time::Duration::from_millis(15)).await;
    }

    pub fn status(&self, db: &Db) -> Result<IndexStatus> {
        let (items_count, pending, sensitive, unreadable) = db.read(|c| {
            let items: i64 = c.query_row(
                "SELECT COUNT(*) FROM items WHERE source='files' AND status='active'",
                [],
                |r| r.get(0),
            )?;
            let pending: i64 =
                c.query_row("SELECT COUNT(*) FROM embeddings WHERE vector IS NULL", [], |r| r.get(0))?;
            let sensitive: i64 = c.query_row(
                "SELECT COUNT(*) FROM items WHERE source='files' AND type='sensible_non_lu' AND status='active'",
                [],
                |r| r.get(0),
            )?;
            let unreadable: i64 = c.query_row(
                "SELECT COUNT(*) FROM items WHERE source='files' AND status='active'
                 AND type='document' AND body IS NULL",
                [],
                |r| r.get(0),
            )?;
            Ok((items, pending, sensitive, unreadable))
        })?;
        let folders = db.read(|c| {
            let mut stmt = c.prepare(
                "SELECT path, last_indexed FROM folders WHERE status='active' ORDER BY path",
            )?;
            let rows = stmt.query_map([], |r| {
                Ok(FolderStatus {
                    path: r.get(0)?,
                    last_indexed: r.get(1)?,
                })
            })?;
            let mut out = vec![];
            for r in rows {
                out.push(r?);
            }
            Ok(out)
        })?;
        let cloud_bootstraps = db.read(|connection| {
            let mut statement = connection.prepare(
                "SELECT provider,resource,processed,total FROM connector_bootstrap_state
                 ORDER BY provider,resource",
            )?;
            let rows = statement.query_map([], |row| {
                Ok(CloudBootstrapStatus {
                    provider: row.get(0)?,
                    resource: row.get(1)?,
                    processed: row.get(2)?,
                    total: row.get(3)?,
                })
            })?;
            let mut statuses = Vec::new();
            for row in rows {
                statuses.push(row?);
            }
            Ok(statuses)
        })?;
        let running = self.running.load(Ordering::SeqCst);
        let catalog_ready =
            !folders.is_empty() && folders.iter().all(|folder| folder.last_indexed.is_some());
        let (eligible, embedded, lexical, coverage_pct, high_water) = coverage(db)?;
        let (replay_count, replayed_events, fallback_count, full_scan_count) = db.read(|c| {
            c.query_row(
                "SELECT COALESCE(SUM(replay_count),0),COALESCE(SUM(replayed_events),0),
                        COALESCE(SUM(fallback_count),0),COALESCE(SUM(full_scan_count),0)
                 FROM fs_journal_state",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .map_err(Into::into)
        })?;
        Ok(IndexStatus {
            running,
            phase: if running && catalog_ready {
                "enriching"
            } else if running {
                "cataloging"
            } else {
                "ready"
            }
            .into(),
            catalog_ready,
            done: self.done.load(Ordering::SeqCst),
            total: self.total.load(Ordering::SeqCst),
            current: self.current.lock().unwrap().clone(),
            items_count,
            pending_embeddings: pending,
            sensitive_skipped: sensitive,
            unreadable_files: unreadable,
            eligible_count: eligible,
            embedded_count: embedded,
            lexical_count: lexical,
            // La valeur publique est un high-water monotone. Les compteurs
            // bruts permettent toujours d'auditer la couverture courante.
            coverage_pct: coverage_pct.max(high_water),
            coverage_high_water_pct: high_water,
            replay_count,
            replayed_events,
            fallback_count,
            full_scan_count,
            folders,
            cloud_bootstraps,
        })
    }
}

fn reconcile_missing_files(db: &Db, folders: &[String]) -> Result<usize> {
    let indexed: Vec<String> = db.read(|c| {
        let mut stmt =
            c.prepare("SELECT source_ref FROM items WHERE source='files' AND status='active'")?;
        let rows = stmt.query_map([], |row| row.get(0))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    })?;
    let mut removed = 0;
    for source_ref in indexed {
        let path = Path::new(&source_ref);
        let in_scanned_scope = folders.iter().any(|folder| path.starts_with(folder));
        if in_scanned_scope && !path.exists() {
            memory::mark_removed(db, "files", &source_ref)?;
            db.with(|connection| {
                connection.execute(
                    "UPDATE enrichment_queue SET state='removed',updated_at=?2
                     WHERE source='files' AND source_ref=?1",
                    params![source_ref, now()],
                )?;
                Ok(())
            })?;
            removed += 1;
        }
    }
    Ok(removed)
}

pub fn folder_paths(db: &Db) -> Result<Vec<String>> {
    db.read(|c| {
        let mut stmt = c.prepare("SELECT path FROM folders WHERE status='active'")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
    })
}

#[derive(Debug)]
enum FsRecovery {
    Delta {
        paths: Vec<PathBuf>,
        root: String,
        current: u64,
    },
    Fallback(PathBuf),
}

fn replay_fsevents(db: &Db) -> Vec<FsRecovery> {
    let roots = folder_paths(db).unwrap_or_default();
    let extractor_changed = db
        .with(|connection| {
            let previous = connection
                .query_row(
                    "SELECT value FROM settings WHERE key='files_extractor_version'",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .unwrap_or_default();
            connection.execute(
                "INSERT INTO settings(key,value) VALUES ('files_extractor_version',?1)
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [EXTRACTION_VERSION_ID],
            )?;
            Ok(previous != EXTRACTION_VERSION_ID)
        })
        .unwrap_or(false);
    if extractor_changed {
        let _ = db.with(|connection| {
            // Nouvelle génération de couverture : un ancien 100 % ne doit pas
            // masquer le rattrapage rendu nécessaire par le nouvel extracteur.
            connection.execute("DELETE FROM index_metric_log", [])?;
            Ok(())
        });
        crate::security::log_access(
            db,
            "files",
            "extractor_version_rebuild",
            Some(EXTRACTION_VERSION_ID),
        );
        return roots
            .into_iter()
            .map(|root| FsRecovery::Fallback(PathBuf::from(root)))
            .collect();
    }
    let mut recoveries = Vec::new();
    for root in roots {
        let since = db
            .read(|connection| {
                Ok(connection
                    .query_row(
                        "SELECT last_event_id FROM fs_journal_state WHERE root=?1",
                        [&root],
                        |row| row.get::<_, i64>(0),
                    )
                    .unwrap_or(0))
            })
            .unwrap_or(0);
        let replay = crate::connectors::native::fsevents_replay(&root, since.max(0) as u64)
            .unwrap_or_else(|_| serde_json::json!({"valid":false,"current_id":since,"events":[]}));
        recoveries.push(apply_fsevents_replay(db, &root, since, &replay));
    }
    recoveries
}

fn apply_fsevents_replay(
    db: &Db,
    root: &str,
    since: i64,
    replay: &serde_json::Value,
) -> FsRecovery {
    let valid = replay["valid"].as_bool().unwrap_or(false);
    let current = replay["current_id"].as_u64().unwrap_or(since.max(0) as u64);
    let mut paths = replay["events"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|event| event["path"].as_str().map(PathBuf::from))
        .filter(|path| path.starts_with(&root))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    let _ = db.with(|connection| {
        connection.execute(
            "INSERT INTO fs_journal_state
                 (root,last_event_id,history_valid,replay_count,replayed_events,fallback_count,updated_at)
                 VALUES (?1,?2,?3,1,?4,?5,?6)
                 ON CONFLICT(root) DO UPDATE SET
                   history_valid=excluded.history_valid,
                   replay_count=replay_count+1,
                   replayed_events=replayed_events+excluded.replayed_events,
                   fallback_count=fallback_count+excluded.fallback_count,
                   updated_at=excluded.updated_at",
            params![
                root,
                since,
                valid,
                paths.len() as i64,
                if valid { 0 } else { 1 },
                now()
            ],
        )?;
        Ok(())
    });
    crate::security::log_access(
        db,
        "files",
        if valid {
            "fsevents_replay"
        } else {
            "catalog_fallback"
        },
        Some(&format!(
            "root={root};events={};since={since};current={current}",
            paths.len()
        )),
    );
    if valid {
        FsRecovery::Delta {
            paths,
            root: root.to_string(),
            current,
        }
    } else {
        FsRecovery::Fallback(PathBuf::from(root))
    }
}

fn set_fsevents_checkpoint(db: &Db, root: &str, event_id: u64) -> Result<()> {
    db.with(|connection| {
        connection.execute(
            "UPDATE fs_journal_state SET last_event_id=?2,history_valid=1,updated_at=?3
             WHERE root=?1",
            params![root, event_id as i64, now()],
        )?;
        Ok(())
    })
}

/// macOS ne fournit pas d'API permettant d'accorder l'accès complet au disque.
/// Ce test vérifie donc l'accès effectif à plusieurs emplacements protégés par TCC.
/// Il ne se contente jamais de la présence d'un chemin dans la base Syn.
pub fn full_disk_access_granted() -> bool {
    #[cfg(target_os = "macos")]
    {
        let Some(home) = dirs::home_dir() else {
            return false;
        };
        let protected = [
            home.join("Library/Mail"),
            home.join("Library/Messages"),
            home.join("Library/Safari"),
        ];
        let existing: Vec<_> = protected.into_iter().filter(|path| path.exists()).collect();
        !existing.is_empty() && existing.iter().any(|path| std::fs::read_dir(path).is_ok())
    }
    #[cfg(not(target_os = "macos"))]
    false
}

/// Active un unique périmètre utilisateur. Les dossiers techniques, cachés,
/// caches et bibliothèques système restent exclus par `walk_collect`.
/// Retourne `(racine, nouvellement_activée)` pour éviter de relancer un scan en boucle.
pub fn ensure_full_access_scope(db: &Db) -> Result<(String, bool)> {
    let home = dirs::home_dir().ok_or_else(|| {
        crate::error::AppError::NotFound("dossier utilisateur introuvable".into())
    })?;
    let root = home
        .canonicalize()
        .unwrap_or(home)
        .to_string_lossy()
        .to_string();
    let existed = db.read(|c| {
        Ok(c.query_row(
            "SELECT status='active' FROM folders WHERE path=?1",
            rusqlite::params![root],
            |row| row.get::<_, bool>(0),
        )
        .unwrap_or(false))
    })?;
    let prefix = format!(
        "{}{}%",
        root.trim_end_matches(std::path::MAIN_SEPARATOR),
        std::path::MAIN_SEPARATOR
    );
    db.with(|c| {
        // Une seule racine évite les scans doublons comme `~/` + `~/Desktop`.
        c.execute(
            "UPDATE folders SET status='removed' WHERE path LIKE ?1",
            rusqlite::params![prefix],
        )?;
        c.execute(
            "INSERT INTO folders (path, added_at, status) VALUES (?1, ?2, 'active')
             ON CONFLICT(path) DO UPDATE SET status='active'",
            rusqlite::params![root, now()],
        )?;
        Ok(())
    })?;
    Ok((root, !existed))
}

pub fn is_path_in_active_scope(db: &Db, path: &Path) -> Result<bool> {
    let candidate = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    Ok(folder_paths(db)?.iter().any(|folder| {
        let root = Path::new(folder)
            .canonicalize()
            .unwrap_or_else(|_| PathBuf::from(folder));
        candidate.starts_with(root)
    }))
}

fn walk_collect(
    dir: &Path,
    files: &mut Vec<PathBuf>,
    projects: &mut Vec<PathBuf>,
    depth: usize,
    stopping: &AtomicBool,
) {
    if depth > 12 || stopping.load(Ordering::SeqCst) {
        return;
    }
    if depth > 0 && is_project_root(dir) {
        // Le projet reste l'unité de présentation/déplacement, mais ses sources
        // utiles sont aussi indexées comme enfants pour permettre une vraie
        // recherche de code et détecter l'activité récente.
        projects.push(dir.to_path_buf());
        collect_project_files(dir, files, stopping);
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return, // permission refusée → skip + on continue
    };
    for entry in entries.flatten() {
        if stopping.load(Ordering::SeqCst) {
            return;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        // Ne jamais suivre un lien symbolique : il peut sortir du périmètre choisi
        // (ou former une boucle) tout en apparaissant sous ce périmètre.
        if entry.file_type().map(|t| t.is_symlink()).unwrap_or(true) {
            continue;
        }
        if path.is_dir() {
            if !is_excluded_dir(&name) {
                walk_collect(&path, files, projects, depth + 1, stopping);
            }
        } else if !name.starts_with('.') && !is_technical_file(&path) && is_indexable_file(&path) {
            if let Ok(meta) = entry.metadata() {
                if meta.len() <= MAX_FILE_SIZE {
                    files.push(path);
                }
            }
        }
    }
}

/// Insère ou rafraîchit toutes les métadonnées en une transaction. Sur un
/// disque de plusieurs dizaines de milliers de fichiers, cette phase prend des
/// secondes plutôt que les heures nécessaires à extraction + OCR + embeddings.
async fn catalog_metadata(db: &Db, files: &[PathBuf]) -> Result<()> {
    // Des transactions courtes empêchent le catalogue de monopoliser le
    // mutex SQLCipher et laissent les commandes interactives répondre.
    // Lots courts, volontairement. Chaque lot est une transaction : pendant sa
    // durée, aucune autre écriture de Syn ne progresse — y compris celles du
    // tour de conversation en cours. Un lot de 500 fichiers gardait la main
    // assez longtemps pour que la question de l'utilisateur échoue sur un
    // verrou. Le débit global change à peine, la réactivité beaucoup.
    for paths in files.chunks(64) {
        let records = paths
            .iter()
            .filter_map(|path| {
                let Ok(meta) = std::fs::metadata(path) else {
                    return None;
                };
                let source_ref = path.to_string_lossy().to_string();
                let title = path
                    .file_name()
                    .map(|name| name.to_string_lossy().to_string());
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|date| date.duration_since(std::time::UNIX_EPOCH).ok())
                    .map(|duration| duration.as_secs() as i64);
                let priority = enrichment_base_priority(path, mtime);
                Some((source_ref, title, meta.len() as i64, mtime, priority))
            })
            .collect::<Vec<_>>();
        db.with(|connection| {
            let transaction = connection.unchecked_transaction()?;
            {
                let mut update = transaction.prepare(
                    "UPDATE items SET title=?2, path=?1, size=?3, mtime=?4, created_at=?4,
                     ingested_at=?5, status='active',
                     hash=CASE WHEN size IS NOT ?3 OR mtime IS NOT ?4 THEN ?6 ELSE hash END
                     WHERE source='files' AND source_ref=?1",
                )?;
                let mut insert = transaction.prepare(
                    "INSERT INTO items (id,source,source_ref,type,title,body,created_at,ingested_at,
                     hash,path,mime,size,mtime,status)
                     VALUES (?1,'files',?2,'document',?3,NULL,?5,?6,?7,?2,NULL,?4,?5,'active')",
                )?;
                let mut enqueue = transaction.prepare(
                    "INSERT INTO enrichment_queue
                     (item_id,source,source_ref,state,base_priority,extractor_version,updated_at)
                     SELECT id,'files',source_ref,'pending',?2,?3,?4 FROM items
                     WHERE source='files' AND source_ref=?1
                     ON CONFLICT(item_id) DO UPDATE SET
                       source_ref=excluded.source_ref,
                       base_priority=excluded.base_priority,
                       state=CASE
                         WHEN enrichment_queue.extractor_version<>excluded.extractor_version
                           OR (SELECT hash FROM items WHERE id=excluded.item_id) LIKE 'metadata:%'
                         THEN 'pending' ELSE enrichment_queue.state END,
                       embedding_ready=CASE
                         WHEN (SELECT hash FROM items WHERE id=excluded.item_id) LIKE 'metadata:%'
                         THEN 0 ELSE enrichment_queue.embedding_ready END,
                       extractor_version=excluded.extractor_version,
                       updated_at=excluded.updated_at",
                )?;
                for (source_ref, title, size, mtime, priority) in &records {
                    let metadata_hash = format!("metadata:{}:{}", size, mtime.unwrap_or_default());
                    let changed = update.execute(params![
                        source_ref,
                        title,
                        size,
                        mtime,
                        now(),
                        metadata_hash
                    ])?;
                    if changed == 0 {
                        insert.execute(params![
                            crate::db::new_id(),
                            source_ref,
                            title,
                            size,
                            mtime,
                            now(),
                            metadata_hash,
                        ])?;
                    }
                    enqueue.execute(params![source_ref, priority, EXTRACTION_VERSION_ID, now()])?;
                }
            }
            transaction.commit()?;
            Ok(())
        })?;
        tokio::task::yield_now().await;
    }
    Ok(())
}

fn enrichment_base_priority(path: &Path, mtime: Option<i64>) -> f64 {
    let type_score = match document_index_priority(path) {
        0 => 500.0,
        1 => 120.0,
        _ => 260.0,
    };
    let folded = crate::db::fold(&path.to_string_lossy());
    let location_score = if ["/documents/", "/desktop/", "/bureau/"]
        .iter()
        .any(|part| folded.contains(part))
    {
        300.0
    } else if folded.contains("application support") || folded.contains("/library/") {
        -500.0
    } else {
        80.0
    };
    let age_days = mtime
        .map(|value| (now() - value).max(0) as f64 / 86_400.0)
        .unwrap_or(3650.0);
    let recency_score = 400.0 / (1.0 + age_days / 30.0);
    type_score + location_score + recency_score
}

fn prioritize_paths(db: &Db, paths: &[PathBuf]) -> Result<()> {
    db.with(|connection| {
        let mut statement = connection.prepare(
            "UPDATE enrichment_queue SET access_count=access_count+1,
             last_accessed=?2, state=CASE WHEN embedding_ready=1 THEN state ELSE 'pending' END,
             updated_at=?2 WHERE source='files' AND source_ref=?1",
        )?;
        for path in paths {
            statement.execute(params![path.to_string_lossy(), now()])?;
        }
        Ok(())
    })
}

fn next_enrichment_jobs(db: &Db, limit: usize) -> Result<Vec<(String, String, String)>> {
    db.read(|connection| {
        let mut statement = connection.prepare(
            "SELECT item_id,source,source_ref FROM enrichment_queue
             WHERE state IN ('pending','error')
             ORDER BY (base_priority + access_count*1000 +
                       CASE WHEN last_accessed IS NULL THEN 0 ELSE 2000 END - attempts*250) DESC,
                      updated_at ASC LIMIT ?1",
        )?;
        let rows = statement.query_map([limit.max(1) as i64], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?;
        let mut jobs = Vec::new();
        for row in rows {
            jobs.push(row?);
        }
        Ok(jobs)
    })
}

fn mark_enrichment_started(db: &Db, item_id: &str) -> Result<()> {
    db.with(|connection| {
        connection.execute(
            "UPDATE enrichment_queue SET state='processing',attempts=attempts+1,
             updated_at=?2 WHERE item_id=?1",
            params![item_id, now()],
        )?;
        Ok(())
    })
}

fn finish_enrichment(db: &Db, item_id: &str, error: Option<&crate::error::AppError>) -> Result<()> {
    let (has_body, has_vector): (bool, bool) = db.read(|connection| {
        let body = connection
            .query_row(
                "SELECT COALESCE(length(body),0)>0 FROM items WHERE id=?1",
                [item_id],
                |row| row.get(0),
            )
            .unwrap_or(false);
        let vector = connection
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM embeddings WHERE item_id=?1 AND vector IS NOT NULL)",
                [item_id],
                |row| row.get(0),
            )
            .unwrap_or(false);
        Ok((body, vector))
    })?;
    let state = if error.is_some() {
        "error"
    } else if has_vector {
        "embedded"
    } else if has_body {
        "waiting_embedding"
    } else {
        "ineligible"
    };
    db.with(|connection| {
        connection.execute(
            "UPDATE enrichment_queue SET state=?2,lexical_ready=?3,embedding_ready=?4,
             last_error=?5,updated_at=?6 WHERE item_id=?1",
            params![
                item_id,
                state,
                has_body,
                has_vector,
                error.map(ToString::to_string),
                now()
            ],
        )?;
        Ok(())
    })
}

fn coverage(db: &Db) -> Result<(i64, i64, i64, f64, f64)> {
    db.read(|connection| {
        let (eligible, embedded, lexical): (i64, i64, i64) = connection.query_row(
            "SELECT COUNT(*),COALESCE(SUM(embedding_ready),0),COALESCE(SUM(lexical_ready),0)
             FROM enrichment_queue WHERE state NOT IN ('ineligible','removed')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;
        let pct = if eligible == 0 {
            0.0
        } else {
            embedded as f64 * 100.0 / eligible as f64
        };
        let old_high: f64 = connection
            .query_row(
                "SELECT COALESCE(MAX(high_water_pct),0) FROM index_metric_log",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0.0);
        Ok((eligible, embedded, lexical, pct, old_high.max(pct)))
    })
}

fn record_coverage(db: &Db, reason: &str) -> Result<()> {
    let (eligible, embedded, lexical, pct, high) = coverage(db)?;
    db.with(|connection| {
        let unchanged = connection
            .query_row(
                "SELECT eligible_count=?1 AND embedded_count=?2 AND lexical_count=?3
                 FROM index_metric_log ORDER BY id DESC LIMIT 1",
                params![eligible, embedded, lexical],
                |row| row.get::<_, bool>(0),
            )
            .unwrap_or(false);
        if unchanged {
            return Ok(());
        }
        connection.execute(
            "INSERT INTO index_metric_log(recorded_at,eligible_count,embedded_count,
             lexical_count,coverage_pct,high_water_pct,reason) VALUES (?1,?2,?3,?4,?5,?6,?7)",
            params![now(), eligible, embedded, lexical, pct, high, reason],
        )?;
        Ok(())
    })
}

fn collect_project_files(dir: &Path, files: &mut Vec<PathBuf>, stopping: &AtomicBool) {
    const MAX_PROJECT_FILES: usize = 500;
    let mut added = 0;
    for entry in walkdir::WalkDir::new(dir)
        .max_depth(10)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_dir()
                || !is_excluded_dir(&entry.file_name().to_string_lossy())
        })
        .flatten()
    {
        if stopping.load(Ordering::SeqCst) || added >= MAX_PROJECT_FILES {
            break;
        }
        let path = entry.path();
        if entry.file_type().is_file()
            && is_searchable_project_file(path)
            && entry
                .metadata()
                .is_ok_and(|meta| meta.len() <= MAX_FILE_SIZE)
        {
            files.push(path.to_path_buf());
            added += 1;
        }
    }
}

fn is_searchable_project_file(path: &Path) -> bool {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("");
    if name.starts_with('.') || is_technical_file(path) {
        return false;
    }
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .unwrap_or("")
            .to_ascii_lowercase()
            .as_str(),
        "md" | "txt"
            | "toml"
            | "yaml"
            | "yml"
            | "json"
            | "rs"
            | "swift"
            | "m"
            | "mm"
            | "c"
            | "h"
            | "cpp"
            | "go"
            | "java"
            | "kt"
            | "py"
            | "rb"
            | "php"
            | "js"
            | "jsx"
            | "ts"
            | "tsx"
            | "vue"
            | "svelte"
            | "css"
            | "scss"
            | "html"
            | "sql"
            | "sh"
    )
}

fn file_hash(path: &Path, meta: &std::fs::Metadata) -> String {
    // Le contenu participe toujours au hash. Une date/une taille identique ne doit
    // pas masquer une modification réelle d'un gros document.
    let mut hasher = blake3::Hasher::new();
    // Une évolution de l'extracteur (OCR, nouveaux formats…) doit retraiter
    // même un fichier dont les octets n'ont pas changé.
    hasher.update(EXTRACTION_VERSION);
    hasher.update(&meta.len().to_le_bytes());
    if let Ok(modified) = meta.modified() {
        if let Ok(d) = modified.duration_since(std::time::UNIX_EPOCH) {
            hasher.update(&d.as_secs().to_le_bytes());
        }
    }
    if let Ok(mut file) = std::fs::File::open(path) {
        use std::io::Read;
        let mut buf = [0u8; 64 * 1024];
        loop {
            match file.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    hasher.update(&buf[..n]);
                }
            }
        }
    }
    hasher.finalize().to_hex().to_string()
}

pub(crate) async fn index_file(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    bus: &Bus,
    embed_model: &str,
    path: &Path,
) -> Result<()> {
    let source_ref = path.to_string_lossy().to_string();
    let ignored = db.read(|c| {
        Ok(c.query_row(
            "SELECT 1 FROM ignored_items WHERE source='files' AND source_ref=?1",
            rusqlite::params![source_ref],
            |_| Ok(true),
        )
        .unwrap_or(false))
    })?;
    if ignored {
        return Ok(());
    }
    let meta = match std::fs::metadata(path) {
        Ok(m) => m,
        Err(_) => return Ok(()), // disparu en cours de traitement → abandon sans erreur
    };
    // Le hash lit potentiellement plusieurs dizaines de Mo : jamais sur un
    // worker async partagé avec l'IPC/UI.
    let hash_path = path.to_path_buf();
    let hash_meta = meta.clone();
    let hash = tokio::task::spawn_blocking(move || file_hash(&hash_path, &hash_meta))
        .await
        .map_err(|error| crate::error::AppError::Other(format!("hash interrompu : {error}")))?;
    let consent: bool = db
        .read(|c| {
            Ok(c.query_row(
                "SELECT value FROM settings WHERE key='sensitive_consent'",
                [],
                |r| r.get::<_, String>(0),
            )
            .map(|v| v == "true")
            .unwrap_or(false))
        })
        .unwrap_or(false);
    let was_sensitive_metadata_only = db.read(|c| {
        Ok(c.query_row(
            "SELECT type='sensible_non_lu' FROM items WHERE source='files' AND source_ref=?1",
            rusqlite::params![source_ref],
            |r| r.get::<_, bool>(0),
        )
        .unwrap_or(false))
    })?;
    if memory::item_hash(db, "files", &source_ref)?.as_deref() == Some(hash.as_str())
        && !(consent && looks_sensitive(path) && was_sensitive_metadata_only)
    {
        return Ok(()); // connu et inchangé → skip (incrémental)
    }

    let mtime = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64);
    let title = path.file_name().map(|n| n.to_string_lossy().to_string());

    // Gate de lecture des sensibles (Média §B8) : sans consentement, métadonnées seules.
    let sensitive = looks_sensitive(path);

    let extracted = if sensitive && !consent {
        extract::Extracted {
            text: None,
            kind: "sensible_non_lu",
            mime: "application/octet-stream".into(),
        }
    } else {
        // Extraction potentiellement lourde → hors du fil d'exécution async.
        let p = path.to_path_buf();
        tokio::task::spawn_blocking(move || extract::extract(&p))
            .await
            .unwrap_or(extract::Extracted {
                text: None,
                kind: "other",
                mime: "application/octet-stream".into(),
            })
    };

    let in_code_project = is_project_content(path);
    let item = Item {
        id: String::new(),
        source: "files".into(),
        source_ref: source_ref.clone(),
        r#type: if in_code_project {
            "code"
        } else {
            match extracted.kind {
                "photo" => "photo",
                "code" => "code",
                "sensible_non_lu" => "sensible_non_lu",
                _ => "document",
            }
        }
        .into(),
        title,
        body: extracted.text.clone(),
        created_at: mtime,
        ingested_at: now(),
        hash: Some(hash),
        path: Some(source_ref.clone()),
        mime: Some(extracted.mime),
        size: Some(meta.len() as i64),
        mtime,
        status: "active".into(),
    };
    ingestion::ingest_item(db, llm, bus, embed_model, item, extracted.text.as_deref()).await?;
    // Le contenu d'un document reste une source de contexte non fiable. On ne
    // transforme jamais automatiquement ses TODO/phrases en engagements.
    Ok(())
}

/// Le projet fournit l'entité racine (README + arborescence + activité). Les
/// sources sont indexées séparément pour la recherche, sans changer l'unité de
/// déplacement utilisée par les outils de rangement.
#[allow(dead_code)]
async fn index_project(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    bus: &Bus,
    embed_model: &str,
    dir: &Path,
) -> Result<()> {
    let source_ref = dir.to_string_lossy().to_string();
    let ignored = db.read(|c| {
        Ok(c.query_row(
            "SELECT 1 FROM ignored_items WHERE source='files' AND source_ref=?1",
            rusqlite::params![source_ref],
            |_| Ok(true),
        )
        .unwrap_or(false))
    })?;
    if ignored {
        return Ok(());
    }
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

    let mut body = format!("Projet : {name}\nChemin : {source_ref}\n");
    for readme in ["README.md", "README.txt", "readme.md", "README"] {
        let p = dir.join(readme);
        if p.exists() {
            if let Ok(text) = std::fs::read_to_string(&p) {
                body.push_str("\n— README —\n");
                body.push_str(&text.chars().take(8000).collect::<String>());
                break;
            }
        }
    }
    body.push_str("\n— Fichiers —\n");
    let mut count = 0;
    for entry in walkdir::WalkDir::new(dir)
        .max_depth(3)
        .into_iter()
        .flatten()
    {
        let rel = entry.path().strip_prefix(dir).unwrap_or(entry.path());
        let s = rel.to_string_lossy();
        if s.is_empty()
            || rel
                .components()
                .any(|c| is_excluded_dir(&c.as_os_str().to_string_lossy()))
        {
            continue;
        }
        body.push_str(&s);
        body.push('\n');
        count += 1;
        if count >= 200 {
            body.push_str("…\n");
            break;
        }
    }

    let mtime = walkdir::WalkDir::new(dir)
        .max_depth(10)
        .into_iter()
        .filter_entry(|entry| {
            entry.depth() == 0
                || !entry.file_type().is_dir()
                || !is_excluded_dir(&entry.file_name().to_string_lossy())
        })
        .flatten()
        .take(2_000)
        .filter_map(|entry| entry.metadata().ok()?.modified().ok())
        .filter_map(|modified| modified.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs() as i64)
        .max();
    body.push_str(&format!(
        "\nDernière activité : {}\n",
        mtime.unwrap_or_default()
    ));
    let hash = blake3::hash(body.as_bytes()).to_hex().to_string();
    if memory::item_hash(db, "files", &source_ref)?.as_deref() == Some(hash.as_str()) {
        return Ok(());
    }
    let item = Item {
        id: String::new(),
        source: "files".into(),
        source_ref: source_ref.clone(),
        r#type: "code_project".into(),
        title: Some(name),
        body: Some(body.clone()),
        created_at: mtime,
        ingested_at: now(),
        hash: Some(hash),
        path: Some(source_ref),
        mime: Some("inode/directory".into()),
        size: None,
        mtime,
        status: "active".into(),
    };
    ingestion::ingest_item(db, llm, bus, embed_model, item, Some(&body)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db(label: &str) -> (Db, PathBuf) {
        let root = std::env::temp_dir().join(format!("syn-{label}-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&root).unwrap();
        let db = Db::open(&root.join("test.db"), &"1".repeat(64)).unwrap();
        (db, root)
    }

    #[test]
    fn exclusions_minecraft_et_caches() {
        assert!(is_excluded_dir("node_modules"));
        assert!(is_excluded_dir(".git"));
        assert!(is_excluded_dir("minecraft"));
        assert!(is_excluded_dir("resourcepacks"));
        assert!(is_excluded_dir("Library"));
        assert!(is_excluded_dir("MonApp.app"));
        assert!(!is_excluded_dir("Documents"));
        assert!(!is_excluded_dir("Projets"));
    }

    #[test]
    fn detection_sensible() {
        assert!(looks_sensitive(Path::new(
            "/Users/x/Documents/bulletin_salaire_mars.pdf"
        )));
        assert!(looks_sensitive(Path::new(
            "/Users/x/Impôts/avis-imposition-2025.pdf"
        )));
        assert!(!looks_sensitive(Path::new(
            "/Users/x/Documents/recette_tarte.md"
        )));
    }

    #[test]
    fn exclusions_fichiers_techniques() {
        assert!(is_technical_file(Path::new("/Music/Library.musicdb")));
        assert!(is_technical_file(Path::new("/App/cache.sqlite-wal")));
        assert!(!is_technical_file(Path::new("/Documents/rapport.pdf")));
    }

    #[test]
    fn les_sources_utiles_des_projets_restent_recherchables() {
        assert!(is_searchable_project_file(Path::new("/Projet/src/main.rs")));
        assert!(is_searchable_project_file(Path::new("/Projet/README.md")));
        assert!(is_searchable_project_file(Path::new("/Projet/App.vue")));
        assert!(!is_searchable_project_file(Path::new("/Projet/Cargo.lock")));
        assert!(!is_searchable_project_file(Path::new("/Projet/image.png")));
    }

    #[test]
    fn reconnait_un_fichier_comme_contenu_de_projet() {
        let root = std::env::temp_dir().join(format!("syn-project-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("Cargo.toml"), "[package]").unwrap();
        assert!(is_project_content(&root.join("README.md")));
        assert!(is_project_content(&root.join("src/main.rs")));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn la_recherche_directe_retrouve_un_fichier_pas_encore_indexe() {
        let root = std::env::temp_dir().join(format!("syn-live-{}", uuid::Uuid::new_v4()));
        let folder = root.join("Archives administratives");
        std::fs::create_dir_all(&folder).unwrap();
        let expected = folder.join("Attestation_assurance_2025.pdf");
        std::fs::write(&expected, b"pdf de test").unwrap();
        let results = live_metadata_search(
            &[root.to_string_lossy().to_string()],
            &["attestation".into(), "assurance".into()],
            8,
        );
        assert_eq!(results.len(), 1, "{results:#?}");
        assert_eq!(results[0].source_ref, expected.to_string_lossy());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn la_file_couvre_tout_le_corpus_sans_plafond_300() {
        let (db, root) = test_db("queue-all");
        db.with(|connection| {
            for index in 0..750 {
                connection.execute(
                    "INSERT INTO items(id,source,source_ref,type,title,ingested_at,status)
                     VALUES (?1,'files',?2,'document',?1,0,'active')",
                    params![format!("i{index}"), format!("/Documents/{index}.txt")],
                )?;
                connection.execute(
                    "INSERT INTO enrichment_queue(item_id,source,source_ref,state,base_priority,updated_at)
                     VALUES (?1,'files',?2,'pending',1,0)",
                    params![format!("i{index}"), format!("/Documents/{index}.txt")],
                )?;
            }
            Ok(())
        }).unwrap();
        assert_eq!(next_enrichment_jobs(&db, 1_000).unwrap().len(), 750);
        assert_eq!(coverage(&db).unwrap().0, 750);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn la_couverture_expose_un_high_water_monotone() {
        let (db, root) = test_db("coverage");
        db.with(|connection| {
            for index in 0..4 {
                connection.execute(
                    "INSERT INTO enrichment_queue(item_id,source,source_ref,state,base_priority,embedding_ready,lexical_ready,updated_at)
                     VALUES (?1,'files',?2,?3,1,?4,1,0)",
                    params![format!("i{index}"), format!("/{index}"), if index < 2 { "embedded" } else { "pending" }, index < 2],
                )?;
            }
            Ok(())
        }).unwrap();
        record_coverage(&db, "half").unwrap();
        db.with(|connection| {
            connection.execute(
                "INSERT INTO enrichment_queue(item_id,source,source_ref,state,base_priority,updated_at)
                 VALUES ('new','files','/new','pending',1,0)", [],)?;
            Ok(())
        }).unwrap();
        let (_, _, _, raw, high) = coverage(&db).unwrap();
        assert!(raw < high);
        assert_eq!(high, 50.0);
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn un_replay_valide_ne_declenche_aucun_rescan_complet() {
        let (db, root_dir) = test_db("fsevents-delta");
        let root = root_dir.to_string_lossy().to_string();
        let changed = root_dir.join("ancien.txt");
        let recovery = apply_fsevents_replay(
            &db,
            &root,
            41,
            &serde_json::json!({
                "valid": true,
                "current_id": 44,
                "events": [{"path": changed}]
            }),
        );
        assert!(
            matches!(recovery, FsRecovery::Delta { paths, current: 44, .. } if paths.len() == 1)
        );
        let counters: (i64, i64, i64, i64) = db.read(|connection| {
            connection.query_row(
                "SELECT replay_count,replayed_events,full_scan_count,last_event_id FROM fs_journal_state WHERE root=?1",
                [&root],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            ).map_err(Into::into)
        }).unwrap();
        assert_eq!(
            counters,
            (1, 1, 0, 41),
            "le curseur ne doit pas avancer avant l'écriture du delta"
        );
        set_fsevents_checkpoint(&db, &root, 44).unwrap();
        let stored: i64 = db
            .read(|connection| {
                connection
                    .query_row(
                        "SELECT last_event_id FROM fs_journal_state WHERE root=?1",
                        [&root],
                        |row| row.get(0),
                    )
                    .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(stored, 44);
        let _ = std::fs::remove_dir_all(root_dir);
    }

    #[tokio::test]
    async fn le_watcher_reprogramme_uniquement_un_fichier_modifie() {
        let (db, root) = test_db("watcher-delta");
        let path = root.join("ancien.txt");
        std::fs::write(&path, "version une").unwrap();
        catalog_metadata(&db, std::slice::from_ref(&path))
            .await
            .unwrap();
        db.with(|connection| {
            connection.execute(
                "UPDATE items SET hash='contenu-complet' WHERE source_ref=?1",
                [path.to_string_lossy().to_string()],
            )?;
            connection.execute(
                "UPDATE enrichment_queue SET state='embedded',embedding_ready=1",
                [],
            )?;
            Ok(())
        })
        .unwrap();
        std::fs::write(&path, "version deux sensiblement plus longue").unwrap();
        catalog_metadata(&db, std::slice::from_ref(&path))
            .await
            .unwrap();
        let state: (String, bool) = db
            .read(|connection| {
                connection
                    .query_row(
                        "SELECT state,embedding_ready FROM enrichment_queue",
                        [],
                        |row| Ok((row.get(0)?, row.get(1)?)),
                    )
                    .map_err(Into::into)
            })
            .unwrap();
        assert_eq!(state, ("pending".into(), false));
        let _ = std::fs::remove_dir_all(root);
    }
}
