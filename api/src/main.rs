use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Json, Response};
use axum::routing::{get, patch, post};
use serde::Deserialize;
use tokio_stream::StreamExt;
use tokio_stream::wrappers::UnboundedReceiverStream;
use tower_http::services::{ServeDir, ServeFile};

use spend_core::chat::{Chat, ChatEvent, ChatHistoryEntry, Selection};

#[derive(Clone)]
struct AppState {
    db_path: PathBuf,
    fx: spend_core::fx::Fx,
    chat: Chat,
}

fn app(state: AppState) -> Router {
    let frontend = PathBuf::from("frontend/dist");
    let spa =
        ServeDir::new(&frontend).not_found_service(ServeFile::new(frontend.join("index.html")));
    Router::new()
        .route("/api/meta", get(meta))
        .route("/api/summary", get(summary))
        .route("/api/series/monthly", get(monthly_series))
        .route("/api/series/yearly", get(yearly_series))
        .route("/api/series/cumulative", get(cumulative_series))
        .route("/api/series/daily", get(daily_series))
        .route("/api/series/sankey", get(sankey_series))
        .route("/api/categories", get(categories))
        .route("/api/transactions", get(list_transactions))
        .route("/api/transactions/{id}", patch(patch_transaction))
        .route("/api/fx", get(fx_spot))
        .route("/api/chat", post(chat))
        .route("/api/{*unmatched}", get(api_not_found))
        .fallback_service(spa)
        .with_state(Arc::new(state))
}

/// `year` plus an optional 1-12 `month`; `None` means the whole year.
#[derive(Debug, Deserialize)]
struct PeriodQuery {
    year: i32,
    #[serde(default)]
    month: Option<u32>,
}

/// `year` plus a required 1-12 `month`.
#[derive(Debug, Deserialize)]
struct MonthQuery {
    year: i32,
    month: u32,
}

fn valid_month(month: u32) -> bool {
    (1..=12).contains(&month)
}

/// Runs a read-only query on a blocking thread and maps errors to 500s.
/// Missing or malformed `year`/`month` query params become 400s via `Query`.
async fn run_query(
    state: State<Arc<AppState>>,
    work: impl FnOnce(&duckdb::Connection) -> anyhow::Result<serde_json::Value> + Send + 'static,
) -> Response {
    let db_path = state.db_path.clone();
    match tokio::task::spawn_blocking(move || {
        let conn = spend_core::db::api_connection(&db_path)?;
        work(&conn)
    })
    .await
    {
        Ok(Ok(json)) => Json(json).into_response(),
        Ok(Err(err)) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

async fn summary(state: State<Arc<AppState>>, query: Query<PeriodQuery>) -> Response {
    if let Some(month) = query.month
        && !valid_month(month)
    {
        return (StatusCode::BAD_REQUEST, "month must be between 1 and 12").into_response();
    }
    let year = query.year;
    run_query(state, move |conn| {
        let summary = spend_core::queries::summary(conn, year, query.month)?;
        Ok(serde_json::to_value(summary)?)
    })
    .await
}

async fn monthly_series(state: State<Arc<AppState>>, query: Query<PeriodQuery>) -> Response {
    let year = query.year;
    run_query(state, move |conn| {
        let months = spend_core::queries::monthly_spend(conn, year)?;
        Ok(serde_json::to_value(months)?)
    })
    .await
}

async fn yearly_series(state: State<Arc<AppState>>) -> Response {
    run_query(state, |conn| {
        let years = spend_core::queries::yearly_spend(conn)?;
        Ok(serde_json::to_value(years)?)
    })
    .await
}

async fn cumulative_series(state: State<Arc<AppState>>, query: Query<PeriodQuery>) -> Response {
    let year = query.year;
    run_query(state, move |conn| {
        let points = spend_core::queries::cumulative_spend(conn, year)?;
        Ok(serde_json::to_value(points)?)
    })
    .await
}

async fn daily_series(state: State<Arc<AppState>>, query: Query<MonthQuery>) -> Response {
    if !valid_month(query.month) {
        return (StatusCode::BAD_REQUEST, "month must be between 1 and 12").into_response();
    }
    let year = query.year;
    let month = query.month;
    run_query(state, move |conn| {
        let days = spend_core::queries::daily_spend(conn, year, month)?;
        Ok(serde_json::to_value(days)?)
    })
    .await
}

async fn sankey_series(state: State<Arc<AppState>>, query: Query<PeriodQuery>) -> Response {
    if let Some(month) = query.month
        && !valid_month(month)
    {
        return (StatusCode::BAD_REQUEST, "month must be between 1 and 12").into_response();
    }
    let year = query.year;
    run_query(state, move |conn| {
        let data = spend_core::queries::sankey(conn, year, query.month)?;
        Ok(serde_json::to_value(data)?)
    })
    .await
}

async fn categories(state: State<Arc<AppState>>, query: Query<PeriodQuery>) -> Response {
    if let Some(month) = query.month
        && !valid_month(month)
    {
        return (StatusCode::BAD_REQUEST, "month must be between 1 and 12").into_response();
    }
    let year = query.year;
    run_query(state, move |conn| {
        let slices = spend_core::queries::category_breakdown(conn, year, query.month)?;
        Ok(serde_json::to_value(slices)?)
    })
    .await
}

async fn meta(state: State<Arc<AppState>>) -> Response {
    run_query(state, |conn| {
        let meta = spend_core::queries::meta(conn)?;
        Ok(serde_json::to_value(meta)?)
    })
    .await
}

/// Query params for the transaction list. Everything is optional; `month`
/// (1-12), `category` (a taxonomy name) and the paging bounds are validated.
#[derive(Debug, Deserialize)]
struct TransactionQuery {
    #[serde(default)]
    year: Option<i32>,
    #[serde(default)]
    month: Option<u32>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    page: Option<u32>,
    #[serde(default)]
    page_size: Option<u32>,
}

const DEFAULT_PAGE_SIZE: u32 = 50;
const MAX_PAGE_SIZE: u32 = 200;

fn valid_category(name: &str) -> bool {
    spend_core::schema::CATEGORIES
        .iter()
        .any(|(n, _)| *n == name)
}

async fn list_transactions(
    state: State<Arc<AppState>>,
    query: Query<TransactionQuery>,
) -> Response {
    if let Some(month) = query.month
        && !valid_month(month)
    {
        return (StatusCode::BAD_REQUEST, "month must be between 1 and 12").into_response();
    }
    if let Some(category) = &query.category
        && !valid_category(category)
    {
        return (
            StatusCode::BAD_REQUEST,
            format!("unknown category '{category}'"),
        )
            .into_response();
    }
    let page = query.page.unwrap_or(1);
    if page < 1 {
        return (StatusCode::BAD_REQUEST, "page must be >= 1").into_response();
    }
    let page_size = query.page_size.unwrap_or(DEFAULT_PAGE_SIZE);
    if !(1..=MAX_PAGE_SIZE).contains(&page_size) {
        return (
            StatusCode::BAD_REQUEST,
            format!("page_size must be between 1 and {MAX_PAGE_SIZE}"),
        )
            .into_response();
    }

    let filters = spend_core::queries::TransactionFilters {
        year: query.year,
        month: query.month,
        source: query.source.clone(),
        category: query.category.clone(),
    };
    run_query(state, move |conn| {
        let (items, total) =
            spend_core::queries::list_transactions(conn, &filters, page, page_size)?;
        let pages = if total == 0 {
            0
        } else {
            (total + page_size as i64 - 1) / page_size as i64
        };
        Ok(serde_json::json!({
            "items": items,
            "total": total,
            "page": page,
            "page_size": page_size,
            "pages": pages,
        }))
    })
    .await
}

/// Body for `PATCH /api/transactions/{id}`. At least one field must be set.
#[derive(Debug, Deserialize)]
struct OverrideBody {
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    is_transfer: Option<bool>,
}

/// The outcome of a patch, mapped to distinct status codes.
enum PatchOutcome {
    Updated(serde_json::Value),
    NotFound,
    BadCategory(String),
}

fn patch_work(
    db_path: PathBuf,
    id: i64,
    category: Option<String>,
    is_transfer: Option<bool>,
) -> anyhow::Result<PatchOutcome> {
    let conn = spend_core::db::api_write_connection(&db_path)?;
    let row = match spend_core::queries::get_transaction(&conn, id)? {
        Some(row) => row,
        None => return Ok(PatchOutcome::NotFound),
    };
    // Tri-state category: not provided -> keep, "uncategorized" -> NULL,
    // a taxonomy name -> its id.
    let category_override: Option<Option<i64>> = match &category {
        Some(name) => match name.as_str() {
            "uncategorized" => Some(None),
            _ => match spend_core::queries::category_id_for_name(&conn, name)? {
                Some(cid) => Some(Some(cid)),
                None => return Ok(PatchOutcome::BadCategory(name.clone())),
            },
        },
        None => None,
    };
    let new_kind = is_transfer
        .map(|flag| spend_core::queries::kind_for_transfer(row.amount_chf, flag).to_string());
    spend_core::queries::set_transaction(&conn, id, category_override, new_kind.as_deref())?;
    let updated = spend_core::queries::get_transaction(&conn, id)?
        .ok_or_else(|| anyhow::anyhow!("transaction {id} vanished after update"))?;
    Ok(PatchOutcome::Updated(serde_json::to_value(updated)?))
}

async fn patch_transaction(
    state: State<Arc<AppState>>,
    Path(id): Path<i64>,
    Json(body): Json<OverrideBody>,
) -> Response {
    if body.category.is_none() && body.is_transfer.is_none() {
        return (
            StatusCode::BAD_REQUEST,
            "provide category and/or is_transfer",
        )
            .into_response();
    }
    let db_path = state.db_path.clone();
    let result = tokio::task::spawn_blocking(move || {
        patch_work(db_path, id, body.category, body.is_transfer)
    })
    .await;
    match result {
        Ok(Ok(PatchOutcome::Updated(json))) => Json(json).into_response(),
        Ok(Ok(PatchOutcome::NotFound)) => {
            (StatusCode::NOT_FOUND, "transaction not found").into_response()
        }
        Ok(Ok(PatchOutcome::BadCategory(name))) => (
            StatusCode::BAD_REQUEST,
            format!("unknown category '{name}'"),
        )
            .into_response(),
        Ok(Err(err)) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

/// The target currency; missing or malformed values are 400s via `Query`.
#[derive(Debug, Deserialize)]
struct FxQuery {
    to: String,
}

/// Today's CHF -> `to` spot rate in a single upstream frankfurter call.
/// CHF is the identity and only USD/EUR targets exist.
async fn fx_spot(state: State<Arc<AppState>>, query: Query<FxQuery>) -> Response {
    let to = query.to.trim().to_ascii_uppercase();
    if to != "USD" && to != "EUR" {
        return (StatusCode::BAD_REQUEST, "to must be USD or EUR").into_response();
    }
    match state.fx.spot_rate(&to).await {
        Ok((rate, date)) => Json(serde_json::json!({
            "from": "CHF",
            "to": to,
            "rate": rate,
            "date": date,
        }))
        .into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, format!("fx: {err}")).into_response(),
    }
}

/// Body for `POST /api/chat`. `message` is required; pinned chart selections
/// may travel with it as context. `history` is the prior visible conversation
/// (oldest first) and is replayed so follow-ups have context.
#[derive(Debug, Deserialize)]
struct ChatBody {
    message: String,
    #[serde(default)]
    selections: Vec<Selection>,
    #[serde(default)]
    history: Vec<ChatHistoryEntry>,
}

const MAX_MESSAGE_CHARS: usize = 20_000;
const MAX_HISTORY_ITEMS: usize = 20;

/// `POST /api/chat`: streams the assistant reply as SSE. Event kinds:
/// default (`data: <token>`), `tool` (`data: {"sql": ...}`), `chart`
/// (`data: <dashboard update JSON>`) and `error` (`data: <message>`).
/// Nothing about the conversation is persisted.
async fn chat(state: State<Arc<AppState>>, Json(body): Json<ChatBody>) -> Response {
    let message = body.message.trim().to_string();
    if message.is_empty() || message.chars().count() > MAX_MESSAGE_CHARS {
        return (
            StatusCode::BAD_REQUEST,
            format!("message must be 1..{MAX_MESSAGE_CHARS} chars"),
        )
            .into_response();
    }
    if body.history.len() > MAX_HISTORY_ITEMS {
        return (
            StatusCode::BAD_REQUEST,
            format!("history must have at most {MAX_HISTORY_ITEMS} items"),
        )
            .into_response();
    }
    for entry in &body.history {
        if let Err(err) = entry.validate() {
            return (StatusCode::BAD_REQUEST, err.to_string()).into_response();
        }
    }
    for selection in &body.selections {
        if let Err(err) = selection.validate() {
            return (StatusCode::BAD_REQUEST, err.to_string()).into_response();
        }
    }
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel();
    let chat = state.chat.clone();
    tokio::spawn(async move {
        let outcome = tokio::time::timeout(std::time::Duration::from_secs(180), {
            let selections = body.selections;
            let history = body.history;
            async {
                chat.run(&message, selections, history, tx.clone()).await;
            }
        })
        .await;
        if outcome.is_err() {
            let _ = tx.send(ChatEvent::Error("timed out".into()));
        }
    });
    let stream = UnboundedReceiverStream::new(rx).map(|event| {
        let event = match event {
            ChatEvent::Token(text) => Event::default().data(text),
            ChatEvent::ToolCall { sql } => Event::default()
                .event("tool")
                .data(serde_json::json!({ "sql": sql }).to_string()),
            ChatEvent::ChartUpdate(update) => Event::default()
                .event("chart")
                .data(serde_json::to_string(&update).unwrap_or_default()),
            ChatEvent::Error(message) => Event::default().event("error").data(message),
        };
        Ok::<_, std::io::Error>(event)
    });
    Sse::new(stream).into_response()
}

async fn api_not_found() -> (StatusCode, &'static str) {
    (StatusCode::NOT_FOUND, "not found")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let config = spend_core::config::Config::load()?;
    let db_path = config.db_path.clone();
    let state = AppState {
        chat: spend_core::chat::Chat::new(&config),
        db_path,
        fx: spend_core::fx::Fx::new(config.fx_base_url),
    };
    if !PathBuf::from("frontend/dist/index.html").exists() {
        eprintln!("warning: frontend/dist is missing; build it with `pnpm --dir frontend build`");
    }

    let host = std::env::var("HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: u16 = std::env::var("PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(3000);
    let addr: SocketAddr = format!("{host}:{port}").parse()?;
    println!(
        "yearly-spend api: db={}, listening on http://{addr}",
        state.db_path.display()
    );

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app(state)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

    /// App with FX and LLM clients pointed at dead ports; DB-only tests
    /// never trigger upstream calls.
    fn test_app(db_path: std::path::PathBuf) -> Router {
        app(AppState {
            db_path,
            fx: spend_core::fx::Fx::new("http://127.0.0.1:1"),
            chat: dead_chat(),
        })
    }

    /// Chat client whose provider endpoints are dead ports.
    fn dead_chat() -> Chat {
        let cfg = spend_core::config::Config {
            db_path: PathBuf::from("unused"),
            llm_provider: spend_core::config::LlmProvider::Local,
            llm_base_url: "http://127.0.0.1:1".into(),
            llm_api_key: "x".into(),
            llm_model: "test-model".into(),
            gemini_api_key: Some("x".into()),
            gemini_model: "test-gemini".into(),
            gemini_base_url: "http://127.0.0.1:1".into(),
            fx_base_url: "http://127.0.0.1:1".into(),
        };
        Chat::new(&cfg)
    }

    /// Chat client with the given provider pointed at `base_url`.
    fn chat_at(
        db_path: std::path::PathBuf,
        provider: spend_core::config::LlmProvider,
        base_url: String,
    ) -> Chat {
        let cfg = spend_core::config::Config {
            db_path,
            llm_provider: provider,
            llm_base_url: format!("{base_url}/v1"),
            llm_api_key: "x".into(),
            llm_model: "test-model".into(),
            gemini_api_key: Some("test-key".into()),
            gemini_model: "test-gemini".into(),
            gemini_base_url: base_url,
            fx_base_url: "http://127.0.0.1:1".into(),
        };
        Chat::new(&cfg)
    }

    /// Minimal single-endpoint frankfurter mock; counts received requests.
    async fn mock_fx_server(
        body: &str,
    ) -> (String, std::sync::Arc<std::sync::atomic::AtomicUsize>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let body = body.to_string();
        let hits_bg = hits.clone();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(_) => break,
                };
                let body = body.clone();
                let hits = hits_bg.clone();
                tokio::spawn(async move {
                    use tokio::io::{AsyncReadExt, AsyncWriteExt};
                    hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let mut buf: Vec<u8> = Vec::new();
                    let mut chunk = [0u8; 512];
                    while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                    // `connection: close`: the socket drops when the task ends.
                });
            }
        });
        (format!("http://{addr}"), hits)
    }

    fn temp_db() -> (std::path::PathBuf, std::path::PathBuf) {
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("spend-api-test-{}-{n}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let db = dir.join("spend.duckdb");
        (dir, db)
    }

    #[tokio::test]
    async fn meta_reports_seeded_data_and_empty_periods() {
        let (dir, db) = temp_db();
        spend_core::db::ingest_connection(&db).unwrap();
        let app = test_app(db);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/meta")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            StatusCode::OK,
            status,
            "response body: {}",
            String::from_utf8_lossy(&bytes)
        );
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert!(json["periods"].as_array().unwrap().is_empty());
        assert_eq!(json["accounts"].as_array().unwrap().len(), 4);
        assert_eq!(json["categories"].as_array().unwrap().len(), 18);
        let _ = std::fs::remove_dir_all(&dir);
    }

    async fn get(app: &Router, uri: &str) -> (StatusCode, serde_json::Value) {
        let response = app
            .clone()
            .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (
            status,
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| panic!("non-JSON response from {uri}: {bytes:?}")),
        )
    }

    fn seed_transactions(db: &std::path::Path) {
        let conn = spend_core::db::ingest_connection(db).unwrap();
        let food: i64 = conn
            .query_row("SELECT id FROM categories WHERE name = 'food'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let travel: i64 = conn
            .query_row("SELECT id FROM categories WHERE name = 'travel'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let income: i64 = conn
            .query_row("SELECT id FROM categories WHERE name = 'income'", [], |r| {
                r.get(0)
            })
            .unwrap();
        let transfer: i64 = conn
            .query_row(
                "SELECT id FROM categories WHERE name = 'transfer'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        let rows = [
            ("k1", "2025-01-05", food, -100.0, "spend"),
            ("k2", "2025-01-20", income, 1000.0, "income"),
            ("k3", "2025-01-30", transfer, -50.0, "transfer_out"),
            ("k8", "2025-01-30", transfer, 50.0, "transfer_in"),
            // Unpaired outflow: transfer-flagged but must not count as moved.
            ("k9", "2025-03-15", transfer, -250.0, "transfer_out"),
            ("k4", "2025-02-14", travel, -250.5, "spend"),
            ("k5", "2025-02-28", income, 2000.0, "income"),
            ("k6", "2025-12-31", food, -75.25, "spend"),
            ("k7", "2024-06-15", food, -999.0, "spend"),
        ];
        for (key, dt, category_id, amount, kind) in rows {
            conn.execute(
                "INSERT INTO transactions
                    (account_id, source, source_key, dt, description, category_id,
                     amount_orig, currency_orig, amount_chf, kind)
                 VALUES (1, 'test', ?, ?, 'x', ?, ?, 'CHF', ?, ?)",
                duckdb::params![key, dt, category_id, amount, amount, kind],
            )
            .unwrap();
        }
        // Pair the k3/k8 legs; moved is the sum of groups, not of outflows.
        conn.execute(
            "INSERT INTO transfer_groups (from_account_id, to_account_id, amount_chf, dt)
             VALUES (1, 2, 50.0, '2025-01-30')",
            [],
        )
        .unwrap();
        conn.execute(
            "UPDATE transactions SET transfer_group_id = 1 WHERE source_key IN ('k3', 'k8')",
            [],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn summary_returns_hand_checked_year_totals() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let app = test_app(db);
        let (status, json) = get(&app, "/api/summary?year=2025").await;
        assert_eq!(status, StatusCode::OK, "body: {json:?}");
        assert_eq!(
            json,
            serde_json::json!({
                "year": 2025,
                "month": null,
                "income": 3000.0,
                "spend": 425.75,
                "moved": 50.0,
                "net": 2574.25
            })
        );
        let (status, json) = get(&app, "/api/summary?year=2024").await;
        assert_eq!(status, StatusCode::OK, "body: {json:?}");
        assert_eq!(json["spend"], 999.0);
        assert_eq!(json["income"], 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn monthly_series_returns_twelve_points_for_the_year() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let app = test_app(db);
        let (status, json) = get(&app, "/api/series/monthly?year=2025").await;
        assert_eq!(status, StatusCode::OK, "body: {json:?}");
        let months = json.as_array().unwrap();
        assert_eq!(months.len(), 12);
        assert_eq!(months[0], serde_json::json!({ "month": 1, "spend": 100.0 }));
        assert_eq!(months[1], serde_json::json!({ "month": 2, "spend": 250.5 }));
        assert_eq!(months[2]["spend"], 0.0);
        assert_eq!(
            months[11],
            serde_json::json!({ "month": 12, "spend": 75.25 })
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn summary_scoped_to_a_single_month() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let app = test_app(db);
        let (status, json) = get(&app, "/api/summary?year=2025&month=1").await;
        assert_eq!(status, StatusCode::OK, "body: {json:?}");
        assert_eq!(
            json,
            serde_json::json!({
                "year": 2025,
                "month": 1,
                "income": 1000.0,
                "spend": 100.0,
                "moved": 50.0,
                "net": 900.0
            })
        );
        let (status, json) = get(&app, "/api/summary?year=2025&month=3").await;
        assert_eq!(status, StatusCode::OK, "body: {json:?}");
        assert_eq!(json["income"], 0.0);
        assert_eq!(json["spend"], 0.0);
        assert_eq!(json["net"], 0.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn yearly_series_returns_every_year_oldest_first() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let app1 = test_app(db);
        let (status, json) = get(&app1, "/api/series/yearly").await;
        assert_eq!(status, StatusCode::OK, "body: {json:?}");
        assert_eq!(
            json,
            serde_json::json!([
                { "year": 2024, "spend": 999.0 },
                { "year": 2025, "spend": 425.75 }
            ])
        );

        let (dir2, db2) = temp_db();
        spend_core::db::ingest_connection(&db2).unwrap();
        let app2 = test_app(db2);
        let (status, json) = get(&app2, "/api/series/yearly").await;
        assert_eq!(status, StatusCode::OK, "body: {json:?}");
        assert!(json.as_array().unwrap().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&dir2);
    }

    #[tokio::test]
    async fn cumulative_series_runs_up_within_the_year() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let app = test_app(db);
        let (status, json) = get(&app, "/api/series/cumulative?year=2025").await;
        assert_eq!(status, StatusCode::OK, "body: {json:?}");
        let points = json.as_array().unwrap();
        assert_eq!(points.len(), 12);
        assert_eq!(
            points[0],
            serde_json::json!({ "month": 1, "cumulative": 100.0 })
        );
        assert_eq!(
            points[1],
            serde_json::json!({ "month": 2, "cumulative": 350.5 })
        );
        assert_eq!(
            points[2],
            serde_json::json!({ "month": 3, "cumulative": 350.5 })
        );
        assert_eq!(
            points[11],
            serde_json::json!({ "month": 12, "cumulative": 425.75 })
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn daily_series_covers_every_day_of_the_month() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let app = test_app(db);
        let (status, json) = get(&app, "/api/series/daily?year=2025&month=2").await;
        assert_eq!(status, StatusCode::OK, "body: {json:?}");
        let days = json.as_array().unwrap();
        assert_eq!(days.len(), 28);
        assert_eq!(days[0], serde_json::json!({ "day": 1, "spend": 0.0 }));
        assert_eq!(days[13], serde_json::json!({ "day": 14, "spend": 250.5 }));
        assert_eq!(days[27], serde_json::json!({ "day": 28, "spend": 0.0 }));

        let (status, json) = get(&app, "/api/series/daily?year=2025&month=1").await;
        assert_eq!(status, StatusCode::OK, "body: {json:?}");
        let days = json.as_array().unwrap();
        assert_eq!(days.len(), 31);
        assert_eq!(days[4], serde_json::json!({ "day": 5, "spend": 100.0 }));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn categories_scoped_to_a_single_month() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let app = test_app(db);
        let (status, json) = get(&app, "/api/categories?year=2025&month=2").await;
        assert_eq!(status, StatusCode::OK, "body: {json:?}");
        assert_eq!(
            json,
            serde_json::json!([
                {
                    "name": "travel",
                    "color": "#8b5cf6",
                    "value": 250.5,
                    "percentage": 100.0
                }
            ])
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn categories_returns_breakdown_with_colors_and_percentages() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let app = test_app(db);
        let (status, json) = get(&app, "/api/categories?year=2025").await;
        assert_eq!(status, StatusCode::OK, "body: {json:?}");
        assert_eq!(
            json,
            serde_json::json!([
                {
                    "name": "travel",
                    "color": "#8b5cf6",
                    "value": 250.5,
                    "percentage": 58.84
                },
                {
                    "name": "food",
                    "color": "#ef4444",
                    "value": 175.25,
                    "percentage": 41.16
                }
            ])
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn endpoints_require_valid_year_and_month_query_params() {
        let (dir, db) = temp_db();
        spend_core::db::ingest_connection(&db).unwrap();
        let app = test_app(db);
        for uri in [
            "/api/summary",
            "/api/series/monthly",
            "/api/series/cumulative",
            "/api/series/daily",
            "/api/series/daily?year=2025",
            "/api/series/daily?year=2025&month=13",
            "/api/series/daily?year=2025&month=0",
            "/api/categories",
            "/api/summary?year=2025&month=13",
            "/api/summary?year=abcd",
            "/api/summary?year=2025&month=abcd",
            "/api/series/cumulative?year=2025&month=abc",
            "/api/fx",
            "/api/fx?to=",
            "/api/fx?to=GBP",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(StatusCode::BAD_REQUEST, response.status(), "uri: {uri}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn fx_endpoint_returns_today_spot_from_a_single_upstream_call() {
        let (dir, db) = temp_db();
        spend_core::db::ingest_connection(&db).unwrap();
        let (base, hits) = mock_fx_server(
            r#"{"name":"Frankfurter API","date":"2026-08-22","base":"CHF","rates":{"USD":0.79,"EUR":0.86}}"#,
        )
        .await;
        let app = app(AppState {
            db_path: db,
            fx: spend_core::fx::Fx::new(base),
            chat: dead_chat(),
        });
        let (status, json) = get(&app, "/api/fx?to=USD").await;
        assert_eq!(status, StatusCode::OK, "body: {json:?}");
        assert_eq!(
            json,
            serde_json::json!({ "from": "CHF", "to": "USD", "rate": 0.79, "date": "2026-08-22" })
        );
        let (status, json) = get(&app, "/api/fx?to=eur").await;
        assert_eq!(status, StatusCode::OK, "body: {json:?}");
        assert_eq!(
            json,
            serde_json::json!({ "from": "CHF", "to": "EUR", "rate": 0.86, "date": "2026-08-22" })
        );
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "each endpoint hit must cost exactly one upstream call"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn fx_endpoint_rejects_unknown_targets_without_upstream_calls() {
        let (dir, db) = temp_db();
        spend_core::db::ingest_connection(&db).unwrap();
        let (base, hits) = mock_fx_server(r#"{"date":"2026-08-22","rates":{}}"#).await;
        let app = app(AppState {
            db_path: db,
            fx: spend_core::fx::Fx::new(base),
            chat: dead_chat(),
        });
        for uri in ["/api/fx", "/api/fx?to=", "/api/fx?to=GBP"] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(StatusCode::BAD_REQUEST, response.status(), "uri: {uri}");
        }
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            0,
            "rejected targets must not reach the upstream"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn unknown_api_routes_get_404() {
        let (dir, db) = temp_db();
        spend_core::db::ingest_connection(&db).unwrap();
        let app = test_app(db);
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/api/nope")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let _ = std::fs::remove_dir_all(&dir);
    }

    async fn patch_raw(app: &Router, uri: &str, body: &serde_json::Value) -> (StatusCode, Vec<u8>) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(uri)
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, bytes.to_vec())
    }

    async fn patch_json(
        app: &Router,
        uri: &str,
        body: &serde_json::Value,
    ) -> (StatusCode, serde_json::Value) {
        let (status, bytes) = patch_raw(app, uri, body).await;
        (
            status,
            serde_json::from_slice(&bytes)
                .unwrap_or_else(|_| panic!("non-JSON response from {uri}: {bytes:?}")),
        )
    }

    fn tx_id(db: &std::path::Path, key: &str) -> i64 {
        let conn = spend_core::db::ingest_connection(db).unwrap();
        conn.query_row(
            "SELECT id FROM transactions WHERE source_key = ?",
            [key],
            |r| r.get(0),
        )
        .unwrap()
    }

    #[tokio::test]
    async fn transactions_list_filters_by_period_source_and_category() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let app = test_app(db);

        // No filter: all nine rows, newest first.
        let (status, json) = get(&app, "/api/transactions").await;
        assert_eq!(status, StatusCode::OK, "body: {json:?}");
        assert_eq!(json["total"], 9);
        let items = json["items"].as_array().unwrap();
        assert_eq!(items.len(), 9);
        assert_eq!(items[0]["dt"], "2025-12-31");
        assert_eq!(items[0]["category"]["name"], "food");

        // Year and month period filters.
        let (_, json) = get(&app, "/api/transactions?year=2025").await;
        assert_eq!(json["total"], 8);
        let (_, json) = get(&app, "/api/transactions?year=2025&month=1").await;
        assert_eq!(json["total"], 4);
        let (_, json) = get(&app, "/api/transactions?year=2024").await;
        assert_eq!(json["total"], 1);

        // Source filter: all rows are 'test'; 'neon' matches nothing.
        let (_, json) = get(&app, "/api/transactions?source=test&year=2024").await;
        assert_eq!(json["total"], 1);
        let (_, json) = get(&app, "/api/transactions?source=neon").await;
        assert_eq!(json["total"], 0);

        // Category filter: three food rows.
        let (_, json) = get(&app, "/api/transactions?category=food").await;
        assert_eq!(json["total"], 3);
        for item in json["items"].as_array().unwrap() {
            assert_eq!(item["category"]["name"], "food");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn transactions_list_paginates_and_reports_pages() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let app = test_app(db);
        let (_, json) = get(&app, "/api/transactions?page_size=4&page=1").await;
        assert_eq!(json["total"], 9);
        assert_eq!(json["pages"], 3);
        assert_eq!(json["page"], 1);
        assert_eq!(json["items"].as_array().unwrap().len(), 4);
        let (_, json) = get(&app, "/api/transactions?page_size=4&page=3").await;
        assert_eq!(json["items"].as_array().unwrap().len(), 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn transactions_list_validates_month_category_and_paging() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let app = test_app(db);
        for uri in [
            "/api/transactions?month=13",
            "/api/transactions?month=0",
            "/api/transactions?category=not-a-category",
            "/api/transactions?page=0",
            "/api/transactions?page_size=0",
            "/api/transactions?page_size=1000",
        ] {
            let response = app
                .clone()
                .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
                .await
                .unwrap();
            assert_eq!(StatusCode::BAD_REQUEST, response.status(), "uri: {uri}");
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn patch_category_persists_and_shifts_category_breakdown() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let app = test_app(db.clone());
        let id = tx_id(&db, "k1"); // food, -100.0, spend, 2025

        let (status, json) = patch_json(
            &app,
            &format!("/api/transactions/{id}"),
            &serde_json::json!({ "category": "dining" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {json:?}");
        assert_eq!(json["category"]["name"], "dining");
        assert_eq!(json["kind"], "spend");

        // Persists on a fresh (read-only) connection and moves the spend from
        // food to dining in the 2025 breakdown.
        let (status, json) = get(&app, "/api/categories?year=2025").await;
        assert_eq!(status, StatusCode::OK, "body: {json:?}");
        let slices = json.as_array().unwrap();
        let food: f64 = slices.iter().find(|s| s["name"] == "food").unwrap()["value"]
            .as_f64()
            .unwrap();
        let dining: f64 = slices.iter().find(|s| s["name"] == "dining").unwrap()["value"]
            .as_f64()
            .unwrap();
        // food lost k1 (100.0): 75.25 remains; dining gained it.
        assert_eq!(food, 75.25);
        assert_eq!(dining, 100.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn patch_transfer_flag_excludes_row_from_spend_kpi() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let app = test_app(db.clone());
        let id = tx_id(&db, "k4"); // travel, -250.5, spend, 2025

        // 2025 spend is 425.75 before the override.
        let (_, before) = get(&app, "/api/summary?year=2025").await;
        assert_eq!(before["spend"], 425.75);

        let (status, json) = patch_json(
            &app,
            &format!("/api/transactions/{id}"),
            &serde_json::json!({ "is_transfer": true }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {json:?}");
        assert_eq!(json["kind"], "transfer_out");
        assert_eq!(json["is_transfer"], true);

        // The row no longer counts as spend.
        let (_, after) = get(&app, "/api/summary?year=2025").await;
        assert_eq!(after["spend"], 425.75 - 250.5);

        // Toggling back restores the spend.
        let (status, _) = patch_json(
            &app,
            &format!("/api/transactions/{id}"),
            &serde_json::json!({ "is_transfer": false }),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (_, restored) = get(&app, "/api/summary?year=2025").await;
        assert_eq!(restored["spend"], 425.75);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn patch_transaction_rejects_unknown_id_empty_body_and_bad_category() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let app = test_app(db.clone());
        let id = tx_id(&db, "k1");

        // Unknown id -> 404.
        let (status, _) = patch_raw(
            &app,
            "/api/transactions/999999",
            &serde_json::json!({ "category": "dining" }),
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Empty body -> 400.
        let (status, _) = patch_raw(
            &app,
            &format!("/api/transactions/{id}"),
            &serde_json::json!({}),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);

        // Unknown category -> 400, and the row is unchanged.
        let (status, _) = patch_raw(
            &app,
            &format!("/api/transactions/{id}"),
            &serde_json::json!({ "category": "not-a-category" }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        let (_, json) = get(&app, "/api/transactions?category=food").await;
        assert_eq!(json["total"], 3);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Mock LLM server: serves `bodies[count]` (last one repeated) as an
    /// SSE response for every request. Records each request's first line
    /// and body in `log` so tests can inspect what the client sent.
    async fn mock_llm_server(
        bodies: Vec<String>,
    ) -> (
        String,
        std::sync::Arc<std::sync::atomic::AtomicUsize>,
        std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>>,
    ) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let hits = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let log: std::sync::Arc<std::sync::Mutex<Vec<(String, String)>>> =
            std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let bodies = std::sync::Arc::new(bodies);
        let (hits_bg, log_bg, bodies_bg) = (hits.clone(), log.clone(), bodies.clone());
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = match listener.accept().await {
                    Ok(pair) => pair,
                    Err(_) => break,
                };
                let (hits, log, bodies) = (hits_bg.clone(), log_bg.clone(), bodies_bg.clone());
                tokio::spawn(async move {
                    let mut buf: Vec<u8> = Vec::new();
                    let mut chunk = [0u8; 4096];
                    'outer: loop {
                        match socket.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                        if let Some(head_end) = find_bytes(&buf, b"\r\n\r\n") {
                            let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
                            let content_length = head
                                .lines()
                                .find_map(|l| {
                                    let lower = l.to_ascii_lowercase();
                                    lower.strip_prefix("content-length:").map(str::to_string)
                                })
                                .and_then(|v| v.trim().parse::<usize>().ok())
                                .unwrap_or(0);
                            if buf.len() - head_end - 4 >= content_length {
                                break 'outer;
                            }
                        }
                        if buf.len() > 1_000_000 {
                            break;
                        }
                    }
                    let (first_line, body) = match find_bytes(&buf, b"\r\n") {
                        Some(pos) => (
                            String::from_utf8_lossy(&buf[..pos]).into_owned(),
                            String::from_utf8_lossy(&buf[pos + 2..]).into_owned(),
                        ),
                        None => (String::new(), String::new()),
                    };
                    log.lock().unwrap().push((first_line, body));
                    let count = hits.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    let response_body = bodies[count.min(bodies.len() - 1)].clone();
                    let response = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        response_body.len(),
                        response_body
                    );
                    let _ = socket.write_all(response.as_bytes()).await;
                });
            }
        });
        (format!("http://{addr}"), hits, log)
    }

    fn find_bytes(hay: &[u8], needle: &[u8]) -> Option<usize> {
        hay.windows(needle.len()).position(|w| w == needle)
    }

    /// One OpenAI-style SSE frame stream: a single tool call to `run_sql`.
    fn openai_tool_sse(sql: &str) -> String {
        let args = serde_json::json!({ "sql": sql }).to_string();
        let args_escaped = serde_json::json!(args).to_string();
        format!(
            "data: {{\"choices\":[{{\"delta\":{{\"role\":\"assistant\",\"tool_calls\":[{{\"index\":0,\"id\":\"call_1\",\"function\":{{\"name\":\"run_sql\",\"arguments\":{args_escaped}}}}}]}}}}]}}\n\ndata: [DONE]\n\n"
        )
    }

    /// One OpenAI-style SSE frame stream: a tool call to `render_dashboard`.
    fn openai_render_sse(args: &serde_json::Value) -> String {
        let args_escaped = serde_json::json!(args.to_string()).to_string();
        format!(
            "data: {{\"choices\":[{{\"delta\":{{\"tool_calls\":[{{\"index\":0,\"id\":\"call_1\",\"function\":{{\"name\":\"render_dashboard\",\"arguments\":{args_escaped}}}}}]}}}}]}}\n\ndata: [DONE]\n\n"
        )
    }

    /// One OpenAI-style SSE frame stream: visible reply text.
    fn openai_text_sse(text: &str) -> String {
        let text_escaped = serde_json::json!(text).to_string();
        format!(
            "data: {{\"choices\":[{{\"delta\":{{\"content\":{text_escaped}}}}}]}}\n\ndata: [DONE]\n\n"
        )
    }

    /// One Gemini-style SSE frame stream: a function_call part.
    fn gemini_tool_sse(sql: &str) -> String {
        let args = serde_json::json!({ "sql": sql }).to_string();
        format!(
            "data: {{\"candidates\":[{{\"content\":{{\"parts\":[{{\"function_call\":{{\"name\":\"run_sql\",\"args\":{args}}}}}]}}}}]}}\n\n"
        )
    }

    /// One Gemini-style SSE frame stream: a `render_dashboard` function_call.
    fn gemini_render_sse(args: &serde_json::Value) -> String {
        format!(
            "data: {{\"candidates\":[{{\"content\":{{\"parts\":[{{\"function_call\":{{\"name\":\"render_dashboard\",\"args\":{args}}}}}]}}}}]}}\n\n"
        )
    }

    /// One Gemini-style SSE frame stream: a text part.
    fn gemini_text_sse(text: &str) -> String {
        let text_escaped = serde_json::json!(text).to_string();
        format!(
            "data: {{\"candidates\":[{{\"content\":{{\"parts\":[{{\"text\":{text_escaped}}}]}}}}]}}\n\n"
        )
    }

    async fn post_chat_raw(app: &Router, body: &str) -> (StatusCode, String) {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/api/chat")
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_string()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn post_chat(app: &Router, body: &serde_json::Value) -> (StatusCode, String) {
        post_chat_raw(app, &body.to_string()).await
    }

    #[tokio::test]
    async fn chat_streams_tool_round_trip_for_local_provider() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let sql = "SELECT COUNT(*) AS n FROM transactions";
        let (base, hits, log) = mock_llm_server(vec![
            openai_tool_sse(sql),
            openai_text_sse("The database holds 9 rows."),
        ])
        .await;
        let chat = chat_at(db.clone(), spend_core::config::LlmProvider::Local, base);
        let app = app(AppState {
            db_path: db,
            fx: spend_core::fx::Fx::new("http://127.0.0.1:1"),
            chat,
        });
        let (status, sse) = post_chat(
            &app,
            &serde_json::json!({
                "message": "How many transactions are there?",
                "selections": [{
                    "chart": "monthly",
                    "series": "spend",
                    "label": "2025-01",
                    "value": 100.0,
                    "year": 2025,
                    "month": 1
                }]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {sse}");
        assert!(sse.contains("event: tool"), "body: {sse}");
        assert!(sse.contains(&format!("\"sql\":\"{sql}\"")), "body: {sse}");
        assert!(sse.contains("The database holds 9 rows."), "body: {sse}");

        // Two model round trips: the second carries the executed tool result
        // (the real count, as JSON) and the pinned selection context.
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "expected exactly two model round trips"
        );
        let log = log.lock().unwrap();
        assert_eq!(log.len(), 2);
        assert!(
            log[0].0.contains("POST /v1/chat/completions"),
            "got: {}",
            log[0].0
        );
        assert!(
            log[0].1.contains("2025-01"),
            "selection context missing: {}",
            log[0].1
        );
        assert!(
            log[1].1.contains("\"role\":\"tool\""),
            "tool result missing: {}",
            log[1].1
        );
        assert!(
            log[1]
                .1
                .contains(r#"{\"columns\":[\"n\"],\"rows\":[{\"n\":9}],\"truncated\":false}"#),
            "tool result rows missing or wrong: {}",
            log[1].1
        );
        drop(log);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn chat_rejects_non_select_sql_and_leaves_db_unchanged() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let sql = "DELETE FROM transactions";
        let (base, _hits, log) =
            mock_llm_server(vec![openai_tool_sse(sql), openai_text_sse("Done.")]).await;
        let chat = chat_at(db.clone(), spend_core::config::LlmProvider::Local, base);
        let app = app(AppState {
            db_path: db.clone(),
            fx: spend_core::fx::Fx::new("http://127.0.0.1:1"),
            chat,
        });
        let (status, sse) = post_chat(
            &app,
            &serde_json::json!({ "message": "Clean up the database" }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {sse}");
        assert!(sse.contains("event: tool"), "body: {sse}");
        assert!(sse.contains(&format!("\"sql\":\"{sql}\"")), "body: {sse}");

        // The tool result fed back to the model is the rejection reason...
        let log = log.lock().unwrap();
        assert!(
            log.get(1)
                .is_some_and(|(_, body)| body.contains("rejected:")),
            "rejection missing from tool result: {log:?}"
        );
        drop(log);
        // ...and the table is untouched.
        let conn = spend_core::db::api_connection(&db).unwrap();
        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM transactions", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 9);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn chat_streams_gemini_function_call_then_text() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let sql = "SELECT COUNT(*) AS n FROM transactions";
        let (base, hits, log) = mock_llm_server(vec![
            gemini_tool_sse(sql),
            gemini_text_sse("Gemini says there are 9 rows."),
        ])
        .await;
        let chat = chat_at(db.clone(), spend_core::config::LlmProvider::Gemini, base);
        let app = app(AppState {
            db_path: db,
            fx: spend_core::fx::Fx::new("http://127.0.0.1:1"),
            chat,
        });
        let (status, sse) =
            post_chat(&app, &serde_json::json!({ "message": "Count the rows" })).await;
        assert_eq!(status, StatusCode::OK, "body: {sse}");
        assert!(sse.contains("event: tool"), "body: {sse}");
        assert!(sse.contains(&format!("\"sql\":\"{sql}\"")), "body: {sse}");
        assert!(sse.contains("Gemini says there are 9 rows."), "body: {sse}");
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "expected exactly two model round trips"
        );
        let log = log.lock().unwrap();
        // Gemini wire path and API key.
        assert!(
            log[0]
                .0
                .contains("/models/test-gemini:streamGenerateContent?alt=sse"),
            "got: {}",
            log[0].0
        );
        assert!(log[0].0.contains("key=test-key"), "got: {}", log[0].0);
        // The follow-up carries the function response in Gemini content form.
        assert!(
            log.get(1)
                .is_some_and(|(_, body)| body.contains("function_response")),
            "function response missing: {log:?}"
        );
        drop(log);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn chat_streams_chart_update_for_render_dashboard() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let args = serde_json::json!({
            "year": 2025,
            "label": "2025",
            "kpi": { "income": 3000.0, "spend": 425.75, "moved": 50.0 },
            "monthly": [{ "month": 1, "value": 225.25 }],
            "categories": [{ "name": "food", "value": 175.25 }]
        });
        let (base, hits, log) = mock_llm_server(vec![
            openai_render_sse(&args),
            openai_text_sse("The dashboard now shows 2025."),
        ])
        .await;
        let chat = chat_at(db.clone(), spend_core::config::LlmProvider::Local, base);
        let app = app(AppState {
            db_path: db,
            fx: spend_core::fx::Fx::new("http://127.0.0.1:1"),
            chat,
        });
        let (status, sse) = post_chat(&app, &serde_json::json!({ "message": "Show 2025" })).await;
        assert_eq!(status, StatusCode::OK, "body: {sse}");
        // The chart event carries the validated payload as JSON.
        assert!(sse.contains("event: chart"), "body: {sse}");
        assert!(sse.contains("\"label\":\"2025\""), "body: {sse}");
        assert!(sse.contains("\"value\":175.25"), "body: {sse}");
        assert!(sse.contains("The dashboard now shows 2025."), "body: {sse}");
        // No generic tool frame for render_dashboard (it has no SQL to show).
        assert!(!sse.contains("event: tool"), "body: {sse}");
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "expected exactly two model round trips"
        );
        let log = log.lock().unwrap();
        // The tool result fed back to the model confirms the update.
        assert!(
            log.get(1)
                .is_some_and(|(_, body)| body.contains("ok: dashboard charts updated for 2025")),
            "tool result missing: {log:?}"
        );
        drop(log);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn chat_rejects_invalid_render_dashboard() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        // 'unknown' is not in the seeded taxonomy, so the update must be
        // rejected and no chart event must be streamed.
        let args = serde_json::json!({
            "year": 2025,
            "label": "2025",
            "categories": [{ "name": "unknown", "value": 1.0 }]
        });
        let (base, hits, log) = mock_llm_server(vec![
            openai_render_sse(&args),
            openai_text_sse("The category does not exist."),
        ])
        .await;
        let chat = chat_at(db.clone(), spend_core::config::LlmProvider::Local, base);
        let app = app(AppState {
            db_path: db,
            fx: spend_core::fx::Fx::new("http://127.0.0.1:1"),
            chat,
        });
        let (status, sse) = post_chat(&app, &serde_json::json!({ "message": "Show food" })).await;
        assert_eq!(status, StatusCode::OK, "body: {sse}");
        assert!(!sse.contains("event: chart"), "body: {sse}");
        assert!(sse.contains("The category does not exist."), "body: {sse}");
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "expected exactly two model round trips"
        );
        let log = log.lock().unwrap();
        assert!(
            log.get(1)
                .is_some_and(|(_, body)| body.contains("rejected: unknown category 'unknown'")),
            "tool result missing: {log:?}"
        );
        drop(log);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn chat_streams_chart_update_gemini() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let args = serde_json::json!({
            "year": 2025,
            "label": "2025",
            "yearly": [{ "year": 2025, "value": 425.75 }]
        });
        let (base, hits, _log) = mock_llm_server(vec![
            gemini_render_sse(&args),
            gemini_text_sse("Yearly totals on the dashboard."),
        ])
        .await;
        let chat = chat_at(db.clone(), spend_core::config::LlmProvider::Gemini, base);
        let app = app(AppState {
            db_path: db,
            fx: spend_core::fx::Fx::new("http://127.0.0.1:1"),
            chat,
        });
        let (status, sse) = post_chat(&app, &serde_json::json!({ "message": "Show yearly" })).await;
        assert_eq!(status, StatusCode::OK, "body: {sse}");
        assert!(sse.contains("event: chart"), "body: {sse}");
        assert!(sse.contains("\"value\":425.75"), "body: {sse}");
        assert!(
            sse.contains("Yearly totals on the dashboard."),
            "body: {sse}"
        );
        assert_eq!(
            hits.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "expected exactly two model round trips"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn chat_validates_message_and_selections() {
        let (dir, db) = temp_db();
        spend_core::db::ingest_connection(&db).unwrap();
        let app = test_app(db);
        // Empty message -> 400.
        let (status, sse) = post_chat(&app, &serde_json::json!({ "message": "   " })).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {sse}");
        // Invalid selection month -> 400.
        let (status, sse) = post_chat(
            &app,
            &serde_json::json!({
                "message": "hi",
                "selections": [{
                    "chart": "monthly",
                    "series": "spend",
                    "label": "2025-13",
                    "value": 1.0,
                    "year": 2025,
                    "month": 13
                }]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {sse}");
        // Empty note -> 400.
        let (status, sse) = post_chat(
            &app,
            &serde_json::json!({
                "message": "hi",
                "selections": [{
                    "chart": "monthly",
                    "series": "spend",
                    "label": "2025-01",
                    "value": 1.0,
                    "note": "   "
                }]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {sse}");
        // Non-finite selection value (1e309 overflows f64) -> 400 at JSON parse.
        let (status, sse) = post_chat_raw(
            &app,
            r#"{"message":"hi","selections":[{"chart":"monthly","series":"spend","label":"x","value":1e309}]}"#,
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "body: {sse}");
        // Multiple selections from the same chart are accepted.
        let selections: Vec<serde_json::Value> = (0..11)
            .map(|i| {
                serde_json::json!({
                    "chart": "monthly",
                    "series": "spend",
                    "label": format!("{i}"),
                    "value": 1.0
                })
            })
            .collect();
        let (status, sse) = post_chat(
            &app,
            &serde_json::json!({ "message": "hi", "selections": selections }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {sse}");
        // Sankey selections and notes are accepted.
        let (status, sse) = post_chat(
            &app,
            &serde_json::json!({
                "message": "hi",
                "selections": [{
                    "chart": "sankey",
                    "series": "spend",
                    "label": "cash -> groceries",
                    "value": 42.0,
                    "year": 2025,
                    "note": "look into this"
                }]
            }),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "body: {sse}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
