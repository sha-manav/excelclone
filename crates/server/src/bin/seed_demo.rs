//! Seed a database with the demonstration scenario.
//!
//! The demo needs three things that no amount of clicking will produce
//! reliably on stage: a workbook with real work already in it, an event log
//! with a habit repeated often enough to mine, and an actor who declined
//! capture so the consent refusal can be *shown* rather than asserted.
//!
//! Everything here goes through the same paths the app does. The workbook is
//! the golden fixture, replayed through `Engine::apply`; every event is built
//! by `engine::telemetry::describe`, the same function the browser calls
//! through wasm; the rows land in the schema the migrations declare. A seeder
//! that fabricated its own payloads would demo a system that does not exist.
//!
//! It also writes the workbook *as it stood when the scripted history
//! finished*, because a log claiming twelve rows were added and a file
//! containing five is a demo that contradicts itself.
//!
//! ```text
//! seed-demo [--db …] [--log demo-events.jsonl] [--workbook demo-ledger.xlsx]
//! ```

use std::io::Write;

use engine::telemetry::{describe, redact_label_text, EventContext, EventEnvelope, PrivacyMode};
use engine::{Action, CellAddr, Engine};
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::SqlitePool;

/// Fixed so the demo is reproducible: the same seed run twice produces the
/// same hashes, and therefore the same dataset.
const SALT: &str = "gridline-demo-salt";
const WORKBOOK_ID: &str = "wb_demo_dues_ledger";
const CLIENT_VERSION: &str = "seed-demo";
/// 2024-01-08T09:00:00Z. A fixed clock keeps the sessionizer's answer stable.
const START_MS: i64 = 1_704_704_400_000;
/// Comfortably past the server's session gap, so each sitting is its own
/// session — which is what the miner counts support in.
const SESSION_GAP_MS: i64 = 6 * 60 * 60 * 1000;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "sqlite://gridline.db?mode=rwc".to_string());
    let mut log_path: Option<String> = None;
    let mut workbook_path: Option<String> = None;
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "--db" => {
                i += 1;
                database_url = args.get(i).cloned().ok_or("--db needs a value")?;
            }
            "--log" => {
                i += 1;
                log_path = Some(args.get(i).cloned().ok_or("--log needs a value")?);
            }
            "--workbook" => {
                i += 1;
                workbook_path = Some(args.get(i).cloned().ok_or("--workbook needs a value")?);
            }
            other => return Err(format!("unknown option {other:?}").into()),
        }
        i += 1;
    }

    let pool = SqlitePoolOptions::new().connect(&database_url).await?;
    server::migrate(&pool).await?;

    // A demo run twice over must not stack two copies of the same history.
    let existing: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM events WHERE workbook_id = ?")
        .bind(WORKBOOK_ID)
        .fetch_one(&pool)
        .await?;
    if existing > 0 {
        clear_demo(&pool).await?;
        eprintln!("cleared {existing} event(s) from a previous seed");
    }

    let willing = server::seed_user(&pool, false).await?;
    let declining = server::seed_user(&pool, false).await?;
    grant(&pool, &willing.id, "structural").await?;
    grant(&pool, &declining.id, "off").await?;

    let (events, worked) = scripted_log(&willing.id);
    for event in &events {
        insert(&pool, event).await?;
    }

    // The actor who said no, with a workbook's worth of work behind them. The
    // export must find none of it — and the only way to show that is for there
    // to be something it could have taken.
    let declined: Vec<EventEnvelope> = scripted_log(&declining.id)
        .0
        .into_iter()
        .map(|mut e| {
            e.event_id = format!("no-{}", e.event_id);
            e.session_id = format!("no-{}", e.session_id);
            e
        })
        .collect();
    for event in &declined {
        insert(&pool, event).await?;
    }

    if let Some(path) = &workbook_path {
        // Exported through the preserved package, so what the presenter opens
        // is the demo fixture with the scripted work in it — charts, styles
        // and all — rather than a fresh file that merely looks similar.
        std::fs::write(path, engine::io::xlsx::export(&worked.wb)?)?;
        eprintln!("wrote the worked workbook to {path}");
    }

    if let Some(path) = &log_path {
        let mut file = std::fs::File::create(path)?;
        for event in &events {
            writeln!(file, "{}", serde_json::to_string(event)?)?;
        }
        eprintln!("wrote {} event(s) to {path}", events.len());
    }

    println!("database:   {database_url}");
    println!("workbook:   {WORKBOOK_ID}");
    println!("actor:      {}  (consent: structural)", willing.id);
    println!("token:      {}", willing.token);
    println!("declined:   {}  (consent: off)", declining.id);
    println!(
        "events:     {} consented, {} withheld across {} session(s) each",
        events.len(),
        declined.len(),
        SITTINGS
    );
    Ok(())
}

async fn clear_demo(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    let actors: Vec<String> =
        sqlx::query_scalar("SELECT DISTINCT actor_id FROM events WHERE workbook_id = ?")
            .bind(WORKBOOK_ID)
            .fetch_all(pool)
            .await?;
    for actor in actors {
        for table in ["events", "consents", "routines"] {
            sqlx::query(&format!("DELETE FROM {table} WHERE actor_id = ?"))
                .bind(&actor)
                .execute(pool)
                .await?;
        }
        sqlx::query("DELETE FROM users WHERE id = ?")
            .bind(&actor)
            .execute(pool)
            .await?;
    }
    Ok(())
}

async fn grant(pool: &SqlitePool, actor: &str, mode: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO consents (actor_id, mode, consent_text_version, granted_at)
         VALUES (?, ?, 'v1', ?)",
    )
    .bind(actor)
    .bind(mode)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

async fn insert(pool: &SqlitePool, e: &EventEnvelope) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO events (event_id, actor_id, session_id, workbook_id, seq, ts_ms,
                             action, payload, context, client_version, received_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&e.event_id)
    .bind(&e.actor_id)
    .bind(&e.session_id)
    .bind(&e.workbook_id)
    .bind(e.seq as i64)
    .bind(e.ts_ms)
    .bind(&e.action)
    .bind(serde_json::to_string(&e.payload).unwrap_or_default())
    .bind(serde_json::to_string(&e.context).unwrap_or_default())
    .bind(&e.client_version)
    .bind(chrono::Utc::now().to_rfc3339())
    .execute(pool)
    .await?;
    Ok(())
}

/// Sittings, and new members entered in each. Three sittings is the miner's
/// minimum support, and four rows a sitting is a loop long enough to be worth
/// automating without being a caricature.
const SITTINGS: usize = 3;
const ROWS_PER_SITTING: usize = 4;

/// New members, in the order they were entered. Deliberately ordinary: the
/// point of the demo is a chore, not a puzzle.
const MEMBERS: [(&str, &str, &str, &str); SITTINGS * ROWS_PER_SITTING] = [
    ("Barbara", "pro", "2024-01-08", "750"),
    ("Donald", "basic", "2024-01-08", "60"),
    ("Frances", "plus", "2024-01-09", "300"),
    ("Tim", "basic", "2024-01-09", "0"),
    ("Margaret", "pro", "2024-01-15", "375"),
    ("Dennis", "plus", "2024-01-15", "300"),
    ("Ken", "basic", "2024-01-16", "120"),
    ("Anita", "pro", "2024-01-16", "750"),
    ("Leslie", "plus", "2024-01-22", "150"),
    ("Adele", "basic", "2024-01-22", "120"),
    ("Jean", "pro", "2024-01-23", "750"),
    ("Radia", "plus", "2024-01-23", "300"),
];

/// The scripted work: someone adding new members to the dues ledger, the same
/// eight keystrokes-worth of gesture every time.
///
/// The workbook is the golden fixture, opened the way the demo opens it, so
/// the formulas the log records are formulas that actually resolve.
fn scripted_log(actor: &str) -> (Vec<EventEnvelope>, Engine) {
    let mut engine = fixture_engine();
    let mut out = Vec::new();
    let mut ts = START_MS;
    let mut row = 7u32; // the fixture's data ends at row 6

    for sitting in 0..SITTINGS {
        let session_id = format!("s_demo_{sitting}");
        let mut seq = 0u64;
        ts += SESSION_GAP_MS;
        for member in MEMBERS
            .iter()
            .skip(sitting * ROWS_PER_SITTING)
            .take(ROWS_PER_SITTING)
        {
            for action in row_actions(*member, row) {
                let selection = primary_cell(&action);
                // Apply first: an event describes something that happened, and
                // a log of actions the engine refused would be fiction.
                if engine.apply(&action).is_err() {
                    continue;
                }
                ts += 4_000; // about how long typing one cell takes
                out.push(envelope(actor, &session_id, seq, ts, &action, &selection));
                seq += 1;
            }
            row += 1;
        }
    }
    (out, engine)
}

fn fixture_engine() -> Engine {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("fixtures/demo-dues-ledger.xlsx");
    match std::fs::read(&path).map(|b| engine::io::xlsx::import(&b)) {
        Ok(Ok(result)) => result.engine,
        _ => {
            // The fixture is generated by `cargo test`, so a clean checkout may
            // not have it yet. An empty ledger still produces a mineable log;
            // it just looks less like a real afternoon.
            eprintln!(
                "warning: {} is missing (run `cargo test -p engine` to generate it); \
                 seeding against an empty workbook",
                path.display()
            );
            let mut e = Engine::new();
            let _ = e.apply(&Action::SheetRename {
                from: "Sheet1".into(),
                to: "Ledger".into(),
            });
            e
        }
    }
}

/// One new member: four typed values, then the six formulas that turn them
/// into a row of the ledger — the same ten cells the rows already in the
/// fixture carry, because that is what continuing someone's sheet means.
fn row_actions((name, tier, joined, paid): (&str, &str, &str, &str), row: u32) -> Vec<Action> {
    [
        ("A", name.to_string()),
        ("B", tier.to_string()),
        ("C", joined.to_string()),
        ("D", paid.to_string()),
        ("E", format!("=VLOOKUP(B{row},Rates!$A$1:$B$3,2,FALSE)")),
        ("F", format!("=E{row}-D{row}")),
        (
            "G",
            format!("=IF(F{row}<=0,\"settled\",IF(F{row}>=E{row},\"unpaid\",\"partial\"))"),
        ),
        ("H", format!("=PROPER(A{row})&\" (\"&UPPER(B{row})&\")\"")),
        ("I", format!("=VALUE(LEFT(C{row},4))")),
        ("J", format!("=TEXT(E{row},\"$#,##0.00\")")),
    ]
    .into_iter()
    .filter_map(|(col, input)| {
        Some(Action::CellEdit {
            sheet: "Ledger".to_string(),
            addr: CellAddr::parse_a1(&format!("{col}{row}"))?,
            input,
        })
    })
    .collect()
}

fn primary_cell(action: &Action) -> String {
    match action {
        Action::CellEdit { addr, .. } => addr.to_a1(),
        _ => "A1".to_string(),
    }
}

fn envelope(
    actor: &str,
    session: &str,
    seq: u64,
    ts_ms: i64,
    action: &Action,
    selection: &str,
) -> EventEnvelope {
    let mode = PrivacyMode::Structural;
    let (name, payload) = describe(action, mode, SALT);
    EventEnvelope {
        schema_version: engine::telemetry::SCHEMA_VERSION,
        event_id: format!("{session}-{seq:04}"),
        session_id: session.to_string(),
        actor_id: actor.to_string(),
        workbook_id: WORKBOOK_ID.to_string(),
        seq,
        ts_ms,
        action: name,
        payload,
        context: EventContext {
            // Redacted through the same function the client uses; a sheet name
            // in clear beside a hashed one would hand over a matched pair.
            sheet: redact_label_text("Ledger", mode, SALT),
            selection: selection.to_string(),
            privacy_mode: mode,
        },
        client_version: CLIENT_VERSION.to_string(),
    }
}
