use std::path::Path;

use duckdb::{AccessMode, Config, Connection};

use crate::schema;

pub fn ingest_connection(path: &Path) -> anyhow::Result<Connection> {
    create_parent(path)?;
    let conn = Connection::open(path)?;
    schema::migrate(&conn)?;
    Ok(conn)
}

/// The API opens a short-lived read-write connection to persist inline
/// overrides (category / transfer flag edits from the dashboard). The file
/// lock is released as soon as the connection drops, so `spend ingest` can
/// still open the database while the API is idle. The schema is ensured
/// idempotently so a PATCH against a fresh checkout cannot fail on missing
/// tables.
pub fn api_write_connection(path: &Path) -> anyhow::Result<Connection> {
    create_parent(path)?;
    let conn = Connection::open(path)?;
    schema::migrate(&conn)?;
    Ok(conn)
}

/// The API process only needs read-only access, but a fresh checkout has no
/// database file yet, so it bootstraps the schema once when missing.
pub fn api_connection(path: &Path) -> anyhow::Result<Connection> {
    if !path.exists() {
        create_parent(path)?;
        let conn = Connection::open(path)?;
        schema::migrate(&conn)?;
    }
    let config = Config::default().access_mode(AccessMode::ReadOnly)?;
    let conn = Connection::open_with_flags(path, config)?;
    schema::assert_initialized(&conn)?;
    Ok(conn)
}

fn create_parent(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ingest_connection_creates_parent_and_migrates() {
        let dir = std::env::temp_dir().join(format!("spend-db-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("nested/deep/spend.duckdb");
        let conn = ingest_connection(&path).unwrap();
        schema::assert_initialized(&conn).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn api_connection_bootstraps_then_reads() {
        let dir = std::env::temp_dir().join(format!("spend-api-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join("spend.duckdb");

        let conn = api_connection(&path).unwrap();
        let n: i64 = conn
            .query_row("SELECT count(*) FROM categories", [], |row| row.get(0))
            .unwrap();
        assert_eq!(n, 18);

        let conn2 = api_connection(&path).unwrap();
        let _: i64 = conn2
            .query_row("SELECT count(*) FROM categories", [], |row| row.get(0))
            .unwrap();

        let _ = std::fs::remove_dir_all(&dir);
    }
}
