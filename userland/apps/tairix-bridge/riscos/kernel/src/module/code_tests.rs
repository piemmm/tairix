//! A built-in's guest code: laid after its routines' entries and branching to
//! them, and the hand-off to User mode and the charged work its routines lean
//! on.

use alloc::vec::Vec;

use tairix_riscos_arm::psr;

use super::Builtin;
use crate::memory::layout::RISC_OS_5;
use crate::memory::{Bits, MemoryError};
use crate::os::{Entry, Fail, Os, Outcome, Regs, Target};
use crate::testkit::{asm, boot_with, Session, CODE};

/// A built-in in the inventory's Squash's row, which no crate provides yet.
static CODED: Builtin = Builtin {
    title: "Squash",
    help_name: None,
    bridge: false,
    swis: None,
    services: None,
    commands: &[],
    init: None,
    finalise: None,
    start: None,
    routines: &[answers, leaves, works],
    vectors: &[],
    resources: &[],
    code: Some(branches),
};

/// The word after the code's branches.
const MARK: u32 = 0xC0DE_C0DE;
/// What `works` asks of the host.
const STEPS: u32 = 1000;

/// A branch to each routine, then [`MARK`].
fn branches(first: i32) -> Result<Vec<u32>, MemoryError> {
    let mut words = Vec::new();
    words.try_reserve(4)?;
    for k in 0..3 {
        words.push(asm::b(4 * k, first.cast_unsigned().wrapping_add(8 * k)));
    }
    words.push(MARK);
    Ok(words)
}

#[expect(clippy::unnecessary_wraps, reason = "a routine's signature")]
fn answers(_os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = 42;
    Ok(())
}

/// Leaves for R1's address with R2 seven and Z set.
fn leaves(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut registers = [0; 15];
    registers[..13].copy_from_slice(&regs.r);
    registers[2] = 7;
    let bits = os.caller_bits();
    Err(os.continue_in_user(bits, regs.r[1], registers, psr::Z))
}

fn works(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    for _ in 0..STEPS {
        os.yield_point()?;
    }
    Ok(())
}

/// The session's built-ins beside the kernel's.
static MODULES: [&Builtin; 1] = [&CODED];

/// A session with [`CODED`] and an application, and the code's address.
fn coded() -> (Session, u32) {
    let mut s = boot_with(&RISC_OS_5, tairix_tz::Zone::UTC, 0, &MODULES);
    s.application(Bits::Bit32, 0x1_0000);
    let code = s.kernel.os().guest_code_of(&CODED).expect("active");
    (s, code)
}

/// The register [`calls`]'s code jumps through.
const TO: usize = 3;

/// Code at [`CODE`] that jumps to R3 with R14 the word after the jump, then
/// `after`; the host pages are beyond a branch's reach.
fn calls(s: &mut Session, after: &[u32]) {
    let to = u32::try_from(TO).expect("a register's number");
    s.poke(CODE, &[asm::add(14, 15, 0), asm::mov_reg(15, to)]);
    s.poke(CODE + 8, after);
}

/// The registers that send [`calls`]'s code to `to`.
fn jumping_to(to: u32) -> [u32; 13] {
    let mut registers = [0; 13];
    registers[TO] = to;
    registers
}

#[test]
fn the_code_lies_after_the_routines_and_reaches_each_through_its_entry() {
    let (mut s, code) = coded();
    assert_eq!(s.peek(code + 12), MARK, "the words emitted");
    calls(&mut s, &[asm::b(CODE + 8, CODE + 8)]);
    s.enter(Bits::Bit32, CODE, jumping_to(code));
    assert_eq!(s.run(1000), Outcome::Budget);
    assert_eq!(s.kernel.state().pc, CODE + 8, "returned through R14");
    assert_eq!(s.regs().r[0], 42);
}

#[test]
fn a_routine_entered_from_user_mode_leaves_for_code_of_its_choosing() {
    let (mut s, code) = coded();
    calls(&mut s, &[]);
    let target = CODE + 0x40;
    s.poke(target, &[asm::b(target, target)]);
    let mut registers = jumping_to(code + 4);
    registers[1] = target;
    s.enter(Bits::Bit32, CODE, registers);
    assert_eq!(s.run(1000), Outcome::Budget);
    let state = s.kernel.state();
    assert_eq!(state.pc, target);
    assert_eq!(state.user[2], 7);
    assert_ne!(state.cpsr & psr::Z, 0, "its flags");
    assert!(!psr::Mode::from_bits(state.cpsr).is_some_and(psr::Mode::privileged));
}

#[test]
fn a_routine_called_within_a_call_cannot_leave_and_fails_instead() {
    let (mut s, code) = coded();
    let mut os = s.kernel.os();
    let mut regs = Regs::default();
    regs.r[1] = CODE;
    let target = Target {
        address: code + 4,
        bits: Bits::Bit32,
    };
    os.call(target, &mut regs, Entry::Supervisor)
        .expect("came back");
    assert_ne!(regs.flags & psr::V, 0, "refused");
}

#[test]
fn a_routine_s_host_work_is_taken_from_the_run_s_instructions() {
    let (mut s, code) = coded();
    calls(&mut s, &[asm::mov(5, 1), asm::b(CODE + 12, CODE + 12)]);
    s.enter(Bits::Bit32, CODE, jumping_to(code + 8));
    assert_eq!(s.run(100), Outcome::Budget);
    assert_eq!(s.kernel.state().pc, CODE + 8, "the work spent the run");
    assert_eq!(s.regs().r[5], 0);
    assert_eq!(s.run(100), Outcome::Budget);
    assert_eq!(s.regs().r[5], 1, "the next run goes on");
}
