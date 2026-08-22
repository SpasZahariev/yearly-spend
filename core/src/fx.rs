use std::collections::BTreeMap;

use chrono::{Datelike, NaiveDate};
use duckdb::Connection;
use serde::Deserialize;

/// Monthly-average FX rates sourced from frankfurter.dev (ECB reference rates),
/// cached in the `fx_rates` table so re-ingests work offline.
pub struct Fx {
    http: reqwest::Client,
    base_url: String,
}

#[derive(Deserialize)]
struct FrankfurterResponse {
    #[serde(default)]
    rates: BTreeMap<String, BTreeMap<String, f64>>,
}

impl Fx {
    pub fn new(base_url: impl Into<String>) -> Self {
        Self {
            http: reqwest::Client::new(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
        }
    }

    pub fn cached_rate(
        conn: &Connection,
        month: NaiveDate,
        from: &str,
        to: &str,
    ) -> anyhow::Result<Option<f64>> {
        let first = NaiveDate::from_ymd_opt(month.year(), month.month(), 1)
            .ok_or_else(|| anyhow::anyhow!("invalid month: {month:?}"))?;
        let mut stmt = conn.prepare(
            "SELECT rate FROM fx_rates WHERE month = CAST(? AS DATE) AND from_ccy = ? AND to_ccy = ?",
        )?;
        let row: Option<f64> = match stmt.query_row(
            duckdb::params!(
                first.format("%Y-%m-%d").to_string(),
                normalize(from),
                normalize(to)
            ),
            |row| -> duckdb::Result<f64> { row.get(0) },
        ) {
            Ok(rate) => Some(rate),
            Err(duckdb::Error::QueryReturnedNoRows) => None,
            Err(e) => return Err(e.into()),
        };
        Ok(row)
    }

    /// Cached monthly average for the transaction's month, fetching and
    /// caching from frankfurter.dev on a miss. Identity is always 1.0.
    pub async fn monthly_rate(
        &self,
        conn: &Connection,
        month: NaiveDate,
        from: &str,
        to: &str,
    ) -> anyhow::Result<f64> {
        let from = normalize(from);
        let to = normalize(to);
        if from == to {
            return Ok(1.0);
        }
        if let Some(rate) = Self::cached_rate(conn, month, &from, &to)? {
            return Ok(rate);
        }

        let (start, end) = month_bounds(month);
        let url = format!(
            "{}/v1/{start}..{end}?base={from}&symbols={to}",
            self.base_url
        );
        let response: FrankfurterResponse = self
            .http
            .get(&url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        // Frankfurter anchors a range with the day before `start`; the
        // monthly average must cover only days of the requested month.
        let rates = Self::in_range(&response.rates, start, end);
        let rate = Self::average(&rates, &to)?;

        let first = NaiveDate::from_ymd_opt(month.year(), month.month(), 1).unwrap();
        conn.execute(
            "INSERT INTO fx_rates (month, from_ccy, to_ccy, rate) VALUES (CAST(? AS DATE), ?, ?, ?)
             ON CONFLICT (month, from_ccy, to_ccy) DO UPDATE SET rate = excluded.rate",
            duckdb::params!(first.format("%Y-%m-%d").to_string(), from, to, rate),
        )?;
        Ok(rate)
    }

    /// Keep only the days inside `[start, end]`; malformed dates are dropped.
    fn in_range(
        rates: &BTreeMap<String, BTreeMap<String, f64>>,
        start: NaiveDate,
        end: NaiveDate,
    ) -> BTreeMap<String, BTreeMap<String, f64>> {
        rates
            .iter()
            .filter(|(day, _)| {
                NaiveDate::parse_from_str(day, "%Y-%m-%d").is_ok_and(|d| (start..=end).contains(&d))
            })
            .map(|(day, day_rates)| (day.clone(), day_rates.clone()))
            .collect()
    }

    /// Mean of the daily `to`-per-`from` rates across all days present.
    pub fn average(
        rates: &BTreeMap<String, BTreeMap<String, f64>>,
        to: &str,
    ) -> anyhow::Result<f64> {
        let values = rates
            .values()
            .filter_map(|day| day.get(&normalize(to)).copied())
            .collect::<Vec<_>>();
        anyhow::ensure!(!values.is_empty(), "no {to} rates in response");
        let sum: f64 = values.iter().sum();
        Ok(sum / values.len() as f64)
    }
}

fn normalize(ccy: &str) -> String {
    ccy.trim().to_ascii_uppercase()
}

/// First day of the month and the latest day available for it (today for the
/// current month, so in-progress months still yield a usable average).
fn month_bounds(d: NaiveDate) -> (NaiveDate, NaiveDate) {
    let first = NaiveDate::from_ymd_opt(d.year(), d.month(), 1).unwrap();
    let last_day = |year: i32, month: u32| -> u32 {
        match month {
            1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
            4 | 6 | 9 | 11 => 30,
            2 => {
                if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) {
                    29
                } else {
                    28
                }
            }
            _ => 31,
        }
    };
    let month_end =
        NaiveDate::from_ymd_opt(d.year(), d.month(), last_day(d.year(), d.month())).unwrap();
    (first, month_end.min(chrono::Utc::now().date_naive()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rates_fixture() -> BTreeMap<String, BTreeMap<String, f64>> {
        let mut one = BTreeMap::new();
        one.insert("CHF".to_string(), 0.94f64);
        let mut two = BTreeMap::new();
        two.insert("CHF".to_string(), 0.96f64);
        let mut three = BTreeMap::new();
        three.insert("CHF".to_string(), 0.98f64);
        let mut m = BTreeMap::new();
        m.insert("2026-06-02".to_string(), one);
        m.insert("2026-06-03".to_string(), two);
        m.insert("2026-06-04".to_string(), three);
        m
    }

    #[test]
    fn average_computes_monthly_mean() {
        let rates = rates_fixture();
        assert!((Fx::average(&rates, "chf").unwrap() - 0.96).abs() < 1e-9);
        assert!(Fx::average(&rates, "USD").is_err());
    }

    #[test]
    fn in_range_ignores_days_outside_the_requested_window() {
        let mut rates = rates_fixture();
        let mut anchor = BTreeMap::new();
        anchor.insert("CHF".to_string(), 9.99f64);
        rates.insert("2026-06-01".to_string(), anchor);
        let mut bogus = BTreeMap::new();
        bogus.insert("CHF".to_string(), 1.0f64);
        rates.insert("not-a-date".to_string(), bogus);

        let window = Fx::in_range(
            &rates,
            NaiveDate::from_ymd_opt(2026, 6, 2).unwrap(),
            NaiveDate::from_ymd_opt(2026, 6, 30).unwrap(),
        );
        assert_eq!(window.len(), 3);
        assert!((Fx::average(&window, "CHF").unwrap() - 0.96).abs() < 1e-9);
    }

    #[test]
    fn bounds_for_past_months_cover_the_whole_month() {
        let (s, e) = month_bounds(NaiveDate::from_ymd_opt(2020, 1, 15).unwrap());
        assert_eq!(s, NaiveDate::from_ymd_opt(2020, 1, 1).unwrap());
        assert_eq!(e, NaiveDate::from_ymd_opt(2020, 1, 31).unwrap());
    }

    #[test]
    fn cached_rate_reads_the_fx_table() {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE fx_rates (month DATE, from_ccy VARCHAR, to_ccy VARCHAR, rate DOUBLE, UNIQUE (month, from_ccy, to_ccy))")
            .unwrap();
        let month = NaiveDate::from_ymd_opt(2026, 6, 12).unwrap();
        assert!(
            Fx::cached_rate(&conn, month, "EUR", "CHF")
                .unwrap()
                .is_none()
        );
        conn.execute(
            "INSERT INTO fx_rates VALUES ('2026-06-01', 'EUR', 'CHF', 0.95)",
            duckdb::params![],
        )
        .unwrap();
        assert_eq!(
            Fx::cached_rate(&conn, month, "eur", "chf").unwrap(),
            Some(0.95)
        );
    }
}
