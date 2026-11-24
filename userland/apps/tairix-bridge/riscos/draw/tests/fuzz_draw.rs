//! Deterministic fuzz harness for the Draw module: every SWI, and `DrawV`
//! called directly, with hostile registers over application space salted
//! with paths — well formed, their codes, orders and continuations broken,
//! points at the ends of the coordinate range — cap and join blocks, dash
//! patterns of any count and length, matrices and output buffers, in
//! screens of every depth. No word reaches the kernel's own memory: Draw
//! writes where it is pointed, as RISC OS's does, and a write there would
//! break the machine rather than the module.
//!
//! The invariants, checked after every call:
//!
//! * nothing panics, and every refusal is a value, a call the host stops
//!   after a few of its words among them;
//! * every error block a call returns can be read back;
//! * the drivers still answer a mode variable afterwards;
//! * the module, re-initialised after a run, holds none of the RMA it took.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz` extends it to a wall-clock
//! budget.

#[path = "../../vdu/tests/common/mod.rs"]
mod common;

use std::sync::LazyLock;

use common::{
    answers, command, drivers_with, rma_used, select_depth, KernelMemory, HOSTILE_CALL_WORDS,
};
use tairix_drawing::riscos::format::{element_points, CONTINUATION, TAG};
use tairix_fuzzseed::Prng;
use tairix_riscos_draw::fixture;
use tairix_riscos_filing::session::{modules_with, Session};
use tairix_riscos_kernel::memory::layout::APPLICATION;
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::{Entry, Fail, Regs};
use tairix_riscos_kernel::swi::chunk::DRAW;
use tairix_riscos_kernel::swi::X;
use tairix_riscos_kernel::vectors;

const SMOKE_RUNS: u64 = 4;
const CALLS: usize = 200;
/// The application space salted with blocks hostile calls read.
const SALTED: u32 = 0x8000;
const BLOCK: u32 = 512;
/// `Draw_ProcessPath`'s R7 bit that has it write a box where the rest points.
const BOX: u32 = 1 << 31;

static MODULES: LazyLock<Vec<&'static Builtin>> =
    LazyLock::new(|| modules_with(&[&tairix_riscos_vdu::MODULES, &tairix_riscos_draw::MODULES]));

/// A coordinate: mostly small, now and then at a range's end or anything.
fn coordinate(rng: &mut Prng) -> u32 {
    let small = rng.next_u32() % 60_000;
    if rng.below(10) == 0 {
        let anything = rng.next_u32();
        return *rng.pick(&[i32::MAX.cast_unsigned(), i32::MIN.cast_unsigned(), anything]);
    }
    *rng.pick(&[small, small.wrapping_neg()])
}

/// A path's words: elements of each code, most in order, now and then a
/// code past the last, an element out of order, or a continuation.
fn path(rng: &mut Prng, base: u32) -> Vec<u32> {
    let mut words = Vec::new();
    let count = 1 + rng.below(12);
    for i in 0..count {
        let code = if i == 0 {
            2
        } else {
            *rng.pick(&[2, 3, 4, 5, 6, 7, 8, 8, 8, 1, 9, 0x123_4508])
        };
        words.push(code);
        let points = match code & TAG {
            CONTINUATION => {
                let into = base + 4 * u32::try_from(rng.below(16)).unwrap_or(0);
                let target = *rng.pick(&[base, into, APPLICATION, 0]);
                words.push(target);
                0
            }
            tag => element_points(tag).unwrap_or(0),
        };
        for _ in 0..2 * points {
            words.push(coordinate(rng));
        }
    }
    words.extend([0, rng.next_u32() % 4096]);
    words
}

/// A session with the drivers and Draw, its application space salted with
/// paths, cap and join blocks, dash patterns, matrices, buffers and noise,
/// and its kernel's own memory, which no salted word reaches.
fn boot(rng: &mut Prng) -> (Session, KernelMemory) {
    let mut session = drivers_with(MODULES.as_slice(), b"");
    let kernel = KernelMemory::of(&mut session);
    let mut words: Vec<u32> = (0..SALTED / 4).map(|_| rng.next_u32()).collect();
    for (n, chunk) in words.chunks_mut(BLOCK as usize / 4).enumerate() {
        let base = APPLICATION + BLOCK * u32::try_from(n).unwrap_or(0);
        let fill: Vec<u32> = match rng.below(6) {
            0 | 1 => path(rng, base),
            2 => {
                let kinds = rng.next_u32() & 0x03_0303;
                let mitre = *rng.pick(&[0, 0xA_0000, u32::MAX, 0x8000]);
                vec![kinds, mitre, rng.next_u32(), rng.next_u32()]
            }
            3 => {
                let count = *rng.pick(&[0, 1, 2, 3, 7, u32::MAX]);
                let mut dash = vec![rng.next_u32() % 20_000, count];
                dash.extend((0..8).map(|_| *rng.pick(&[0, 4000, 9000, u32::MAX])));
                dash
            }
            4 => vec![0, *rng.pick(&[0, 8, 24, 200, u32::MAX])],
            _ => (0..6)
                .map(|_| {
                    let anything = rng.next_u32();
                    *rng.pick(&[0, 0x1_0000, 0x8000, u32::MAX, anything])
                })
                .collect(),
        };
        for (slot, word) in chunk.iter_mut().zip(fill) {
            *slot = word;
        }
    }
    for word in &mut words {
        if reaches(&kernel, *word) {
            *word = hostile(rng, &kernel);
        }
    }
    let bytes = fixture::words(&words);
    let _ = session.kernel.os().write_bytes(APPLICATION, &bytes);
    (session, kernel)
}

/// Whether a call handed `word` could write into the kernel's own memory,
/// there as RISC OS's Draw would: where it points, or with its box bit
/// dropped.
fn reaches(kernel: &KernelMemory, word: u32) -> bool {
    kernel.holds(word) || kernel.holds(word & !BOX)
}

/// A hostile register: a block, a pointer anywhere in the salted space, a
/// flag combination, or anything that does not reach the kernel's memory.
fn hostile(rng: &mut Prng, kernel: &KernelMemory) -> u32 {
    loop {
        let block =
            APPLICATION + BLOCK * u32::try_from(rng.below((SALTED / BLOCK) as usize)).unwrap_or(0);
        let into = APPLICATION + u32::try_from(rng.below(SALTED as usize)).unwrap_or(0);
        let flags =
            (rng.next_u32() & 0x7C00_00FF) | *rng.pick(&[0, 1 << 25, 1 << 26, 3 << 25, BOX]);
        let anything = rng.next_u32();
        let value = *rng.pick(&[
            0,
            1,
            2,
            3,
            block,
            block,
            into,
            flags,
            flags,
            u32::MAX,
            BOX | block,
            anything,
        ]);
        if !reaches(kernel, value) {
            return value;
        }
    }
}

fn call(session: &mut Session, rng: &mut Prng, kernel: &KernelMemory) {
    session.allow_words(Some(HOSTILE_CALL_WORDS));
    if rng.below(16) == 0 {
        let _ = select_depth(session, u32::try_from(rng.below(6)).unwrap_or(0));
        return;
    }
    let mut regs = Regs::new([0; 13]);
    for register in &mut regs.r[..8] {
        *register = hostile(rng, kernel);
    }
    if rng.below(5) < 3 {
        regs.r[0] =
            APPLICATION + BLOCK * u32::try_from(rng.below((SALTED / BLOCK) as usize)).unwrap_or(0);
    }
    let result = if rng.below(8) == 0 {
        regs.r[8] = u32::try_from(rng.below(20)).unwrap_or(0);
        let mut os = session.kernel.os();
        match os.call_vector(vectors::DRAW, &mut regs, Entry::Supervisor) {
            Ok(()) if regs.flag(tairix_riscos_arm::psr::V) => Err(Fail::Error(regs.r[0])),
            Ok(()) => Ok(()),
            Err(unwind) => Err(Fail::Unwind(unwind)),
        }
    } else {
        let swi = DRAW + u32::try_from(rng.below(18)).unwrap_or(0);
        session.kernel.os().perform(X | swi, &mut regs)
    };
    if let Err(Fail::Error(block)) = result {
        assert!(
            session.kernel.os().read_word(block).is_ok(),
            "the error block at {block:#x} can be read"
        );
    }
}

/// A sound path counted and filled: the calls reach the module.
fn reaches_draw(session: &mut Session) {
    let path = [2u32, 0, 0, 8, 9000, 0, 8, 9000, 9000, 5, 0, 64];
    let at = APPLICATION + SALTED;
    let bytes = fixture::words(&path);
    session
        .kernel
        .os()
        .write_bytes(at, &bytes)
        .expect("written");
    let mut count = Regs::new([at, 0, 0, 0, 0, 0, 0, 3, 0, 0, 0, 0, 0]);
    session
        .kernel
        .os()
        .perform(X | DRAW, &mut count)
        .expect("counted");
    assert_eq!(count.r[0], 48);
    let mut fill = Regs::new([at, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    session
        .kernel
        .os()
        .perform(X | (DRAW + 2), &mut fill)
        .expect("filled");
}

#[test]
fn hostile_draw_calls_leave_the_drivers_answering() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_draw_calls_leave_the_drivers_answering",
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
        reaches_draw(&mut session);
        for step in 0..CALLS {
            call(&mut session, &mut rng, &kernel);
            assert!(
                answers(&mut session),
                "OS_ReadModeVariable after step {step}"
            );
        }
        session.allow_words(None);
        command(&mut session, "RMReInit Draw").expect("reinitialised");
        assert_eq!(rma_used(&mut session), held, "all it took given back");
        runs += 1;
    }
}

/// No register or salted word the harness makes reaches the kernel's own
/// memory, as an output buffer at the RMA's first block once did: Draw wrote
/// over the heap there, as RISC OS's would, and no claim could be freed.
#[test]
fn hostile_words_stay_out_of_the_kernels_memory() {
    let mut rng = Prng::new(0x2000_0011);
    let (mut session, kernel) = boot(&mut rng);
    assert!(reaches(&kernel, 0x2000_0011) && reaches(&kernel, BOX | 0x2000_0011));
    let salted = session
        .kernel
        .os()
        .read_vec(APPLICATION, SALTED as usize)
        .expect("salted");
    for word in salted.as_chunks::<4>().0 {
        assert!(!reaches(&kernel, u32::from_le_bytes(*word)));
    }
    for _ in 0..100_000 {
        assert!(!reaches(&kernel, hostile(&mut rng, &kernel)));
    }
}
