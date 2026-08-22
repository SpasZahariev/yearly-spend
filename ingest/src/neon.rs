use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::Context;
use chrono::NaiveDate;
use csv::StringRecord;
use duckdb::{Connection, Transaction};
use serde::Deserialize;
use sha2::{Digest, Sha256};

use spend_core::config::Config;
use spend_core::llm::{Llm, Message};
use spend_core::schema;

const SOURCE: &str = "neon";
const ACCOUNT_NAME: &str = "Neon";
const CATEGORY_BATCH_SIZE: usize = 60;
const REQUIRED_HEADERS: &[&str] = &["Date", "Amount", "Description", "Subject", "Category"];

#[derive(Debug, Clone, PartialEq)]
struct NeonRow {
    source_key: String,
    dt: NaiveDate,
    description: String,
    subject: Option<String>,
    amount: f64,
    category: Option<String>,
    kind: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IngestReport {
    pub parsed_rows: usize,
    pub inserted_or_updated_rows: usize,
    pub skipped: bool,
    pub llm_batches: usize,
}

#[derive(Debug, Clone)]
struct LlmAudit {
    context: String,
    phase: String,
    attempt: i64,
    ok: bool,
}

#[derive(Debug, Deserialize)]
struct LlmAssignment {
    index: usize,
    category: String,
}

pub async fn ingest_file(
    conn: &mut Connection,
    path: &Path,
    config: &Config,
) -> anyhow::Result<IngestReport> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read Neon file {}", path.display()))?;
    let file_sha = sha256_hex(&bytes);

    if file_already_ingested(conn, SOURCE, &file_sha)? {
        return Ok(IngestReport {
            skipped: true,
            ..IngestReport::default()
        });
    }

    let mut rows =
        parse_csv(&bytes).with_context(|| format!("parse Neon file {}", path.display()))?;
    let parsed_rows = rows.len();
    let audits = categorize_uncategorized(&mut rows, config).await?;

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

fn parse_csv(bytes: &[u8]) -> anyhow::Result<Vec<NeonRow>> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let mut reader = csv::ReaderBuilder::new()
        .delimiter(b';')
        .has_headers(true)
        .flexible(false)
        .from_reader(bytes);
    let headers = reader.headers()?.clone();
    let indexes = header_indexes(&headers)?;
    let mut rows = Vec::new();

    for (line, result) in reader.records().enumerate() {
        let record =
            result.with_context(|| format!("invalid CSV record near line {}", line + 2))?;
        rows.push(
            parse_record(&record, &indexes)
                .with_context(|| format!("invalid Neon transaction near CSV line {}", line + 2))?,
        );
    }

    Ok(rows)
}

#[derive(Debug, Clone, Copy)]
struct HeaderIndexes {
    date: usize,
    amount: usize,
    description: usize,
    subject: usize,
    category: usize,
}

fn header_indexes(headers: &StringRecord) -> anyhow::Result<HeaderIndexes> {
    let index = |name: &str| {
        headers
            .iter()
            .position(|header| header.trim_start_matches('\u{feff}') == name)
            .ok_or_else(|| anyhow::anyhow!("Neon CSV is missing required column {name:?}"))
    };

    for header in REQUIRED_HEADERS {
        index(header)?;
    }

    Ok(HeaderIndexes {
        date: index("Date")?,
        amount: index("Amount")?,
        description: index("Description")?,
        subject: index("Subject")?,
        category: index("Category")?,
    })
}

fn parse_record(record: &StringRecord, indexes: &HeaderIndexes) -> anyhow::Result<NeonRow> {
    let value = |column: usize, name: &str| {
        record
            .get(column)
            .ok_or_else(|| anyhow::anyhow!("missing {name} field"))
    };

    let date_text = value(indexes.date, "Date")?.trim();
    let dt = NaiveDate::parse_from_str(date_text, "%Y-%m-%d")
        .with_context(|| format!("invalid Neon date {date_text:?}"))?;

    let amount_text = value(indexes.amount, "Amount")?.trim();
    let amount = amount_text
        .parse::<f64>()
        .with_context(|| format!("invalid Neon amount {amount_text:?}"))?;
    anyhow::ensure!(
        amount.is_finite(),
        "Neon amount is not finite: {amount_text:?}"
    );

    let description = value(indexes.description, "Description")?
        .trim()
        .to_string();
    let subject_text = value(indexes.subject, "Subject")?;
    let subject = (!subject_text.is_empty()).then(|| subject_text.to_string());
    let source_category = value(indexes.category, "Category")?.trim();
    let transfer_out = amount < 0.0 && is_transfer_counterparty(&description);
    let kind = if transfer_out {
        "transfer_out"
    } else if amount > 0.0 {
        "income"
    } else {
        "spend"
    }
    .to_string();

    let category = if transfer_out {
        Some("transfer".to_string())
    } else {
        map_category(source_category)?
    };

    Ok(NeonRow {
        source_key: natural_key(dt, amount, &description, subject.as_deref()),
        dt,
        description,
        subject,
        amount,
        category,
        kind,
    })
}

fn map_category(source: &str) -> anyhow::Result<Option<String>> {
    let normalized = source
        .trim()
        .to_ascii_lowercase()
        .chars()
        .map(|character| match character {
            '-' | ' ' | '/' | '&' => '_',
            character => character,
        })
        .collect::<String>();
    let normalized = normalized.trim_matches('_');
    if normalized.is_empty() || normalized == "uncategorized" {
        return Ok(None);
    }

    let mapped = match normalized {
        "income" => "income",
        "invest" => "invest",
        "transfer" => "transfer",
        "housing" => "housing",
        "food" => "food",
        "transport" => "transport",
        "travel" => "travel",
        "entertainment" => "entertainment",
        "health" => "health",
        "shopping" => "shopping",
        "subscriptions" => "subscriptions",
        "groceries" => "groceries",
        "dining" => "dining",
        "utilities" => "utilities",
        "education" => "education",
        "pets" => "pets",
        "fees" | "finances" => "fees",
        "household" => "utilities",
        "leisure" => "entertainment",
        other => {
            if other.starts_with("income_") {
                return Ok(Some("income".to_string()));
            }
            anyhow::bail!("unsupported non-empty Neon category {source:?} (normalized {other:?})")
        }
    };

    Ok(Some(mapped.to_string()))
}

fn is_transfer_counterparty(description: &str) -> bool {
    let normalized = description.to_ascii_lowercase();
    ["revolut", "swisscard aecs", "interactive brokers"]
        .iter()
        .any(|counterparty| normalized.contains(counterparty))
}

fn natural_key(dt: NaiveDate, amount: f64, description: &str, subject: Option<&str>) -> String {
    let amount_cents = (amount * 100.0).round() as i64;
    let mut hasher = Sha256::new();
    for value in [
        dt.format("%Y-%m-%d").to_string(),
        amount_cents.to_string(),
        description.to_string(),
        subject.unwrap_or_default().to_string(),
    ] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

async fn categorize_uncategorized(
    rows: &mut [NeonRow],
    config: &Config,
) -> anyhow::Result<Vec<LlmAudit>> {
    let pending = rows
        .iter()
        .enumerate()
        .filter_map(|(index, row)| row.category.is_none().then_some(index))
        .collect::<Vec<_>>();
    if pending.is_empty() {
        return Ok(Vec::new());
    }

    let llm = Llm::new(config);
    let mut audits = Vec::new();
    for (batch_number, batch) in pending.chunks(CATEGORY_BATCH_SIZE).enumerate() {
        let items = batch
            .iter()
            .enumerate()
            .map(|(index, row_index)| {
                let row = &rows[*row_index];
                serde_json::json!({
                    "index": index,
                    "date": row.dt.format("%Y-%m-%d").to_string(),
                    "amount": row.amount,
                    "description": row.description,
                    "subject": row.subject,
                })
            })
            .collect::<Vec<_>>();
        let prompt = serde_json::to_string(&items)?;
        let taxonomy = schema::CATEGORIES
            .iter()
            .map(|(name, _)| *name)
            .collect::<Vec<_>>();
        let messages = [
            Message::system(format!(
                "Classify each transaction into exactly one category from this fixed taxonomy: {}. \
                 Return only a JSON array of objects with integer `index` and string `category`. \
                 Include exactly one object for every input item. Do not use markdown. \
                 Choose a specific category when possible; `uncategorized` is allowed only when \
                 the transaction cannot be classified from the supplied fields.",
                serde_json::to_string(&taxonomy)?
            )),
            Message::user(format!(
                "Classify this JSON array of Neon transactions:\n{prompt}"
            )),
        ];

        let response = llm.complete(&messages).await.with_context(|| {
            format!(
                "local LLM categorization failed for Neon batch {}",
                batch_number + 1
            )
        })?;
        let assignments = parse_assignments(&response, batch.len())?;
        for assignment in assignments {
            let row_index = batch[assignment.index];
            rows[row_index].category = Some(assignment.category);
        }
        audits.push(LlmAudit {
            context: format!("neon category batch {}", batch_number + 1),
            phase: "neon_category".to_string(),
            attempt: 1,
            ok: true,
        });
    }

    Ok(audits)
}

fn parse_assignments(response: &str, expected: usize) -> anyhow::Result<Vec<LlmAssignment>> {
    let json = response.trim();
    let json = match json.strip_prefix("```") {
        Some(json) => {
            let json = json
                .strip_prefix("json")
                .or_else(|| json.strip_prefix("JSON"))
                .unwrap_or(json);
            json.trim_end_matches("```").trim()
        }
        None => json,
    };
    let assignments: Vec<LlmAssignment> =
        serde_json::from_str(json).context("LLM categorization response was not a JSON array")?;
    anyhow::ensure!(
        assignments.len() == expected,
        "LLM returned {} categories for {expected} transactions",
        assignments.len()
    );

    let taxonomy = schema::CATEGORIES
        .iter()
        .map(|(name, _)| *name)
        .collect::<HashSet<_>>();
    let mut indexes = HashSet::new();
    for assignment in &assignments {
        anyhow::ensure!(
            assignment.index < expected,
            "LLM returned out-of-range category index {}",
            assignment.index
        );
        anyhow::ensure!(
            indexes.insert(assignment.index),
            "LLM returned duplicate category index {}",
            assignment.index
        );
        anyhow::ensure!(
            taxonomy.contains(assignment.category.as_str()),
            "LLM returned category outside the taxonomy: {:?}",
            assignment.category
        );
    }
    anyhow::ensure!(
        indexes.len() == expected,
        "LLM did not return every category index"
    );
    Ok(assignments)
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
    .context("Neon account is missing from the database")
}

fn category_ids(conn: &Connection) -> anyhow::Result<HashMap<String, i64>> {
    let mut stmt = conn.prepare("SELECT id, name FROM categories")?;
    let rows = stmt.query_map([], |row| Ok((row.get(1)?, row.get(0)?)))?;
    Ok(rows.collect::<duckdb::Result<HashMap<_, _>>>()?)
}

fn upsert_transaction(
    conn: &Transaction<'_>,
    account_id: i64,
    row: &NeonRow,
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
            Option::<String>::None,
            row.description,
            row.subject,
            category_id,
            row.amount,
            "CHF",
            row.amount,
            row.kind,
            file_sha
        ],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;
    use axum::{Json, Router};
    use serde_json::json;
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicU64, Ordering};
    use tokio::net::TcpListener;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn sample_csv(category: &str) -> Vec<u8> {
        format!(
            concat!(
                "\"Date\";\"Amount\";\"Original amount\";\"Original currency\";\"Exchange rate\";\"Description\";\"Subject\";\"Category\";\"Tags\";\"Wise\";\"Spaces\"\r\n",
                "\"2026-08-01\";\"-12.50\";\"\";\"\";\"\";\"Coffee shop\";\"Order 1\";\"{}\";\"\";\"no\";\"no\"\r\n",
                "\"2026-08-02\";\"-100.00\";\"\";\"\";\"\";\"Swisscard AECS\";\"line one\r\nline two\";\"finances\";\"\";\"no\";\"no\"\r\n"
            ),
            category
        )
        .into_bytes()
    }

    fn temp_dir() -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("spend-neon-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn parser_preserves_multiline_subject_and_maps_transfer_counterparties() {
        let rows = parse_csv(&sample_csv("housing")).unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].category.as_deref(), Some("housing"));
        assert_eq!(rows[1].kind, "transfer_out");
        assert_eq!(rows[1].category.as_deref(), Some("transfer"));
        assert_eq!(rows[1].subject.as_deref(), Some("line one\r\nline two"));
    }

    #[test]
    fn transfer_matching_requires_an_outflow_and_supports_all_counterparties() {
        for description in [
            "Revolut",
            "Revolut Bank UAB",
            "Swisscard AECS",
            "Interactive Brokers",
        ] {
            assert!(is_transfer_counterparty(description));
            let row = parse_csv(
                format!(
                    concat!(
                        "\"Date\";\"Amount\";\"Description\";\"Subject\";\"Category\"\r\n",
                        "\"2026-08-01\";\"-10.00\";\"{}\";\"subject\";\"food\"\r\n"
                    ),
                    description
                )
                .as_bytes(),
            )
            .unwrap()
            .remove(0);
            assert_eq!(row.kind, "transfer_out");
            assert_eq!(row.category.as_deref(), Some("transfer"));
        }

        let incoming = parse_record(
            &StringRecord::from(vec!["2026-08-01", "10.00", "Revolut", "subject", "food"]),
            &HeaderIndexes {
                date: 0,
                amount: 1,
                description: 2,
                subject: 3,
                category: 4,
            },
        )
        .unwrap();
        assert_eq!(incoming.kind, "income");
        assert_eq!(incoming.category.as_deref(), Some("food"));
    }

    #[test]
    fn category_mapping_covers_neon_labels() {
        assert_eq!(
            map_category("income_salary").unwrap().as_deref(),
            Some("income")
        );
        assert_eq!(map_category("finances").unwrap().as_deref(), Some("fees"));
        assert_eq!(
            map_category("household").unwrap().as_deref(),
            Some("utilities")
        );
        assert_eq!(
            map_category("leisure").unwrap().as_deref(),
            Some("entertainment")
        );
        assert_eq!(
            map_category("income salary").unwrap().as_deref(),
            Some("income")
        );
        assert!(map_category("uncategorized").unwrap().is_none());
        assert!(map_category("new-neon-label").is_err());
    }

    #[test]
    fn llm_response_is_validated_against_taxonomy() {
        let parsed = parse_assignments(r#"[{"index":0,"category":"food"}]"#, 1).unwrap();
        assert_eq!(parsed[0].category, "food");
        let fenced =
            parse_assignments("```json\n[{\"index\":0,\"category\":\"food\"}]\n```", 1).unwrap();
        assert_eq!(fenced[0].category, "food");
        assert!(parse_assignments(r#"[{"index":0,"category":"not-a-category"}]"#, 1).is_err());
        assert!(parse_assignments(r#"[{"index":1,"category":"food"}]"#, 1).is_err());
    }

    #[tokio::test]
    async fn ingest_is_idempotent_and_updates_neon_categories() {
        async fn completions() -> Json<serde_json::Value> {
            Json(json!({
                "choices": [{
                    "message": {
                        "content": "[{\"index\":0,\"category\":\"shopping\"}]"
                    }
                }]
            }))
        }

        let app = Router::new().route("/v1/chat/completions", post(completions));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let dir = temp_dir();
        let db_path = dir.join("spend.duckdb");
        let file = dir.join("statement.csv");
        std::fs::write(&file, sample_csv("uncategorized")).unwrap();
        let config = Config {
            db_path: db_path.clone(),
            llm_provider: spend_core::config::LlmProvider::Local,
            llm_base_url: format!("http://{address}/v1"),
            llm_api_key: "test".to_string(),
            llm_model: "mock".to_string(),
            gemini_api_key: None,
            gemini_model: "unused".to_string(),
            fx_base_url: "unused".to_string(),
        };

        let mut conn = spend_core::db::ingest_connection(&db_path).unwrap();
        let first = ingest_file(&mut conn, &file, &config).await.unwrap();
        assert_eq!(first.parsed_rows, 2);
        assert_eq!(first.llm_batches, 1);
        assert!(!first.skipped);

        let second = ingest_file(&mut conn, &file, &config).await.unwrap();
        assert!(second.skipped);
        assert_eq!(second.parsed_rows, 0);

        let category_changed = sample_csv("housing");
        std::fs::write(&file, category_changed).unwrap();
        let third = ingest_file(&mut conn, &file, &config).await.unwrap();
        assert_eq!(third.inserted_or_updated_rows, 2);
        assert!(!third.skipped);

        let count: i64 = conn
            .query_row("SELECT count(*) FROM transactions", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 2);
        let shopping: i64 = conn
            .query_row(
                "SELECT count(*) FROM transactions t
                 JOIN categories c ON c.id = t.category_id
                 WHERE t.description = 'Coffee shop' AND c.name = 'housing'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(shopping, 1);
        let multiline: String = conn
            .query_row(
                "SELECT subject FROM transactions WHERE description = 'Swisscard AECS'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(multiline, "line one\r\nline two");
        let transfer_kind: String = conn
            .query_row(
                "SELECT kind FROM transactions WHERE description = 'Swisscard AECS'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(transfer_kind, "transfer_out");
        let llm_calls: i64 = conn
            .query_row("SELECT count(*) FROM llm_calls", [], |row| row.get(0))
            .unwrap();
        assert_eq!(llm_calls, 1);
        let file_count: i64 = conn
            .query_row("SELECT count(*) FROM ingested_files", [], |row| row.get(0))
            .unwrap();
        assert_eq!(file_count, 2);

        let _ = std::fs::remove_dir_all(dir);
    }
}
