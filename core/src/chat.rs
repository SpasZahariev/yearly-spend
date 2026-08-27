//! Chat backend for the dashboard inspector: the configured LLM (local
//! llama-server by default, Gemini when `.env` selects it) streams tokens
//! over HTTP SSE and may call a read-only `run_sql` tool plus
//! `render_dashboard`, which pushes computed numbers onto the live
//! dashboard charts.
//!
//! The SQL tool is SELECT-only: statements are parsed and rejected unless
//! they are a single SELECT/WITH-SELECT query, and execution happens on a
//! short-lived read-only DuckDB connection, so the chat can never write to
//! the database. Nothing is persisted; every request starts from scratch.

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Context;
use chrono::NaiveDate;
use duckdb::types::{TimeUnit, Value};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use sqlparser::ast::Statement;
use sqlparser::dialect::DuckDbDialect;
use sqlparser::parser::Parser;
use tokio::sync::mpsc::UnboundedSender;

use crate::config::{Config, LlmProvider};

/// Maximum LLM <-> tool round trips per request; guards against loops.
const MAX_ROUNDS: usize = 8;
/// Result rows returned to the model for one tool call.
const MAX_ROWS: usize = 100;

/// num_days_from_ce of 1970-01-01 (the Arrow/DuckDB DATE epoch).
const UNIX_EPOCH_DAYS: i32 = 719_163;

const TOOL_NAME: &str = "run_sql";
const TOOL_DESCRIPTION: &str = "Run a single read-only SELECT query against the \
spending database (DuckDB) and return up to 100 rows as JSON. Use it to ground \
every data-related answer in actual query results.";

const RENDER_TOOL_NAME: &str = "render_dashboard";
const RENDER_TOOL_DESCRIPTION: &str = "Push numbers you already computed with run_sql \
onto the live dashboard: KPI cards (income/spend/moved), the monthly spend bar \
chart, the yearly spend bar chart, the cumulative spend line, the category \
donut and the money-flow sankey, and also drive the navbar pickers (year, month, \
view, currency) so the dashboard shows the period and currency the user asked \
about. The user sees the data on the dashboard; call it when your answer covers \
data one of these charts can show, then mention in your reply that the \
dashboard now shows it.";

/// One turn of the visible chat history. The frontend sends the whole
/// conversation (oldest first, excluding the current user message) so the
/// model can answer follow-ups. Tool traces are not persisted; only the
/// rendered text the user saw is carried.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChatHistoryEntry {
    pub role: String,
    pub content: String,
}

impl ChatHistoryEntry {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.role == "user" || self.role == "assistant",
            "history role must be 'user' or 'assistant'"
        );
        let trimmed = self.content.trim();
        anyhow::ensure!(!trimmed.is_empty(), "history content is empty");
        anyhow::ensure!(
            trimmed.chars().count() <= 20_000,
            "history content must be 1..20000 chars"
        );
        Ok(())
    }
}

/// Which chart a pinned selection chip came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChartKind {
    Yearly,
    Cumulative,
    Monthly,
    Daily,
    Categories,
    Sankey,
    Summary,
}

impl std::fmt::Display for ChartKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Yearly => "yearly",
            Self::Cumulative => "cumulative",
            Self::Monthly => "monthly",
            Self::Daily => "daily",
            Self::Categories => "categories",
            Self::Sankey => "sankey",
            Self::Summary => "summary",
        })
    }
}

/// A pinned chart selection (bar, slice or point) carried as context with
/// every chat message. Values are the raw CHF numbers from the API, before
/// any client-side currency conversion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Selection {
    pub chart: ChartKind,
    pub series: String,
    pub label: String,
    pub value: f64,
    pub year: Option<i32>,
    pub month: Option<u32>,
    pub category: Option<String>,
    pub note: Option<String>,
}

impl Selection {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(!self.series.trim().is_empty(), "selection series is empty");
        anyhow::ensure!(!self.label.trim().is_empty(), "selection label is empty");
        anyhow::ensure!(self.value.is_finite(), "selection value must be finite");
        if let Some(year) = self.year {
            anyhow::ensure!((1000..=3000).contains(&year), "selection year out of range");
        }
        if let Some(month) = self.month {
            anyhow::ensure!((1..=12).contains(&month), "selection month must be 1-12");
        }
        if let Some(note) = &self.note {
            let trimmed = note.trim();
            anyhow::ensure!(!trimmed.is_empty(), "selection note is empty");
            anyhow::ensure!(
                trimmed.chars().count() <= 500,
                "selection note must be 1-500 chars"
            );
        }
        Ok(())
    }
}

/// One SSE event streamed back to the client.
#[derive(Debug, Clone, PartialEq)]
#[allow(clippy::large_enum_variant)]
pub enum ChatEvent {
    /// A chunk of the assistant's visible reply.
    Token(String),
    /// The assistant is invoking the SQL tool with this statement.
    ToolCall { sql: String },
    /// The assistant pushed chart data onto the dashboard.
    ChartUpdate(DashboardUpdate),
    /// Terminal error; no further events follow.
    Error(String),
}

/// A tool call requested by the model, with its raw JSON arguments.
struct PendingCall {
    id: String,
    name: String,
    arguments: serde_json::Value,
}

/// One completed round trip: assistant output plus the executed tool calls.
struct Round {
    assistant_text: String,
    calls: Vec<ExecutedCall>,
}

struct ExecutedCall {
    id: String,
    name: String,
    arguments: serde_json::Value,
    result: String,
}

/// One point of a month-indexed chart (monthly bars, cumulative line).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MonthPoint {
    /// 1-12.
    pub month: u8,
    /// Positive CHF magnitude.
    pub value: f64,
}

/// One point of the yearly spend bar chart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct YearPoint {
    pub year: i32,
    /// Positive CHF magnitude.
    pub value: f64,
}

/// One slice of the category donut.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CategoryPoint {
    /// Must match a row of the categories table.
    pub name: String,
    /// Positive CHF magnitude.
    pub value: f64,
}

/// KPI card values. All three are required together (validated).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KpiValues {
    pub income: f64,
    pub spend: f64,
    pub moved: f64,
}

/// Chart data pushed to the dashboard by the `render_dashboard` tool.
/// All amounts are positive CHF magnitudes; the frontend merges them over
/// the standard API data for the matching year and drives navbar pickers
/// (year, month, view, currency) plus the sankey category filter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DashboardUpdate {
    pub year: i32,
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kpi: Option<KpiValues>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monthly: Option<Vec<MonthPoint>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub yearly: Option<Vec<YearPoint>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cumulative: Option<Vec<MonthPoint>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub categories: Option<Vec<CategoryPoint>>,
    /// Target display currency for the navbar (CHF, USD, EUR). When set the
    /// frontend switches the currency picker; the numeric payload stays in CHF
    /// and the client converts for display.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
    /// 1-12 month the data is scoped to. When set the frontend switches to
    /// that month; omit for year-scoped payloads.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub month: Option<u8>,
    /// "month" or "year" – drives the navbar view toggle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view: Option<String>,
    /// Subset of category names to show as outflows in the money-flow sankey.
    /// When omitted the sankey mirrors `categories` if that is present;
    /// otherwise it shows all categories. Names must be from the categories table.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sankey: Option<Vec<String>>,
}

impl DashboardUpdate {
    /// Validate a model-supplied update against the fixed taxonomy and the
    /// shape constraints of the dashboard charts. Empty sections are treated
    /// as absent.
    pub fn validate(&self, category_names: &HashSet<String>) -> Result<(), String> {
        if !(1000..=3000).contains(&self.year) {
            return Err("year must be between 1000 and 3000".into());
        }
        let label = self.label.trim();
        if label.is_empty() || label.chars().count() > 40 {
            return Err("label must be 1-40 chars".into());
        }
        let section_present = [
            self.kpi.is_some(),
            !self.monthly.as_deref().unwrap_or_default().is_empty(),
            !self.yearly.as_deref().unwrap_or_default().is_empty(),
            !self.cumulative.as_deref().unwrap_or_default().is_empty(),
            !self.categories.as_deref().unwrap_or_default().is_empty(),
            !self.sankey.as_deref().unwrap_or_default().is_empty(),
        ];
        if !section_present.into_iter().any(|present| present) {
            return Err(
                "at least one chart section (kpi, monthly, yearly, cumulative, categories, sankey) is required".into(),
            );
        }
        if let Some(kpi) = &self.kpi {
            for (name, value) in [
                ("income", kpi.income),
                ("spend", kpi.spend),
                ("moved", kpi.moved),
            ] {
                if !value.is_finite() || value < 0.0 {
                    return Err(format!("kpi.{name} must be a finite non-negative number"));
                }
            }
        }
        for (name, points) in [("monthly", &self.monthly), ("cumulative", &self.cumulative)] {
            let Some(points) = points else {
                continue;
            };
            if points.len() > 12 {
                return Err(format!("{name} holds at most 12 points"));
            }
            let mut seen = [false; 13];
            for point in points {
                if !(1..=12).contains(&point.month) {
                    return Err(format!("{name} month must be 1-12"));
                }
                if seen[point.month as usize] {
                    return Err(format!("duplicate month {} in {name}", point.month));
                }
                seen[point.month as usize] = true;
                if !point.value.is_finite() || point.value < 0.0 {
                    return Err(format!("{name} values must be finite non-negative numbers"));
                }
            }
        }
        if let Some(yearly) = &self.yearly {
            if yearly.len() > 30 {
                return Err("yearly holds at most 30 points".into());
            }
            let mut years: HashSet<i32> = HashSet::new();
            for point in yearly {
                if !(1000..=3000).contains(&point.year) {
                    return Err("yearly years must be between 1000 and 3000".into());
                }
                if !years.insert(point.year) {
                    return Err(format!("duplicate year {} in yearly", point.year));
                }
                if !point.value.is_finite() || point.value < 0.0 {
                    return Err("yearly values must be finite non-negative numbers".into());
                }
            }
        }
        if let Some(categories) = &self.categories {
            if categories.len() > 18 {
                return Err("categories holds at most 18 entries".into());
            }
            let mut seen: HashSet<&str> = HashSet::new();
            for entry in categories {
                let name = entry.name.trim();
                if name.is_empty() || !category_names.contains(name) {
                    return Err(format!(
                        "unknown category '{}'; use names from the categories table",
                        entry.name
                    ));
                }
                if !seen.insert(name) {
                    return Err(format!("duplicate category '{name}'"));
                }
                if !entry.value.is_finite() || entry.value < 0.0 {
                    return Err(format!(
                        "categories value for '{name}' must be finite non-negative"
                    ));
                }
            }
        }
        if let Some(currency) = &self.currency {
            let normalized = currency.trim().to_ascii_uppercase();
            if !["CHF", "USD", "EUR"].contains(&normalized.as_str()) {
                return Err("currency must be CHF, USD or EUR".into());
            }
        }
        if let Some(month) = self.month
            && !(1..=12).contains(&month)
        {
            return Err("month must be 1-12".into());
        }
        if let Some(view) = &self.view
            && view != "month"
            && view != "year"
        {
            return Err("view must be 'month' or 'year'".into());
        }
        if let Some(sankey) = &self.sankey {
            if sankey.len() > 18 {
                return Err("sankey holds at most 18 entries".into());
            }
            let mut seen: HashSet<&str> = HashSet::new();
            for name in sankey {
                let trimmed = name.trim();
                if trimmed.is_empty() || !category_names.contains(trimmed) {
                    return Err(format!(
                        "unknown sankey category '{}'; use names from the categories table",
                        name
                    ));
                }
                if !seen.insert(trimmed) {
                    return Err(format!("duplicate sankey category '{}'", trimmed));
                }
            }
        }
        Ok(())
    }
}

struct StreamOutcome {
    text: String,
    calls: Vec<PendingCall>,
}

/// Chat client bound to one provider configuration and one database file.
#[derive(Debug, Clone)]
pub struct Chat {
    http: reqwest::Client,
    provider: LlmProvider,
    base_url: String,
    api_key: String,
    local_model: String,
    gemini_base_url: String,
    gemini_api_key: String,
    gemini_model: String,
    db_path: PathBuf,
}

impl Chat {
    pub fn new(cfg: &Config) -> Self {
        Self {
            http: reqwest::Client::new(),
            provider: cfg.llm_provider,
            base_url: cfg.llm_base_url.trim_end_matches('/').to_string(),
            api_key: cfg.llm_api_key.clone(),
            local_model: cfg.llm_model.clone(),
            gemini_base_url: cfg.gemini_base_url.trim_end_matches('/').to_string(),
            gemini_api_key: cfg.gemini_api_key.clone().unwrap_or_default(),
            gemini_model: crate::config::effective_model(cfg),
            db_path: cfg.db_path.clone(),
        }
    }

    /// Run the chat loop for one user message, streaming events to `tx`.
    /// `history` is the prior visible conversation (oldest first) and is
    /// replayed verbatim so follow-ups have context. Terminates with
    /// `ChatEvent::Error` on failure or after `MAX_ROUNDS` tool round trips;
    /// on success it simply ends when the model stops calling tools.
    pub async fn run(
        &self,
        message: &str,
        selections: Vec<Selection>,
        history: Vec<ChatHistoryEntry>,
        tx: UnboundedSender<ChatEvent>,
    ) {
        let system = build_system_prompt(selections);
        let mut rounds: Vec<Round> = Vec::new();
        for _ in 0..MAX_ROUNDS {
            let outcome = match self.provider {
                LlmProvider::Local => {
                    self.stream_local(&system, &history, message, &rounds, &tx)
                        .await
                }
                LlmProvider::Gemini => {
                    self.stream_gemini(&system, &history, message, &rounds, &tx)
                        .await
                }
            };
            let outcome = match outcome {
                Ok(outcome) => outcome,
                Err(err) => {
                    let _ = tx.send(ChatEvent::Error(err.to_string()));
                    return;
                }
            };
            if outcome.calls.is_empty() {
                return;
            }
            let mut executed = Vec::new();
            for call in outcome.calls {
                let result = self.exec_call(&call, &tx).await;
                executed.push(ExecutedCall {
                    id: call.id,
                    name: call.name,
                    arguments: call.arguments,
                    result,
                });
            }
            rounds.push(Round {
                assistant_text: outcome.text,
                calls: executed,
            });
        }
        let _ = tx.send(ChatEvent::Error(
            "stopped: too many tool round trips".into(),
        ));
    }

    async fn exec_call(&self, call: &PendingCall, tx: &UnboundedSender<ChatEvent>) -> String {
        match call.name.as_str() {
            TOOL_NAME => {
                let sql = call
                    .arguments
                    .get("sql")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default()
                    .to_string();
                let _ = tx.send(ChatEvent::ToolCall { sql: sql.clone() });
                if sql.trim().is_empty() {
                    return "error: missing sql argument".into();
                }
                if let Err(reason) = validate_select_only(&sql) {
                    return format!("rejected: {reason}");
                }
                match run_readonly_sql(self.db_path.clone(), sql).await {
                    Ok(result) => result,
                    Err(err) => format!("error: {err}"),
                }
            }
            RENDER_TOOL_NAME => self.exec_render(&call.arguments, tx).await,
            other => format!("error: unknown tool '{other}'"),
        }
    }

    /// Validate a `render_dashboard` call and, when it is valid, stream the
    /// update to the client as a `chart` event. The returned string is the
    /// tool result fed back to the model.
    async fn exec_render(
        &self,
        arguments: &serde_json::Value,
        tx: &UnboundedSender<ChatEvent>,
    ) -> String {
        let update: DashboardUpdate = match serde_json::from_value(arguments.clone()) {
            Ok(update) => update,
            Err(err) => return format!("error: invalid render_dashboard arguments: {err}"),
        };
        let category_names = match load_category_names(self.db_path.clone()).await {
            Ok(names) => names,
            Err(err) => return format!("error: {err}"),
        };
        match update.validate(&category_names) {
            Ok(()) => {
                let _ = tx.send(ChatEvent::ChartUpdate(update.clone()));
                format!(
                    "ok: dashboard charts updated for {} (year {})",
                    update.label, update.year
                )
            }
            Err(reason) => format!("rejected: {reason}"),
        }
    }

    async fn stream_local(
        &self,
        system: &str,
        history: &[ChatHistoryEntry],
        user: &str,
        rounds: &[Round],
        tx: &UnboundedSender<ChatEvent>,
    ) -> anyhow::Result<StreamOutcome> {
        let body = serde_json::json!({
            "model": self.local_model,
            "messages": local_messages(system, history, user, rounds),
            "temperature": 0.2,
            "stream": true,
            "tools": [tool_spec_local(), render_tool_spec_local()],
            "chat_template_kwargs": { "enable_thinking": false },
        });
        anyhow::ensure!(!self.local_model.is_empty(), "LLM_MODEL is not set");
        let response = self
            .http
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let mut outcome = StreamOutcome {
            text: String::new(),
            calls: Vec::new(),
        };
        let mut acc: Vec<MutCall> = Vec::new();
        let mut sse = SseLines::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("reading LLM stream failed")?;
            for data in sse.push(&chunk) {
                let value: serde_json::Value =
                    serde_json::from_str(&data).context("malformed SSE payload from LLM")?;
                let Some(choice) = value["choices"].as_array().and_then(|c| c.first()) else {
                    continue;
                };
                let delta = &choice["delta"];
                if let Some(text) = delta["content"].as_str()
                    && !text.is_empty()
                {
                    outcome.text.push_str(text);
                    let _ = tx.send(ChatEvent::Token(text.to_string()));
                }
                if let Some(items) = delta["tool_calls"].as_array() {
                    for item in items {
                        let Some(raw) = item["index"].as_u64() else {
                            continue;
                        };
                        if raw >= 64 {
                            continue;
                        }
                        let index = raw as usize;
                        while acc.len() <= index {
                            acc.push(MutCall {
                                id: None,
                                name: String::new(),
                                arguments: String::new(),
                            });
                        }
                        let slot = &mut acc[index];
                        if let Some(id) = item["id"].as_str() {
                            slot.id = Some(id.to_string());
                        }
                        if let Some(name) = item["function"]["name"].as_str() {
                            slot.name.push_str(name);
                        }
                        if let Some(part) = item["function"]["arguments"].as_str() {
                            slot.arguments.push_str(part);
                        }
                    }
                }
            }
        }
        for (i, slot) in acc.into_iter().enumerate() {
            let args: serde_json::Value = if slot.arguments.is_empty() {
                serde_json::Value::Object(Default::default())
            } else {
                serde_json::from_str(&slot.arguments)
                    .unwrap_or(serde_json::Value::Object(Default::default()))
            };
            outcome.calls.push(PendingCall {
                id: slot.id.unwrap_or_else(|| format!("call_local_{i}")),
                name: slot.name,
                arguments: args,
            });
        }
        Ok(outcome)
    }

    async fn stream_gemini(
        &self,
        system: &str,
        history: &[ChatHistoryEntry],
        user: &str,
        rounds: &[Round],
        tx: &UnboundedSender<ChatEvent>,
    ) -> anyhow::Result<StreamOutcome> {
        anyhow::ensure!(
            !self.gemini_api_key.is_empty(),
            "GEMINI_API_KEY is not set for provider 'gemini'"
        );
        let body = serde_json::json!({
            "contents": gemini_contents(history, user, rounds),
            "system_instruction": { "parts": [{ "text": system }] },
            "tools": [{
                "function_declarations": [tool_spec_gemini(), render_tool_spec_gemini()]
            }],
            "generation_config": { "temperature": 0.2 },
        });
        let response = self
            .http
            .post(format!(
                "{}/models/{}:streamGenerateContent?alt=sse",
                self.gemini_base_url, self.gemini_model
            ))
            .query(&[("key", &self.gemini_api_key)])
            .json(&body)
            .send()
            .await?
            .error_for_status()?;
        let mut outcome = StreamOutcome {
            text: String::new(),
            calls: Vec::new(),
        };
        let mut sse = SseLines::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("reading LLM stream failed")?;
            for data in sse.push(&chunk) {
                let value: serde_json::Value =
                    serde_json::from_str(&data).context("malformed SSE payload from LLM")?;
                let candidates = value["candidates"].as_array().cloned().unwrap_or_default();
                for candidate in candidates {
                    let parts = candidate["content"]["parts"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default();
                    for part in parts {
                        if let Some(text) = part.get("text").and_then(|v| v.as_str())
                            && !text.is_empty()
                        {
                            outcome.text.push_str(text);
                            let _ = tx.send(ChatEvent::Token(text.to_string()));
                        }
                        if let Some(call) = part.get("function_call") {
                            let name = call["name"].as_str().unwrap_or_default().to_string();
                            let args = call.get("args").cloned().unwrap_or_default();
                            outcome.calls.push(PendingCall {
                                id: format!("call_gemini_{}", outcome.calls.len()),
                                name,
                                arguments: args,
                            });
                        }
                    }
                }
            }
        }
        Ok(outcome)
    }
}

#[derive(Default)]
struct MutCall {
    id: Option<String>,
    name: String,
    arguments: String,
}

/// Incremental SSE `data:` payload splitter (handles payloads split across
/// TCP chunks). `[DONE]` sentinels and empty payloads are dropped.
#[derive(Default)]
struct SseLines {
    buffer: String,
}

impl SseLines {
    fn new() -> Self {
        Self::default()
    }

    fn push(&mut self, chunk: &[u8]) -> Vec<String> {
        self.buffer.push_str(&String::from_utf8_lossy(chunk));
        let mut payloads = Vec::new();
        while let Some(pos) = self.buffer.find('\n') {
            let line = self.buffer[..pos].trim_end_matches('\r').to_string();
            self.buffer.drain(..pos + 1);
            let data = line.trim().strip_prefix("data:").map(str::trim);
            if let Some(data) = data
                && !data.is_empty()
                && data != "[DONE]"
            {
                payloads.push(data.to_string());
            }
        }
        payloads
    }
}

/// Refuse anything that is not a single SELECT (or WITH-SELECT) statement.
pub fn validate_select_only(sql: &str) -> Result<(), String> {
    let statements = Parser::parse_sql(&DuckDbDialect {}, sql)
        .map_err(|err| format!("unparseable SQL: {err}"))?;
    if statements.len() != 1 {
        return Err(format!(
            "only one statement is allowed, found {}",
            statements.len()
        ));
    }
    match &statements[0] {
        Statement::Query(_) => {}
        _ => return Err("only SELECT queries are allowed".to_string()),
    }
    // sqlparser models `WITH ... INSERT/UPDATE/DELETE` (CTE DML) as a
    // Query too, so the tail keyword of a WITH statement is checked
    // lexically: it must be SELECT or VALUES.
    if let Some(tail) = with_tail_keyword(sql)
        && tail != "SELECT"
        && tail != "VALUES"
    {
        return Err(format!("WITH ... {tail} is not a SELECT query"));
    }
    Ok(())
}

/// First top-level keyword after the `WITH` CTE definitions, if the
/// statement starts with WITH. CTE names (optionally with a column list)
/// are skipped; string literals, quoted identifiers and comments inside
/// the definitions are handled.
fn with_tail_keyword(sql: &str) -> Option<String> {
    let chars: Vec<char> = sql.chars().collect();
    let mut i = skip_space_comments(&chars, 0);
    let mut end = i;
    while end < chars.len() && is_ident_char(chars[end]) {
        end += 1;
    }
    if !sql[i..end].eq_ignore_ascii_case("WITH") {
        return None;
    }
    i = end;
    loop {
        i = skip_space_comments(&chars, i);
        if i < chars.len() && chars[i] == ',' {
            i += 1;
            i = skip_space_comments(&chars, i);
        }
        let name_start = i;
        while i < chars.len() && (is_ident_char(chars[i]) || is_quote(chars[i])) {
            if is_quote(chars[i]) {
                i = skip_quoted(&chars, i)?;
            } else {
                i += 1;
            }
        }
        if name_start == i {
            return None;
        }
        i = skip_space_comments(&chars, i);
        if i < chars.len() && chars[i] == '(' {
            i = skip_parens(&chars, i)?;
            i = skip_space_comments(&chars, i);
        }
        let word_start = i;
        while i < chars.len() && is_ident_char(chars[i]) {
            i += 1;
        }
        if !sql[word_start..i].eq_ignore_ascii_case("AS") {
            return Some(sql[name_start..i].to_ascii_uppercase());
        }
        i = skip_space_comments(&chars, i);
        if i >= chars.len() || chars[i] != '(' {
            return None;
        }
        let after = skip_parens(&chars, i)?;
        i = skip_space_comments(&chars, after);
        if i < chars.len() && chars[i] == ',' {
            continue;
        }
        let tail_start = i;
        while i < chars.len() && is_ident_char(chars[i]) {
            i += 1;
        }
        return Some(sql[tail_start..i].to_ascii_uppercase());
    }
}

fn is_ident_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn is_quote(c: char) -> bool {
    c == '"' || c == '`'
}

fn starts_with2(chars: &[char], i: usize, a: char, b: char) -> bool {
    i + 1 < chars.len() && chars[i] == a && chars[i + 1] == b
}

fn skip_space_comments(chars: &[char], mut i: usize) -> usize {
    loop {
        while i < chars.len() && chars[i].is_whitespace() {
            i += 1;
        }
        if starts_with2(chars, i, '-', '-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
            continue;
        }
        if starts_with2(chars, i, '/', '*') {
            i += 2;
            while i < chars.len() && !starts_with2(chars, i, '*', '/') {
                i += 1;
            }
            i = (i + 2).min(chars.len());
            continue;
        }
        return i;
    }
}

/// Skip a single-quoted string literal starting at `i` (with `''`
/// escaping); returns the index after the closing quote.
fn skip_quoted(chars: &[char], i: usize) -> Option<usize> {
    let mut i = i + 1;
    while i < chars.len() {
        if chars[i] == '\'' {
            if i + 1 < chars.len() && chars[i + 1] == '\'' {
                i += 2;
            } else {
                return Some(i + 1);
            }
        } else {
            i += 1;
        }
    }
    None
}

/// Skip a balanced paren group starting at `i` (the opening paren),
/// honoring string literals, quoted identifiers and comments.
fn skip_parens(chars: &[char], i: usize) -> Option<usize> {
    let mut i = i + 1;
    let mut depth = 1;
    while i < chars.len() {
        match chars[i] {
            '(' => depth += 1,
            ')' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i + 1);
                }
            }
            '\'' => {
                i = skip_quoted(chars, i)?;
                continue;
            }
            '"' | '`' => {
                let mut j = i + 1;
                while j < chars.len() && chars[j] != chars[i] {
                    j += 1;
                }
                i = j + 1;
                continue;
            }
            _ => {}
        }
        if starts_with2(chars, i, '-', '-') {
            while i < chars.len() && chars[i] != '\n' {
                i += 1;
            }
        } else if starts_with2(chars, i, '/', '*') {
            i += 2;
            while i < chars.len() && !starts_with2(chars, i, '*', '/') {
                i += 1;
            }
            i = (i + 2).min(chars.len());
        } else {
            i += 1;
        }
    }
    None
}

/// Load the fixed category taxonomy names used to validate
/// `render_dashboard` category slices.
async fn load_category_names(db_path: PathBuf) -> anyhow::Result<HashSet<String>> {
    tokio::task::spawn_blocking(move || {
        let conn = crate::db::api_connection(&db_path)?;
        let mut stmt = conn.prepare("SELECT name FROM categories")?;
        let mut rows = stmt.query([])?;
        let mut names = HashSet::new();
        while let Some(row) = rows.next()? {
            let name: String = row.get(0)?;
            names.insert(name);
        }
        Ok(names)
    })
    .await?
}

/// Execute a validated SELECT on a short-lived read-only connection and
/// render the rows (capped at `MAX_ROWS`) as a JSON string for the model.
async fn run_readonly_sql(db_path: PathBuf, sql: String) -> anyhow::Result<String> {
    tokio::task::spawn_blocking(move || {
        let conn = crate::db::api_connection(&db_path)?;
        let mut stmt = conn.prepare(&sql)?;
        stmt.execute([])?;
        let names: Vec<String> = stmt
            .schema()
            .fields()
            .iter()
            .map(|field| field.name().clone())
            .collect();
        let mut rows = stmt.raw_query();
        let mut out: Vec<serde_json::Value> = Vec::with_capacity(MAX_ROWS);
        let mut truncated = false;
        while let Some(row) = rows.next()? {
            if out.len() >= MAX_ROWS {
                truncated = true;
                break;
            }
            let mut obj = serde_json::Map::new();
            for (i, name) in names.iter().enumerate() {
                let value: Value = row.get(i)?;
                obj.insert(name.clone(), value_to_json(value));
            }
            out.push(serde_json::Value::Object(obj));
        }
        Ok(serde_json::json!({
            "columns": names,
            "rows": out,
            "truncated": truncated,
        })
        .to_string())
    })
    .await?
}

fn value_to_json(value: Value) -> serde_json::Value {
    use duckdb::types::Value::*;
    match value {
        Null => serde_json::Value::Null,
        Boolean(b) => b.into(),
        TinyInt(i) => i.into(),
        SmallInt(i) => i.into(),
        Int(i) => i.into(),
        BigInt(i) => i.into(),
        HugeInt(i) => i.to_string().into(),
        UHugeInt(i) => i.to_string().into(),
        UTinyInt(i) => i.into(),
        USmallInt(i) => i.into(),
        UInt(i) => i.into(),
        UBigInt(i) => i.into(),
        Float(f) => serde_json::json!(f),
        Double(f) => serde_json::json!(f),
        Decimal(d) => d.to_string().into(),
        Text(s) => s.into(),
        Enum(s) => s.into(),
        // Arrow/DuckDB Date32 counts days from the Unix epoch (1970-01-01),
        // while from_num_days_from_ce counts from 0001-01-01.
        Date32(days) => NaiveDate::from_num_days_from_ce_opt(days.saturating_add(UNIX_EPOCH_DAYS))
            .map(|d| d.format("%Y-%m-%d").to_string())
            .unwrap_or_default()
            .into(),
        Timestamp(unit, value) => timestamp_to_string(unit, value).into(),
        Time64(unit, value) => time_to_string(unit, value).into(),
        Interval {
            months,
            days,
            nanos,
        } => format!("{months}mo {days}d {:.3}s", nanos as f64 / 1_000_000_000.0).into(),
        List(items) => serde_json::Value::Array(items.iter().cloned().map(value_to_json).collect()),
        Struct(fields) => {
            let mut obj = serde_json::Map::new();
            for (name, field) in fields.iter() {
                obj.insert(name.clone(), value_to_json(field.clone()));
            }
            serde_json::Value::Object(obj)
        }
        Array(items) => {
            serde_json::Value::Array(items.iter().cloned().map(value_to_json).collect())
        }
        Map(pairs) => {
            let mut obj = serde_json::Map::new();
            for (key, val) in pairs.iter() {
                obj.insert(
                    value_to_json(key.clone()).to_string(),
                    value_to_json(val.clone()),
                );
            }
            serde_json::Value::Object(obj)
        }
        Blob(_) => "<blob>".into(),
        Geometry(_) => "<geometry>".into(),
        Union(_) => "<union>".into(),
        other => format!("{other:?}").into(),
    }
}

fn timestamp_to_string(unit: TimeUnit, value: i64) -> String {
    let (secs, nanos) = match unit {
        TimeUnit::Second => (value, 0i64),
        TimeUnit::Millisecond => (value.div_euclid(1_000), value.rem_euclid(1_000) * 1_000_000),
        TimeUnit::Microsecond => (
            value.div_euclid(1_000_000),
            value.rem_euclid(1_000_000) * 1_000,
        ),
        TimeUnit::Nanosecond => (
            value.div_euclid(1_000_000_000),
            value.rem_euclid(1_000_000_000),
        ),
    };
    let days = secs.div_euclid(86_400);
    let date = i32::try_from(days)
        .ok()
        .and_then(|d| NaiveDate::from_num_days_from_ce_opt(d.saturating_add(UNIX_EPOCH_DAYS)));
    let time = chrono::NaiveTime::from_num_seconds_from_midnight_opt(
        secs.rem_euclid(86_400) as u32,
        nanos.max(0) as u32,
    );
    match (date, time) {
        (Some(date), Some(time)) => chrono::NaiveDateTime::new(date, time)
            .format("%Y-%m-%d %H:%M:%S")
            .to_string(),
        _ => value.to_string(),
    }
}

fn time_to_string(unit: TimeUnit, value: i64) -> String {
    use chrono::NaiveTime;
    let (secs, nanos) = match unit {
        TimeUnit::Second => (value, 0),
        TimeUnit::Millisecond => (value.div_euclid(1000), (value.rem_euclid(1000)) * 1_000_000),
        TimeUnit::Microsecond => (
            value.div_euclid(1_000_000),
            (value.rem_euclid(1_000_000)) * 1_000,
        ),
        TimeUnit::Nanosecond => (
            value.div_euclid(1_000_000_000),
            value.rem_euclid(1_000_000_000),
        ),
    };
    NaiveTime::from_num_seconds_from_midnight_opt(secs.max(0) as u32, nanos.max(0) as u32)
        .map(|t| t.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| value.to_string())
}

fn tool_spec_local() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": TOOL_NAME,
            "description": TOOL_DESCRIPTION,
            "parameters": {
                "type": "object",
                "properties": {
                    "sql": {
                        "type": "string",
                        "description": "A single SELECT statement (no other statement kinds, no multiple statements)."
                    }
                },
                "required": ["sql"]
            }
        }
    })
}

fn tool_spec_gemini() -> serde_json::Value {
    serde_json::json!({
        "name": TOOL_NAME,
        "description": TOOL_DESCRIPTION,
        "parameters": {
            "type": "object",
            "properties": {
                "sql": {
                    "type": "string",
                    "description": "A single SELECT statement (no other statement kinds, no multiple statements)."
                }
            },
            "required": ["sql"]
        }
    })
}

/// Shared JSON Schema for the `render_dashboard` arguments.
fn render_tool_parameters() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "properties": {
            "year": {
                "type": "integer",
                "description": "Year the data covers; it drives the dashboard year picker."
            },
            "label": {
                "type": "string",
                "description": "Short label of the period the data covers, e.g. '2024' or '2024-03'."
            },
            "kpi": {
                "type": "object",
                "description": "KPI card values in CHF (positive magnitudes). Provide all three or omit the object.",
                "properties": {
                    "income": { "type": "number" },
                    "spend": { "type": "number" },
                    "moved": { "type": "number" }
                },
                "required": ["income", "spend", "moved"]
            },
            "monthly": {
                "type": "array",
                "description": "Spend per month (at most 12 entries) for the monthly bar chart.",
                "items": {
                    "type": "object",
                    "properties": {
                        "month": { "type": "integer", "minimum": 1, "maximum": 12 },
                        "value": { "type": "number" }
                    },
                    "required": ["month", "value"]
                }
            },
            "yearly": {
                "type": "array",
                "description": "Total spend per year for the yearly bar chart.",
                "items": {
                    "type": "object",
                    "properties": {
                        "year": { "type": "integer" },
                        "value": { "type": "number" }
                    },
                    "required": ["year", "value"]
                }
            },
            "cumulative": {
                "type": "array",
                "description": "Cumulative spend per month (at most 12 entries) for the cumulative line.",
                "items": {
                    "type": "object",
                    "properties": {
                        "month": { "type": "integer", "minimum": 1, "maximum": 12 },
                        "value": { "type": "number" }
                    },
                    "required": ["month", "value"]
                }
            },
            "categories": {
                "type": "array",
                "description": "Spend per category for the donut; names must match the categories table exactly.",
                "items": {
                    "type": "object",
                    "properties": {
                        "name": { "type": "string" },
                        "value": { "type": "number" }
                    },
                    "required": ["name", "value"]
                }
            },
            "currency": {
                "type": "string",
                "enum": ["CHF", "USD", "EUR"],
                "description": "Display currency for the navbar; switches the currency picker. Values stay in CHF."
            },
            "month": {
                "type": "integer",
                "minimum": 1,
                "maximum": 12,
                "description": "Month (1-12) the data is scoped to; drives the navbar month picker and sankey period. Omit for year scope."
            },
            "view": {
                "type": "string",
                "enum": ["month", "year"],
                "description": "Navbar view toggle; 'month' shows monthly/daily charts, 'year' shows yearly/cumulative."
            },
            "sankey": {
                "type": "array",
                "description": "Category names to show as outflows in the money-flow sankey. When omitted the sankey mirrors categories; use names from the categories table.",
                "items": { "type": "string" }
            }
        },
        "required": ["year", "label"]
    })
}

fn render_tool_spec_local() -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": RENDER_TOOL_NAME,
            "description": RENDER_TOOL_DESCRIPTION,
            "parameters": render_tool_parameters()
        }
    })
}

fn render_tool_spec_gemini() -> serde_json::Value {
    serde_json::json!({
        "name": RENDER_TOOL_NAME,
        "description": RENDER_TOOL_DESCRIPTION,
        "parameters": render_tool_parameters()
    })
}

/// OpenAI-compatible wire messages: system + history + user, then per round
/// an assistant message with tool_calls and one `tool` message per result.
fn local_messages(
    system: &str,
    history: &[ChatHistoryEntry],
    user: &str,
    rounds: &[Round],
) -> Vec<serde_json::Value> {
    let mut out = vec![serde_json::json!({ "role": "system", "content": system })];
    for entry in history {
        let role = if entry.role == "assistant" {
            "assistant"
        } else {
            "user"
        };
        out.push(serde_json::json!({ "role": role, "content": entry.content }));
    }
    out.push(serde_json::json!({ "role": "user", "content": user }));
    for round in rounds {
        out.push(serde_json::json!({
            "role": "assistant",
            "content": round.assistant_text,
            "tool_calls": round
                .calls
                .iter()
                .map(|call| {
                    serde_json::json!({
                        "id": call.id,
                        "type": "function",
                        "function": {
                            "name": call.name,
                            "arguments": call.arguments.to_string(),
                        },
                    })
                })
                .collect::<Vec<_>>(),
        }));
        for call in &round.calls {
            out.push(serde_json::json!({
                "role": "tool",
                "tool_call_id": call.id,
                "content": call.result,
            }));
        }
    }
    out
}

/// Gemini wire contents: history + user first, then per round a `model`
/// message with text and function_call parts followed by one `user` message
/// with function_response parts.
fn gemini_contents(
    history: &[ChatHistoryEntry],
    user: &str,
    rounds: &[Round],
) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for entry in history {
        if entry.role == "assistant" {
            out.push(serde_json::json!({ "role": "model", "parts": [{ "text": entry.content }] }));
        } else {
            out.push(serde_json::json!({ "role": "user", "parts": [{ "text": entry.content }] }));
        }
    }
    out.push(serde_json::json!({ "role": "user", "parts": [{ "text": user }] }));
    for round in rounds {
        let mut parts = Vec::new();
        if !round.assistant_text.is_empty() {
            parts.push(serde_json::json!({ "text": round.assistant_text }));
        }
        for call in &round.calls {
            parts.push(serde_json::json!({
                "function_call": { "name": call.name, "args": call.arguments },
            }));
        }
        out.push(serde_json::json!({ "role": "model", "parts": parts }));
        let responses: Vec<serde_json::Value> = round
            .calls
            .iter()
            .map(|call| {
                serde_json::json!({
                    "function_response": {
                        "name": call.name,
                        "response": { "content": call.result },
                    },
                })
            })
            .collect();
        out.push(serde_json::json!({ "role": "user", "parts": responses }));
    }
    out
}

const BASE_PROMPT: &str = "You are the spending assistant for a personal yearly-spend \
dashboard backed by a local DuckDB database.

Schema:
- accounts(id INTEGER, source VARCHAR, name VARCHAR, currency VARCHAR, is_internal BOOLEAN)
- categories(id INTEGER, name VARCHAR, color VARCHAR) -- fixed 18-entry taxonomy
- transactions(id INTEGER, account_id INTEGER, source VARCHAR, source_key VARCHAR, \
dt DATE, ts TIMESTAMP, description VARCHAR, subject VARCHAR, category_id INTEGER, \
amount_orig DOUBLE, currency_orig VARCHAR, amount_chf DOUBLE, kind VARCHAR, \
transfer_group_id INTEGER, file_sha VARCHAR, ingested_at TIMESTAMP)
- transfer_groups(id INTEGER, from_account_id INTEGER, to_account_id INTEGER, amount_chf DOUBLE, dt DATE)
- fx_rates(month DATE, from_ccy VARCHAR, to_ccy VARCHAR, rate DOUBLE)
- ingested_files(id INTEGER, source VARCHAR, file_sha256 VARCHAR, path VARCHAR, ingested_at TIMESTAMP, rows INTEGER)
- llm_calls(id INTEGER, context VARCHAR, phase VARCHAR, attempt INTEGER, ok BOOLEAN, created_at TIMESTAMP)

Conventions:
- transactions.amount_chf is normalized to CHF. Spend rows (kind = 'spend') are stored \
negative, except refund rows which are stored positive. The dashboard computes spend \
totals as sum(-amount_chf) FILTER (WHERE kind = 'spend'); use that exact expression so \
your totals match the dashboard.
- kind is one of: spend, income, transfer_out, transfer_in, internal. Spend \
aggregates must filter kind = 'spend'; transfer and internal rows are never spend.
- dt is a DATE. Filter with dt BETWEEN DATE '2025-03-01' AND DATE '2025-03-31' or with \
year(dt), month(dt) and strftime().
- Category names live in categories; join on transactions.category_id = categories.id.

Tools:
- run_sql: read-only SQL (a single SELECT statement). Use it to ground every \
data-related claim in actual query results; never invent numbers. Make at most a \
couple of calls, then answer.
- render_dashboard: pushes computed numbers onto the live dashboard charts: KPI \
cards (kpi), the monthly spend bar chart (monthly), the yearly spend bar chart \
(yearly), the cumulative spend line (cumulative), the category donut \
(categories) and the money-flow sankey (sankey), and drives the navbar pickers \
(year, month, view, currency). When the user's question is about data one of \
these charts can show, compute the values with run_sql, call render_dashboard \
so the user sees the answer on screen, and briefly say so in your reply. Do not \
call it for questions the dashboard cannot show (lists of individual \
transactions, single facts, explanations).

render_dashboard rules:
- year is the year the data covers; label is a short period label like '2024' \
or '2024-03'.
- Values are positive CHF magnitudes. Spend values use the dashboard formula \
sum(-amount_chf) FILTER (WHERE kind = 'spend') with whatever filters the \
question implies (year, month, category, account, source).
- kpi must contain all of income, spend and moved, or be omitted entirely.
- Provide 'monthly' or 'yearly'/'cumulative', never both: the dashboard shows \
one of those chart pairs at a time. You may combine charts with kpi and \
categories in a single call.
- Category names must match the categories table exactly; query it if unsure.
- Navbar: set `year` to the year the user asked about; set `month` (1-12) when \
the question is month-scoped or implies a specific month, and set `view` to \
'month' for month-scoped and 'year' for year-scoped payloads; the frontend \
switches the pickers. For 'top N' queries, report in the period the user asked \
about (default to the selected year when none is mentioned) and set the pickers \
accordingly. Also set `currency` to CHF/EUR/USD when the user asks in a \
specific currency (case-insensitive) or when the data implies a different \
display currency; the payload stays in CHF and the client converts.
- Sankey: the money-flow card always reflects the current year/month. Its \
category outflows normally mirror `categories` when you provide them; for a \
'top N' question, provide exactly N categories and the sankey will show exactly \
N outflows. Use `sankey` to override: a list of category names to show as \
outflows (must be from the taxonomy). If the question scopes to a month, also \
set `month` and `view='month'` so the sankey fetches the correct period before \
filtering.

Style: concise plain prose, exact CHF amounts (two decimals), short lists or tables \
where they help, and answer the user's question directly.";

fn build_system_prompt(selections: Vec<Selection>) -> String {
    if selections.is_empty() {
        return BASE_PROMPT.to_string();
    }
    let mut prompt = String::from(BASE_PROMPT);
    prompt.push_str("\n\nPinned chart selections from the dashboard (values in CHF):\n");
    for sel in &selections {
        let label = serde_json::to_string(&sel.label).unwrap_or_else(|_| "\"\"".to_string());
        prompt.push_str(&format!(
            "- chart={} series={} label={} value={:.2} CHF",
            sel.chart, sel.series, label, sel.value,
        ));
        if let Some(year) = sel.year {
            prompt.push_str(&format!(" year={year}"));
        }
        if let Some(month) = sel.month {
            prompt.push_str(&format!(" month={month}"));
        }
        if let Some(category) = &sel.category {
            prompt.push_str(&format!(" category={category}"));
        }
        if let Some(note) = &sel.note {
            let note = serde_json::to_string(note).unwrap_or_else(|_| "\"\"".to_string());
            prompt.push_str(&format!(" note={note}"));
        }
        prompt.push('\n');
    }
    prompt.push_str(
        "Interpret the user's question in the context of these pinned selections \
(unless they say otherwise): their year/month scope, the highlighted element and value, \
and any attached note.\n",
    );
    prompt
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_only_accepts_selects() {
        for sql in [
            "SELECT 1",
            "select count(*) from transactions",
            "SELECT * FROM transactions WHERE kind = 'spend' ORDER BY dt DESC LIMIT 10",
            "WITH top AS (SELECT category_id, SUM(-amount_chf) FILTER (WHERE kind = 'spend') \
             AS total FROM transactions GROUP BY 1 ORDER BY total DESC LIMIT 5) \
             SELECT * FROM top",
            "SELECT * FROM transactions LIMIT 5;",
            "-- a comment\nSELECT 1",
            "SELECT strftime(dt, '%Y-%m') AS m, SUM(-amount_chf) FILTER (WHERE kind = 'spend') \
             FROM transactions GROUP BY 1",
            "WITH a AS (SELECT 1), b AS (SELECT 2) SELECT * FROM a, b",
            "-- comment\nWITH x AS (SELECT 1) SELECT * FROM x",
            "WITH x AS (SELECT 'WITH y AS (SELECT 1) DELETE FROM t') SELECT 1",
            "VALUES (1), (2)",
            "WITH x AS (SELECT 1) VALUES (1)",
        ] {
            assert!(validate_select_only(sql).is_ok(), "rejected: {sql}");
        }
    }

    #[test]
    fn select_only_rejects_non_selects() {
        for sql in [
            "INSERT INTO transactions (account_id, source, source_key, dt, description, \
             amount_orig, currency_orig, amount_chf, kind) \
             VALUES (1, 'x', 'y', '2025-01-01', 'z', 1, 'CHF', 1, 'spend')",
            "UPDATE transactions SET amount_chf = 0",
            "DELETE FROM transactions",
            "CREATE TABLE evil (id INTEGER)",
            "DROP TABLE transactions",
            "ALTER TABLE transactions RENAME TO renamed",
            "PRAGMA database_list",
            "COPY (SELECT 1) TO '/tmp/x.csv'",
            "ATTACH '/tmp/other.db' AS other",
            "EXPORT DATABASE '/tmp/out'",
            "INSTALL httpfs",
            "SELECT 1; DELETE FROM transactions",
            "WITH x AS (SELECT 1) INSERT INTO transactions (source_key) VALUES ('a')",
            "WITH x AS (SELECT 1) UPDATE transactions SET amount_chf = 0",
            "WITH x AS (SELECT 1) DELETE FROM transactions",
            "WITH a AS (SELECT 1), b AS (SELECT 2) UPDATE transactions SET amount_chf = 0",
            "-- comment\nWITH x AS (SELECT 1) DELETE FROM transactions",
            "",
            "   ",
        ] {
            assert!(validate_select_only(sql).is_err(), "accepted: {sql}");
        }
    }

    #[test]
    fn value_to_json_maps_core_types() {
        assert_eq!(value_to_json(Value::Null), serde_json::Value::Null);
        assert_eq!(value_to_json(Value::Boolean(true)), serde_json::json!(true));
        assert_eq!(value_to_json(Value::BigInt(42)), serde_json::json!(42));
        assert_eq!(value_to_json(Value::Double(1.5)), serde_json::json!(1.5));
        assert_eq!(
            value_to_json(Value::Text("x".into())),
            serde_json::json!("x")
        );
        // 2025-03-01 is 20148 days after the Unix epoch.
        assert_eq!(
            value_to_json(Value::Date32(20148)),
            serde_json::json!("2025-03-01")
        );
        assert_eq!(
            value_to_json(Value::Date32(0)),
            serde_json::json!("1970-01-01")
        );
        let ts = chrono::DateTime::parse_from_rfc3339("2025-03-01T12:34:56Z")
            .unwrap()
            .timestamp_micros();
        assert_eq!(
            value_to_json(Value::Timestamp(TimeUnit::Microsecond, ts)),
            serde_json::json!("2025-03-01 12:34:56")
        );
    }

    #[test]
    fn selection_validation() {
        let ok = Selection {
            chart: ChartKind::Monthly,
            series: "spend".into(),
            label: "Mar 2025".into(),
            value: 12.5,
            year: Some(2025),
            month: Some(3),
            category: None,
            note: None,
        };
        assert!(ok.validate().is_ok());

        let mut bad = ok.clone();
        bad.label = "  ".into();
        assert!(bad.validate().is_err());
        bad = ok.clone();
        bad.value = f64::NAN;
        assert!(bad.validate().is_err());
        bad = ok.clone();
        bad.month = Some(13);
        assert!(bad.validate().is_err());
        bad = ok.clone();
        bad.year = Some(99);
        assert!(bad.validate().is_err());
        bad = ok.clone();
        bad.note = Some(" ".into());
        assert!(bad.validate().is_err());
        bad = ok.clone();
        bad.note = Some("x".repeat(501));
        assert!(bad.validate().is_err());
    }

    #[test]
    fn system_prompt_includes_pinned_context() {
        let prompt = build_system_prompt(vec![
            Selection {
                chart: ChartKind::Categories,
                series: "category".into(),
                label: "food".into(),
                value: 99.999,
                year: Some(2025),
                month: Some(3),
                category: Some("food".into()),
                note: Some("flag this spike".into()),
            },
            Selection {
                chart: ChartKind::Summary,
                series: "spend".into(),
                label: "spend 2025".into(),
                value: 26303.21,
                year: Some(2025),
                month: None,
                category: None,
                note: None,
            },
        ]);
        assert!(prompt.contains("Pinned chart selections"));
        assert!(
            prompt.contains(r#"chart=summary series=spend label="spend 2025" value=26303.21 CHF"#,)
        );
        assert!(prompt.contains(r#"label="food" value=100.00 CHF"#));
        assert!(prompt.contains("year=2025"));
        assert!(prompt.contains("month=3"));
        assert!(prompt.contains("category=food"));
        assert!(prompt.contains(r#"note="flag this spike""#));
        assert_eq!(build_system_prompt(Vec::new()), BASE_PROMPT);
    }

    fn valid_update() -> DashboardUpdate {
        DashboardUpdate {
            year: 2024,
            label: "2024".into(),
            kpi: Some(KpiValues {
                income: 10_000.0,
                spend: 5_000.0,
                moved: 1_000.0,
            }),
            monthly: Some(vec![MonthPoint {
                month: 1,
                value: 100.0,
            }]),
            yearly: Some(vec![YearPoint {
                year: 2024,
                value: 5_000.0,
            }]),
            cumulative: Some(vec![MonthPoint {
                month: 1,
                value: 100.0,
            }]),
            categories: Some(vec![CategoryPoint {
                name: "food".into(),
                value: 250.0,
            }]),
            currency: None,
            month: None,
            view: None,
            sankey: None,
        }
    }

    fn taxonomy_names() -> HashSet<String> {
        ["food", "travel", "housing"]
            .into_iter()
            .map(String::from)
            .collect()
    }

    #[test]
    fn render_update_validation() {
        let names = taxonomy_names();
        assert!(valid_update().validate(&names).is_ok());

        // At least one section is required; empty sections count as absent.
        let mut update = valid_update();
        update.kpi = None;
        update.monthly = Some(vec![]);
        update.yearly = None;
        update.cumulative = None;
        update.categories = None;
        assert!(update.validate(&names).is_err(), "no sections should fail");
        update.kpi = Some(KpiValues {
            income: 0.0,
            spend: 0.0,
            moved: 0.0,
        });
        assert!(update.validate(&names).is_ok());

        // Year and label bounds.
        update = valid_update();
        update.year = 999;
        assert!(update.validate(&names).is_err());
        update = valid_update();
        update.label = "  ".into();
        assert!(update.validate(&names).is_err());
        update = valid_update();
        update.label = "x".repeat(41);
        assert!(update.validate(&names).is_err());

        // Values must be finite and non-negative.
        for field in ["income", "spend", "moved"] {
            update = valid_update();
            match field {
                "income" => update.kpi.as_mut().unwrap().income = -1.0,
                "spend" => update.kpi.as_mut().unwrap().spend = f64::NAN,
                "moved" => update.kpi.as_mut().unwrap().moved = f64::INFINITY,
                _ => unreachable!(),
            }
            assert!(update.validate(&names).is_err(), "{field} should fail");
        }
        update = valid_update();
        update.monthly.as_mut().unwrap()[0].value = -0.01;
        assert!(update.validate(&names).is_err());
        update = valid_update();
        update.yearly.as_mut().unwrap()[0].value = f64::NAN;
        assert!(update.validate(&names).is_err());

        // Month index bounds, per-section caps and duplicate keys.
        update = valid_update();
        update.monthly.as_mut().unwrap()[0].month = 0;
        assert!(update.validate(&names).is_err());
        update = valid_update();
        update.monthly.as_mut().unwrap()[0].month = 13;
        assert!(update.validate(&names).is_err());
        update = valid_update();
        update.monthly = Some(
            (1..=13)
                .map(|month| MonthPoint { month, value: 1.0 })
                .collect(),
        );
        assert!(update.validate(&names).is_err());
        update = valid_update();
        update.monthly = Some(vec![
            MonthPoint {
                month: 1,
                value: 1.0,
            },
            MonthPoint {
                month: 1,
                value: 2.0,
            },
        ]);
        assert!(update.validate(&names).is_err(), "duplicate month");
        update = valid_update();
        update.cumulative.as_mut().unwrap()[0].month = 13;
        assert!(update.validate(&names).is_err());
        update = valid_update();
        update.yearly = Some(vec![
            YearPoint {
                year: 2024,
                value: 1.0,
            },
            YearPoint {
                year: 2024,
                value: 2.0,
            },
        ]);
        assert!(update.validate(&names).is_err(), "duplicate year");
        update = valid_update();
        update.yearly.as_mut().unwrap()[0].year = 999;
        assert!(update.validate(&names).is_err());

        // Category names must come from the taxonomy.
        update = valid_update();
        update.categories.as_mut().unwrap()[0].name = "unknown".into();
        assert!(update.validate(&names).is_err());
        update = valid_update();
        update.categories = Some(vec![
            CategoryPoint {
                name: "food".into(),
                value: 1.0,
            },
            CategoryPoint {
                name: "food".into(),
                value: 2.0,
            },
        ]);
        assert!(update.validate(&names).is_err(), "duplicate category");
        update = valid_update();
        update.categories = Some(
            (0..19)
                .map(|index| CategoryPoint {
                    name: if index < 3 {
                        ["food", "travel", "housing"][index].into()
                    } else {
                        format!("filler-{index}")
                    },
                    value: 1.0,
                })
                .collect(),
        );
        assert!(update.validate(&names).is_err(), "18-entry cap");
    }

    #[test]
    fn render_update_serializes_only_present_sections() {
        let update = DashboardUpdate {
            year: 2024,
            label: "2024".into(),
            kpi: None,
            monthly: Some(vec![MonthPoint {
                month: 1,
                value: 1.5,
            }]),
            yearly: None,
            cumulative: None,
            categories: None,
            currency: None,
            month: None,
            view: None,
            sankey: None,
        };
        assert_eq!(
            serde_json::to_value(&update).unwrap(),
            serde_json::json!({
                "year": 2024,
                "label": "2024",
                "monthly": [{ "month": 1, "value": 1.5 }]
            })
        );
        // Round trip: missing sections deserialize to None.
        let parsed: DashboardUpdate = serde_json::from_value(serde_json::json!({
            "year": 2024,
            "label": "2024",
            "monthly": [{ "month": 1, "value": 1.5 }]
        }))
        .unwrap();
        assert_eq!(parsed, update);
    }
}
