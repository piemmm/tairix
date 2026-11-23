//! Deterministic fuzz harness for ColourTrans, `InverseTable` and `BlendTable`:
//! every SWI with hostile registers — palettes, calibration tables, sprite
//! areas and buffers pointing anywhere, sizes asked and translucencies past
//! any range — in screens of every depth, and `*ColourTransMap`,
//! `*ColourTransMapSize` and `*ColourTransLoadings` with hostile numbers.
//! No pointer reaches the kernel's own memory, where a table written would
//! break the machine rather than the modules.
//!
//! The invariants, checked after every call:
//!
//! * nothing panics, and every refusal is a value;
//! * every error block a call returns can be read back;
//! * the drivers still answer a mode variable afterwards;
//! * the three modules, re-initialised after a run, hold none of the RMA they
//!   took during it.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz` extends it to a wall-clock
//! budget.

mod common;

use common::{answers, command, drivers, rma_used, select_depth, KernelMemory};
use tairix_fuzzseed::Prng;
use tairix_riscos_filing::session::Session;
use tairix_riscos_kernel::memory::layout::APPLICATION;
use tairix_riscos_kernel::os::{Fail, Regs};
use tairix_riscos_kernel::swi::chunk::{BLEND_TABLE, COLOUR_TRANS, INVERSE_TABLE};
use tairix_riscos_kernel::swi::X;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_RUNS: u64 = 6;
/// Calls made in one run.
const CALLS: usize = 300;
/// The application space salted with blocks hostile calls read.
const SALTED: u32 = 0x4000;
/// The SWIs of ColourTrans, `InverseTable` and `BlendTable`, each counted from
/// its chunk's start.
const SWIS: [(u32, u32); 3] = [(COLOUR_TRANS, 36), (INVERSE_TABLE, 2), (BLEND_TABLE, 2)];

/// A session with the drivers and their modules, its application space
/// salted with palettes, calibration tables and noise, and its kernel's own
/// memory, which no salted word points into.
fn boot(rng: &mut Prng) -> (Session, KernelMemory) {
    let mut session = drivers(b"");
    let kernel = KernelMemory::of(&mut session);
    let mut bytes = vec![0u8; SALTED as usize];
    rng.fill(&mut bytes);
    for chunk in bytes.chunks_mut(256) {
        match rng.below(4) {
            0 => {
                let counts = [2u32, 2, 2].map(|n| n + u32::try_from(rng.below(3)).unwrap_or(0));
                let mut words = counts.to_vec();
                for &count in &counts {
                    words.extend(
                        (0..count).map(|i| (255 * i / (count - 1)) | (rng.next_u32() << 8)),
                    );
                }
                for (slot, word) in chunk.as_chunks_mut::<4>().0.iter_mut().zip(words) {
                    *slot = word.to_le_bytes();
                }
            }
            1 => {
                for slot in chunk.as_chunks_mut::<4>().0 {
                    *slot = (rng.next_u32() & 0xFFFF_FF00).to_le_bytes();
                }
            }
            _ => {}
        }
    }
    for slot in bytes.as_chunks_mut::<4>().0 {
        if kernel.holds(u32::from_le_bytes(*slot)) {
            *slot = hostile(rng, &kernel).to_le_bytes();
        }
    }
    let _ = session.kernel.os().write_bytes(APPLICATION, &bytes);
    (session, kernel)
}

/// A hostile register value: small numbers, flags, translucencies past any
/// range, pointers into the salted space, at the start of one of its blocks
/// or nowhere, but never into the kernel's own memory.
fn hostile(rng: &mut Prng, kernel: &KernelMemory) -> u32 {
    loop {
        let into = APPLICATION + u32::try_from(rng.below(SALTED as usize)).unwrap_or(0);
        let block =
            APPLICATION + 256 * u32::try_from(rng.below(SALTED as usize / 256)).unwrap_or(0);
        let small = u32::try_from(rng.below(300)).unwrap_or(0);
        let anything = rng.next_u32();
        let value = *rng.pick(&[
            0,
            1,
            small,
            256,
            257,
            u32::MAX,
            0x8000_0000,
            into,
            into | 3,
            block,
            0xFFFF_FFFC,
            anything,
        ]);
        if !kernel.holds(value) {
            return value;
        }
    }
}

/// A command with hostile numbers: the calibration's sizes, its pairs or the
/// loadings.
fn hostile_command(rng: &mut Prng) -> String {
    let number = |rng: &mut Prng| {
        let picked = *rng.pick(&[0u32, 1, 2, 3, 100, 256, 257, u32::MAX]);
        if rng.below(2) == 0 {
            format!("{picked}")
        } else {
            format!("&{:X}", rng.next_u32())
        }
    };
    let (name, count) = match rng.below(3) {
        0 => ("ColourTransMapSize", 3),
        1 => ("ColourTransMap", 1 + rng.below(20)),
        _ => ("ColourTransLoadings", 3),
    };
    let mut text = String::from(name);
    for _ in 0..count {
        text.push(' ');
        text.push_str(&number(rng));
    }
    text
}

/// One hostile call, its outcome a value.
fn call(session: &mut Session, rng: &mut Prng, kernel: &KernelMemory) {
    let result = match rng.below(8) {
        0 => command(session, &hostile_command(rng)),
        1 => select_depth(session, u32::try_from(rng.below(6)).unwrap_or(0)),
        _ => {
            let mut regs = Regs::new([0; 13]);
            for register in &mut regs.r {
                *register = hostile(rng, kernel);
            }
            let &(chunk, count) = rng.pick(&SWIS);
            let swi = chunk + u32::try_from(rng.below(count as usize)).unwrap_or(0);
            session.kernel.os().perform(X | swi, &mut regs)
        }
    };
    if let Err(Fail::Error(block)) = result {
        assert!(
            session.kernel.os().read_word(block).is_ok(),
            "the error block at {block:#x} can be read"
        );
    }
}

#[test]
fn hostile_colour_calls_leave_the_drivers_answering() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_colour_calls_leave_the_drivers_answering",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        let (mut session, kernel) = boot(&mut rng);
        let held = rma_used(&mut session);
        for step in 0..CALLS {
            call(&mut session, &mut rng, &kernel);
            assert!(
                answers(&mut session),
                "OS_ReadModeVariable after step {step}"
            );
        }
        for module in ["ColourTrans", "InverseTable", "BlendTable"] {
            command(&mut session, &format!("RMReInit {module}")).expect("reinitialised");
        }
        assert_eq!(rma_used(&mut session), held, "all they took given back");
        runs += 1;
    }
}
