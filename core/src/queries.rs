use std::collections::HashMap;

use chrono::Datelike;
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
    pub month: Option<u32>,
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
pub struct YearlySpend {
    pub year: i32,
    pub spend: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct CumulativePoint {
    pub month: u32,
    pub cumulative: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DailySpend {
    pub day: u32,
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

/// Income, spend (excl. transfer_out), moved-between-accounts, and net for a
/// year or a single month of it. Amounts are returned as positive magnitudes in CHF.
pub fn summary(conn: &Connection, year: i32, month: Option<u32>) -> anyhow::Result<Summary> {
    let (income, spend, moved): (f64, f64, f64) = conn.query_row(
        "SELECT
                COALESCE(sum(amount_chf) FILTER (kind = 'income'), 0),
                COALESCE(sum(-amount_chf) FILTER (kind = 'spend'), 0),
                COALESCE(sum(-amount_chf) FILTER (kind = 'transfer_out'), 0)
             FROM transactions
             WHERE year(dt) = ? AND (? IS NULL OR month(dt) = ?)",
        duckdb::params![year, month, month],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?;
    let income = cents(income);
    let spend = cents(spend);
    let moved = cents(moved);
    Ok(Summary {
        year,
        month,
        income,
        spend,
        moved,
        net: cents(income - spend),
    })
}

/// Total spend per year for every year that has transactions, oldest first.
pub fn yearly_spend(conn: &Connection) -> anyhow::Result<Vec<YearlySpend>> {
    let mut stmt = conn.prepare(
        "SELECT year(dt),
                COALESCE(sum(-amount_chf) FILTER (kind = 'spend'), 0)
         FROM transactions
         GROUP BY 1
         ORDER BY 1",
    )?;
    let rows: Vec<(i32, f64)> = stmt
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
        .collect::<duckdb::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|(year, spend)| YearlySpend {
            year,
            spend: cents(spend),
        })
        .collect())
}

/// Running spend within a year; all 12 months are always present.
pub fn cumulative_spend(conn: &Connection, year: i32) -> anyhow::Result<Vec<CumulativePoint>> {
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

    let mut running = 0.0;
    Ok((1..=12)
        .map(|month| {
            running += by_month.get(&month).copied().unwrap_or(0.0);
            CumulativePoint {
                month,
                cumulative: cents(running),
            }
        })
        .collect())
}

/// Spend per day for a month; every day of the month is always present.
pub fn daily_spend(conn: &Connection, year: i32, month: u32) -> anyhow::Result<Vec<DailySpend>> {
    let days_in_month = chrono::NaiveDate::from_ymd_opt(year, month, 1)
        .ok_or_else(|| anyhow::anyhow!("invalid month {month} in {year}"))?
        .num_days_in_month() as u32;
    let mut stmt = conn.prepare(
        "SELECT day(dt), sum(-amount_chf)
         FROM transactions
         WHERE year(dt) = ? AND month(dt) = ? AND kind = 'spend'
         GROUP BY 1",
    )?;
    let by_day: HashMap<u32, f64> = stmt
        .query_map(duckdb::params![year, month], |row| {
            let day: i32 = row.get(0)?;
            let spend: f64 = row.get(1)?;
            Ok((day as u32, spend))
        })?
        .collect::<duckdb::Result<HashMap<_, _>>>()?;

    Ok((1..=days_in_month)
        .map(|day| DailySpend {
            day,
            spend: cents(by_day.get(&day).copied().unwrap_or(0.0)),
        })
        .collect())
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

/// Spend broken down by category for a year or a single month of it, largest first.
pub fn category_breakdown(
    conn: &Connection,
    year: i32,
    month: Option<u32>,
) -> anyhow::Result<Vec<CategorySlice>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(c.name, 'uncategorized') AS name,
                COALESCE(c.color, '#78716c') AS color,
                sum(-t.amount_chf) AS total
         FROM transactions t
         LEFT JOIN categories c ON c.id = t.category_id
         WHERE t.kind = 'spend' AND year(t.dt) = ? AND (? IS NULL OR month(t.dt) = ?)
         GROUP BY 1, 2
         ORDER BY 3 DESC",
    )?;
    let rows: Vec<(String, String, f64)> = stmt
        .query_map(duckdb::params![year, month, month], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?))
        })?
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
        let got = summary(&conn, 2025, None).unwrap();
        assert_eq!(got.year, 2025);
        assert_eq!(got.month, None);
        assert_eq!(got.income, 3000.0);
        assert_eq!(got.spend, 425.75);
        assert_eq!(got.moved, 50.0);
        assert_eq!(got.net, 2574.25);

        let empty = summary(&conn, 2023, None).unwrap();
        assert_eq!(empty.income, 0.0);
        assert_eq!(empty.spend, 0.0);
        assert_eq!(empty.moved, 0.0);
        assert_eq!(empty.net, 0.0);
    }

    #[test]
    fn summary_scoped_to_a_single_month() {
        let conn = seeded();
        insert_sample(&conn);
        let jan = summary(&conn, 2025, Some(1)).unwrap();
        assert_eq!(jan.year, 2025);
        assert_eq!(jan.month, Some(1));
        assert_eq!(jan.income, 1000.0);
        assert_eq!(jan.spend, 100.0);
        assert_eq!(jan.moved, 50.0);
        assert_eq!(jan.net, 900.0);

        let dec = summary(&conn, 2025, Some(12)).unwrap();
        assert_eq!(dec.income, 0.0);
        assert_eq!(dec.spend, 75.25);
        assert_eq!(dec.moved, 0.0);
        assert_eq!(dec.net, -75.25);

        let empty = summary(&conn, 2025, Some(3)).unwrap();
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
        let slices = category_breakdown(&conn, 2025, None).unwrap();
        assert_eq!(
            slices
                .iter()
                .map(|s| (s.name.as_str(), s.value, s.percentage))
                .collect::<Vec<_>>(),
            vec![("travel", 250.5, 58.84), ("food", 175.25, 41.16),]
        );
        assert_eq!(slices[0].color, "#8b5cf6");
        assert_eq!(slices[1].color, "#ef4444");

        assert!(category_breakdown(&conn, 2023, None).unwrap().is_empty());
    }

    #[test]
    fn category_breakdown_scoped_to_a_single_month() {
        let conn = seeded();
        insert_sample(&conn);
        let feb = category_breakdown(&conn, 2025, Some(2)).unwrap();
        assert_eq!(
            feb.iter()
                .map(|s| (s.name.as_str(), s.value, s.percentage))
                .collect::<Vec<_>>(),
            vec![("travel", 250.5, 100.0),]
        );
        assert!(category_breakdown(&conn, 2025, Some(3)).unwrap().is_empty());
    }

    #[test]
    fn yearly_spend_covers_every_year_with_data_oldest_first() {
        let conn = seeded();
        insert_sample(&conn);
        let got = yearly_spend(&conn).unwrap();
        assert_eq!(
            got.iter().map(|p| (p.year, p.spend)).collect::<Vec<_>>(),
            vec![(2024, 999.0), (2025, 425.75)]
        );

        let empty = yearly_spend(&seeded()).unwrap();
        assert!(empty.is_empty());
    }

    #[test]
    fn cumulative_spend_runs_up_within_the_year() {
        let conn = seeded();
        insert_sample(&conn);
        let got = cumulative_spend(&conn, 2025).unwrap();
        assert_eq!(got.len(), 12);
        assert_eq!(
            got.iter()
                .map(|p| (p.month, p.cumulative))
                .collect::<Vec<_>>(),
            vec![
                (1, 100.0),
                (2, 350.5),
                (3, 350.5),
                (4, 350.5),
                (5, 350.5),
                (6, 350.5),
                (7, 350.5),
                (8, 350.5),
                (9, 350.5),
                (10, 350.5),
                (11, 350.5),
                (12, 425.75)
            ]
        );

        let empty = cumulative_spend(&conn, 2023).unwrap();
        assert_eq!(empty.len(), 12);
        assert!(empty.iter().all(|p| p.cumulative == 0.0));
    }

    #[test]
    fn daily_spend_covers_every_day_of_the_month() {
        let conn = seeded();
        insert_sample(&conn);
        let jan = daily_spend(&conn, 2025, 1).unwrap();
        assert_eq!(jan.len(), 31);
        assert_eq!(
            jan.iter()
                .filter(|p| p.spend > 0.0)
                .map(|p| (p.day, p.spend))
                .collect::<Vec<_>>(),
            vec![(5, 100.0)]
        );
        assert_eq!(jan[11].spend, 0.0);

        let feb = daily_spend(&conn, 2025, 2).unwrap();
        assert_eq!(feb.len(), 28);
        assert_eq!(feb[13].spend, 250.5);

        // Leap-year February has 29 days; the 2024 spend lands on day 15.
        let feb_2024 = daily_spend(&conn, 2024, 2).unwrap();
        assert_eq!(feb_2024.len(), 29);
        assert!(feb_2024.iter().all(|p| p.spend == 0.0));
        let jun_2024 = daily_spend(&conn, 2024, 6).unwrap();
        assert_eq!(jun_2024.len(), 30);
        assert_eq!(jun_2024[14].spend, 999.0);
    }
}
