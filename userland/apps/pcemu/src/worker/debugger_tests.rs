use super::*;
use crate::cpu::Seg;
use crate::debug::probe::{Breakpoint, Go, Reason};
use crate::link::debug::{Debug, Look};

/// The reset vector, `FFFF:0000`, where the processor begins.
const RESET: u32 = 0xF_FFF0;

/// A PC with no disk, a debugger on it holding `breakpoints`, the
/// machine run.
fn debugged(breakpoints: &[Breakpoint]) -> (TestWorker, Rc<RefCell<Front>>) {
    let (worker, front) = open(&setup(Template::IbmPc.config(), None), vec![]);
    let worker = worker.unwrap();
    let mut queue = front.borrow_mut();
    queue.queue(&Command::Debug(Debug::Attach));
    for &breakpoint in breakpoints {
        queue.queue(&Command::Debug(Debug::Add(breakpoint)));
    }
    queue.queue(&Command::Run);
    drop(queue);
    (worker, front)
}

/// Turns until the debugger has reported `count` stops, within a second of
/// the front-end's clock: the worker runs a slice only once the clock it is
/// paced to has moved.
fn turn_until_stops(worker: &mut TestWorker, front: &Rc<RefCell<Front>>, count: usize) {
    let deadline = front.borrow().clock + SECOND;
    while front.borrow().stops.len() < count {
        assert!(front.borrow().clock < deadline, "never stopped");
        assert_eq!(worker.turn(), None);
    }
}

fn debug(front: &Rc<RefCell<Front>>, request: Debug) {
    front.borrow_mut().queue(&Command::Debug(request));
}

fn place(registers: &crate::debug::Registers) -> (u16, u16) {
    (registers.regs.sreg(Seg::Cs), registers.regs.ip)
}

#[test]
fn a_breakpoint_holds_the_machine_still_until_it_is_sent_on() {
    let (mut worker, front) = debugged(&[Breakpoint::Code(RESET)]);
    turn_until_stops(&mut worker, &front, 1);
    let (reason, registers) = front.borrow().stops[0].clone();
    assert_eq!(reason, Reason::Breakpoint(Breakpoint::Code(RESET)));
    assert_eq!(place(&registers), (0xFFFF, 0));
    let still = worker.run.machine.now();
    debug(
        &front,
        Debug::Look(Look {
            number: 3,
            code: (0xFFFF, 0),
            lines: (0, 2),
            memory: (0xFFFF, 0),
            bytes: 5,
            stack: 2,
        }),
    );
    front.borrow_mut().clock += SECOND;
    assert_eq!(worker.turn(), None);
    assert_eq!(
        worker.run.machine.now(),
        still,
        "time stands still at a stop"
    );
    let seen = front.borrow().seen[0].clone();
    assert_eq!((seen.number, seen.before), (3, 0));
    assert_eq!(seen.lines[0], (0, String::from("JMP 0F000h:0E05Bh")));
    assert_eq!(
        seen.memory,
        [0xEA, 0x5B, 0xE0, 0x00, 0xF0],
        "the jump's bytes"
    );
    assert_eq!(seen.stack.len(), 4);
    debug(&front, Debug::Go(Go::Step));
    turn_until_stops(&mut worker, &front, 2);
    let (reason, registers) = front.borrow().stops[1].clone();
    assert_eq!(reason, Reason::Stepped);
    assert_eq!(place(&registers), (0xF000, 0xE05B), "one instruction on");
    debug(&front, Debug::Detach);
    turn_for(&mut worker, &front, SECOND / 10);
    assert!(
        worker.run.machine.now() > still,
        "and it runs on once let go"
    );
    assert_eq!(front.borrow().stops.len(), 2);
    assert_eq!(front.borrow().detached, 1, "the detach said to be taken");
}

#[test]
fn a_break_reaches_a_machine_held_still() {
    let (worker, front) = open(&setup(Template::IbmPc.config(), None), vec![]);
    let mut worker = worker.unwrap();
    debug(&front, Debug::Attach);
    debug(&front, Debug::Break);
    turn_until_stops(&mut worker, &front, 1);
    let (reason, registers) = front.borrow().stops[0].clone();
    assert_eq!(reason, Reason::Break);
    assert_eq!(place(&registers), (0xFFFF, 0), "at the first instruction");
}

#[test]
fn a_reset_keeps_the_breakpoints_but_not_the_stop() {
    let (mut worker, front) = debugged(&[Breakpoint::Code(RESET)]);
    turn_until_stops(&mut worker, &front, 1);
    front.borrow_mut().queue(&Command::Reset);
    turn_until_stops(&mut worker, &front, 2);
    let front = front.borrow();
    assert_eq!(front.resets, 1, "the reset said to be taken");
    assert_eq!(
        front.stops[1].0,
        Reason::Breakpoint(Breakpoint::Code(RESET)),
        "stopped again at the reset vector"
    );
}

#[test]
fn a_reset_is_said_while_the_probe_is_on_stopped_or_not() {
    let (mut worker, front) = debugged(&[]);
    turn_for(&mut worker, &front, SECOND / 10);
    front.borrow_mut().queue(&Command::Reset);
    turn_for(&mut worker, &front, SECOND / 10);
    assert_eq!(front.borrow().resets, 1, "said of a processor running");
    debug(&front, Debug::Detach);
    front.borrow_mut().queue(&Command::Reset);
    turn_for(&mut worker, &front, SECOND / 10);
    let front = front.borrow();
    assert_eq!((front.resets, front.detached), (1, 1), "not once it is off");
}

#[test]
fn a_request_out_of_turn_ends_the_session() {
    for (requests, why) in [
        (
            &[Debug::Go(Go::Continue)][..],
            "a debugger request out of turn",
        ),
        (
            &[Debug::Attach, Debug::Attach][..],
            "a debugger request out of turn",
        ),
        (
            &[Debug::Attach, Debug::Go(Go::Step)][..],
            "a debugger request out of turn",
        ),
        (
            &[Debug::Attach, Debug::Remove(Breakpoint::Code(0))][..],
            "a breakpoint removed that was not held",
        ),
        (
            &[
                Debug::Attach,
                Debug::Add(Breakpoint::Code(0)),
                Debug::Add(Breakpoint::Code(0)),
            ][..],
            "a breakpoint the probe cannot hold",
        ),
    ] {
        let (worker, front) = open(&setup(Template::IbmPc.config(), None), vec![]);
        let mut worker = worker.unwrap();
        for &request in requests {
            debug(&front, request);
        }
        assert_eq!(worker.turn(), Some(End::Violation(why)), "{requests:?}");
    }
}
