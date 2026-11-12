use super::super::bench::{machine, Bench, CODE};
use super::*;
use crate::cpu::{flag, Stop};
use alloc::vec;
use alloc::vec::Vec;

const HANDLER: u16 = 0x0500;

/// Runs until the probe stops the processor, within a bound of cycles.
fn run(cpu: &mut Cpu, bus: &mut Bench) -> Reason {
    let limit = cpu.cycles() + 100_000;
    assert_eq!(cpu.run(bus, limit), Stop::Break, "the probe stopped it");
    bus.probe.take_stop().expect("a reason")
}

fn at(cpu: &Cpu) -> (u16, u16) {
    (cpu.regs().sreg(Seg::Cs), cpu.regs().ip)
}

#[test]
fn a_code_breakpoint_stops_before_its_instruction_and_lets_it_go_on() {
    // NOP; NOP; INC AX; NOP.
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &[0x90, 0x90, 0x40, 0x90]);
    let breakpoint = Breakpoint::Code(linear(CODE, 2));
    assert!(bus.probe.set(&[breakpoint]));
    assert!(bus.probe.watches_instructions());
    assert_eq!(run(&mut cpu, &mut bus), Reason::Breakpoint(breakpoint));
    assert_eq!(at(&cpu), (CODE, 2));
    assert_eq!(cpu.regs().ax(), 0, "not yet run");
    bus.go(Go::Continue, &cpu);
    let limit = cpu.cycles() + 50;
    assert_eq!(cpu.run(&mut bus, limit), Stop::Limit, "not stopped again");
    assert_eq!(cpu.regs().ax(), 1);
}

#[test]
fn a_step_runs_one_instruction() {
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &[0x90, 0x40, 0x40, 0x90]);
    bus.probe.request_break();
    assert_eq!(run(&mut cpu, &mut bus), Reason::Break);
    assert_eq!(at(&cpu), (CODE, 0));
    for (ip, ax) in [(1, 0), (2, 1), (3, 2)] {
        bus.go(Go::Step, &cpu);
        assert_eq!(run(&mut cpu, &mut bus), Reason::Stepped);
        assert_eq!((cpu.regs().ip, cpu.regs().ax()), (ip, ax));
    }
}

#[test]
fn a_prefixed_instruction_is_one_step() {
    // ES: REP STOSB with CX = 3, then INC AX.
    let (mut cpu, mut bus) = machine(CpuModel::I8086, &[0x26, 0xF3, 0xAA, 0x40]);
    cpu.regs_mut().gpr[gpr::CX] = 3;
    cpu.regs_mut().gpr[gpr::DI] = 0x0400;
    bus.probe.request_break();
    run(&mut cpu, &mut bus);
    bus.go(Go::Step, &cpu);
    assert_eq!(run(&mut cpu, &mut bus), Reason::Stepped);
    assert_eq!((cpu.regs().ip, cpu.regs().cx()), (3, 0));
}

/// CALL 0010h; NOP, a procedure at 10h: CALL 0020h; INC AX; RET, and at
/// 20h: INC AX; RET.
const CALLS: [u8; 0x23] = {
    let mut code = [0x90; 0x23];
    code[0] = 0xE8;
    code[1] = 0x0D;
    code[2] = 0x00;
    code[0x10] = 0xE8;
    code[0x11] = 0x0D;
    code[0x12] = 0x00;
    code[0x13] = 0x40;
    code[0x14] = 0xC3;
    code[0x20] = 0x40;
    code[0x21] = 0xC3;
    code
};

#[test]
fn a_step_over_a_call_runs_all_it_calls() {
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &CALLS);
    bus.probe.request_break();
    run(&mut cpu, &mut bus);
    bus.go(Go::StepOver, &cpu);
    assert_eq!(run(&mut cpu, &mut bus), Reason::Stepped);
    assert_eq!((cpu.regs().ip, cpu.regs().ax()), (3, 2));
    assert_eq!(cpu.regs().sp(), 0x0100, "back in the caller's frame");
}

#[test]
fn a_breakpoint_in_what_a_step_over_calls_stops_it_first() {
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &CALLS);
    let inside = Breakpoint::Code(linear(CODE, 0x20));
    assert!(bus.probe.set(&[inside]));
    bus.probe.request_break();
    run(&mut cpu, &mut bus);
    bus.go(Go::StepOver, &cpu);
    assert_eq!(run(&mut cpu, &mut bus), Reason::Breakpoint(inside));
    assert_eq!(cpu.regs().ip, 0x20);
}

#[test]
fn a_step_out_stops_where_the_procedure_returns_not_where_its_calls_do() {
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &CALLS);
    let procedure = Breakpoint::Code(linear(CODE, 0x10));
    assert!(bus.probe.set(&[procedure]));
    assert_eq!(run(&mut cpu, &mut bus), Reason::Breakpoint(procedure));
    assert!(bus.probe.set(&[]));
    bus.go(Go::StepOut, &cpu);
    assert_eq!(run(&mut cpu, &mut bus), Reason::Stepped);
    assert_eq!((cpu.regs().ip, cpu.regs().ax()), (3, 2));
}

#[test]
fn a_step_out_from_the_return_itself_stops_where_it_returns() {
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &CALLS);
    assert!(bus.probe.set(&[Breakpoint::Code(linear(CODE, 0x14))]));
    run(&mut cpu, &mut bus);
    assert!(bus.probe.set(&[]));
    bus.go(Go::StepOut, &cpu);
    assert_eq!(run(&mut cpu, &mut bus), Reason::Stepped);
    assert_eq!(cpu.regs().ip, 3);
}

#[test]
fn an_interrupt_breakpoint_stops_at_its_handler_for_the_function_asked() {
    // MOV AH,4Ch; INT 21h, its handler a NOP.
    let code = [0xB4, 0x4C, 0xCD, 0x21, 0x90];
    for (ah, stops) in [(None, true), (Some(0x4C), true), (Some(0x09), false)] {
        let (mut cpu, mut bus) = machine(CpuModel::I8088, &code);
        bus.vector_to(0x21, HANDLER, 0);
        bus.load(HANDLER, 0, &[0x90, 0x90, 0x90]);
        let breakpoint = Breakpoint::Interrupt { vector: 0x21, ah };
        assert!(bus.probe.set(&[breakpoint]));
        assert!(!bus.probe.watches_instructions(), "translated code may run");
        let limit = cpu.cycles() + 200;
        let stop = cpu.run(&mut bus, limit);
        assert_eq!(stop == Stop::Break, stops, "{ah:?}");
        if stops {
            assert_eq!(bus.probe.take_stop(), Some(Reason::Breakpoint(breakpoint)));
            assert_eq!(at(&cpu), (HANDLER, 0), "before its first instruction");
        }
    }
}

#[test]
fn a_step_runs_through_a_handler_a_pin_enters_on_the_way() {
    // NOP; NOP, with the timer's handler INC BX; IRET due on the way.
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &[0x90, 0x90, 0x90]);
    cpu.regs_mut().flags |= flag::IF;
    bus.vector_to(8, HANDLER, 0);
    bus.load(HANDLER, 0, &[0x43, 0xCF]);
    bus.vector = 8;
    bus.probe.request_break();
    run(&mut cpu, &mut bus);
    bus.intr = true;
    bus.go(Go::Step, &cpu);
    assert_eq!(run(&mut cpu, &mut bus), Reason::Stepped);
    assert_eq!(at(&cpu), (CODE, 1), "not stopped in the handler");
    assert_eq!(cpu.regs().gpr[gpr::BX], 1, "which ran");
}

#[test]
fn a_step_into_an_int_stops_at_its_handler() {
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &[0xCD, 0x30, 0x90]);
    bus.vector_to(0x30, HANDLER, 0);
    bus.probe.request_break();
    run(&mut cpu, &mut bus);
    bus.go(Go::Step, &cpu);
    assert_eq!(run(&mut cpu, &mut bus), Reason::Stepped);
    assert_eq!(at(&cpu), (HANDLER, 0));
}

#[test]
fn watches_stop_after_the_instruction_whose_cycle_they_saw() {
    // MOV [0300h],AL; NOP; MOV AL,[0302h]; NOP; OUT 61h,AL; NOP.
    let code = [
        0xA2, 0x00, 0x03, 0x90, 0xA0, 0x02, 0x03, 0x90, 0xE6, 0x61, 0x90,
    ];
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &code);
    cpu.regs_mut().gpr[gpr::AX] = 0x5A;
    let place = linear(CODE, 0x0300);
    let set = [
        Breakpoint::Memory {
            first: place,
            len: 1,
            access: Access::Write,
        },
        Breakpoint::Memory {
            first: place + 2,
            len: 1,
            access: Access::Any,
        },
        Breakpoint::Port {
            first: 0x60,
            last: 0x63,
            direction: Direction::Out,
        },
    ];
    assert!(bus.probe.set(&set));
    let hit = |master, cycle, at, value| Hit {
        master,
        cycle,
        at,
        value,
    };
    let expected = [
        (3, 0, hit(Master::Cpu, Cycle::Write, place, 0x5A)),
        (7, 1, hit(Master::Cpu, Cycle::Read, place + 2, 0)),
        (10, 2, hit(Master::Cpu, Cycle::Out, 0x61, 0)),
    ];
    for (ip, index, hit) in expected {
        let watch = set[index];
        assert_eq!(run(&mut cpu, &mut bus), Reason::Watched { watch, hit });
        assert_eq!(cpu.regs().ip, ip);
        bus.go(Go::Continue, &cpu);
    }
}

#[test]
fn a_read_watch_sees_no_code_fetch() {
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &[0x90; 16]);
    assert!(bus.probe.set(&[Breakpoint::Memory {
        first: linear(CODE, 0),
        len: 16,
        access: Access::Any,
    }]));
    let limit = cpu.cycles() + 40;
    assert_eq!(cpu.run(&mut bus, limit), Stop::Limit);
}

#[test]
fn a_breakpoint_on_a_string_instruction_does_not_stop_it_where_it_pauses() {
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &[0xF3, 0xAA, 0x90]);
    cpu.regs_mut().gpr[gpr::CX] = 200;
    cpu.regs_mut().gpr[gpr::DI] = 0x0400;
    assert!(bus.probe.set(&[Breakpoint::Code(linear(CODE, 0))]));
    run(&mut cpu, &mut bus);
    bus.go(Go::Continue, &cpu);
    let mut paused = false;
    for _ in 0..1_000 {
        let limit = cpu.cycles() + 30;
        assert_ne!(cpu.run(&mut bus, limit), Stop::Break);
        paused |= cpu.resumes_string();
        if cpu.regs().cx() == 0 {
            break;
        }
    }
    assert_eq!(cpu.regs().cx(), 0, "the string ran to its end");
    assert!(paused, "the string paused and went on");
}

#[test]
fn a_break_stops_a_halted_processor_where_it_halts() {
    let (mut cpu, mut bus) = machine(CpuModel::I8088, &[0xF4]);
    let limit = cpu.cycles() + 100;
    assert_eq!(cpu.run(&mut bus, limit), Stop::Halted);
    assert!(!bus.probe.halted(), "nothing asked it to stop");
    bus.probe.request_break();
    assert!(bus.probe.halted());
    assert_eq!(bus.probe.take_stop(), Some(Reason::Break));
}

#[test]
fn a_set_it_cannot_hold_leaves_the_one_it_held() {
    let mut probe = Probe::new(CpuModel::I8088);
    let held = [Breakpoint::Code(0x500)];
    assert!(probe.set(&held));
    for refused in [
        vec![Breakpoint::Code(SPACE)],
        vec![Breakpoint::Memory {
            first: SPACE - 1,
            len: 2,
            access: Access::Write,
        }],
        vec![Breakpoint::Memory {
            first: 0,
            len: 0,
            access: Access::Write,
        }],
        vec![Breakpoint::Port {
            first: 2,
            last: 1,
            direction: Direction::Both,
        }],
        (0..=u32::try_from(MOST_BREAKPOINTS).unwrap())
            .map(Breakpoint::Code)
            .collect(),
        vec![Breakpoint::Code(7), Breakpoint::Code(7)],
    ] {
        assert!(!probe.set(&refused), "{refused:?}");
        assert_eq!(probe.breakpoints(), held);
    }
    let full: Vec<_> = (0..u32::try_from(MOST_BREAKPOINTS).unwrap())
        .map(Breakpoint::Code)
        .collect();
    assert!(probe.set(&full));
    assert!(!probe.add(Breakpoint::Code(0x500)), "the set is full");
    assert!(probe.remove(Breakpoint::Code(3)));
    assert!(probe.add(Breakpoint::Code(0x500)));
    assert!(!probe.add(Breakpoint::Code(0x500)), "held already");
}

#[test]
fn a_memory_watch_covers_the_pages_its_bytes_lie_in() {
    let mut probe = Probe::new(CpuModel::I8088);
    assert!(probe.set(&[Breakpoint::Memory {
        first: 0x1FFF,
        len: 2,
        access: Access::Write,
    }]));
    assert!(probe.watches(0x1000, 0x1FFF));
    assert!(probe.watches(0x2000, 0x2FFF));
    assert!(!probe.watches(0x3000, 0x3FFF));
    assert!(!probe.watches(0x0000, 0x0FFF));
    assert!(probe.set(&[Breakpoint::Memory {
        first: 0,
        len: SPACE,
        access: Access::Any,
    }]));
    assert!(probe.watches(SPACE - 0x1000, SPACE - 1));
}

#[test]
fn in_8080_mode_a_step_over_a_call_returns_to_the_next_instruction() {
    // BRKEM 80h to: CALL 0010h; INR A; and at 10h: INR B; RET.
    let (mut cpu, mut bus) = machine(CpuModel::V20, &[0x0F, 0xFF, 0x80]);
    bus.vector_to(0x80, 0x0700, 0);
    bus.load(0x0700, 0, &[0xCD, 0x10, 0x00, 0x3C]);
    bus.load(0x0700, 0x10, &[0x04, 0xC9]);
    cpu.regs_mut().gpr[gpr::BP] = 0x0400;
    assert!(bus.probe.set(&[Breakpoint::Code(linear(0x0700, 0))]));
    run(&mut cpu, &mut bus);
    assert!(cpu.in_8080_mode());
    assert!(bus.probe.set(&[]));
    bus.go(Go::StepOver, &cpu);
    assert_eq!(run(&mut cpu, &mut bus), Reason::Stepped);
    assert_eq!(at(&cpu), (0x0700, 3));
    assert_eq!(cpu.regs().get8(5), 1, "B, in CH, was incremented");
}
