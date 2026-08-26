//! Chat backend for the dashboard inspector: the configured LLM (local
//! llama-server by default, Gemini when `.env` selects it) streams tokens
//! over HTTP SSE and may call a single read-only `run_sql` tool.
//!
//! The SQL tool is SELECT-only: statements are parsed and rejected unless
//! they are a single SELECT/WITH-SELECT query, and execution happens on a
//! short-lived read-only DuckDB connection, so the chat can never write to
//! the database. Nothing is persisted; every request starts from scratch.

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
pub enum ChatEvent {
    /// A chunk of the assistant's visible reply.
    Token(String),
    /// The assistant is invoking the SQL tool with this statement.
    ToolCall { sql: String },
    /// Terminal error; no further events follow.
    Error(String),
}

/// A tool call requested by the model, with the parsed `sql` argument.
struct PendingCall {
    id: String,
    name: String,
    sql: String,
}

/// One completed round trip: assistant output plus the executed tool calls.
struct Round {
    assistant_text: String,
    calls: Vec<ExecutedCall>,
}

struct ExecutedCall {
    id: String,
    sql: String,
    result: String,
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
    /// Terminates with `ChatEvent::Error` on failure or after `MAX_ROUNDS`
    /// tool round trips; on success it simply ends when the model stops
    /// calling tools.
    pub async fn run(
        &self,
        message: &str,
        selections: Vec<Selection>,
        tx: UnboundedSender<ChatEvent>,
    ) {
        let system = build_system_prompt(selections);
        let mut rounds: Vec<Round> = Vec::new();
        for _ in 0..MAX_ROUNDS {
            let outcome = match self.provider {
                LlmProvider::Local => self.stream_local(&system, message, &rounds, &tx).await,
                LlmProvider::Gemini => self.stream_gemini(&system, message, &rounds, &tx).await,
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
                    sql: call.sql,
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
        let _ = tx.send(ChatEvent::ToolCall {
            sql: call.sql.clone(),
        });
        if call.name != TOOL_NAME || call.sql.trim().is_empty() {
            return format!("error: unknown tool '{}' or missing sql", call.name);
        }
        if let Err(reason) = validate_select_only(&call.sql) {
            return format!("rejected: {reason}");
        }
        match run_readonly_sql(self.db_path.clone(), call.sql.clone()).await {
            Ok(result) => result,
            Err(err) => format!("error: {err}"),
        }
    }

    async fn stream_local(
        &self,
        system: &str,
        user: &str,
        rounds: &[Round],
        tx: &UnboundedSender<ChatEvent>,
    ) -> anyhow::Result<StreamOutcome> {
        let body = serde_json::json!({
            "model": self.local_model,
            "messages": local_messages(system, user, rounds),
            "temperature": 0.2,
            "stream": true,
            "tools": [tool_spec_local()],
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
                sql: args
                    .get("sql")
                    .and_then(|v| v.as_str())
                    .unwrap_or_default()
                    .to_string(),
            });
        }
        Ok(outcome)
    }

    async fn stream_gemini(
        &self,
        system: &str,
        user: &str,
        rounds: &[Round],
        tx: &UnboundedSender<ChatEvent>,
    ) -> anyhow::Result<StreamOutcome> {
        anyhow::ensure!(
            !self.gemini_api_key.is_empty(),
            "GEMINI_API_KEY is not set for provider 'gemini'"
        );
        let body = serde_json::json!({
            "contents": gemini_contents(user, rounds),
            "system_instruction": { "parts": [{ "text": system }] },
            "tools": [{ "function_declarations": [tool_spec_gemini()] }],
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
                                sql: args
                                    .get("sql")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or_default()
                                    .to_string(),
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

/// OpenAI-compatible wire messages: system + user, then per round an
/// assistant message with tool_calls and one `tool` message per result.
fn local_messages(system: &str, user: &str, rounds: &[Round]) -> Vec<serde_json::Value> {
    let mut out = vec![
        serde_json::json!({ "role": "system", "content": system }),
        serde_json::json!({ "role": "user", "content": user }),
    ];
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
                            "name": TOOL_NAME,
                            "arguments": serde_json::json!({ "sql": call.sql }).to_string(),
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

/// Gemini wire contents: user first, then per round a `model` message with
/// text and function_call parts followed by one `user` message with
/// function_response parts.
fn gemini_contents(user: &str, rounds: &[Round]) -> Vec<serde_json::Value> {
    let mut out = vec![serde_json::json!({ "role": "user", "parts": [{ "text": user }] })];
    for round in rounds {
        let mut parts = Vec::new();
        if !round.assistant_text.is_empty() {
            parts.push(serde_json::json!({ "text": round.assistant_text }));
        }
        for call in &round.calls {
            parts.push(serde_json::json!({
                "function_call": { "name": TOOL_NAME, "args": { "sql": call.sql } },
            }));
        }
        out.push(serde_json::json!({ "role": "model", "parts": parts }));
        let responses: Vec<serde_json::Value> = round
            .calls
            .iter()
            .map(|call| {
                serde_json::json!({
                    "function_response": {
                        "name": TOOL_NAME,
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

You have the run_sql tool for read-only SQL (a single SELECT statement). Use it to \
ground every data-related claim in actual query results; never invent numbers. Make \
at most a couple of calls, then answer.

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
}
