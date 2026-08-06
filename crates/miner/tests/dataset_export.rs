//! The `export` subcommand, run as a subprocess against a real database.
//!
//! The unit tests in `store` prove the consent query is right and the ones in
//! `dataset` prove the records are right. Neither proves that the binary an
//! operator actually types wires the two together — and the whole promise of
//! `--consented-only` lives in that wiring. So this drives the built binary.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use sqlx::SqlitePool;

/// A directory of our own under the system temp dir, named uniquely so
/// concurrent test binaries cannot collide.
fn scratch(label: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("gridline-{label}-{}", ulid::Ulid::new()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

async fn database(dir: &Path) -> (PathBuf, SqlitePool) {
    let path = dir.join("gridline.db");
    let pool = SqlitePool::connect(&format!("sqlite://{}?mode=rwc", path.display()))
        .await
        .unwrap();
    let migration = include_str!("../../server/migrations/0001_init.sql");
    sqlx::raw_sql(migration).execute(&pool).await.unwrap();
    (path, pool)
}

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

/// One `cell.edit`, recorded the way the client records it under `structural`
/// capture: the value reaches us as a hash, never as itself.
async fn edit(pool: &SqlitePool, actor: &str, session: &str, seq: i64, addr: &str) {
    let payload = serde_json::json!({
        "addr": addr,
        "input": { "hash": format!("{seq:08x}"), "type": "number", "len": 3 },
        "is_formula": false,
    });
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
    .bind(payload.to_string())
    .bind(r#"{"sheet":"h-sheet","selection":"A1","privacy_mode":"structural"}"#)
    .execute(pool)
    .await
    .unwrap();
}

fn miner(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_gridline-miner"))
        .args(args)
        .output()
        .expect("running the miner")
}

/// A database with three actors: one who agreed, one who declined, and one
/// who agreed and then revoked.
async fn seeded(dir: &Path) -> PathBuf {
    let (path, pool) = database(dir).await;
    grant(&pool, "yes", "structural").await;
    grant(&pool, "no", "off").await;
    grant(&pool, "gone", "structural").await;
    sqlx::query("UPDATE consents SET revoked_at = 'later' WHERE actor_id = 'gone'")
        .execute(&pool)
        .await
        .unwrap();

    for (actor, session) in [("yes", "s-yes"), ("no", "s-no"), ("gone", "s-gone")] {
        edit(&pool, actor, session, 0, "A1").await;
        edit(&pool, actor, session, 1, "A2").await;
    }
    pool.close().await;
    path
}

fn read_all(dir: &Path) -> String {
    let mut text = String::new();
    for entry in std::fs::read_dir(dir).unwrap().flatten() {
        text.push_str(&std::fs::read_to_string(entry.path()).unwrap_or_default());
    }
    text
}

#[tokio::test]
async fn an_export_carries_only_the_actor_who_still_consents() {
    let dir = scratch("export");
    let db = seeded(&dir).await;
    let out = dir.join("data");

    let result = miner(&[
        "export",
        "--consented-only",
        "--mode",
        "structural",
        "--db",
        db.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let text = read_all(&out);
    assert!(text.contains("s-yes"), "the consenting actor is missing");
    assert!(!text.contains("s-no"), "an actor who declined was exported");
    assert!(
        !text.contains("s-gone"),
        "an actor who revoked was exported"
    );

    // One file per session, and the session that survived is the only one.
    let files: Vec<String> = std::fs::read_dir(&out)
        .unwrap()
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".jsonl"))
        .collect();
    assert_eq!(files, vec!["0000-s-yes.jsonl".to_string()]);
}

#[tokio::test]
async fn every_record_has_the_shape_the_dataset_promises() {
    let dir = scratch("shape");
    let db = seeded(&dir).await;
    let out = dir.join("data");
    let result = miner(&[
        "export",
        "--consented-only",
        "--db",
        db.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(result.status.success());

    let text = std::fs::read_to_string(out.join("0000-s-yes.jsonl")).unwrap();
    let lines: Vec<serde_json::Value> = text
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(lines.len(), 2);
    for line in &lines {
        for key in ["pre_state_digest", "context", "action", "post_state_digest"] {
            assert!(line.get(key).is_some(), "missing {key} in {line}");
        }
        // A digest that equalled its predecessor would mean the action did
        // nothing, and a record of nothing is not a demonstration.
        assert_ne!(line["pre_state_digest"], line["post_state_digest"]);
        assert_eq!(line["context"]["values_synthetic"], true);
    }
    // The chain: this action started where the previous one finished.
    assert_eq!(lines[0]["post_state_digest"], lines[1]["pre_state_digest"]);

    // The hashes the log carried must not travel with the dataset.
    assert!(!text.contains("\"hash\""), "{text}");

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(out.join("manifest.json")).unwrap()).unwrap();
    assert_eq!(manifest["consent_checked"], true);
    assert_eq!(manifest["records"], 2);
    assert_eq!(manifest["sessions"][0]["file"], "0000-s-yes.jsonl");
}

#[tokio::test]
async fn a_database_export_without_the_flag_is_refused() {
    // The flag is the whole guarantee. If leaving it off quietly exported
    // everyone, it would be decoration.
    let dir = scratch("noflag");
    let db = seeded(&dir).await;
    let out = dir.join("data");
    let result = miner(&[
        "export",
        "--db",
        db.to_str().unwrap(),
        "--out",
        out.to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    let err = String::from_utf8_lossy(&result.stderr);
    assert!(err.contains("--consented-only"), "{err}");
    assert!(!out.exists(), "a refused export still created its output");
}

#[tokio::test]
async fn the_flag_is_refused_where_it_cannot_be_honoured() {
    // A JSONL log has no consent records in it. Accepting the flag would let
    // a script claim a check that never ran.
    let dir = scratch("fileflag");
    let log = dir.join("events.jsonl");
    std::fs::write(&log, "").unwrap();
    let result = miner(&[
        "export",
        "--consented-only",
        "--in",
        log.to_str().unwrap(),
        "--out",
        dir.join("data").to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    let err = String::from_utf8_lossy(&result.stderr);
    assert!(err.contains("carries no consent records"), "{err}");
}

#[tokio::test]
async fn a_mode_nobody_can_consent_to_is_rejected_rather_than_silently_empty() {
    let dir = scratch("modeoff");
    let db = seeded(&dir).await;
    let result = miner(&[
        "export",
        "--consented-only",
        "--mode",
        "off",
        "--db",
        db.to_str().unwrap(),
        "--out",
        dir.join("data").to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    assert!(String::from_utf8_lossy(&result.stderr).contains("nothing to export"));
}

#[tokio::test]
async fn exporting_the_same_database_twice_produces_the_same_bytes() {
    // The dataset is a build artefact; a non-reproducible one cannot be
    // reviewed by diffing it against the last one.
    let dir = scratch("repeat");
    let db = seeded(&dir).await;
    let run = |out: PathBuf| {
        let r = miner(&[
            "export",
            "--consented-only",
            "--db",
            db.to_str().unwrap(),
            "--out",
            out.to_str().unwrap(),
        ]);
        assert!(r.status.success());
        read_all(&out)
    };
    assert_eq!(run(dir.join("a")), run(dir.join("b")));
}
