use std::collections::HashMap;

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

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub year: i32,
    pub income: f64,
    pub spend: f64,
    pub moved: f64,
    pub net: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct MonthlySpend {
    pub month: u32,
    pub spend: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CategorySlice {
    pub name: String,
    pub color: String,
    pub value: f64,
    pub percentage: f64,
}

fn cents(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Income, spend (excl. transfer_out), moved-between-accounts, and net for a year.
/// Amounts are returned as positive magnitudes in CHF.
pub fn summary(conn: &Connection, year: i32) -> anyhow::Result<Summary> {
    let (income, spend, moved): (f64, f64, f64) = conn.query_row(
        "SELECT
                COALESCE(sum(amount_chf) FILTER (kind = 'income'), 0),
                COALESCE(sum(-amount_chf) FILTER (kind = 'spend'), 0),
                COALESCE(sum(-amount_chf) FILTER (kind = 'transfer_out'), 0)
             FROM transactions
             WHERE year(dt) = ?",
        duckdb::params![year],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let income = cents(income);
    let spend = cents(spend);
    let moved = cents(moved);
    Ok(Summary {
        year,
        income,
        spend,
        moved,
        net: cents(income - spend),
    })
}

/// Spend per month for a year; all 12 months are always present.
pub fn monthly_spend(conn: &Connection, year: i32) -> anyhow::Result<Vec<MonthlySpend>> {
    let mut stmt = conn.prepare(
        "SELECT month(dt), sum(-amount_chf)
         FROM transactions
         WHERE year(dt) = ? AND kind = 'spend'
         GROUP BY 1",
    )?;
    let by_month: HashMap<u32, f64> = stmt
        .query_map([year], |row| {
            let month: i32 = row.get(0)?;
            let spend: f64 = row.get(1)?;
            Ok((month as u32, spend))
        })?
        .collect::<duckdb::Result<HashMap<_, _>>>()?;

    Ok((1..=12)
        .map(|month| MonthlySpend {
            month,
            spend: cents(by_month.get(&month).copied().unwrap_or(0.0)),
        })
        .collect())
}

/// Spend broken down by category for a year, largest first.
pub fn category_breakdown(conn: &Connection, year: i32) -> anyhow::Result<Vec<CategorySlice>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(c.name, 'uncategorized') AS name,
                COALESCE(c.color, '#78716c') AS color,
                sum(-t.amount_chf) AS total
         FROM transactions t
         LEFT JOIN categories c ON c.id = t.category_id
         WHERE t.kind = 'spend' AND year(t.dt) = ?
         GROUP BY 1, 2
         ORDER BY 3 DESC",
    )?;
    let rows: Vec<(String, String, f64)> = stmt
        .query_map([year], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<duckdb::Result<Vec<_>>>()?;
    let total: f64 = rows.iter().map(|(_, _, v)| *v).sum();
    Ok(rows
        .into_iter()
        .map(|(name, color, value)| {
            let value = cents(value);
            let percentage = if total > 0.0 {
                cents(value / total * 100.0)
            } else {
                0.0
            };
            CategorySlice {
                name,
                color,
                value,
                percentage,
            }
        })
        .collect())
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

    fn seeded() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        schema::migrate(&conn).unwrap();
        conn
    }

    /// One spend, one income, and one transfer_out per month of 2025, plus a
    /// spend row in 2024 that must be excluded from 2025 results.
    fn insert_sample(conn: &Connection) {
        let rows = [
            ("2025-01-05", "food", -100.0, "spend"),
            ("2025-01-20", "income", 1000.0, "income"),
            ("2025-01-30", "transfer", -50.0, "transfer_out"),
            ("2025-02-14", "travel", -250.5, "spend"),
            ("2025-02-28", "income", 2000.0, "income"),
            ("2025-12-31", "food", -75.25, "spend"),
            ("2024-06-15", "food", -999.0, "spend"),
        ];
        for (dt, category, amount, kind) in rows {
            let category_id: i64 = conn
                .query_row(
                    "SELECT id FROM categories WHERE name = ?",
                    [category],
                    |row| row.get(0),
                )
                .unwrap();
            conn.execute(
                "INSERT INTO transactions
                    (account_id, source, source_key, dt, description, category_id,
                     amount_orig, currency_orig, amount_chf, kind)
                 VALUES (1, 'test', ?, ?, 'x', ?, ?, 'CHF', ?, ?)",
                duckdb::params![dt, dt, category_id, amount, amount, kind],
            )
            .unwrap();
        }
    }

    #[test]
    fn meta_reports_seeded_accounts_and_empty_periods() {
        let conn = seeded();
        let meta = meta(&conn).unwrap();
        assert_eq!(meta.accounts.len(), 4);
        assert_eq!(meta.categories.len(), 18);
        assert!(meta.periods.is_empty());
    }

    #[test]
    fn summary_aggregates_year_and_excludes_other_years() {
        let conn = seeded();
        insert_sample(&conn);
        let got = summary(&conn, 2025).unwrap();
        assert_eq!(got.year, 2025);
        assert_eq!(got.income, 3000.0);
        assert_eq!(got.spend, 425.75);
        assert_eq!(got.moved, 50.0);
        assert_eq!(got.net, 2574.25);

        let empty = summary(&conn, 2023).unwrap();
        assert_eq!(empty.income, 0.0);
        assert_eq!(empty.spend, 0.0);
        assert_eq!(empty.moved, 0.0);
        assert_eq!(empty.net, 0.0);
    }

    #[test]
    fn monthly_spend_always_returns_twelve_months() {
        let conn = seeded();
        insert_sample(&conn);
        let months = monthly_spend(&conn, 2025).unwrap();
        assert_eq!(months.len(), 12);
        assert_eq!(
            months.iter().map(|m| m.month).collect::<Vec<_>>(),
            (1..=12).collect::<Vec<_>>()
        );
        assert_eq!(months[0].spend, 100.0);
        assert_eq!(months[1].spend, 250.5);
        assert_eq!(months[11].spend, 75.25);
        assert!(
            months[2..11].iter().all(|m| m.spend == 0.0),
            "months without data must be zero"
        );

        let empty = monthly_spend(&conn, 2023).unwrap();
        assert_eq!(empty.len(), 12);
        assert!(empty.iter().all(|m| m.spend == 0.0));
    }

    #[test]
    fn category_breakdown_orders_by_value_and_computes_percentages() {
        let conn = seeded();
        insert_sample(&conn);
        let slices = category_breakdown(&conn, 2025).unwrap();
        assert_eq!(
            slices
                .iter()
                .map(|s| (s.name.as_str(), s.value, s.percentage))
                .collect::<Vec<_>>(),
            vec![("travel", 250.5, 58.84), ("food", 175.25, 41.16),]
        );
        assert_eq!(slices[0].color, "#8b5cf6");
        assert_eq!(slices[1].color, "#ef4444");

        assert!(category_breakdown(&conn, 2023).unwrap().is_empty());
    }
}
