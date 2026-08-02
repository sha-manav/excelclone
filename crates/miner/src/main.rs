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
        Some("export") => export(&args[1..]),
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
    gridline-miner mine   --in <events.jsonl> [OPTIONS]
    gridline-miner export --consented-only --out <dir> [OPTIONS]

mine — find repeated work
    --in <path>            JSONL of event envelopes; '-' reads stdin
    --out <path>           where to write routines (default: stdout)
    --db <path>            also upsert them into a Gridline database
    --min-support <n>      sessions a pattern needs (default: 3)
    --max-length <n>       longest pattern to mine (default: 12)
    --gap <n>              tokens allowed between matched items (default: 1)

export — write the demonstration dataset
    --out <dir>            directory to write; one .jsonl file per session
    --consented-only       required when reading a database (see below)
    --mode <full|structural>
                           export only actors whose consent is exactly this
    --db <path>            database to read (default: $DATABASE_URL, else
                           gridline.db)
    --in <path>            read a JSONL log instead of a database

Routines are mined per (actor, workbook) pair, because a habit belongs to
the person who has it. `--db` upserts on the routine id, which is derived
from the pattern's shape and so is stable across runs: re-mining a growing
log refreshes a proposal rather than stacking a second copy of it, and a
routine the user has already accepted or dismissed keeps that verdict.

`export` writes one record per action: {pre_state_digest, context, action,
post_state_digest}. Under `structural` consent the literals were never
recorded, so the replay uses placeholders derived from their hashes —
equal values stay equal, but they are not the user's numbers.

`--consented-only` is mandatory for a database export rather than a default,
because an operator who has to type it cannot later say they did not know
the export was filtered — and a flag that has to be typed cannot be dropped
by a script that was copied from somewhere else. It is refused with `--in`:
a JSONL log carries no consent records, so nothing there can be checked.
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

    let events = read_jsonl(&input)?;

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

/// Read a JSONL event log from a path, or from stdin when the path is `-`.
///
/// A malformed line is reported and skipped rather than aborting the run: one
/// bad envelope must not cost a user every routine in the log.
fn read_jsonl(path: &str) -> Result<Vec<EventEnvelope>, String> {
    let text = if path == "-" {
        let mut buf = String::new();
        io::stdin()
            .read_to_string(&mut buf)
            .map_err(|e| format!("reading stdin: {e}"))?;
        buf
    } else {
        std::fs::read_to_string(path).map_err(|e| format!("reading {path}: {e}"))?
    };

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
    Ok(events)
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

/// `export` — write the demonstration dataset, one JSONL file per session.
fn export(args: &[String]) -> Result<(), String> {
    let mut out: Option<String> = None;
    let mut db: Option<String> = None;
    let mut input: Option<String> = None;
    let mut mode: Option<String> = None;
    let mut consented_only = false;

    let mut i = 0;
    while i < args.len() {
        let take = |i: &mut usize| -> Result<String, String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| format!("{} needs a value", args[*i - 1]))
        };
        match args[i].as_str() {
            "--out" => out = Some(take(&mut i)?),
            "--db" => db = Some(take(&mut i)?),
            "--in" => input = Some(take(&mut i)?),
            "--mode" => mode = Some(take(&mut i)?),
            "--consented-only" => consented_only = true,
            other => return Err(format!("unknown option {other:?}\n\n{USAGE}")),
        }
        i += 1;
    }
    let out = out.ok_or_else(|| format!("--out is required\n\n{USAGE}"))?;

    if let Some(m) = &mode {
        match m.as_str() {
            "full" | "structural" => {}
            "off" => {
                return Err(
                    "--mode off would select actors who declined; there is nothing to export"
                        .into(),
                )
            }
            other => return Err(format!("--mode {other:?}: expected full or structural")),
        }
    }

    let events = match &input {
        Some(path) => {
            // A JSONL log carries events, not consent records. Accepting the
            // flag here would let a script claim a check that never ran.
            if consented_only {
                return Err(
                    "--consented-only needs a database: a JSONL log carries no consent \
                            records, so nothing in it can be checked. Export from --db, or drop \
                            the flag and accept that the log's own provenance is all you have."
                        .into(),
                );
            }
            if mode.is_some() {
                return Err(
                    "--mode filters on recorded consent, which a JSONL log does not \
                            carry. Export from --db instead."
                        .into(),
                );
            }
            eprintln!("reading {path}: consent is not checked for a file export");
            read_jsonl(path)?
        }
        None => {
            if !consented_only {
                return Err(
                    "refusing to export a database without --consented-only: without it \
                            this would include actors who never agreed, and those who have since \
                            revoked."
                        .into(),
                );
            }
            let url = db.unwrap_or_else(default_database_url);
            read_consented(&url, mode.as_deref())?
        }
    };

    let (records, report) = miner::dataset::export(&events);
    eprintln!(
        "{} event(s) → {} record(s) across {} session(s); {} skipped, {} refused by the engine",
        events.len(),
        report.records,
        report.sessions,
        report.skipped,
        report.rejected
    );

    write_dataset(&out, &records, mode.as_deref(), input.is_some())
}

/// The same database the server opens, so `export` with no `--db` reads the
/// events the running server just wrote.
fn default_database_url() -> String {
    std::env::var("DATABASE_URL").unwrap_or_else(|_| "sqlite://gridline.db".into())
}

fn read_consented(url: &str, mode: Option<&str>) -> Result<Vec<EventEnvelope>, String> {
    let url = if url.starts_with("sqlite:") {
        url.to_string()
    } else {
        format!("sqlite://{url}")
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("starting a runtime: {e}"))?;
    runtime.block_on(async {
        let pool = sqlx::SqlitePool::connect(&url)
            .await
            .map_err(|e| format!("opening {url}: {e}"))?;
        miner::store::consented_events(&pool, mode)
            .await
            .map_err(|e| format!("reading events: {e}"))
    })
}

/// One file per session, plus a manifest naming exactly the files that belong
/// to the dataset.
fn write_dataset(
    dir: &str,
    records: &[miner::dataset::Record],
    mode: Option<&str>,
    from_file: bool,
) -> Result<(), String> {
    let path = std::path::Path::new(dir);
    std::fs::create_dir_all(path).map_err(|e| format!("creating {dir}: {e}"))?;

    // Group in the order the records arrived; `dataset::export` already
    // emits a session's records contiguously.
    let mut sessions: Vec<(&str, Vec<&miner::dataset::Record>)> = Vec::new();
    for r in records {
        let id = r.context.session_id.as_str();
        match sessions.last_mut() {
            Some((last, bucket)) if *last == id => bucket.push(r),
            _ => sessions.push((id, vec![r])),
        }
    }

    let mut written: Vec<serde_json::Value> = Vec::new();
    for (n, (id, bucket)) in sessions.iter().enumerate() {
        let name = format!("{:04}-{}.jsonl", n, safe_name(id));
        let owned: Vec<miner::dataset::Record> = bucket.iter().map(|r| (*r).clone()).collect();
        std::fs::write(path.join(&name), miner::dataset::to_jsonl(&owned))
            .map_err(|e| format!("writing {name}: {e}"))?;
        written.push(serde_json::json!({
            "file": name,
            "session_id": id,
            "records": bucket.len(),
            "values_synthetic": bucket.iter().any(|r| r.context.values_synthetic),
        }));
    }

    let manifest = serde_json::json!({
        "schema": "gridline.dataset.v1",
        "source": if from_file { "jsonl" } else { "database" },
        "consent_checked": !from_file,
        "consent_mode": mode,
        "sessions": written,
        "records": records.len(),
        "note": "Under structural capture the literals in `action` are placeholders \
                 derived from value hashes: equal values stay equal, but these are \
                 not the user's numbers.",
    });
    let text = serde_json::to_string_pretty(&manifest)
        .map_err(|e| format!("serializing the manifest: {e}"))?;
    std::fs::write(path.join("manifest.json"), format!("{text}\n"))
        .map_err(|e| format!("writing manifest.json: {e}"))?;

    // A file left behind by an earlier run would read as part of this dataset
    // to anyone globbing the directory. Deleting it is not ours to do, so say
    // so instead.
    let ours: Vec<&str> = written.iter().filter_map(|w| w["file"].as_str()).collect();
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.ends_with(".jsonl") && !ours.contains(&name.as_str()) {
                eprintln!(
                    "warning: {name} is left over from an earlier run and is not in the manifest"
                );
            }
        }
    }

    eprintln!("wrote {} session file(s) to {dir}", written.len());
    Ok(())
}

/// A filename derived from a session id, which arrives from a client and so
/// is not trusted to be a filename.
fn safe_name(id: &str) -> String {
    let cleaned: String = id
        .chars()
        .take(64)
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.is_empty() {
        "session".into()
    } else {
        cleaned
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_session_id_cannot_escape_the_output_directory() {
        // Session ids come from the client. A file named `../../etc/whatever`
        // must land in the output directory as a mangled name, not outside it.
        assert_eq!(safe_name("../../etc/passwd"), "______etc_passwd");
        assert_eq!(safe_name("/absolute"), "_absolute");
        assert_eq!(safe_name(""), "session");
        assert!(!safe_name(&"x".repeat(500)).contains('/'));
        assert_eq!(safe_name(&"x".repeat(500)).len(), 64);
    }

    #[test]
    fn ordinary_session_ids_survive_intact() {
        assert_eq!(
            safe_name("01HQ8Z9J5K2M3N4P5Q6R7S8T9V"),
            "01HQ8Z9J5K2M3N4P5Q6R7S8T9V"
        );
    }
}
