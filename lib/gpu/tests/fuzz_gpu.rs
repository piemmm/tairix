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
//!   texel, held to a texel reference — half the rounds on a copy engine of a
//!   drawn granularity, its family's rules restated.
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
#[path = "fuzz_gpu/common.rs"]
mod common;
#[path = "fuzz_gpu/images.rs"]
mod images;
#[path = "fuzz_gpu/shaders.rs"]
mod shaders;
#[path = "fuzz_gpu/work.rs"]
mod work;

use std::sync::Arc;

use common::{below, draw_word_extent, index_of, sweep, MEMORY};
use tairix_abi::time::MonotonicClock;
use tairix_gpu::{Device, NoThreads, QueueFamily, SoftwareDesc, DEFAULT_SUBGROUP_SIZE};

/// A clock that never moves: nothing here overruns its deadline.
struct Still;

impl MonotonicClock for Still {
    fn now_ns(&self) -> u64 {
        0
    }
}

/// A software device of [`MEMORY`] sharing its work across `runner`,
/// stating `second` beside its own family where given.
fn software(
    runner: Arc<dyn tairix_parallel::JobRunner + Send>,
    second: Option<QueueFamily>,
) -> Device {
    match second {
        None => Device::software(&SoftwareDesc {
            memory_bytes: MEMORY,
            clock: Arc::new(Still),
            runner,
            threads: &NoThreads,
            subgroup_size: DEFAULT_SUBGROUP_SIZE,
        }),
        Some(_) => tairix_gpu::testing::software(MEMORY, runner, second),
    }
    .expect("a software device")
}

#[test]
fn submissions_agree_with_a_byte_array_reference() {
    sweep(
        "submissions_agree_with_a_byte_array_reference",
        buffers::ROUNDS,
        |rng| buffers::run_round(rng, &software),
    );
}

#[test]
fn images_and_their_transfers_agree_with_a_texel_reference() {
    sweep(
        "images_and_their_transfers_agree_with_a_texel_reference",
        images::ROUNDS,
        |rng| images::run_round(rng, &software),
    );
}

#[test]
fn passes_draws_and_dispatches_agree_with_their_rules_and_their_twins() {
    let mut worlds = work::Worlds::new(&software);
    sweep(
        "passes_draws_and_dispatches_agree_with_their_rules_and_their_twins",
        work::ROUNDS,
        |rng| worlds.run_round(rng),
    );
    worlds.exercised();
}
