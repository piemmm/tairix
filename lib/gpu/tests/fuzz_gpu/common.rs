//! What the submission sweeps share, whichever device they run on: what makes
//! a round's device, the draws its numbers come from, and the budgeted loop
//! that runs it.

use std::sync::Arc;

use tairix_fuzzseed::Prng;
use tairix_gpu::{Device, QueueFamily};

/// What makes a round's device, its work shared across a runner, stating a
/// second family beside its own where given.
pub(crate) type Make<'a> =
    &'a dyn Fn(Arc<dyn tairix_parallel::JobRunner + Send>, Option<QueueFamily>) -> Device;

/// The machine memory a round's device has: its heap is half of it.
pub(crate) const MEMORY: u64 = 64 << 20;

/// A value below `n`.
pub(crate) fn below(rng: &mut Prng, n: u32) -> u32 {
    u32::try_from(rng.below(n as usize)).expect("below a u32")
}

/// An offset the reference has proved within one of its memories.
pub(crate) fn index_of(value: u64) -> usize {
    usize::try_from(value).expect("within a small memory")
}

/// An offset or size drawn to land inside, at the edge of, or past `limit`.
pub(crate) fn draw_extent(rng: &mut Prng, limit: u64) -> u64 {
    match rng.below(8) {
        0 => 0,
        1 => limit,
        2 => limit + 4,
        3 => u64::MAX - rng.below(8) as u64,
        _ => rng.below(usize::try_from(limit).unwrap_or(0) + 1) as u64,
    }
}

/// [`draw_extent`], whole words three times in four.
pub(crate) fn draw_word_extent(rng: &mut Prng, limit: u64) -> u64 {
    let raw = draw_extent(rng, limit);
    if rng.below(4) == 0 {
        raw
    } else {
        raw & !3
    }
}

/// Run `round` with `test`'s seeded stream, `rounds` at a time, until the
/// budget elapses, or once where there is none.
pub(crate) fn sweep(test: &str, rounds: u32, mut round: impl FnMut(&mut Prng)) {
    let mut rng = Prng::new(tairix_fuzzseed::start(test, tairix_fuzzseed::FUZZ_SEED_ENV));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..rounds {
            round(&mut rng);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}
