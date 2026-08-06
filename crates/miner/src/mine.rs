//! Finding the patterns.
//!
//! Two miners, because two different things are worth finding and neither
//! finds the other's answers:
//!
//! * [`tandem_repeats`] catches a loop the user ran *by hand right now* —
//!   `w` repeated `k` times back to back. This is the strongest possible
//!   evidence and it needs only one session: someone who just typed the same
//!   five-step gesture eight times down a column does not need three
//!   sessions of corroboration.
//! * [`prefixspan`] catches a habit spread thinly across the log, where the
//!   occurrences are separated by other work. It needs support, because a
//!   subsequence that happens to appear twice means nothing.
//!
//! Both work on the token stream from [`crate::normalize`], never on raw
//! events, so neither can accidentally key a pattern on a cell address.

use crate::normalize::{Step, Token};
use std::collections::HashMap;

/// PrefixSpan settings, per the spec.
#[derive(Debug, Clone, Copy)]
pub struct MineConfig {
    /// Sessions a pattern must appear in before it is believed.
    pub min_support: usize,
    /// Longest pattern to mine. Bounds a search that is otherwise
    /// exponential.
    pub max_length: usize,
    /// How many unrelated tokens may sit between two matched items.
    pub gap_tolerance: usize,
    /// Shortest pattern worth proposing; a single token is not a routine.
    pub min_length: usize,
}

impl Default for MineConfig {
    fn default() -> Self {
        MineConfig {
            min_support: 3,
            max_length: 12,
            gap_tolerance: 1,
            min_length: 2,
        }
    }
}

/// A pattern the miner believes in.
#[derive(Debug, Clone, PartialEq)]
pub struct Pattern {
    pub tokens: Vec<Token>,
    /// How many independent occurrences back it.
    pub support: usize,
    /// Where each occurrence sits in the step stream, so synthesis can walk
    /// back to the concrete actions.
    pub occurrences: Vec<Occurrence>,
    pub kind: PatternKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PatternKind {
    /// Repeated back to back within one session.
    Loop,
    /// Recurring across sessions with other work in between.
    Recurring,
}

/// A half-open span of steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Occurrence {
    pub start: usize,
    pub end: usize,
}

/// Contiguous runs `w^k` with `k >= 2`.
///
/// The scan is over period length rather than over positions: for each
/// candidate period `p`, walk the stream finding maximal runs of that period.
/// `O(n * max_period)`, which is nothing for a session's worth of steps and,
/// unlike a suffix-automaton approach, short enough to be obviously correct.
///
/// Runs are taken greedily, longest period first, and never overlap. Without
/// that, `a a a a` would be reported as a period-1 run of four, a period-2
/// run of two and a period-4 run of one, and the panel would offer three
/// versions of one suggestion.
pub fn tandem_repeats(steps: &[Step], cfg: MineConfig) -> Vec<Pattern> {
    // Per session, never across. A "loop" claims the user did something back
    // to back in one sitting; three separate sittings that happen to start
    // the same way are a *recurring* pattern, and reporting them as a loop
    // both overstates the evidence and puts a false sentence in the panel.
    let mut found: Vec<Pattern> = Vec::new();
    for (base, len) in session_spans(steps) {
        found.extend(runs_within(&steps[base..base + len], cfg, base));
    }
    found.retain(|p| p.tokens.len() >= cfg.min_length || p.support >= cfg.min_support);
    found.sort_by(|a, b| {
        (b.tokens.len() * b.support)
            .cmp(&(a.tokens.len() * a.support))
            .then(a.occurrences[0].start.cmp(&b.occurrences[0].start))
    });
    found
}

/// `(start, len)` of each contiguous run of one session id.
fn session_spans(steps: &[Step]) -> Vec<(usize, usize)> {
    let mut out: Vec<(usize, usize)> = Vec::new();
    for (i, s) in steps.iter().enumerate() {
        match out.last_mut() {
            Some((start, len)) if steps[*start].session_id == s.session_id => *len += 1,
            _ => out.push((i, 1)),
        }
    }
    out
}

/// The period scan, over one session's steps. `base` offsets the reported
/// occurrences back into the whole stream.
fn runs_within(steps: &[Step], cfg: MineConfig, base: usize) -> Vec<Pattern> {
    let tokens: Vec<&Token> = steps.iter().map(|s| &s.token).collect();
    let n = tokens.len();
    let max_period = cfg.max_length.min(n / 2);
    let mut taken = vec![false; n];
    let mut found: Vec<Pattern> = Vec::new();

    // Longest period first: a five-step gesture repeated four times is a
    // better suggestion than the one-step repeat hiding inside it.
    for period in (1..=max_period).rev() {
        let mut i = 0;
        while i + period * 2 <= n {
            if taken[i] {
                i += 1;
                continue;
            }
            // Only primitive periods. `a b a b a b a b` has a period of 4
            // as well as 2, but the gesture the user actually repeated is
            // `a b` four times, not `a b a b` twice — and reporting the
            // longer one would propose a routine that does the work twice.
            if !is_primitive(&tokens[i..i + period]) {
                i += 1;
                continue;
            }
            let mut reps = 1;
            while i + period * (reps + 1) <= n
                && (0..period).all(|k| tokens[i + k] == tokens[i + period * reps + k])
            {
                reps += 1;
            }
            let span = period * reps;
            if reps < 2 || taken[i..i + span].iter().any(|t| *t) {
                i += 1;
                continue;
            }
            for t in taken.iter_mut().skip(i).take(span) {
                *t = true;
            }
            found.push(Pattern {
                tokens: tokens[i..i + period].iter().map(|t| (*t).clone()).collect(),
                support: reps,
                occurrences: (0..reps)
                    .map(|r| Occurrence {
                        start: base + i + period * r,
                        end: base + i + period * (r + 1),
                    })
                    .collect(),
                kind: PatternKind::Loop,
            });
            i += span;
        }
    }
    found
}

/// Frequent subsequences across sessions, by PrefixSpan.
///
/// Each session is one input sequence, so "three times" means three separate
/// sittings rather than three rows of one fill. The loop miner already owns
/// the within-session case, and letting a single session mint support would
/// make every busy afternoon look like a habit.
///
/// `gap_tolerance` bounds how far a projection may skip to find the next
/// item: at 1, `a c b` still matches `a b` but `a c d b` does not. Unbounded
/// gaps would make almost any pair of common tokens "frequent".
pub fn prefixspan(steps: &[Step], cfg: MineConfig) -> Vec<Pattern> {
    prefixspan_excluding(steps, &vec![false; steps.len()], cfg)
}

/// PrefixSpan over the steps a loop has not already accounted for.
///
/// `explained[i]` marks a step that a tandem repeat already covers. Such a
/// step may not be matched, but it still *occupies its position*, so it counts
/// against the gap tolerance exactly as an unrelated action would. Deleting it
/// instead would make the work either side of a loop look adjacent, and
/// manufacture a pattern out of two things that were minutes apart.
pub fn prefixspan_excluding(steps: &[Step], explained: &[bool], cfg: MineConfig) -> Vec<Pattern> {
    let sessions = split_sessions(steps, explained);
    if sessions.len() < cfg.min_support {
        return Vec::new();
    }
    let projections: Vec<Projection> = (0..sessions.len())
        .filter(|i| !sessions[*i].is_empty())
        .map(|i| Projection { session: i, at: 0 })
        .collect();

    let mut out = Vec::new();
    grow(&sessions, &[], &projections, cfg, &mut out);

    // Keep only maximal patterns: if `a b` and `a b c` both reach support,
    // proposing both is proposing one routine twice.
    let mut kept: Vec<Pattern> = out
        .iter()
        .filter(|p| {
            !out.iter()
                .any(|q| q.tokens.len() > p.tokens.len() && is_subsequence(&p.tokens, &q.tokens))
        })
        .cloned()
        .collect();
    kept.sort_by(|a, b| {
        (b.tokens.len() * b.support)
            .cmp(&(a.tokens.len() * a.support))
            .then_with(|| a.tokens.cmp(&b.tokens))
    });
    kept
}

#[derive(Debug, Clone, Copy)]
struct Projection {
    session: usize,
    /// First offset in the session the next item may match at.
    at: usize,
}

/// One step of a session, with its index in the original stream.
#[derive(Debug, Clone)]
struct SessionStep {
    index: usize,
    token: Token,
    /// A loop already accounts for this step, so no recurring pattern may
    /// claim it as evidence.
    explained: bool,
}

type Session = Vec<SessionStep>;

fn split_sessions(steps: &[Step], explained: &[bool]) -> Vec<Session> {
    let mut out: Vec<Session> = Vec::new();
    let mut current: Option<&str> = None;
    for (i, s) in steps.iter().enumerate() {
        if current != Some(s.session_id.as_str()) {
            current = Some(s.session_id.as_str());
            out.push(Vec::new());
        }
        out.last_mut().expect("a session exists").push(SessionStep {
            index: i,
            token: s.token.clone(),
            explained: explained.get(i).copied().unwrap_or(false),
        });
    }
    out
}

fn grow(
    sessions: &[Session],
    prefix: &[Token],
    projections: &[Projection],
    cfg: MineConfig,
    out: &mut Vec<Pattern>,
) {
    if prefix.len() >= cfg.max_length {
        return;
    }
    // Candidate next items, counted once per session so a token repeated
    // inside one session cannot inflate its own support.
    let mut counts: HashMap<Token, Vec<Projection>> = HashMap::new();
    for p in projections {
        let session = &sessions[p.session];
        let limit = if prefix.is_empty() {
            session.len()
        } else {
            (p.at + cfg.gap_tolerance + 1).min(session.len())
        };
        let mut seen_here: Vec<&Token> = Vec::new();
        for (offset, step) in session.iter().enumerate().take(limit).skip(p.at) {
            let token = &step.token;
            // Skipped but not removed: the position still counts as a gap.
            if step.explained || seen_here.contains(&token) {
                continue;
            }
            seen_here.push(token);
            counts.entry(token.clone()).or_default().push(Projection {
                session: p.session,
                at: offset + 1,
            });
        }
    }

    let mut candidates: Vec<(Token, Vec<Projection>)> = counts.into_iter().collect();
    // Deterministic order: what the panel shows must not depend on a hash
    // seed, and the replay guarantee extends to what we propose.
    candidates.sort_by(|a, b| a.0.cmp(&b.0));

    for (token, next) in candidates {
        let mut hit: Vec<usize> = next.iter().map(|p| p.session).collect();
        hit.sort_unstable();
        hit.dedup();
        if hit.len() < cfg.min_support {
            continue;
        }
        let mut extended = prefix.to_vec();
        extended.push(token);
        if extended.len() >= cfg.min_length {
            out.push(Pattern {
                tokens: extended.clone(),
                support: hit.len(),
                occurrences: occurrences_of(sessions, &extended, cfg),
                kind: PatternKind::Recurring,
            });
        }
        grow(sessions, &extended, &next, cfg, out);
    }
}

/// Where a pattern actually occurs, as spans in the original step stream.
/// Recomputed rather than threaded through the search, which carries only
/// "where the next item may start", not where each match began.
fn occurrences_of(sessions: &[Session], pattern: &[Token], cfg: MineConfig) -> Vec<Occurrence> {
    let mut out = Vec::new();
    for session in sessions {
        let mut from = 0;
        while let Some((first, last)) = match_from(session, pattern, from, cfg) {
            out.push(Occurrence {
                start: session[first].index,
                end: session[last].index + 1,
            });
            from = last + 1; // non-overlapping
        }
    }
    out
}

/// First match of `pattern` in `session` at or after `from`, as offsets.
fn match_from(
    session: &Session,
    pattern: &[Token],
    from: usize,
    cfg: MineConfig,
) -> Option<(usize, usize)> {
    let mut start = from;
    while start < session.len() {
        if session[start].explained || session[start].token != pattern[0] {
            start += 1;
            continue;
        }
        let mut at = start + 1;
        let mut ok = true;
        for want in &pattern[1..] {
            let limit = (at + cfg.gap_tolerance + 1).min(session.len());
            match (at..limit).find(|i| !session[*i].explained && session[*i].token == *want) {
                Some(found) => at = found + 1,
                None => {
                    ok = false;
                    break;
                }
            }
        }
        if ok {
            return Some((start, at - 1));
        }
        start += 1;
    }
    None
}

/// Whether a word is *not* some shorter word repeated.
fn is_primitive(word: &[&Token]) -> bool {
    let n = word.len();
    for d in 1..n {
        if !n.is_multiple_of(d) {
            continue;
        }
        if (0..n).all(|k| word[k] == word[k % d]) {
            return false;
        }
    }
    true
}

fn is_subsequence(needle: &[Token], hay: &[Token]) -> bool {
    let mut it = hay.iter();
    needle.iter().all(|n| it.any(|h| h == n))
}

/// Both miners, loops first, with any recurring pattern that merely restates
/// a loop dropped.
pub fn mine_all(steps: &[Step], cfg: MineConfig) -> Vec<Pattern> {
    let loops = merge_loops(tandem_repeats(steps, cfg));

    // Steps a loop already accounts for are not independent evidence of
    // anything else. Without this, twelve identical rows typed over three
    // sittings produce not one proposal but a hundred and fifty: every
    // subsequence that straddles a row boundary reaches support, and the ones
    // that hit `max_length` look maximal only because the search stopped
    // there. The loop is the honest description of that work; the straddles
    // are an artefact of where you start counting.
    let mut explained = vec![false; steps.len()];
    for l in &loops {
        for occ in &l.occurrences {
            for flag in explained
                .iter_mut()
                .take(occ.end.min(steps.len()))
                .skip(occ.start)
            {
                *flag = true;
            }
        }
    }

    let mut out = loops.clone();
    for p in prefixspan_excluding(steps, &explained, cfg) {
        if loops.iter().any(|l| l.tokens == p.tokens) {
            continue;
        }
        out.push(p);
    }
    out
}

/// One loop shape, however many sittings it turned up in.
///
/// The detector works per session, so a habit run every morning arrives as
/// several identical patterns. Proposing each of them separately would offer
/// the same routine three times and understate what it saved: the user did it
/// twelve times, not four.
fn merge_loops(found: Vec<Pattern>) -> Vec<Pattern> {
    let mut out: Vec<Pattern> = Vec::new();
    for p in found {
        match out.iter_mut().find(|q| q.tokens == p.tokens) {
            Some(q) => {
                q.support += p.support;
                q.occurrences.extend(p.occurrences);
            }
            None => out.push(p),
        }
    }
    out.sort_by(|a, b| {
        (b.tokens.len() * b.support)
            .cmp(&(a.tokens.len() * a.support))
            .then(a.occurrences[0].start.cmp(&b.occurrences[0].start))
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok(s: &str) -> Token {
        Token::Other {
            action: s.to_string(),
        }
    }

    fn steps(session: &str, spec: &str) -> Vec<Step> {
        spec.split_whitespace()
            .enumerate()
            .map(|(i, t)| Step {
                token: tok(t),
                source: i,
                session_id: session.to_string(),
                ts_ms: i as i64 * 1000,
            })
            .collect()
    }

    fn joined(sessions: &[(&str, &str)]) -> Vec<Step> {
        let mut out: Vec<Step> = Vec::new();
        for (id, spec) in sessions {
            let base = out.len();
            for mut step in steps(id, spec) {
                step.source += base;
                out.push(step);
            }
        }
        out
    }

    fn shapes(p: &Pattern) -> String {
        p.tokens
            .iter()
            .map(|t| t.to_string().replace("other:", ""))
            .collect::<Vec<_>>()
            .join(" ")
    }

    /* ------------------------------------------------------------- loops */

    #[test]
    fn a_repeated_gesture_is_found_with_its_period() {
        let found = tandem_repeats(&steps("s1", "a b c a b c a b c"), MineConfig::default());
        assert_eq!(shapes(&found[0]), "a b c");
        assert_eq!(found[0].support, 3);
        assert_eq!(found[0].kind, PatternKind::Loop);
    }

    #[test]
    fn a_partial_final_repetition_does_not_extend_the_run() {
        let found = tandem_repeats(&steps("s1", "a b a b a"), MineConfig::default());
        assert_eq!(shapes(&found[0]), "a b");
        assert_eq!(found[0].support, 2);
        assert_eq!(found[0].occurrences.last().unwrap().end, 4);
    }

    #[test]
    fn the_same_run_is_not_reported_at_several_periods() {
        let found = tandem_repeats(&steps("s1", "a a a a"), MineConfig::default());
        assert_eq!(found.len(), 1);
        assert_eq!(shapes(&found[0]), "a");
        assert_eq!(found[0].support, 4);
    }

    #[test]
    fn a_longer_period_wins_over_the_short_one_inside_it() {
        let found = tandem_repeats(&steps("s1", "a b a b a b a b"), MineConfig::default());
        assert_eq!(shapes(&found[0]), "a b");
        assert_eq!(found[0].support, 4);
    }

    #[test]
    fn runs_never_overlap() {
        let found = tandem_repeats(&steps("s1", "a b a b c d c d"), MineConfig::default());
        let mut spans: Vec<(usize, usize)> = found
            .iter()
            .flat_map(|p| p.occurrences.iter().map(|o| (o.start, o.end)))
            .collect();
        spans.sort();
        for w in spans.windows(2) {
            assert!(w[0].1 <= w[1].0, "spans {:?} and {:?} overlap", w[0], w[1]);
        }
    }

    #[test]
    fn a_run_never_spans_a_session_boundary() {
        // Three sittings that each start the same way are not one loop. The
        // concatenated stream reads as `(a b c)^3`, and a scan that ignores
        // session boundaries reports exactly that — overstating the evidence
        // and putting "repeated 3 times in a row" in front of a user for
        // whom it never happened.
        let s = joined(&[("s1", "a b c"), ("s2", "a b c"), ("s3", "a b c")]);
        assert!(
            tandem_repeats(&s, MineConfig::default()).is_empty(),
            "a loop was found across separate sessions"
        );
        // ...whereas the same tokens inside one sitting are a loop.
        let one = joined(&[("s1", "a b c a b c a b c")]);
        assert_eq!(tandem_repeats(&one, MineConfig::default())[0].support, 3);
    }

    #[test]
    fn nothing_repeated_yields_nothing() {
        assert!(tandem_repeats(&steps("s1", "a b c d e"), MineConfig::default()).is_empty());
    }

    #[test]
    fn a_period_longer_than_the_cap_is_not_mined() {
        let unit: String = (0..30).map(|i| format!("t{i} ")).collect();
        let found = tandem_repeats(&steps("s1", &unit.repeat(2)), MineConfig::default());
        assert!(
            found.iter().all(|p| p.tokens.len() <= 12),
            "mined a pattern longer than max_length"
        );
    }

    /* -------------------------------------------------------- prefixspan */

    #[test]
    fn a_habit_repeated_across_sessions_reaches_support() {
        let s = joined(&[("s1", "x a b y"), ("s2", "a b z"), ("s3", "q a b")]);
        let found = prefixspan(&s, MineConfig::default());
        assert!(
            found.iter().any(|p| shapes(p) == "a b" && p.support == 3),
            "{:?}",
            found.iter().map(shapes).collect::<Vec<_>>()
        );
    }

    #[test]
    fn two_sessions_are_not_enough() {
        let s = joined(&[("s1", "a b"), ("s2", "a b")]);
        assert!(prefixspan(&s, MineConfig::default()).is_empty());
    }

    #[test]
    fn a_gap_of_one_is_tolerated_and_a_gap_of_two_is_not() {
        // The intervening tokens differ per session, so `a ? b` cannot reach
        // support and `a b` is the only candidate — which isolates the gap
        // tolerance from the maximality filter.
        let near = joined(&[("s1", "a p b"), ("s2", "a q b"), ("s3", "a r b")]);
        assert!(
            prefixspan(&near, MineConfig::default())
                .iter()
                .any(|p| shapes(p) == "a b"),
            "one intervening token should still match"
        );

        let far = joined(&[("s1", "a p q b"), ("s2", "a r s b"), ("s3", "a t u b")]);
        assert!(
            !prefixspan(&far, MineConfig::default())
                .iter()
                .any(|p| shapes(p) == "a b"),
            "two intervening tokens should not"
        );
    }

    #[test]
    fn a_pattern_swallowed_by_a_longer_one_is_not_proposed_separately() {
        // When every session runs `a x b`, that is the routine; offering
        // `a b` as well would be offering half of it.
        let s = joined(&[("s1", "a x b"), ("s2", "a x b"), ("s3", "a x b")]);
        let names: Vec<String> = prefixspan(&s, MineConfig::default())
            .iter()
            .map(shapes)
            .collect();
        assert_eq!(names, vec!["a x b"]);
    }

    #[test]
    fn repeating_a_pattern_inside_one_session_does_not_manufacture_support() {
        // One busy afternoon is a loop, not three sessions of evidence.
        let s = joined(&[("s1", "a b a b a b a b a b")]);
        assert!(prefixspan(&s, MineConfig::default()).is_empty());
    }

    #[test]
    fn only_maximal_patterns_survive() {
        let s = joined(&[("s1", "a b c"), ("s2", "a b c"), ("s3", "a b c")]);
        let found = prefixspan(&s, MineConfig::default());
        assert_eq!(found.iter().map(shapes).collect::<Vec<_>>(), vec!["a b c"]);
    }

    #[test]
    fn a_single_token_is_never_a_routine() {
        let s = joined(&[("s1", "a q"), ("s2", "a r"), ("s3", "a t")]);
        assert!(prefixspan(&s, MineConfig::default())
            .iter()
            .all(|p| p.tokens.len() >= 2));
    }

    #[test]
    fn results_are_deterministic() {
        let s = joined(&[
            ("s1", "a b c d"),
            ("s2", "a b c d"),
            ("s3", "a b c d"),
            ("s4", "d c b a"),
        ]);
        let a: Vec<String> = prefixspan(&s, MineConfig::default())
            .iter()
            .map(shapes)
            .collect();
        let b: Vec<String> = prefixspan(&s, MineConfig::default())
            .iter()
            .map(shapes)
            .collect();
        assert_eq!(a, b);
    }

    #[test]
    fn occurrences_point_back_into_the_step_stream() {
        let s = joined(&[("s1", "x a b"), ("s2", "a b"), ("s3", "a b")]);
        let found = prefixspan(&s, MineConfig::default());
        let p = found.iter().find(|p| shapes(p) == "a b").expect("pattern");
        assert_eq!(
            p.occurrences,
            vec![
                Occurrence { start: 1, end: 3 },
                Occurrence { start: 3, end: 5 },
                Occurrence { start: 5, end: 7 },
            ]
        );
    }

    #[test]
    fn mine_all_does_not_offer_the_same_pattern_twice() {
        let s = joined(&[("s1", "a b a b a b"), ("s2", "a b"), ("s3", "a b")]);
        let names: Vec<String> = mine_all(&s, MineConfig::default())
            .iter()
            .map(shapes)
            .collect();
        let mut deduped = names.clone();
        deduped.sort();
        deduped.dedup();
        assert_eq!(names.len(), deduped.len(), "duplicate proposals: {names:?}");
    }
}
