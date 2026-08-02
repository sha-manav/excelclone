//! Workbook storage: latest-wins, scoped to the owning actor.
//!
//! A workbook belonging to someone else answers 404, never 403 — a 403 would
//! confirm the id exists.

use axum::{
    extract::{Path, State},
    Json,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sqlx::SqlitePool;
use ulid::Ulid;

use crate::{auth::AuthUser, error::ApiError, now_rfc3339};

#[derive(Debug, Deserialize)]
pub struct WorkbookRequest {
    pub id: Option<String>,
    pub name: String,
    pub state: Value,
}

#[derive(Debug, Serialize)]
pub struct WorkbookSummary {
    pub id: String,
    pub name: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize)]
pub struct Workbook {
    pub id: String,
    pub name: String,
    pub state: Value,
    pub updated_at: String,
}

pub async fn upsert(
    State(pool): State<SqlitePool>,
    user: AuthUser,
    Json(req): Json<WorkbookRequest>,
) -> Result<Json<WorkbookSummary>, ApiError> {
    if req.name.trim().is_empty() {
        return Err(ApiError::BadRequest("name is required".into()));
    }
    let id = match req.id {
        Some(id) if !id.trim().is_empty() => id,
        _ => format!("wb_{}", Ulid::new().to_string().to_lowercase()),
    };
    let updated_at = now_rfc3339();
    let state = serde_json::to_string(&req.state).unwrap_or_else(|_| "null".into());

    // The `WHERE` on the upsert is the ownership check: a conflict on someone
    // else's id updates nothing, and reports as a 404 below.
    let result = sqlx::query(
        "INSERT INTO workbooks (id, actor_id, name, state, updated_at) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
             name = excluded.name,
             state = excluded.state,
             updated_at = excluded.updated_at
         WHERE workbooks.actor_id = excluded.actor_id",
    )
    .bind(&id)
    .bind(&user.id)
    .bind(&req.name)
    .bind(&state)
    .bind(&updated_at)
    .execute(&pool)
    .await?;

    if result.rows_affected() == 0 {
        return Err(ApiError::NotFound);
    }
    Ok(Json(WorkbookSummary {
        id,
        name: req.name,
        updated_at,
    }))
}

pub async fn list(
    State(pool): State<SqlitePool>,
    user: AuthUser,
) -> Result<Json<Vec<WorkbookSummary>>, ApiError> {
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT id, name, updated_at FROM workbooks WHERE actor_id = ? ORDER BY updated_at DESC, id",
    )
    .bind(&user.id)
    .fetch_all(&pool)
    .await?;
    Ok(Json(
        rows.into_iter()
            .map(|(id, name, updated_at)| WorkbookSummary {
                id,
                name,
                updated_at,
            })
            .collect(),
    ))
}

pub async fn get_one(
    State(pool): State<SqlitePool>,
    user: AuthUser,
    Path(id): Path<String>,
) -> Result<Json<Workbook>, ApiError> {
    let row: Option<(String, String, String, String)> = sqlx::query_as(
        "SELECT id, name, state, updated_at FROM workbooks WHERE id = ? AND actor_id = ?",
    )
    .bind(&id)
    .bind(&user.id)
    .fetch_optional(&pool)
    .await?;
    let (id, name, state, updated_at) = row.ok_or(ApiError::NotFound)?;
    Ok(Json(Workbook {
        id,
        name,
        state: serde_json::from_str(&state).unwrap_or(Value::Null),
        updated_at,
    }))
}
