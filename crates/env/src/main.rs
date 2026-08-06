//! `gridline-env` — drive the environment from a shell.
//!
//! Deliberately thin. Everything here is a few lines over the library, and
//! the library is what the training loop calls; a CLI that grew its own
//! logic would be a second implementation nobody tests.
//!
//! ```text
//! gridline-env put   --store DIR <workbook.json|actions.jsonl>...
//! gridline-env show  --store DIR <snapshot-id>        print an observation
//! gridline-env grade --store DIR --tasks t.jsonl [--actions a.jsonl]
//! ```

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
        other => {
            eprintln!("unknown command `{other}`\n{USAGE}");
            Ok(ExitCode::FAILURE)
        }
    }
}

const USAGE: &str = "usage: gridline-env <put|show|grade> [--store DIR] ...";

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
                other => flags.positional.push(PathBuf::from(other)),
            }
        }
        flags
    }
}
