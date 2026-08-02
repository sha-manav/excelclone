//! Event ingest and dataset export.
//!
//! Ingest is idempotent on `event_id` (delivery is at-least-once, see
//! `docs/EVENTS.md`) and gated on consent. A batch is never all-or-nothing:
//! one malformed envelope must not cost a user the rest of their work.

use axum::{
    body::Body,
    extract::{Query, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use chrono::{DateTime, SecondsFormat, Utc};
use engine::telemetry::{
    EventContext, EventEnvelope, PrivacyMode, ACTION_VOCABULARY, SCHEMA_VERSION,
};
use serde::{Deserialize, Serialize};
use sqlx::{Row, SqlitePool};
use tokio_stream::StreamExt;

use crate::{
    auth::{AdminUser, AuthUser},
    consent::{self, CurrentConsent},
    error::ApiError,
    now_rfc3339,
};

const DEFAULT_EXPORT_LIMIT: i64 = 10_000;

#[derive(Debug, Deserialize)]
pub struct IngestRequest {
    pub events: Vec<EventEnvelope>,
}

#[derive(Debug, Default, Serialize)]
pub struct IngestResponse {
    pub accepted: usize,
    pub duplicates: usize,
    pub rejected: usize,
    pub warnings: Vec<String>,
}

/// Why an envelope was refused, and whether consent was the reason. The
/// distinction decides the status code: a batch turned away entirely because
/// the actor has not consented is a 403, not a partially successful 200.
struct Rejection {
    reason: String,
    consent: bool,
}

fn refuse(reason: impl Into<String>) -> Rejection {
    Rejection {
        reason: reason.into(),
        consent: false,
    }
}

fn refuse_consent(reason: impl Into<String>) -> Rejection {
    Rejection {
        reason: reason.into(),
        consent: true,
    }
}

fn check(
    ev: &EventEnvelope,
    actor_id: &str,
    consent: Option<&CurrentConsent>,
) -> Option<Rejection> {
    // Checked before consent: writing into someone else's log is a different
    // failure from having no consent of your own, and must not turn the whole
    // batch into a 403 about consent.
    if ev.actor_id != actor_id {
        return Some(refuse(format!(
            "actor_id {:?} does not match the authenticated actor",
            ev.actor_id
        )));
    }
    match consent {
        None => return Some(refuse_consent("no consent on record for this actor")),
        Some(c) if c.revoked_at.is_some() => {
            return Some(refuse_consent("consent has been revoked"));
        }
        Some(c) if !c.mode.captures() => {
            return Some(refuse_consent("consent mode is off"));
        }
        // The envelope may not claim a wider mode than the actor agreed to;
        // otherwise a client bug or a forged batch could smuggle in verbatim
        // values under a structural grant.
        Some(c)
            if c.mode == PrivacyMode::Structural
                && ev.context.privacy_mode == PrivacyMode::Full =>
        {
            return Some(refuse(
                "privacy_mode \"full\" exceeds the consented mode \"structural\"",
            ));
        }
        Some(_) => {}
    }
    if ev.schema_version != SCHEMA_VERSION {
        return Some(refuse(format!(
            "unknown schema_version {} (server speaks {SCHEMA_VERSION})",
            ev.schema_version
        )));
    }
    if !ACTION_VOCABULARY.contains(&ev.action.as_str()) {
        return Some(refuse(format!(
            "action {:?} is not in the documented vocabulary",
            ev.action
        )));
    }
    if ev.seq > i64::MAX as u64 {
        return Some(refuse(format!("seq {} is out of range", ev.seq)));
    }
    if ev.event_id.trim().is_empty() {
        return Some(refuse("event_id is empty"));
    }
    None
}

pub async fn ingest(
    State(pool): State<SqlitePool>,
    user: AuthUser,
    Json(req): Json<IngestRequest>,
) -> Result<Response, ApiError> {
    let consent = consent::current(&pool, &user.id).await?;
    let received_at = now_rfc3339();
    let mut out = IngestResponse::default();
    let mut consent_rejected = 0usize;

    for ev in &req.events {
        if let Some(rejection) = check(ev, &user.id, consent.as_ref()) {
            out.rejected += 1;
            if rejection.consent {
                consent_rejected += 1;
            }
            out.warnings
                .push(format!("{}: {}", ev.event_id, rejection.reason));
            continue;
        }

        let result = sqlx::query(
            "INSERT INTO events
                (event_id, actor_id, session_id, workbook_id, seq, ts_ms, action,
                 payload, context, client_version, received_at)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
             ON CONFLICT(event_id) DO NOTHING",
        )
        .bind(&ev.event_id)
        .bind(&ev.actor_id)
        .bind(&ev.session_id)
        .bind(&ev.workbook_id)
        .bind(ev.seq as i64)
        .bind(ev.ts_ms)
        .bind(&ev.action)
        .bind(json_text(&ev.payload))
        .bind(json_text(&ev.context))
        .bind(&ev.client_version)
        .bind(&received_at)
        .execute(&pool)
        .await?;

        if result.rows_affected() == 0 {
            out.duplicates += 1;
        } else {
            out.accepted += 1;
        }
    }

    // Whole batch turned away for consent: say so with the status code, so a
    // client can stop flushing instead of retrying forever.
    let status = if out.rejected > 0 && out.rejected == consent_rejected {
        StatusCode::FORBIDDEN
    } else {
        StatusCode::OK
    };
    Ok((status, Json(out)).into_response())
}

fn json_text<T: Serialize>(value: &T) -> String {
    serde_json::to_string(value).unwrap_or_else(|_| "null".into())
}

#[derive(Debug, Deserialize)]
pub struct ExportParams {
    pub since: Option<String>,
    pub limit: Option<i64>,
}

/// Accepts either an RFC 3339 timestamp or epoch milliseconds, and
/// normalizes to the same fixed-width UTC form `received_at` is stored in, so
/// the comparison can stay a plain string comparison in SQL.
fn parse_since(raw: &str) -> Result<String, ApiError> {
    let raw = raw.trim();
    if let Ok(ms) = raw.parse::<i64>() {
        return DateTime::from_timestamp_millis(ms)
            .map(|dt| dt.to_rfc3339_opts(SecondsFormat::Millis, true))
            .ok_or_else(|| ApiError::BadRequest(format!("since={raw} is out of range")));
    }
    DateTime::parse_from_rfc3339(raw)
        .map(|dt| {
            dt.with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Millis, true)
        })
        .map_err(|_| ApiError::BadRequest(format!("since={raw} is not RFC 3339 or epoch ms")))
}

const EXPORT_SQL: &str = "SELECT event_id, actor_id, session_id, workbook_id, seq, ts_ms, action,
            payload, context, client_version
     FROM events e
     WHERE e.received_at >= ?
       AND EXISTS (
           SELECT 1 FROM consents c
           WHERE c.actor_id = e.actor_id
             AND c.id = (SELECT MAX(id) FROM consents c2 WHERE c2.actor_id = e.actor_id)
             AND c.revoked_at IS NULL
             AND c.mode <> 'off'
       )
     ORDER BY e.actor_id, e.session_id, e.seq
     LIMIT ?";

/// Admin-only JSONL export, one envelope per line. Actors whose current
/// consent is `off` or revoked are excluded here, in the query — the promise
/// in `docs/PRIVACY.md` is enforced by the exporter, not by convention.
pub async fn export(
    State(pool): State<SqlitePool>,
    _admin: AdminUser,
    Query(params): Query<ExportParams>,
) -> Result<Response, ApiError> {
    let since = match params.since.as_deref() {
        Some(raw) => parse_since(raw)?,
        None => String::new(), // sorts before every stored timestamp
    };
    let limit = params.limit.unwrap_or(DEFAULT_EXPORT_LIMIT);
    if limit <= 0 {
        return Err(ApiError::BadRequest("limit must be positive".into()));
    }

    let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, std::io::Error>>(64);
    tokio::spawn(async move {
        let mut rows = sqlx::query(EXPORT_SQL).bind(since).bind(limit).fetch(&pool);
        while let Some(row) = rows.next().await {
            let line = match row.map_err(ApiError::from).and_then(envelope_line) {
                Ok(line) => line,
                Err(e) => {
                    tracing::error!(error = %e, "export row skipped");
                    continue;
                }
            };
            if tx.send(Ok(line)).await.is_err() {
                break; // client hung up
            }
        }
    });

    Ok((
        [(header::CONTENT_TYPE, "application/x-ndjson")],
        Body::from_stream(tokio_stream::wrappers::ReceiverStream::new(rx)),
    )
        .into_response())
}

fn envelope_line(row: sqlx::sqlite::SqliteRow) -> Result<String, ApiError> {
    let payload: serde_json::Value = serde_json::from_str(row.try_get("payload")?)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let context: EventContext = serde_json::from_str(row.try_get("context")?)
        .map_err(|e| ApiError::BadRequest(e.to_string()))?;
    let seq: i64 = row.try_get("seq")?;
    let envelope = EventEnvelope {
        schema_version: SCHEMA_VERSION,
        event_id: row.try_get("event_id")?,
        session_id: row.try_get("session_id")?,
        actor_id: row.try_get("actor_id")?,
        workbook_id: row.try_get("workbook_id")?,
        seq: seq.max(0) as u64,
        ts_ms: row.try_get("ts_ms")?,
        action: row.try_get("action")?,
        payload,
        context,
        client_version: row.try_get("client_version")?,
    };
    let mut line =
        serde_json::to_string(&envelope).map_err(|e| ApiError::BadRequest(e.to_string()))?;
    line.push('\n');
    Ok(line)
}
