use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::Context;
use chrono::{Datelike, NaiveDate, NaiveDateTime};
use csv::StringRecord;
use duckdb::{Connection, Transaction};
use serde_json::json;
use sha2::{Digest, Sha256};

use spend_core::config::Config;
use spend_core::fx::Fx;

use crate::IngestReport;
use crate::categorize::{self, LlmCategorizable};

const SOURCE: &str = "revolut";
const ACCOUNT_NAME: &str = "Revolut";
const REQUIRED_HEADERS: &[&str] = &[
    "Type",
    "Started Date",
    "Completed Date",
    "Description",
    "Amount",
    "Fee",
    "Currency",
    "State",
];

#[derive(Debug, Clone, PartialEq)]
pub struct RevolutRow {
    pub source_key: String,
    pub dt: NaiveDate,
    pub ts: NaiveDateTime,
    pub description: String,
    pub row_type: String,
    pub amount: f64,
    pub currency: String,
    pub kind: String,
    pub category: Option<String>,
    amount_chf: f64,
}

pub async fn ingest_file(
    conn: &mut Connection,
    path: &Path,
    config: &Config,
) -> anyhow::Result<IngestReport> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read Revolut file {}", path.display()))?;
    let file_sha = sha256_hex(&bytes);

    if file_already_ingested(conn, SOURCE, &file_sha)? {
        return Ok(IngestReport {
            skipped: true,
            ..IngestReport::default()
        });
    }

    let mut rows =
        parse_csv(&bytes).with_context(|| format!("parse Revolut file {}", path.display()))?;
    let parsed_rows = rows.len();
    let audits = categorize::categorize_uncategorized(&mut rows, SOURCE, config).await?;
    convert_to_chf(&mut rows, conn, config).await?;

    let tx = conn.transaction()?;
    let account_id = account_id(&tx)?;
    let category_ids = category_ids(&tx)?;

    for row in &rows {
        let category = row.category.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "row {} has no category after categorization",
                row.source_key
            )
        })?;
        let category_id = *category_ids
            .get(category)
            .ok_or_else(|| anyhow::anyhow!("category is not in the taxonomy: {category}"))?;
        upsert_transaction(&tx, account_id, row, category_id, &file_sha)?;
    }

    tx.execute(
        "INSERT INTO ingested_files
            (source, file_sha256, path, rows)
         VALUES (?, ?, ?, ?)
         ON CONFLICT (source, file_sha256) DO NOTHING",
        duckdb::params![
            SOURCE,
            file_sha,
            path.to_string_lossy().as_ref(),
            parsed_rows as i64
        ],
    )?;

    for audit in &audits {
        tx.execute(
            "INSERT INTO llm_calls (context, phase, attempt, ok)
             VALUES (?, ?, ?, ?)",
            duckdb::params![audit.context, audit.phase, audit.attempt, audit.ok],
        )?;
    }

    tx.commit()?;

    Ok(IngestReport {
        parsed_rows,
        inserted_or_updated_rows: parsed_rows,
        skipped: false,
        llm_batches: audits.len(),
    })
}

fn parse_csv(bytes: &[u8]) -> anyhow::Result<Vec<RevolutRow>> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .flexible(false)
        .from_reader(bytes);
    let headers = reader.headers()?.clone();
    let indexes = header_indexes(&headers)?;
    // REVERTED rows are dropped entirely; COMPLETED rows collapse onto their
    // natural key, which also dedupes exact duplicate rows within a file.
    let mut by_key: BTreeMap<String, RevolutRow> = BTreeMap::new();
    for (line, result) in reader.records().enumerate() {
        let record =
            result.with_context(|| format!("invalid CSV record near line {}", line + 2))?;
        let row = parse_record(&record, &indexes)
            .with_context(|| format!("invalid Revolut transaction near CSV line {}", line + 2))?;
        if let Some(row) = row {
            by_key.insert(row.source_key.clone(), row);
        }
    }
    Ok(by_key.into_values().collect())
}

#[derive(Debug, Clone, Copy)]
struct HeaderIndexes {
    row_type: usize,
    started: usize,
    completed: usize,
    description: usize,
    amount: usize,
    fee: usize,
    currency: usize,
    state: usize,
}

fn header_indexes(headers: &StringRecord) -> anyhow::Result<HeaderIndexes> {
    let index = |name: &str| {
        headers
            .iter()
            .position(|header| header.trim_start_matches('\u{feff}') == name)
            .ok_or_else(|| anyhow::anyhow!("Revolut CSV is missing required column {name:?}"))
    };

    for header in REQUIRED_HEADERS {
        index(header)?;
    }

    Ok(HeaderIndexes {
        row_type: index("Type")?,
        started: index("Started Date")?,
        completed: index("Completed Date")?,
        description: index("Description")?,
        amount: index("Amount")?,
        fee: index("Fee")?,
        currency: index("Currency")?,
        state: index("State")?,
    })
}

/// None means the row is dropped (REVERTED).
fn parse_record(
    record: &StringRecord,
    indexes: &HeaderIndexes,
) -> anyhow::Result<Option<RevolutRow>> {
    let value = |column: usize, name: &str| {
        record
            .get(column)
            .ok_or_else(|| anyhow::anyhow!("missing {name} field"))
    };

    let row_type = value(indexes.row_type, "Type")?.trim().to_string();
    anyhow::ensure!(!row_type.is_empty(), "empty Type");

    let state = value(indexes.state, "State")?.trim().to_string();
    if state == "REVERTED" {
        return Ok(None);
    }
    anyhow::ensure!(state == "COMPLETED", "unsupported Revolut state {state:?}");

    let started_text = value(indexes.started, "Started Date")?.trim();
    NaiveDateTime::parse_from_str(started_text, "%Y-%m-%d %H:%M:%S")
        .with_context(|| format!("invalid Revolut started date {started_text:?}"))?;
    let completed_text = value(indexes.completed, "Completed Date")?.trim();
    let completed = NaiveDateTime::parse_from_str(completed_text, "%Y-%m-%d %H:%M:%S")
        .with_context(|| format!("invalid Revolut completed date {completed_text:?}"))?;

    let amount_text = value(indexes.amount, "Amount")?.trim().to_string();
    let amount = parse_finite(&amount_text, "Amount")?;
    let fee = parse_finite(value(indexes.fee, "Fee")?.trim(), "Fee")?;
    // The Fee column is always a cost charged alongside the amount; the
    // Balance column confirms the applied movement is amount - fee.
    let amount = amount - fee;

    let currency = value(indexes.currency, "Currency")?
        .trim()
        .to_ascii_uppercase();
    anyhow::ensure!(!currency.is_empty(), "empty Currency");
    let description = value(indexes.description, "Description")?
        .trim()
        .to_string();

    let (kind, category) = classify(&row_type, &description, amount);

    Ok(Some(RevolutRow {
        source_key: natural_key(
            started_text,
            &row_type,
            &description,
            &amount_text,
            &currency,
        ),
        dt: completed.date(),
        ts: completed,
        description,
        row_type,
        amount,
        currency,
        kind,
        category,
        amount_chf: 0.0,
    }))
}

fn parse_finite(text: &str, name: &str) -> anyhow::Result<f64> {
    let value = text
        .parse::<f64>()
        .with_context(|| format!("invalid Revolut {name} {text:?}"))?;
    anyhow::ensure!(value.is_finite(), "Revolut {name} is not finite: {text:?}");
    Ok(value)
}

/// Kind and deterministic category for the hygiene rules. Spend and income
/// rows (category None) go through the batched LLM backfill.
fn classify(row_type: &str, description: &str, amount: f64) -> (String, Option<String>) {
    // Exchange FX-swap pairs and TEMP_BLOCK rows never count as spend.
    if row_type == "Exchange" || row_type == "TEMP_BLOCK" {
        return ("internal".into(), Some("transfer".into()));
    }
    // Zero-amount Closing transaction rows are sub-account housekeeping.
    if row_type == "Transfer" && description == "Closing transaction" && amount == 0.0 {
        return ("internal".into(), Some("transfer".into()));
    }
    // Topup rows are the Revolut leg of Neon funding transfers.
    if row_type == "Topup" {
        return ("transfer_in".into(), Some("transfer".into()));
    }
    if amount < 0.0 {
        ("spend".into(), None)
    } else if amount > 0.0 {
        ("income".into(), None)
    } else {
        ("internal".into(), Some("transfer".into()))
    }
}

fn natural_key(
    started: &str,
    row_type: &str,
    description: &str,
    amount: &str,
    currency: &str,
) -> String {
    let mut hasher = Sha256::new();
    for value in [started, row_type, description, amount, currency] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Convert every non-CHF row at the monthly average rate, fetching one
/// currency-month at a time and caching in `fx_rates` so later runs work
/// offline.
async fn convert_to_chf(
    rows: &mut [RevolutRow],
    conn: &Connection,
    config: &Config,
) -> anyhow::Result<()> {
    let fx = Fx::new(&config.fx_base_url);
    let needed: BTreeMap<(String, NaiveDate), ()> = rows
        .iter()
        .filter(|row| row.currency != "CHF")
        .map(|row| ((row.currency.clone(), month_first(row.dt)), ()))
        .collect();
    let mut rates: HashMap<(String, NaiveDate), f64> = HashMap::new();
    for ((currency, month), _) in needed {
        let rate = fx
            .monthly_rate(conn, month, &currency, "CHF")
            .await
            .with_context(|| format!("fetch monthly FX rate {currency}->CHF for {month:?}"))?;
        rates.insert((currency, month), rate);
    }
    for row in rows {
        let amount_chf = if row.currency == "CHF" {
            row.amount
        } else {
            row.amount
                * rates
                    .get(&(row.currency.clone(), month_first(row.dt)))
                    .copied()
                    .expect("rate for every non-CHF currency-month was fetched")
        };
        row.amount_chf = amount_chf;
    }
    Ok(())
}

fn month_first(dt: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(dt.year(), dt.month(), 1).expect("valid date yields a valid month")
}

impl LlmCategorizable for RevolutRow {
    fn needs_category(&self) -> bool {
        self.category.is_none()
    }

    fn llm_item(&self) -> serde_json::Value {
        json!({
            "date": self.dt.format("%Y-%m-%d").to_string(),
            "amount": self.amount,
            "type": self.row_type,
            "description": self.description,
        })
    }

    fn set_category(&mut self, category: String) {
        self.category = Some(category);
    }
}

fn file_already_ingested(conn: &Connection, source: &str, file_sha: &str) -> anyhow::Result<bool> {
    let count: i64 = conn.query_row(
        "SELECT count(*) FROM ingested_files WHERE source = ? AND file_sha256 = ?",
        duckdb::params![source, file_sha],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn account_id(conn: &Connection) -> anyhow::Result<i64> {
    conn.query_row(
        "SELECT id FROM accounts WHERE source = ? OR name = ? LIMIT 1",
        duckdb::params![SOURCE, ACCOUNT_NAME],
        |row| row.get(0),
    )
    .context("Revolut account is missing from the database")
}

fn category_ids(conn: &Connection) -> anyhow::Result<HashMap<String, i64>> {
    let mut stmt = conn.prepare("SELECT id, name FROM categories")?;
    let rows = stmt.query_map([], |row| Ok((row.get(1)?, row.get(0)?)))?;
    Ok(rows.collect::<duckdb::Result<HashMap<_, _>>>()?)
}

fn upsert_transaction(
    conn: &Transaction<'_>,
    account_id: i64,
    row: &RevolutRow,
    category_id: i64,
    file_sha: &str,
) -> anyhow::Result<()> {
    conn.execute(
        "INSERT INTO transactions
            (account_id, source, source_key, dt, ts, description, subject,
             category_id, amount_orig, currency_orig, amount_chf, kind, file_sha)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT (source, source_key) DO UPDATE SET
            account_id = excluded.account_id,
            dt = excluded.dt,
            ts = excluded.ts,
            description = excluded.description,
            subject = excluded.subject,
            category_id = excluded.category_id,
            amount_orig = excluded.amount_orig,
            currency_orig = excluded.currency_orig,
            amount_chf = excluded.amount_chf,
            kind = excluded.kind,
            file_sha = excluded.file_sha,
            ingested_at = excluded.ingested_at",
        duckdb::params![
            account_id,
            SOURCE,
            row.source_key,
            row.dt.format("%Y-%m-%d").to_string(),
            Some(row.ts.format("%Y-%m-%d %H:%M:%S").to_string()),
            row.description,
            None::<String>,
            category_id,
            row.amount,
            row.currency,
            row.amount_chf,
            row.kind,
            file_sha
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::{Query, State};
    use axum::routing::{get, post};
    use axum::{Json, Router};
    use serde::Deserialize;
    use std::collections::HashMap;
    use std::net::SocketAddr;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::net::TcpListener;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn temp_dir() -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("spend-revolut-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const SAMPLE_CSV: &str = concat!(
        "Type,Product,Started Date,Completed Date,Description,Amount,Fee,Currency,State,Balance\r\n",
        "Exchange,Current,2025-01-05 10:00:00,2025-01-05 10:00:00,Exchanged to EUR,-100.00,0.00,CHF,COMPLETED,0.00\r\n",
        "Exchange,Current,2025-01-05 10:00:00,2025-01-05 10:00:00,Exchanged to EUR,95.00,0.00,EUR,COMPLETED,95.00\r\n",
        "Topup,Current,2025-01-06 09:00:00,2025-01-06 09:00:00,Payment from SPAS ANEV ZAHARIEV,300.00,0.00,CHF,COMPLETED,295.00\r\n",
        "Card Payment,Current,2025-01-07 12:00:00,2025-01-08 15:30:00,Zara,-10.00,0.50,CHF,COMPLETED,284.50\r\n",
        "ATM,Current,2025-01-09 08:00:00,2025-01-10 08:00:00,Cash withdrawal at Some Atm,-50.00,0.00,EUR,COMPLETED,45.00\r\n",
        "Card Payment,Current,2025-01-11 10:00:00,,Google,-1.00,0.00,CHF,REVERTED,\r\n",
        "TEMP_BLOCK,Current,2025-01-12 10:00:00,2025-01-12 10:00:00,To Some Person,-18.00,0.00,EUR,COMPLETED,27.00\r\n",
        "Transfer,Current,2025-01-13 10:00:00,2025-01-13 10:00:00,Closing transaction,0.00,0.00,EUR,COMPLETED,27.00\r\n",
        "Transfer,Current,2025-01-13 10:00:00,2025-01-13 10:00:00,Closing transaction,0.00,0.00,EUR,COMPLETED,27.00\r\n",
        "Transfer,Current,2025-01-14 10:00:00,2025-01-14 10:00:00,Transfer from FRIEND NAME,20.00,0.00,EUR,COMPLETED,47.00\r\n"
    );

    fn parsed() -> Vec<RevolutRow> {
        parse_csv(SAMPLE_CSV.as_bytes()).unwrap()
    }

    #[test]
    fn parser_drops_reverted_rows_and_dedupes_exact_duplicates() {
        let rows = parsed();
        assert_eq!(rows.len(), 8, "9 completed rows minus one duplicate pair");
        assert!(rows.iter().all(|row| row.description != "Google"));
        let closings = rows
            .iter()
            .filter(|row| row.description == "Closing transaction")
            .count();
        assert_eq!(closings, 1);
    }

    #[test]
    fn parser_applies_hygiene_kinds_and_fee_netting() {
        let rows = parsed();
        let by_desc = |desc: &str| rows.iter().find(|row| row.description == desc).unwrap();

        for row in rows
            .iter()
            .filter(|row| row.description == "Exchanged to EUR")
        {
            assert_eq!(row.kind, "internal");
            assert_eq!(row.category.as_deref(), Some("transfer"));
        }
        let exchange_eur = rows
            .iter()
            .find(|row| row.currency == "EUR" && row.description == "Exchanged to EUR")
            .unwrap();
        assert_eq!(exchange_eur.amount, 95.00);
        let exchange_chf = rows
            .iter()
            .find(|row| row.currency == "CHF" && row.description == "Exchanged to EUR")
            .unwrap();
        assert_eq!(exchange_chf.amount, -100.00);

        let topup = by_desc("Payment from SPAS ANEV ZAHARIEV");
        assert_eq!(topup.kind, "transfer_in");
        assert_eq!(topup.category.as_deref(), Some("transfer"));
        assert_eq!(topup.amount, 300.00);

        let zara = by_desc("Zara");
        assert_eq!(zara.kind, "spend");
        assert!(zara.category.is_none());
        assert_eq!(zara.amount, -10.50, "amount is net of the fee");
        assert_eq!(
            zara.dt,
            NaiveDate::from_ymd_opt(2025, 1, 8).unwrap(),
            "dt is the completed date"
        );
        assert_eq!(
            zara.ts,
            NaiveDateTime::parse_from_str("2025-01-08 15:30:00", "%Y-%m-%d %H:%M:%S").unwrap()
        );

        let atm = by_desc("Cash withdrawal at Some Atm");
        assert_eq!(atm.kind, "spend");
        assert_eq!(atm.amount, -50.00);

        let temp_block = by_desc("To Some Person");
        assert_eq!(temp_block.kind, "internal");

        let closing = by_desc("Closing transaction");
        assert_eq!(closing.kind, "internal");
        assert_eq!(closing.amount, 0.0);

        let incoming = by_desc("Transfer from FRIEND NAME");
        assert_eq!(incoming.kind, "income");
        assert!(incoming.category.is_none());
    }

    #[test]
    fn natural_key_covers_start_type_description_amount_currency() {
        let rows = parsed();
        let closing = rows
            .iter()
            .find(|row| row.description == "Closing transaction")
            .unwrap();
        assert_eq!(
            closing.source_key,
            super::natural_key(
                "2025-01-13 10:00:00",
                "Transfer",
                "Closing transaction",
                "0.00",
                "EUR"
            )
        );

        let mut changed =
            SAMPLE_CSV.replace("Transfer from FRIEND NAME", "Transfer from OTHER NAME");
        changed = changed.replace("20.00,0.00,EUR", "21.00,0.00,EUR");
        let other = parse_csv(changed.as_bytes()).unwrap();
        let other_incoming = other
            .iter()
            .find(|row| row.description == "Transfer from OTHER NAME")
            .unwrap();
        assert_ne!(
            other_incoming.source_key, closing.source_key,
            "description or amount changes must yield a different key"
        );
    }

    #[test]
    fn unsupported_state_is_an_error() {
        let csv_text = SAMPLE_CSV.replace("REVERTED", "PENDING");
        assert!(parse_csv(csv_text.as_bytes()).is_err());
    }

    #[test]
    fn corpus_files_parse_with_expected_unique_row_counts() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../statements/Revolut");
        let expected = [
            ("account-statement_2023-07-18_2023-12-31_en_baed4b.csv", 122),
            ("account-statement_2024-01-01_2024-12-31_en_23c711.csv", 231),
            ("account-statement_2025-01-01_2025-12-31_en_bce200.csv", 320),
            ("account-statement_2026-01-01_2026-08-22_en_7bbb35.csv", 188),
        ];
        for (name, count) in expected {
            let path = dir.join(name);
            let bytes = std::fs::read(&path)
                .with_context(|| format!("read corpus file {}", path.display()))
                .unwrap();
            let rows = parse_csv(&bytes).unwrap();
            // The counts include REVERTED rows dropped and the exact
            // duplicate row in the 2025 file collapsed.
            assert_eq!(rows.len(), count, "{name}");
            assert!(
                rows.iter().all(|row| !row.description.is_empty()),
                "{name}: empty description"
            );
        }
    }

    #[derive(Deserialize)]
    struct ChatRequest {
        messages: Vec<serde_json::Value>,
    }

    async fn fx_handler(
        State(counter): State<Arc<AtomicU64>>,
        Query(params): Query<HashMap<String, String>>,
    ) -> Json<serde_json::Value> {
        counter.fetch_add(1, Ordering::SeqCst);
        let base = params.get("base").cloned().unwrap_or_default();
        let rate = match base.as_str() {
            "EUR" => 0.95,
            "BGN" => 0.41,
            "GBP" => 1.15,
            "USD" => 0.88,
            "HUF" => 0.0026,
            _ => 1.0,
        };
        Json(json!({ "rates": { "2025-01-01": { "CHF": rate } } }))
    }

    async fn llm_handler(
        State(counter): State<Arc<AtomicU64>>,
        Json(request): Json<ChatRequest>,
    ) -> Json<serde_json::Value> {
        counter.fetch_add(1, Ordering::SeqCst);
        let user = request
            .messages
            .iter()
            .find(|m| m["role"] == "user")
            .and_then(|m| m["content"].as_str())
            .unwrap()
            .to_string();
        let items = user.split_once('\n').unwrap().1;
        let items: Vec<serde_json::Value> = serde_json::from_str(items).unwrap();
        let assignments = items
            .iter()
            .enumerate()
            .map(|(index, _)| json!({ "index": index, "category": "food" }))
            .collect::<Vec<_>>();
        Json(json!({
            "choices": [{ "message": { "content": serde_json::to_string(&assignments).unwrap() } }]
        }))
    }

    async fn mock_servers() -> (SocketAddr, SocketAddr, Arc<AtomicU64>, Arc<AtomicU64>) {
        let fx_hits = Arc::new(AtomicU64::new(0));
        let llm_hits = Arc::new(AtomicU64::new(0));

        let fx_app = Router::new()
            .route("/v1/{range}", get(fx_handler))
            .with_state(fx_hits.clone());
        let fx_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let fx_address = fx_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(fx_listener, fx_app).await.unwrap();
        });

        let llm_app = Router::new()
            .route("/v1/chat/completions", post(llm_handler))
            .with_state(llm_hits.clone());
        let llm_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let llm_address = llm_listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(llm_listener, llm_app).await.unwrap();
        });

        (fx_address, llm_address, fx_hits, llm_hits)
    }

    #[tokio::test]
    async fn ingest_is_idempotent_and_caches_one_fx_rate_per_currency_month() {
        let (fx_address, llm_address, fx_hits, llm_hits) = mock_servers().await;
        let dir = temp_dir();
        let db_path = dir.join("spend.duckdb");
        let file = dir.join("revolut.csv");
        std::fs::write(&file, SAMPLE_CSV).unwrap();
        let config = Config {
            db_path: db_path.clone(),
            llm_provider: spend_core::config::LlmProvider::Local,
            llm_base_url: format!("http://{llm_address}/v1"),
            llm_api_key: "test".to_string(),
            llm_model: "mock".to_string(),
            gemini_api_key: None,
            gemini_model: "unused".to_string(),
            fx_base_url: format!("http://{fx_address}"),
        };

        let mut conn = spend_core::db::ingest_connection(&db_path).unwrap();
        let first = ingest_file(&mut conn, &file, &config).await.unwrap();
        assert_eq!(first.parsed_rows, 8);
        assert_eq!(first.inserted_or_updated_rows, 8);
        assert_eq!(first.llm_batches, 1);
        assert!(!first.skipped);
        assert_eq!(
            fx_hits.load(Ordering::SeqCst),
            1,
            "one call per currency-month"
        );
        assert_eq!(llm_hits.load(Ordering::SeqCst), 1);

        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM transactions WHERE source = 'revolut'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 8);

        let mut stmt = conn
            .prepare(
                "SELECT kind, count(*) FROM transactions WHERE source = 'revolut'
                 GROUP BY kind ORDER BY kind",
            )
            .unwrap();
        let kinds: Vec<(String, i64)> = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            kinds,
            vec![
                ("income".to_string(), 1),
                ("internal".to_string(), 4),
                ("spend".to_string(), 2),
                ("transfer_in".to_string(), 1),
            ]
        );

        let reverted: i64 = conn
            .query_row(
                "SELECT count(*) FROM transactions WHERE source = 'revolut' AND description = 'Google'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(reverted, 0, "REVERTED rows are absent from the DB");

        let fx_rows: i64 = conn
            .query_row("SELECT count(*) FROM fx_rates", [], |row| row.get(0))
            .unwrap();
        assert_eq!(fx_rows, 1, "one fx_rates row per currency-month");
        let (fx_month, fx_ccy, fx_rate): (NaiveDate, String, f64) = conn
            .query_row("SELECT month, from_ccy, rate FROM fx_rates", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap();
        assert_eq!(fx_month, NaiveDate::from_ymd_opt(2025, 1, 1).unwrap());
        assert_eq!(fx_ccy, "EUR");
        assert!((fx_rate - 0.95).abs() < 1e-9);

        let spend: f64 = conn
            .query_row(
                "SELECT sum(-amount_chf) FROM transactions WHERE source = 'revolut' AND kind = 'spend'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        // Zara 10.50 CHF plus ATM 50 EUR at 0.95; exchange, topup, TEMP_BLOCK
        // and closing rows never enter spend.
        assert!((spend - (10.50 + 50.0 * 0.95)).abs() < 1e-9);

        let atm_chf: f64 = conn
            .query_row(
                "SELECT amount_chf FROM transactions WHERE source = 'revolut' AND description = 'Cash withdrawal at Some Atm'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!((atm_chf - (-50.0 * 0.95)).abs() < 1e-9);
        let topup_chf: f64 = conn
            .query_row(
                "SELECT amount_chf FROM transactions WHERE source = 'revolut' AND kind = 'transfer_in'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!((topup_chf - 300.0).abs() < 1e-9);

        let llm_calls: i64 = conn
            .query_row("SELECT count(*) FROM llm_calls", [], |row| row.get(0))
            .unwrap();
        assert_eq!(llm_calls, 1);
        let files: i64 = conn
            .query_row("SELECT count(*) FROM ingested_files", [], |row| row.get(0))
            .unwrap();
        assert_eq!(files, 1);

        let second = ingest_file(&mut conn, &file, &config).await.unwrap();
        assert!(second.skipped);
        assert_eq!(second.parsed_rows, 0);
        assert_eq!(
            fx_hits.load(Ordering::SeqCst),
            1,
            "re-ingest makes no network calls"
        );
        assert_eq!(llm_hits.load(Ordering::SeqCst), 1);
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM transactions WHERE source = 'revolut'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 8);

        let _ = std::fs::remove_dir_all(dir);
    }
}
