//! Deterministic fuzz harness for the translator, which turns untrusted guest
//! code into code the host runs. For any code in any machine state, on every
//! model, the block there is translated and run through the code
//! generator's interpreter and the x86_64, aarch64 and riscv64 backends'
//! machine models, with and without the extensions each lowers to, and each
//! leaves the processor, memory, pending waits and DRAM refresh exactly as
//! the CPU interpreter stepping to the same cycle does, calling the same
//! helpers with the same arguments.
//!
//! Runs a fixed smoke sweep under plain `cargo test`; keeps drawing from the
//! same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

use tairix_fuzzseed::Prng;
use tairix_pcemu::cpu::jit::differential::{self, Case, Seen};

/// Machines drawn per pass.
const SMOKE_ITERATIONS: usize = 400;

#[test]
fn every_backend_runs_a_block_as_the_interpreter_does() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "every_backend_runs_a_block_as_the_interpreter_does",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let pattern = differential::pattern();
    let mut seen = Seen::default();
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            let case = Case::random(&mut || rng.next_u64(), &pattern);
            let _ = differential::agree(&case, &mut seen);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
    // A third of the machines drawn begin a block, so none in a pass is not
    // chance but a harness that draws nothing a block begins with.
    assert!(seen.modelled > 0, "no block ran on a model");
}
