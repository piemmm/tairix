//! Contexts: each one's own kept from every other's, the session's shared,
//! a context set aside only at rest, an ended one's own given back, a launch
//! refused its room making nothing, and a switch or an end taking none.

use alloc::boxed::Box;
use alloc::rc::Rc;
use core::any::Any;
use core::cell::Cell;

use tairix_fuzzseed::meter::{metered, refusing_after};
use tairix_riscos_arm::cpu::State;
use tairix_riscos_arm::psr::{self, Mode};

use super::*;
use crate::env::MEMORY_LIMIT;
use crate::input::InputEvent;
use crate::kheap::KernelHeap;
use crate::memory::layout::{APPLICATION, RISC_OS_5, SHARED};
use crate::memory::Bits;
use crate::os::{Outcome, Regs, Wait};
use crate::swi::os::{BYTE, CHANGE_REDIRECTION, FIND, READ_C, WRITE_C};
use crate::swi::X;
use crate::testkit::{with_application, OneFile, Session, CODE, DATA, SLOT_BYTES};

/// FPEmulator's SWIs.
const FP_ACTIVATE: u32 = crate::swi::chunk::FP_EMULATOR + 2;
const FP_CHANGE: u32 = crate::swi::chunk::FP_EMULATOR + 3;
/// FPEmulator's name for its default context.
const FP_DEFAULT: u32 = u32::MAX;

fn session() -> Session {
    with_application(&RISC_OS_5, Bits::Bit32)
}

fn registers(low: &[u32]) -> [u32; 13] {
    let mut r = [0; 13];
    r[..low.len()].copy_from_slice(low);
    r
}

/// SWI `number` from User mode with `low` from R0 on, which must not fail.
fn ok(s: &mut Session, number: u32, low: &[u32]) -> Regs {
    let (regs, _) = s.user_swi(X | number, registers(low));
    assert!(!regs.flag(psr::V), "SWI &{number:X}");
    regs
}

/// `OS_Byte` variable `number`.
fn variable(s: &mut Session, number: u8) -> u8 {
    let r1 = ok(s, BYTE, &[u32::from(number), 0, 0xFF]).r[1];
    u8::try_from(r1).expect("a byte")
}

fn set_variable(s: &mut Session, number: u8, value: u8) {
    ok(s, BYTE, &[u32::from(number), u32::from(value), 0]);
}

/// The characters in the keyboard buffer, as `OS_Byte` 128 counts them.
fn typed(s: &mut Session) -> u32 {
    ok(s, BYTE, &[128, 0xFF]).r[1]
}

fn memory_limit(s: &mut Session) -> u32 {
    s.kernel.os().world.env.slot(MEMORY_LIMIT).address
}

fn mode(s: &mut Session) -> Mode {
    s.kernel.os().world.processor.cpu().mode()
}

/// A new context, current, with an application slot of `bytes` of `bits`.
fn launched(s: &mut Session, bits: Bits, bytes: u32) -> ContextId {
    let id = s.kernel.os().launch_context().expect("launched");
    s.application(bits, bytes);
    id
}

fn switch(s: &mut Session, id: ContextId) {
    s.kernel.os().switch_context(id).expect("switched");
}

#[test]
fn each_context_has_its_own_processor_slot_and_environment() {
    let mut s = session();
    s.write(DATA, b"first");
    s.enter(Bits::Bit32, CODE, [1; 13]);
    let first = s.kernel.os().context();
    let second = s.kernel.os().launch_context().expect("launched");
    assert_ne!(first, second);
    assert_eq!(s.kernel.os().context(), second);
    assert_eq!(
        s.kernel.memory().current_slot(),
        None,
        "no slot of its own yet"
    );
    assert_eq!(memory_limit(&mut s), APPLICATION, "no application memory");
    assert_eq!(mode(&mut s), Mode::Supervisor, "as a reset leaves it");
    s.application(Bits::Bit32, 2 * SLOT_BYTES);
    s.write(DATA, b"other");
    s.enter(Bits::Bit32, CODE + 8, [2; 13]);
    switch(&mut s, first);
    assert_eq!(s.read(DATA, 5), b"first");
    assert_eq!(memory_limit(&mut s), APPLICATION + SLOT_BYTES);
    let state = s.kernel.state();
    assert_eq!((state.pc, state.user[0]), (CODE, 1));
    switch(&mut s, second);
    assert_eq!(s.read(DATA, 5), b"other");
    assert_eq!(memory_limit(&mut s), APPLICATION + 2 * SLOT_BYTES);
    assert_eq!(s.kernel.state().user[0], 2);
}

#[test]
fn a_context_runs_under_its_own_profile() {
    let mut s = crate::testkit::boot(&SHARED);
    s.application(Bits::Bit32, SLOT_BYTES);
    let first = s.kernel.os().context();
    let second = launched(&mut s, Bits::Bit26, SLOT_BYTES);
    assert_eq!(s.kernel.bits(), Bits::Bit26);
    switch(&mut s, first);
    assert_eq!(s.kernel.bits(), Bits::Bit32);
    assert_eq!(mode(&mut s), Mode::User);
    switch(&mut s, second);
    assert_eq!(s.kernel.bits(), Bits::Bit26);
    assert_eq!(mode(&mut s), Mode::User26);
}

#[test]
fn typing_is_read_only_in_its_own_context() {
    let mut s = session();
    ok(&mut s, BYTE, &[138, 0, u32::from(b'a')]);
    let first = s.kernel.os().context();
    let second = launched(&mut s, Bits::Bit32, SLOT_BYTES);
    assert_eq!(typed(&mut s), 0, "nothing typed in the new context");
    ok(&mut s, BYTE, &[138, 0, u32::from(b'b')]);
    ok(&mut s, BYTE, &[138, 0, u32::from(b'c')]);
    switch(&mut s, first);
    assert_eq!(typed(&mut s), 1);
    assert_eq!(ok(&mut s, READ_C, &[]).r[0], u32::from(b'a'));
    switch(&mut s, second);
    assert_eq!(typed(&mut s), 2);
}

#[test]
fn a_screen_s_and_keyboard_s_variables_are_each_context_s_and_the_machine_s_shared() {
    const CURSOR_EDIT: u8 = 0xED;
    const PRINTER_IGNORE: u8 = 0xF6;
    let mut s = session();
    set_variable(&mut s, CURSOR_EDIT, 1);
    set_variable(&mut s, PRINTER_IGNORE, 0x42);
    let first = s.kernel.os().context();
    let second = launched(&mut s, Bits::Bit32, SLOT_BYTES);
    assert_eq!(variable(&mut s, CURSOR_EDIT), 0, "as a reset leaves it");
    assert_eq!(variable(&mut s, PRINTER_IGNORE), 0x42, "the machine's");
    set_variable(&mut s, CURSOR_EDIT, 2);
    set_variable(&mut s, PRINTER_IGNORE, 0x43);
    switch(&mut s, first);
    assert_eq!(variable(&mut s, CURSOR_EDIT), 1);
    assert_eq!(variable(&mut s, PRINTER_IGNORE), 0x43);
    switch(&mut s, second);
    assert_eq!(variable(&mut s, CURSOR_EDIT), 2);
}

#[test]
fn a_new_context_s_variables_follow_the_configuration_as_it_stands() {
    const REPEAT_DELAY: u8 = 0xC4;
    let mut s = session();
    set_variable(&mut s, REPEAT_DELAY, 99);
    assert!(s.kernel.os().world.cmos.write(crate::cmos::KEY_DELAY, 7));
    launched(&mut s, Bits::Bit32, SLOT_BYTES);
    assert_eq!(variable(&mut s, REPEAT_DELAY), 7, "the configured delay");
}

/// A pattern for zero page offset `offset` in context `which`.
fn pattern(offset: u32, which: u8) -> u8 {
    offset.to_le_bytes()[0].wrapping_mul(3) ^ which
}

/// The bytes of each of a context's own ranges of zero page.
fn own_zero_page(s: &mut Session) -> alloc::vec::Vec<alloc::vec::Vec<u8>> {
    ZERO_PAGE
        .iter()
        .map(|&(offset, length)| s.read(HIGH_ZERO_PAGE + offset, length as usize))
        .collect()
}

#[test]
fn a_context_s_own_zero_page_is_its_alone_and_the_rest_is_shared() {
    let mut s = session();
    let at_boot = own_zero_page(&mut s);
    let shared = HIGH_ZERO_PAGE + zeropage::DEBUGGER_SPACE;
    for (offset, length) in ZERO_PAGE {
        for at in offset..offset + length {
            s.write(HIGH_ZERO_PAGE + at, &[pattern(at, 1)]);
        }
    }
    s.write(shared, b"seen");
    let first = s.kernel.os().context();
    let second = launched(&mut s, Bits::Bit32, SLOT_BYTES);
    assert_eq!(
        own_zero_page(&mut s),
        at_boot,
        "as the boot's context began"
    );
    for (offset, length) in ZERO_PAGE {
        for at in offset..offset + length {
            s.write(HIGH_ZERO_PAGE + at, &[pattern(at, 2)]);
        }
    }
    assert_eq!(s.read(shared, 4), b"seen", "the session's");
    switch(&mut s, first);
    for (offset, length) in ZERO_PAGE {
        for at in offset..offset + length {
            assert_eq!(s.read(HIGH_ZERO_PAGE + at, 1), [pattern(at, 1)], "&{at:X}");
        }
    }
    switch(&mut s, second);
    for (offset, _) in ZERO_PAGE {
        assert_eq!(s.read(HIGH_ZERO_PAGE + offset, 1), [pattern(offset, 2)]);
    }
}

#[test]
fn the_escape_condition_is_each_context_s() {
    let mut s = session();
    ok(&mut s, BYTE, &[125]);
    let escape = HIGH_ZERO_PAGE + zeropage::ESC_STATUS;
    assert_ne!(s.read(escape, 1)[0] & 0x40, 0, "set");
    let first = s.kernel.os().context();
    launched(&mut s, Bits::Bit32, SLOT_BYTES);
    assert_eq!(s.read(escape, 1)[0] & 0x40, 0, "none in the new context");
    switch(&mut s, first);
    assert_ne!(s.read(escape, 1)[0] & 0x40, 0, "still set");
}

#[test]
fn the_key_handler_s_workspace_and_the_mouse_buffer_are_each_context_s() {
    let mut s = session();
    let workspace = s.kernel.os().world.keyboard.workspace;
    s.write(workspace, &[0x5A; HANDLER_WORKSPACE as usize]);
    s.kernel
        .os()
        .pointer_moved(100, 100, 4)
        .expect("a button pressed");
    let packets = |s: &mut Session| ok(s, BYTE, &[128, 0xFF ^ crate::input::MOUSE]).r[1];
    let held = packets(&mut s);
    assert_ne!(held, 0, "a packet buffered");
    let first = s.kernel.os().context();
    launched(&mut s, Bits::Bit32, SLOT_BYTES);
    assert_eq!(packets(&mut s), 0, "the new context's mouse buffer empty");
    assert_eq!(
        s.read(workspace, HANDLER_WORKSPACE as usize),
        [0; HANDLER_WORKSPACE as usize],
        "its key handler's workspace its own"
    );
    switch(&mut s, first);
    assert_eq!(packets(&mut s), held);
    assert_eq!(s.read(workspace, 1), [0x5A]);
}

#[test]
fn a_swi_waiting_for_input_waits_in_its_own_context() {
    let mut s = session();
    assert_eq!(
        s.start_swi(X | READ_C, [0; 13]),
        Outcome::Waiting(Wait::Input { until: None })
    );
    let first = s.kernel.os().context();
    launched(&mut s, Bits::Bit32, SLOT_BYTES);
    assert_eq!(typed(&mut s), 0, "the other context's SWIs run meanwhile");
    switch(&mut s, first);
    s.arrive(0, InputEvent::Key(b'q'));
    assert_eq!(s.resume(), Outcome::Budget);
    assert_eq!(s.regs().r[0], u32::from(b'q'), "the waiting SWI's");
}

#[test]
fn a_context_is_set_aside_only_at_rest_and_in_user_mode() {
    let mut s = session();
    let first = s.kernel.os().context();
    let second = launched(&mut s, Bits::Bit32, SLOT_BYTES);
    switch(&mut s, first);
    s.kernel.os().world.running = true;
    let user = s.kernel.state();
    let privileged = State {
        cpsr: Mode::Supervisor.bits(),
        ..user
    };
    s.kernel
        .os()
        .world
        .processor
        .load(Bits::Bit32, &privileged)
        .expect("supervisor");
    assert_eq!(
        s.kernel.os().switch_context(second),
        Err(ContextError::Busy),
        "its program privileged"
    );
    s.kernel.os().world.transfer = Some(Transfer {
        bits: Bits::Bit32,
        state: user,
    });
    assert!(s.kernel.os().switchable(), "about to be replaced");
    s.kernel.os().world.transfer = None;
    s.kernel
        .os()
        .world
        .processor
        .load(Bits::Bit32, &user)
        .expect("user");
    s.kernel.os().world.depth = 1;
    assert!(!s.kernel.os().switchable(), "a call in progress");
    s.kernel.os().world.depth = 0;
    s.kernel.os().world.kernel_calls = 1;
    assert!(!s.kernel.os().switchable(), "a SWI the kernel performs");
    s.kernel.os().world.kernel_calls = 0;
    switch(&mut s, second);
    assert_eq!(
        s.kernel.os().switch_context(second),
        Err(ContextError::Current)
    );
}

/// What a context's value records of its end.
struct Marker(Rc<Cell<u32>>);

fn marker_ended(_: &mut Os<'_>, value: Box<dyn Any>) {
    if let Ok(marker) = value.downcast::<Marker>() {
        marker.0.set(marker.0.get() + 1);
    }
}

#[test]
fn an_ended_context_gives_back_its_slot_buffers_and_values() {
    let mut s = session();
    let first = s.kernel.os().context();
    let second = launched(&mut s, Bits::Bit32, SLOT_BYTES);
    let ended = Rc::new(Cell::new(0));
    s.kernel
        .os()
        .hold_context_extension(Marker(Rc::clone(&ended)), marker_ended)
        .expect("held");
    let slot = s.kernel.memory().current_slot().expect("its slot");
    let node = s.kernel.os().world.env.buffers.error;
    let size = s
        .kernel
        .os()
        .heap_block_size(KernelHeap::System, node)
        .expect("its buffers' node");
    let (_, during) = s
        .kernel
        .os()
        .heap_describe(KernelHeap::System)
        .expect("described");
    s.kernel.os().end_context(first).expect("ended");
    assert_eq!(ended.get(), 1, "its value's end run once");
    assert_eq!(s.kernel.os().context(), first);
    assert_eq!(
        s.kernel.memory().slot_bits(slot),
        None,
        "its slot destroyed"
    );
    let (_, after) = s
        .kernel
        .os()
        .heap_describe(KernelHeap::System)
        .expect("described");
    assert_eq!(after - during, size, "its buffers given back");
    assert_eq!(
        s.kernel.os().switch_context(second),
        Err(ContextError::Unknown)
    );
    assert!(
        s.kernel.memory().current_slot().is_some(),
        "the first's slot"
    );
}

#[test]
fn a_context_s_name_is_never_given_to_another() {
    let mut s = session();
    let first = s.kernel.os().context();
    let second = launched(&mut s, Bits::Bit32, SLOT_BYTES);
    s.kernel.os().end_context(first).expect("ended");
    let third = launched(&mut s, Bits::Bit32, SLOT_BYTES);
    assert_ne!(third, second);
    assert_eq!(third.index, second.index, "its place taken again");
    switch(&mut s, first);
    assert_eq!(
        s.kernel.os().switch_context(second),
        Err(ContextError::Unknown)
    );
    switch(&mut s, third);
}

#[derive(Debug, PartialEq, Eq)]
struct Counter(u32);

#[derive(Debug, PartialEq, Eq)]
struct Name(&'static str);

fn dropped(_: &mut Os<'_>, _: Box<dyn Any>) {}

#[test]
fn a_value_held_for_a_context_is_found_only_in_it() {
    let mut s = session();
    let mut os = s.kernel.os();
    os.hold_context_extension(Counter(1), dropped)
        .expect("held");
    os.hold_extension(Name("session")).expect("held");
    let first = os.context();
    let second = launched(&mut s, Bits::Bit32, SLOT_BYTES);
    let mut os = s.kernel.os();
    assert_eq!(os.extension::<Counter>(), None, "the other context's");
    assert_eq!(os.extension::<Name>(), Some(&mut Name("session")));
    os.hold_context_extension(Counter(2), dropped)
        .expect("held");
    switch(&mut s, first);
    assert_eq!(s.kernel.os().extension::<Counter>(), Some(&mut Counter(1)));
    switch(&mut s, second);
    assert_eq!(s.kernel.os().extension::<Counter>(), Some(&mut Counter(2)));
    assert_eq!(
        s.kernel.os().release_extension::<Counter>(),
        Some(Counter(2)),
        "the current context's handed back"
    );
    switch(&mut s, first);
    assert_eq!(
        s.kernel.os().extension::<Counter>(),
        None,
        "every context's let go of"
    );
}

#[test]
fn each_context_has_its_own_fpa_and_default_floating_point_context() {
    let mut s = session();
    s.kernel.os().set_fpa_double(0, 2.5f64.to_bits());
    let fpsr = s.kernel.os().fpsr();
    let first = s.kernel.os().context();
    let second = launched(&mut s, Bits::Bit32, SLOT_BYTES);
    assert_eq!(s.kernel.os().fpa_double(0), Ok(0), "a fresh FPA");
    assert_eq!(s.kernel.os().fpsr(), crate::fpemulator::FRESH_FPSR);
    s.kernel.os().set_fpa_double(0, 7.0f64.to_bits());
    assert_eq!(ok(&mut s, FP_CHANGE, &[0]).r[0], FP_DEFAULT);
    assert!(!s.kernel.os().world.processor.cpu().fpa_active());
    switch(&mut s, first);
    assert!(s.kernel.os().world.processor.cpu().fpa_active());
    assert_eq!(s.kernel.os().fpa_double(0), Ok(2.5f64.to_bits()));
    assert_eq!(s.kernel.os().fpsr(), fpsr);
    ok(&mut s, FP_CHANGE, &[0]);
    switch(&mut s, second);
    assert!(!s.kernel.os().world.processor.cpu().fpa_active());
    ok(&mut s, FP_ACTIVATE, &[FP_DEFAULT]);
    assert_eq!(
        s.kernel.os().fpa_double(0),
        Ok(7.0f64.to_bits()),
        "its own default context, not the other's"
    );
}

#[test]
fn vfp_support_s_active_context_is_each_context_s_and_the_unit_let_go_at_a_switch() {
    let mut s = session();
    s.kernel.os().world.vfp_support.active = DATA;
    let first = s.kernel.os().context();
    launched(&mut s, Bits::Bit32, SLOT_BYTES);
    assert_eq!(
        s.kernel.os().world.vfp_support.active,
        crate::vfpsupport::NO_CONTEXT
    );
    switch(&mut s, first);
    assert_eq!(s.kernel.os().world.vfp_support.active, DATA);
}

#[test]
fn a_launch_from_a_context_not_at_rest_is_refused_and_makes_nothing() {
    let mut s = session();
    let (_, before) = s
        .kernel
        .os()
        .heap_describe(KernelHeap::System)
        .expect("described");
    s.kernel.os().world.depth = 1;
    assert_eq!(s.kernel.os().launch_context(), Err(ContextError::Busy));
    s.kernel.os().world.depth = 0;
    let (_, after) = s
        .kernel
        .os()
        .heap_describe(KernelHeap::System)
        .expect("described");
    assert_eq!(after, before);
}

/// The current slot's one file opened for output: it, and its handle.
fn opened(s: &mut Session) -> (OneFile, u32) {
    let file = s.one_file(b"");
    s.write(DATA, b"out\0");
    let handle = ok(s, FIND, &[0x80, DATA]).r[0];
    (file, handle)
}

/// The current context's output redirected to `handle`, none for nought.
fn redirect_output(s: &mut Session, handle: u32) {
    ok(s, CHANGE_REDIRECTION, &[0x100, handle]);
}

#[test]
fn one_context_s_output_redirection_passes_another_s_output_on() {
    let mut s = session();
    s.capture_output();
    let (file, handle) = opened(&mut s);
    let first = s.kernel.os().context();
    redirect_output(&mut s, handle);
    launched(&mut s, Bits::Bit32, SLOT_BYTES);
    s.capture_here();
    ok(&mut s, WRITE_C, &[u32::from(b'x')]);
    assert_eq!(
        s.output(),
        b"x",
        "written as though nothing were redirected"
    );
    switch(&mut s, first);
    ok(&mut s, WRITE_C, &[u32::from(b'y')]);
    assert_eq!(file.bytes(&mut s), b"y", "the first's to its file");
    assert!(s.output().is_empty());
}

#[test]
fn one_context_ending_its_output_redirection_leaves_another_s_in_force() {
    let mut s = session();
    let (file, handle) = opened(&mut s);
    let first = s.kernel.os().context();
    redirect_output(&mut s, handle);
    launched(&mut s, Bits::Bit32, SLOT_BYTES);
    let (_, own) = opened(&mut s);
    redirect_output(&mut s, own);
    redirect_output(&mut s, 0);
    switch(&mut s, first);
    ok(&mut s, WRITE_C, &[u32::from(b'z')]);
    assert_eq!(file.bytes(&mut s), b"z", "still to its file");
}

/// What a call refused its room leaves as it was: the current context, and
/// the system heap a context's making takes from.
fn held(s: &mut Session) -> (ContextId, (u32, u32)) {
    let mut os = s.kernel.os();
    let heap = os.heap_describe(KernelHeap::System).expect("described");
    (os.context(), heap)
}

#[test]
fn a_launch_refused_its_room_from_any_request_makes_nothing() {
    let mut s = session();
    let (launched, taken) = metered(|| s.kernel.os().launch_context());
    assert!(launched.is_ok() && taken.allocations > 0, "{launched:?}");
    for granted in 0..taken.allocations {
        let mut s = session();
        let before = held(&mut s);
        let Err(error) = refusing_after(granted, || s.kernel.os().launch_context()) else {
            continue;
        };
        assert!(
            matches!(error, ContextError::NoMemory | ContextError::Start(_)),
            "{error:?} at request {granted}"
        );
        assert_eq!(held(&mut s), before, "made at request {granted}");
    }
}

#[test]
fn a_switch_and_an_end_take_no_room_from_the_host() {
    // So that under pressure no switch, and no launch's undoing, fails.
    let mut s = session();
    let first = s.kernel.os().context();
    let second = launched(&mut s, Bits::Bit32, SLOT_BYTES);
    let ((), switching) = metered(|| switch(&mut s, first));
    switch(&mut s, second);
    let (ended, ending) = metered(|| s.kernel.os().end_context(first));
    assert_eq!(ended, Ok(()));
    assert_eq!((switching.allocations, ending.allocations), (0, 0));
}
