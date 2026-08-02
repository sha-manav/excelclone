//! Mined routines and the user's verdict on them. The miner writes rows
//! here; the server only serves them back and records feedback.

use axum::{
    extract::{Path, Query, State},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;

use crate::{auth::AuthUser, error::ApiError};

#[derive(Debug, Deserialize)]
pub struct RoutineQuery {
    pub workbook: String,
}

#[derive(Debug, Serialize)]
pub struct Routine {
    pub id: String,
    pub workbook_id: String,
    pub summary: String,
    pub body: Value,
    pub estimated_minutes_saved: f64,
    pub support: i64,
    pub status: String,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct FeedbackRequest {
    pub status: String,
}

type RoutineRow = (String, String, String, String, f64, i64, String, String);

pub async fn list(
    State(pool): State<SqlitePool>,
    user: AuthUser,
    Query(query): Query<RoutineQuery>,
) -> Result<Json<Vec<Routine>>, ApiError> {
    let rows: Vec<RoutineRow> = sqlx::query_as(
        "SELECT id, workbook_id, summary, body, estimated_minutes_saved, support, status, created_at
         FROM routines
         WHERE actor_id = ? AND workbook_id = ?
         ORDER BY estimated_minutes_saved DESC, support DESC, id",
    )
    .bind(&user.id)
    .bind(&query.workbook)
    .fetch_all(&pool)
    .await?;

    Ok(Json(
        rows.into_iter()
            .map(
                |(
                    id,
                    workbook_id,
                    summary,
                    body,
                    estimated_minutes_saved,
                    support,
                    status,
                    created_at,
                )| Routine {
                    id,
                    workbook_id,
                    summary,
                    body: serde_json::from_str(&body).unwrap_or(Value::Null),
                    estimated_minutes_saved,
                    support,
                    status,
                    created_at,
                },
            )
            .collect(),
    ))
}

pub async fn feedback(
    State(pool): State<SqlitePool>,
    user: AuthUser,
    Path(id): Path<String>,
    Json(req): Json<FeedbackRequest>,
) -> Result<Json<Value>, ApiError> {
    if !matches!(req.status.as_str(), "accepted" | "dismissed") {
        return Err(ApiError::BadRequest(format!(
            "status must be \"accepted\" or \"dismissed\", got {:?}",
            req.status
        )));
    }
    let result = sqlx::query("UPDATE routines SET status = ? WHERE id = ? AND actor_id = ?")
        .bind(&req.status)
        .bind(&id)
        .bind(&user.id)
        .execute(&pool)
        .await?;
    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(Json(serde_json::json!({ "id": id, "status": req.status })))
}
