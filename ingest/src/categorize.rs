use std::collections::HashSet;

use anyhow::Context;
use serde::Deserialize;
use serde_json::{Value, json};

use spend_core::config::Config;
use spend_core::llm::{Llm, Message};
use spend_core::schema;

/// Rows per LLM batch, as agreed in the spec.
pub const BATCH_SIZE: usize = 60;

#[derive(Debug, Clone)]
pub struct LlmAudit {
    pub context: String,
    pub phase: String,
    pub attempt: i64,
    pub ok: bool,
}

#[derive(Debug, Deserialize)]
struct LlmAssignment {
    index: usize,
    category: String,
}

/// A statement row that can be categorized by the LLM: it contributes one
/// JSON item to the batch prompt and receives the assigned category.
pub trait LlmCategorizable {
    fn needs_category(&self) -> bool;
    fn llm_item(&self) -> Value;
    fn set_category(&mut self, category: String);
}

/// Batch-categorize every row that still needs a category through the
/// configured LLM (thinking disabled, constrained to the fixed taxonomy).
/// Returns one audit entry per batch. Hard-fails when the LLM is required
/// and unreachable, or when it returns an invalid response.
pub async fn categorize_uncategorized<T: LlmCategorizable>(
    rows: &mut [T],
    source: &str,
    config: &Config,
) -> anyhow::Result<Vec<LlmAudit>> {
    let pending: Vec<usize> = (0..rows.len())
        .filter(|index| rows[*index].needs_category())
        .collect();
    if pending.is_empty() {
        return Ok(Vec::new());
    }

    let llm = Llm::new(config);
    let taxonomy = schema::CATEGORIES
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>();
    let mut audits = Vec::new();
    for (batch_number, batch) in pending.chunks(BATCH_SIZE).enumerate() {
        let items = batch
            .iter()
            .enumerate()
            .map(|(index, row_index)| {
                let mut item = rows[*row_index].llm_item();
                item["index"] = json!(index);
                item
            })
            .collect::<Vec<_>>();
        let prompt = serde_json::to_string(&items)?;
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
                "Classify this JSON array of {source} transactions:\n{prompt}"
            )),
        ];

        let response = llm.complete(&messages).await.with_context(|| {
            format!(
                "local LLM categorization failed for {source} batch {}",
                batch_number + 1
            )
        })?;
        let assignments = parse_assignments(&response, batch.len())?;
        for assignment in assignments {
            let row_index = batch[assignment.index];
            rows[row_index].set_category(assignment.category);
        }
        audits.push(LlmAudit {
            context: format!("{source} category batch {}", batch_number + 1),
            phase: format!("{source}_category"),
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
