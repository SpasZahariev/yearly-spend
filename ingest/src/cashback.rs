use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;
use chrono::{Datelike, NaiveDate};
use csv::StringRecord;
use duckdb::{Connection, Transaction};
use serde_json::json;
use sha2::{Digest, Sha256};

use spend_core::config::Config;
use spend_core::fx::Fx;

use crate::IngestReport;
use crate::categorize::{self, LlmCategorizable};

const SOURCE: &str = "cashback";
const ACCOUNT_NAME: &str = "Swisscard Cashback";
const REQUIRED_HEADERS: &[&str] = &[
    "Transaction date",
    "Description",
    "Merchant",
    "Card number",
    "Currency",
    "Amount",
    "Foreign Currency",
    "Amount in foreign currency",
    "Debit/Credit",
    "Status",
    "Merchant Category",
    "Registered Category",
];

/// The card applies its own FX markup, so the charged CHF amount deviates a
/// few percent from the Frankfurter monthly average. A deviation beyond this
/// tolerance indicates a parsing bug (wrong currency or magnitude), not a
/// legitimate card rate, and hard-fails the run.
const FX_SANITY_TOLERANCE: f64 = 0.10;

#[derive(Debug, Clone, PartialEq)]
struct CashbackRow {
    source_key: String,
    dt: NaiveDate,
    description: String,
    merchant: String,
    amount_chf: f64,
    amount_orig: f64,
    currency_orig: String,
    kind: String,
    category: Option<String>,
}

pub async fn ingest_file(
    conn: &mut Connection,
    path: &Path,
    config: &Config,
) -> anyhow::Result<IngestReport> {
    let bytes =
        std::fs::read(path).with_context(|| format!("read cashback file {}", path.display()))?;
    let file_sha = sha256_hex(&bytes);

    if file_already_ingested(conn, SOURCE, &file_sha)? {
        return Ok(IngestReport {
            skipped: true,
            ..IngestReport::default()
        });
    }

    // Parse, validate, and categorize before touching the database so a
    // malformed row or a validation mismatch hard-fails with no partial
    // writes.
    let mut rows =
        parse_csv(&bytes).with_context(|| format!("parse cashback file {}", path.display()))?;
    let parsed_rows = rows.len();

    let neon = neon_swisscard_rows(conn)?;
    validate_payments(&rows, &neon, path)?;
    sanity_check_fx(&rows, conn)?;

    let audits = categorize::categorize_uncategorized(&mut rows, SOURCE, config).await?;

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

fn parse_csv(bytes: &[u8]) -> anyhow::Result<Vec<CashbackRow>> {
    let bytes = bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes);
    let mut reader = csv::ReaderBuilder::new()
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
            parse_record(&record, &indexes).with_context(|| {
                format!("invalid cashback transaction near CSV line {}", line + 2)
            })?,
        );
    }

    Ok(rows)
}

#[derive(Debug, Clone, Copy)]
struct HeaderIndexes {
    date: usize,
    description: usize,
    merchant: usize,
    card: usize,
    currency: usize,
    amount: usize,
    foreign_currency: usize,
    foreign_amount: usize,
    debit_credit: usize,
    status: usize,
    merchant_category: usize,
}

fn header_indexes(headers: &StringRecord) -> anyhow::Result<HeaderIndexes> {
    let index = |name: &str| {
        headers
            .iter()
            .position(|header| header.trim_start_matches('\u{feff}') == name)
            .ok_or_else(|| anyhow::anyhow!("cashback CSV is missing required column {name:?}"))
    };

    for header in REQUIRED_HEADERS {
        index(header)?;
    }

    Ok(HeaderIndexes {
        date: index("Transaction date")?,
        description: index("Description")?,
        merchant: index("Merchant")?,
        card: index("Card number")?,
        currency: index("Currency")?,
        amount: index("Amount")?,
        foreign_currency: index("Foreign Currency")?,
        foreign_amount: index("Amount in foreign currency")?,
        debit_credit: index("Debit/Credit")?,
        status: index("Status")?,
        merchant_category: index("Merchant Category")?,
    })
}

fn parse_record(record: &StringRecord, indexes: &HeaderIndexes) -> anyhow::Result<CashbackRow> {
    let value = |column: usize, name: &str| {
        record
            .get(column)
            .ok_or_else(|| anyhow::anyhow!("missing {name} field"))
    };

    let date_text = value(indexes.date, "Transaction date")?.trim();
    let dt = NaiveDate::parse_from_str(date_text, "%d.%m.%Y")
        .with_context(|| format!("invalid cashback date {date_text:?}"))?;

    let description = value(indexes.description, "Description")?
        .trim()
        .to_string();
    anyhow::ensure!(!description.is_empty(), "empty Description");

    let merchant = value(indexes.merchant, "Merchant")?.trim().to_string();

    let card = value(indexes.card, "Card number")?.trim().to_string();
    anyhow::ensure!(!card.is_empty(), "empty Card number");

    let currency = value(indexes.currency, "Currency")?
        .trim()
        .to_ascii_uppercase();
    anyhow::ensure!(
        currency == "CHF",
        "unexpected cashback Currency {currency:?} (the Amount column is always CHF)"
    );

    let amount = parse_finite(value(indexes.amount, "Amount")?.trim(), "Amount")?;

    let debit_credit = value(indexes.debit_credit, "Debit/Credit")?.trim();
    anyhow::ensure!(
        matches!(debit_credit, "Debit" | "Credit"),
        "unexpected Debit/Credit value {debit_credit:?}"
    );

    let _status = value(indexes.status, "Status")?.trim();

    let merchant_category = value(indexes.merchant_category, "Merchant Category")?
        .trim()
        .to_string();

    // Debit rows carry a positive Amount (money out) and credit rows a
    // negative one (money in); the stored signed amount is the negation,
    // matching the Neon/Revolut convention (spend negative, money-in positive).
    let signed_chf = -amount;
    let (amount_orig, currency_orig) = match (
        value(indexes.foreign_currency, "Foreign Currency")?.trim(),
        value(indexes.foreign_amount, "Amount in foreign currency")?.trim(),
    ) {
        ("", "") => (signed_chf, "CHF".to_string()),
        (foreign_currency, foreign_amount) => {
            let foreign_currency = foreign_currency.to_ascii_uppercase();
            let foreign_amount = parse_finite(foreign_amount, "Amount in foreign currency")?;
            anyhow::ensure!(
                !foreign_currency.is_empty(),
                "foreign amount present without a Foreign Currency code"
            );
            (-foreign_amount, foreign_currency)
        }
    };

    let (kind, category) = classify(&description, &merchant_category);

    Ok(CashbackRow {
        source_key: natural_key(
            &card,
            &dt,
            &description,
            (signed_chf * 100.0).round() as i64,
        ),
        dt,
        description,
        merchant,
        amount_chf: signed_chf,
        amount_orig,
        currency_orig,
        kind,
        category,
    })
}

fn parse_finite(text: &str, name: &str) -> anyhow::Result<f64> {
    let value = text
        .parse::<f64>()
        .with_context(|| format!("invalid cashback {name} {text:?}"))?;
    anyhow::ensure!(value.is_finite(), "cashback {name} is not finite: {text:?}");
    Ok(value)
}

/// Kind and deterministic category for the special rows. Spend rows (category
/// `None`) go through the batched LLM backfill.
fn classify(description: &str, merchant_category: &str) -> (String, Option<String>) {
    let normalized = description.to_ascii_uppercase();
    if normalized == "CASHBACK" {
        return ("income".into(), Some("income".into()));
    }
    if normalized.starts_with("YOUR PAYMENT") {
        return ("transfer_in".into(), Some("transfer".into()));
    }
    ("spend".into(), map_category(merchant_category))
}

/// The export's Merchant Category is the source of truth. Labels with a clear
/// taxonomy counterpart map deterministically; the ambiguous ones (and any
/// future label) have no counterpart and go to the LLM backfill.
fn map_category(source: &str) -> Option<String> {
    let normalized = source.trim().to_ascii_lowercase();
    Some(
        match normalized.as_str() {
            "travel" => "travel",
            "groceries" => "groceries",
            "food and drink" => "dining",
            "shopping" => "shopping",
            "entertainment" => "entertainment",
            "health and beauty" => "health",
            "auto" => "transport",
            "finance" => "fees",
            _ => return None,
        }
        .to_string(),
    )
}

fn natural_key(card: &str, dt: &NaiveDate, description: &str, amount_cents: i64) -> String {
    let mut hasher = Sha256::new();
    for value in [
        card.to_string(),
        dt.format("%Y-%m-%d").to_string(),
        description.to_string(),
        amount_cents.to_string(),
    ] {
        hasher.update((value.len() as u64).to_be_bytes());
        hasher.update(value.as_bytes());
    }
    format!("{:x}", hasher.finalize())
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// Neon's `Swisscard AECS` rows, keyed by (date, signed cents) with the raw
/// Subject, used to validate the cashback funding legs.
fn neon_swisscard_rows(conn: &Connection) -> anyhow::Result<HashMap<(NaiveDate, i64), String>> {
    let mut stmt = conn.prepare(
        "SELECT dt, amount_chf, subject FROM transactions
         WHERE source = 'neon' AND description = 'Swisscard AECS'",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok((
            (
                row.get::<_, NaiveDate>(0)?,
                (row.get::<_, f64>(1)? * 100.0).round() as i64,
            ),
            row.get::<_, String>(2)?,
        ))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let ((dt, cents), subject) = row?;
        map.insert((dt, cents), subject);
    }
    Ok(map)
}

/// Every `YOUR PAYMENT (DD)` funding leg must pair with a Neon `Swisscard
/// AECS` row of the same date and opposite amount, and that Neon row's
/// statement date must fall in the payment's month. Any mismatch hard-fails
/// the run.
fn validate_payments(
    rows: &[CashbackRow],
    neon: &HashMap<(NaiveDate, i64), String>,
    path: &Path,
) -> anyhow::Result<()> {
    let context = path.display();
    for row in rows.iter().filter(|r| r.kind == "transfer_in") {
        let cashback_cents = (row.amount_chf * 100.0).round() as i64;
        let subject = neon.get(&(row.dt, -cashback_cents)).with_context(|| {
            format!(
                "{context}: YOUR PAYMENT {} ({}) has no matching Neon Swisscard AECS row of \
                     the same date and amount",
                row.dt, row.description
            )
        })?;
        let pay_month = (row.dt.year(), row.dt.month());
        let statement_month = statement_month(subject).with_context(|| {
            format!(
                "{context}: Neon Swisscard row for {} has no STATEMENT DATED line",
                row.dt
            )
        })?;
        anyhow::ensure!(
            statement_month == pay_month,
            "{context}: Neon Swisscard statement month {statement_month:?} does not match the \
             payment month {pay_month:?} on {}",
            row.dt
        );
    }
    Ok(())
}

/// The `STATEMENT DATED` line in a Neon Subject is a `ddMMYYYY` stamp; return
/// its (year, month).
fn statement_month(subject: &str) -> Option<(i32, u32)> {
    let lines: Vec<&str> = subject
        .split(['\r', '\n'])
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    for window in lines.windows(2) {
        if window[0].eq_ignore_ascii_case("STATEMENT DATED") && window[1].len() == 8 {
            let s = window[1];
            if s.chars().all(|c| c.is_ascii_digit()) {
                let day: u32 = s[0..2].parse().ok()?;
                let month: u32 = s[2..4].parse().ok()?;
                let year: i32 = s[4..8].parse().ok()?;
                NaiveDate::from_ymd_opt(year, month, day)?;
                return Some((year, month));
            }
        }
    }
    None
}

/// Foreign-currency rows keep the card-charged CHF amount as the converted
/// value (no re-conversion); the charged amount is only sanity-checked
/// against the cached `fx_rates` monthly average when that rate is available
/// offline.
fn sanity_check_fx(rows: &[CashbackRow], conn: &Connection) -> anyhow::Result<()> {
    for row in rows
        .iter()
        .filter(|r| r.currency_orig != "CHF" && r.amount_orig != 0.0)
    {
        let month = month_first(row.dt);
        let Some(rate) = Fx::cached_rate(conn, month, &row.currency_orig, "CHF")? else {
            continue;
        };
        let expected = row.amount_orig * rate;
        let deviation = (row.amount_chf - expected).abs() / expected.abs();
        let implied = row.amount_chf / row.amount_orig;
        anyhow::ensure!(
            deviation <= FX_SANITY_TOLERANCE,
            "FX sanity check failed for {} {}: CHF {} is {}% off the cached {} monthly average \
             (implied rate {} vs cached {})",
            row.dt,
            row.description,
            row.amount_chf,
            deviation * 100.0,
            row.currency_orig,
            implied,
            rate
        );
    }
    Ok(())
}

fn month_first(dt: NaiveDate) -> NaiveDate {
    NaiveDate::from_ymd_opt(dt.year(), dt.month(), 1).expect("valid date yields a valid month")
}

impl LlmCategorizable for CashbackRow {
    fn needs_category(&self) -> bool {
        self.category.is_none()
    }

    fn llm_item(&self) -> serde_json::Value {
        json!({
            "date": self.dt.format("%Y-%m-%d").to_string(),
            "amount": self.amount_chf,
            "description": self.description,
            "merchant": self.merchant,
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
    .context("cashback account is missing from the database")
}

fn category_ids(conn: &Connection) -> anyhow::Result<HashMap<String, i64>> {
    let mut stmt = conn.prepare("SELECT id, name FROM categories")?;
    let rows = stmt.query_map([], |row| Ok((row.get(1)?, row.get(0)?)))?;
    Ok(rows.collect::<duckdb::Result<HashMap<_, _>>>()?)
}

fn upsert_transaction(
    conn: &Transaction<'_>,
    account_id: i64,
    row: &CashbackRow,
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
            None::<String>,
            category_id,
            row.amount_orig,
            row.currency_orig,
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
    use std::sync::atomic::{AtomicU64, Ordering};

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    const SAMPLE_CSV: &str = concat!(
        "\"Transaction date\",\"Description\",\"Merchant\",\"Card number\",\"Currency\",\"Amount\",\"Foreign Currency\",\"Amount in foreign currency\",\"Debit/Credit\",\"Status\",\"Merchant Category\",\"Registered Category\"\r\n",
        "\"05.10.2024\",\"DENNER DISCOUNT 597, ZÜRICH\",\"Denner\",\"3776 60**** *8526\",\"CHF\",\"10.85\",\"\",\"\",\"Debit\",\"Posted\",\"Groceries\",\"DEPARTMENT STORES\"\r\n",
        "\"05.10.2024\",\"SBB MOBILE, BERN\",\"SBB CFF FFS\",\"3776 60**** *8526\",\"CHF\",\"9.00\",\"\",\"\",\"Debit\",\"Posted\",\"Travel\",\"PASSENGER RAILWAYS\"\r\n",
        "\"17.01.2025\",\"ALI*ALIEXPRESS ALIPAY, SINGAPORE\",\"Alipay\",\"3776 60**** *8526\",\"CHF\",\"60.70\",\"EUR\",\"61.92\",\"Debit\",\"Posted\",\"Shopping\",\"ONLINE STORES\"\r\n",
        "\"26.06.2026\",\"CKO*DRIFFLE 548525, VILNIAUS M.\",\"Driffle\",\"3776 60**** *8526\",\"CHF\",\"-44.45\",\"EUR\",\"-47.39\",\"Credit\",\"Posted\",\"Food and Drink\",\"EATING PLACES, RESTAURANTS\"\r\n",
        "\"06.01.2025\",\"CASHBACK\",\"\",\"3776 60**** *8526\",\"CHF\",\"-87.45\",\"\",\"\",\"Credit\",\"Posted\",\"General\",\"\"\r\n",
        "\"11.09.2024\",\"YOUR PAYMENT (DD) – THANK YOU\",\"\",\"3776 60**** *8526\",\"CHF\",\"-1600.80\",\"\",\"\",\"Credit\",\"Posted\",\"Payment\",\"\"\r\n",
        "\"09.12.2024\",\"PAYPAL *TOOGOODTOGO, 4029357733\",\"To Good To Go\",\"3776 60**** *8526\",\"CHF\",\"5.00\",\"\",\"\",\"Debit\",\"Posted\",\"General\",\"\"\r\n"
    );

    fn temp_dir() -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("spend-cashback-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn by_desc<'a>(rows: &'a [CashbackRow], desc: &str) -> &'a CashbackRow {
        rows.iter().find(|row| row.description == desc).unwrap()
    }

    #[test]
    fn parser_maps_kinds_signs_and_categories() {
        let rows = parse_csv(SAMPLE_CSV.as_bytes()).unwrap();
        assert_eq!(rows.len(), 7);

        let denner = by_desc(&rows, "DENNER DISCOUNT 597, ZÜRICH");
        assert_eq!(denner.kind, "spend");
        assert_eq!(denner.category.as_deref(), Some("groceries"));
        assert_eq!(denner.amount_chf, -10.85);
        assert_eq!(denner.amount_orig, -10.85);
        assert_eq!(denner.currency_orig, "CHF");

        let sbb = by_desc(&rows, "SBB MOBILE, BERN");
        assert_eq!(sbb.category.as_deref(), Some("travel"));
        assert_eq!(sbb.amount_chf, -9.00);

        let alipay = by_desc(&rows, "ALI*ALIEXPRESS ALIPAY, SINGAPORE");
        assert_eq!(alipay.kind, "spend");
        assert_eq!(
            alipay.amount_chf, -60.70,
            "CHF charge is the converted value"
        );
        assert_eq!(alipay.amount_orig, -61.92, "foreign amount is the original");
        assert_eq!(alipay.currency_orig, "EUR");

        let refund = by_desc(&rows, "CKO*DRIFFLE 548525, VILNIAUS M.");
        assert_eq!(refund.kind, "spend");
        assert_eq!(refund.category.as_deref(), Some("dining"));
        assert_eq!(refund.amount_chf, 44.45, "credit refund is positive");
        assert_eq!(refund.amount_orig, 47.39);
        assert_eq!(refund.currency_orig, "EUR");

        let cashback = by_desc(&rows, "CASHBACK");
        assert_eq!(cashback.kind, "income");
        assert_eq!(cashback.category.as_deref(), Some("income"));
        assert_eq!(cashback.amount_chf, 87.45);

        let payment = by_desc(&rows, "YOUR PAYMENT (DD) – THANK YOU");
        assert_eq!(payment.kind, "transfer_in");
        assert_eq!(payment.category.as_deref(), Some("transfer"));
        assert_eq!(payment.amount_chf, 1600.80);

        let togood = by_desc(&rows, "PAYPAL *TOOGOODTOGO, 4029357733");
        assert_eq!(togood.kind, "spend");
        assert!(
            togood.category.is_none(),
            "General has no counterpart and goes to the LLM"
        );
    }

    #[test]
    fn merchant_category_maps_deterministic_labels_and_leaves_ambiguous_ones() {
        assert_eq!(map_category("Travel").as_deref(), Some("travel"));
        assert_eq!(map_category("Groceries").as_deref(), Some("groceries"));
        assert_eq!(map_category("Food and Drink").as_deref(), Some("dining"));
        assert_eq!(map_category("Shopping").as_deref(), Some("shopping"));
        assert_eq!(
            map_category("Entertainment").as_deref(),
            Some("entertainment")
        );
        assert_eq!(map_category("Health and Beauty").as_deref(), Some("health"));
        assert_eq!(map_category("Auto").as_deref(), Some("transport"));
        assert_eq!(map_category("Finance").as_deref(), Some("fees"));
        assert_eq!(map_category("General").as_deref(), None);
        assert_eq!(map_category("Services").as_deref(), None);
        assert_eq!(map_category("Family and Household").as_deref(), None);
        assert_eq!(map_category("brand-new-label").as_deref(), None);
    }

    #[test]
    fn natural_key_covers_card_date_description_and_amount() {
        let rows = parse_csv(SAMPLE_CSV.as_bytes()).unwrap();
        let denner = by_desc(&rows, "DENNER DISCOUNT 597, ZÜRICH");
        let dt = NaiveDate::parse_from_str("2024-10-05", "%Y-%m-%d").unwrap();
        assert_eq!(
            denner.source_key,
            super::natural_key(
                "3776 60**** *8526",
                &dt,
                "DENNER DISCOUNT 597, ZÜRICH",
                -1085
            )
        );

        // Same transaction re-downloaded under a different file name keeps the
        // key; a changed amount, card, date, or description does not.
        assert_eq!(
            super::natural_key(
                "3776 60**** *8526",
                &dt,
                "DENNER DISCOUNT 597, ZÜRICH",
                -1085
            ),
            denner.source_key
        );
        assert_ne!(
            super::natural_key(
                "5100 21** **** 1963",
                &dt,
                "DENNER DISCOUNT 597, ZÜRICH",
                -1085
            ),
            denner.source_key
        );
        assert_ne!(
            super::natural_key(
                "3776 60**** *8526",
                &dt,
                "DENNER DISCOUNT 598, ZÜRICH",
                -1085
            ),
            denner.source_key
        );
        assert_ne!(
            super::natural_key(
                "3776 60**** *8526",
                &dt,
                "DENNER DISCOUNT 597, ZÜRICH",
                -1086
            ),
            denner.source_key
        );
    }

    #[test]
    fn statement_month_reads_the_ddmmyyyy_stamp() {
        let subject = "927462233411741400070068400\n377660800638526\nSTATEMENT DATED\n06122023";
        assert_eq!(statement_month(subject), Some((2023, 12)));
        assert_eq!(
            statement_month("QR-Bill: PURPOSE: Cashback Cards 4000 7006 840"),
            None
        );
    }

    #[test]
    fn malformed_rows_are_rejected() {
        // Wrong column count.
        let short = SAMPLE_CSV.replace(
            "\"09.12.2024\",\"PAYPAL *TOOGOODTOGO, 4029357733\",\"To Good To Go\",\"3776 60**** *8526\",\"CHF\",\"5.00\",\"\",\"\",\"Debit\",\"Posted\",\"General\",\"\"",
            "\"09.12.2024\",\"PAYPAL *TOOGOODTOGO, 4029357733\",\"To Good To Go\",\"3776 60**** *8526\",\"CHF\",\"5.00\",\"\",\"Debit\"",
        );
        assert!(parse_csv(short.as_bytes()).is_err());

        // Bad date.
        let bad_date = SAMPLE_CSV.replace("05.10.2024", "2024-10-05");
        assert!(parse_csv(bad_date.as_bytes()).is_err());

        // Non-CHF base currency.
        let bad_ccy = SAMPLE_CSV.replace("\"CHF\",\"10.85\"", "\"EUR\",\"10.85\"");
        assert!(parse_csv(bad_ccy.as_bytes()).is_err());

        // Unsupported Debit/Credit.
        let bad_dc = SAMPLE_CSV.replace(
            "\"Debit\",\"Posted\",\"Groceries\"",
            "\"Pending\",\"Posted\",\"Groceries\"",
        );
        assert!(parse_csv(bad_dc.as_bytes()).is_err());
    }

    #[test]
    fn corpus_files_parse_with_expected_row_counts() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../statements/cashback_cards");
        let expected = [
            ("SC-Transactions_2026-08-22_18-44-57.csv", 29),
            ("SC-Transactions_2026-08-22_18-45-04.csv", 31),
            ("SC-Transactions_2026-08-22_18-45-10.csv", 34),
            ("SC-Transactions_2026-08-22_18-46-38.csv", 24),
            ("SC-Transactions_2026-08-22_18-47-05.csv", 54),
            ("SC-Transactions_2026-08-22_18-47-31.csv", 45),
            ("SC-Transactions_2026-08-22_18-47-35.csv", 30),
            ("SC-Transactions_2026-08-22_18-47-38.csv", 16),
            ("SC-Transactions_2026-08-22_18-47-42.csv", 43),
            ("SC-Transactions_2026-08-22_18-47-45.csv", 14),
            ("SC-Transactions_2026-08-22_18-47-49.csv", 21),
            ("SC-Transactions_2026-08-22_18-47-52.csv", 50),
            ("SC-Transactions_2026-08-22_18-47-55.csv", 36),
            ("SC-Transactions_2026-08-22_18-47-58.csv", 44),
            ("SC-Transactions_2026-08-22_18-48-02.csv", 44),
            ("SC-Transactions_2026-08-22_18-48-06.csv", 29),
            ("SC-Transactions_2026-08-22_18-48-11.csv", 24),
            ("SC-Transactions_2026-08-22_18-48-14.csv", 26),
            ("SC-Transactions_2026-08-22_18-48-17.csv", 9),
            ("SC-Transactions_2026-08-22_18-48-19.csv", 16),
            ("SC-Transactions_2026-08-22_18-48-22.csv", 39),
            ("SC-Transactions_2026-08-22_18-48-25.csv", 34),
            ("SC-Transactions_2026-08-22_18-48-28.csv", 54),
        ];
        let mut total = 0;
        for (name, count) in expected {
            let path = dir.join(name);
            let bytes = std::fs::read(&path)
                .with_context(|| format!("read corpus file {}", path.display()))
                .unwrap();
            let rows = parse_csv(&bytes).unwrap();
            assert_eq!(rows.len(), count, "{name}");
            total += rows.len();
        }
        assert_eq!(total, 746, "the corpus holds 746 transactions");
    }

    fn seed_neon_payment(conn: &Connection, date: &str, amount: f64) {
        conn.execute(
            "INSERT INTO transactions
                (account_id, source, source_key, dt, description, category_id,
                 amount_orig, currency_orig, amount_chf, kind, subject)
             VALUES (1, 'neon', ?, CAST(? AS DATE), 'Swisscard AECS', 3,
                     ?, 'CHF', ?, 'transfer_out', ?)",
            duckdb::params![
                format!("neon-{date}-{amount}"),
                date,
                amount,
                amount,
                format!(
                    "ref\n377660800638526\nSTATEMENT DATED\n06{}20{}",
                    &date[5..7],
                    &date[2..4]
                )
            ],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn ingest_validates_payments_is_idempotent_and_categorizes_via_llm() {
        use axum::routing::post;
        use axum::{Json, Router};

        let app = Router::new().route(
            "/v1/chat/completions",
            post(|_: Json<serde_json::Value>| async {
                Json(json!({
                    "choices": [{ "message": { "content": "[{\"index\":0,\"category\":\"shopping\"}]" } }]
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address: std::net::SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let dir = temp_dir();
        let db_path = dir.join("spend.duckdb");
        let mut conn = spend_core::db::ingest_connection(&db_path).unwrap();

        // The 11.09.2024 funding leg needs a matching Neon row to pass.
        seed_neon_payment(&conn, "2024-09-11", -1600.80);

        let file = dir.join("cashback.csv");
        // Keep only the funding leg and one General spend row for the LLM.
        let payload = concat!(
            "\"Transaction date\",\"Description\",\"Merchant\",\"Card number\",\"Currency\",\"Amount\",\"Foreign Currency\",\"Amount in foreign currency\",\"Debit/Credit\",\"Status\",\"Merchant Category\",\"Registered Category\"\r\n",
            "\"11.09.2024\",\"YOUR PAYMENT (DD) – THANK YOU\",\"\",\"3776 60**** *8526\",\"CHF\",\"-1600.80\",\"\",\"\",\"Credit\",\"Posted\",\"Payment\",\"\"\r\n",
            "\"09.12.2024\",\"PAYPAL *TOOGOODTOGO, 4029357733\",\"To Good To Go\",\"3776 60**** *8526\",\"CHF\",\"5.00\",\"\",\"\",\"Debit\",\"Posted\",\"General\",\"\"\r\n"
        );
        std::fs::write(&file, payload).unwrap();
        let config = Config {
            db_path: db_path.clone(),
            llm_provider: spend_core::config::LlmProvider::Local,
            llm_base_url: format!("http://{address}/v1"),
            llm_api_key: "test".to_string(),
            llm_model: "mock".to_string(),
            gemini_api_key: None,
            gemini_model: "unused".to_string(),
            gemini_base_url: "unused".to_string(),
            fx_base_url: "unused".to_string(),
        };

        // A payment with no Neon counterpart must hard-fail before any write.
        let orphan = concat!(
            "\"Transaction date\",\"Description\",\"Merchant\",\"Card number\",\"Currency\",\"Amount\",\"Foreign Currency\",\"Amount in foreign currency\",\"Debit/Credit\",\"Status\",\"Merchant Category\",\"Registered Category\"\r\n",
            "\"12.10.2024\",\"YOUR PAYMENT (DD) – THANK YOU\",\"\",\"3776 60**** *8526\",\"CHF\",\"-42.42\",\"\",\"\",\"Credit\",\"Posted\",\"Payment\",\"\"\r\n"
        );
        let orphan_file = dir.join("orphan.csv");
        std::fs::write(&orphan_file, orphan).unwrap();
        let orphan_err = ingest_file(&mut conn, &orphan_file, &config)
            .await
            .unwrap_err();
        assert!(
            orphan_err
                .to_string()
                .contains("no matching Neon Swisscard AECS row"),
            "unexpected error: {orphan_err}"
        );
        let orphan_rows: i64 = conn
            .query_row(
                "SELECT count(*) FROM transactions WHERE source = 'cashback'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(orphan_rows, 0, "a failed validation writes nothing");

        let first = ingest_file(&mut conn, &file, &config).await.unwrap();
        assert_eq!(first.parsed_rows, 2);
        assert_eq!(first.llm_batches, 1);
        assert!(!first.skipped);

        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM transactions WHERE source = 'cashback'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2);

        let kind: String = conn
            .query_row(
                "SELECT kind FROM transactions WHERE source = 'cashback' AND description LIKE 'YOUR PAYMENT%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(kind, "transfer_in");

        let togood_category: String = conn
            .query_row(
                "SELECT c.name FROM transactions t JOIN categories c ON c.id = t.category_id
                 WHERE t.source = 'cashback' AND t.description LIKE 'PAYPAL *TOOGOODTOGO%'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(togood_category, "shopping");

        let second = ingest_file(&mut conn, &file, &config).await.unwrap();
        assert!(second.skipped);
        assert_eq!(second.parsed_rows, 0);
        let count: i64 = conn
            .query_row(
                "SELECT count(*) FROM transactions WHERE source = 'cashback'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 2, "re-ingest adds no rows");

        let _ = std::fs::remove_dir_all(dir);
    }
}
