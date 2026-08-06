//! Gridline API server: auth, consent, event ingest, workbooks, routines.
//!
//! Everything is exposed as `app(pool) -> Router` so the whole surface can be
//! driven in process by the tests; `main.rs` adds only a listener, tracing,
//! CORS and migrations.

pub mod auth;
pub mod consent;
pub mod error;
pub mod events;
pub mod routines;
pub mod sessions;
pub mod workbooks;

pub use auth::{seed_user, AdminUser, AuthUser, SeededUser};
pub use error::ApiError;
pub use sessions::{sessions_for, SessionSummary, SESSION_GAP_MS};

use axum::{
    routing::{get, post},
    Json, Router,
};
use serde_json::{json, Value};
use sqlx::SqlitePool;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");

pub async fn migrate(pool: &SqlitePool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!().run(pool).await
}

pub fn app(pool: SqlitePool) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/v1/events", post(events::ingest))
        .route("/v1/events/export", get(events::export))
        .route("/v1/events/recent", get(events::recent))
        .route("/v1/consent", post(consent::record))
        .route("/v1/consent/me", get(consent::me))
        .route(
            "/v1/workbooks",
            post(workbooks::upsert).get(workbooks::list),
        )
        .route("/v1/workbooks/{id}", get(workbooks::get_one))
        .route("/v1/routines", get(routines::list))
        .route("/v1/routines/{id}/feedback", post(routines::feedback))
        .route("/v1/sessions", get(sessions::list))
        .with_state(pool)
}

async fn health() -> Json<Value> {
    Json(json!({ "status": "ok", "version": VERSION }))
}

/// Fixed-width UTC, so `received_at` sorts and compares as a string.
pub(crate) fn now_rfc3339() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true)
}
