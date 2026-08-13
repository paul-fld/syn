pub mod egress;
pub mod keys;
pub mod provenance;

use crate::db::{new_id, now, Db};
use crate::error::Result;

/// Journal d'accès (invariant : chaque lecture de connecteur est tracée).
pub fn log_access(db: &Db, connector: &str, operation: &str, item_ref: Option<&str>) {
    let _ = db.with(|c| {
        c.execute(
            "INSERT INTO access_log (id, connector, operation, item_ref, created_at) VALUES (?1,?2,?3,?4,?5)",
            rusqlite::params![new_id(), connector, operation, item_ref, now()],
        )?;
        Ok(())
    });
}

pub fn _noop() -> Result<()> {
    Ok(())
}
