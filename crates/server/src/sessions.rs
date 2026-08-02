//! Server-side sessionization.
//!
//! The client stamps a `session_id` on every envelope, but the client is not
//! the authority: a tab left open overnight, a clock adjustment, or two
//! windows of the same app all produce ids that do not match what actually
//! happened. Sessions are therefore derived here from inactivity gaps in the
//! actor's own event stream, and the client's id is carried alongside so the
//! disagreement is visible rather than silent.

use axum::{extract::State, Json};
use serde::Serialize;
use sqlx::SqlitePool;

use crate::{auth::AuthUser, error::ApiError};

/// A session ends after ten minutes of actor inactivity (`docs/EVENTS.md`).
pub const SESSION_GAP_MS: i64 = 600_000;

#[derive(Debug, Clone, Serialize)]
pub struct SessionSummary {
    /// Derived id, stable for a given actor and start time.
    pub session_id: String,
    pub started_ms: i64,
    pub ended_ms: i64,
    pub duration_ms: i64,
    pub event_count: usize,
    pub workbooks: Vec<String>,
    /// Every `session_id` the client claimed inside this derived session.
    /// More than one entry means the client split a session the server did
    /// not, or never rotated its id across a real break.
    pub client_session_ids: Vec<String>,
}

pub async fn sessions_for(
    pool: &SqlitePool,
    actor_id: &str,
) -> Result<Vec<SessionSummary>, sqlx::Error> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT ts_ms, session_id, workbook_id FROM events WHERE actor_id = ? ORDER BY ts_ms, seq",
    )
    .bind(actor_id)
    .fetch_all(pool)
    .await?;

    let mut sessions: Vec<SessionSummary> = Vec::new();
    for (ts_ms, client_session_id, workbook_id) in rows {
        match sessions.last_mut() {
            Some(open) if ts_ms - open.ended_ms <= SESSION_GAP_MS => {
                open.ended_ms = ts_ms;
                open.duration_ms = open.ended_ms - open.started_ms;
                open.event_count += 1;
                push_unique(&mut open.workbooks, workbook_id);
                push_unique(&mut open.client_session_ids, client_session_id);
            }
            _ => sessions.push(SessionSummary {
                session_id: format!("s_{actor_id}_{ts_ms}"),
                started_ms: ts_ms,
                ended_ms: ts_ms,
                duration_ms: 0,
                event_count: 1,
                workbooks: vec![workbook_id],
                client_session_ids: vec![client_session_id],
            }),
        }
    }
    Ok(sessions)
}

fn push_unique(values: &mut Vec<String>, value: String) {
    if !values.contains(&value) {
        values.push(value);
    }
}

pub async fn list(
    State(pool): State<SqlitePool>,
    user: AuthUser,
) -> Result<Json<Vec<SessionSummary>>, ApiError> {
    Ok(Json(sessions_for(&pool, &user.id).await?))
}
