//! Is this worth proposing?
//!
//! A routine earns its place in the panel by the time it would have saved,
//! not by how often the miner noticed it. A two-step gesture repeated fifty
//! times is worth more than a twelve-step gesture seen three times, and the
//! scoring has to say so.
//!
//! Every number here is an *estimate*, and the panel labels it as one. What
//! the estimates have to get right is the ordering; the absolute minutes are
//! a rough guide to whether something is worth a user's attention at all.
//!
//! The threshold matters more than the model. A panel that proposes
//! everything is a panel nobody reads, so anything under [`MIN_MINUTES_SAVED`]
//! is discarded rather than shown greyed out.

use crate::mine::{Pattern, PatternKind};

/// Nothing below this reaches the panel.
pub const MIN_MINUTES_SAVED: f64 = 2.0;

/// Reading the preview and deciding to trust it. Charged **once**: it is the
/// price of adopting a routine, not of using one. Charging it per repetition
/// (the first version of this did) makes a genuine twelve-row habit look
/// worthless, because it prices in re-reading the same preview twelve times.
const ROUTINE_REVIEW_SECONDS: f64 = 8.0;

/// Running an already-trusted routine once: select the anchor, click Run.
const ROUTINE_RUN_SECONDS: f64 = 1.5;

/// A pattern with its verdict.
#[derive(Debug, Clone)]
pub struct Scored {
    pub pattern: Pattern,
    /// Estimated minutes the user would have saved across the occurrences
    /// already observed.
    pub minutes_saved: f64,
    /// Seconds one occurrence costs by hand.
    pub manual_seconds: f64,
}

/// Score a pattern by the time its observed occurrences would have saved.
///
/// Deliberately backward-looking: this is "you have already spent this long",
/// which the user can check against their own memory, rather than a
/// projection of future savings, which they cannot.
pub fn score(pattern: &Pattern) -> Scored {
    let manual_seconds: f64 = pattern.tokens.iter().map(|t| t.manual_seconds()).sum();
    let per_occurrence = (manual_seconds - ROUTINE_RUN_SECONDS).max(0.0);

    // A loop's repetitions are all evidence of the same sitting, so its
    // occurrence count is the number of repetitions. A recurring pattern is
    // counted by sessions, and each session may have run it more than once.
    let occurrences = match pattern.kind {
        PatternKind::Loop => pattern.support,
        PatternKind::Recurring => pattern.occurrences.len().max(pattern.support),
    } as f64;

    let gross = per_occurrence * occurrences;
    Scored {
        minutes_saved: ((gross - ROUTINE_REVIEW_SECONDS) / 60.0).max(0.0),
        manual_seconds,
        pattern: pattern.clone(),
    }
}

/// Score everything, drop what is not worth a user's attention, best first.
pub fn rank(patterns: &[Pattern]) -> Vec<Scored> {
    let mut out: Vec<Scored> = patterns
        .iter()
        .map(score)
        .filter(|s| s.minutes_saved >= MIN_MINUTES_SAVED)
        .collect();
    out.sort_by(|a, b| {
        b.minutes_saved
            .partial_cmp(&a.minutes_saved)
            .unwrap_or(std::cmp::Ordering::Equal)
            // Ties broken deterministically, so the panel is stable between
            // runs on the same log.
            .then(b.pattern.support.cmp(&a.pattern.support))
            .then_with(|| {
                let (x, y) = (summary_key(&a.pattern), summary_key(&b.pattern));
                x.cmp(&y)
            })
    });
    out
}

fn summary_key(p: &Pattern) -> String {
    p.tokens
        .iter()
        .map(|t| t.to_string())
        .collect::<Vec<_>>()
        .join(">")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mine::Occurrence;
    use crate::normalize::Token;

    fn pattern(tokens: Vec<Token>, support: usize, kind: PatternKind) -> Pattern {
        Pattern {
            occurrences: (0..support)
                .map(|i| Occurrence {
                    start: i * tokens.len(),
                    end: (i + 1) * tokens.len(),
                })
                .collect(),
            tokens,
            support,
            kind,
        }
    }

    fn formula() -> Token {
        Token::Formula {
            shape: "SUM(R[0]C[-3]:R[0]C[-1])".into(),
        }
    }

    #[test]
    fn a_long_gesture_repeated_often_scores_highest() {
        let big = score(&pattern(
            vec![formula(), formula(), Token::Fill { down: true }],
            20,
            PatternKind::Loop,
        ));
        let small = score(&pattern(
            vec![Token::Format {
                attribute: "bold".into(),
            }],
            20,
            PatternKind::Loop,
        ));
        assert!(big.minutes_saved > small.minutes_saved);
    }

    #[test]
    fn doing_something_once_is_never_worth_proposing() {
        // One occurrence pays for the review and almost nothing else, so a
        // one-off cannot clear the threshold however long the gesture is.
        for len in 1..12 {
            let once = score(&pattern(vec![formula(); len], 1, PatternKind::Loop));
            assert!(
                once.minutes_saved < MIN_MINUTES_SAVED,
                "a single occurrence of {len} steps was proposed"
            );
        }
    }

    #[test]
    fn review_is_charged_once_not_once_per_repetition() {
        // The bug this pins: pricing in a re-read of the same preview on
        // every repetition, which made a real twelve-row habit score below
        // the threshold and vanish from the panel.
        let ten = score(&pattern(vec![formula(), formula()], 10, PatternKind::Loop));
        let twenty = score(&pattern(vec![formula(), formula()], 20, PatternKind::Loop));
        let per_extra = (twenty.minutes_saved - ten.minutes_saved) / 10.0;
        let first_ten = (ten.minutes_saved + ROUTINE_REVIEW_SECONDS / 60.0) / 10.0;
        assert!(
            (per_extra - first_ten).abs() < 1e-9,
            "each repetition should be worth the same: {per_extra} vs {first_ten}"
        );
    }

    #[test]
    fn a_gesture_cheaper_than_running_a_routine_saves_nothing() {
        // Clearing one cell takes less time than reading a preview and
        // clicking Run, so automating it is a net loss however often it
        // happens.
        let s = score(&pattern(vec![Token::Clear], 500, PatternKind::Loop));
        assert_eq!(s.minutes_saved, 0.0);
    }

    #[test]
    fn undo_and_redo_contribute_nothing() {
        let with = score(&pattern(
            vec![formula(), Token::Undo, Token::Redo],
            10,
            PatternKind::Loop,
        ));
        let without = score(&pattern(vec![formula()], 10, PatternKind::Loop));
        assert_eq!(with.manual_seconds, without.manual_seconds);
    }

    #[test]
    fn rank_discards_everything_under_the_threshold() {
        let trivial = pattern(
            vec![
                Token::Format {
                    attribute: "bold".into(),
                },
                Token::Format {
                    attribute: "italic".into(),
                },
            ],
            3,
            PatternKind::Loop,
        );
        assert!(score(&trivial).minutes_saved < MIN_MINUTES_SAVED);
        assert!(rank(&[trivial]).is_empty());
    }

    #[test]
    fn rank_keeps_something_genuinely_repetitive() {
        let worthwhile = pattern(
            vec![formula(), formula(), formula(), Token::Fill { down: true }],
            30,
            PatternKind::Loop,
        );
        let ranked = rank(&[worthwhile]);
        assert_eq!(ranked.len(), 1);
        assert!(ranked[0].minutes_saved > MIN_MINUTES_SAVED);
    }

    #[test]
    fn ranking_is_stable_and_deterministic() {
        let a = pattern(vec![formula(), formula()], 40, PatternKind::Loop);
        let b = pattern(vec![formula(), formula()], 40, PatternKind::Loop);
        let ranked = rank(&[a.clone(), b.clone()]);
        let again = rank(&[b, a]);
        assert_eq!(
            ranked.iter().map(|s| s.minutes_saved).collect::<Vec<_>>(),
            again.iter().map(|s| s.minutes_saved).collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_recurring_pattern_is_counted_by_its_occurrences_not_its_sessions() {
        // Three sessions that each ran the gesture twice is six occurrences,
        // and undercounting it to three would hide a real habit.
        let mut p = pattern(
            vec![formula(), formula(), formula()],
            3,
            PatternKind::Recurring,
        );
        p.occurrences = (0..6)
            .map(|i| Occurrence {
                start: i * 3,
                end: i * 3 + 3,
            })
            .collect();
        let by_occurrence = score(&p);
        let by_session = score(&pattern(
            vec![formula(), formula(), formula()],
            3,
            PatternKind::Recurring,
        ));
        assert!(by_occurrence.minutes_saved > by_session.minutes_saved);
    }
}
