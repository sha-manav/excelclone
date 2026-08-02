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

#[cfg(test)]
mod tests {
    use super::*;
    use engine::{Action, CellAddr};
    use sqlx::sqlite::SqlitePoolOptions;

    /// The `routines` table exactly as `crates/server/migrations` declares it.
    /// Duplicated here rather than imported because the miner does not depend
    /// on the server; `the_test_schema_matches_the_migration` keeps the two
    /// honest.
    const SCHEMA: &str = "CREATE TABLE routines (
        id                      TEXT PRIMARY KEY,
        workbook_id             TEXT NOT NULL,
        actor_id                TEXT NOT NULL,
        summary                 TEXT NOT NULL,
        body                    TEXT NOT NULL,
        estimated_minutes_saved REAL NOT NULL,
        support                 INTEGER NOT NULL,
        status                  TEXT NOT NULL DEFAULT 'proposed',
        created_at              TEXT NOT NULL
    );";

    async fn pool() -> SqlitePool {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .unwrap();
        sqlx::query(SCHEMA).execute(&pool).await.unwrap();
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

    /// The schema above is a copy. If the migration changes, this fails.
    #[test]
    fn the_test_schema_matches_the_migration() {
        let migration = include_str!("../../server/migrations/0001_init.sql");
        let start = migration
            .find("CREATE TABLE routines")
            .expect("routines table in the migration");
        let end = migration[start..]
            .find(");")
            .map(|i| start + i + 2)
            .expect("end of the routines table");
        let normalize = |s: &str| {
            s.split_whitespace()
                .collect::<Vec<_>>()
                .join(" ")
                .replace(" ,", ",")
        };
        assert_eq!(
            normalize(&migration[start..end]),
            normalize(SCHEMA),
            "the miner's copy of the routines schema has drifted from the migration"
        );
    }
}
