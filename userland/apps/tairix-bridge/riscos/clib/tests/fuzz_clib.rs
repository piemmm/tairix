//! Deterministic fuzz harness for `SharedCLibrary`: its SWIs with hostile
//! stub descriptions — chunks it holds and not, vectors and statics of any
//! size and place, lists without an end — workspaces and stack requests;
//! then guest code calling each entry of every chunk through the vectors a
//! sound `LibInit` wrote, with hostile registers: SL above a real stack
//! chunk or anywhere, SP and pointers in the application space or past it,
//! each call given a few of the host's words.
//!
//! The invariants, checked after every call:
//!
//! * nothing panics, and every refusal is a value, a guest run stopped at
//!   its budget among them;
//! * the kernel still answers a SWI afterwards;
//! * the library, re-initialised after a run, holds none of the RMA it took,
//!   unless the guest wrote over the RMA's own structure, as a program may on
//!   RISC OS.

use std::ops::Range;
use std::sync::LazyLock;

use tairix_fuzzseed::Prng;
use tairix_riscos_filing::session::{command, modules_with, session, Session, Setup};
use tairix_riscos_kernel::heap::HeapError;
use tairix_riscos_kernel::memory::layout::APPLICATION;
use tairix_riscos_kernel::memory::Bits;
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::{Fail, Regs};
use tairix_riscos_kernel::swi::chunk::SHARED_C_LIBRARY;
use tairix_riscos_kernel::swi::os::{EXIT, MODULE, READ_DYNAMIC_AREA, READ_MONOTONIC_TIME};
use tairix_riscos_kernel::swi::X;

const SMOKE_RUNS: u64 = 4;
const CALLS: usize = 60;
/// The application slot's bytes, and where in it each thing lies.
const SLOT: u32 = 4 << 20;
const DESCRIPTIONS: u32 = APPLICATION + 0x1000;
const VECTOR: u32 = APPLICATION + 0x2000;
const STATICS: u32 = APPLICATION + 0x5000;
const CODE: u32 = APPLICATION + 0x6000;
const WORKSPACE: u32 = APPLICATION + 0x1_0000;
const END: u32 = APPLICATION + SLOT;
/// The library kernel's statics, and the C library's after them.
const KERNEL_STATICS: u32 = 0x31C;
const C_STATICS: u32 = 0xB48;
/// The chunks a sound client asks for, as RISC OS 5.30's stubs do: each
/// one's id, entries and statics' bytes, its vector after the one before's.
const CHUNKS: [(u32, u32, u32); 8] = [
    (1, 48, KERNEL_STATICS),
    (2, 187, C_STATICS),
    (4, 50, 0),
    (5, 189, 0),
    (6, 40, 0),
    (7, 63, 0),
    (8, 50, 0),
    (9, 25, 0),
];
/// The instructions a guest call may run for.
const BUDGET: u64 = 200_000;
/// The host's words a hostile call is given, each after a slice of work: a
/// procedure the library calls back may never return, as no host lets it.
const WORDS: u32 = 8;
/// The library's own message file, which it must close as it dies.
const LIBRARY_MESSAGES: &[u8] = b"Resources:$.Resources.CLib.Messages";

static MODULES: LazyLock<Vec<&'static Builtin>> =
    LazyLock::new(|| modules_with(&[&tairix_riscos_clib::MODULES]));

fn fresh() -> Session {
    session(&Setup {
        vdu: None,
        modules: MODULES.as_slice(),
        bytes: SLOT,
        keys: &[],
        disc: None,
    })
}

fn call(session: &mut Session, swi: u32, r: &[u32]) -> Result<Regs, Fail> {
    let mut regs = Regs::default();
    regs.r[..r.len()].copy_from_slice(r);
    session.kernel.os().perform(X | swi, &mut regs)?;
    Ok(regs)
}

fn answers(session: &mut Session) -> bool {
    call(session, READ_MONOTONIC_TIME, &[]).is_ok()
}

/// The RMA in use but for what MessageTrans holds for other modules' files:
/// a module keeps the file it opens at its first error, which the library's
/// calls may be the first to raise.
fn rma_used(session: &mut Session) -> u32 {
    let free = call(session, MODULE, &[5]).expect("described").r[3];
    let used = call(session, READ_DYNAMIC_AREA, &[1]).expect("the RMA").r[1] - free;
    let files = session.kernel.os().message_files().expect("the files");
    let others: u32 = files
        .iter()
        .filter(|(name, _)| name.as_slice() != LIBRARY_MESSAGES)
        .map(|&(_, bytes)| bytes)
        .sum();
    used - others
}

/// A number below `n`.
fn under(rng: &mut Prng, n: u32) -> u32 {
    u32::try_from(rng.below(n as usize)).unwrap_or(0)
}

/// The kernel's heaps, the system heap's area and the RMA's at their most:
/// a guest writing on them breaks the system its calls are held to, as one
/// may on RISC OS, so a raw word is kept out of them.
fn kernel_heaps(session: &mut Session) -> [Range<u32>; 2] {
    [0x80, 0x81].map(|asked| {
        let area = call(session, READ_DYNAMIC_AREA, &[asked]).expect("a kernel heap");
        area.r[0]..area.r[0].saturating_add(area.r[2])
    })
}

/// A raw word, none in `heaps`.
fn wild(rng: &mut Prng, heaps: &[Range<u32>]) -> u32 {
    loop {
        let word = rng.next_u32();
        if !heaps.iter().any(|heap| heap.contains(&word)) {
            return word;
        }
    }
}

/// An address a hostile register holds: in the application space, at its
/// edges, or past it.
fn pointer(rng: &mut Prng) -> u32 {
    match rng.below(6) {
        0 => APPLICATION + under(rng, SLOT),
        1 => END - 4 * under(rng, 16),
        2 => *rng.pick(&[0, 4, 0x7FFC]),
        3 => END + under(rng, 0x10_0000),
        4 => rng.next_u32() | 0x8000_0000,
        _ => WORKSPACE + 4 * under(rng, 0x400),
    }
}

/// A word a stub description holds.
fn described(rng: &mut Prng, heaps: &[Range<u32>]) -> u32 {
    match rng.below(5) {
        0 => *rng.pick(&[1, 2, 3, 4, 9, 12, u32::MAX]),
        1 => wild(rng, heaps),
        _ => pointer(rng),
    }
}

/// A `LibInit` SWI with a hostile description list and registers.
fn lib_init(session: &mut Session, rng: &mut Prng, heaps: &[Range<u32>]) {
    let mut words = Vec::new();
    for _ in 0..rng.below(4) {
        let any = rng.next_u32();
        let id = *rng.pick(&[1, 2, 5, 9, 3, any]);
        let start = described(rng, heaps);
        let entries = under(rng, 256);
        let statics = described(rng, heaps);
        let any = rng.next_u32();
        let size = *rng.pick(&[KERNEL_STATICS, C_STATICS, 0, any]);
        words.extend([
            id,
            start,
            start.wrapping_add(4 * entries),
            statics,
            statics.wrapping_add(size),
        ]);
    }
    if rng.below(8) != 0 {
        words.push(u32::MAX);
    }
    let mut os = session.kernel.os();
    for (n, &word) in (0..).zip(&words) {
        os.write_word(DESCRIPTIONS + 4 * n, word)
            .expect("described");
    }
    let swi = SHARED_C_LIBRARY + under(rng, 8);
    let workspace = if rng.below(3) == 0 {
        pointer(rng)
    } else {
        WORKSPACE
    };
    let limit = if rng.below(3) == 0 { pointer(rng) } else { END };
    let r = [
        DESCRIPTIONS,
        workspace,
        limit,
        described(rng, heaps),
        described(rng, heaps),
        described(rng, heaps),
        rng.next_u32(),
    ];
    session.allow_words(Some(WORDS));
    let _ = call(session, swi, &r);
    session.allow_words(None);
}

/// Where chunk `place`'s vector lies: its entries, then their table.
fn vector_of(place: usize) -> u32 {
    VECTOR
        + 8 * CHUNKS[..place]
            .iter()
            .map(|&(_, entries, _)| entries)
            .sum::<u32>()
}

/// A sound application `LibInit`: every chunk's vector from [`VECTOR`], the
/// statics from [`STATICS`], the root stack at [`WORKSPACE`].
fn sound_lib_init(session: &mut Session) {
    let mut words = Vec::new();
    let mut statics = STATICS;
    for (place, &(id, entries, bytes)) in CHUNKS.iter().enumerate() {
        let vector = vector_of(place);
        words.extend([id, vector, vector + 4 * entries, statics, statics + bytes]);
        statics += bytes;
    }
    words.push(u32::MAX);
    let mut os = session.kernel.os();
    for (n, &word) in (0..).zip(&words) {
        os.write_word(DESCRIPTIONS + 4 * n, word)
            .expect("described");
    }
    let r = [
        DESCRIPTIONS,
        WORKSPACE,
        END,
        u32::MAX,
        0,
        u32::MAX,
        0x4_0000,
    ];
    call(session, SHARED_C_LIBRARY + 3, &r).expect("a sound client");
}

/// Guest code calling chunk `place`'s entry `entry` through its vector with
/// `registers`.
struct EntryCall {
    place: usize,
    entry: u32,
    registers: [u32; 15],
}

impl EntryCall {
    /// A random entry of a random chunk with hostile registers.
    fn drawn(rng: &mut Prng, heaps: &[Range<u32>]) -> Self {
        let place = rng.below(CHUNKS.len());
        let entry = under(rng, CHUNKS[place].1);
        let mut registers = [0; 15];
        for slot in &mut registers[..10] {
            *slot = if rng.below(3) == 0 {
                wild(rng, heaps)
            } else {
                pointer(rng)
            };
        }
        registers[10] = if rng.below(4) == 0 {
            pointer(rng)
        } else {
            WORKSPACE + 560
        };
        let frame = pointer(rng);
        registers[11] = *rng.pick(&[0, frame]);
        registers[13] = if rng.below(4) == 0 {
            pointer(rng)
        } else {
            WORKSPACE + 0x800 + 4 * under(rng, 0x100)
        };
        Self {
            place,
            entry,
            registers,
        }
    }

    /// The call made, run to its end or its budget.
    fn make(&self, session: &mut Session) {
        let target = vector_of(self.place) + 4 * self.entry;
        let call = 0xEB00_0000 | ((target.wrapping_sub(CODE + 8) >> 2) & 0x00FF_FFFF);
        let exit = 0xEF00_0000 | EXIT;
        let mut os = session.kernel.os();
        os.write_word(CODE, call).expect("written");
        os.write_word(CODE + 4, exit).expect("written");
        let fail = os.enter_application(Bits::Bit32, CODE, |user| *user = self.registers);
        assert!(
            matches!(fail, Fail::Unwind(_)),
            "the call entered: {fail:?}"
        );
        session.allow_words(Some(WORDS));
        let _ = session.kernel.run(BUDGET);
        session.allow_words(None);
    }
}

/// The library re-initialised after a run holds none of the RMA it took,
/// unless the guest wrote over the RMA's own structure, as a program may on
/// RISC OS: the heap then answers that it is corrupt, and what the library
/// gave back cannot be counted. Any other refusal is the library's.
fn the_rma_given_back(session: &mut Session, held: u32) {
    let mut os = session.kernel.os();
    match command(&mut os, "RMReInit SharedCLibrary") {
        Ok(()) => assert_eq!(rma_used(session), held, "the RMA given back"),
        Err(Fail::Error(block)) => {
            let number = os.read_word(block).expect("the error's number");
            assert_eq!(
                number,
                HeapError::BadLink.error().number,
                "reinitialised: error {number:#X}"
            );
        }
        Err(unwind) => panic!("reinitialised: {unwind:?}"),
    }
}

#[test]
fn hostile_library_calls_leave_the_kernel_answering_and_the_rma_given_back() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_library_calls_leave_the_kernel_answering_and_the_rma_given_back",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut runs = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if runs >= SMOKE_RUNS => break,
            _ => {}
        }
        let mut session = fresh();
        let heaps = kernel_heaps(&mut session);
        let held = rma_used(&mut session);
        sound_lib_init(&mut session);
        for step in 0..CALLS {
            if rng.below(3) == 0 {
                lib_init(&mut session, &mut rng, &heaps);
            } else {
                EntryCall::drawn(&mut rng, &heaps).make(&mut session);
            }
            assert!(
                answers(&mut session),
                "the kernel answering after step {step}"
            );
        }
        the_rma_given_back(&mut session, held);
        runs += 1;
    }
}

/// The soak's find: `_kernel_system`, its SL eight bytes off its stack chunk,
/// puts the handlers back through what it takes for the statics, making a
/// FileCore routine the abort handler; `islower`'s abort runs it and the
/// entries after it in Abort mode, the last filling the RMA. Every answer
/// stays a value.
#[test]
fn a_client_whose_handlers_fill_the_rma_is_answered_as_values() {
    let mut session = fresh();
    let held = rma_used(&mut session);
    sound_lib_init(&mut session);
    let calls = [
        EntryCall {
            place: 0,
            entry: 22,
            registers: [
                0x8AEB_9FA9,
                END,
                0x6EA2_162F,
                0,
                WORKSPACE + 0x1B0,
                END - 8,
                WORKSPACE + 0xB2C,
                0x3344_EEF8,
                END - 0x1C,
                WORKSPACE + 0xEA8,
                WORKSPACE + 552,
                0,
                0,
                0x895A_1D97,
                0,
            ],
        },
        EntryCall {
            place: 1,
            entry: 160,
            registers: [
                0x7FFC,
                0x7FFC,
                0xB2B9_0BA8,
                END - 0x28,
                0xCBC0_F741,
                0x7FFC,
                WORKSPACE + 0xE4C,
                WORKSPACE + 0x434,
                0x7B6B_FCD3,
                0x7EE2_26CC,
                END + 0x2_FAE2,
                0,
                0,
                WORKSPACE + 0x968,
                0,
            ],
        },
    ];
    for call in &calls {
        call.make(&mut session);
        assert!(answers(&mut session), "the kernel answering");
    }
    the_rma_given_back(&mut session, held);
}
