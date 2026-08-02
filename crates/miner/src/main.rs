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
    --min-support <n>      sessions a pattern needs (default: 3)
    --max-length <n>       longest pattern to mine (default: 12)
    --gap <n>              tokens allowed between matched items (default: 1)
";

fn mine(args: &[String]) -> Result<(), String> {
    let mut input: Option<String> = None;
    let mut output: Option<String> = None;
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

    let routines = miner::mine_routines(&events, cfg);
    eprintln!(
        "{} event(s), {} routine(s) worth proposing",
        events.len(),
        routines.len()
    );

    let json = serde_json::to_string_pretty(&routines)
        .map_err(|e| format!("serializing routines: {e}"))?;
    match output {
        Some(path) => std::fs::write(&path, format!("{json}\n"))
            .map_err(|e| format!("writing {path}: {e}"))?,
        None => {
            let mut out = io::stdout();
            writeln!(out, "{json}").map_err(|e| format!("writing stdout: {e}"))?;
        }
    }
    Ok(())
}

fn parse_usize(s: &str) -> Result<usize, String> {
    s.parse().map_err(|_| format!("{s:?} is not a number"))
}
