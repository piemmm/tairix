//! VFPSupport held to the ROOL wiki's interface: its contexts switched now
//! and lazily, the unit's gate, the dump, the fast entry points and the
//! elementary functions.

use alloc::format;

use tairix_riscos_arm::instruction::{ExchangeOp, Op, SimdFp, VfpDyadicOp};
use tairix_riscos_arm::profile::BIT32;
use tairix_riscos_arm::psr::{self, Mode};
use tairix_riscos_arm::reg::Reg;

use super::*;
use crate::memory::layout::{HIGH_ZERO_PAGE, RISC_OS_5, SHARED};
use crate::memory::Bits;
use crate::os::{Ended, Outcome};
use crate::swi::os::{CLAIM_PROCESSOR_VECTOR, DYNAMIC_AREA, MODULE, SWI_NUMBER_TO_STRING};
use crate::swi::vfp_support::{
    ACTIVE_CONTEXT, CHANGE_CONTEXT, CHECK_CONTEXT, CREATE_CONTEXT, DESTROY_CONTEXT,
    ELEMENTARY_FUNCTIONS, EXAMINE_CONTEXT, EXCEPTION_DUMP, FAST_API, FEATURES,
    VERSION as VERSION_SWI,
};
use crate::swi::X;
use crate::testkit::{asm, with_application, Session, CODE, DATA};

const USER: u32 = kept::USER;
const APPLICATION: u32 = kept::APPLICATION;
const LAZILY: u32 = create::LAZILY;
const NOW: u32 = create::NOW;
const INVALID_PARAMETERS: u32 = 0x1EA;
const UNDEFINED_INSTRUCTION: u32 = 0x8000_0000;
const NAME: u32 = DATA + 0x700;

fn session() -> Session {
    with_application(&RISC_OS_5, Bits::Bit32)
}

fn registers(low: &[u32]) -> [u32; 13] {
    let mut r = [0; 13];
    r[..low.len()].copy_from_slice(low);
    r
}

/// SWI `number`, which must not fail, with `low` from R0 on.
fn ok(s: &mut Session, number: u32, low: &[u32]) -> Regs {
    let (regs, _) = s.user_swi(X | number, registers(low));
    assert!(
        !regs.flag(psr::V),
        "SWI &{number:X} {}",
        s.error_text(regs.r[0]).escape_ascii()
    );
    regs
}

/// The number of the error SWI `number` gives with `low` from R0 on.
fn refused(s: &mut Session, number: u32, low: &[u32]) -> u32 {
    let (regs, _) = s.user_swi(X | number, registers(low));
    assert!(regs.flag(psr::V), "SWI &{number:X} refused");
    s.peek(regs.r[0])
}

fn encoded(op: Op) -> u32 {
    instruction(op).expect("encodable")
}

/// `VADD.F32 S0, S1, S2`.
fn vadd() -> u32 {
    encoded(Op::SimdFp(SimdFp::Dyadic {
        op: VfpDyadicOp::Vadd,
        double: false,
        d: 0,
        n: 1,
        m: 2,
    }))
}

/// `code` run at `CODE` in User mode with `low` from R0 on, a spin after it.
fn run(s: &mut Session, bits: Bits, code: &[u32], low: &[u32]) -> Outcome {
    let spin = CODE + 4 * u32::try_from(code.len()).expect("short");
    s.poke(CODE, &[code, &[asm::b(spin, spin)]].concat());
    s.enter(bits, CODE, registers(low));
    s.resume()
}

/// A call of the routine at `target` from User mode, its operands from R0.
fn call(s: &mut Session, target: u32, low: &[u32]) -> Outcome {
    let mut r = [0; 4];
    r[..low.len()].copy_from_slice(low);
    let blx = encoded(Op::Exchange {
        op: ExchangeOp::Blx,
        rm: Reg::field(4),
    });
    run(s, Bits::Bit32, &[blx], &[r[0], r[1], r[2], r[3], target])
}

fn undefined(outcome: Outcome) -> u32 {
    match outcome {
        Outcome::Ended(Ended::Error { number, .. }) => number,
        other => panic!("{other:?}"),
    }
}

fn f(value: f32) -> u32 {
    value.to_bits()
}

fn set_s(s: &mut Session, n: u8, value: u32) {
    s.kernel
        .os()
        .world
        .processor
        .with_vfp()
        .vfp_mut()
        .set_s(n, value);
}

fn get_s(s: &mut Session, n: u8) -> u32 {
    s.kernel.os().world.processor.with_vfp().vfp().s(n)
}

fn set_d(s: &mut Session, n: u8, value: f64) {
    s.kernel
        .os()
        .world
        .processor
        .with_vfp()
        .vfp_mut()
        .set_d(n, value.to_bits());
}

fn get_d(s: &mut Session, n: u8) -> u64 {
    s.kernel.os().world.processor.with_vfp().vfp().d(n)
}

fn fpscr_of(s: &mut Session) -> u32 {
    s.kernel.os().world.processor.with_vfp().vfp().fpscr()
}

fn set_fpscr(s: &mut Session, value: u32) {
    s.kernel
        .os()
        .world
        .processor
        .with_vfp()
        .vfp_mut()
        .set_fpscr(value);
}

fn cpacr(s: &mut Session) -> Option<u32> {
    s.kernel
        .os()
        .world
        .processor
        .with_vfp()
        .system_register([0, 1, 0, 2])
}

fn words(s: &mut Session, at: u32, n: u32) -> alloc::vec::Vec<u32> {
    (0..n).map(|k| s.peek(at + 4 * k)).collect()
}

/// The undefined instruction vector's owner.
fn vector_owner(s: &mut Session) -> u32 {
    s.peek(HIGH_ZERO_PAGE + crate::zeropage::PROCESSOR_VECTOR_TABLE + 4 * crate::host::UNDEFINED)
}

#[test]
fn the_version_features_and_names_are_the_wiki_s_and_the_unit_s() {
    let mut s = session();
    assert_eq!(ok(&mut s, VERSION_SWI, &[]).r[0], 18);
    let isa = BIT32.isa;
    let identity = |register: System| register.identity(isa);
    assert_eq!(
        ok(&mut s, FEATURES, &[0]).r[..3],
        [
            identity(System::Fpsid),
            identity(System::Mvfr0),
            identity(System::Mvfr1)
        ]
    );
    assert_eq!(ok(&mut s, FEATURES, &[1]).r[0], 0, "no trapped exception");
    assert_eq!(
        ok(&mut s, FEATURES, &[2]).r[0],
        1,
        "short vectors in hardware"
    );
    assert_eq!(
        ok(&mut s, FEATURES, &[3, 9, 9, 9]).r[..4],
        [identity(System::Mvfr2), 0, 0, 0]
    );
    assert_eq!(
        refused(&mut s, FEATURES, &[4]),
        0x180,
        "no such reason code"
    );
    let names = [
        "CheckContext",
        "CreateContext",
        "DestroyContext",
        "ChangeContext",
        "ExamineContext",
        "FastAPI",
        "ActiveContext",
        "Version",
        "Features",
        "ExceptionDump",
        "ElementaryFunctions",
        "11",
    ];
    for (offset, name) in (0..).zip(names) {
        s.user_swi(SWI_NUMBER_TO_STRING, registers(&[CHUNK + offset, NAME, 64]));
        let text = s
            .kernel
            .os()
            .read_until(NAME, 64, |b| b == 0)
            .expect("a name")
            .0;
        assert_eq!(text, [&b"VFPSupport_"[..], name.as_bytes()].concat());
    }
    let (regs, _) = s.user_swi(X | (CHUNK + 11), registers(&[]));
    assert!(regs.flag(psr::V));
    assert!(s
        .error_text(regs.r[0])
        .windows(10)
        .any(|word| word == b"VFPSupport"));
    s.write(NAME, b"VFPSupport\0");
    let base = ok(&mut s, MODULE, &[18, NAME]).r[3];
    let help_at = base + s.peek(base + 20);
    let help = s
        .kernel
        .os()
        .read_until(help_at, 64, |b| b == 0)
        .expect("help")
        .0;
    assert!(
        help.starts_with(b"VFPSupport\t0.18 ("),
        "{}",
        help.escape_ascii()
    );
}

#[test]
fn check_context_sizes_a_context_and_refuses_what_the_unit_cannot_hold() {
    let mut s = session();
    assert_eq!(ok(&mut s, CHECK_CONTEXT, &[0, 1]).r[0], 28);
    assert_eq!(
        ok(
            &mut s,
            CHECK_CONTEXT,
            &[USER | APPLICATION | LAZILY | NOW, 32]
        )
        .r[0],
        276,
        "bits 30 and 31 ignored"
    );
    for (flags, count) in [(0, 0), (0, 33), (1 << 2, 1)] {
        assert_eq!(
            refused(&mut s, CHECK_CONTEXT, &[flags, count]),
            INVALID_PARAMETERS
        );
    }
    assert_eq!(
        refused(&mut s, CREATE_CONTEXT, &[APPLICATION, 4, 0, 0]),
        INVALID_PARAMETERS,
        "VFPSupport's own memory is no application space"
    );
    assert_eq!(
        refused(&mut s, CREATE_CONTEXT, &[0, 4, DATA + 2, 0]),
        INVALID_PARAMETERS
    );
}

#[test]
fn with_no_context_the_unit_s_instructions_are_undefined_and_cpacr_grants_none() {
    let mut s = session();
    assert_eq!(ok(&mut s, ACTIVE_CONTEXT, &[]).r[0], NO_CONTEXT);
    assert_eq!(cpacr(&mut s), Some(0));
    assert_eq!(
        undefined(run(&mut s, Bits::Bit32, &[vadd()], &[])),
        UNDEFINED_INSTRUCTION
    );
}

#[test]
fn a_context_activated_now_takes_the_unit_and_keeps_its_registers_across_a_switch() {
    let mut s = session();
    let created = ok(&mut s, CREATE_CONTEXT, &[USER | NOW, 32, 0, 0]);
    let (a, previous) = (created.r[0], created.r[1]);
    assert_eq!(previous, NO_CONTEXT);
    assert_eq!(
        cpacr(&mut s),
        Some(0x00F0_0000),
        "User mode granted the unit"
    );
    set_s(&mut s, 1, f(1.5));
    set_s(&mut s, 2, f(2.25));
    assert_eq!(run(&mut s, Bits::Bit32, &[vadd()], &[]), Outcome::Budget);
    assert_eq!(get_s(&mut s, 0), f(3.75));

    let toward_zero = 3 << fpscr::RMODE_SHIFT;
    let b = ok(&mut s, CREATE_CONTEXT, &[USER | NOW, 16, 0, toward_zero]);
    assert_eq!(b.r[1], a, "the context it replaced");
    assert_eq!(
        words(&mut s, a + at::FPSCR, 6),
        [0, fpexc::EN, f(3.75), f(1.5), f(2.25), 0],
        "A's status and registers kept in its area"
    );
    assert_eq!(fpscr_of(&mut s), toward_zero, "B's FPSCR");
    set_s(&mut s, 0, f(9.0));
    assert_eq!(ok(&mut s, CHANGE_CONTEXT, &[a, 0]).r[0], b.r[0]);
    assert_eq!(get_s(&mut s, 0), f(3.75), "A's own again");
    assert_eq!(fpscr_of(&mut s), 0);
    assert_eq!(s.peek(b.r[0] + at::DUMP), f(9.0), "B's kept in its area");
}

#[test]
fn a_context_that_cannot_be_loaded_leaves_the_unit_denied_and_the_last_kept() {
    let mut s = session();
    let a = ok(&mut s, CREATE_CONTEXT, &[USER | NOW, 32, 0, 0]).r[0];
    set_s(&mut s, 1, f(1.5));
    // Its header fits the slot; its registers run past the slot's end.
    let end = crate::memory::layout::APPLICATION + crate::testkit::SLOT_BYTES;
    let made = s.start_swi(
        X | CREATE_CONTEXT,
        registers(&[USER | NOW, 32, end - 20, 0]),
    );
    assert!(
        matches!(
            made,
            Outcome::Ended(Ended::Error {
                number: 0x8000_0002,
                ..
            })
        ),
        "{made:?}"
    );
    assert_eq!(
        cpacr(&mut s),
        Some(0),
        "no context in the unit, none granted it"
    );
    assert_eq!(ok(&mut s, ACTIVE_CONTEXT, &[]).r[0], a);
    assert_eq!(
        run(&mut s, Bits::Bit32, &[vadd()], &[]),
        Outcome::Budget,
        "the unit's instruction activates A again"
    );
    assert_eq!(get_s(&mut s, 1), f(1.5), "A's registers as they were");
}

#[test]
fn destroying_the_active_context_for_one_that_cannot_be_loaded_destroys_nothing() {
    let mut s = session();
    let a = ok(&mut s, CREATE_CONTEXT, &[USER | NOW, 32, 0, 0]).r[0];
    let end = crate::memory::layout::APPLICATION + crate::testkit::SLOT_BYTES;
    let b = ok(&mut s, CREATE_CONTEXT, &[USER, 32, end - 20, 0]).r[0];
    let destroyed = s.start_swi(X | DESTROY_CONTEXT, registers(&[a, b]));
    assert!(
        matches!(
            destroyed,
            Outcome::Ended(Ended::Error {
                number: 0x8000_0002,
                ..
            })
        ),
        "{destroyed:?}"
    );
    assert_eq!(ok(&mut s, ACTIVE_CONTEXT, &[]).r[0], a, "A still active");
    assert_eq!(
        ok(&mut s, CHANGE_CONTEXT, &[a, 0]).r[0],
        a,
        "and still a context"
    );
}

#[test]
fn a_context_without_the_user_flag_keeps_user_mode_out_of_the_unit() {
    let mut s = session();
    ok(&mut s, CREATE_CONTEXT, &[NOW, 4, 0, 0]);
    assert_eq!(cpacr(&mut s), Some(0x0050_0000), "privileged modes only");
    assert_eq!(
        undefined(run(&mut s, Bits::Bit32, &[vadd()], &[])),
        UNDEFINED_INSTRUCTION
    );
}

#[test]
fn examine_describes_a_context_and_serialising_leaves_its_state_in_its_area() {
    let mut s = session();
    ok(&mut s, CREATE_CONTEXT, &[USER | NOW, 4, DATA, 0]);
    set_s(&mut s, 0, f(1.0));
    let examined = ok(&mut s, EXAMINE_CONTEXT, &[DATA, 0]);
    assert_eq!(examined.r[0], USER | examined::LIVE);
    assert_eq!(examined.r[1..3], [4, 0xF]);
    assert_eq!(
        words(&mut s, examined.r[3], 4),
        [0x000C_0000, 0x0010_0001, 0x0014_0005, u32::MAX],
        "the FPSCR, FPEXC and dump at their offsets"
    );
    assert_eq!(examined.r[4], 52);

    let serialised = ok(&mut s, EXAMINE_CONTEXT, &[DATA, examined::SERIALISE]);
    assert_eq!(serialised.r[0], USER | examined::AWAITING);
    assert_eq!(serialised.r[2], 0, "nothing of it the unit's");
    assert_eq!(s.peek(DATA + at::DUMP), f(1.0));
    // A debugger's change to the area is what the next activation loads.
    s.poke(DATA + at::DUMP, &[0, f(5.0), f(2.0), 0]);
    assert_eq!(run(&mut s, Bits::Bit32, &[vadd()], &[]), Outcome::Budget);
    assert_eq!(get_s(&mut s, 0), f(7.0));
    assert_eq!(ok(&mut s, ACTIVE_CONTEXT, &[]).r[0], DATA);
    assert_eq!(
        refused(&mut s, EXAMINE_CONTEXT, &[DATA, 2]),
        INVALID_PARAMETERS
    );
    assert_eq!(
        refused(&mut s, EXAMINE_CONTEXT, &[DATA + 0x100, 0]),
        INVALID_PARAMETERS
    );
}

#[test]
fn lazy_activation_waits_for_the_unit_s_first_instruction() {
    let mut s = session();
    let (a, b) = (DATA, DATA + 0x100);
    ok(&mut s, CREATE_CONTEXT, &[USER | NOW, 4, a, 0]);
    ok(&mut s, CREATE_CONTEXT, &[USER, 4, b, 0]);
    set_s(&mut s, 0, f(1.0));
    set_s(&mut s, 1, f(1.5));
    set_s(&mut s, 2, f(2.5));
    s.poke(a + at::DUMP, &[0xAAAA_AAAA; 8]);
    assert_eq!(ok(&mut s, CHANGE_CONTEXT, &[b, change::LAZILY]).r[0], a);
    assert_eq!(
        ok(&mut s, ACTIVE_CONTEXT, &[]).r[0],
        b,
        "the context awaiting"
    );
    assert_eq!(
        ok(&mut s, EXAMINE_CONTEXT, &[b, 0]).r[0],
        USER | examined::AWAITING
    );
    assert_eq!(
        ok(&mut s, EXAMINE_CONTEXT, &[a, 0]).r[0],
        USER | examined::LIVE
    );

    // Back before any instruction: the unit holds it still, and nothing is saved.
    assert_eq!(ok(&mut s, CHANGE_CONTEXT, &[a, change::LAZILY]).r[0], b);
    assert_eq!(
        ok(&mut s, EXAMINE_CONTEXT, &[a, 0]).r[0],
        USER | examined::LIVE
    );
    assert_eq!(s.peek(a + at::DUMP), 0xAAAA_AAAA);

    ok(&mut s, CHANGE_CONTEXT, &[b, change::LAZILY]);
    assert_eq!(run(&mut s, Bits::Bit32, &[vadd()], &[]), Outcome::Budget);
    assert_eq!(get_s(&mut s, 0), f(4.0), "run again once B was active");
    assert_eq!(
        words(&mut s, a + at::DUMP, 4),
        [f(1.0), f(1.5), f(2.5), 0],
        "A saved as B took the unit"
    );
    assert_eq!(
        ok(&mut s, EXAMINE_CONTEXT, &[b, 0]).r[0],
        USER | examined::LIVE
    );
}

#[test]
fn a_thumb_instruction_activates_the_context_awaiting_and_runs_again() {
    let mut s = session();
    ok(&mut s, CREATE_CONTEXT, &[USER | LAZILY, 4, 0, 0]);
    set_s(&mut s, 1, f(1.0));
    set_s(&mut s, 2, f(2.0));
    // VADD.F32 S0, S1, S2, then B . in Thumb state.
    s.poke(CODE, &[0x0A81_EE30, 0xE7FE_E7FE]);
    s.enter(Bits::Bit32, CODE, [0; 13]);
    let thumb = Mode::User.bits() | psr::T;
    s.kernel
        .os()
        .world
        .processor
        .cpu()
        .set_cpsr(thumb)
        .expect("Thumb");
    assert_eq!(s.resume(), Outcome::Budget);
    assert_eq!(get_s(&mut s, 0), f(3.0));
    assert_eq!(s.kernel.state().pc, CODE + 4, "the spin after it");
}

#[test]
fn destroying_the_active_context_hands_the_unit_to_its_replacement() {
    let mut s = session();
    let (a, b) = (DATA, DATA + 0x100);
    ok(&mut s, CREATE_CONTEXT, &[USER | NOW, 4, a, 0]);
    ok(&mut s, CREATE_CONTEXT, &[USER, 4, b, 0]);
    assert_eq!(ok(&mut s, DESTROY_CONTEXT, &[a, b]).r[0], b);
    assert_eq!(ok(&mut s, ACTIVE_CONTEXT, &[]).r[0], b);
    assert_eq!(s.peek(a), 0, "no longer marked a context");
    assert_eq!(refused(&mut s, CHANGE_CONTEXT, &[a, 0]), INVALID_PARAMETERS);
    let owned = ok(&mut s, CREATE_CONTEXT, &[USER, 1, 0, 0]).r[0];
    assert_eq!(
        ok(&mut s, DESTROY_CONTEXT, &[owned, a]).r[0],
        b,
        "an inactive one changes nothing"
    );
    assert_eq!(ok(&mut s, DESTROY_CONTEXT, &[b, 0]).r[0], NO_CONTEXT);
    assert_eq!(cpacr(&mut s), Some(0));
}

#[test]
fn application_space_changing_saves_a_context_there_now() {
    let mut s = session();
    ok(
        &mut s,
        CREATE_CONTEXT,
        &[USER | APPLICATION | NOW, 4, DATA, 0],
    );
    set_s(&mut s, 0, f(6.0));
    let changing = change::APPLICATION_CHANGING | change::LAZILY;
    assert_eq!(ok(&mut s, CHANGE_CONTEXT, &[0, changing]).r[0], DATA);
    assert_eq!(s.peek(DATA + at::DUMP), f(6.0));
    assert_eq!(
        ok(&mut s, EXAMINE_CONTEXT, &[DATA, 0]).r[0],
        USER | APPLICATION
    );
    // The next task's context is checked only once it is mapped and used.
    assert_eq!(
        ok(&mut s, CHANGE_CONTEXT, &[DATA + 0x100, changing]).r[0],
        NO_CONTEXT
    );
    assert_eq!(
        undefined(run(&mut s, Bits::Bit32, &[vadd()], &[])),
        UNDEFINED_INSTRUCTION
    );
    assert_eq!(
        ok(&mut s, ACTIVE_CONTEXT, &[]).r[0],
        NO_CONTEXT,
        "none lay there"
    );
}

#[test]
fn the_fast_entry_points_are_the_swis() {
    let mut s = session();
    let fast = ok(&mut s, FAST_API, &[]);
    assert_eq!(fast.r[0], 0, "no workspace");
    for (n, &address) in fast.r[1..5].iter().enumerate() {
        let routine = s.kernel.os().routine_of(&VFP_SUPPORT, routine::FAST + n);
        assert_eq!(Some(address), routine);
    }
    assert_eq!(call(&mut s, fast.r[1], &[0, 4]), Outcome::Budget);
    assert_eq!(s.regs().r[0], 52, "CheckContext's answer");
    assert_eq!(call(&mut s, fast.r[1], &[0, 0]), Outcome::Budget);
    assert!(s.regs().flag(psr::V), "and its refusal");
}

/// The bytes of the RMA in use.
fn rma_used(s: &mut Session) -> u32 {
    let free = ok(s, MODULE, &[5]).r[3];
    ok(s, crate::swi::os::READ_DYNAMIC_AREA, &[1]).r[1] - free
}

#[test]
fn a_copy_made_before_a_clear_the_heap_refuses_is_freed_with_the_refusal() {
    let mut s = session();
    // A hole ahead of the dump, which the copy is made in, so freeing the
    // copy walks no block the stray write below spoils.
    let hole = ok(&mut s, MODULE, &[6, 0, 0, 256]).r[2];
    ok(&mut s, CREATE_CONTEXT, &[USER | NOW, 2, DATA, 0]);
    ok(&mut s, EXCEPTION_DUMP, &[dump::CREATE, DATA]);
    let (block, _) = s.kernel.os().world.vfp_support.dump.expect("a dump");
    assert!(hole < block, "the hole ahead of it");
    ok(&mut s, MODULE, &[7, 0, hole]);
    // Its heap block's size overwritten, as a stray write would leave it;
    // the heap's refusal looked up once, so its message is held already.
    s.poke(block - 4, &[0]);
    refused(&mut s, MODULE, &[7, 0, block]);
    let before = rma_used(&mut s);
    refused(&mut s, EXCEPTION_DUMP, &[dump::COPY | dump::CLEAR]);
    assert_eq!(rma_used(&mut s), before, "the copy freed with the refusal");
}

#[test]
fn a_dying_module_whose_dump_will_not_free_dies_with_its_vector_given_back() {
    let mut s = session();
    ok(&mut s, CREATE_CONTEXT, &[USER | NOW, 2, DATA, 0]);
    ok(&mut s, EXCEPTION_DUMP, &[dump::CREATE, DATA]);
    let (block, _) = s.kernel.os().world.vfp_support.dump.expect("a dump");
    // Freed under it, so its own free is refused.
    ok(&mut s, MODULE, &[7, 0, block]);
    s.write(NAME, b"VFPSupport\0");
    ok(&mut s, MODULE, &[4, NAME]);
    let kernel_s = {
        let os = s.kernel.os();
        os.world
            .routine_address(crate::host::Routine::Exception(crate::host::UNDEFINED))
    };
    assert_eq!(vector_owner(&mut s), kernel_s, "given back as it died");
    ok(&mut s, MODULE, &[3, NAME]);
}

#[test]
fn the_exception_dump_keeps_what_was_made_until_cleared() {
    let mut s = session();
    let empty = ok(&mut s, EXCEPTION_DUMP, &[dump::COPY | dump::ORIGINAL, 7]);
    assert_eq!(empty.r[..2], [0, 0]);
    ok(&mut s, CREATE_CONTEXT, &[USER | NOW, 2, DATA, 0]);
    set_s(&mut s, 0, f(8.0));
    ok(&mut s, EXCEPTION_DUMP, &[dump::CREATE, DATA]);
    let got = ok(&mut s, EXCEPTION_DUMP, &[dump::COPY | dump::ORIGINAL]);
    let copy = got.r[0];
    assert_ne!(copy, 0);
    assert_eq!(got.r[1], DATA);
    let examined = ok(&mut s, EXAMINE_CONTEXT, &[copy, 0]);
    assert_eq!(
        examined.r[..2],
        [USER, 2],
        "a context of its own, the caller's to free"
    );
    assert_eq!(
        s.peek(copy + at::DUMP),
        f(8.0),
        "the unit's state as it was"
    );
    ok(&mut s, MODULE, &[7, 0, copy]);
    let cleared = ok(&mut s, EXCEPTION_DUMP, &[dump::COPY | dump::CLEAR]);
    assert_ne!(cleared.r[0], 0, "got before it was cleared");
    ok(&mut s, MODULE, &[7, 0, cleared.r[0]]);
    assert_eq!(ok(&mut s, EXCEPTION_DUMP, &[dump::COPY]).r[0], 0);
    assert_eq!(
        refused(&mut s, EXCEPTION_DUMP, &[1 << 4]),
        INVALID_PARAMETERS
    );
}

#[test]
fn the_elementary_functions_compute_in_the_unit_s_registers_under_its_fpscr() {
    let mut s = session();
    let tables = ok(&mut s, ELEMENTARY_FUNCTIONS, &[0]);
    assert_eq!(tables.r[0], 12);
    let (single, double) = (tables.r[1], tables.r[2]);
    assert_eq!(
        refused(&mut s, ELEMENTARY_FUNCTIONS, &[1]),
        INVALID_PARAMETERS
    );
    assert_eq!(
        undefined(call(&mut s, double, &[])),
        UNDEFINED_INSTRUCTION,
        "with no context the function's first instruction is undefined"
    );

    ok(&mut s, CREATE_CONTEXT, &[USER | LAZILY, 32, 0, 0]);
    set_d(&mut s, 0, 1.0);
    assert_eq!(
        call(&mut s, double, &[]),
        Outcome::Budget,
        "activated by the stub"
    );
    assert_eq!(get_d(&mut s, 0), 0x3FEA_ED54_8F09_0CEE, "sin 1");
    assert_ne!(fpscr_of(&mut s) & fpscr::IXC, 0);

    set_fpscr(&mut s, 1 << fpscr::RMODE_SHIFT);
    set_d(&mut s, 0, 1.0);
    call(&mut s, double, &[]);
    assert_eq!(get_d(&mut s, 0), 0x3FEA_ED54_8F09_0CEF, "rounded up");

    set_fpscr(&mut s, 0);
    set_d(&mut s, 0, 2.0);
    set_d(&mut s, 1, 10.0);
    call(&mut s, double + 4 * 10, &[]);
    assert_eq!(get_d(&mut s, 0), 1024f64.to_bits(), "pow");
    assert_eq!(fpscr_of(&mut s) & fpscr::FLAGS, 0, "exact");
    for (n, value) in [2.0, 3.0, 1.0].into_iter().enumerate() {
        set_d(&mut s, u8::try_from(n).expect("small"), value);
    }
    call(&mut s, double + 4 * 11, &[]);
    assert_eq!(get_d(&mut s, 0), 7f64.to_bits(), "fma");

    set_s(&mut s, 0, f(1.0));
    call(&mut s, single, &[]);
    assert_eq!(get_s(&mut s, 0), 0x3F57_6AA4, "sinf 1");
    set_s(&mut s, 0, f(1.0));
    set_s(&mut s, 1, f(1.0));
    call(&mut s, single + 4 * 6, &[]);
    assert_eq!(
        get_s(&mut s, 0),
        core::f32::consts::FRAC_PI_4.to_bits(),
        "atan2f"
    );
}

#[test]
fn the_vector_is_claimed_and_given_back_and_a_later_claim_keeps_the_module() {
    let mut s = session();
    let entry = s
        .kernel
        .os()
        .routine_of(&VFP_SUPPORT, routine::UNDEFINED)
        .expect("active");
    assert_eq!(vector_owner(&mut s), entry);
    s.write(NAME, b"VFPSupport\0");
    ok(&mut s, MODULE, &[4, NAME]);
    let kernel_s = {
        let os = s.kernel.os();
        os.world
            .routine_address(crate::host::Routine::Exception(crate::host::UNDEFINED))
    };
    assert_eq!(vector_owner(&mut s), kernel_s, "given back");
    assert_eq!(cpacr(&mut s), Some(0));
    ok(&mut s, MODULE, &[3, NAME]);
    assert_eq!(vector_owner(&mut s), entry, "claimed again");

    let claim = ok(
        &mut s,
        CLAIM_PROCESSOR_VECTOR,
        &[1 | crate::traps::CLAIM, CODE],
    );
    assert_eq!(claim.r[1], entry);
    refused(&mut s, MODULE, &[4, NAME]);
    assert_eq!(vector_owner(&mut s), CODE);
    assert_eq!(ok(&mut s, VERSION_SWI, &[]).r[0], 18, "still there");
}

#[test]
fn a_kill_a_later_claim_refuses_keeps_the_exception_dump() {
    let mut s = session();
    ok(&mut s, CREATE_CONTEXT, &[USER | NOW, 2, DATA, 0]);
    ok(&mut s, EXCEPTION_DUMP, &[dump::CREATE, DATA]);
    ok(
        &mut s,
        CLAIM_PROCESSOR_VECTOR,
        &[1 | crate::traps::CLAIM, CODE],
    );
    s.write(NAME, b"VFPSupport\0");
    refused(&mut s, MODULE, &[4, NAME]);
    let kept = ok(&mut s, EXCEPTION_DUMP, &[dump::COPY | dump::ORIGINAL]);
    assert_ne!(kept.r[0], 0, "a copy of the dump");
    assert_eq!(kept.r[1], DATA, "made from the context it was");
}

#[test]
fn the_other_profile_s_undefined_instruction_passes_through_untouched() {
    let mut s = with_application(&SHARED, Bits::Bit26);
    let awaiting = ok(&mut s, CREATE_CONTEXT, &[USER | LAZILY, 1, 0, 0]).r[0];
    assert_eq!(
        undefined(run(&mut s, Bits::Bit26, &[0xE7F0_00F0], &[])),
        UNDEFINED_INSTRUCTION
    );
    assert_eq!(ok(&mut s, ACTIVE_CONTEXT, &[]).r[0], awaiting);
    assert_eq!(
        ok(&mut s, EXAMINE_CONTEXT, &[awaiting, 0]).r[0],
        USER | kept::OWNED | examined::AWAITING
    );
}

#[test]
fn an_activation_that_aborts_aborts_the_instruction() {
    let mut s = session();
    s.write(NAME, b"Context\0");
    let made = ok(
        &mut s,
        DYNAMIC_AREA,
        &[0, u32::MAX, 0x1000, u32::MAX, 0, 0x1000, 0, 0, NAME],
    );
    let (area, base) = (made.r[1], made.r[3]);
    ok(&mut s, CREATE_CONTEXT, &[USER | NOW, 4, base, 0]);
    ok(&mut s, CREATE_CONTEXT, &[USER | LAZILY, 4, 0, 0]);
    ok(&mut s, DYNAMIC_AREA, &[1, area]);
    match run(&mut s, Bits::Bit32, &[vadd()], &[]) {
        Outcome::Ended(Ended::Error { number, block }) => {
            assert_eq!(number, 0x8000_0002);
            assert_eq!(
                s.error_text(block),
                format!("Data transfer aborted at &{CODE:08X}").as_bytes()
            );
        }
        other => panic!("{other:?}"),
    }
}
