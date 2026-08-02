//! The parity harness: how close is Gridline to Excel, measured rather than
//! claimed.
//!
//! Three numbers, each earned separately:
//!
//! 1. **Function coverage** — how much of the target function set exists, and
//!    how much of what exists is actually pinned by a case.
//! 2. **Cell-match rate** — of the corpus cases whose expected value is
//!    settled, how many the engine reproduces exactly.
//! 3. **Round-trip fidelity** — whether opening and saving a workbook
//!    preserves both its state and the parts we do not model.
//!
//! # Where the expectations come from
//!
//! **Microsoft Excel does not run on the machines this project is developed
//! on, and no expectation in the corpus was recorded by running it.** That is
//! stated here, in `PARITY.md`, and in the corpus files themselves, because a
//! score whose provenance is vague is worse than no score: it invites the
//! reader to believe a comparison that never happened.
//!
//! Every case therefore carries a [`Source`]. Only `known` and `spec` cases
//! count toward the match rate. `open` cases are the ones nobody here can
//! settle — they record every candidate answer and appear in the report as
//! open questions, which is the honest place for them.
//!
//! A live oracle (a second implementation, or a recording harness run on a
//! machine that has Excel) can be added later without changing the corpus:
//! it would supply `expect` values with a stronger source, and the score
//! would rise or fall accordingly. LibreOffice was tried as that oracle and
//! rejected: it is a different implementation with its own divergences, so
//! agreement with it is evidence about LibreOffice, not about Excel.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use engine::{Action, CellAddr, Engine};
use serde::Deserialize;

pub mod report;
pub mod roundtrip;

/// Where a case's expected value comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Well-established Excel behaviour: documented by Microsoft, or so
    /// widely reproduced that it is not in question. The case's `note` says
    /// which.
    Known,
    /// Cited to ECMA-376 or MS-OI29500, with the section in the `note`.
    Spec,
    /// Nobody here can settle it. Every candidate answer is recorded and the
    /// case is excluded from the score.
    Open,
}

impl Source {
    /// Whether a case with this source counts toward the match rate.
    pub fn settled(self) -> bool {
        !matches!(self, Source::Open)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Source::Known => "known",
            Source::Spec => "spec",
            Source::Open => "open",
        }
    }
}

/// One comparison: set up a sheet, evaluate a formula, check the result.
#[derive(Debug, Clone, Deserialize)]
pub struct Case {
    /// Stable, dotted, and unique across the corpus — it is what a report
    /// entry is cited by.
    pub id: String,
    /// Cells to fill in first, as formula-bar text keyed by A1 address.
    #[serde(default)]
    pub given: BTreeMap<String, String>,
    /// The formula under test, written where `at` says.
    pub formula: String,
    /// Where the formula goes. Away from the setup by default, because a
    /// formula that overwrites its own input measures nothing.
    #[serde(default = "default_at")]
    pub at: String,
    /// The displayed value Excel produces, as `Value::display` spells it.
    pub expect: String,
    pub source: Source,
    /// Why the expectation is what it is. Required for `known` and `spec`;
    /// for `open` it must say what the candidates are and why it is unsettled.
    #[serde(default)]
    pub note: String,
    /// For `open` cases: the answers under consideration, including ours.
    #[serde(default)]
    pub candidates: Vec<String>,
    /// A difference we know about and have chosen not to fix yet.
    ///
    /// It still counts against the match rate — the score has to reflect what
    /// the engine actually does — but it does not fail the run, because CI's
    /// job is to catch a *regression*, not to re-report a number somebody
    /// already wrote down. If a case marked this way starts matching, the run
    /// fails and asks for the flag to be removed: a fixed difference that
    /// stays recorded is a lie in the other direction.
    #[serde(default)]
    pub known_difference: bool,
    /// Functions this case exercises. Drives the "pinned by a case" column of
    /// the coverage table; a case that names none pins nothing.
    #[serde(default)]
    pub functions: Vec<String>,
}

fn default_at() -> String {
    "Z1".to_string()
}

#[derive(Debug, Deserialize)]
struct CaseFile {
    #[serde(default)]
    case: Vec<Case>,
}

/// The functions parity is measured against.
///
/// Tier 1 is the v1 scope; tier 2 is the next most commonly used set, which
/// exists in this list precisely so coverage is not trivially 100%. A target
/// that only names what is already built measures nothing.
#[derive(Debug, Deserialize)]
pub struct Targets {
    pub tier1: Vec<String>,
    pub tier2: Vec<String>,
}

impl Targets {
    pub fn all(&self) -> impl Iterator<Item = &String> {
        self.tier1.iter().chain(self.tier2.iter())
    }
}

#[derive(Debug)]
pub enum LoadError {
    Io(String),
    Parse(String),
    Duplicate(String),
    Unsourced(String),
}

impl std::fmt::Display for LoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LoadError::Io(m) | LoadError::Parse(m) => write!(f, "{m}"),
            LoadError::Duplicate(id) => write!(f, "two cases share the id {id:?}"),
            LoadError::Unsourced(id) => write!(
                f,
                "case {id:?} has no note; an expectation with no stated provenance is a guess"
            ),
        }
    }
}

/// Read every `*.toml` in the corpus directory, in filename order.
pub fn load_cases(dir: &Path) -> Result<Vec<Case>, LoadError> {
    let mut files: Vec<PathBuf> = std::fs::read_dir(dir)
        .map_err(|e| LoadError::Io(format!("reading {}: {e}", dir.display())))?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().is_some_and(|x| x == "toml"))
        .collect();
    files.sort();

    let mut out: Vec<Case> = Vec::new();
    for path in files {
        let text = std::fs::read_to_string(&path)
            .map_err(|e| LoadError::Io(format!("reading {}: {e}", path.display())))?;
        let parsed: CaseFile = toml::from_str(&text)
            .map_err(|e| LoadError::Parse(format!("{}: {e}", path.display())))?;
        for case in parsed.case {
            if out.iter().any(|c| c.id == case.id) {
                return Err(LoadError::Duplicate(case.id));
            }
            // A note is not decoration: it is the whole difference between a
            // corpus and a list of things somebody assumed.
            if case.note.trim().is_empty() {
                return Err(LoadError::Unsourced(case.id));
            }
            out.push(case);
        }
    }
    Ok(out)
}

pub fn load_targets(path: &Path) -> Result<Targets, LoadError> {
    let text = std::fs::read_to_string(path)
        .map_err(|e| LoadError::Io(format!("reading {}: {e}", path.display())))?;
    toml::from_str(&text).map_err(|e| LoadError::Parse(format!("{}: {e}", path.display())))
}

/// What the engine did with one case.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Match,
    /// The engine produced something else. Both values are carried so the
    /// report can show the difference rather than just its existence.
    Differs {
        got: String,
    },
    /// A difference the corpus already knows about. Scored as a miss, not a
    /// failure.
    Accepted {
        got: String,
    },
    /// A case marked `known_difference` that now matches. A failure, because
    /// the corpus is claiming a divergence that no longer exists.
    Fixed,
    /// An `open` case: recorded, not scored.
    Unsettled {
        got: String,
    },
}

impl Verdict {
    /// Whether this verdict should stop the build.
    pub fn fails(&self) -> bool {
        matches!(self, Verdict::Differs { .. } | Verdict::Fixed)
    }
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub case: Case,
    pub verdict: Verdict,
}

/// Evaluate one case in a fresh workbook.
///
/// A fresh engine per case, deliberately: a corpus where case 40 depends on
/// case 39 having run is a corpus nobody can bisect.
pub fn evaluate(case: &Case) -> String {
    let mut engine = Engine::new();
    // Pinned so date and time functions are comparable at all: 2024-01-01
    // 12:00:00Z, the same instant the golden fixtures use.
    engine.now_ms = 1_704_110_400_000;
    let sheet = engine.wb.sheets[0].name.clone();

    for (addr, input) in &case.given {
        let Some(addr) = CellAddr::parse_a1(addr) else {
            return format!("!bad address in `given`: {addr}");
        };
        if let Err(e) = engine.apply(&Action::CellEdit {
            sheet: sheet.clone(),
            addr,
            input: input.clone(),
        }) {
            return format!("!setup refused: {e}");
        }
    }

    let Some(at) = CellAddr::parse_a1(&case.at) else {
        return format!("!bad address in `at`: {}", case.at);
    };
    if let Err(e) = engine.apply(&Action::CellEdit {
        sheet: sheet.clone(),
        addr: at,
        input: case.formula.clone(),
    }) {
        return format!("!refused: {e}");
    }
    engine.value_at(&sheet, &case.at).display()
}

pub fn run(cases: &[Case]) -> Vec<Outcome> {
    cases
        .iter()
        .map(|case| {
            let got = evaluate(case);
            let verdict = match (
                case.source.settled(),
                got == case.expect,
                case.known_difference,
            ) {
                (false, _, _) => Verdict::Unsettled { got },
                (true, true, false) => Verdict::Match,
                (true, true, true) => Verdict::Fixed,
                (true, false, false) => Verdict::Differs { got },
                (true, false, true) => Verdict::Accepted { got },
            };
            Outcome {
                case: case.clone(),
                verdict,
            }
        })
        .collect()
}

/// Whether the engine has a function at all, asked the only way that cannot
/// go stale: by calling it and seeing whether the answer is `#NAME?`.
pub fn implemented(name: &str) -> bool {
    engine::functions::IMPLEMENTED.contains(&name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn case(formula: &str, expect: &str) -> Case {
        Case {
            id: "t".into(),
            given: BTreeMap::new(),
            formula: formula.into(),
            at: default_at(),
            expect: expect.into(),
            source: Source::Known,
            note: "test".into(),
            candidates: Vec::new(),
            known_difference: false,
            functions: Vec::new(),
        }
    }

    #[test]
    fn a_matching_case_matches_and_a_differing_one_reports_what_it_got() {
        let outcomes = run(&[case("=1+1", "2"), case("=1+1", "3")]);
        assert_eq!(outcomes[0].verdict, Verdict::Match);
        assert_eq!(
            outcomes[1].verdict,
            Verdict::Differs { got: "2".into() },
            "a mismatch must carry the value, or the report cannot show the difference"
        );
    }

    #[test]
    fn an_open_case_is_recorded_but_never_scored() {
        // The whole point of the `open` source: an answer nobody here can
        // settle must not be able to inflate — or deflate — the score.
        let mut c = case("=1+1", "2");
        c.source = Source::Open;
        assert_eq!(run(&[c])[0].verdict, Verdict::Unsettled { got: "2".into() });
    }

    #[test]
    fn setup_cells_are_visible_to_the_formula() {
        let mut c = case("=A1*2", "84");
        c.given.insert("A1".into(), "42".into());
        assert_eq!(run(&[c])[0].verdict, Verdict::Match);
    }

    #[test]
    fn the_formula_does_not_land_on_its_own_input_by_default() {
        // `at` defaults away from A1 precisely so a case can say
        // given.A1 = "1" without the formula overwriting it.
        let mut c = case("=A1+1", "2");
        c.given.insert("A1".into(), "1".into());
        assert_eq!(run(&[c])[0].verdict, Verdict::Match);
    }

    #[test]
    fn a_case_evaluates_the_same_however_many_ran_before_it() {
        // A fresh engine per case. If state leaked, the second run of the
        // same case after a different one would drift.
        let mut dirty = case("=A1+1", "6");
        dirty.given.insert("A1".into(), "5".into());
        let clean = case("=A1+1", "1");
        let outcomes = run(&[dirty.clone(), clean.clone(), dirty]);
        assert_eq!(outcomes[0].verdict, Verdict::Match);
        assert_eq!(
            outcomes[1].verdict,
            Verdict::Match,
            "A1 survived from the previous case"
        );
        assert_eq!(outcomes[2].verdict, Verdict::Match);
    }

    #[test]
    fn an_accepted_difference_is_scored_as_a_miss_but_does_not_fail() {
        let mut c = case("=1+1", "3");
        c.known_difference = true;
        let v = &run(&[c])[0].verdict;
        assert_eq!(*v, Verdict::Accepted { got: "2".into() });
        assert!(!v.fails(), "a recorded difference must not fail the build");
    }

    #[test]
    fn a_recorded_difference_that_has_been_fixed_fails_loudly() {
        // Otherwise the corpus keeps claiming a divergence that is gone, and
        // the score stays lower than the engine deserves.
        let mut c = case("=1+1", "2");
        c.known_difference = true;
        let v = &run(&[c])[0].verdict;
        assert_eq!(*v, Verdict::Fixed);
        assert!(v.fails());
    }

    #[test]
    fn a_case_with_no_note_is_refused_at_load_time() {
        let dir = std::env::temp_dir().join(format!("parity-note-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("a.toml"),
            "[[case]]\nid = \"x\"\nformula = \"=1\"\nexpect = \"1\"\nsource = \"known\"\n",
        )
        .unwrap();
        let err = load_cases(&dir).unwrap_err();
        assert!(matches!(err, LoadError::Unsourced(_)), "{err}");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn two_cases_cannot_share_an_id() {
        // Report entries are cited by id; two rows with the same name make
        // the report unreadable and a fix unverifiable.
        let dir = std::env::temp_dir().join(format!("parity-dupe-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let body = "[[case]]\nid = \"x\"\nformula = \"=1\"\nexpect = \"1\"\n\
                    source = \"known\"\nnote = \"n\"\n";
        std::fs::write(dir.join("a.toml"), format!("{body}{body}")).unwrap();
        assert!(matches!(
            load_cases(&dir).unwrap_err(),
            LoadError::Duplicate(_)
        ));
        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod corpus_tests {
    use super::*;

    fn root() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf()
    }

    fn corpus() -> Vec<Case> {
        load_cases(&root().join("parity/cases")).expect("the corpus loads")
    }

    /// The measurement itself, run by `cargo test` so a regression fails the
    /// ordinary build and not only the parity job.
    #[test]
    fn the_committed_corpus_holds() {
        let outcomes = run(&corpus());
        let broken: Vec<String> = outcomes
            .iter()
            .filter(|o| o.verdict.fails())
            .map(|o| match &o.verdict {
                Verdict::Differs { got } => format!(
                    "  {} : {} — expected {:?}, got {:?}",
                    o.case.id, o.case.formula, o.case.expect, got
                ),
                Verdict::Fixed => format!(
                    "  {} now matches; remove `known_difference` from the case",
                    o.case.id
                ),
                _ => String::new(),
            })
            .collect();
        assert!(broken.is_empty(), "parity changed:\n{}", broken.join("\n"));
    }

    #[test]
    fn every_open_case_records_its_candidates() {
        // An open question with no candidates is a shrug, not a record. The
        // point of the category is that someone with Excel can settle it in
        // one line.
        for c in corpus().iter().filter(|c| c.source == Source::Open) {
            assert!(
                c.candidates.len() >= 2,
                "{} is open but lists {} candidate(s)",
                c.id,
                c.candidates.len()
            );
        }
    }

    #[test]
    fn every_function_a_case_names_is_one_the_targets_list() {
        // A typo in `functions` would quietly under-report coverage.
        let targets = load_targets(&root().join("parity/functions.toml")).unwrap();
        let known: Vec<&str> = targets.all().map(|s| s.as_str()).collect();
        for c in corpus() {
            for f in &c.functions {
                assert!(
                    known.contains(&f.as_str()),
                    "{} names {f}, which is not in parity/functions.toml",
                    c.id
                );
            }
        }
    }

    #[test]
    fn the_committed_report_is_up_to_date() {
        // The same arrangement as the golden fixtures: the score lives in the
        // repository, and it cannot change without a diff.
        let targets = load_targets(&root().join("parity/functions.toml")).unwrap();
        let outcomes = run(&corpus());
        let trips = roundtrip::run(&root().join("fixtures"));
        let text = report::render(&report::Summary {
            outcomes: &outcomes,
            targets: &targets,
            trips: &trips,
        });
        let committed = std::fs::read_to_string(root().join("PARITY.md")).unwrap_or_default();
        assert_eq!(
            committed, text,
            "PARITY.md is stale; run `make parity` and commit the result"
        );
    }
}
