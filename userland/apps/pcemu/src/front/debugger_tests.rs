use super::*;
use crate::cpu::Regs;
use crate::debug::probe::{Breakpoint, Go, Reason, MOST_BREAKPOINTS};
use crate::debug::{Activity, Registers};
use crate::link::debug::{Debug, Listing, Look, Said, Seen};

fn registers() -> Registers {
    Registers {
        regs: Regs::default(),
        cycles: 0,
        activity: Activity::Executing,
        emulating: false,
        coprocessor: None,
    }
}

fn stopped(reason: Reason) -> Event<'static> {
    Event::Debug(Said::Stopped {
        reason,
        registers: registers(),
    })
}

/// A ready host with a debugger attached, the attach sent.
fn attached() -> (Host<FrontEnd, DiscardSink>, Rc<RefCell<Pipes>>) {
    let (mut host, pipes) = ready_host();
    host.attach_debugger().unwrap();
    assert_eq!(
        decoded(&sent(&mut host, &pipes)),
        [Command::Debug(Debug::Attach)]
    );
    (host, pipes)
}

/// What the host made of `events` from its worker.
fn after(
    host: &mut Host<FrontEnd, DiscardSink>,
    pipes: &Rc<RefCell<Pipes>>,
    events: &[Event<'_>],
) -> Option<Ending> {
    for event in events {
        from_worker(pipes, event);
    }
    host.on_readable();
    let mut ending = None;
    while let Some(event) = host.event() {
        if let HostEvent::Ended(end) = event {
            ending = Some(end);
        }
    }
    ending
}

const LOOK: Look = Look {
    number: 0,
    code: (0xF000, 0xE05B),
    lines: (2, 1),
    memory: (0x0040, 0),
    bytes: 4,
    stack: 1,
};

#[test]
fn a_report_with_no_debugger_or_of_another_machine_ends_the_worker() {
    assert_eq!(
        verdict(&[stopped(Reason::Break)]),
        Some(Ending::Violation("a debugger report with no debugger"))
    );
    let (mut host, pipes) = attached();
    assert_eq!(
        after(
            &mut host,
            &pipes,
            &[stopped(Reason::Break), stopped(Reason::Stepped)]
        ),
        Some(Ending::Violation("a stop of a processor already stopped"))
    );
    for (emulating, fitted) in [(true, false), (false, true)] {
        let (mut host, pipes) = attached();
        let mut registers = registers();
        registers.emulating = emulating;
        if fitted {
            registers.coprocessor = Some(crate::debug::Coprocessor {
                control: 0,
                status: 0,
                tags: 0,
                ip: 0,
                opcode: 0,
                dp: 0,
                stack: [tairix_inline::ArrayString::from_str_truncating("Empty"); 8],
            });
        }
        let report = Event::Debug(Said::Stopped {
            reason: Reason::Break,
            registers,
        });
        assert_eq!(
            after(&mut host, &pipes, &[report]),
            Some(Ending::Violation("registers of another machine")),
            "an 8088 with no 8087"
        );
    }
}

#[test]
fn a_look_is_answered_once_and_only_as_asked() {
    let (mut host, pipes) = attached();
    let lines = [crate::debug::disasm::Line {
        ip: 0xE05B,
        bytes: alloc::vec![0xFA],
        text: alloc::string::String::from("CLI"),
    }];
    let (before, after_count, encoded) = Listing::encode(&lines, 0);
    let listing = Listing::new(before, after_count, &encoded);
    let answer = |number, memory: &'static [u8]| Seen {
        number,
        listing,
        memory,
        stack: &[0x34, 0x12],
    };
    let unasked = Event::Debug(Said::Seen(answer(0, &[1, 2, 3, 4])));
    assert_eq!(
        after(&mut host, &pipes, &[unasked]),
        Some(Ending::Violation("an answer to a look never sent"))
    );
    let (mut host, pipes) = attached();
    assert_eq!(host.look(LOOK), Ok(0));
    assert_eq!(host.look(LOOK), Err(Refused::Busy));
    assert_eq!(
        decoded(&sent(&mut host, &pipes)),
        [Command::Debug(Debug::Look(LOOK))]
    );
    let short = Event::Debug(Said::Seen(answer(0, &[1, 2, 3])));
    assert_eq!(
        after(&mut host, &pipes, &[short]),
        Some(Ending::Violation("an answer that is not the one asked for"))
    );
    let (mut host, pipes) = attached();
    host.look(LOOK).unwrap();
    sent(&mut host, &pipes);
    let seen = answer(0, &[1, 2, 3, 4]);
    from_worker(&pipes, &Event::Debug(Said::Seen(seen)));
    host.on_readable();
    assert_eq!(host.event(), Some(HostEvent::Seen(seen)));
    assert_eq!(host.look(LOOK), Ok(1), "the next look");
}

#[test]
fn breakpoints_cross_one_at_a_time_and_only_ones_a_probe_can_hold() {
    let (mut bare, _) = ready_host();
    assert_eq!(
        bare.add_breakpoint(Breakpoint::Code(0)),
        Err(Refused::Detached)
    );
    let (mut host, pipes) = attached();
    let boot = Breakpoint::Code(0x7C00);
    host.add_breakpoint(boot).unwrap();
    assert_eq!(
        host.add_breakpoint(boot),
        Err(Refused::Breakpoint),
        "held already"
    );
    assert_eq!(
        host.add_breakpoint(Breakpoint::Code(1 << 20)),
        Err(Refused::Breakpoint),
        "past the space"
    );
    assert_eq!(
        host.remove_breakpoint(Breakpoint::Code(0x7C01)),
        Err(Refused::Breakpoint),
        "not held"
    );
    host.remove_breakpoint(boot).unwrap();
    assert_eq!(
        decoded(&sent(&mut host, &pipes)),
        [
            Command::Debug(Debug::Add(boot)),
            Command::Debug(Debug::Remove(boot))
        ]
    );
    for n in 0..u32::try_from(MOST_BREAKPOINTS).unwrap() {
        host.add_breakpoint(Breakpoint::Code(n)).unwrap();
    }
    assert_eq!(
        host.add_breakpoint(boot),
        Err(Refused::Breakpoint),
        "the set is full"
    );
}

#[test]
fn the_processor_is_sent_on_only_from_a_stop_and_a_reset_clears_one() {
    let (mut host, pipes) = attached();
    assert_eq!(host.debug_go(Go::Step), Err(Refused::Busy));
    host.debug_break().unwrap();
    assert_eq!(after(&mut host, &pipes, &[stopped(Reason::Break)]), None);
    assert!(host.stopped());
    host.debug_break().unwrap();
    host.debug_go(Go::StepOver).unwrap();
    assert!(!host.stopped());
    assert_eq!(
        decoded(&sent(&mut host, &pipes)),
        [
            Command::Debug(Debug::Break),
            Command::Debug(Debug::Go(Go::StepOver))
        ],
        "no break for a processor already stopped"
    );
    assert_eq!(after(&mut host, &pipes, &[stopped(Reason::Stepped)]), None);
    host.reset().unwrap();
    assert!(!host.stopped(), "the stop goes with the machine reset");
    assert_eq!(host.debug_go(Go::Continue), Err(Refused::Busy));
    host.detach_debugger().unwrap();
    assert_eq!(host.look(LOOK), Err(Refused::Detached));
}

/// The stops and answers the host passed on from `events`.
fn heard(
    host: &mut Host<FrontEnd, DiscardSink>,
    pipes: &Rc<RefCell<Pipes>>,
    events: &[Event<'_>],
) -> (usize, usize) {
    for event in events {
        from_worker(pipes, event);
    }
    host.on_readable();
    let (mut stops, mut answers) = (0, 0);
    while let Some(event) = host.event() {
        match event {
            HostEvent::Stopped { .. } => stops += 1,
            HostEvent::Seen(_) => answers += 1,
            HostEvent::Ended(end) => panic!("the worker was ended: {end:?}"),
            _ => {}
        }
    }
    (stops, answers)
}

#[test]
fn what_was_said_before_a_reset_or_a_detach_was_taken_is_dropped() {
    let (mut host, pipes) = attached();
    assert_eq!(heard(&mut host, &pipes, &[stopped(Reason::Break)]), (1, 0));
    host.look(LOOK).unwrap();
    host.reset().unwrap();
    let lines = [crate::debug::disasm::Line {
        ip: 0xE05B,
        bytes: alloc::vec![0xFA],
        text: alloc::string::String::from("CLI"),
    }];
    let (before, after_count, encoded) = Listing::encode(&lines, 0);
    let answer = Event::Debug(Said::Seen(Seen {
        number: 0,
        listing: Listing::new(before, after_count, &encoded),
        memory: &[0; 4],
        stack: &[0; 2],
    }));
    let before_reset = [answer, stopped(Reason::Stepped), Event::Debug(Said::Reset)];
    assert_eq!(
        heard(&mut host, &pipes, &before_reset),
        (0, 0),
        "of the machine reset"
    );
    assert!(!host.stopped());
    assert_eq!(
        heard(&mut host, &pipes, &[stopped(Reason::Break)]),
        (1, 0),
        "of the one after"
    );
    host.detach_debugger().unwrap();
    host.attach_debugger().unwrap();
    let before_detach = [stopped(Reason::Stepped), Event::Debug(Said::Detached)];
    assert_eq!(
        heard(&mut host, &pipes, &before_detach),
        (0, 0),
        "of the debugger taken off"
    );
    assert!(!host.stopped());
    assert_eq!(
        heard(&mut host, &pipes, &[stopped(Reason::Break)]),
        (1, 0),
        "of the new one"
    );
    assert_eq!(
        verdict(&[Event::Debug(Said::Detached)]),
        Some(Ending::Violation("a detach never asked"))
    );
    let (mut host, pipes) = attached();
    assert_eq!(
        after(&mut host, &pipes, &[Event::Debug(Said::Reset)]),
        Some(Ending::Violation("a reset never sent"))
    );
}
