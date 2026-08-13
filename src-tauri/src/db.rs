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
];

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
