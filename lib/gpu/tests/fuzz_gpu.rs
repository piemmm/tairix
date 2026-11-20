//! Deterministic fuzz harness for the submission model, run on the software
//! device against references that state its rules again.
//!
//! A submission's commands carry objects, offsets, sizes, counts and records a
//! client chooses. Three sweeps draw them:
//!
//! - `buffers`: copies, fills and updates over buffers sharing allocations,
//!   ranges inside, across and past what they name, held to a byte array.
//! - `images`: images of every shape a description may take, refused or made
//!   as the device's rules say, and the copies between them and buffers and
//!   their clears, ranges inside, across and past every level, layer and
//!   texel, held to a texel reference.
//! - `work`: bind sets made over ranges and views both fit and not,
//!   dispatches and render passes, direct and indirect, their draws direct,
//!   indexed and indirect, each sound or carrying faults drawn from a stated
//!   catalogue, run on a device of one participant and its twin of several,
//!   which draws each indirect record as the direct draw it names.
//!
//! The invariants are:
//!
//! 1. Nothing panics, whatever is asked.
//! 2. A request the reference refuses is refused, naming the same command and
//!    a reason the reference gives, and changes no byte anywhere.
//! 3. A request the reference admits is admitted and completes, or loses its
//!    context where the reference says it does, and leaves every byte the
//!    reference knows as the reference does; what it may write but the
//!    reference cannot foresee — what a pass draws — it writes nowhere else.
//! 4. Two devices running the same work, one of them indirect records as
//!    direct draws on several participants, leave every byte alike.
//!
//! Each sweep runs a fixed smoke sweep under plain `cargo test`, and keeps
//! drawing from its seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses
//! under `cargo xtask fuzz`.

#[path = "fuzz_gpu/buffers.rs"]
mod buffers;
#[path = "fuzz_gpu/images.rs"]
mod images;
#[path = "fuzz_gpu/shaders.rs"]
mod shaders;
#[path = "fuzz_gpu/work.rs"]
mod work;

use std::sync::Arc;

use tairix_abi::time::MonotonicClock;
use tairix_fuzzseed::Prng;
use tairix_gpu::{Device, NoThreads, SoftwareDesc, DEFAULT_SUBGROUP_SIZE};

/// A clock that never moves: nothing here overruns its deadline.
struct Still;

impl MonotonicClock for Still {
    fn now_ns(&self) -> u64 {
        0
    }
}

/// A software device of 64 MiB of memory sharing its work across `runner`.
fn device(runner: Arc<dyn tairix_parallel::JobRunner + Send>) -> Device {
    Device::software(&SoftwareDesc {
        memory_bytes: 64 << 20,
        clock: Arc::new(Still),
        runner,
        threads: &NoThreads,
        subgroup_size: DEFAULT_SUBGROUP_SIZE,
    })
    .expect("a software device")
}

/// A value below `n`.
fn below(rng: &mut Prng, n: u32) -> u32 {
    u32::try_from(rng.below(n as usize)).expect("below a u32")
}

/// An offset the reference has proved within one of its memories.
fn index_of(value: u64) -> usize {
    usize::try_from(value).expect("within a small memory")
}

/// An offset or size drawn to land inside, at the edge of, or past `limit`.
fn draw_extent(rng: &mut Prng, limit: u64) -> u64 {
    match rng.below(8) {
        0 => 0,
        1 => limit,
        2 => limit + 4,
        3 => u64::MAX - rng.below(8) as u64,
        _ => rng.below(usize::try_from(limit).unwrap_or(0) + 1) as u64,
    }
}

/// [`draw_extent`], whole words three times in four.
fn draw_word_extent(rng: &mut Prng, limit: u64) -> u64 {
    let raw = draw_extent(rng, limit);
    if rng.below(4) == 0 {
        raw
    } else {
        raw & !3
    }
}

/// Run `round` with `test`'s seeded stream, `rounds` at a time, until the
/// budget elapses, or once where there is none.
fn sweep(test: &str, rounds: u32, mut round: impl FnMut(&mut Prng)) {
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

#[test]
fn submissions_agree_with_a_byte_array_reference() {
    sweep(
        "submissions_agree_with_a_byte_array_reference",
        buffers::ROUNDS,
        buffers::run_round,
    );
}

#[test]
fn images_and_their_transfers_agree_with_a_texel_reference() {
    sweep(
        "images_and_their_transfers_agree_with_a_texel_reference",
        images::ROUNDS,
        images::run_round,
    );
}

#[test]
fn passes_draws_and_dispatches_agree_with_their_rules_and_their_twins() {
    let mut worlds = work::Worlds::new();
    sweep(
        "passes_draws_and_dispatches_agree_with_their_rules_and_their_twins",
        work::ROUNDS,
        |rng| worlds.run_round(rng),
    );
    worlds.exercised();
}
