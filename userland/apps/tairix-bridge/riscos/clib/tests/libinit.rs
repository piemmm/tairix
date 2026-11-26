//! The library's SWIs held to what RISC OS 5.30 recorded: the calling
//! standards refused, a client's vector, statics and root stack laid for
//! each chunk in turn, and the checks in the order genuine makes them.

use std::sync::LazyLock;

use tairix_riscos_filing::session::{modules_with, session, Session, Setup};
use tairix_riscos_kernel::memory::layout::APPLICATION;
use tairix_riscos_kernel::module::Builtin;
use tairix_riscos_kernel::os::{Fail, Regs, Unwind};
use tairix_riscos_kernel::swi::chunk::SHARED_C_LIBRARY;
use tairix_riscos_kernel::swi::X;

static MODULES: LazyLock<Vec<&'static Builtin>> =
    LazyLock::new(|| modules_with(&[&tairix_riscos_clib::MODULES]));

const SLOT: u32 = 1 << 20;
const DESCRIPTIONS: u32 = APPLICATION + 0x100;
const VECTOR: u32 = APPLICATION + 0x1000;
const STATICS: u32 = APPLICATION + 0x2000;
const WORKSPACE: u32 = APPLICATION + 0x4000;
const END: u32 = APPLICATION + SLOT;
/// The SWIs, from the chunk's base.
const APCS_A: u32 = 0;
const APCS_R: u32 = 1;
const MODULE: u32 = 2;
const APCS_32: u32 = 3;
/// The errors.
const NO_MEMORY: u32 = 0x80_0E80;
const UNKNOWN_CHUNK: u32 = 0x80_0E81;
const STATIC_SIZE: u32 = 0x80_0E83;
const SWI_RANGE: u32 = 0x80_0E85;
const REFUSED_STANDARD: u32 = 0x80_0E86;
const OUT_OF_DATE: u32 = 0x80_0E91;
/// The kernel chunk.
const KERNEL_STATICS: u32 = 0x31C;
/// A word the library must not write.
const UNTOUCHED: u32 = 0x5EED_5EED;

fn fresh() -> Session {
    session(&Setup {
        vdu: None,
        modules: MODULES.as_slice(),
        bytes: SLOT,
        keys: &[],
        disc: None,
    })
}

/// `descriptions` laid at [`DESCRIPTIONS`], ended, then SWI `swi` with the
/// workspace from [`WORKSPACE`] to `end` and R6 `stack`: its registers, or
/// its error's number.
fn lib_init(
    s: &mut Session,
    swi: u32,
    descriptions: &[[u32; 5]],
    end: u32,
    stack: u32,
) -> Result<Regs, u32> {
    let mut os = s.kernel.os();
    let words: Vec<u32> = descriptions
        .iter()
        .flatten()
        .copied()
        .chain([u32::MAX])
        .collect();
    for (n, &word) in (0..).zip(&words) {
        os.write_word(DESCRIPTIONS + 4 * n, word)
            .expect("described");
    }
    let mut regs = Regs::default();
    regs.r[..7].copy_from_slice(&[DESCRIPTIONS, WORKSPACE, end, u32::MAX, 0, u32::MAX, stack]);
    match os.perform(X | (SHARED_C_LIBRARY + swi), &mut regs) {
        Ok(()) => Ok(regs),
        Err(Fail::Error(block)) => Err(os.read_word(block).expect("its number")),
        Err(unwind) => panic!("{unwind:?}"),
    }
}

/// The kernel chunk with `entries` entries and `statics` bytes of statics.
const fn kernel(entries: u32, statics: u32) -> [u32; 5] {
    [1, VECTOR, VECTOR + 4 * entries, STATICS, STATICS + statics]
}

fn word(s: &mut Session, at: u32) -> u32 {
    s.kernel.os().read_word(at).expect("read")
}

fn mark(s: &mut Session, at: u32) {
    s.kernel.os().write_word(at, UNTOUCHED).expect("marked");
}

#[test]
fn the_older_calling_standards_are_refused_from_a_32_bit_caller() {
    let mut s = fresh();
    for swi in [APCS_A, APCS_R, MODULE] {
        let refused = lib_init(&mut s, swi, &[kernel(48, KERNEL_STATICS)], END, 0);
        assert_eq!(refused.err(), Some(REFUSED_STANDARD), "SWI {swi}");
    }
    for swi in [5, 6, 0x3F] {
        let refused = lib_init(&mut s, swi, &[kernel(48, KERNEL_STATICS)], END, 0);
        assert_eq!(refused.err(), Some(SWI_RANGE), "SWI {swi}");
    }
}

#[test]
fn a_32_bit_client_gets_its_vector_statics_and_root_stack() {
    let mut s = fresh();
    mark(&mut s, VECTOR + 8 * 48);
    let done =
        lib_init(&mut s, APCS_32, &[kernel(48, KERNEL_STATICS)], END, 4 << 16).expect("laid");
    assert_eq!(done.r[..3], [END, WORKSPACE, WORKSPACE + 0x1000]);
    assert_eq!(done.r[6], 6, "the library's version");
    for entry in 0..48 {
        assert_eq!(
            word(&mut s, VECTOR + 4 * entry),
            0xE59F_F000 | (4 * 48 - 8),
            "entry {entry}"
        );
    }
    let first = word(&mut s, VECTOR + 4 * 48);
    let second = word(&mut s, VECTOR + 4 * 49);
    assert!(first != 0 && second == first + 8, "the routines in a row");
    assert_eq!(
        word(&mut s, VECTOR + 8 * 48),
        UNTOUCHED,
        "nothing past the table"
    );
    assert_eq!(
        word(&mut s, WORKSPACE + 12),
        0x1000,
        "the root stack's size"
    );
    assert_ne!(word(&mut s, WORKSPACE + 20), 0, "the library static offset");
    assert_eq!(word(&mut s, WORKSPACE + 24), 0, "no client statics");
}

#[test]
fn a_short_vector_holds_only_the_entries_it_asks_for() {
    let mut s = fresh();
    mark(&mut s, VECTOR + 8 * 10);
    lib_init(&mut s, APCS_32, &[kernel(10, KERNEL_STATICS)], END, 0).expect("laid");
    assert_eq!(word(&mut s, VECTOR), 0xE59F_F000 | (4 * 10 - 8));
    assert_eq!(word(&mut s, VECTOR + 8 * 10), UNTOUCHED);
}

#[test]
fn the_root_stack_defaults_to_16_kib_and_needs_its_room_first() {
    let mut s = fresh();
    let done = lib_init(&mut s, APCS_32, &[kernel(48, KERNEL_STATICS)], END, 0).expect("laid");
    assert_eq!(done.r[2], WORKSPACE + 0x4000);
    let mut s = fresh();
    mark(&mut s, VECTOR);
    let refused = lib_init(
        &mut s,
        APCS_32,
        &[kernel(48, KERNEL_STATICS)],
        WORKSPACE + 0x100,
        4 << 16,
    );
    assert_eq!(refused.err(), Some(NO_MEMORY));
    assert_eq!(
        word(&mut s, VECTOR),
        UNTOUCHED,
        "checked before anything is written"
    );
}

#[test]
fn the_statics_size_is_checked_before_the_entries_and_must_match() {
    let mut s = fresh();
    let refused = lib_init(&mut s, APCS_32, &[kernel(49, KERNEL_STATICS + 4)], END, 0);
    assert_eq!(refused.err(), Some(STATIC_SIZE));
    let refused = lib_init(&mut s, APCS_32, &[kernel(49, KERNEL_STATICS)], END, 0);
    assert_eq!(
        refused.err(),
        Some(OUT_OF_DATE),
        "more entries than the library has"
    );
}

#[test]
fn a_long_list_of_descriptions_is_work_the_host_may_stop() {
    const C_LIBRARY_ENTRIES: u32 = 187;
    const C_LIBRARY_STATICS: u32 = 0xB48;
    let mut s = fresh();
    let list = APPLICATION + 0x1_0000;
    let c_library = [
        2,
        VECTOR,
        VECTOR + 4 * C_LIBRARY_ENTRIES,
        STATICS,
        STATICS + C_LIBRARY_STATICS,
    ];
    let words: Vec<u32> = std::iter::repeat_n(c_library, 1000)
        .flatten()
        .chain([u32::MAX])
        .collect();
    let mut os = s.kernel.os();
    for (n, &word) in (0..).zip(&words) {
        os.write_word(list + 4 * n, word).expect("described");
    }
    s.allow_words(Some(0));
    let mut regs = Regs::default();
    regs.r[..7].copy_from_slice(&[list, WORKSPACE, END, u32::MAX, 0, u32::MAX, 0]);
    let outcome = s
        .kernel
        .os()
        .perform(X | (SHARED_C_LIBRARY + APCS_32), &mut regs);
    assert!(
        matches!(outcome, Err(Fail::Unwind(Unwind::Stopped))),
        "{outcome:?}"
    );
}

#[test]
fn each_chunk_is_laid_whole_before_the_next_is_checked() {
    let mut s = fresh();
    mark(&mut s, VECTOR);
    let unknown = [
        3,
        VECTOR + 0x800,
        VECTOR + 0x804,
        STATICS + 0x800,
        STATICS + 0x804,
    ];
    let refused = lib_init(
        &mut s,
        APCS_32,
        &[kernel(48, KERNEL_STATICS), unknown],
        END,
        0,
    );
    assert_eq!(refused.err(), Some(UNKNOWN_CHUNK));
    assert_ne!(
        word(&mut s, VECTOR),
        UNTOUCHED,
        "the kernel chunk's vector written"
    );
    for id in [10, 11, 12] {
        let refused = lib_init(
            &mut s,
            APCS_32,
            &[[id, VECTOR, VECTOR + 4, STATICS, STATICS]],
            END,
            0,
        );
        assert_eq!(refused.err(), Some(UNKNOWN_CHUNK), "chunk {id}");
    }
}
