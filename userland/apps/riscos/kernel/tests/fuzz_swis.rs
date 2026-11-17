//! Deterministic fuzz harness for the kernel's SWIs: random SWIs performed
//! with hostile registers — pointers into application space, at the top of
//! memory, nowhere, misaligned — and random guest code salted with SWIs
//! run from User mode, under a host that stops endless work.
//!
//! The invariants, checked after every call:
//!
//! * nothing panics, and every refusal is a value;
//! * nothing runs for ever: the host's word stops it;
//! * every error the kernel raises is a block it can read back;
//! * the kernel still answers a SWI afterwards.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

use std::cell::Cell;
use std::rc::Rc;

#[path = "support/boot.rs"]
mod boot;

use boot::APPLICATION_SIZE;

use tairix_fuzzseed::Prng;
use tairix_riscos_kernel::input::InputEvent;
use tairix_riscos_kernel::memory::layout::APPLICATION;
use tairix_riscos_kernel::os::{Fail, Host, Kernel, Pulse, Regs};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_RUNS: u64 = 40;
/// SWIs performed in one run.
const CALLS: usize = 120;
/// Where a run's guest code is.
const CODE: u32 = APPLICATION + 0x8000;
/// The words of it.
const CODE_WORDS: u32 = 256;
/// The instructions a run of guest code is given.
const BUDGET: u64 = 20_000;
/// `OS_SWI` X bit.
const X: u32 = 0x2_0000;
/// The host's words a call is given before it is stopped, each after a
/// slice of work.
const WORDS: u32 = 0;

/// A host whose clock moves on as it is asked, with input now and then, and
/// which stops work after a few of its words.
struct Hostile {
    clock: Rc<Cell<u64>>,
    pulses: Rc<Cell<u32>>,
    keys: Rc<Cell<u32>>,
}

impl Host for Hostile {
    fn monotonic(&mut self) -> u64 {
        self.clock.get()
    }

    fn real_time(&mut self) -> u64 {
        self.clock.get()
    }

    fn pulse(&mut self) -> Pulse {
        match self.pulses.get() {
            0 => Pulse::Stop,
            left => {
                self.pulses.set(left - 1);
                Pulse::Continue
            }
        }
    }

    fn input(&mut self) -> Option<InputEvent> {
        let keys = self.keys.get();
        (keys != 0).then(|| {
            self.keys.set(keys - 1);
            InputEvent::Key(keys.to_le_bytes()[0])
        })
    }

    fn wait(&mut self, until: Option<u64>) -> Pulse {
        if let Some(until) = until {
            self.clock.set(self.clock.get().max(until));
        }
        self.clock.set(self.clock.get() + 1);
        self.pulse()
    }
}

/// A run's kernel and its host's handles.
struct Run {
    kernel: Kernel,
    pulses: Rc<Cell<u32>>,
    keys: Rc<Cell<u32>>,
}

fn boot(rng: &mut Prng) -> Run {
    let clock = Rc::new(Cell::new(0));
    let pulses = Rc::new(Cell::new(WORDS));
    let keys = Rc::new(Cell::new(0));
    let host = Box::new(Hostile {
        clock: Rc::clone(&clock),
        pulses: Rc::clone(&pulses),
        keys: Rc::clone(&keys),
    });
    let kernel = boot::boot_with(rng, host);
    let mut run = Run {
        kernel,
        pulses,
        keys,
    };
    salt(&mut run, rng);
    run
}

/// Fills application space with what hostile SWIs read: strings, words,
/// pointers, and guest code salted with SWIs.
fn salt(run: &mut Run, rng: &mut Prng) {
    let mut os = run.kernel.os();
    let mut bytes = vec![0u8; CODE as usize - APPLICATION as usize];
    rng.fill(&mut bytes);
    for chunk in bytes.chunks_mut(64) {
        if rng.below(3) == 0 {
            let text: &[u8] = rng.pick(&[
                &b"Corpus$Var\0"[..],
                b"OS_ReadMonotonicTime\0",
                b"<Alias$.>|M|J\0",
                b"1+2*(3\0",
                b"name/A,size/N\0",
                b"&FF\0",
                b"F*\0",
            ]);
            let length = text.len().min(chunk.len());
            chunk[..length].copy_from_slice(&text[..length]);
        }
    }
    let _ = os.write_bytes(APPLICATION, &bytes);
    for i in 0..CODE_WORDS {
        let word = match rng.below(8) {
            0 => 0xEF00_0000 | X | swi_number(rng),
            1 => 0xE3A0_0000 | (rng.next_u32() & 0xF_FFFF),
            2 => 0xE59F_0000 | (rng.next_u32() & 0xF_FFFF),
            _ => rng.next_u32(),
        };
        let _ = os.write_word(CODE + 4 * i, word);
    }
}

/// A SWI number: mostly the kernel's own, sometimes past them.
fn swi_number(rng: &mut Prng) -> u32 {
    match rng.below(10) {
        0..=5 => u32::try_from(rng.below(0x80)).unwrap_or(0),
        6 => 0xC0 + u32::try_from(rng.below(0x2E)).unwrap_or(0),
        7 => 0x100 + u32::try_from(rng.below(0x100)).unwrap_or(0),
        8 => u32::try_from(rng.below(0x200)).unwrap_or(0),
        _ => rng.next_u32() & 0xF_FFFF,
    }
}

/// A hostile register value.
fn hostile(rng: &mut Prng) -> u32 {
    let in_application =
        APPLICATION + u32::try_from(rng.below(APPLICATION_SIZE as usize)).unwrap_or(0);
    let small = u32::try_from(rng.below(300)).unwrap_or(0);
    let near_top = 0xFFFF_FFF0 + u32::try_from(rng.below(16)).unwrap_or(0);
    let anything = rng.next_u32();
    *rng.pick(&[
        0,
        1,
        small,
        u32::MAX,
        0x7FFF_FFFF,
        0x8000_0000,
        in_application,
        in_application & !3,
        in_application | 3,
        0xFFFF_FFFC,
        near_top,
        0x7000_0000,
        0x4000,
        anything,
    ])
}

/// Performs one hostile SWI, checking what it left.
fn perform(run: &mut Run, rng: &mut Prng) {
    let number = swi_number(rng);
    let mut regs = Regs::new([0; 13]);
    for register in &mut regs.r {
        *register = hostile(rng);
    }
    run.pulses.set(WORDS);
    if rng.below(8) == 0 {
        run.keys.set(u32::try_from(rng.below(40)).unwrap_or(0));
    }
    let result = run.kernel.os().perform(X | number, &mut regs);
    if let Err(Fail::Error(block)) = result {
        // Every block reads, whether the kernel's or a pointer a guest gave.
        let _ = run.kernel.os().read_word(block);
    }
}

/// Runs the salted code from User mode, entered as the exit handler.
fn run_code(run: &mut Run, rng: &mut Prng) {
    let entry = CODE + 4 * u32::try_from(rng.below(CODE_WORDS as usize)).unwrap_or(0);
    let mut change = Regs::new([11, entry, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let _ = run.kernel.os().perform(X | 0x40, &mut change);
    let mut exit = Regs::new([0, 0x5845_4241, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let _ = run.kernel.os().perform(0x11, &mut exit);
    run.pulses.set(WORDS);
    let _ = run.kernel.run(BUDGET);
    let mut restore = Regs::new([11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let mut read = Regs::new([11, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    if run.kernel.os().perform(X | 0x55, &mut read).is_ok() {
        restore.r[1] = read.r[1];
        let _ = run.kernel.os().perform(X | 0x40, &mut restore);
    }
}

/// The kernel still answers.
fn still_answers(run: &mut Run, step: usize) {
    let mut regs = Regs::new([0; 13]);
    run.pulses.set(WORDS);
    assert!(
        run.kernel.os().perform(X | 0x42, &mut regs).is_ok(),
        "OS_ReadMonotonicTime after step {step}"
    );
}

#[test]
fn any_swi_with_any_registers_answers_as_a_value() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_swi_with_any_registers_answers_as_a_value",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        let mut run = boot(&mut rng);
        for step in 0..CALLS {
            if rng.below(12) == 0 {
                run_code(&mut run, &mut rng);
            } else {
                perform(&mut run, &mut rng);
            }
            still_answers(&mut run, step);
        }
        runs += 1;
    }
}
