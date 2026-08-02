//! Gridline miner: turn a captured event log into routines worth proposing.
//!
//! The pipeline is four steps, each in its own module and each testable on
//! its own:
//!
//!   1. [`normalize`] — events to abstract tokens, dropping position and
//!      literal values so a habit repeated in twenty rows reads as one habit.
//!   2. [`mine`] — tandem-repeat detection for loops the user ran by hand,
//!      then PrefixSpan for patterns spread across sessions.
//!   3. [`score`] — estimated minutes saved, so the panel can be ranked and
//!      the small stuff never shown.
//!   4. [`routine`] — synthesis into a macro of typed engine `Action`s. The
//!      routine type and its dry-run sandbox live in the engine, because the
//!      miner, the server and the client all need them and they must agree.
//!
//! [`store`] writes the result into the server's `routines` table.
//!
//! Nothing here reads a file or a socket; the CLI in `main.rs` does the I/O.

pub mod mine;
pub mod normalize;
pub mod routine;
pub mod score;
pub mod store;

use engine::telemetry::EventEnvelope;
use mine::MineConfig;
use routine::Routine;

/// The whole pipeline: envelopes in, routines out, best first.
///
/// Envelopes are expected in the order they were recorded. Sessionization is
/// the server's job, not the miner's — a miner that re-derived session
/// boundaries could disagree with the log it is mining.
pub fn mine_routines(events: &[EventEnvelope], cfg: MineConfig) -> Vec<Routine> {
    let steps = normalize::normalize(events);
    let patterns = mine::mine_all(&steps, cfg);
    // Synthesis needs the raw payloads, which the tokens deliberately dropped.
    let raw: Vec<serde_json::Value> = events
        .iter()
        .map(|e| serde_json::json!({ "action": e.action, "payload": e.payload }))
        .collect();

    let mut out: Vec<Routine> = Vec::new();
    for scored in score::rank(&patterns) {
        let Some(r) = routine::synthesize(&scored, &steps, &raw) else {
            continue;
        };
        // Two patterns can synthesize to the same macro; propose it once.
        if out.iter().any(|existing| existing.id == r.id) {
            continue;
        }
        out.push(r);
    }
    out
}
