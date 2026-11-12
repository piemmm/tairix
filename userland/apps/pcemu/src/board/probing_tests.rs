use super::*;
use crate::bits::word;
use crate::cpu::bus::Escape;
use crate::cpu::{linear, Bus, CpuModel, Seg};
use crate::debug::probe::{Access, Direction, Hit};
use crate::machine::{Exit, Machine, Template};
use crate::time::NANOS_PER_MILLISECOND as MS;

fn probed(template: Template) -> Board {
    let config = template.config();
    let mut board = Board::new(&config).unwrap();
    board.attach_probe(Box::new(Probe::new(config.cpu)));
    board
}

/// The stop the probe makes at the next boundary, if any.
fn stop_at_boundary(board: &mut Board, model: CpuModel) -> Option<Reason> {
    board
        .probe_begins(&Cpu::new(model))
        .then(|| board.take_stop())
        .flatten()
}

/// Has `board`'s probe hold each of `breakpoints`: whether it held them all.
fn hold(board: &mut Board, breakpoints: &[Breakpoint]) -> bool {
    breakpoints
        .iter()
        .all(|&breakpoint| board.add_breakpoint(breakpoint))
}

fn watch(first: u32, access: Access) -> Breakpoint {
    Breakpoint::Memory {
        first,
        len: 4,
        access,
    }
}

#[test]
fn a_watched_page_is_the_boards_to_see_every_access_to() {
    let mut board = probed(Template::IbmXt);
    assert!(hold(&mut board, &[watch(0x5010, Access::Write)]));
    let memory = &board.planar.memory;
    assert!(memory.special(0x5000) && memory.probed(0x5FFF));
    assert_eq!(
        memory.backing(0x5000),
        None,
        "translated code may not touch it"
    );
    assert!(!memory.writable(0x5000));
    assert!(memory.backing(0x6000).is_some() && !memory.special(0x6000));
    assert!(board.detach_probe().is_some());
    assert!(!board.planar.memory.special(0x5000));
    assert!(board.planar.memory.backing(0x5000).is_some());
}

#[test]
fn a_breakpoint_needs_a_probe_and_one_it_can_hold() {
    let mut bare = Board::new(&Template::IbmXt.config()).unwrap();
    assert!(!bare.add_breakpoint(Breakpoint::Code(0)));
    let mut board = probed(Template::IbmXt);
    let boot = Breakpoint::Code(0x7C00);
    assert!(
        !board.add_breakpoint(Breakpoint::Code(1 << 20)),
        "past the space"
    );
    assert!(board.add_breakpoint(boot));
    assert!(!board.add_breakpoint(boot), "held already");
    assert_eq!(board.probe().unwrap().breakpoints(), [boot]);
    assert!(
        !board.remove_breakpoint(Breakpoint::Code(0x7C01)),
        "not held"
    );
    assert!(board.add_breakpoint(watch(0x5000, Access::Write)));
    assert!(board.planar.memory.probed(0x5000));
    assert!(board.remove_breakpoint(watch(0x5000, Access::Write)));
    assert!(!board.planar.memory.probed(0x5000), "its page unwatched");
    assert_eq!(board.probe().unwrap().breakpoints(), [boot]);
}

#[test]
fn the_processor_is_seen_reading_and_writing_but_not_fetching() {
    let mut board = probed(Template::IbmXt);
    assert!(hold(&mut board, &[watch(0x5010, Access::Any)]));
    board.fetch(0x5010, 0);
    assert_eq!(stop_at_boundary(&mut board, CpuModel::I8088), None);
    board.write(0x5011, 0xA5, 0);
    let hit = Hit {
        master: Master::Cpu,
        cycle: Cycle::Write,
        at: 0x5011,
        value: 0xA5,
    };
    assert_eq!(
        stop_at_boundary(&mut board, CpuModel::I8088),
        Some(Reason::Watched {
            watch: watch(0x5010, Access::Any),
            hit
        })
    );
    assert_eq!(board.read(0x5012, 0), 0);
    assert!(matches!(
        stop_at_boundary(&mut board, CpuModel::I8088),
        Some(Reason::Watched {
            hit: Hit {
                cycle: Cycle::Read,
                at: 0x5012,
                ..
            },
            ..
        })
    ));
}

#[test]
fn the_8087_is_seen_writing_its_result_when_its_store_lands() {
    let mut board = probed(Template::TurboXt);
    for (n, byte) in (0..).zip(2.5f64.to_le_bytes()) {
        board.write(0x500 + n, byte, 0);
    }
    assert!(hold(&mut board, &[watch(0x600, Access::Write)]));
    let escape = |board: &mut Board, modrm: u8, addr: u32, at: u64| {
        let operand = Some((addr, word(board.peek(addr), board.peek(addr + 1))));
        let escape = Escape {
            opcode: (5 << 8) | u16::from(modrm),
            address: 0x7C00,
            operand,
        };
        board.escape(escape, at);
    };
    // FLD QWORD PTR, then FSTP QWORD PTR to the watched bytes.
    escape(&mut board, 0x06, 0x500, 0);
    let t = board.time_of(1_000);
    board.advance_to(t);
    escape(&mut board, 0x1E, 0x600, 1_000);
    assert_eq!(
        stop_at_boundary(&mut board, CpuModel::V20),
        None,
        "not yet stored"
    );
    let t = board.time_of(2_000);
    board.advance_to(t);
    let Some(Reason::Watched { hit, .. }) = stop_at_boundary(&mut board, CpuModel::V20) else {
        panic!("the store was not seen");
    };
    assert_eq!(
        (hit.master, hit.cycle, hit.at),
        (Master::Coprocessor, Cycle::Write, 0x600)
    );
}

#[test]
fn ports_are_watched_on_the_bus_and_in_the_80186s_control_block() {
    let mut board = probed(Template::Workstation186);
    let port = |first, direction| Breakpoint::Port {
        first,
        last: first,
        direction,
    };
    assert!(hold(
        &mut board,
        &[port(0xFF22, Direction::Out), port(0x61, Direction::In)]
    ));
    board.output(0x61, 0x4C, 0);
    assert_eq!(
        stop_at_boundary(&mut board, CpuModel::I80186),
        None,
        "an output"
    );
    board.output_word(0xFF22, 0x8000, 0);
    let Some(Reason::Watched { hit, .. }) = stop_at_boundary(&mut board, CpuModel::I80186) else {
        panic!("the control block's EOI was not seen");
    };
    assert_eq!((hit.cycle, hit.at, hit.value), (Cycle::Out, 0xFF22, 0x00));
    board.input(0x61, 0);
    assert!(matches!(
        stop_at_boundary(&mut board, CpuModel::I80186),
        Some(Reason::Watched {
            hit: Hit {
                cycle: Cycle::In,
                at: 0x61,
                ..
            },
            ..
        })
    ));
}

/// A machine about to run `code` at `0500:0000`, interrupts disabled, a
/// probe on its board.
fn machine(code: &[u8]) -> Machine {
    let config = Template::IbmXt.config();
    let mut machine = Machine::new(&config).unwrap();
    for (n, &byte) in (0..).zip(code) {
        machine.board_mut().write(linear(0x0500, n), byte, 0);
    }
    let regs = machine.cpu_mut().regs_mut();
    regs.set_sreg(Seg::Cs, 0x0500);
    regs.ip = 0;
    machine
        .board_mut()
        .attach_probe(Box::new(Probe::new(config.cpu)));
    machine
}

#[test]
fn the_machine_stops_at_a_breakpoint_and_goes_on_from_it() {
    // MOV AX,5; INC AX; JMP $.
    let mut m = machine(&[0xB8, 0x05, 0x00, 0x40, 0xEB, 0xFE]);
    let breakpoint = Breakpoint::Code(linear(0x0500, 3));
    assert!(m.board_mut().add_breakpoint(breakpoint));
    assert_eq!(m.run_until(10 * MS), Exit::Break);
    assert_eq!(
        m.board_mut().take_stop(),
        Some(Reason::Breakpoint(breakpoint))
    );
    assert_eq!((m.cpu().regs().ip, m.cpu().regs().ax()), (3, 5));
    let stopped = m.now();
    m.go(Go::Continue);
    assert_eq!(m.run_until(stopped + MS), Exit::Deadline);
    assert_eq!(m.cpu().regs().ax(), 6);
}

#[test]
fn a_break_stops_a_halted_machine_where_it_halts() {
    let mut m = machine(&[0xF4]);
    assert_eq!(m.run_until(MS), Exit::Deadline);
    assert!(m.cpu().halted());
    m.board_mut().request_break();
    assert_eq!(m.run_until(2 * MS), Exit::Break);
    assert_eq!(m.board_mut().take_stop(), Some(Reason::Break));
    assert_eq!(m.cpu().regs().ip, 1);
}

#[test]
fn a_snapshot_of_a_probed_machine_keeps_no_probe() {
    let mut m = machine(&[0x40, 0xEB, 0xFD]);
    assert!(hold(
        m.board_mut(),
        &[
            Breakpoint::Code(linear(0x0500, 1)),
            watch(0x7000, Access::Write)
        ]
    ));
    assert_eq!(m.run_until(MS), Exit::Break);
    let bytes = m.snapshot(b"probed").unwrap();
    let mut twin = Machine::new(&Template::IbmXt.config()).unwrap();
    twin.restore(&bytes, b"probed").unwrap();
    assert!(twin.board().probe().is_none());
    assert!(!twin.board().planar.memory.special(0x7000));
    // The stop's boundary had its interrupts taken: the twin goes on from it
    // as the machine itself does.
    m.go(Go::Continue);
    assert!(m.board_mut().detach_probe().is_some());
    let until = m.now() + MS;
    m.run_until(until);
    twin.run_until(until);
    assert_eq!(twin.cpu().regs(), m.cpu().regs());
}
