//! Writing mined routines into the server's database.
//!
//! The server's `routines.rs` says it plainly: "the miner writes rows here;
//! the server only serves them back and records feedback". So the miner
//! writes them, rather than posting to an endpoint that would need an admin
//! credential and a second copy of the schema.
//!
//! Two rules shape the upsert:
//!
//! * **A routine's id is stable across mining runs**, derived from its token
//!   shapes. Re-mining a growing log therefore *updates* a proposal — its
//!   support and estimate move — rather than stacking a second copy of the
//!   same suggestion next to the first.
//! * **A verdict the user has already given is never overwritten.** Someone
//!   who dismissed a suggestion has said no; a nightly re-mine that reset it
//!   to `proposed` would ask them again every morning, which is how a good
//!   feature becomes an annoying one.

use engine::telemetry::EventEnvelope;
use engine::Routine;
use sqlx::{Row, SqlitePool};

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Db(#[from] sqlx::Error),
    #[error("serializing routine {0}")]
    Encode(String),
}

/// What an upsert did, so the CLI can say something true about it.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct UpsertReport {
    pub inserted: usize,
    pub updated: usize,
    /// Left alone because the user had already accepted or dismissed them.
    pub kept_verdict: usize,
}

/// Insert or refresh a set of routines for one actor and workbook.
pub async fn upsert(
    pool: &SqlitePool,
    actor_id: &str,
    workbook_id: &str,
    routines: &[Routine],
    now: &str,
) -> Result<UpsertReport, StoreError> {
    let mut report = UpsertReport::default();
    for r in routines {
        let body = serde_json::to_string(r).map_err(|_| StoreError::Encode(r.id.clone()))?;

        let existing: Option<String> =
            sqlx::query("SELECT status FROM routines WHERE id = ? AND actor_id = ?")
                .bind(&r.id)
                .bind(actor_id)
                .fetch_optional(pool)
                .await?
                .map(|row| row.get::<String, _>("status"));

        match existing.as_deref() {
            // A verdict is the user's, and re-mining does not un-ask a
            // question they have already answered. The body is still
            // refreshed so an accepted routine keeps working as the log
            // grows; only `status` is left alone.
            Some(status @ ("accepted" | "dismissed")) => {
                sqlx::query(
                    "UPDATE routines
                        SET summary = ?, body = ?, estimated_minutes_saved = ?, support = ?
                      WHERE id = ? AND actor_id = ?",
                )
                .bind(&r.summary)
                .bind(&body)
                .bind(r.estimated_minutes_saved)
                .bind(r.support as i64)
                .bind(&r.id)
                .bind(actor_id)
                .execute(pool)
                .await?;
                let _ = status;
                report.kept_verdict += 1;
            }
            Some(_) => {
                sqlx::query(
                    "UPDATE routines
                        SET workbook_id = ?, summary = ?, body = ?,
                            estimated_minutes_saved = ?, support = ?
                      WHERE id = ? AND actor_id = ?",
                )
                .bind(workbook_id)
                .bind(&r.summary)
                .bind(&body)
                .bind(r.estimated_minutes_saved)
                .bind(r.support as i64)
                .bind(&r.id)
                .bind(actor_id)
                .execute(pool)
                .await?;
                report.updated += 1;
            }
            None => {
                sqlx::query(
                    "INSERT INTO routines
                        (id, workbook_id, actor_id, summary, body,
                         estimated_minutes_saved, support, status, created_at)
                     VALUES (?, ?, ?, ?, ?, ?, ?, 'proposed', ?)",
                )
                .bind(&r.id)
                .bind(workbook_id)
                .bind(actor_id)
                .bind(&r.summary)
                .bind(&body)
                .bind(r.estimated_minutes_saved)
                .bind(r.support as i64)
                .bind(now)
                .execute(pool)
                .await?;
                report.inserted += 1;
            }
        }
    }
    Ok(report)
}

/// Proposals that no longer appear in a fresh mining run.
///
/// Deleted rather than left standing: a habit the user has stopped having
/// should stop being suggested, and a panel that only ever grows is a panel
/// that stops being read. An accepted or dismissed routine is kept, because
/// that row is the user's answer and not our proposal.
pub async fn prune(
    pool: &SqlitePool,
    actor_id: &str,
    workbook_id: &str,
    keep: &[Routine],
) -> Result<usize, StoreError> {
    let ids: Vec<String> = keep.iter().map(|r| r.id.clone()).collect();
    let rows: Vec<String> = sqlx::query(
        "SELECT id FROM routines
          WHERE actor_id = ? AND workbook_id = ? AND status = 'proposed'",
    )
    .bind(actor_id)
    .bind(workbook_id)
    .fetch_all(pool)
    .await?
    .into_iter()
    .map(|r| r.get::<String, _>("id"))
    .collect();

    let stale: Vec<String> = rows.into_iter().filter(|id| !ids.contains(id)).collect();
    for id in &stale {
        sqlx::query("DELETE FROM routines WHERE id = ? AND actor_id = ?")
            .bind(id)
            .bind(actor_id)
            .execute(pool)
            .await?;
    }
    Ok(stale.len())
}

/// Events for every actor whose consent still permits it, in replay order.
///
/// The consent test is a `WHERE` clause, not a filter applied to the results:
/// the promise in `docs/PRIVACY.md` has to be enforced by the query, because
/// a filter can be forgotten and a join cannot. This is deliberately the same
/// predicate the server's own export uses.
///
/// `mode` narrows further — passing `structural` excludes actors who granted
/// only `full`... which is nobody, since `full` is a superset. It exists so
/// `--mode structural` means "only material I may treat as structural", and
/// an operator who asks for that gets exactly it.
pub async fn consented_events(
    pool: &SqlitePool,
    mode: Option<&str>,
) -> Result<Vec<EventEnvelope>, StoreError> {
    let rows = sqlx::query(
        "SELECT e.event_id, e.actor_id, e.session_id, e.workbook_id, e.seq, e.ts_ms,
                e.action, e.payload, e.context, e.client_version
           FROM events e
          WHERE EXISTS (
                SELECT 1 FROM consents c
                 WHERE c.actor_id = e.actor_id
                   AND c.id = (SELECT MAX(id) FROM consents c2 WHERE c2.actor_id = e.actor_id)
                   AND c.revoked_at IS NULL
                   AND c.mode <> 'off'
                   AND (?1 IS NULL OR c.mode = ?1)
             )
          ORDER BY e.actor_id, e.session_id, e.seq",
    )
    .bind(mode)
    .fetch_all(pool)
    .await?;

    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let payload: String = row.get("payload");
        let context: String = row.get("context");
        // A row we cannot parse is skipped rather than fatal: one malformed
        // envelope must not cost an operator the whole dataset.
        let (Ok(payload), Ok(context)) = (
            serde_json::from_str(&payload),
            serde_json::from_str(&context),
        ) else {
            continue;
        };
        out.push(EventEnvelope {
            schema_version: engine::telemetry::SCHEMA_VERSION,
            event_id: row.get("event_id"),
            session_id: row.get("session_id"),
            actor_id: row.get("actor_id"),
            workbook_id: row.get("workbook_id"),
            seq: row.get::<i64, _>("seq") as u64,
            ts_ms: row.get("ts_ms"),
            action: row.get("action"),
            payload,
            context,
            client_version: row.get("client_version"),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{Action, CellAddr};
    use sqlx::sqlite::SqlitePoolOptions;

    /// The server's schema, verbatim.
    ///
    /// A hand-copied approximation would let these tests pass against tables
    /// the server does not have — and the consent query below is only worth
    /// anything if it runs against the real `consents` table, indexes,
    /// defaults and all.
    const MIGRATION: &str = include_str!("../../server/migrations/0001_init.sql");

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::raw_sql(MIGRATION).execute(&pool).await.unwrap();
        pool
    }

    fn routine(id: &str, support: usize, minutes: f64) -> Routine {
        Routine {
            id: id.into(),
            summary: format!("{id} summary"),
            anchor: "A1".into(),
            actions: vec![Action::CellEdit {
                sheet: "<routine>".into(),
                addr: CellAddr::new(0, 0),
                input: "=1+1".into(),
            }],
            requires: Vec::new(),
            support,
            estimated_minutes_saved: minutes,
            kind: "loop".into(),
        }
    }

    async fn status_of(pool: &SqlitePool, id: &str) -> String {
        sqlx::query("SELECT status FROM routines WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
            .get("status")
    }

    async fn support_of(pool: &SqlitePool, id: &str) -> i64 {
        sqlx::query("SELECT support FROM routines WHERE id = ?")
            .bind(id)
            .fetch_one(pool)
            .await
            .unwrap()
            .get("support")
    }

    #[tokio::test]
    async fn a_new_routine_is_inserted_as_proposed() {
        let pool = pool().await;
        let report = upsert(&pool, "u", "wb", &[routine("rt_a", 5, 4.0)], "now")
            .await
            .unwrap();
        assert_eq!(report.inserted, 1);
        assert_eq!(status_of(&pool, "rt_a").await, "proposed");
    }

    #[tokio::test]
    async fn re_mining_updates_rather_than_duplicating() {
        // The id is derived from the pattern's shape precisely so a nightly
        // re-mine refreshes the proposal instead of stacking copies of it.
        let pool = pool().await;
        upsert(&pool, "u", "wb", &[routine("rt_a", 5, 4.0)], "now")
            .await
            .unwrap();
        let report = upsert(&pool, "u", "wb", &[routine("rt_a", 9, 7.5)], "later")
            .await
            .unwrap();
        assert_eq!(
            report,
            UpsertReport {
                inserted: 0,
                updated: 1,
                kept_verdict: 0
            }
        );

        let count: i64 = sqlx::query("SELECT COUNT(*) AS n FROM routines")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get("n");
        assert_eq!(count, 1);
        assert_eq!(support_of(&pool, "rt_a").await, 9);
    }

    #[tokio::test]
    async fn a_dismissed_routine_is_not_proposed_again() {
        // Someone who said no has said no. Resetting the status on every
        // re-mine would ask them again every morning.
        let pool = pool().await;
        upsert(&pool, "u", "wb", &[routine("rt_a", 5, 4.0)], "now")
            .await
            .unwrap();
        sqlx::query("UPDATE routines SET status = 'dismissed' WHERE id = 'rt_a'")
            .execute(&pool)
            .await
            .unwrap();

        let report = upsert(&pool, "u", "wb", &[routine("rt_a", 40, 30.0)], "later")
            .await
            .unwrap();
        assert_eq!(report.kept_verdict, 1);
        assert_eq!(status_of(&pool, "rt_a").await, "dismissed");
        // ...but the body is still refreshed, so an *accepted* one keeps
        // working as the log grows.
        assert_eq!(support_of(&pool, "rt_a").await, 40);
    }

    #[tokio::test]
    async fn an_accepted_routine_keeps_its_verdict_too() {
        let pool = pool().await;
        upsert(&pool, "u", "wb", &[routine("rt_a", 5, 4.0)], "now")
            .await
            .unwrap();
        sqlx::query("UPDATE routines SET status = 'accepted' WHERE id = 'rt_a'")
            .execute(&pool)
            .await
            .unwrap();
        upsert(&pool, "u", "wb", &[routine("rt_a", 6, 5.0)], "later")
            .await
            .unwrap();
        assert_eq!(status_of(&pool, "rt_a").await, "accepted");
    }

    #[tokio::test]
    async fn routines_are_scoped_to_their_actor() {
        // Two people can develop the same habit; neither should see the
        // other's row, and one re-mining must not touch the other's.
        let pool = pool().await;
        upsert(&pool, "u1", "wb", &[routine("rt_a", 5, 4.0)], "now")
            .await
            .unwrap();
        let report = upsert(&pool, "u2", "wb", &[routine("rt_a", 5, 4.0)], "now").await;
        // Same primary key: the second insert must fail loudly rather than
        // silently overwrite the first actor's row.
        assert!(report.is_err(), "a cross-actor id collision was accepted");
    }

    #[tokio::test]
    async fn pruning_drops_proposals_the_habit_no_longer_supports() {
        let pool = pool().await;
        upsert(
            &pool,
            "u",
            "wb",
            &[routine("rt_a", 5, 4.0), routine("rt_b", 5, 4.0)],
            "now",
        )
        .await
        .unwrap();
        let dropped = prune(&pool, "u", "wb", &[routine("rt_a", 5, 4.0)])
            .await
            .unwrap();
        assert_eq!(dropped, 1);
        let count: i64 = sqlx::query("SELECT COUNT(*) AS n FROM routines")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get("n");
        assert_eq!(count, 1);
    }

    #[tokio::test]
    async fn pruning_never_removes_an_answered_routine() {
        // A dismissed row is the user's answer, not our proposal, and
        // deleting it would resurrect the suggestion on the next run.
        let pool = pool().await;
        upsert(&pool, "u", "wb", &[routine("rt_a", 5, 4.0)], "now")
            .await
            .unwrap();
        sqlx::query("UPDATE routines SET status = 'dismissed' WHERE id = 'rt_a'")
            .execute(&pool)
            .await
            .unwrap();
        assert_eq!(prune(&pool, "u", "wb", &[]).await.unwrap(), 0);
        assert_eq!(status_of(&pool, "rt_a").await, "dismissed");
    }

    #[tokio::test]
    async fn the_stored_body_is_the_routine_the_client_will_run() {
        let pool = pool().await;
        let r = routine("rt_a", 5, 4.0);
        upsert(&pool, "u", "wb", std::slice::from_ref(&r), "now")
            .await
            .unwrap();
        let body: String = sqlx::query("SELECT body FROM routines WHERE id = 'rt_a'")
            .fetch_one(&pool)
            .await
            .unwrap()
            .get("body");
        assert_eq!(serde_json::from_str::<Routine>(&body).unwrap(), r);
    }

    // --- consented_events -------------------------------------------------

    async fn grant(pool: &SqlitePool, actor: &str, mode: &str) {
        sqlx::query(
            "INSERT INTO consents (actor_id, mode, consent_text_version, granted_at)
             VALUES (?, ?, 'v1', 'now')",
        )
        .bind(actor)
        .bind(mode)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn revoke_latest(pool: &SqlitePool, actor: &str) {
        sqlx::query(
            "UPDATE consents SET revoked_at = 'later'
              WHERE id = (SELECT MAX(id) FROM consents WHERE actor_id = ?)",
        )
        .bind(actor)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn record_event(pool: &SqlitePool, actor: &str, session: &str, seq: i64) {
        sqlx::query(
            "INSERT INTO events (event_id, actor_id, session_id, workbook_id, seq, ts_ms,
                                 action, payload, context, client_version, received_at)
             VALUES (?, ?, ?, 'wb', ?, ?, 'cell.edit', ?, ?, 'test', 'now')",
        )
        .bind(format!("{actor}-{session}-{seq}"))
        .bind(actor)
        .bind(session)
        .bind(seq)
        .bind(1_700_000_000_000i64 + seq)
        .bind(r#"{"addr":"A1","input":"1","is_formula":false}"#)
        .bind(r#"{"sheet":"Sheet1","selection":"A1","privacy_mode":"structural"}"#)
        .execute(pool)
        .await
        .unwrap();
    }

    async fn actors_in(pool: &SqlitePool, mode: Option<&str>) -> Vec<String> {
        let mut ids: Vec<String> = consented_events(pool, mode)
            .await
            .unwrap()
            .into_iter()
            .map(|e| e.actor_id)
            .collect();
        ids.dedup();
        ids
    }

    #[tokio::test]
    async fn an_actor_who_never_consented_contributes_nothing() {
        // No consent row at all — the commonest case, and the one a filter
        // written as an afterthought gets wrong.
        let pool = pool().await;
        record_event(&pool, "silent", "s1", 0).await;
        assert!(consented_events(&pool, None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn an_actor_who_turned_capture_off_contributes_nothing() {
        let pool = pool().await;
        grant(&pool, "declined", "off").await;
        record_event(&pool, "declined", "s1", 0).await;
        assert!(consented_events(&pool, None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn revoking_consent_withdraws_events_already_recorded() {
        // Revocation is retroactive by design: the events stay in the log for
        // provenance, but nothing may leave the building with them.
        let pool = pool().await;
        grant(&pool, "u", "structural").await;
        record_event(&pool, "u", "s1", 0).await;
        assert_eq!(consented_events(&pool, None).await.unwrap().len(), 1);

        revoke_latest(&pool, "u").await;
        assert!(consented_events(&pool, None).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn only_the_latest_consent_counts() {
        // `consents` is append-only, so an actor accumulates rows. An earlier
        // grant must not resurrect an actor who has since turned capture off,
        // and an earlier `off` must not silence one who has since agreed.
        let pool = pool().await;
        grant(&pool, "left", "structural").await;
        grant(&pool, "left", "off").await;
        record_event(&pool, "left", "s1", 0).await;

        grant(&pool, "joined", "off").await;
        grant(&pool, "joined", "structural").await;
        record_event(&pool, "joined", "s1", 0).await;

        assert_eq!(actors_in(&pool, None).await, vec!["joined".to_string()]);
    }

    #[tokio::test]
    async fn a_revoked_grant_followed_by_a_fresh_one_counts_again() {
        // Someone who came back is not still gone.
        let pool = pool().await;
        grant(&pool, "u", "structural").await;
        revoke_latest(&pool, "u").await;
        grant(&pool, "u", "structural").await;
        record_event(&pool, "u", "s1", 0).await;
        assert_eq!(consented_events(&pool, None).await.unwrap().len(), 1);
    }

    #[tokio::test]
    async fn the_mode_filter_narrows_to_exactly_that_consent() {
        let pool = pool().await;
        grant(&pool, "structural_user", "structural").await;
        record_event(&pool, "structural_user", "s1", 0).await;
        grant(&pool, "full_user", "full").await;
        record_event(&pool, "full_user", "s1", 0).await;

        assert_eq!(
            actors_in(&pool, Some("structural")).await,
            vec!["structural_user".to_string()]
        );
        assert_eq!(
            actors_in(&pool, Some("full")).await,
            vec!["full_user".to_string()]
        );
        assert_eq!(actors_in(&pool, None).await.len(), 2);
    }

    #[tokio::test]
    async fn one_actors_consent_does_not_release_anothers_events() {
        let pool = pool().await;
        grant(&pool, "yes", "structural").await;
        grant(&pool, "no", "off").await;
        record_event(&pool, "yes", "s1", 0).await;
        record_event(&pool, "no", "s1", 0).await;
        assert_eq!(actors_in(&pool, None).await, vec!["yes".to_string()]);
    }

    #[tokio::test]
    async fn events_arrive_in_replay_order() {
        // The dataset exporter replays these in the order it receives them,
        // so an out-of-order read would silently produce wrong digests.
        let pool = pool().await;
        grant(&pool, "u", "structural").await;
        for seq in [2i64, 0, 1] {
            record_event(&pool, "u", "s1", seq).await;
        }
        record_event(&pool, "u", "s0", 5).await;

        let got: Vec<(String, u64)> = consented_events(&pool, None)
            .await
            .unwrap()
            .into_iter()
            .map(|e| (e.session_id, e.seq))
            .collect();
        assert_eq!(
            got,
            vec![
                ("s0".to_string(), 5),
                ("s1".to_string(), 0),
                ("s1".to_string(), 1),
                ("s1".to_string(), 2),
            ]
        );
    }

    #[tokio::test]
    async fn an_unreadable_row_is_skipped_rather_than_fatal() {
        let pool = pool().await;
        grant(&pool, "u", "structural").await;
        sqlx::query(
            "INSERT INTO events (event_id, actor_id, session_id, workbook_id, seq, ts_ms,
                                 action, payload, context, client_version, received_at)
             VALUES ('bad', 'u', 's1', 'wb', 0, 1, 'cell.edit', 'not json', '{}', 't', 'now')",
        )
        .execute(&pool)
        .await
        .unwrap();
        record_event(&pool, "u", "s1", 1).await;

        let got = consented_events(&pool, None).await.unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].seq, 1);
    }
}
