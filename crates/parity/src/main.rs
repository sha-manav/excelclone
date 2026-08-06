//! The parity CLI.
//!
//! ```text
//! gridline-parity run                 # measure, print a summary, exit 1 on a difference
//! gridline-parity report              # regenerate PARITY.md
//! gridline-parity report --check      # fail if PARITY.md is out of date
//! ```

use std::path::{Path, PathBuf};

use parity::report::Summary;
use parity::{load_cases, load_targets, roundtrip, run, Verdict};

fn main() {
    if let Err(e) = go() {
        eprintln!("gridline-parity: {e}");
        std::process::exit(2);
    }
}

const USAGE: &str = "\
gridline-parity — measure how close the engine is to Excel

USAGE:
    gridline-parity run [--corpus <dir>] [--fixtures <dir>]
    gridline-parity report [--out <path>] [--check]

Exit codes: 0 all settled cases match and every workbook round-trips,
1 something differs, 2 the harness could not run.
";

fn go() -> Result<(), String> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let root = repo_root();
    let mut corpus = root.join("parity/cases");
    let mut fixtures = root.join("fixtures");
    let mut targets_path = root.join("parity/functions.toml");
    let mut out = root.join("PARITY.md");
    let mut check = false;

    let command = args.first().cloned().unwrap_or_else(|| "run".into());
    let mut i = 1;
    while i < args.len() {
        let take = |i: &mut usize| -> Result<String, String> {
            *i += 1;
            args.get(*i)
                .cloned()
                .ok_or_else(|| format!("{} needs a value", args[*i - 1]))
        };
        match args[i].as_str() {
            "--corpus" => corpus = PathBuf::from(take(&mut i)?),
            "--fixtures" => fixtures = PathBuf::from(take(&mut i)?),
            "--functions" => targets_path = PathBuf::from(take(&mut i)?),
            "--out" => out = PathBuf::from(take(&mut i)?),
            "--check" => check = true,
            other => return Err(format!("unknown option {other:?}\n\n{USAGE}")),
        }
        i += 1;
    }

    let cases = load_cases(&corpus).map_err(|e| e.to_string())?;
    let targets = load_targets(&targets_path).map_err(|e| e.to_string())?;
    let outcomes = run(&cases);
    let trips = roundtrip::run(&fixtures);
    let summary = Summary {
        outcomes: &outcomes,
        targets: &targets,
        trips: &trips,
    };

    match command.as_str() {
        "run" => {
            let c = summary.counts();
            for o in &outcomes {
                match &o.verdict {
                    Verdict::Differs { got } => println!(
                        "differs  {}\n  {}\n  expected {:?}, got {:?}",
                        o.case.id, o.case.formula, o.case.expect, got
                    ),
                    // Without this the run exits 1 and says nothing, which is
                    // exactly the failure mode the flag exists to prevent.
                    Verdict::Fixed => println!(
                        "fixed    {}\n  {}\n  now matches {:?}; remove `known_difference` \
                         from the case",
                        o.case.id, o.case.formula, o.case.expect
                    ),
                    _ => {}
                }
            }
            for t in &trips {
                if !t.clean() {
                    println!(
                        "round trip  {}  state_identical={} lost={:?} error={:?}",
                        t.file, t.state_identical, t.lost, t.error
                    );
                }
            }
            println!(
                "{} of {} settled cases match; {} open; {} of {} workbooks round-trip",
                c.matched,
                c.settled,
                c.open,
                trips.iter().filter(|t| t.clean()).count(),
                trips.len()
            );
            if !summary.ok() {
                std::process::exit(1);
            }
        }
        "report" => {
            let text = parity::report::render(&summary);
            if check {
                let current = std::fs::read_to_string(&out).unwrap_or_default();
                if current != text {
                    eprintln!(
                        "{} is out of date; run `make parity` and commit the result",
                        out.display()
                    );
                    std::process::exit(1);
                }
                println!("{} is up to date", out.display());
            } else {
                std::fs::write(&out, &text)
                    .map_err(|e| format!("writing {}: {e}", out.display()))?;
                println!("wrote {}", out.display());
            }
            // A stale report and a real difference are different failures, so
            // the difference is reported after the freshness check either way.
            if !summary.ok() {
                std::process::exit(1);
            }
        }
        "--help" | "-h" => println!("{USAGE}"),
        other => return Err(format!("unknown command {other:?}\n\n{USAGE}")),
    }
    Ok(())
}

/// The workspace root, found from the compiled-in manifest path so the binary
/// works from any working directory.
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}
