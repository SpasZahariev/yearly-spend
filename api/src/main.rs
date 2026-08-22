use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
use serde::Deserialize;
use tower_http::services::{ServeDir, ServeFile};

#[derive(Clone)]
struct AppState {
    db_path: PathBuf,
}

fn app(state: AppState) -> Router {
    let frontend = PathBuf::from("frontend/dist");
    let spa =
        ServeDir::new(&frontend).not_found_service(ServeFile::new(frontend.join("index.html")));
    Router::new()
        .route("/api/meta", get(meta))
        .route("/api/summary", get(summary))
        .route("/api/series/monthly", get(monthly_series))
        .route("/api/categories", get(categories))
        .route("/api/{*unmatched}", get(api_not_found))
        .fallback_service(spa)
        .with_state(Arc::new(state))
}

#[derive(Debug, Deserialize)]
struct YearQuery {
    year: i32,
}

/// Runs a year-scoped read-only query on a blocking thread and maps errors to
/// 500s. Missing or invalid `year` query params become 400s via `Query`.
async fn with_year(
    State(state): State<Arc<AppState>>,
    year: i32,
    work: impl FnOnce(&duckdb::Connection, i32) -> anyhow::Result<serde_json::Value> + Send + 'static,
) -> Response {
    let db_path = state.db_path.clone();
    match tokio::task::spawn_blocking(move || {
        let conn = spend_core::db::api_connection(&db_path)?;
        work(&conn, year)
    })
    .await
    {
        Ok(Ok(json)) => Json(json).into_response(),
        Ok(Err(err)) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
}

async fn summary(state: State<Arc<AppState>>, query: Query<YearQuery>) -> Response {
    with_year(state, query.year, |conn, year| {
        let summary = spend_core::queries::summary(conn, year)?;
        Ok(serde_json::to_value(summary)?)
    })
    .await
}

async fn monthly_series(state: State<Arc<AppState>>, query: Query<YearQuery>) -> Response {
    with_year(state, query.year, |conn, year| {
        let months = spend_core::queries::monthly_spend(conn, year)?;
        Ok(serde_json::to_value(months)?)
    })
    .await
}

async fn categories(state: State<Arc<AppState>>, query: Query<YearQuery>) -> Response {
    with_year(state, query.year, |conn, year| {
        let slices = spend_core::queries::category_breakdown(conn, year)?;
        Ok(serde_json::to_value(slices)?)
    })
    .await
}

async fn meta(State(state): State<Arc<AppState>>) -> Response {
    let db_path = state.db_path.clone();
    match tokio::task::spawn_blocking(move || {
        let conn = spend_core::db::api_connection(&db_path)?;
        spend_core::queries::meta(&conn)
    })
    .await
    {
        Ok(Ok(meta)) => Json(meta).into_response(),
        Ok(Err(err)) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
        Err(err) => (StatusCode::INTERNAL_SERVER_ERROR, err.to_string()).into_response(),
    }
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
    let state = AppState { db_path };
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
        let app = app(AppState { db_path: db });
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
    }

    #[tokio::test]
    async fn summary_returns_hand_checked_year_totals() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let app = app(AppState { db_path: db });
        let (status, json) = get(&app, "/api/summary?year=2025").await;
        assert_eq!(status, StatusCode::OK, "body: {json:?}");
        assert_eq!(
            json,
            serde_json::json!({
                "year": 2025,
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
        let app = app(AppState { db_path: db });
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
    async fn categories_returns_breakdown_with_colors_and_percentages() {
        let (dir, db) = temp_db();
        seed_transactions(&db);
        let app = app(AppState { db_path: db });
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
    async fn endpoints_require_a_year_query_param() {
        let (dir, db) = temp_db();
        spend_core::db::ingest_connection(&db).unwrap();
        let app = app(AppState { db_path: db });
        for uri in [
            "/api/summary",
            "/api/series/monthly",
            "/api/categories",
            "/api/summary?year=abcd",
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
    async fn unknown_api_routes_get_404() {
        let (dir, db) = temp_db();
        spend_core::db::ingest_connection(&db).unwrap();
        let app = app(AppState { db_path: db });
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
}
