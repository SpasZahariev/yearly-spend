//! Cross-account funding transfer pairing.
//!
//! The parsers tag funding legs per source: Neon outflows to Revolut,
//! Swisscard AECS or Interactive Brokers are `transfer_out`, Revolut topups
//! and cashback `YOUR PAYMENT` rows are `transfer_in`. This module links the
//! two legs of one transfer into a `transfer_groups` row in two passes:
//!
//! 1. Deterministic, in two passes inside one scope. The scope separates
//!    Neon -> Revolut funding (out description contains `revolut`, in leg
//!    from source `revolut`) from Neon -> Swisscard cashback funding (out
//!    description contains `swisscard`, in leg from source `cashback`). Pass A
//!    links exact same-date, exact-CHF-amount legs; pass B links legs whose
//!    inflow settles 1-3 days late (exact amount, inflow on or after the
//!    outflow, smallest delay preferred). Both passes are one-to-one and
//!    consume bucket members in id order, so the result is deterministic.
//! 2. LLM review: legs still unpaired after pass 1 are batched to the LLM,
//!    which may confirm pairs the deterministic passes did not. Every proposal
//!    is strictly validated (exact amount, inflow on or after outflow, at most
//!    3 days gap, matching scope, one-to-one); an invalid response fails the
//!    run. Legs sent to the LLM are recorded in `transfer_review` so re-runs
//!    do not re-review them. Deterministic pairing still runs on reviewed
//!    legs: a later statement can complete a pair without an LLM call.
//!
//! Legs that stay unpaired keep their transfer kind, so spend excludes both
//! legs, but they form no group and do not count as `moved`. Interactive
//! Brokers outflows have no inflow leg in this corpus: they are scoped out of
//! both passes and remain transfer-flagged but ungrouped.

use std::collections::{HashMap, HashSet};

use anyhow::{Context, ensure};
use chrono::NaiveDate;
use duckdb::Connection;
use serde::Deserialize;
use serde_json::{Value, json};

use spend_core::config::Config;
use spend_core::llm::{Llm, Message};

/// Out legs per LLM review batch.
const BATCH_SIZE: usize = 60;

/// Maximum inflow delay in days the deterministic settlement-late pass and
/// the LLM validation accept.
const MAX_DELAY_DAYS: i64 = 3;

/// Outcome of one pairing run, for CLI reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PairReport {
    pub deterministic_pairs: usize,
    pub llm_pairs: usize,
    pub llm_batches: usize,
    /// Legs still ungrouped afterwards, both directions.
    pub unpaired_legs: usize,
}

#[derive(Debug, Clone)]
struct Leg {
    id: i64,
    account_id: i64,
    dt: NaiveDate,
    /// Signed CHF cents: negative for out legs, positive for in legs.
    cents: i64,
    description: String,
    /// `"revolut"` or `"cashback"`; `None` means no partner can exist.
    scope: Option<&'static str>,
}

fn out_scope(description: &str) -> Option<&'static str> {
    let lower = description.to_lowercase();
    if lower.contains("revolut") {
        Some("revolut")
    } else if lower.contains("swisscard") {
        Some("cashback")
    } else {
        None
    }
}

fn in_scope(source: &str) -> Option<&'static str> {
    match source {
        "revolut" => Some("revolut"),
        "cashback" => Some("cashback"),
        _ => None,
    }
}

fn load_legs(conn: &Connection) -> anyhow::Result<(Vec<Leg>, Vec<Leg>)> {
    fn load(conn: &Connection, kind: &str, is_out: bool) -> anyhow::Result<Vec<Leg>> {
        let mut stmt = conn
            .prepare(&format!(
                "SELECT t.id, t.account_id, t.dt, t.amount_chf, t.description, t.source
                   FROM transactions t
                  WHERE t.kind = '{kind}' AND t.transfer_group_id IS NULL
                  ORDER BY t.id"
            ))
            .context("preparing transfer leg query")?;
        let rows = stmt
            .query_map([], |r| {
                let id: i64 = r.get(0)?;
                let account_id: i64 = r.get(1)?;
                let dt: String = r.get(2)?;
                let amount: f64 = r.get(3)?;
                let description: String = r.get(4)?;
                let source: String = r.get(5)?;
                Ok((id, account_id, dt, amount, description, source))
            })?
            .collect::<duckdb::Result<Vec<_>>>()
            .context("reading transfer legs")?;
        rows.into_iter()
            .map(|(id, account_id, dt, amount, description, source)| {
                let cents = (amount * 100.0).round() as i64;
                let scope = if is_out {
                    out_scope(&description)
                } else {
                    in_scope(&source)
                };
                Ok(Leg {
                    id,
                    account_id,
                    dt: NaiveDate::parse_from_str(&dt, "%Y-%m-%d")
                        .with_context(|| format!("invalid transfer leg date {dt}"))?,
                    cents,
                    description,
                    scope,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()
    }
    Ok((
        load(conn, "transfer_out", true)?,
        load(conn, "transfer_in", false)?,
    ))
}

/// Pair ungrouped transfer legs deterministically.
///
/// Pass A links every out leg with an unpaired in leg of the same scope on
/// the same day and exact opposite amount. Pass B then links the survivors
/// when the inflow settles 1-3 days late: same scope, exact amount, inflow on
/// or after the outflow, smallest delay preferred, ties broken by in-leg id.
/// Bucket members are consumed in id order, so the pairing is deterministic
/// and one-to-one.
fn deterministic_pairs(outs: &[Leg], ins: &[Leg]) -> Vec<(usize, usize)> {
    let mut buckets: HashMap<(&str, NaiveDate, i64), Vec<usize>> = HashMap::new();
    for (i, in_leg) in ins.iter().enumerate() {
        if let Some(scope) = in_leg.scope {
            buckets
                .entry((scope, in_leg.dt, in_leg.cents))
                .or_default()
                .push(i);
        }
    }
    let mut taken_in: HashSet<usize> = HashSet::new();
    let mut pairs: Vec<(usize, usize)> = Vec::new();
    for (o, out) in outs.iter().enumerate() {
        let Some(scope) = out.scope else { continue };
        if let Some(bucket) = buckets.get_mut(&(scope, out.dt, -out.cents))
            && let Some(i) = bucket.iter().copied().find(|i| taken_in.insert(*i))
        {
            pairs.push((o, i));
        }
    }
    let taken_out: HashSet<usize> = pairs.iter().map(|&(o, _)| o).collect();
    for (o, out) in outs.iter().enumerate() {
        if taken_out.contains(&o) {
            continue;
        }
        let Some(scope) = out.scope else { continue };
        let mut best: Option<(i64, usize)> = None;
        for (i, in_leg) in ins.iter().enumerate() {
            if taken_in.contains(&i) || in_leg.scope != Some(scope) || in_leg.cents != -out.cents {
                continue;
            }
            if in_leg.dt < out.dt {
                continue;
            }
            let gap = (in_leg.dt - out.dt).num_days();
            if gap > MAX_DELAY_DAYS {
                continue;
            }
            if best.is_none_or(|(best_gap, _)| gap < best_gap) {
                best = Some((gap, i));
            }
        }
        if let Some((_, i)) = best {
            taken_in.insert(i);
            pairs.push((o, i));
        }
    }
    pairs
}

fn persist_pairs(
    conn: &mut Connection,
    outs: &[Leg],
    ins: &[Leg],
    pairs: &[(usize, usize)],
) -> anyhow::Result<()> {
    let tx = conn
        .transaction()
        .context("starting transfer group transaction")?;
    for (o, i) in pairs {
        let (out, in_leg) = (&outs[*o], &ins[*i]);
        let group_id: i64 = tx
            .query_row(
                "INSERT INTO transfer_groups (from_account_id, to_account_id, amount_chf, dt)
                 VALUES (?, ?, ?, ?) RETURNING id",
                duckdb::params![
                    out.account_id,
                    in_leg.account_id,
                    (-out.cents) as f64 / 100.0,
                    out.dt.format("%Y-%m-%d").to_string(),
                ],
                |r| r.get(0),
            )
            .context("inserting transfer group")?;
        tx.execute(
            "UPDATE transactions SET transfer_group_id = ? WHERE id IN (?, ?)",
            duckdb::params![group_id, out.id, in_leg.id],
        )
        .context("linking transfer legs to group")?;
    }
    tx.commit()
        .context("committing transfer group transaction")?;
    Ok(())
}

fn reviewed_ids(conn: &Connection) -> anyhow::Result<HashSet<i64>> {
    let mut stmt = conn
        .prepare("SELECT tx_id FROM transfer_review")
        .context("preparing transfer review query")?;
    stmt.query_map([], |r| {
        let tx_id: i64 = r.get(0)?;
        Ok(tx_id)
    })
    .context("reading transfer review query")?
    .collect::<duckdb::Result<HashSet<_>>>()
    .context("reading transfer review query")
}

/// Mark LLM-reviewed legs and audit the call in one transaction.
fn record_review(conn: &mut Connection, tx_ids: &[i64], call_context: &str) -> anyhow::Result<()> {
    let tx = conn
        .transaction()
        .context("starting transfer review transaction")?;
    for id in tx_ids {
        tx.execute(
            "INSERT INTO transfer_review (tx_id) VALUES (?) ON CONFLICT DO NOTHING",
            duckdb::params![id],
        )
        .context("marking transfer leg reviewed")?;
    }
    tx.execute(
        "INSERT INTO llm_calls (context, phase, attempt, ok)
         VALUES (?, 'transfer_pair', 1, 1)",
        duckdb::params![call_context],
    )
    .context("auditing LLM pairing call")?;
    tx.commit()
        .context("committing transfer review transaction")?;
    Ok(())
}

const SYSTEM_PROMPT: &str = "You reconcile funding transfers between the user's own \
accounts. You receive a JSON object with two arrays: `outs` are outflows \
(negative CHF amounts) and `ins` are inflows (positive CHF amounts). Pair an \
outflow with an inflow only if the inflow is the arrival of exactly that \
outflow: identical amount, inflow date on or after the outflow date and at \
most 3 days later, and matching destination: outflows whose description \
mentions Revolut arrive on the revolut account, outflows whose description \
mentions Swisscard arrive on the cashback account. Respond with ONLY a JSON \
array of objects like {\"out\": 0, \"in\": 1} using integer indices into the \
given arrays. Respond with [] when no valid pair exists. Do not use markdown \
or commentary.";

fn build_prompt(outs: &[&Leg], ins: &[&Leg]) -> String {
    let render = |legs: &[&Leg]| {
        legs.iter()
            .enumerate()
            .map(|(index, leg)| {
                json!({
                    "index": index,
                    "date": leg.dt.format("%Y-%m-%d").to_string(),
                    "amount": leg.cents as f64 / 100.0,
                    "description": leg.description,
                })
            })
            .collect::<Vec<Value>>()
    };
    json!({ "outs": render(outs), "ins": render(ins) }).to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
struct LlmPair {
    out: usize,
    #[serde(rename = "in")]
    in_idx: usize,
}

fn strip_code_fence(response: &str) -> &str {
    let json = response.trim();
    match json.strip_prefix("```") {
        Some(json) => json
            .strip_prefix("json")
            .or_else(|| json.strip_prefix("JSON"))
            .unwrap_or(json)
            .trim_end_matches("```")
            .trim(),
        None => json,
    }
}

fn parse_pairs(response: &str, n_outs: usize, n_ins: usize) -> anyhow::Result<Vec<LlmPair>> {
    let json = strip_code_fence(response);
    let pairs: Vec<LlmPair> = serde_json::from_str(json)
        .with_context(|| format!("LLM pairing response is not a JSON array: {response:?}"))?;
    for (i, pair) in pairs.iter().enumerate() {
        ensure!(
            pair.out < n_outs,
            "LLM pair {i} references out index {} of {n_outs}",
            pair.out
        );
        ensure!(
            pair.in_idx < n_ins,
            "LLM pair {i} references in index {} of {n_ins}",
            pair.in_idx
        );
    }
    Ok(pairs)
}

/// Strict validation of LLM proposals: the LLM may only confirm pairs the
/// deterministic rules would accept with a date gap of 0-3 days.
fn validate_pairs(
    proposals: &[LlmPair],
    outs: &[&Leg],
    ins: &[&Leg],
) -> anyhow::Result<Vec<(usize, usize)>> {
    let mut used_out: HashSet<usize> = HashSet::new();
    let mut used_in: HashSet<usize> = HashSet::new();
    let mut pairs = Vec::with_capacity(proposals.len());
    for (i, pair) in proposals.iter().enumerate() {
        let (out, in_leg) = (&outs[pair.out], &ins[pair.in_idx]);
        ensure!(
            out.scope.is_some() && out.scope == in_leg.scope,
            "LLM pair {i} (out #{} / in #{}): no shared scope",
            pair.out,
            pair.in_idx
        );
        ensure!(
            -out.cents == in_leg.cents,
            "LLM pair {i} (out #{} / in #{}): amounts differ ({} vs {})",
            pair.out,
            pair.in_idx,
            out.cents,
            in_leg.cents
        );
        ensure!(
            in_leg.dt >= out.dt,
            "LLM pair {i} (out #{} / in #{}): inflow {} is before outflow {}",
            pair.out,
            pair.in_idx,
            in_leg.dt,
            out.dt
        );
        let gap = (in_leg.dt - out.dt).num_days();
        ensure!(
            gap <= MAX_DELAY_DAYS,
            "LLM pair {i} (out #{} / in #{}): {gap} days apart, max {MAX_DELAY_DAYS}",
            pair.out,
            pair.in_idx
        );
        ensure!(
            used_out.insert(pair.out),
            "LLM pair {i} reuses out #{}",
            pair.out
        );
        ensure!(
            used_in.insert(pair.in_idx),
            "LLM pair {i} reuses in #{}",
            pair.in_idx
        );
        pairs.push((pair.out, pair.in_idx));
    }
    Ok(pairs)
}

/// Pair all ungrouped transfer legs: deterministic rules first, then LLM
/// review for the remaining scoped candidates. Idempotent: a re-run on a
/// fully paired database performs no writes and no LLM calls.
pub async fn pair_transfers(conn: &mut Connection, config: &Config) -> anyhow::Result<PairReport> {
    let (outs, ins) = load_legs(conn)?;
    if outs.is_empty() && ins.is_empty() {
        return Ok(PairReport {
            deterministic_pairs: 0,
            llm_pairs: 0,
            llm_batches: 0,
            unpaired_legs: 0,
        });
    }

    let det = deterministic_pairs(&outs, &ins);
    persist_pairs(conn, &outs, &ins, &det)?;

    let paired_out: HashSet<usize> = det.iter().map(|(o, _)| *o).collect();
    let paired_in: HashSet<usize> = det.iter().map(|(_, i)| *i).collect();
    let reviewed = reviewed_ids(conn)?;
    let out_cands: Vec<usize> = (0..outs.len())
        .filter(|i| {
            !paired_out.contains(i) && outs[*i].scope.is_some() && !reviewed.contains(&outs[*i].id)
        })
        .collect();
    let mut in_pool: Vec<usize> = (0..ins.len())
        .filter(|i| {
            !paired_in.contains(i) && ins[*i].scope.is_some() && !reviewed.contains(&ins[*i].id)
        })
        .collect();

    let mut llm_pairs = 0;
    let mut llm_batches = 0;
    if !out_cands.is_empty() && !in_pool.is_empty() {
        let llm = Llm::new(config);
        for (batch_number, chunk) in out_cands.chunks(BATCH_SIZE).enumerate() {
            let batch_outs: Vec<&Leg> = chunk.iter().map(|&i| &outs[i]).collect();
            let batch_ins: Vec<&Leg> = in_pool.iter().map(|&i| &ins[i]).collect();
            let messages = [
                Message::system(SYSTEM_PROMPT),
                Message::user(build_prompt(&batch_outs, &batch_ins)),
            ];
            let response = llm.complete(&messages).await.with_context(|| {
                format!(
                    "LLM transfer pairing review failed for batch {}",
                    batch_number + 1
                )
            })?;
            let proposals = parse_pairs(&response, batch_outs.len(), batch_ins.len())?;
            let local = validate_pairs(&proposals, &batch_outs, &batch_ins)?;
            let global: Vec<(usize, usize)> = local
                .iter()
                .map(|(o, i)| (chunk[*o], in_pool[*i]))
                .collect();
            persist_pairs(conn, &outs, &ins, &global)?;
            let reviewed: Vec<i64> = batch_outs
                .iter()
                .map(|l| l.id)
                .chain(batch_ins.iter().map(|l| l.id))
                .collect();
            record_review(
                conn,
                &reviewed,
                &format!("transfer pair batch {}", batch_number + 1),
            )?;
            let used_in: HashSet<usize> = local.iter().map(|(_, i)| *i).collect();
            in_pool.retain(|i| !used_in.contains(i));
            llm_pairs += local.len();
            llm_batches += 1;
        }
    }

    let unpaired: i64 = conn
        .query_row(
            "SELECT count(*) FROM transactions
              WHERE kind IN ('transfer_out', 'transfer_in')
                AND transfer_group_id IS NULL",
            [],
            |r| r.get(0),
        )
        .context("counting unpaired transfer legs")?;

    Ok(PairReport {
        deterministic_pairs: det.len(),
        llm_pairs,
        llm_batches,
        unpaired_legs: unpaired as usize,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::routing::post;
    use axum::{Json, Router};
    use std::net::SocketAddr;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;

    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn leg(
        id: i64,
        account_id: i64,
        dt: &str,
        cents: i64,
        description: &str,
        scope: Option<&'static str>,
    ) -> Leg {
        Leg {
            id,
            account_id,
            dt: NaiveDate::parse_from_str(dt, "%Y-%m-%d").unwrap(),
            cents,
            description: description.into(),
            scope,
        }
    }

    #[test]
    fn deterministic_pairing_is_one_to_one_and_scoped() {
        let outs = vec![
            leg(
                1,
                1,
                "2025-01-01",
                -10000,
                "Payment to Revolut",
                Some("revolut"),
            ),
            leg(
                2,
                1,
                "2025-01-01",
                -25000,
                "Swisscard AECS",
                Some("cashback"),
            ),
            // Same bucket as out 1: only one in leg exists, out 1 wins.
            leg(3, 1, "2025-01-01", -10000, "Revolut", Some("revolut")),
            // Cross-scope and date/amount mismatches: no partner.
            leg(
                4,
                1,
                "2025-01-02",
                -10000,
                "Swisscard AECS",
                Some("cashback"),
            ),
            leg(
                5,
                1,
                "2025-01-03",
                -9999,
                "Payment to Revolut",
                Some("revolut"),
            ),
            // No scope: never paired.
            leg(6, 1, "2025-01-04", -100000, "Interactive Brokers", None),
        ];
        let ins = vec![
            leg(
                7,
                2,
                "2025-01-01",
                10000,
                "Payment from Neon",
                Some("revolut"),
            ),
            leg(
                8,
                3,
                "2025-01-01",
                25000,
                "YOUR PAYMENT (DD)",
                Some("cashback"),
            ),
            leg(9, 2, "2025-01-04", 100000, "topup", Some("revolut")),
        ];
        assert_eq!(deterministic_pairs(&outs, &ins), vec![(0, 0), (1, 1)]);
    }

    #[test]
    fn deterministic_pairing_catches_settlement_delay() {
        // One day late: the same transfer, must pair.
        let outs = [leg(1, 1, "2024-07-16", -50000, "Revolut", Some("revolut"))];
        let ins = [
            leg(
                2,
                2,
                "2024-07-17",
                50000,
                "Payment from Neon",
                Some("revolut"),
            ),
            // Same amount but outside the 3-day window: never paired.
            leg(
                3,
                2,
                "2024-07-20",
                50000,
                "Payment from Neon",
                Some("revolut"),
            ),
        ];
        assert_eq!(deterministic_pairs(&outs, &ins), vec![(0, 0)]);

        // An inflow that lands *before* the outflow is a different transfer
        // even with the exact amount (the 2023-09 310 trap in the corpus).
        let outs = [leg(1, 1, "2023-09-22", -31000, "Revolut", Some("revolut"))];
        let ins = [leg(
            2,
            2,
            "2023-09-18",
            31000,
            "Payment from Neon",
            Some("revolut"),
        )];
        assert_eq!(
            deterministic_pairs(&outs, &ins),
            Vec::<(usize, usize)>::new()
        );

        // Same-day partners win over late ones; each in leg is used once.
        let outs = [
            leg(1, 1, "2025-01-01", -50000, "Revolut", Some("revolut")),
            leg(2, 1, "2025-01-02", -50000, "Revolut", Some("revolut")),
        ];
        let ins = [
            leg(3, 2, "2025-01-02", 50000, "topup", Some("revolut")),
            leg(4, 2, "2025-01-03", 50000, "topup", Some("revolut")),
        ];
        assert_eq!(deterministic_pairs(&outs, &ins), vec![(1, 0), (0, 1)]);
    }

    #[test]
    fn scopes_come_from_description_and_source() {
        assert_eq!(out_scope("Revolut Bank UAB"), Some("revolut"));
        assert_eq!(out_scope("swisscard aecs"), Some("cashback"));
        assert_eq!(out_scope("Interactive Brokers"), None);
        assert_eq!(in_scope("revolut"), Some("revolut"));
        assert_eq!(in_scope("cashback"), Some("cashback"));
        assert_eq!(in_scope("neon"), None);
    }

    #[test]
    fn llm_validation_rejects_invalid_pairs() {
        let outs = [
            leg(
                1,
                1,
                "2025-01-01",
                -50000,
                "Payment to Revolut",
                Some("revolut"),
            ),
            leg(
                2,
                1,
                "2025-01-01",
                -50000,
                "Payment to Revolut",
                Some("revolut"),
            ),
        ];
        let valid = leg(2, 2, "2025-01-02", 50000, "topup", Some("revolut"));
        let late = leg(3, 2, "2025-01-05", 50000, "topup", Some("revolut"));
        let wrong_amount = leg(4, 2, "2025-01-02", 50001, "topup", Some("revolut"));
        let cross_scope = leg(5, 3, "2025-01-02", 50000, "topup", Some("cashback"));
        let before = leg(6, 2, "2024-12-31", 50000, "topup", Some("revolut"));

        let outs_refs: &[&Leg] = &[&outs[0], &outs[1]];
        let ok = validate_pairs(&[LlmPair { out: 0, in_idx: 0 }], outs_refs, &[&valid]).unwrap();
        assert_eq!(ok, vec![(0, 0)]);

        let cases: [(&[LlmPair], &[&Leg], &str); 6] = [
            (&[LlmPair { out: 0, in_idx: 0 }], &[&late], "4 days apart"),
            (
                &[LlmPair { out: 0, in_idx: 0 }],
                &[&wrong_amount],
                "amounts differ",
            ),
            (
                &[LlmPair { out: 0, in_idx: 0 }],
                &[&cross_scope],
                "no shared scope",
            ),
            (
                &[LlmPair { out: 0, in_idx: 0 }],
                &[&before],
                "is before outflow",
            ),
            (
                &[LlmPair { out: 0, in_idx: 0 }, LlmPair { out: 0, in_idx: 0 }],
                &[&valid, &late],
                "reuses out",
            ),
            (
                &[LlmPair { out: 0, in_idx: 0 }, LlmPair { out: 1, in_idx: 0 }],
                &[&valid, &late],
                "reuses in",
            ),
        ];
        for (proposals, ins, expected) in cases {
            let err = validate_pairs(proposals, outs_refs, ins)
                .unwrap_err()
                .to_string();
            assert!(err.contains(expected), "{err} !~ {expected}");
        }
    }

    #[test]
    fn parse_pairs_strips_fences_and_bounds_indexes() {
        let parsed = parse_pairs(r#"[{"out":0,"in":1}]"#, 1, 2).unwrap();
        assert_eq!(parsed, vec![LlmPair { out: 0, in_idx: 1 }]);
        let fenced = parse_pairs("```json\n[{\"out\":0,\"in\":0}]\n```", 1, 1).unwrap();
        assert_eq!(fenced, vec![LlmPair { out: 0, in_idx: 0 }]);
        assert!(parse_pairs("[]", 0, 0).unwrap().is_empty());
        assert!(parse_pairs("nope", 1, 1).is_err());
        assert!(parse_pairs(r#"[{"out":1,"in":0}]"#, 1, 1).is_err());
        assert!(parse_pairs(r#"[{"out":0,"in":1}]"#, 1, 1).is_err());
    }

    fn temp_dir() -> std::path::PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("spend-pair-{n}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn config(base_url: &str, db_path: &std::path::Path) -> Config {
        Config {
            db_path: db_path.to_path_buf(),
            llm_provider: spend_core::config::LlmProvider::Local,
            llm_base_url: base_url.to_string(),
            llm_api_key: "test".to_string(),
            llm_model: "mock".to_string(),
            gemini_api_key: None,
            gemini_model: "unused".to_string(),
            fx_base_url: "unused".to_string(),
        }
    }

    /// Mock LLM that returns `content` verbatim and records every request
    /// body for later assertions.
    async fn mock_llm(content: String, requests: Arc<Mutex<Vec<String>>>) -> String {
        let app = Router::new().route(
            "/v1/chat/completions",
            post(move |Json(body): Json<serde_json::Value>| {
                let requests = requests.clone();
                let content = content.clone();
                async move {
                    requests
                        .lock()
                        .unwrap()
                        .push(serde_json::to_string(&body).unwrap());
                    Json(json!({
                        "choices": [{ "message": { "content": content } }]
                    }))
                }
            }),
        );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address: SocketAddr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        format!("http://{address}/v1")
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_leg(
        conn: &Connection,
        source_key: &str,
        account_id: i64,
        source: &str,
        dt: &str,
        description: &str,
        amount: f64,
        kind: &str,
    ) {
        conn.execute(
            "INSERT INTO transactions
                (account_id, source, source_key, dt, description,
                 amount_orig, currency_orig, amount_chf, kind)
             VALUES (?, ?, ?, ?, ?, ?, 'CHF', ?, ?)",
            duckdb::params![
                account_id,
                source,
                source_key,
                dt,
                description,
                amount,
                amount,
                kind
            ],
        )
        .unwrap();
    }

    /// Seed the standard scenario: two same-day deterministic pairs, one
    /// settlement-late deterministic pair, one IB outflow (no partner) and
    /// one fully unpaired leg pair.
    fn seed_standard(conn: &Connection) {
        insert_leg(
            conn,
            "o1",
            1,
            "neon",
            "2025-03-01",
            "Payment to Revolut",
            -100.0,
            "transfer_out",
        );
        insert_leg(
            conn,
            "i1",
            2,
            "revolut",
            "2025-03-01",
            "Payment from Neon",
            100.0,
            "transfer_in",
        );
        insert_leg(
            conn,
            "o2",
            1,
            "neon",
            "2025-03-05",
            "Swisscard AECS",
            -250.0,
            "transfer_out",
        );
        insert_leg(
            conn,
            "i2",
            3,
            "cashback",
            "2025-03-05",
            "YOUR PAYMENT (DD) - THANK YOU",
            250.0,
            "transfer_in",
        );
        insert_leg(
            conn,
            "o3",
            1,
            "neon",
            "2025-03-10",
            "Payment to Revolut",
            -500.0,
            "transfer_out",
        );
        insert_leg(
            conn,
            "i3",
            2,
            "revolut",
            "2025-03-11",
            "Payment from Neon",
            500.0,
            "transfer_in",
        );
        insert_leg(
            conn,
            "o4",
            1,
            "neon",
            "2025-03-12",
            "Interactive Brokers",
            -1000.0,
            "transfer_out",
        );
        insert_leg(
            conn,
            "o5",
            1,
            "neon",
            "2025-03-20",
            "Payment to Revolut",
            -77.77,
            "transfer_out",
        );
        insert_leg(
            conn,
            "i4",
            2,
            "revolut",
            "2025-03-21",
            "Payment from Neon",
            321.0,
            "transfer_in",
        );
    }

    fn count(conn: &Connection, sql: &str) -> i64 {
        conn.query_row(sql, [], |r| r.get(0)).unwrap()
    }

    #[tokio::test]
    async fn pairing_groups_deterministic_and_llm_pairs_and_is_idempotent() {
        let requests = Arc::new(Mutex::new(Vec::new()));
        // The only unpaired scoped candidates (o5/i4) have mismatching
        // amounts, so the review correctly confirms no pair.
        let url = mock_llm("[]".to_string(), requests.clone()).await;
        let dir = temp_dir();
        let db_path = dir.join("spend.duckdb");
        let mut conn = spend_core::db::ingest_connection(&db_path).unwrap();
        seed_standard(&conn);

        let report = pair_transfers(&mut conn, &config(&url, &db_path))
            .await
            .unwrap();
        assert_eq!(
            report,
            PairReport {
                deterministic_pairs: 3,
                llm_pairs: 0,
                llm_batches: 1,
                unpaired_legs: 3,
            }
        );

        let mut stmt = conn
            .prepare(
                "SELECT from_account_id, to_account_id, amount_chf, dt
                   FROM transfer_groups ORDER BY id",
            )
            .unwrap();
        let groups: Vec<(i64, i64, f64, String)> = stmt
            .query_map([], |r| {
                let from: i64 = r.get(0)?;
                let to: i64 = r.get(1)?;
                let amount: f64 = r.get(2)?;
                let dt: String = r.get(3)?;
                Ok((from, to, amount, dt))
            })
            .unwrap()
            .collect::<duckdb::Result<Vec<_>>>()
            .unwrap();
        assert_eq!(
            groups,
            vec![
                (1, 2, 100.0, "2025-03-01".into()),
                (1, 3, 250.0, "2025-03-05".into()),
                (1, 2, 500.0, "2025-03-10".into()),
            ]
        );
        let linked: i64 = count(
            &conn,
            "SELECT count(*) FROM transactions
              WHERE transfer_group_id IS NOT NULL",
        );
        assert_eq!(linked, 6);
        let ib_ungrouped: i64 = count(
            &conn,
            "SELECT count(*) FROM transactions
              WHERE source_key = 'o4' AND transfer_group_id IS NULL
                AND kind = 'transfer_out'",
        );
        assert_eq!(ib_ungrouped, 1);

        // The IB outflow never reaches the LLM; the prompt indexes the two
        // unpaired scoped candidates only.
        let prompts = requests.lock().unwrap().clone();
        assert_eq!(prompts.len(), 1);
        let body: serde_json::Value = serde_json::from_str(&prompts[0]).unwrap();
        let user = body["messages"][1]["content"].as_str().unwrap();
        assert!(!user.contains("Interactive Brokers"));
        let prompt: serde_json::Value = serde_json::from_str(user).unwrap();
        assert_eq!(prompt["outs"].as_array().unwrap().len(), 1);
        assert_eq!(prompt["ins"].as_array().unwrap().len(), 1);

        // Reviewed legs: the two unpaired scoped candidates. The IB leg and
        // the deterministic pairs are not reviewed.
        assert_eq!(count(&conn, "SELECT count(*) FROM transfer_review"), 2);
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM llm_calls WHERE phase = 'transfer_pair'",
            ),
            1
        );

        // Summary: moved is the sum of the three groups (including the
        // settlement-late one); spend stays empty.
        let summary = spend_core::queries::summary(&conn, 2025, None).unwrap();
        assert_eq!(summary.moved, 850.0);
        assert_eq!(summary.spend, 0.0);
        assert_eq!(summary.income, 0.0);

        // Re-run with an unreachable LLM: everything is already paired or
        // reviewed, so no writes and no LLM call may happen.
        let dead = config("http://127.0.0.1:9/v1", &db_path);
        let again = pair_transfers(&mut conn, &dead).await.unwrap();
        assert_eq!(
            again,
            PairReport {
                deterministic_pairs: 0,
                llm_pairs: 0,
                llm_batches: 0,
                unpaired_legs: 3,
            }
        );
        assert_eq!(count(&conn, "SELECT count(*) FROM transfer_groups"), 3);
        assert_eq!(count(&conn, "SELECT count(*) FROM transfer_review"), 2);
        assert_eq!(
            count(
                &conn,
                "SELECT count(*) FROM llm_calls WHERE phase = 'transfer_pair'"
            ),
            1
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn pairing_reviews_new_candidates_when_a_later_statement_arrives() {
        let dir = temp_dir();
        let db_path = dir.join("spend.duckdb");
        let mut conn = spend_core::db::ingest_connection(&db_path).unwrap();
        seed_standard(&conn);

        let requests = Arc::new(Mutex::new(Vec::new()));
        let url = mock_llm("[]".to_string(), requests.clone()).await;
        let first = pair_transfers(&mut conn, &config(&url, &db_path))
            .await
            .unwrap();
        assert_eq!(first.llm_batches, 1);
        assert_eq!(count(&conn, "SELECT count(*) FROM transfer_review"), 2);

        // A later Neon statement funds Revolut two days before the money
        // arrives: the settlement-late pass pairs it without an LLM call.
        // A fresh unpaired candidate pair (o7/i6) must be LLM-reviewed.
        insert_leg(
            &conn,
            "o6",
            1,
            "neon",
            "2025-04-01",
            "Payment to Revolut",
            -300.0,
            "transfer_out",
        );
        insert_leg(
            &conn,
            "i5",
            2,
            "revolut",
            "2025-04-03",
            "Payment from Neon",
            300.0,
            "transfer_in",
        );
        insert_leg(
            &conn,
            "o7",
            1,
            "neon",
            "2025-04-10",
            "Payment to Revolut",
            -42.42,
            "transfer_out",
        );
        insert_leg(
            &conn,
            "i6",
            2,
            "revolut",
            "2025-04-12",
            "Payment from Neon",
            999.0,
            "transfer_in",
        );

        let requests2 = Arc::new(Mutex::new(Vec::new()));
        let url2 = mock_llm("[]".to_string(), requests2.clone()).await;
        let report = pair_transfers(&mut conn, &config(&url2, &db_path))
            .await
            .unwrap();
        assert_eq!(
            report,
            PairReport {
                deterministic_pairs: 1,
                llm_pairs: 0,
                llm_batches: 1,
                unpaired_legs: 5,
            }
        );
        // The first review is not repeated; only o7/i6 go to the LLM.
        let requests2 = requests2.lock().unwrap();
        assert_eq!(requests2.len(), 1);
        let body: serde_json::Value = serde_json::from_str(&requests2[0]).unwrap();
        let user = body["messages"][1]["content"].as_str().unwrap();
        let prompt: serde_json::Value = serde_json::from_str(user).unwrap();
        assert_eq!(prompt["outs"].as_array().unwrap().len(), 1);
        assert_eq!(prompt["ins"].as_array().unwrap().len(), 1);
        assert_eq!(count(&conn, "SELECT count(*) FROM transfer_review"), 4);

        let summary = spend_core::queries::summary(&conn, 2025, None).unwrap();
        assert_eq!(summary.moved, 1150.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    async fn assert_invalid_proposal_fails(
        response: &str,
        seed: impl Fn(&Connection),
        expected: &str,
    ) {
        let dir = temp_dir();
        let db_path = dir.join("spend.duckdb");
        let mut conn = spend_core::db::ingest_connection(&db_path).unwrap();
        seed(&conn);
        let requests = Arc::new(Mutex::new(Vec::new()));
        let url = mock_llm(response.to_string(), requests).await;
        let err = pair_transfers(&mut conn, &config(&url, &db_path))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains(expected), "{err} !~ {expected}");
        assert_eq!(count(&conn, "SELECT count(*) FROM transfer_groups"), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn llm_proposals_are_validated_strictly() {
        let four_days = |conn: &Connection| {
            insert_leg(
                conn,
                "o1",
                1,
                "neon",
                "2025-01-01",
                "Payment to Revolut",
                -500.0,
                "transfer_out",
            );
            insert_leg(
                conn,
                "i1",
                2,
                "revolut",
                "2025-01-05",
                "Payment from Neon",
                500.0,
                "transfer_in",
            );
        };
        assert_invalid_proposal_fails(r#"[{"out":0,"in":0}]"#, four_days, "days apart").await;

        let wrong_amount = |conn: &Connection| {
            insert_leg(
                conn,
                "o1",
                1,
                "neon",
                "2025-01-01",
                "Payment to Revolut",
                -500.0,
                "transfer_out",
            );
            insert_leg(
                conn,
                "i1",
                2,
                "revolut",
                "2025-01-02",
                "Payment from Neon",
                500.01,
                "transfer_in",
            );
        };
        assert_invalid_proposal_fails(r#"[{"out":0,"in":0}]"#, wrong_amount, "amounts differ")
            .await;

        let cross_scope = |conn: &Connection| {
            insert_leg(
                conn,
                "o1",
                1,
                "neon",
                "2025-01-01",
                "Swisscard AECS",
                -500.0,
                "transfer_out",
            );
            insert_leg(
                conn,
                "i1",
                2,
                "revolut",
                "2025-01-02",
                "Payment from Neon",
                500.0,
                "transfer_in",
            );
        };
        assert_invalid_proposal_fails(r#"[{"out":0,"in":0}]"#, cross_scope, "no shared scope")
            .await;

        let before_outflow = |conn: &Connection| {
            insert_leg(
                conn,
                "o1",
                1,
                "neon",
                "2025-01-05",
                "Payment to Revolut",
                -500.0,
                "transfer_out",
            );
            insert_leg(
                conn,
                "i1",
                2,
                "revolut",
                "2025-01-02",
                "Payment from Neon",
                500.0,
                "transfer_in",
            );
        };
        assert_invalid_proposal_fails(r#"[{"out":0,"in":0}]"#, before_outflow, "is before outflow")
            .await;

        // The inflow settles 4 days late, outside the deterministic window,
        // so the LLM path (and its index bounds) is the one under test.
        let out_of_range = |conn: &Connection| {
            insert_leg(
                conn,
                "o1",
                1,
                "neon",
                "2025-01-01",
                "Payment to Revolut",
                -500.0,
                "transfer_out",
            );
            insert_leg(
                conn,
                "i1",
                2,
                "revolut",
                "2025-01-05",
                "Payment from Neon",
                500.0,
                "transfer_in",
            );
        };
        assert_invalid_proposal_fails(r#"[{"out":0,"in":7}]"#, out_of_range, "references in index")
            .await;

        let malformed = |conn: &Connection| {
            insert_leg(
                conn,
                "o1",
                1,
                "neon",
                "2025-01-01",
                "Payment to Revolut",
                -500.0,
                "transfer_out",
            );
            insert_leg(
                conn,
                "i1",
                2,
                "revolut",
                "2025-01-05",
                "Payment from Neon",
                500.0,
                "transfer_in",
            );
        };
        assert_invalid_proposal_fails("sure thing!", malformed, "not a JSON array").await;
    }
}
