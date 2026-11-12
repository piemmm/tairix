use super::*;
use crate::board::ConfigError;
use crate::chips::keyboard::Scan;
use crate::cpu::{gpr, Bus, Seg};
use crate::time::{Hertz, NANOS_PER_MILLISECOND as MS, NANOS_PER_SECOND};
use crate::video::monitor::Monitor;
use crate::video::Adapter;
use alloc::vec;
use alloc::vec::Vec;

const CODE: u32 = 0x0400;
const COUNTER: u32 = 0x0600;

/// Initialises the 8259A as the BIOS does and unmasks `mask`'s clear bits.
fn pic_setup(mask: u8) -> [u8; 16] {
    [
        0xB0, 0x13, 0xE6, 0x20, 0xB0, 0x08, 0xE6, 0x21, 0xB0, 0x09, 0xE6, 0x21, 0xB0, mask, 0xE6,
        0x21,
    ]
}

/// `INC WORD [COUNTER]`, then end the interrupt at the 8259A and return.
const COUNTING_HANDLER: [u8; 9] = [0xFF, 0x06, 0x00, 0x06, 0xB0, 0x20, 0xE6, 0x20, 0xCF];
/// `STI`, then `HLT` and loop back to it.
const IDLE: [u8; 4] = [0xFB, 0xF4, 0xEB, 0xFD];

fn machine(template: Template, program: &[u8]) -> Machine {
    machine_with(&template.config(), program)
}

fn machine_with(config: &BoardConfig, program: &[u8]) -> Machine {
    let mut m = Machine::new(config).unwrap();
    load(&mut m, CODE, program);
    let regs = m.cpu_mut().regs_mut();
    regs.set_sreg(Seg::Cs, 0);
    regs.ip = 0x0400;
    regs.set_sreg(Seg::Ss, 0);
    regs.gpr[gpr::SP] = 0x7C00;
    m
}

fn load(m: &mut Machine, at: u32, bytes: &[u8]) {
    for (addr, &byte) in (at..).zip(bytes) {
        m.board_mut().write(addr, byte, 0);
    }
}

fn vector(m: &mut Machine, n: u8, handler: u16) {
    let slot = u32::from(n) * 4;
    load(
        m,
        slot,
        &[crate::bits::lo(handler), crate::bits::hi(handler), 0, 0],
    );
}

fn counter(m: &Machine) -> u16 {
    crate::bits::word(m.board().peek(COUNTER), m.board().peek(COUNTER + 1))
}

#[test]
fn the_bios_tick_counts_18_2_times_a_second_while_the_cpu_sleeps() {
    // Channel 0, mode 3, a count of 65 536, then the 8259A, as the BIOS
    // orders them.
    let mut program = vec![0xB0, 0x36, 0xE6, 0x43, 0x30, 0xC0, 0xE6, 0x40, 0xE6, 0x40];
    program.extend(pic_setup(0xFE));
    program.extend(IDLE);
    let mut m = machine(Template::IbmXt, &program);
    vector(&mut m, 8, 0x0500);
    load(&mut m, 0x0500, &COUNTING_HANDLER);
    assert_eq!(m.run_until(10 * NANOS_PER_SECOND), Exit::Deadline);
    assert_eq!(
        counter(&m),
        182,
        "1 193 182 Hz over 65 536, for ten seconds"
    );
    assert!(m.now() >= 10 * NANOS_PER_SECOND);
}

#[test]
fn a_keystroke_arrives_through_irq_1() {
    let mut program = pic_setup(0xFD).to_vec();
    // Port B: the BIOS's mode, then the keyboard clock running.
    program.extend([0xB0, 0x99, 0xE6, 0x63, 0xB0, 0x40, 0xE6, 0x61]);
    program.extend(IDLE);
    let mut m = machine(Template::IbmPc, &program);
    vector(&mut m, 9, 0x0520);
    // IN AL,60h; MOV [0602h],AL; pulse port B's clear; EOI; IRET.
    let handler = [
        0xE4, 0x60, 0xA2, 0x02, 0x06, 0xE4, 0x61, 0x0C, 0x80, 0xE6, 0x61, 0x24, 0x7F, 0xE6, 0x61,
        0xB0, 0x20, 0xE6, 0x20, 0xCF,
    ];
    load(&mut m, 0x0520, &handler);
    m.run_until(MS);
    m.board_mut().key(Scan::plain(0x2C), true);
    m.run_until(5 * MS);
    assert_eq!(m.board().peek(0x0602), 0x2C);
    m.board_mut().key(Scan::plain(0x2C), false);
    m.run_until(10 * MS);
    assert_eq!(m.board().peek(0x0602), 0xAC, "the break code follows");
}

/// Runs `MOV CX,count; LOOP $` and a host call, returning the cycles taken.
fn timed_loop(
    template: Template,
    setup: &[u8],
    count: u16,
    rate: Option<CpuClock>,
) -> (u64, Nanos) {
    let mut program = setup.to_vec();
    program.extend([
        0xB9,
        crate::bits::lo(count),
        crate::bits::hi(count),
        0xE2,
        0xFE,
    ]);
    // JMP F000:0000, where the ROM makes host call 7.
    program.extend([0xEA, 0x00, 0x00, 0x00, 0xF0]);
    let mut m = machine(template, &program);
    m.board_mut()
        .map_rom(0xF_0000, &[0xFE, 0x38, 0x07, 0x00], true)
        .unwrap();
    if let Some(rate) = rate {
        m.set_cpu_rate(rate);
    }
    assert_eq!(m.run_until(NANOS_PER_SECOND), Exit::HostCall(7));
    (m.cpu().cycles(), m.now())
}

#[test]
fn refresh_costs_the_cpu_its_bus_cycles() {
    let refresh = [
        0xB0, 0x54, 0xE6, 0x43, 0xB0, 0x12, 0xE6, 0x41, // channel 1, mode 2, count 18
        0xB0, 0x58, 0xE6, 0x0B, 0xB0, 0xFF, 0xE6, 0x01, 0xE6, 0x01, 0xB0, 0x00, 0xE6,
        0x0A, // DMA 0
    ];
    let idle = [0x90; 22];
    let (plain, _) = timed_loop(Template::IbmXt, &idle, 20_000, None);
    let (refreshed, _) = timed_loop(Template::IbmXt, &refresh, 20_000, None);
    // Five cycles of every 72: eighteen timer ticks of four CPU clocks.
    let permille = (refreshed - plain) * 1000 / refreshed;
    assert!(
        (68..=70).contains(&permille),
        "refresh took {permille} per mille of the bus"
    );
}

#[test]
fn host_calls_stop_only_in_the_emulators_rom() {
    let mut m = machine(Template::IbmXt, &[0xEA, 0x00, 0x00, 0x00, 0xF0]);
    m.board_mut()
        .map_rom(0xF_0000, &[0xFE, 0x38, 0x34, 0x12], true)
        .unwrap();
    assert_eq!(m.run_until(MS), Exit::HostCall(0x1234));
    let mut m = machine(Template::IbmXt, &[0xFE, 0x38, 0x34, 0x12, 0xF4]);
    assert_eq!(m.run_until(MS), Exit::Deadline);
}

#[test]
fn the_80186_passes_the_8259a_through_without_its_own_eoi() {
    // I0CON: cascade, level, special fully nested, lowest priority.
    let mut program = vec![0xBA, 0x38, 0xFF, 0xB8, 0x77, 0x00, 0xEF];
    // Channel 0, mode 2, a count of 1193: about a kilohertz.
    program.extend([
        0xB0, 0x34, 0xE6, 0x43, 0xB0, 0xA9, 0xE6, 0x40, 0xB0, 0x04, 0xE6, 0x40,
    ]);
    program.extend(pic_setup(0xFE));
    program.extend(IDLE);
    let mut m = machine(Template::Workstation186, &program);
    vector(&mut m, 8, 0x0500);
    load(&mut m, 0x0500, &COUNTING_HANDLER);
    // Half a period past the hundredth edge, wherever the setup put the first.
    m.run_until(100 * MS + MS / 2);
    assert_eq!(
        counter(&m),
        100,
        "every tick delivered though only the 8259A is ever ended"
    );
}

#[test]
fn turbo_runs_the_cpu_faster_against_unchanged_time() {
    let (cycles, slow) = timed_loop(Template::IbmXt, &[], 10_000, None);
    let (fast_cycles, fast) = timed_loop(
        Template::IbmXt,
        &[],
        10_000,
        Hertz::ratio(315_000_000, 33).and_then(CpuClock::new),
    );
    assert_eq!(cycles, fast_cycles, "the same work");
    assert!(
        slow.abs_diff(2 * fast) <= 2,
        "twice the clock, half the time: {slow} against {fast}"
    );
}

#[test]
fn a_halted_cpu_with_interrupts_off_sleeps_to_the_deadline() {
    let mut m = machine(Template::TurboXt, &[0xFA, 0xF4]);
    assert_eq!(m.run_until(NANOS_PER_SECOND), Exit::Deadline);
    assert!(m.cpu().halted());
    assert_eq!(m.run_until(2 * NANOS_PER_SECOND), Exit::Deadline);
    assert!(m.now() >= 2 * NANOS_PER_SECOND);
}

/// `MOV AX,(value << 8) | index` and `OUT DX,AX` for each 6845 register.
fn crtc_program(port: u16, registers: &[u8]) -> Vec<u8> {
    let mut code = vec![0xBA, crate::bits::lo(port), crate::bits::hi(port)];
    for (index, &value) in (0u8..).zip(registers) {
        code.extend([0xB8, index, value, 0xEF]);
    }
    code
}

#[test]
fn a_program_draws_on_the_cga_through_the_bus() {
    let mut program = crtc_program(
        0x3D4,
        &[
            0x71, 0x50, 0x5A, 0x0A, 0x1F, 0x06, 0x19, 0x1C, 0x02, 0x07, 0x06, 0x07,
        ],
    );
    // Mode 29h; ES = B800h; the word 1F41h ('A', white on blue) at ES:0.
    program.extend([
        0xBA, 0xD8, 0x03, 0xB0, 0x29, 0xEE, 0xB8, 0x00, 0xB8, 0x8E, 0xC0,
    ]);
    program.extend([0x26, 0xC7, 0x06, 0x00, 0x00, 0x41, 0x1F, 0xFA, 0xF4]);
    let mut m = machine(Template::IbmXt, &program);
    m.run_until(60 * MS);
    let frame = m.board_mut().frame(0).expect("a frame finished");
    let (x, y) = (912 - 720 - 128, 262 - 224 - 22);
    let at = |dx: usize, dy: usize| {
        let i = ((y + dy) * frame.width as usize + x + dx) * 4;
        [frame.pixels[i], frame.pixels[i + 1], frame.pixels[i + 2]]
    };
    assert_eq!(at(2, 0), [0xFF, 0xFF, 0xFF], "'A' is white");
    assert_eq!(at(0, 0), [0x00, 0x00, 0xAA], "on blue");
}

#[test]
fn a_mono_and_a_colour_adapter_share_the_bus() {
    let mut config = Template::IbmXt.config();
    config.video = [
        Some(Adapter::Mda(crate::video::monitor::Phosphor::Green)),
        Some(Adapter::Cga(Monitor::Colour)),
    ];
    let mut m = Machine::new(&config).unwrap();
    m.board_mut().write(0xB_0000, 0x11, 0);
    m.board_mut().write(0xB_8000, 0x22, 0);
    assert_eq!(
        (m.board().peek(0xB_0000), m.board().peek(0xB_8000)),
        (0x11, 0x22)
    );
    assert_eq!(m.board().peek(0xB_1000), 0x11, "the MDA's 4 KiB repeats");
}

#[test]
fn adapters_that_collide_are_refused() {
    let mut config = Template::IbmXt.config();
    config.video = [
        Some(Adapter::Cga(Monitor::Colour)),
        Some(Adapter::Cga(Monitor::Composite)),
    ];
    assert_eq!(
        Machine::new(&config).err(),
        Some(ConfigError::VideoConflict)
    );
}

#[test]
fn a_hercules_page_over_a_cga_leaves_the_cga_its_memory() {
    let mut config = Template::IbmXt.config();
    config.video = [
        Some(Adapter::Cga(Monitor::Colour)),
        Some(Adapter::Hercules(crate::video::monitor::Phosphor::Amber)),
    ];
    // OUT 3BFh, 3: allow graphics and the second page.
    let mut m = machine_with(&config, &[0xBA, 0xBF, 0x03, 0xB0, 0x03, 0xEE, 0xFA, 0xF4]);
    m.run_until(MS);
    m.board_mut().write(0xB_8000, 0x33, 0);
    m.board_mut().write(0xB_0000, 0x44, 0);
    assert_eq!(m.board().peek(0xB_8000), 0x33, "the CGA keeps B800h");
    assert_eq!(
        m.board().peek(0xB_0000),
        0x44,
        "the Hercules card keeps its first page"
    );
}

/// `OUT DX,AL` with DX and AL set: the index then data writes a register
/// pair takes.
fn out_pairs(port: u16, pairs: &[(u8, u8)]) -> Vec<u8> {
    let mut code = vec![0xBA, crate::bits::lo(port), crate::bits::hi(port)];
    for &(index, value) in pairs {
        code.extend([0xB8, index, value, 0xEF]);
    }
    code
}

#[test]
fn a_program_sets_mode_13h_on_a_vga_and_draws() {
    let mut config = Template::IbmXt.config();
    config.video = [Some(Adapter::Vga(Monitor::Colour)), None];
    let crtc: Vec<(u8, u8)> = (0u8..)
        .zip([
            0x5F, 0x4F, 0x50, 0x82, 0x54, 0x80, 0xBF, 0x1F, 0x00, 0x41, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x9C, 0x8E, 0x8F, 0x28, 0x40, 0x96, 0xB9, 0xA3, 0xFF,
        ])
        .collect();
    let mut program = vec![0xBA, 0xC2, 0x03, 0xB0, 0x63, 0xEE];
    program.extend(out_pairs(
        0x3C4,
        &[(0, 3), (1, 1), (2, 0x0F), (3, 0), (4, 0x0E)],
    ));
    program.extend(out_pairs(0x3D4, &[(0x11, 0)]));
    program.extend(out_pairs(0x3D4, &crtc));
    program.extend(out_pairs(
        0x3CE,
        &[(5, 0x40), (6, 0x05), (7, 0x0F), (8, 0xFF)],
    ));
    // The attribute controller: identity palette, mode 41h, plane enable.
    program.extend([0xBA, 0xDA, 0x03, 0xEC, 0xBA, 0xC0, 0x03]);
    for (index, value) in
        (0u8..16)
            .map(|i| (i, i))
            .chain([(0x10, 0x41), (0x12, 0x0F), (0x20, 0x20)])
    {
        program.extend([0xB0, index, 0xEE, 0xB0, value, 0xEE]);
    }
    program.truncate(program.len() - 3);
    // DAC entry 4 bright red, then pixel 4 at A000:0000.
    program.extend([
        0xBA, 0xC8, 0x03, 0xB0, 0x04, 0xEE, 0x42, 0xB0, 0x3F, 0xEE, 0x30, 0xC0, 0xEE, 0xEE,
    ]);
    program.extend([
        0xB8, 0x00, 0xA0, 0x8E, 0xC0, 0x26, 0xC6, 0x06, 0x00, 0x00, 0x04, 0xFA, 0xF4,
    ]);
    let mut m = machine_with(&config, &program);
    m.run_until(60 * MS);
    let frame = m.board_mut().frame(0).expect("a frame finished");
    let at = ((8 * frame.width + 16) * 4) as usize;
    assert_eq!(
        &frame.pixels[at..at + 3],
        &[255, 0, 0],
        "pixel 0 is DAC entry 4"
    );
}

#[test]
fn the_ega_vertical_interrupt_reaches_a_handler_on_irq_2() {
    let mut config = Template::IbmXt.config();
    config.video = [Some(Adapter::Ega(Monitor::Colour)), None];
    let crtc: Vec<(u8, u8)> = (0u8..)
        .zip([
            0x5B, 0x4F, 0x53, 0x37, 0x52, 0x00, 0x6C, 0x1F, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x5E, 0x2B, 0x5D, 0x28, 0x0F, 0x5F, 0x0A, 0xE3, 0xFF,
        ])
        .collect();
    let mut program = vec![0xBA, 0xC2, 0x03, 0xB0, 0xA7, 0xEE];
    program.extend(out_pairs(0x3C4, &[(1, 0x01)]));
    program.extend(out_pairs(0x3D4, &crtc));
    // Enable the vertical interrupt: register 11h with bit 5 clear, bit 4 set.
    program.extend(out_pairs(0x3D4, &[(0x11, 0x1B)]));
    program.extend(pic_setup(0xFB));
    program.extend(IDLE);
    let mut m = machine_with(&config, &program);
    vector(&mut m, 0x0A, 0x0500);
    // INC WORD [0600h]; re-arm by clearing then setting 11h bit 4; EOI; IRET.
    let mut handler = vec![0xFF, 0x06, 0x00, 0x06];
    handler.extend(out_pairs(0x3D4, &[(0x11, 0x0B), (0x11, 0x1B)]));
    handler.extend([0xB0, 0x20, 0xE6, 0x20, 0xCF]);
    load(&mut m, 0x0500, &handler);
    m.run_until(1000 * MS);
    let count = counter(&m);
    assert!(
        (58..=61).contains(&count),
        "one interrupt a frame at 59.8 Hz: {count}"
    );
}

#[test]
fn a_halted_cpu_waits_out_the_bus_its_80186_dma_holds() {
    let mut m = machine(Template::Workstation186, &[0xFA, 0xF4]);
    m.run_until(MS / 100);
    // Channel 0 then copies memory to memory, unsynchronized and with no
    // stop, so it takes the bus for as long as it runs.
    let at = m.cpu().cycles();
    for (register, value) in [
        (0xFFC0, 0x0000),
        (0xFFC2, 0x0003),
        (0xFFC4, 0x0000),
        (0xFFC6, 0x0002),
        (0xFFC8, 0x0000),
        (0xFFCA, 0xB006),
    ] {
        m.board_mut().output_word(register, value, at);
    }
    let before = m.cpu().cycles();
    let now = m.now();
    m.run_until(now + MS / 1000);
    // A burst of 65,536 byte transfers, two bus cycles each.
    let burst = 65_536 * 8;
    assert!(
        m.cpu().cycles() - before >= burst,
        "the CPU woke {} cycles on, inside the burst",
        m.cpu().cycles() - before
    );
}

/// Where the coprocessor tests keep their data.
const DATA: u32 = 0x0700;

fn put_double(m: &mut Machine, at: u32, value: f64) {
    load(m, at, &value.to_le_bytes());
}

#[test]
fn a_program_computes_on_the_8087_waiting_for_each_result() {
    // FNINIT; FLD [0700h]; WAIT; FMUL [0708h]; WAIT; FSQRT; WAIT;
    // FSTP [0710h]; WAIT; HLT.
    let program = [
        0xDB, 0xE3, 0xDD, 0x06, 0x00, 0x07, 0x9B, 0xDC, 0x0E, 0x08, 0x07, 0x9B, 0xD9, 0xFA, 0x9B,
        0xDD, 0x1E, 0x10, 0x07, 0x9B, 0xF4,
    ];
    let mut m = machine(Template::TurboXt, &program);
    put_double(&mut m, DATA, 3.0);
    put_double(&mut m, DATA + 8, 12.0);
    m.run_until(MS);
    assert!(m.cpu().halted());
    let stored: Vec<u8> = (0..8).map(|n| m.board().peek(DATA + 16 + n)).collect();
    assert_eq!(stored, 6.0f64.to_le_bytes());
}

#[test]
fn a_wait_with_no_coprocessor_never_ends_while_interrupts_are_served() {
    // Channel 0 at a count of 1000h, the 8259A, then STI; WAIT;
    // INC BYTE [0602h]; HLT.
    let mut program = vec![
        0xB0, 0x36, 0xE6, 0x43, 0x30, 0xC0, 0xE6, 0x40, 0xB0, 0x10, 0xE6, 0x40,
    ];
    program.extend(pic_setup(0xFE));
    program.extend([0xFB, 0x9B, 0xFE, 0x06, 0x02, 0x06, 0xF4]);
    let mut m = machine(Template::IbmXt, &program);
    vector(&mut m, 8, 0x0500);
    load(&mut m, 0x0500, &COUNTING_HANDLER);
    m.run_until(100 * MS);
    assert!(counter(&m) > 20, "{} ticks served", counter(&m));
    assert_eq!(m.board().peek(0x0602), 0, "never past the WAIT");
    assert!(m.cpu().waiting());
}

#[test]
fn an_unmasked_8087_exception_reaches_the_nmi_handler_that_clears_it() {
    // NMI on; FNINIT; FLDCW [0700h]; FLD1; FLDZ; FDIV ST(1),ST; WAIT;
    // INC BYTE [0602h]; HLT.
    let program = [
        0xB0, 0x80, 0xE6, 0xA0, 0xDB, 0xE3, 0xD9, 0x2E, 0x00, 0x07, 0xD9, 0xE8, 0xD9, 0xEE, 0xDC,
        0xF9, 0x9B, 0xFE, 0x06, 0x02, 0x06, 0xF4,
    ];
    let mut m = machine(Template::TurboXt, &program);
    // Every exception masked but zero divide, interrupts enabled.
    load(&mut m, DATA, &0x037Bu16.to_le_bytes());
    // FNCLEX; INC WORD [0600h]; IRET.
    vector(&mut m, 2, 0x0500);
    load(&mut m, 0x0500, &[0xDB, 0xE2, 0xFF, 0x06, 0x00, 0x06, 0xCF]);
    m.run_until(MS);
    assert_eq!(counter(&m), 1, "one NMI");
    assert_eq!(m.board().peek(0x0602), 1, "the WAIT ended once cleared");
    assert!(m.cpu().halted());
}
