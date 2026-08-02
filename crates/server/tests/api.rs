//! End-to-end tests over the real router, in process.
//!
//! Every test builds a fresh in-memory database, so nothing leaks between
//! them. The pool is capped at one connection because each connection to
//! `sqlite::memory:` would otherwise open its own empty database.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;
use tower::ServiceExt;

const CONSENT_VERSION: &str = "2026-01-01";

async fn test_pool() -> SqlitePool {
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect("sqlite::memory:")
        .await
        .expect("in-memory sqlite");
    server::migrate(&pool).await.expect("migrations");
    pool
}

async fn send(pool: &SqlitePool, req: Request<Body>) -> (StatusCode, Value) {
    let (status, text) = send_raw(pool, req).await;
    let body = serde_json::from_str(&text).unwrap_or(Value::Null);
    (status, body)
}

async fn send_raw(pool: &SqlitePool, req: Request<Body>) -> (StatusCode, String) {
    let response = server::app(pool.clone())
        .oneshot(req)
        .await
        .expect("router response");
    let status = response.status();
    let bytes = response
        .into_body()
        .collect()
        .await
        .expect("body")
        .to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

fn get(uri: &str, token: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .expect("request")
}

fn post(uri: &str, token: &str, body: &Value) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri(uri)
        .header("authorization", format!("Bearer {token}"))
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .expect("request")
}

async fn grant(pool: &SqlitePool, token: &str, mode: &str) -> StatusCode {
    send(
        pool,
        post(
            "/v1/consent",
            token,
            &json!({ "mode": mode, "consent_text_version": CONSENT_VERSION }),
        ),
    )
    .await
    .0
}

fn envelope(actor: &str, id: &str, seq: u64, ts_ms: i64) -> Value {
    json!({
        "schema_version": 1,
        "event_id": id,
        "session_id": "sess_client_1",
        "actor_id": actor,
        "workbook_id": "wb_1",
        "seq": seq,
        "ts_ms": ts_ms,
        "action": "cell.edit",
        "payload": { "addr": "A1", "is_formula": false },
        "context": { "sheet": "Sheet1", "selection": "A1", "privacy_mode": "structural" },
        "client_version": "0.1.0"
    })
}

fn batch(events: Vec<Value>) -> Value {
    json!({ "events": events })
}

async fn count_events(pool: &SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM events")
        .fetch_one(pool)
        .await
        .expect("count")
}

#[tokio::test]
async fn health_needs_no_token() {
    let pool = test_pool().await;
    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .expect("request");
    let (status, body) = send(&pool, req).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "ok");
    assert!(body["version"].is_string());
}

#[tokio::test]
async fn unknown_and_missing_tokens_are_unauthorized() {
    let pool = test_pool().await;
    let (status, _) = send(&pool, get("/v1/consent/me", "not-a-real-token")).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let req = Request::builder()
        .uri("/v1/consent/me")
        .body(Body::empty())
        .expect("request");
    let (status, _) = send(&pool, req).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn ingest_is_idempotent() {
    let pool = test_pool().await;
    let user = server::seed_user(&pool, false).await.expect("seed");
    assert_eq!(
        grant(&pool, &user.token, "structural").await,
        StatusCode::OK
    );

    let events = batch(vec![
        envelope(&user.id, "ev_1", 1, 1_000),
        envelope(&user.id, "ev_2", 2, 2_000),
    ]);

    let (status, body) = send(&pool, post("/v1/events", &user.token, &events)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["accepted"], 2);
    assert_eq!(body["duplicates"], 0);
    assert_eq!(body["rejected"], 0);

    // At-least-once delivery: the same batch again must change nothing.
    let (status, body) = send(&pool, post("/v1/events", &user.token, &events)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["accepted"], 0);
    assert_eq!(body["duplicates"], 2);
    assert_eq!(count_events(&pool).await, 2);
}

#[tokio::test]
async fn consent_gates_ingest_end_to_end() {
    let pool = test_pool().await;
    let user = server::seed_user(&pool, false).await.expect("seed");
    let events = batch(vec![envelope(&user.id, "ev_1", 1, 1_000)]);

    // Never consented.
    let (status, body) = send(&pool, post("/v1/events", &user.token, &events)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["rejected"], 1);
    assert!(body["warnings"][0]
        .as_str()
        .is_some_and(|w| w.contains("no consent")));
    assert_eq!(count_events(&pool).await, 0);

    // Explicitly off.
    assert_eq!(grant(&pool, &user.token, "off").await, StatusCode::OK);
    let (status, body) = send(&pool, post("/v1/events", &user.token, &events)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["accepted"], 0);
    assert_eq!(count_events(&pool).await, 0);

    // Granted.
    assert_eq!(
        grant(&pool, &user.token, "structural").await,
        StatusCode::OK
    );
    let (status, body) = send(&pool, post("/v1/events", &user.token, &events)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["accepted"], 1);
    assert_eq!(count_events(&pool).await, 1);

    // Revoked again: new events are refused, and the log already written
    // stays put, because it is append-only.
    assert_eq!(grant(&pool, &user.token, "off").await, StatusCode::OK);
    let later = batch(vec![envelope(&user.id, "ev_2", 2, 2_000)]);
    let (status, _) = send(&pool, post("/v1/events", &user.token, &later)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(count_events(&pool).await, 1);

    let (status, body) = send(&pool, get("/v1/consent/me", &user.token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["mode"], "off");
    assert_eq!(body["captures"], false);
    assert!(body["revoked_at"].is_string());
}

#[tokio::test]
async fn consent_me_is_null_before_any_grant() {
    let pool = test_pool().await;
    let user = server::seed_user(&pool, false).await.expect("seed");
    let (status, body) = send(&pool, get("/v1/consent/me", &user.token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["mode"].is_null());
    assert!(body["granted_at"].is_null());
    assert_eq!(body["captures"], false);
}

#[tokio::test]
async fn unknown_consent_mode_is_rejected() {
    let pool = test_pool().await;
    let user = server::seed_user(&pool, false).await.expect("seed");
    let (status, _) = send(
        &pool,
        post(
            "/v1/consent",
            &user.token,
            &json!({ "mode": "everything", "consent_text_version": CONSENT_VERSION }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn bad_envelopes_are_rejected_without_losing_the_good_ones() {
    let pool = test_pool().await;
    let user = server::seed_user(&pool, false).await.expect("seed");
    let other = server::seed_user(&pool, false).await.expect("seed");
    grant(&pool, &user.token, "structural").await;

    let mut unknown_action = envelope(&user.id, "ev_bad_action", 2, 2_000);
    unknown_action["action"] = json!("cell.telepathy");
    let mut unknown_schema = envelope(&user.id, "ev_bad_schema", 3, 3_000);
    unknown_schema["schema_version"] = json!(99);
    let cross_actor = envelope(&other.id, "ev_cross", 4, 4_000);
    let mut too_much = envelope(&user.id, "ev_full_under_structural", 5, 5_000);
    too_much["context"]["privacy_mode"] = json!("full");

    let events = batch(vec![
        envelope(&user.id, "ev_good", 1, 1_000),
        unknown_action,
        unknown_schema,
        cross_actor,
        too_much,
    ]);
    let (status, body) = send(&pool, post("/v1/events", &user.token, &events)).await;

    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["accepted"], 1);
    assert_eq!(body["rejected"], 4);
    let warnings = body["warnings"].as_array().expect("warnings").clone();
    let joined = warnings
        .iter()
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n");
    assert!(joined.contains("cell.telepathy"), "{joined}");
    assert!(joined.contains("schema_version 99"), "{joined}");
    assert!(
        joined.contains("does not match the authenticated actor"),
        "{joined}"
    );
    assert!(joined.contains("exceeds the consented mode"), "{joined}");

    // Only the good one landed, and nothing was written into the other actor.
    assert_eq!(count_events(&pool).await, 1);
    let stored: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE actor_id = ?")
        .bind(&other.id)
        .fetch_one(&pool)
        .await
        .expect("count");
    assert_eq!(stored, 0);
}

#[tokio::test]
async fn export_requires_an_admin_token() {
    let pool = test_pool().await;
    let user = server::seed_user(&pool, false).await.expect("seed");

    let anonymous = Request::builder()
        .uri("/v1/events/export")
        .body(Body::empty())
        .expect("request");
    let (status, _) = send(&pool, anonymous).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    let (status, _) = send(&pool, get("/v1/events/export", &user.token)).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn export_excludes_revoked_and_off_actors() {
    let pool = test_pool().await;
    let admin = server::seed_user(&pool, true).await.expect("seed");
    let keeper = server::seed_user(&pool, false).await.expect("seed");
    let leaver = server::seed_user(&pool, false).await.expect("seed");

    for (user, id) in [(&keeper, "ev_keep"), (&leaver, "ev_leave")] {
        grant(&pool, &user.token, "structural").await;
        let events = batch(vec![envelope(&user.id, id, 1, 1_000)]);
        let (status, _) = send(&pool, post("/v1/events", &user.token, &events)).await;
        assert_eq!(status, StatusCode::OK);
    }

    let (status, lines) = send_raw(&pool, get("/v1/events/export", &admin.token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(lines.lines().count(), 2);

    // Revoking excludes the actor from every future export, including the
    // events captured while consent was live.
    grant(&pool, &leaver.token, "off").await;
    let (status, lines) = send_raw(&pool, get("/v1/events/export", &admin.token)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(!lines.contains("ev_leave"), "{lines}");
    assert!(lines.contains("ev_keep"), "{lines}");
    assert_eq!(lines.lines().count(), 1);
}

#[tokio::test]
async fn export_streams_ordered_envelopes_and_honours_since_and_limit() {
    let pool = test_pool().await;
    let admin = server::seed_user(&pool, true).await.expect("seed");
    let user = server::seed_user(&pool, false).await.expect("seed");
    grant(&pool, &user.token, "structural").await;

    let mut out_of_order = vec![
        envelope(&user.id, "ev_3", 3, 3_000),
        envelope(&user.id, "ev_1", 1, 1_000),
        envelope(&user.id, "ev_2", 2, 2_000),
    ];
    out_of_order[0]["session_id"] = json!("sess_b");
    let (status, _) = send(&pool, post("/v1/events", &user.token, &batch(out_of_order))).await;
    assert_eq!(status, StatusCode::OK);

    let (status, lines) = send_raw(&pool, get("/v1/events/export", &admin.token)).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<String> = lines
        .lines()
        .map(|line| {
            let v: Value = serde_json::from_str(line).expect("jsonl line");
            assert_eq!(v["schema_version"], 1);
            v["event_id"].as_str().unwrap_or_default().to_string()
        })
        .collect();
    // Ordered by (actor_id, session_id, seq), not by arrival: "sess_b" sorts
    // before "sess_client_1", so the event posted first comes back first only
    // by coincidence of its session, and 1 then 2 follow inside theirs.
    assert_eq!(ids, vec!["ev_3", "ev_1", "ev_2"]);

    let (status, lines) = send_raw(&pool, get("/v1/events/export?limit=1", &admin.token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(lines.lines().count(), 1);

    // A `since` in the future matches nothing; both accepted forms parse.
    let (status, lines) = send_raw(
        &pool,
        get("/v1/events/export?since=2099-01-01T00:00:00Z", &admin.token),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(lines.lines().count(), 0);

    let (status, lines) = send_raw(&pool, get("/v1/events/export?since=0", &admin.token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(lines.lines().count(), 3);

    let (status, _) = send(
        &pool,
        get("/v1/events/export?since=yesterday", &admin.token),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn workbooks_are_scoped_to_their_owner() {
    let pool = test_pool().await;
    let owner = server::seed_user(&pool, false).await.expect("seed");
    let stranger = server::seed_user(&pool, false).await.expect("seed");

    let (status, body) = send(
        &pool,
        post(
            "/v1/workbooks",
            &owner.token,
            &json!({ "name": "Q3 payroll", "state": { "sheets": ["Sheet1"] } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let id = body["id"].as_str().expect("id").to_string();

    // Latest wins.
    let (status, _) = send(
        &pool,
        post(
            "/v1/workbooks",
            &owner.token,
            &json!({ "id": id, "name": "Q4 payroll", "state": { "sheets": ["Sheet1", "Sheet2"] } }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);

    let (status, body) = send(&pool, get("/v1/workbooks", &owner.token)).await;
    assert_eq!(status, StatusCode::OK);
    let list = body.as_array().expect("list");
    assert_eq!(list.len(), 1);
    assert_eq!(list[0]["name"], "Q4 payroll");
    assert!(list[0].get("state").is_none(), "list must not carry state");

    let (status, body) = send(&pool, get(&format!("/v1/workbooks/{id}"), &owner.token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["state"]["sheets"][1], "Sheet2");

    // Another actor learns nothing, not even that the id exists.
    let (status, _) = send(&pool, get(&format!("/v1/workbooks/{id}"), &stranger.token)).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _) = send(
        &pool,
        post(
            "/v1/workbooks",
            &stranger.token,
            &json!({ "id": id, "name": "mine now", "state": {} }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (_, body) = send(&pool, get(&format!("/v1/workbooks/{id}"), &owner.token)).await;
    assert_eq!(body["name"], "Q4 payroll");
}

#[tokio::test]
async fn sessions_split_on_a_gap_longer_than_ten_minutes() {
    let pool = test_pool().await;
    let user = server::seed_user(&pool, false).await.expect("seed");
    grant(&pool, &user.token, "structural").await;

    let start = 1_767_225_600_000i64;
    let nine_minutes = 9 * 60 * 1_000;
    let eleven_minutes = 11 * 60 * 1_000;
    let events = batch(vec![
        envelope(&user.id, "ev_1", 1, start),
        envelope(&user.id, "ev_2", 2, start + nine_minutes),
        envelope(&user.id, "ev_3", 3, start + nine_minutes + eleven_minutes),
    ]);
    let (status, _) = send(&pool, post("/v1/events", &user.token, &events)).await;
    assert_eq!(status, StatusCode::OK);

    let sessions = server::sessions_for(&pool, &user.id)
        .await
        .expect("sessions");
    assert_eq!(sessions.len(), 2, "{sessions:?}");
    assert_eq!(sessions[0].event_count, 2);
    assert_eq!(sessions[0].duration_ms, nine_minutes);
    assert_eq!(sessions[1].event_count, 1);
    // The client called all three one session; the server disagrees.
    assert_eq!(sessions[0].client_session_ids, vec!["sess_client_1"]);
    assert_eq!(sessions[1].client_session_ids, vec!["sess_client_1"]);

    let (status, body) = send(&pool, get("/v1/sessions", &user.token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().map(Vec::len), Some(2));
    assert_eq!(body[0]["started_ms"], start);

    // An actor only ever sees their own sessions.
    let stranger = server::seed_user(&pool, false).await.expect("seed");
    let (status, body) = send(&pool, get("/v1/sessions", &stranger.token)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body.as_array().map(Vec::len), Some(0));
}

async fn seed_routine(
    pool: &SqlitePool,
    id: &str,
    actor_id: &str,
    workbook_id: &str,
    minutes: f64,
) {
    sqlx::query(
        "INSERT INTO routines
            (id, workbook_id, actor_id, summary, body, estimated_minutes_saved, support, status, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, 'proposed', '2026-01-01T00:00:00.000Z')",
    )
    .bind(id)
    .bind(workbook_id)
    .bind(actor_id)
    .bind(format!("routine {id}"))
    .bind(r#"{"steps":[]}"#)
    .bind(minutes)
    .bind(4i64)
    .execute(pool)
    .await
    .expect("seed routine");
}

#[tokio::test]
async fn routines_list_by_value_and_accept_feedback() {
    let pool = test_pool().await;
    let user = server::seed_user(&pool, false).await.expect("seed");
    let stranger = server::seed_user(&pool, false).await.expect("seed");

    seed_routine(&pool, "r_small", &user.id, "wb_1", 2.5).await;
    seed_routine(&pool, "r_big", &user.id, "wb_1", 17.0).await;
    seed_routine(&pool, "r_other_wb", &user.id, "wb_2", 99.0).await;
    seed_routine(&pool, "r_stranger", &stranger.id, "wb_1", 50.0).await;

    let (status, body) = send(&pool, get("/v1/routines?workbook=wb_1", &user.token)).await;
    assert_eq!(status, StatusCode::OK);
    let ids: Vec<&str> = body
        .as_array()
        .expect("list")
        .iter()
        .filter_map(|r| r["id"].as_str())
        .collect();
    assert_eq!(ids, vec!["r_big", "r_small"]);
    assert_eq!(body[0]["status"], "proposed");
    assert!(body[0]["body"].is_object());

    // The `workbook` filter is required.
    let (status, _) = send(&pool, get("/v1/routines", &user.token)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    let (status, body) = send(
        &pool,
        post(
            "/v1/routines/r_big/feedback",
            &user.token,
            &json!({ "status": "accepted" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["status"], "accepted");

    let (_, body) = send(&pool, get("/v1/routines?workbook=wb_1", &user.token)).await;
    assert_eq!(body[0]["status"], "accepted");

    let (status, _) = send(
        &pool,
        post(
            "/v1/routines/r_big/feedback",
            &user.token,
            &json!({ "status": "maybe" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);

    // Someone else's routine does not exist as far as this actor is concerned.
    let (status, _) = send(
        &pool,
        post(
            "/v1/routines/r_stranger/feedback",
            &user.token,
            &json!({ "status": "dismissed" }),
        ),
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn malformed_bodies_are_client_errors_not_panics() {
    let pool = test_pool().await;
    let user = server::seed_user(&pool, false).await.expect("seed");
    grant(&pool, &user.token, "structural").await;

    for body in [
        "not json at all",
        r#"{"events": "not an array"}"#,
        r#"{"events": [{"schema_version": 1}]}"#,
    ] {
        let req = Request::builder()
            .method("POST")
            .uri("/v1/events")
            .header("authorization", format!("Bearer {}", user.token))
            .header("content-type", "application/json")
            .body(Body::from(body))
            .expect("request");
        let (status, _) = send(&pool, req).await;
        assert!(status.is_client_error(), "{body} -> {status}");
    }
}

#[tokio::test]
async fn an_empty_batch_is_accepted_not_forbidden() {
    let pool = test_pool().await;
    let user = server::seed_user(&pool, false).await.expect("seed");
    let (status, body) = send(&pool, post("/v1/events", &user.token, &batch(vec![]))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["accepted"], 0);
    assert_eq!(body["rejected"], 0);
}
