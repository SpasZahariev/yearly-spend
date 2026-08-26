use std::collections::{HashMap, HashSet};

use chrono::{Datelike, NaiveDate};
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

#[derive(Debug, Clone, Serialize)]
pub struct SankeyNode {
    pub id: String,
    pub label: String,
    pub color: String,
    pub column: u8,
    pub value: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct SankeyLink {
    pub source: String,
    pub target: String,
    pub value: f64,
    pub color: String,
    pub kind: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct SankeyData {
    pub year: i32,
    pub month: Option<u32>,
    pub nodes: Vec<SankeyNode>,
    pub links: Vec<SankeyLink>,
}

fn sankey_sort_value(column: u8, incoming: f64, outgoing: f64) -> f64 {
    if column == 2 { incoming } else { outgoing }
}

fn cents(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// Income, spend (excluding transfer legs), moved, and net for a year or a
/// single month of it. Amounts are returned as positive magnitudes in CHF.
/// `moved` is the sum of paired cross-account transfers (`transfer_groups`);
/// unpaired transfer-flagged legs count as nothing.
pub fn summary(conn: &Connection, year: i32, month: Option<u32>) -> anyhow::Result<Summary> {
    let (income, spend, moved): (f64, f64, f64) = conn.query_row(
        "SELECT
                COALESCE(sum(amount_chf) FILTER (kind = 'income'), 0),
                COALESCE(sum(-amount_chf) FILTER (kind = 'spend'), 0),
                (SELECT COALESCE(sum(g.amount_chf), 0)
                   FROM transfer_groups g
                  WHERE year(g.dt) = ? AND (? IS NULL OR month(g.dt) = ?))
             FROM transactions
             WHERE year(dt) = ? AND (? IS NULL OR month(dt) = ?)",
        duckdb::params![year, month, month, year, month, month],
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

/// Spend flows from accounts to categories, plus paired transfers from one
/// account to another. Accounts that receive paired transfers are placed
/// downstream from their funding accounts. Internal bookkeeping rows and
/// unpaired transfer legs are intentionally excluded because they do not
/// identify a destination.
pub fn sankey(conn: &Connection, year: i32, month: Option<u32>) -> anyhow::Result<SankeyData> {
    let mut nodes = HashMap::<String, SankeyNode>::new();
    let mut links = Vec::new();
    let mut transfer_sources = HashSet::new();
    let mut transfer_targets = HashSet::new();

    let mut spend_stmt = conn.prepare(
        "SELECT t.account_id,
                a.name,
                COALESCE(c.name, 'uncategorized') AS category,
                COALESCE(c.color, '#78716c') AS color,
                sum(-t.amount_chf) AS value
         FROM transactions t
         JOIN accounts a ON a.id = t.account_id
         LEFT JOIN categories c ON c.id = t.category_id
         WHERE t.kind = 'spend'
           AND year(t.dt) = ?
           AND (? IS NULL OR month(t.dt) = ?)
         GROUP BY 1, 2, 3, 4
         HAVING sum(-t.amount_chf) > 0
         ORDER BY 5 DESC",
    )?;
    let spend_rows: Vec<(i64, String, String, String, f64)> = spend_stmt
        .query_map(duckdb::params![year, month, month], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;

    for (account_id, account_name, category_name, category_color, value) in spend_rows {
        let source = format!("account:{account_id}");
        let target = format!("category:{category_name}");
        add_sankey_node(
            &mut nodes,
            &source,
            &account_name,
            account_color(account_id),
            0,
        );
        add_sankey_node(&mut nodes, &target, &category_name, &category_color, 2);
        links.push(SankeyLink {
            source,
            target,
            value: cents(value),
            color: category_color,
            kind: "spend".into(),
        });
    }

    let mut transfer_stmt = conn.prepare(
        "SELECT g.from_account_id,
                from_account.name,
                g.to_account_id,
                to_account.name,
                sum(g.amount_chf) AS value
         FROM transfer_groups g
         JOIN accounts from_account ON from_account.id = g.from_account_id
         JOIN accounts to_account ON to_account.id = g.to_account_id
         WHERE year(g.dt) = ?
           AND (? IS NULL OR month(g.dt) = ?)
         GROUP BY 1, 2, 3, 4
         HAVING sum(g.amount_chf) > 0
         ORDER BY 5 DESC",
    )?;
    let transfer_rows: Vec<(i64, String, i64, String, f64)> = transfer_stmt
        .query_map(duckdb::params![year, month, month], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?
        .collect::<duckdb::Result<Vec<_>>>()?;

    for (from_id, from_name, to_id, to_name, value) in transfer_rows {
        let source = format!("account:{from_id}");
        let target = format!("account:{to_id}");
        add_sankey_node(&mut nodes, &source, &from_name, account_color(from_id), 0);
        add_sankey_node(&mut nodes, &target, &to_name, account_color(to_id), 1);
        transfer_sources.insert(from_id);
        transfer_targets.insert(to_id);
        links.push(SankeyLink {
            source,
            target,
            value: cents(value),
            color: transfer_color(to_id).into(),
            kind: "transfer".into(),
        });
    }

    for (account_id, node) in nodes.iter_mut().filter_map(|(id, node)| {
        id.strip_prefix("account:")
            .and_then(|value| value.parse::<i64>().ok())
            .map(|account_id| (account_id, node))
    }) {
        node.column =
            if transfer_targets.contains(&account_id) && !transfer_sources.contains(&account_id) {
                1
            } else {
                0
            };
    }

    let mut nodes: Vec<SankeyNode> = nodes.into_values().collect();
    let mut node_totals = HashMap::<String, (f64, f64)>::new();
    for link in &links {
        let source = node_totals.entry(link.source.clone()).or_insert((0.0, 0.0));
        source.1 += link.value;
        let target = node_totals.entry(link.target.clone()).or_insert((0.0, 0.0));
        target.0 += link.value;
    }
    for node in &mut nodes {
        let (incoming, outgoing) = node_totals.get(&node.id).copied().unwrap_or((0.0, 0.0));
        node.value = cents(incoming.max(outgoing));
    }
    nodes.sort_by(|a, b| {
        let (a_incoming, a_outgoing) = node_totals.get(&a.id).copied().unwrap_or((0.0, 0.0));
        let (b_incoming, b_outgoing) = node_totals.get(&b.id).copied().unwrap_or((0.0, 0.0));
        a.column
            .cmp(&b.column)
            .then_with(|| {
                sankey_sort_value(b.column, b_incoming, b_outgoing)
                    .total_cmp(&sankey_sort_value(a.column, a_incoming, a_outgoing))
            })
            .then_with(|| b.value.total_cmp(&a.value))
            .then_with(|| a.label.cmp(&b.label))
    });

    Ok(SankeyData {
        year,
        month,
        nodes,
        links,
    })
}

fn add_sankey_node(
    nodes: &mut HashMap<String, SankeyNode>,
    id: &str,
    label: &str,
    color: &str,
    column: u8,
) {
    nodes.entry(id.to_string()).or_insert_with(|| SankeyNode {
        id: id.to_string(),
        label: label.to_string(),
        color: color.to_string(),
        column,
        value: 0.0,
    });
}

fn account_color(id: i64) -> &'static str {
    match id {
        1 => "#f50db4",
        2 => "#111111",
        3 => "#0ea5e9",
        _ => "#8b5cf6",
    }
}

fn transfer_color(target_account_id: i64) -> &'static str {
    match target_account_id {
        1 => "#d946ef",
        2 => "#a855f7",
        3 => "#06b6d4",
        _ => "#a855f7",
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Transaction {
    pub id: i64,
    pub dt: String,
    pub description: String,
    pub subject: Option<String>,
    pub source: String,
    pub account: String,
    pub amount_chf: f64,
    pub currency_orig: String,
    pub amount_orig: f64,
    pub kind: String,
    pub is_transfer: bool,
    pub category: Option<Category>,
}

/// Optional filters for the transaction list. `None` fields are unconstrained;
/// a `category` of `"uncategorized"` matches rows with no category.
#[derive(Debug, Clone, Default)]
pub struct TransactionFilters {
    pub year: Option<i32>,
    pub month: Option<u32>,
    pub source: Option<String>,
    pub category: Option<String>,
}

/// The `WHERE` clause (positional `?` placeholders) plus the matching bound
/// values, in order.
fn transaction_where_params(filters: &TransactionFilters) -> (String, Vec<Box<dyn duckdb::ToSql>>) {
    let mut where_clause = String::from(" WHERE 1 = 1");
    let mut params: Vec<Box<dyn duckdb::ToSql>> = Vec::new();
    if let Some(year) = filters.year {
        where_clause.push_str(" AND year(t.dt) = ?");
        params.push(Box::new(year));
    }
    if let Some(month) = filters.month {
        where_clause.push_str(" AND month(t.dt) = ?");
        params.push(Box::new(month as i32));
    }
    if let Some(source) = &filters.source {
        where_clause.push_str(" AND t.source = ?");
        params.push(Box::new(source.clone()));
    }
    if let Some(category) = &filters.category {
        where_clause.push_str(" AND (? = 'uncategorized' AND t.category_id IS NULL OR c.name = ?)");
        params.push(Box::new(category.clone()));
        params.push(Box::new(category.clone()));
    }
    (where_clause, params)
}

fn row_to_transaction(row: &duckdb::Row<'_>) -> duckdb::Result<Transaction> {
    let dt: NaiveDate = row.get(1)?;
    let kind: String = row.get(9)?;
    Ok(Transaction {
        id: row.get(0)?,
        dt: dt.format("%Y-%m-%d").to_string(),
        description: row.get(2)?,
        subject: row.get(3)?,
        source: row.get(4)?,
        account: row.get(5)?,
        amount_chf: row.get(6)?,
        currency_orig: row.get(7)?,
        amount_orig: row.get(8)?,
        is_transfer: kind == "transfer_out" || kind == "transfer_in",
        kind,
        category: match (
            row.get::<_, Option<i64>>(10)?,
            row.get::<_, Option<String>>(11)?,
            row.get::<_, Option<String>>(12)?,
        ) {
            (Some(id), Some(name), Some(color)) => Some(Category { id, name, color }),
            _ => None,
        },
    })
}

const TRANSACTION_SELECT: &str = "SELECT t.id, t.dt, t.description, t.subject, t.source, a.name, t.amount_chf, t.currency_orig, t.amount_orig, t.kind, c.id, c.name, c.color FROM transactions t JOIN accounts a ON a.id = t.account_id LEFT JOIN categories c ON c.id = t.category_id";

/// One page of transactions, newest first, with the total filtered count.
/// `page` is 1-based; `page_size` is clamped by the caller.
pub fn list_transactions(
    conn: &Connection,
    filters: &TransactionFilters,
    page: u32,
    page_size: u32,
) -> anyhow::Result<(Vec<Transaction>, i64)> {
    let (where_clause, mut values) = transaction_where_params(filters);
    values.push(Box::new(page_size as i32));
    let offset = (page.saturating_sub(1) as i32) * (page_size as i32);
    values.push(Box::new(offset));
    let params: Vec<&dyn duckdb::ToSql> = values.iter().map(|v| v.as_ref()).collect();

    let count_sql = format!(
        "SELECT count(*) FROM transactions t LEFT JOIN categories c ON c.id = t.category_id{where_clause}"
    );
    let total: i64 = conn.query_row(&count_sql, &params[..params.len() - 2], |row| row.get(0))?;

    let list_sql = format!(
        "{TRANSACTION_SELECT}{where_clause} ORDER BY t.dt DESC, t.id DESC LIMIT ? OFFSET ?"
    );
    let mut stmt = conn.prepare(&list_sql)?;
    let rows = stmt.query_map(params.as_slice(), row_to_transaction)?;
    let items: Vec<Transaction> = rows.collect::<duckdb::Result<Vec<_>>>()?;
    Ok((items, total))
}

/// A single transaction by id, or `None` when it does not exist.
pub fn get_transaction(conn: &Connection, id: i64) -> anyhow::Result<Option<Transaction>> {
    let mut stmt = conn.prepare(&format!("{TRANSACTION_SELECT} WHERE t.id = ?"))?;
    let mut rows = stmt.query_map([id], row_to_transaction)?;
    Ok(rows.next().transpose()?)
}

/// The taxonomy id for a category name, or `None` when the name is not in the
/// fixed taxonomy.
pub fn category_id_for_name(conn: &Connection, name: &str) -> anyhow::Result<Option<i64>> {
    let mut stmt = conn.prepare("SELECT id FROM categories WHERE name = ?")?;
    let mut rows = stmt.query_map([name], |row| row.get(0))?;
    Ok(rows.next().transpose()?)
}

/// Apply an inline override. Returns the number of rows updated (0 when the
/// id does not exist).
///
/// `category` is a tri-state: `None` leaves the category untouched,
/// `Some(None)` clears it to `NULL` (uncategorized), `Some(Some(id))` sets it
/// to `id`. `new_kind = None` leaves the kind untouched.
pub fn set_transaction(
    conn: &Connection,
    id: i64,
    category: Option<Option<i64>>,
    new_kind: Option<&str>,
) -> anyhow::Result<usize> {
    let mut sets: Vec<&str> = Vec::new();
    let mut params: Vec<Box<dyn duckdb::ToSql>> = Vec::new();
    match category {
        Some(Some(cid)) => {
            sets.push("category_id = ?");
            params.push(Box::new(cid));
        }
        Some(None) => sets.push("category_id = NULL"),
        None => {}
    }
    if let Some(kind) = new_kind {
        sets.push("kind = ?");
        params.push(Box::new(kind.to_string()));
    }
    if sets.is_empty() {
        return Ok(0);
    }
    let sql = format!("UPDATE transactions SET {} WHERE id = ?", sets.join(", "));
    params.push(Box::new(id));
    let refs: Vec<&dyn duckdb::ToSql> = params.iter().map(|p| p.as_ref()).collect();
    let updated = conn.execute(&sql, refs.as_slice())?;
    Ok(updated)
}

/// The `kind` a row takes for a transfer flag, from the sign of its CHF
/// amount. A negative amount leaving the account is `transfer_out` when
/// flagged and `spend` when not; a positive (or zero) amount is `transfer_in`
/// / `income`.
pub fn kind_for_transfer(amount_chf: f64, is_transfer: bool) -> &'static str {
    match (is_transfer, amount_chf < 0.0) {
        (true, true) => "transfer_out",
        (true, false) => "transfer_in",
        (false, true) => "spend",
        (false, false) => "income",
    }
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
        // The transfer_out leg has no group yet: it counts as nothing.
        assert_eq!(got.moved, 0.0);
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
        assert_eq!(jan.moved, 0.0);
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
    fn summary_moved_is_the_sum_of_paired_transfer_groups() {
        let conn = seeded();
        insert_sample(&conn);
        // Pair the 2025-01-30 transfer_out leg with an arrival on account 2.
        conn.execute(
            "INSERT INTO transactions
                (account_id, source, source_key, dt, description, amount_orig,
                 currency_orig, amount_chf, kind)
             VALUES (2, 'test', 'k8', '2025-01-30', 'in', 50.0, 'CHF', 50.0, 'transfer_in')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO transfer_groups (from_account_id, to_account_id, amount_chf, dt)
             VALUES (1, 2, 50.0, '2025-01-30')",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE transactions SET transfer_group_id = 1
              WHERE source_key IN ('2025-01-30', 'k8')",
            [],
        )
        .unwrap();

        let year = summary(&conn, 2025, None).unwrap();
        assert_eq!(year.moved, 50.0);
        assert_eq!(year.spend, 425.75);
        assert_eq!(year.income, 3000.0);
        let january = summary(&conn, 2025, Some(1)).unwrap();
        assert_eq!(january.moved, 50.0);
        let february = summary(&conn, 2025, Some(2)).unwrap();
        assert_eq!(february.moved, 0.0);
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
    fn sankey_returns_category_spend_and_paired_account_transfers() {
        let conn = seeded();
        insert_sample(&conn);
        conn.execute(
            "INSERT INTO transfer_groups (from_account_id, to_account_id, amount_chf, dt)
             VALUES (1, 2, 50.0, '2025-01-30')",
            [],
        )
        .unwrap();

        let got = sankey(&conn, 2025, None).unwrap();
        assert_eq!(got.year, 2025);
        assert_eq!(got.month, None);
        assert_eq!(got.links.len(), 3);
        assert_eq!(
            got.links
                .iter()
                .map(|link| (
                    link.source.as_str(),
                    link.target.as_str(),
                    link.value,
                    link.kind.as_str()
                ))
                .collect::<Vec<_>>(),
            vec![
                ("account:1", "category:travel", 250.5, "spend"),
                ("account:1", "category:food", 175.25, "spend"),
                ("account:1", "account:2", 50.0, "transfer"),
            ]
        );
        assert_eq!(
            got.nodes
                .iter()
                .map(|node| (node.id.as_str(), node.column, node.value))
                .collect::<Vec<_>>(),
            vec![
                ("account:1", 0, 475.75),
                ("account:2", 1, 50.0),
                ("category:travel", 2, 250.5),
                ("category:food", 2, 175.25),
            ]
        );
    }

    #[test]
    fn sankey_sorts_source_columns_by_outgoing_and_outputs_by_incoming() {
        let conn = seeded();
        let food_id: i64 = conn
            .query_row("SELECT id FROM categories WHERE name = 'food'", [], |row| {
                row.get(0)
            })
            .unwrap();
        let travel_id: i64 = conn
            .query_row(
                "SELECT id FROM categories WHERE name = 'travel'",
                [],
                |row| row.get(0),
            )
            .unwrap();

        conn.execute(
            "INSERT INTO transactions
                (account_id, source, source_key, dt, description, category_id,
                 amount_orig, currency_orig, amount_chf, kind)
             VALUES
                (2, 'test', 'a2-food', '2025-01-02', 'x', ?, -80.0, 'CHF', -80.0, 'spend'),
                (3, 'test', 'a3-travel', '2025-01-03', 'x', ?, -120.0, 'CHF', -120.0, 'spend')",
            duckdb::params![food_id, travel_id],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO transfer_groups (from_account_id, to_account_id, amount_chf, dt)
             VALUES
                (1, 2, 500.0, '2025-01-01'),
                (1, 3, 50.0, '2025-01-01')",
            [],
        )
        .unwrap();

        let got = sankey(&conn, 2025, None).unwrap();
        assert_eq!(
            got.nodes
                .iter()
                .filter(|node| node.column == 1)
                .map(|node| node.id.as_str())
                .collect::<Vec<_>>(),
            vec!["account:3", "account:2"]
        );
        assert_eq!(
            got.nodes
                .iter()
                .filter(|node| node.column == 2)
                .map(|node| node.id.as_str())
                .collect::<Vec<_>>(),
            vec!["category:travel", "category:food"]
        );
    }

    #[test]
    fn sankey_transfer_links_use_target_specific_colors() {
        let conn = seeded();
        conn.execute(
            "INSERT INTO transfer_groups (from_account_id, to_account_id, amount_chf, dt)
             VALUES
                (1, 2, 500.0, '2025-01-01'),
                (1, 3, 300.0, '2025-01-02')",
            [],
        )
        .unwrap();

        let got = sankey(&conn, 2025, None).unwrap();
        assert_eq!(
            got.links
                .iter()
                .filter(|link| link.kind == "transfer")
                .map(|link| (link.target.as_str(), link.color.as_str()))
                .collect::<Vec<_>>(),
            vec![("account:2", "#a855f7"), ("account:3", "#06b6d4"),]
        );
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

    /// The id of the sample row whose `source_key` (== its date) is `dt`.
    fn id_for_key(conn: &Connection, key: &str) -> i64 {
        conn.query_row(
            "SELECT id FROM transactions WHERE source_key = ?",
            [key],
            |row| row.get(0),
        )
        .unwrap()
    }

    #[test]
    fn list_transactions_returns_all_newest_first_with_total() {
        let conn = seeded();
        insert_sample(&conn);
        let (items, total) =
            list_transactions(&conn, &TransactionFilters::default(), 1, 100).unwrap();
        assert_eq!(total, 7);
        assert_eq!(items.len(), 7);
        // Newest first: the 2025-12-31 row leads, the 2024-06-15 row trails.
        assert_eq!(items[0].dt, "2025-12-31");
        assert_eq!(items[6].dt, "2024-06-15");
        assert_eq!(items[0].category.as_ref().unwrap().name, "food");
        assert!(!items[0].is_transfer);
    }

    #[test]
    fn list_transactions_filters_by_year_source_and_category() {
        let conn = seeded();
        insert_sample(&conn);

        let year = TransactionFilters {
            year: Some(2025),
            ..Default::default()
        };
        let (items, total) = list_transactions(&conn, &year, 1, 100).unwrap();
        assert_eq!(total, 6);
        assert!(items.iter().all(|t| t.dt.starts_with("2025")));

        // Every sample row is source 'test'; filtering by it keeps all of 2025.
        let source = TransactionFilters {
            year: Some(2025),
            source: Some("test".into()),
            ..Default::default()
        };
        assert_eq!(
            list_transactions(&conn, &source, 1, 100).unwrap().0.len(),
            6
        );
        let other = TransactionFilters {
            source: Some("neon".into()),
            ..Default::default()
        };
        assert_eq!(list_transactions(&conn, &other, 1, 100).unwrap().1, 0);

        // Category filter: the three food rows match 'food' (two in 2025, one in 2024).
        let food = TransactionFilters {
            category: Some("food".into()),
            ..Default::default()
        };
        let (items, total) = list_transactions(&conn, &food, 1, 100).unwrap();
        assert_eq!(total, 3);
        assert!(
            items
                .iter()
                .all(|t| t.category.as_ref().unwrap().name == "food")
        );
    }

    #[test]
    fn list_transactions_uncategorized_matches_null_category() {
        let conn = seeded();
        // A row with no category at all.
        conn.execute(
            "INSERT INTO transactions
                (account_id, source, source_key, dt, description, category_id,
                 amount_orig, currency_orig, amount_chf, kind)
             VALUES (1, 'test', 'k-null', '2025-03-01', 'x', NULL, -10.0, 'CHF', -10.0, 'spend')",
            [],
        )
        .unwrap();
        let uncat = TransactionFilters {
            category: Some("uncategorized".into()),
            ..Default::default()
        };
        let (items, total) = list_transactions(&conn, &uncat, 1, 100).unwrap();
        assert_eq!(total, 1);
        assert!(items[0].category.is_none());

        // A named category excludes the null row.
        let food = TransactionFilters {
            category: Some("food".into()),
            ..Default::default()
        };
        let (items, _) = list_transactions(&conn, &food, 1, 100).unwrap();
        assert!(items.iter().all(|t| t.category.is_some()));
    }

    #[test]
    fn list_transactions_paginates() {
        let conn = seeded();
        insert_sample(&conn);
        let filters = TransactionFilters::default();
        let (page1, total) = list_transactions(&conn, &filters, 1, 3).unwrap();
        assert_eq!(total, 7);
        assert_eq!(page1.len(), 3);
        let (page2, _) = list_transactions(&conn, &filters, 2, 3).unwrap();
        assert_eq!(page2.len(), 3);
        let (page3, _) = list_transactions(&conn, &filters, 3, 3).unwrap();
        assert_eq!(page3.len(), 1);
        // No overlap between pages.
        let ids: Vec<i64> = page1
            .iter()
            .chain(page2.iter())
            .chain(page3.iter())
            .map(|t| t.id)
            .collect();
        assert_eq!(
            ids.len(),
            ids.into_iter()
                .collect::<std::collections::HashSet<_>>()
                .len()
        );
    }

    #[test]
    fn get_transaction_finds_rows_and_returns_none_for_missing() {
        let conn = seeded();
        insert_sample(&conn);
        let id = id_for_key(&conn, "2025-02-14");
        let got = get_transaction(&conn, id).unwrap().unwrap();
        assert_eq!(got.dt, "2025-02-14");
        assert_eq!(got.kind, "spend");
        assert_eq!(got.amount_chf, -250.5);
        assert_eq!(got.account, "Neon");
        assert!(get_transaction(&conn, 999_999).unwrap().is_none());
    }

    #[test]
    fn category_id_for_name_resolves_taxonomy_and_rejects_unknown() {
        let conn = seeded();
        assert_eq!(
            category_id_for_name(&conn, "food").unwrap(),
            category_id_for_name(&conn, "food").unwrap()
        );
        assert!(category_id_for_name(&conn, "food").unwrap().is_some());
        assert!(
            category_id_for_name(&conn, "not-a-category")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn set_transaction_overrides_category_and_transfer_flag() {
        let conn = seeded();
        insert_sample(&conn);
        let id = id_for_key(&conn, "2025-01-05"); // food, -100.0, spend
        let dining = category_id_for_name(&conn, "dining").unwrap().unwrap();

        // Change only the category; kind stays spend.
        let updated = set_transaction(&conn, id, Some(Some(dining)), None).unwrap();
        assert_eq!(updated, 1);
        let row = get_transaction(&conn, id).unwrap().unwrap();
        assert_eq!(row.category.as_ref().unwrap().name, "dining");
        assert_eq!(row.kind, "spend");

        // Flag as a transfer: negative amount -> transfer_out, is_transfer true.
        set_transaction(&conn, id, None, Some("transfer_out")).unwrap();
        let row = get_transaction(&conn, id).unwrap().unwrap();
        assert_eq!(row.kind, "transfer_out");
        assert!(row.is_transfer);

        // Unflag: negative amount -> spend again.
        set_transaction(&conn, id, None, Some("spend")).unwrap();
        let row = get_transaction(&conn, id).unwrap().unwrap();
        assert_eq!(row.kind, "spend");
        assert!(!row.is_transfer);

        // Updating a missing id affects no rows.
        assert_eq!(
            set_transaction(&conn, 999_999, Some(Some(dining)), None).unwrap(),
            0
        );

        // Clearing the category sets it to NULL (uncategorized).
        set_transaction(&conn, id, Some(None), None).unwrap();
        let row = get_transaction(&conn, id).unwrap().unwrap();
        assert!(row.category.is_none());
    }

    #[test]
    fn set_transaction_no_fields_is_a_noop() {
        let conn = seeded();
        insert_sample(&conn);
        let id = id_for_key(&conn, "2025-01-05"); // food, -100.0, spend
        assert_eq!(set_transaction(&conn, id, None, None).unwrap(), 0);
        let row = get_transaction(&conn, id).unwrap().unwrap();
        assert_eq!(row.category.as_ref().unwrap().name, "food");
        assert_eq!(row.kind, "spend");
    }

    #[test]
    fn kind_for_transfer_maps_sign_and_flag() {
        assert_eq!(kind_for_transfer(-10.0, true), "transfer_out");
        assert_eq!(kind_for_transfer(10.0, true), "transfer_in");
        assert_eq!(kind_for_transfer(-10.0, false), "spend");
        assert_eq!(kind_for_transfer(10.0, false), "income");
        // Zero is treated as a positive (inflow) amount.
        assert_eq!(kind_for_transfer(0.0, true), "transfer_in");
        assert_eq!(kind_for_transfer(0.0, false), "income");
    }

    #[test]
    fn set_transaction_positive_amount_maps_to_income_and_transfer_in() {
        let conn = seeded();
        insert_sample(&conn);
        let id = id_for_key(&conn, "2025-01-20"); // income, +1000.0
        set_transaction(&conn, id, None, Some("transfer_in")).unwrap();
        let row = get_transaction(&conn, id).unwrap().unwrap();
        assert_eq!(row.kind, "transfer_in");
        assert!(row.is_transfer);
        set_transaction(&conn, id, None, Some("income")).unwrap();
        let row = get_transaction(&conn, id).unwrap().unwrap();
        assert_eq!(row.kind, "income");
    }
}
