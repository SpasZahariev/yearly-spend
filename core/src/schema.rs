pub const ALL_TABLES: &[&str] = &[
    "accounts",
    "categories",
    "transactions",
    "transfer_groups",
    "transfer_review",
    "fx_rates",
    "ingested_files",
    "llm_calls",
];

const SCHEMA: &str = r#"
CREATE SEQUENCE IF NOT EXISTS accounts_id_seq START 1;
CREATE SEQUENCE IF NOT EXISTS categories_id_seq START 1;
CREATE SEQUENCE IF NOT EXISTS transfer_groups_id_seq START 1;
CREATE SEQUENCE IF NOT EXISTS transactions_id_seq START 1;
CREATE SEQUENCE IF NOT EXISTS ingested_files_id_seq START 1;
CREATE SEQUENCE IF NOT EXISTS llm_calls_id_seq START 1;

CREATE TABLE IF NOT EXISTS accounts (
    id INTEGER PRIMARY KEY DEFAULT nextval('accounts_id_seq'),
    source VARCHAR,
    name VARCHAR NOT NULL UNIQUE,
    currency VARCHAR,
    is_internal BOOLEAN NOT NULL DEFAULT FALSE
);

CREATE TABLE IF NOT EXISTS categories (
    id INTEGER PRIMARY KEY DEFAULT nextval('categories_id_seq'),
    name VARCHAR NOT NULL UNIQUE,
    color VARCHAR NOT NULL
);

CREATE TABLE IF NOT EXISTS transfer_groups (
    id INTEGER PRIMARY KEY DEFAULT nextval('transfer_groups_id_seq'),
    from_account_id INTEGER NOT NULL REFERENCES accounts(id),
    to_account_id INTEGER NOT NULL REFERENCES accounts(id),
    amount_chf DOUBLE NOT NULL,
    dt DATE NOT NULL
);

CREATE TABLE IF NOT EXISTS transactions (
    id INTEGER PRIMARY KEY DEFAULT nextval('transactions_id_seq'),
    account_id INTEGER NOT NULL REFERENCES accounts(id),
    source VARCHAR NOT NULL,
    source_key VARCHAR NOT NULL,
    dt DATE NOT NULL,
    ts TIMESTAMP,
    description VARCHAR NOT NULL,
    subject VARCHAR,
    category_id INTEGER REFERENCES categories(id),
    amount_orig DOUBLE NOT NULL,
    currency_orig VARCHAR NOT NULL,
    amount_chf DOUBLE NOT NULL,
    kind VARCHAR NOT NULL CHECK (kind IN ('spend', 'income', 'transfer_out', 'transfer_in', 'internal')),
    transfer_group_id INTEGER REFERENCES transfer_groups(id),
    file_sha VARCHAR,
    ingested_at TIMESTAMP NOT NULL DEFAULT current_timestamp,
    UNIQUE (source, source_key)
);

-- Transfer legs whose LLM review finished. Legs left unpaired by the review
-- are not re-reviewed on later runs; a new deterministic partner can still
-- pair them without an LLM call. No REFERENCES clause: DuckDB forbids
-- updating rows of a table that some other table references, and the
-- pairing pass updates transactions.
CREATE TABLE IF NOT EXISTS transfer_review (
    tx_id INTEGER PRIMARY KEY,
    reviewed_at TIMESTAMP NOT NULL DEFAULT current_timestamp
);

CREATE TABLE IF NOT EXISTS fx_rates (
    month DATE NOT NULL,
    from_ccy VARCHAR NOT NULL,
    to_ccy VARCHAR NOT NULL,
    rate DOUBLE NOT NULL,
    UNIQUE (month, from_ccy, to_ccy)
);

CREATE TABLE IF NOT EXISTS ingested_files (
    id INTEGER PRIMARY KEY DEFAULT nextval('ingested_files_id_seq'),
    source VARCHAR NOT NULL,
    file_sha256 VARCHAR NOT NULL,
    path VARCHAR,
    ingested_at TIMESTAMP NOT NULL DEFAULT current_timestamp,
    rows INTEGER,
    UNIQUE (source, file_sha256)
);

CREATE TABLE IF NOT EXISTS llm_calls (
    id INTEGER PRIMARY KEY DEFAULT nextval('llm_calls_id_seq'),
    context VARCHAR NOT NULL,
    phase VARCHAR NOT NULL,
    attempt INTEGER NOT NULL DEFAULT 1,
    ok BOOLEAN NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT current_timestamp
);
"#;

/// The fixed categorization taxonomy the LLM is constrained to.
pub const CATEGORIES: &[(&str, &str)] = &[
    ("income", "#22c55e"),
    ("invest", "#3b82f6"),
    ("transfer", "#94a3b8"),
    ("housing", "#f59e0b"),
    ("food", "#ef4444"),
    ("transport", "#0ea5e9"),
    ("travel", "#8b5cf6"),
    ("entertainment", "#ec4899"),
    ("health", "#10b981"),
    ("shopping", "#f97316"),
    ("subscriptions", "#6366f1"),
    ("groceries", "#84cc16"),
    ("dining", "#fb7185"),
    ("utilities", "#eab308"),
    ("education", "#14b8a6"),
    ("pets", "#a3e635"),
    ("fees", "#64748b"),
    ("uncategorized", "#78716c"),
];

const ACCOUNTS: &[(Option<&str>, &str, Option<&str>, bool)] = &[
    (Some("neon"), "Neon", Some("CHF"), false),
    (Some("revolut"), "Revolut", None, false),
    (Some("cashback"), "Swisscard Cashback", Some("CHF"), false),
    (None, "Interactive Brokers", Some("CHF"), true),
];

pub fn migrate(conn: &duckdb::Connection) -> anyhow::Result<()> {
    conn.execute_batch(SCHEMA)?;
    seed(conn)?;
    Ok(())
}

fn seed(conn: &duckdb::Connection) -> anyhow::Result<()> {
    let mut sql = String::from("INSERT INTO categories (name, color) VALUES");
    let mut first = true;
    for (name, color) in CATEGORIES {
        if !first {
            sql.push(',');
        }
        first = false;
        sql.push_str(&format!(" ('{name}', '{color}')"));
    }
    sql.push_str(" ON CONFLICT (name) DO NOTHING;\n");

    for (source, name, currency, is_internal) in ACCOUNTS {
        sql.push_str(&format!(
            "INSERT INTO accounts (source, name, currency, is_internal) VALUES ({src}, '{name}', {cur}, {internal}) ON CONFLICT (name) DO NOTHING;\n",
            src = source.map(|s| format!("'{s}'")).unwrap_or_else(|| "NULL".into()),
            cur = currency.map(|c| format!("'{c}'")).unwrap_or_else(|| "NULL".into()),
            internal = if *is_internal { "TRUE" } else { "FALSE" }
        ));
    }

    conn.execute_batch(&sql)?;
    Ok(())
}

pub fn assert_initialized(conn: &duckdb::Connection) -> anyhow::Result<()> {
    let mut stmt =
        conn.prepare("SELECT table_name FROM duckdb_tables() WHERE schema_name NOT LIKE 'tmp%'")?;
    let names: Vec<String> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<duckdb::Result<Vec<_>>>()?;
    let missing: Vec<&str> = ALL_TABLES
        .iter()
        .copied()
        .filter(|t| !names.iter().any(|n| n == t))
        .collect();
    anyhow::ensure!(
        missing.is_empty(),
        "database is not initialized (missing tables: {}); run the ingest CLI first",
        missing.join(", ")
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_db() -> (std::path::PathBuf, duckdb::Connection) {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let path =
            std::env::temp_dir().join(format!("spend-test-{}-{}.duckdb", std::process::id(), n));
        let _ = std::fs::remove_file(&path);
        let conn = duckdb::Connection::open(&path).unwrap();
        (path, conn)
    }

    fn cleanup(path: &std::path::Path) {
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn migrate_creates_all_tables() {
        let (path, conn) = temp_db();
        migrate(&conn).unwrap();
        let mut stmt = conn
            .prepare("SELECT table_name FROM duckdb_tables() WHERE schema_name NOT LIKE 'tmp%'")
            .unwrap();
        let names: Vec<String> = stmt
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap();
        for expected in ALL_TABLES {
            assert!(
                names.iter().any(|n| n == expected),
                "missing table {expected}"
            );
        }
        cleanup(&path);
    }

    #[test]
    fn migrate_generates_ids_for_seeded_and_future_rows() {
        let (path, conn) = temp_db();
        migrate(&conn).unwrap();

        let account_ids: Vec<i64> = conn
            .prepare("SELECT id FROM accounts ORDER BY id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(account_ids, vec![1, 2, 3, 4]);

        conn.execute(
            "INSERT INTO accounts (source, name, currency, is_internal)
             VALUES ('test', 'Test account', 'CHF', FALSE)",
            [],
        )
        .unwrap();
        let id: i64 = conn
            .query_row(
                "SELECT id FROM accounts WHERE name = 'Test account'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(id, 5);

        cleanup(&path);
    }

    #[test]
    fn migrate_seeds_taxonomy_and_accounts_idempotently() {
        let (path, conn) = temp_db();
        migrate(&conn).unwrap();
        migrate(&conn).unwrap();

        let count = |sql: &str| -> i64 { conn.query_row(sql, [], |row| row.get(0)).unwrap() };
        assert_eq!(count("SELECT count(*) FROM categories"), 18);
        assert_eq!(count("SELECT count(*) FROM accounts"), 4);
        assert_eq!(count("SELECT count(*) FROM transactions"), 0);
        cleanup(&path);
    }

    #[test]
    fn assert_initialized_passes_after_migrate_and_fails_on_empty() {
        let (path, conn) = temp_db();
        let empty = duckdb::Connection::open_in_memory().unwrap();
        assert!(assert_initialized(&empty).is_err());
        migrate(&conn).unwrap();
        assert!(assert_initialized(&conn).is_ok());
        cleanup(&path);
    }
}
