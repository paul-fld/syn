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
const EXTRACTION_VERSION: &[u8] = b"files-v3-ocr-project-domain";
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
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Default)]
pub struct IndexStatus {
    pub running: bool,
    pub done: u64,
    pub total: u64,
    pub current: Option<String>,
    pub items_count: i64,
    pub pending_embeddings: i64,
    pub sensitive_skipped: i64,
    pub unreadable_files: i64,
    pub folders: Vec<FolderStatus>,
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
        let worker = tauri::async_runtime::spawn(async move {
            while let Some(job) = rx.recv().await {
                if ix.stopping.load(Ordering::SeqCst) || matches!(job, IndexJob::Shutdown) {
                    break;
                }
                ix.running.store(true, Ordering::SeqCst);
                let result = match job {
                    IndexJob::FullScan(one) => {
                        ix.full_scan(&db, &llm, &bus, &embed_model, one).await
                    }
                    IndexJob::Paths(paths) => {
                        ix.incremental(&db, &llm, &bus, &embed_model, paths).await
                    }
                    IndexJob::Shutdown => break,
                };
                if let Err(e) = result {
                    bus.emit(BusEvent::FilesError {
                        path: String::new(),
                        reason: e.to_string(),
                    });
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
        llm: &Arc<dyn LlmClient>,
        bus: &Bus,
        embed_model: &str,
        only: Option<PathBuf>,
    ) -> Result<()> {
        let folders = match only {
            Some(p) => vec![p.to_string_lossy().to_string()],
            None => folder_paths(db)?,
        };
        crate::security::log_access(db, "files", "full_scan", None);

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

        // Les documents personnels sont utiles immédiatement. Les images,
        // médias et projets de développement passent ensuite.
        files.sort_by_key(|path| document_index_priority(path));

        // 2. Traitement incrémental, throttlé, reprenable (checkpoint = upsert par fichier).
        for file in files {
            if self.stopping.load(Ordering::SeqCst) {
                break;
            }
            if !is_path_in_active_scope(db, &file)? {
                continue;
            }
            self.set_current(&file);
            if let Err(e) = index_file(db, llm, bus, embed_model, &file).await {
                bus.emit(BusEvent::FilesError {
                    path: file.to_string_lossy().into(),
                    reason: e.to_string(),
                });
            }
            self.tick(bus).await;
        }
        for project in projects {
            if self.stopping.load(Ordering::SeqCst) {
                break;
            }
            if !is_path_in_active_scope(db, &project)? {
                continue;
            }
            self.set_current(&project);
            if let Err(e) = index_project(db, llm, bus, embed_model, &project).await {
                bus.emit(BusEvent::FilesError {
                    path: project.to_string_lossy().into(),
                    reason: e.to_string(),
                });
            }
            self.tick(bus).await;
        }

        for folder in &folders {
            db.with(|c| {
                c.execute(
                    "UPDATE folders SET last_indexed=?2 WHERE path=?1",
                    params![folder, now()],
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
        Ok(())
    }

    async fn incremental(
        &self,
        db: &Db,
        llm: &Arc<dyn LlmClient>,
        bus: &Bus,
        embed_model: &str,
        paths: Vec<PathBuf>,
    ) -> Result<()> {
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
                continue;
            }
            if path.is_dir() {
                continue;
            }
            self.set_current(&path);
            if let Err(e) = index_file(db, llm, bus, embed_model, &path).await {
                bus.emit(BusEvent::FilesError {
                    path: path.to_string_lossy().into(),
                    reason: e.to_string(),
                });
            }
            self.tick(bus).await;
        }
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
        let (items_count, pending, sensitive, unreadable) = db.with(|c| {
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
        let folders = db.with(|c| {
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
        Ok(IndexStatus {
            running: self.running.load(Ordering::SeqCst),
            done: self.done.load(Ordering::SeqCst),
            total: self.total.load(Ordering::SeqCst),
            current: self.current.lock().unwrap().clone(),
            items_count,
            pending_embeddings: pending,
            sensitive_skipped: sensitive,
            unreadable_files: unreadable,
            folders,
        })
    }
}

fn reconcile_missing_files(db: &Db, folders: &[String]) -> Result<usize> {
    let indexed: Vec<String> = db.with(|c| {
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
            removed += 1;
        }
    }
    Ok(removed)
}

pub fn folder_paths(db: &Db) -> Result<Vec<String>> {
    db.with(|c| {
        let mut stmt = c.prepare("SELECT path FROM folders WHERE status='active'")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        let mut out = vec![];
        for r in rows {
            out.push(r?);
        }
        Ok(out)
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
    let existed = db.with(|c| {
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

async fn index_file(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    bus: &Bus,
    embed_model: &str,
    path: &Path,
) -> Result<()> {
    let source_ref = path.to_string_lossy().to_string();
    let ignored = db.with(|c| {
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
    let hash = file_hash(path, &meta);
    let consent: bool = db
        .with(|c| {
            Ok(c.query_row(
                "SELECT value FROM settings WHERE key='sensitive_consent'",
                [],
                |r| r.get::<_, String>(0),
            )
            .map(|v| v == "true")
            .unwrap_or(false))
        })
        .unwrap_or(false);
    let was_sensitive_metadata_only = db.with(|c| {
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
async fn index_project(
    db: &Db,
    llm: &Arc<dyn LlmClient>,
    bus: &Bus,
    embed_model: &str,
    dir: &Path,
) -> Result<()> {
    let source_ref = dir.to_string_lossy().to_string();
    let ignored = db.with(|c| {
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
}
