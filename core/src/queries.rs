use duckdb::Connection;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Account {
    pub id: i64,
    pub source: Option<String>,
    pub name: String,
    pub currency: Option<String>,
    pub is_internal: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct Category {
    pub id: i64,
    pub name: String,
    pub color: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct Period {
    pub year: i32,
    pub month: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Meta {
    pub accounts: Vec<Account>,
    pub categories: Vec<Category>,
    pub periods: Vec<Period>,
}

pub fn meta(conn: &Connection) -> anyhow::Result<Meta> {
    let mut stmt =
        conn.prepare("SELECT id, source, name, currency, is_internal FROM accounts ORDER BY id")?;
    let accounts: Vec<Account> = stmt
        .query_map([], |row| {
            Ok(Account {
                id: row.get(0)?,
                source: row.get(1)?,
                name: row.get(2)?,
                currency: row.get(3)?,
                is_internal: row.get(4)?,
            })
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;

    let mut stmt = conn.prepare("SELECT id, name, color FROM categories ORDER BY id")?;
    let categories: Vec<Category> = stmt
        .query_map([], |row| {
            Ok(Category {
                id: row.get(0)?,
                name: row.get(1)?,
                color: row.get(2)?,
            })
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;

    let mut stmt =
        conn.prepare("SELECT DISTINCT year(dt), month(dt) FROM transactions ORDER BY 1, 2")?;
    let periods: Vec<Period> = stmt
        .query_map([], |row| {
            let year: i32 = row.get(0)?;
            let month: i32 = row.get(1)?;
            Ok(Period {
                year,
                month: month as u32,
            })
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;

    Ok(Meta {
        accounts,
        categories,
        periods,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema;

    #[test]
    fn meta_reports_seeded_accounts_and_empty_periods() {
        let conn = Connection::open_in_memory().unwrap();
        schema::migrate(&conn).unwrap();
        let meta = meta(&conn).unwrap();
        assert_eq!(meta.accounts.len(), 4);
        assert_eq!(meta.categories.len(), 18);
        assert!(meta.periods.is_empty());
    }
}
