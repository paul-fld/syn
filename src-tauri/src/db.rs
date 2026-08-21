use crate::error::{AppError, Result};
use rusqlite::Connection;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

const MIGRATIONS: &[(&str, &str)] = &[
    ("0001_init", include_str!("../../migrations/0001_init.sql")),
    (
        "0002_provenance_cleanup",
        include_str!("../../migrations/0002_provenance_cleanup.sql"),
    ),
    (
        "0003_conversation_projects",
        include_str!("../../migrations/0003_conversation_projects.sql"),
    ),
    (
        "0004_search_and_senses",
        include_str!("../../migrations/0004_search_and_senses.sql"),
    ),
    (
        "0005_full_text_search",
        include_str!("../../migrations/0005_full_text_search.sql"),
    ),
    (
        "0006_connector_event_upserts",
        include_str!("../../migrations/0006_connector_event_upserts.sql"),
    ),
    (
        "0007_progressive_indexing",
        include_str!("../../migrations/0007_progressive_indexing.sql"),
    ),
    (
        "0008_mail_compositions",
        include_str!("../../migrations/0008_mail_compositions.sql"),
    ),
    (
        "0009_mail_draft_validation",
        include_str!("../../migrations/0009_mail_draft_validation.sql"),
    ),
    (
        "0010_mail_recipient_source",
        include_str!("../../migrations/0010_mail_recipient_source.sql"),
    ),
    (
        "0011_session_documents",
        include_str!("../../migrations/0011_session_documents.sql"),
    ),
    (
        "0012_memory_graph",
        include_str!("../../migrations/0012_memory_graph.sql"),
    ),
    (
        "0013_language",
        include_str!("../../migrations/0013_language.sql"),
    ),
    (
        "0014_mail_cleanup",
        include_str!("../../migrations/0014_mail_cleanup.sql"),
    ),
    (
        "0015_mail_cleanup_audit_v2",
        include_str!("../../migrations/0015_mail_cleanup_audit_v2.sql"),
    ),
    (
        "0016_connector_bootstrap",
        include_str!("../../migrations/0016_connector_bootstrap.sql"),
    ),
];

/// Normalisation de recherche : minuscules + suppression des diacritiques
/// français. Exposée à SQLite sous le nom `syn_fold` pour des LIKE
/// insensibles aux accents et à la casse.
pub fn fold(s: &str) -> String {
    s.chars()
        .flat_map(|c| {
            let lower = c.to_lowercase().next().unwrap_or(c);
            let mapped = match lower {
                'à' | 'â' | 'ä' | 'á' | 'ã' => 'a',
                'é' | 'è' | 'ê' | 'ë' => 'e',
                'î' | 'ï' | 'í' | 'ì' => 'i',
                'ô' | 'ö' | 'ó' | 'ò' | 'õ' => 'o',
                'ù' | 'û' | 'ü' | 'ú' => 'u',
                'ç' => 'c',
                'ÿ' | 'ý' => 'y',
                'ñ' => 'n',
                'œ' => 'o', // approximation : "œuvre" → "oeuvre" est géré par le doublage ci-dessous
                'æ' => 'a',
                other => other,
            };
            if lower == 'œ' {
                vec!['o', 'e']
            } else if lower == 'æ' {
                vec!['a', 'e']
            } else {
                vec![mapped]
            }
        })
        .collect()
}

/// Connexions de LECTURE. En WAL, elles avancent sur le dernier instantané
/// validé pendant que l'écrivain travaille : c'est ce qui permet à l'interface
/// de répondre pendant l'indexation.
const READERS: usize = 3;

/// Connexion SQLite chiffrée (SQLCipher AES-256). La vérité est dans les sources ;
/// l'index est dérivé et reconstructible (doc maître §8).
///
/// **Un écrivain, plusieurs lecteurs.** Une connexion unique derrière un mutex
/// sérialisait toute l'application : pendant l'indexation de démarrage, chaque
/// appel de l'interface attendait la fin du travail de fond — c'était le gel au
/// lancement. Mais ouvrir plusieurs connexions ÉCRIVANTES échange ce gel contre
/// pire : deux écrivains se disputent le verrou du fichier et SQLite renvoie
/// « database is locked », que `busy_timeout` ne rattrape pas toujours (une
/// transaction différée qui passe en écriture échoue immédiatement).
///
/// D'où cette forme : les écritures restent sérialisées EN INTERNE sur une
/// connexion unique — exactement la garantie d'avant, donc aucun conflit
/// possible entre les connexions de Syn — tandis que les lectures se répartissent
/// sur un bassin séparé et n'attendent plus personne.
#[derive(Clone)]
pub struct Db {
    writer: Arc<Mutex<Connection>>,
    readers: Arc<Vec<Mutex<Connection>>>,
    /// Prochain lecteur à tenter : évite que tout le monde se presse sur le
    /// premier du bassin.
    next: Arc<AtomicUsize>,
    path: Arc<std::path::PathBuf>,
}

impl Db {
    /// Ouvre (ou crée) la base avec la clé maîtresse (hex 64 chars).
    pub fn open(path: &Path, key_hex: &str) -> Result<Self> {
        let writer = Self::connect(path, key_hex)?;
        let mut readers = Vec::with_capacity(READERS);
        for _ in 0..READERS {
            readers.push(Mutex::new(Self::connect(path, key_hex)?));
        }
        let db = Db {
            writer: Arc::new(Mutex::new(writer)),
            readers: Arc::new(readers),
            next: Arc::new(AtomicUsize::new(0)),
            path: Arc::new(path.to_path_buf()),
        };
        db.migrate()?;
        Ok(db)
    }

    fn connect(path: &Path, key_hex: &str) -> Result<Connection> {
        let conn = Connection::open(path)?;
        // Clé brute : PRAGMA key = "x'...'" évite la re-dérivation PBKDF2 de SQLCipher.
        conn.pragma_update(None, "key", format!("x'{}'", key_hex))?;
        // Vérifie que la clé est bonne (échoue sinon).
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
            .map_err(|_| AppError::Security("Mot de passe maître incorrect.".into()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        // Deux écrivains ne peuvent pas progresser ensemble : plutôt que de
        // remonter une erreur « database is locked » à l'utilisateur, on attend
        // brièvement. La contention réelle se compte en millisecondes.
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        conn.create_scalar_function(
            "syn_fold",
            1,
            rusqlite::functions::FunctionFlags::SQLITE_UTF8
                | rusqlite::functions::FunctionFlags::SQLITE_DETERMINISTIC,
            |ctx| {
                let s: String = ctx.get(0)?;
                Ok(fold(&s))
            },
        )?;
        Ok(conn)
    }

    fn migrate(&self) -> Result<()> {
        self.with(|c| {
            c.execute_batch("CREATE TABLE IF NOT EXISTS _migrations (name TEXT PRIMARY KEY, applied_at INTEGER)")?;
            for (name, sql) in MIGRATIONS {
                let done: bool = c
                    .query_row("SELECT 1 FROM _migrations WHERE name = ?1", [name], |_| Ok(true))
                    .unwrap_or(false);
                if !done {
                    c.execute_batch(sql)?;
                    c.execute(
                        "INSERT INTO _migrations (name, applied_at) VALUES (?1, strftime('%s','now'))",
                        [name],
                    )?;
                }
            }
            Ok(())
        })
    }

    /// Section critique sur la connexion d'écriture. C'est le défaut : toute
    /// opération qui pourrait écrire passe par ici, et donc se sérialise avec
    /// les autres écritures de Syn sans jamais provoquer de conflit SQLite.
    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let guard = self
            .writer
            .lock()
            .map_err(|_| AppError::Other("verrou base empoisonné".into()))?;
        f(&guard)
    }

    /// Lecture seule, sur une connexion libre du bassin.
    ///
    /// À n'utiliser que pour des closures qui ne font que des `SELECT` : une
    /// écriture passée par ici entrerait en concurrence avec l'écrivain et
    /// referait apparaître « database is locked ».
    ///
    /// On cherche d'abord un lecteur disponible sans attendre : une requête de
    /// l'interface n'a aucune raison de faire la queue alors que deux autres
    /// connexions dorment.
    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let start = self.next.fetch_add(1, Ordering::Relaxed) % self.readers.len();
        for offset in 0..self.readers.len() {
            let index = (start + offset) % self.readers.len();
            if let Ok(guard) = self.readers[index].try_lock() {
                return f(&guard);
            }
        }
        let guard = self.readers[start]
            .lock()
            .map_err(|_| AppError::Other("verrou base empoisonné".into()))?;
        f(&guard)
    }

    /// Change la clé de chiffrement (changement de mot de passe maître).
    ///
    /// Le rekey s'applique au FICHIER : les lecteurs, encore ouverts avec
    /// l'ancienne clé, deviennent inutilisables. On les rouvre donc tous avec la
    /// nouvelle clé avant de rendre la main.
    pub fn rekey(&self, new_key_hex: &str) -> Result<()> {
        let writer = self
            .writer
            .lock()
            .map_err(|_| AppError::Other("verrou base empoisonné".into()))?;
        let mut guards = Vec::with_capacity(self.readers.len());
        for slot in self.readers.iter() {
            guards.push(
                slot.lock()
                    .map_err(|_| AppError::Other("verrou base empoisonné".into()))?,
            );
        }
        writer.pragma_update(None, "rekey", format!("x'{}'", new_key_hex))?;
        for guard in guards.iter_mut() {
            **guard = Self::connect(&self.path, new_key_hex)?;
        }
        Ok(())
    }
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Régression du 17/08/2026 : « database is locked » affiché à l'utilisateur.
    ///
    /// Le bassin de connexions introduit pour supprimer le gel au lancement avait
    /// créé plusieurs écrivains concurrents. L'indexeur enchaîne des transactions
    /// courtes sans relâche ; un second écrivain se faisait affamer, `busy_timeout`
    /// expirait, et l'échec remontait jusqu'au fil de conversation.
    ///
    /// Le test reproduit ce régime : écriture soutenue d'un côté, tour de
    /// conversation et lectures de l'autre. Aucune des deux ne doit échouer.
    #[test]
    fn une_indexation_soutenue_ne_bloque_ni_les_lectures_ni_le_tour_de_conversation() {
        let dir = std::env::temp_dir().join(format!("syn-db-lock-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let db = Db::open(&dir.join("t.db"), &"8".repeat(64)).unwrap();

        let indexeur = db.clone();
        let indexation = std::thread::spawn(move || {
            // Même forme que `catalog_metadata` : des lots successifs, chacun
            // dans sa transaction, sans pause entre eux.
            for lot in 0..200 {
                indexeur
                    .with(|connection| {
                        let transaction = connection.unchecked_transaction()?;
                        for index in 0..64 {
                            transaction.execute(
                                "INSERT INTO items(id,source,source_ref,type,title,ingested_at,status)
                                 VALUES (?1,'files',?2,'document','Archive',1,'active')",
                                rusqlite::params![
                                    format!("w{lot}-{index}"),
                                    format!("/tmp/f{lot}-{index}")
                                ],
                            )?;
                        }
                        transaction.commit()?;
                        Ok(())
                    })
                    .expect("l'indexation ne doit pas échouer");
            }
        });

        // Pendant tout ce temps : le tour de conversation écrit, et l'interface lit.
        let mut tours = 0;
        while !indexation.is_finished() {
            db.with(|connection| {
                connection.execute(
                    "INSERT INTO sessions(id,title,created_at,updated_at)
                     VALUES (?1,'Question',1,1)",
                    rusqlite::params![format!("s{tours}")],
                )?;
                Ok(())
            })
            .expect("le tour de conversation ne doit jamais buter sur un verrou");
            db.read(|connection| {
                Ok(connection.query_row("SELECT COUNT(*) FROM items", [], |row| {
                    row.get::<_, i64>(0)
                })?)
            })
            .expect("une lecture ne doit jamais échouer pendant l'indexation");
            tours += 1;
        }
        indexation.join().unwrap();
        assert!(
            tours > 0,
            "l'indexation a été trop rapide pour que le test conclue"
        );

        let _ = std::fs::remove_dir_all(dir);
    }
}
