//! `gridline-agent` — run the agent over a set of tasks and score it.
//!
//! ```text
//! gridline-agent solve    --store DIR --tasks t.jsonl [--out runs.jsonl]
//!                         [--memory plans.jsonl] [--min-support N]
//! gridline-agent evaluate --store DIR --tasks t.jsonl --policy rules|memo
//!                         [--memory plans.jsonl] --out card.json
//! gridline-agent promote  --incumbent a.json --candidate b.json
//! ```
//!
//! `evaluate` scores a policy against a versioned corpus from immutable
//! snapshots; `promote` decides whether one scorecard may replace another,
//! and refuses on any of several conditions that a single number cannot
//! express — most importantly that a policy which completes more tasks while
//! modifying unrelated cells is worse than one that completes fewer.
//!
//! With `--memory`, successful plans are remembered and the ones that recur
//! become micro-policies; the run then routes to them first and falls back to
//! the rule planner. Run it twice over the same corpus and the second run
//! answers more of it from memory — which is the whole loop in one command.
//!
//! The scorecard it prints is deliberately more than a pass rate. A policy
//! that completes more tasks while modifying unrelated cells is worse than
//! one that completes fewer, and a single number cannot say so.

use std::path::PathBuf;
use std::process::ExitCode;

use agent::memory::{MemoPlanner, PlanLibrary};
use agent::policy::Router;
use agent::run::{run, RunConfig};
use agent::RulePlanner;
use env::{Env, EnvError, SnapshotStore};

fn main() -> ExitCode {
    match go() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("gridline-agent: {e}");
            ExitCode::FAILURE
        }
    }
}

const USAGE: &str = "usage: gridline-agent <solve|evaluate|promote> ...";

fn go() -> Result<ExitCode, EnvError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        return Ok(ExitCode::FAILURE);
    };
    let mut flags = Flags::default();
    let mut it = args[1..].iter();
    while let Some(arg) = it.next() {
        match arg.as_str() {
            "--store" => flags.store = it.next().map(PathBuf::from),
            "--tasks" => flags.tasks = it.next().map(PathBuf::from),
            "--out" => flags.out = it.next().map(PathBuf::from),
            "--memory" => flags.memory = it.next().map(PathBuf::from),
            "--policy" => flags.policy = it.next().cloned().unwrap_or_default(),
            "--incumbent" => flags.incumbent = it.next().map(PathBuf::from),
            "--candidate" => flags.candidate = it.next().map(PathBuf::from),
            "--min-support" => {
                flags.min_support = it.next().and_then(|n| n.parse().ok()).unwrap_or(2)
            }
            other => eprintln!("ignoring {other}"),
        }
    }
    match command {
        "solve" => solve(&flags),
        "evaluate" => evaluate_cmd(&flags),
        "promote" => promote_cmd(&flags),
        other => {
            eprintln!("unknown command `{other}`\n{USAGE}");
            Ok(ExitCode::FAILURE)
        }
    }
}

fn solve(flags: &Flags) -> Result<ExitCode, EnvError> {
    let Some(tasks_path) = &flags.tasks else {
        eprintln!("solve: needs --tasks FILE.jsonl");
        return Ok(ExitCode::FAILURE);
    };
    let tasks = env::task::load_tasks(tasks_path)?;
    let store = match &flags.store {
        Some(dir) => SnapshotStore::read_only_at(dir)?,
        None => SnapshotStore::in_memory(),
    };

    // Everything remembered from previous runs, distilled into policies the
    // cheap side of the router can answer from.
    let mut library = match &flags.memory {
        Some(path) if path.exists() => PlanLibrary::load(path)?,
        _ => PlanLibrary::new(),
    };
    let policies = library.cluster(flags.min_support);
    if !policies.is_empty() {
        eprintln!("{} micro-polic(ies) in memory:", policies.len());
        for p in &policies {
            eprintln!("  [{}x] {}", p.support, p.description);
        }
    }

    let mut env = Some(Env::new(store));
    let mut card = Scorecard::default();
    let mut trajectories = Vec::new();

    for task in &tasks {
        let mut memo = MemoPlanner::new(policies.clone());
        let mut rules = RulePlanner::new();
        let mut router = Router::new(&mut memo, &mut rules, 0.7);
        let (returned, outcome) = run(
            env.take().expect("environment is threaded through"),
            &mut router,
            task,
            &RunConfig::default(),
        )?;
        card.from_memory += router.stats().fast;
        card.from_planner += router.stats().slow;
        env = Some(returned);

        card.tasks += 1;
        if outcome.passed() {
            card.passed += 1;
        }
        if outcome.incidental_changes() > 0 {
            card.with_collateral += 1;
        }
        card.incidental_cells += outcome.incidental_changes();
        card.replans += outcome.replans;
        card.refusals += outcome.rejected.len() as u32;

        println!(
            "{}\t{}\t{} step(s)\t{} replan(s)\t{} incidental",
            if outcome.passed() { "pass" } else { "FAIL" },
            task.id,
            outcome.applied.len(),
            outcome.replans,
            outcome.incidental_changes(),
        );
        for r in &outcome.rejected {
            println!("      refused {}: {}", r.step.kind(), r.reason);
        }
        if library.remember(&outcome) {
            card.remembered += 1;
        }
        trajectories.push(outcome.trajectory);
    }

    if let Some(out) = &flags.out {
        env::trajectory::append_jsonl(out, &trajectories)?;
    }
    if let Some(path) = &flags.memory {
        library.save(path)?;
    }
    eprintln!("{}", card.render());

    // A run with collateral damage is not a success even when every task
    // passed, and the exit code says so.
    Ok(if card.passed == card.tasks && card.with_collateral == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

#[derive(Default)]
struct Scorecard {
    tasks: u32,
    passed: u32,
    /// Tasks that passed or failed while changing cells nobody asked about.
    with_collateral: u32,
    incidental_cells: u32,
    replans: u32,
    refusals: u32,
    /// Proposals answered from memory rather than by the planner. What the
    /// whole distillation exercise is measured by — a memory that never
    /// answers has saved nothing.
    from_memory: u32,
    from_planner: u32,
    remembered: u32,
}

impl Scorecard {
    fn render(&self) -> String {
        format!(
            "{}/{} passed | {} run(s) touched cells nobody asked about ({} cell(s)) | \
             {} replan(s) | {} refusal(s) | {} from memory, {} from the planner | \
             {} new plan(s) remembered",
            self.passed,
            self.tasks,
            self.with_collateral,
            self.incidental_cells,
            self.replans,
            self.refusals,
            self.from_memory,
            self.from_planner,
            self.remembered
        )
    }
}

struct Flags {
    store: Option<PathBuf>,
    tasks: Option<PathBuf>,
    out: Option<PathBuf>,
    memory: Option<PathBuf>,
    min_support: usize,
    policy: String,
    incumbent: Option<PathBuf>,
    candidate: Option<PathBuf>,
}

impl Default for Flags {
    fn default() -> Self {
        Flags {
            store: None,
            tasks: None,
            out: None,
            memory: None,
            min_support: 2,
            policy: "rules".to_string(),
            incumbent: None,
            candidate: None,
        }
    }
}

/// Score one policy against a versioned corpus.
fn evaluate_cmd(flags: &Flags) -> Result<ExitCode, EnvError> {
    let Some(tasks_path) = &flags.tasks else {
        eprintln!("evaluate: needs --tasks FILE.jsonl");
        return Ok(ExitCode::FAILURE);
    };
    let corpus = agent::eval::EvalCorpus::load(tasks_path)?;
    // Read-only: an evaluation that leaves new snapshots in the corpus it
    // measured against has changed the thing it was measuring.
    let store = match &flags.store {
        Some(dir) => SnapshotStore::read_only_at(dir)?,
        None => SnapshotStore::in_memory(),
    };

    let policies = match &flags.memory {
        Some(path) if path.exists() => PlanLibrary::load(path)?.cluster(flags.min_support),
        _ => Vec::new(),
    };
    if flags.policy == "memo" && policies.is_empty() {
        eprintln!("evaluate: --policy memo needs a --memory file with something in it");
        return Ok(ExitCode::FAILURE);
    }

    // The counts a router would report. Without one, every proposal came
    // from whichever planner was asked, and saying so is more honest than
    // reporting zero.
    let from_memory = flags.policy == "memo";
    let mut counts = || {
        if from_memory {
            agent::eval::CallCounts {
                planner: 0,
                memory: 1,
            }
        } else {
            agent::eval::CallCounts {
                planner: 1,
                memory: 0,
            }
        }
    };
    let name = flags.policy.clone();
    let policies_for_factory = policies.clone();
    let mut make = || -> Box<dyn agent::run::Planner> {
        if from_memory {
            Box::new(MemoPlanner::new(policies_for_factory.clone()))
        } else {
            Box::new(RulePlanner::new())
        }
    };

    let (_, card) = agent::eval::evaluate(
        Env::new(store),
        &name,
        &corpus,
        &mut make,
        &mut counts,
        &RunConfig::default(),
    )?;
    print!("{}", card.render());
    if let Some(out) = &flags.out {
        if let Some(parent) = out.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent)?;
            }
        }
        std::fs::write(out, serde_json::to_string_pretty(&card)?)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Decide whether a candidate may replace the incumbent.
fn promote_cmd(flags: &Flags) -> Result<ExitCode, EnvError> {
    let (Some(a), Some(b)) = (&flags.incumbent, &flags.candidate) else {
        eprintln!("promote: needs --incumbent and --candidate scorecards");
        return Ok(ExitCode::FAILURE);
    };
    let incumbent: agent::eval::Scorecard = serde_json::from_str(&std::fs::read_to_string(a)?)?;
    let candidate: agent::eval::Scorecard = serde_json::from_str(&std::fs::read_to_string(b)?)?;
    match agent::eval::promotion(&incumbent, &candidate) {
        agent::eval::Verdict::Promote { because } => {
            println!("PROMOTE {} over {}", candidate.policy, incumbent.policy);
            for r in &because {
                println!("  {r}");
            }
            Ok(ExitCode::SUCCESS)
        }
        agent::eval::Verdict::Hold { reasons } => {
            println!(
                "HOLD {} — not promoted over {}",
                candidate.policy, incumbent.policy
            );
            for r in &reasons {
                println!("  {r}");
            }
            Ok(ExitCode::FAILURE)
        }
    }
}
