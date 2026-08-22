use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Json, Response};
use axum::routing::get;
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
        .route("/api/{*unmatched}", get(api_not_found))
        .fallback_service(spa)
        .with_state(Arc::new(state))
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
