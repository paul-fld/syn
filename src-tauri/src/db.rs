use crate::error::{AppError, Result};
use rusqlite::Connection;
use std::path::Path;
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

/// Connexion SQLite chiffrée (SQLCipher AES-256). La vérité est dans les sources ;
/// l'index est dérivé et reconstructible (doc maître §8).
#[derive(Clone)]
pub struct Db {
    conn: Arc<Mutex<Connection>>,
}

impl Db {
    /// Ouvre (ou crée) la base avec la clé maîtresse (hex 64 chars).
    pub fn open(path: &Path, key_hex: &str) -> Result<Self> {
        let conn = Connection::open(path)?;
        // Clé brute : PRAGMA key = "x'...'" évite la re-dérivation PBKDF2 de SQLCipher.
        conn.pragma_update(None, "key", format!("x'{}'", key_hex))?;
        // Vérifie que la clé est bonne (échoue sinon).
        conn.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
            .map_err(|_| AppError::Security("Mot de passe maître incorrect.".into()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
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
        let db = Db {
            conn: Arc::new(Mutex::new(conn)),
        };
        db.migrate()?;
        Ok(db)
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

    /// Section critique courte sur la connexion.
    pub fn with<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let guard = self
            .conn
            .lock()
            .map_err(|_| AppError::Other("verrou base empoisonné".into()))?;
        f(&guard)
    }

    /// Change la clé de chiffrement (changement de mot de passe maître).
    pub fn rekey(&self, new_key_hex: &str) -> Result<()> {
        self.with(|c| {
            c.pragma_update(None, "rekey", format!("x'{}'", new_key_hex))?;
            Ok(())
        })
    }
}

pub fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}
