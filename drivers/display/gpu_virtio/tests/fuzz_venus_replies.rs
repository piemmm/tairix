//! Deterministic fuzz harness for the virtio-gpu driver's reading of the
//! Venus replies the host's renderer writes — bytes it trusts no further than
//! it checks them: a result, an object made, the physical devices, a
//! device's properties, queue families and memory, a buffer's requirements,
//! the command buffers allocated, and the statement narrowed from them.
//!
//! Held to: no input panics; a reply built from drawn values reads back
//! exactly those values and is refused cut short; and a statement narrowed
//! from a device's replies is one the seam takes.
//!
//! A plain `cargo test` runs the [`SMOKE_ITERATIONS`] sweep once from a fresh,
//! logged seed; `cargo xtask fuzz` extends it to a wall-clock budget.

use tairix_drv_display_gpu_virtio::testing::{read_every_reply, replies};
use tairix_fuzzseed::Prng;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
///
/// Interpreted, it only samples the reader's paths: it carries no `unsafe`,
/// and a few rounds reach every one.
const SMOKE_ITERATIONS: u64 = if cfg!(miri) { 8 } else { 2_000 };

/// The longest noise a round reads: past the longest reply, a device's
/// properties.
const NOISE_MAX: usize = 1024;

#[test]
fn venus_replies_are_read_fail_closed() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "venus_replies_are_read_fail_closed",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            let noise_len = rng.at_most(NOISE_MAX);
            read_every_reply(&rng.noise(noise_len));
            let made = replies(&mut || rng.next_u64());
            let source = rng.pick(&made);
            read_every_reply(&rng.mutated(source));
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}
