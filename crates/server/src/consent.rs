//! Consent records and the single predicate the rest of the server asks:
//! may this actor's data be captured or exported right now?

use axum::{extract::State, Json};
use engine::telemetry::PrivacyMode;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::SqlitePool;

use crate::{auth::AuthUser, error::ApiError, now_rfc3339};

/// The latest consent row for an actor.
#[derive(Debug, Clone)]
pub struct CurrentConsent {
    pub mode: PrivacyMode,
    pub granted_at: String,
    pub revoked_at: Option<String>,
}

impl CurrentConsent {
    /// The gate. Anything other than a live, non-`off` grant means no.
    pub fn captures(&self) -> bool {
        self.revoked_at.is_none() && self.mode.captures()
    }
}

/// `None` means the actor has never consented, which is also a no.
pub async fn current(
    pool: &SqlitePool,
    actor_id: &str,
) -> Result<Option<CurrentConsent>, sqlx::Error> {
    let row: Option<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT mode, granted_at, revoked_at FROM consents WHERE actor_id = ? ORDER BY id DESC LIMIT 1",
    )
    .bind(actor_id)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(mode, granted_at, revoked_at)| CurrentConsent {
        // A mode this build does not understand fails closed, not open.
        mode: PrivacyMode::parse(&mode).unwrap_or(PrivacyMode::Off),
        granted_at,
        revoked_at,
    }))
}

#[derive(Debug, Deserialize)]
pub struct ConsentRequest {
    pub mode: String,
    pub consent_text_version: String,
}

pub async fn record(
    State(pool): State<SqlitePool>,
    user: AuthUser,
    Json(req): Json<ConsentRequest>,
) -> Result<Json<Value>, ApiError> {
    let mode = PrivacyMode::parse(&req.mode)
        .ok_or_else(|| ApiError::BadRequest(format!("unknown consent mode {:?}", req.mode)))?;
    if req.consent_text_version.trim().is_empty() {
        return Err(ApiError::BadRequest(
            "consent_text_version is required".into(),
        ));
    }

    let now = now_rfc3339();
    let mut tx = pool.begin().await?;
    // Choosing `off` is a withdrawal: outstanding grants are revoked, and the
    // `off` row records itself as revoked so both halves of the gate agree.
    let revoked_at = if mode == PrivacyMode::Off {
        sqlx::query("UPDATE consents SET revoked_at = ? WHERE actor_id = ? AND revoked_at IS NULL")
            .bind(&now)
            .bind(&user.id)
            .execute(&mut *tx)
            .await?;
        Some(now.clone())
    } else {
        None
    };
    sqlx::query(
        "INSERT INTO consents (actor_id, mode, consent_text_version, granted_at, revoked_at)
         VALUES (?, ?, ?, ?, ?)",
    )
    .bind(&user.id)
    .bind(mode.as_str())
    .bind(&req.consent_text_version)
    .bind(&now)
    .bind(&revoked_at)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;

    Ok(Json(json!({
        "actor_id": user.id,
        "mode": mode.as_str(),
        "consent_text_version": req.consent_text_version,
        "granted_at": now,
        "revoked_at": revoked_at,
        "captures": mode.captures() && revoked_at.is_none(),
    })))
}

pub async fn me(State(pool): State<SqlitePool>, user: AuthUser) -> Result<Json<Value>, ApiError> {
    let body = match current(&pool, &user.id).await? {
        Some(c) => json!({
            "actor_id": user.id,
            "mode": c.mode.as_str(),
            "granted_at": c.granted_at,
            "revoked_at": c.revoked_at,
            "captures": c.captures(),
        }),
        None => json!({
            "actor_id": user.id,
            "mode": Value::Null,
            "granted_at": Value::Null,
            "revoked_at": Value::Null,
            "captures": false,
        }),
    };
    Ok(Json(body))
}
