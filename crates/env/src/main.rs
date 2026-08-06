//! `gridline-env` — drive the environment from a shell.
//!
//! Deliberately thin. Everything here is a few lines over the library, and
//! the library is what the training loop calls; a CLI that grew its own
//! logic would be a second implementation nobody tests.
//!
//! ```text
//! gridline-env put      --store DIR <workbook.json|actions.jsonl>...
//! gridline-env show     --store DIR <snapshot-id>
//! gridline-env grade    --store DIR --tasks t.jsonl [--actions a.jsonl]
//! gridline-env record   --store DIR --tasks t.jsonl --actions a.jsonl --out d.jsonl
//! gridline-env augment  --store DIR --tasks t.jsonl --dataset d.jsonl \
//!                       --recipes r.json --out variants.jsonl \
//!                       [--out-tasks variant-tasks.jsonl]
//! gridline-env validate --store DIR --dataset d.jsonl [--tasks t.jsonl]
//! ```
//!
//! `record` then `augment` then `validate` is the dataset pipeline: capture a
//! demonstration, multiply it, and re-check the lot. `validate` is the one
//! worth running in CI — it is what notices when an engine change has quietly
//! invalidated everything recorded before it.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use engine::Action;
use env::{Env, EnvError, SnapshotId, SnapshotStore};

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(e) => {
            eprintln!("gridline-env: {e}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode, EnvError> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(command) = args.first().map(String::as_str) else {
        eprintln!("{USAGE}");
        return Ok(ExitCode::FAILURE);
    };
    let flags = Flags::parse(&args[1..]);

    match command {
        "put" => {
            let mut store = open(&flags)?;
            for path in &flags.positional {
                let wb = read_workbook(path)?;
                println!("{}\t{}", store.put(&wb)?, path.display());
            }
            Ok(ExitCode::SUCCESS)
        }
        "show" => {
            let store = open(&flags)?;
            let Some(id) = flags.positional.first() else {
                eprintln!("show: needs a snapshot id");
                return Ok(ExitCode::FAILURE);
            };
            let mut env = Env::new(store);
            env.reset(&SnapshotId(id.to_string_lossy().into_owned()))?;
            println!("{}", serde_json::to_string_pretty(&env.observe()?)?);
            Ok(ExitCode::SUCCESS)
        }
        "grade" => grade(&flags),
        "record" => record(&flags),
        "augment" => augment(&flags),
        "validate" => validate(&flags),
        other => {
            eprintln!("unknown command `{other}`\n{USAGE}");
            Ok(ExitCode::FAILURE)
        }
    }
}

const USAGE: &str =
    "usage: gridline-env <put|show|grade|record|augment|validate> [--store DIR] ...";

fn open(flags: &Flags) -> Result<SnapshotStore, EnvError> {
    match &flags.store {
        Some(dir) => SnapshotStore::at(dir),
        None => Ok(SnapshotStore::in_memory()),
    }
}

/// Replay a file of actions against each task and report the grade.
///
/// With no `--actions`, this grades the untouched starting state — which is
/// not a pointless thing to run: a task whose checks already pass before
/// anything happens is a broken task, and this is how that gets caught before
/// it reaches the corpus.
fn grade(flags: &Flags) -> Result<ExitCode, EnvError> {
    let Some(tasks_path) = &flags.tasks else {
        eprintln!("grade: needs --tasks FILE.jsonl");
        return Ok(ExitCode::FAILURE);
    };
    let tasks = env::task::load_tasks(tasks_path)?;
    let actions = match &flags.actions {
        Some(path) => load_actions(path)?,
        None => Vec::new(),
    };

    let mut env = Env::new(open(flags)?);
    let mut failed = 0usize;
    for task in &tasks {
        env.reset_for(task)?;
        for action in &actions {
            let result = env.step(action)?;
            if result.budget_exhausted {
                break;
            }
        }
        let grade = env.grade(task)?;
        if !grade.passed {
            failed += 1;
        }
        println!("{}", serde_json::to_string(&grade)?);
    }
    eprintln!("{}/{} tasks passed", tasks.len() - failed, tasks.len());
    Ok(if failed == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Record the same action log against each task and write the trajectories.
///
/// Only demonstrations that pass their grader are written. A recording that
/// failed is a real thing worth keeping — it is Phase 3's raw material — but
/// it does not belong in a file called a dataset of demonstrations, so it is
/// reported and dropped here rather than silently mixed in.
fn record(flags: &Flags) -> Result<ExitCode, EnvError> {
    let (Some(tasks_path), Some(actions_path), Some(out)) =
        (&flags.tasks, &flags.actions, &flags.out)
    else {
        eprintln!("record: needs --tasks, --actions and --out");
        return Ok(ExitCode::FAILURE);
    };
    let tasks = env::task::load_tasks(tasks_path)?;
    let actions = load_actions(actions_path)?;

    let mut env = Some(Env::new(open(flags)?));
    let mut kept = Vec::new();
    let mut dropped = 0usize;
    for task in &tasks {
        let mut recorder = env::Recorder::start_task(
            env.take().expect("environment is threaded through"),
            format!("{}::recorded", task.id),
            task,
            env::Source::Human,
        )?;
        let mut budget_spent = false;
        for action in &actions {
            if recorder.step(action)?.budget_exhausted {
                budget_spent = true;
                break;
            }
        }
        let termination = if budget_spent {
            env::Termination::BudgetExhausted
        } else {
            env::Termination::Done
        };
        let (trajectory, returned) = recorder.finish_with_env(termination, Some(task))?;
        env = Some(returned);
        if trajectory.is_demonstration() {
            kept.push(trajectory);
        } else {
            dropped += 1;
            eprintln!("dropped {}: did not pass its own grader", task.id);
        }
    }
    env::trajectory::append_jsonl(out, &kept)?;
    eprintln!("wrote {} demonstration(s), dropped {dropped}", kept.len());
    Ok(if dropped == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Multiply every demonstration in a dataset by every recipe, keeping the
/// variants that pass.
fn augment(flags: &Flags) -> Result<ExitCode, EnvError> {
    let (Some(dataset), Some(tasks_path), Some(recipes_path), Some(out)) =
        (&flags.dataset, &flags.tasks, &flags.recipes, &flags.out)
    else {
        eprintln!("augment: needs --dataset, --tasks, --recipes and --out");
        return Ok(ExitCode::FAILURE);
    };
    let sources = env::trajectory::load_jsonl(dataset)?;
    let tasks = env::task::load_tasks(tasks_path)?;
    let recipes: Vec<env::augment::Recipe> =
        serde_json::from_str(&std::fs::read_to_string(recipes_path)?)?;

    let mut env = Env::new(open(flags)?);
    let mut variants = Vec::new();
    let mut variant_tasks = Vec::new();
    let (mut accepted, mut rejected) = (0usize, 0usize);
    for source in &sources {
        let Some(task) = tasks
            .iter()
            .find(|t| Some(&t.id) == source.task_id.as_ref())
        else {
            eprintln!("skipped {}: no task with its id", source.id);
            continue;
        };
        if !source.is_demonstration() {
            eprintln!("skipped {}: not a validated demonstration", source.id);
            continue;
        }
        let (returned, report) = env::augment::augment(env, source, task, &recipes)?;
        env = returned;
        accepted += report.accepted.len();
        rejected += report.rejected.len();
        for r in &report.rejected {
            eprintln!("rejected {}::{}: {}", source.id, r.recipe, r.reason);
        }
        for v in report.accepted {
            variant_tasks.push(v.task);
            variants.push(v.trajectory);
        }
    }
    env::trajectory::append_jsonl(out, &variants)?;
    // The variants' task specs, not just their trajectories. Without these
    // the generated corpus can be replayed but not *attempted* — which is
    // most of what it is for.
    if let Some(path) = &flags.out_tasks {
        let mut text = String::new();
        for task in &variant_tasks {
            text.push_str(&serde_json::to_string(task)?);
            text.push('\n');
        }
        std::fs::write(path, text)?;
    }
    eprintln!("{accepted} variant(s) kept, {rejected} rejected");
    Ok(ExitCode::SUCCESS)
}

/// Replay every trajectory in a dataset and report the ones that no longer
/// reproduce.
///
/// The command to run in CI. A trajectory that stops replaying is either
/// corrupt or the engine moved underneath it, and either way it has to stop
/// being training data before it teaches something no longer true.
fn validate(flags: &Flags) -> Result<ExitCode, EnvError> {
    let Some(dataset) = &flags.dataset else {
        eprintln!("validate: needs --dataset");
        return Ok(ExitCode::FAILURE);
    };
    let trajectories = env::trajectory::load_jsonl(dataset)?;
    let tasks = match &flags.tasks {
        Some(p) => env::task::load_tasks(p)?,
        None => Vec::new(),
    };
    let store = open(flags)?;

    let mut bad = 0usize;
    for t in &trajectories {
        let task = tasks
            .iter()
            .find(|task| Some(&task.id) == t.task_id.as_ref());
        let report = env::trajectory::replay(store.clone(), t, task)?;
        let regraded_wrong = report
            .grade
            .as_ref()
            .is_some_and(|g| !g.passed && t.is_demonstration());
        if !report.faithful || regraded_wrong {
            bad += 1;
        }
        println!("{}", serde_json::to_string(&report)?);
    }
    eprintln!(
        "{}/{} trajectories still replay",
        trajectories.len() - bad,
        trajectories.len()
    );
    Ok(if bad == 0 {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// A workbook from a `.json` serialization, or built by replaying a `.jsonl`
/// action log from empty.
///
/// The action log is the form that actually gets written by hand and by the
/// capture pipeline — a serialized `Workbook` carries parsed formula ASTs and
/// is nobody's idea of an input format. Replaying is also the same path the
/// engine's determinism tests exercise, so a starting workbook built this way
/// is reproducible by construction.
fn read_workbook(path: &Path) -> Result<engine::Workbook, EnvError> {
    if path.extension().is_some_and(|e| e == "jsonl") {
        let mut engine = engine::Engine::new();
        for (i, action) in load_actions(path)?.iter().enumerate() {
            engine.apply(action).map_err(|e| {
                EnvError::Io(std::io::Error::other(format!(
                    "{}: action {} was rejected: {e}",
                    path.display(),
                    i + 1
                )))
            })?;
        }
        return Ok(engine.wb);
    }
    let text = std::fs::read_to_string(path)?;
    Ok(serde_json::from_str(&text)?)
}

fn load_actions(path: &Path) -> Result<Vec<Action>, EnvError> {
    let text = std::fs::read_to_string(path)?;
    let mut out = Vec::new();
    for line in text.lines().filter(|l| !l.trim().is_empty()) {
        out.push(serde_json::from_str(line)?);
    }
    Ok(out)
}

#[derive(Default)]
struct Flags {
    store: Option<PathBuf>,
    tasks: Option<PathBuf>,
    actions: Option<PathBuf>,
    dataset: Option<PathBuf>,
    recipes: Option<PathBuf>,
    out: Option<PathBuf>,
    out_tasks: Option<PathBuf>,
    positional: Vec<PathBuf>,
}

impl Flags {
    fn parse(args: &[String]) -> Flags {
        let mut flags = Flags::default();
        let mut it = args.iter();
        while let Some(arg) = it.next() {
            match arg.as_str() {
                "--store" => flags.store = it.next().map(PathBuf::from),
                "--tasks" => flags.tasks = it.next().map(PathBuf::from),
                "--actions" => flags.actions = it.next().map(PathBuf::from),
                "--dataset" => flags.dataset = it.next().map(PathBuf::from),
                "--recipes" => flags.recipes = it.next().map(PathBuf::from),
                "--out" => flags.out = it.next().map(PathBuf::from),
                "--out-tasks" => flags.out_tasks = it.next().map(PathBuf::from),
                other => flags.positional.push(PathBuf::from(other)),
            }
        }
        flags
    }
}
