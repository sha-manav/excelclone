//! The miner CLI.
//!
//! Reads a JSONL event log — the format `GET /v1/events/export` produces —
//! and writes routines. All I/O lives here; the pipeline itself is pure, so
//! it can be tested without a filesystem and reasoned about without one.
//!
//! ```text
//! gridline-miner mine --in events.jsonl [--out routines.json] [--min-support 3]
//! ```

use std::io::{self, Read, Write};

use engine::telemetry::EventEnvelope;
use miner::mine::MineConfig;

fn main() {
    if let Err(e) = run() {
        eprintln!("gridline-miner: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("mine") => mine(&args[1..]),
        Some("--version") | Some("-V") => {
            println!("gridline-miner {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("--help") | Some("-h") | None => {
            println!("{USAGE}");
            Ok(())
        }
        Some(other) => Err(format!("unknown command {other:?}\n\n{USAGE}")),
    }
}

const USAGE: &str = "\
gridline-miner — find repeated work in a captured event log

USAGE:
    gridline-miner mine --in <events.jsonl> [OPTIONS]

OPTIONS:
    --in <path>            JSONL of event envelopes; '-' reads stdin
    --out <path>           where to write routines (default: stdout)
    --db <path>            also upsert them into a Gridline database
    --min-support <n>      sessions a pattern needs (default: 3)
    --max-length <n>       longest pattern to mine (default: 12)
    --gap <n>              tokens allowed between matched items (default: 1)

Routines are mined per (actor, workbook) pair, because a habit belongs to
the person who has it. `--db` upserts on the routine id, which is derived
from the pattern's shape and so is stable across runs: re-mining a growing
log refreshes a proposal rather than stacking a second copy of it, and a
routine the user has already accepted or dismissed keeps that verdict.
";

fn mine(args: &[String]) -> Result<(), String> {
    let mut input: Option<String> = None;
    let mut output: Option<String> = None;
    let mut db: Option<String> = None;
    let mut cfg = MineConfig::default();

    let mut i = 0;
    while i < args.len() {
        let take = |i: &mut usize| -> Result<String, String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| format!("{} needs a value", args[*i - 1]))
        };
        match args[i].as_str() {
            "--in" => input = Some(take(&mut i)?),
            "--out" => output = Some(take(&mut i)?),
            "--db" => db = Some(take(&mut i)?),
            "--min-support" => cfg.min_support = parse_usize(&take(&mut i)?)?,
            "--max-length" => cfg.max_length = parse_usize(&take(&mut i)?)?,
            "--gap" => cfg.gap_tolerance = parse_usize(&take(&mut i)?)?,
            other => return Err(format!("unknown option {other:?}\n\n{USAGE}")),
        }
        i += 1;
    }
    let input = input.ok_or_else(|| format!("--in is required\n\n{USAGE}"))?;

    let text = if input == "-" {
        let mut buf = String::new();
        io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("reading stdin: {e}"))?;
        buf
    } else {
        std::fs::read_to_string(&input).map_err(|e| format!("reading {input}: {e}"))?
    };

    // A malformed line is reported and skipped rather than aborting the run:
    // one bad envelope must not cost a user every routine in the log.
    let mut events: Vec<EventEnvelope> = Vec::new();
    let mut skipped = 0usize;
    for (n, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        match serde_json::from_str::<EventEnvelope>(line) {
            Ok(e) => events.push(e),
            Err(err) => {
                skipped += 1;
                eprintln!("line {}: skipped ({err})", n + 1);
            }
        }
    }
    if skipped > 0 {
        eprintln!("{skipped} line(s) skipped");
    }

    // Mining is per (actor, workbook): a habit belongs to the person who has
    // it, and mixing two people's logs would manufacture support that neither
    // of them earned.
    let mut groups: Vec<(String, String, Vec<EventEnvelope>)> = Vec::new();
    for e in events {
        match groups
            .iter_mut()
            .find(|(a, w, _)| *a == e.actor_id && *w == e.workbook_id)
        {
            Some((_, _, bucket)) => bucket.push(e),
            None => groups.push((e.actor_id.clone(), e.workbook_id.clone(), vec![e])),
        }
    }

    let mut all: Vec<miner::routine::Routine> = Vec::new();
    let mut total_events = 0usize;
    for (actor, workbook, bucket) in &groups {
        total_events += bucket.len();
        let routines = miner::mine_routines(bucket, cfg);
        eprintln!(
            "{actor}/{workbook}: {} event(s), {} routine(s)",
            bucket.len(),
            routines.len()
        );
        if let Some(path) = &db {
            let report = write_to_db(path, actor, workbook, &routines)?;
            eprintln!(
                "  {} new, {} refreshed, {} kept the user's verdict, {} pruned",
                report.0.inserted, report.0.updated, report.0.kept_verdict, report.1
            );
        }
        all.extend(routines);
    }
    eprintln!(
        "{total_events} event(s) across {} workbook(s), {} routine(s) worth proposing",
        groups.len(),
        all.len()
    );

    let json =
        serde_json::to_string_pretty(&all).map_err(|e| format!("serializing routines: {e}"))?;
    match output {
        Some(path) => std::fs::write(&path, format!("{json}\n"))
            .map_err(|e| format!("writing {path}: {e}"))?,
        // Writing to a database and to stdout at once would bury the report
        // under the payload; --db alone stays quiet.
        None if db.is_some() => {}
        None => {
            let mut out = io::stdout();
            writeln!(out, "{json}").map_err(|e| format!("writing stdout: {e}"))?;
        }
    }
    Ok(())
}

type WriteReport = (miner::store::UpsertReport, usize);

/// Upsert one workbook's routines, then drop proposals the log no longer
/// supports. Synchronous from the caller's point of view: the miner is a
/// batch job and has nothing else to do while it waits.
fn write_to_db(
    path: &str,
    actor: &str,
    workbook: &str,
    routines: &[miner::routine::Routine],
) -> Result<WriteReport, String> {
    let now = chrono::Utc::now().to_rfc3339();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("starting a runtime: {e}"))?;
    runtime.block_on(async {
        let pool = sqlx::SqlitePool::connect(&format!("sqlite://{path}"))
            .await
            .map_err(|e| format!("opening {path}: {e}"))?;
        let report = miner::store::upsert(&pool, actor, workbook, routines, &now)
            .await
            .map_err(|e| format!("writing routines: {e}"))?;
        let pruned = miner::store::prune(&pool, actor, workbook, routines)
            .await
            .map_err(|e| format!("pruning routines: {e}"))?;
        Ok((report, pruned))
    })
}

fn parse_usize(s: &str) -> Result<usize, String> {
    s.parse().map_err(|_| format!("{s:?} is not a number"))
}
