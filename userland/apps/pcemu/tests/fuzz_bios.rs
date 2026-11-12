//! Deterministic fuzz harness for the BIOS services a program calls with any
//! registers, over a BIOS data area it may have written anything into: the
//! video services on each display adapter, the equipment, memory, disk,
//! serial, system, keyboard, printer and clock services. For any call:
//!
//! 1. Nothing panics.
//! 2. Every call returns within the slowest service's time: every retry of
//!    a diskette read with no diskette in the drive.
//!
//! The data area's video fields and the vectors to the parameter and font
//! tables are scrambled between calls, as a program that writes them
//! scrambles them; pointers passed in registers point at the harness's own
//! area, so what is hostile is what a program may say to the BIOS, not a
//! program overwriting the stub that calls it. Runs the fixed smoke sweep
//! under plain `cargo test`; keeps drawing from the same seeded stream until
//! `TAIRIX_FUZZ_BUDGET_SECS` elapses under `cargo xtask fuzz`.

use tairix_fuzzseed::Prng;
use tairix_pcemu::bios::{self, Firmware};
use tairix_pcemu::chips::keyboard::Scan;
use tairix_pcemu::cpu::{gpr, Bus, Seg};
use tairix_pcemu::machine::{Machine, Template};
use tairix_pcemu::time::{Nanos, NANOS_PER_SECOND};
use tairix_pcemu::video::monitor::{Monitor, Phosphor};
use tairix_pcemu::video::Adapter;

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 4;
/// Calls a run makes.
const CALLS: usize = 150;
/// The longest a call may take.
const LONGEST: Nanos = 60 * NANOS_PER_SECOND;
const SECOND: Nanos = NANOS_PER_SECOND;
/// Where the stub that makes each call lies: `INT n`, then a jump to itself.
const STUB: u16 = 0x0060;
/// The stack's segment.
const STACK: u16 = 0x1000;
/// The segment pointers in registers point into.
const AREA: u16 = 0x2000;
/// The video fields of the BIOS data area: the mode, columns, page size and
/// start, cursors, shape, page, CRTC port, mode register and palette; the
/// EGA's and VGA's rows, character height and switches.
const VIDEO_FIELDS: [core::ops::Range<u32>; 2] = [0x449..0x467, 0x484..0x48B];
/// The vectors to the video parameters, the CGA's upper font and the EGA's
/// and VGA's font.
const TABLE_VECTORS: [u32; 3] = [0x1D, 0x1F, 0x43];

/// Keys that put a keystroke in the buffer for every read function: digits,
/// letters, Enter, the space bar and the function keys an XT keyboard has.
const TYPING: [u8; 14] = [
    0x02, 0x0B, 0x10, 0x19, 0x1C, 0x1E, 0x26, 0x2C, 0x32, 0x39, 0x3B, 0x40, 0x44, 0x0E,
];

/// The adapters a run may have, beside each template's own.
const ADAPTERS: [Adapter; 5] = [
    Adapter::Mda(Phosphor::Green),
    Adapter::Hercules(Phosphor::Amber),
    Adapter::Cga(Monitor::Colour),
    Adapter::Ega(Monitor::Colour),
    Adapter::Vga(Monitor::Colour),
];

/// A machine past its power-on self test, waiting at its no-boot prompt.
fn machine(rng: &mut Prng) -> Machine {
    let templates = [
        Template::IbmPc,
        Template::IbmXt,
        Template::TurboXt,
        Template::Workstation186,
    ];
    let mut config = rng.pick(&templates).config();
    if rng.below(2) == 0 {
        config.video = [Some(*rng.pick(&ADAPTERS)), None];
    }
    let mut machine = Machine::new(&config).unwrap();
    Firmware::new(&config, [None, None])
        .unwrap()
        .install(machine.board_mut())
        .unwrap();
    bios::run_until(&mut machine, 4 * SECOND);
    machine
}

fn poke(machine: &mut Machine, at: u32, value: u8) {
    machine.board_mut().write(at, value, 0);
}

/// Writes noise over some of the data area's video fields and table vectors.
fn scramble(machine: &mut Machine, rng: &mut Prng) {
    for field in VIDEO_FIELDS {
        for at in field {
            if rng.below(8) == 0 {
                poke(machine, at, rng.edge_biased_u8());
            }
        }
    }
    for vector in TABLE_VECTORS {
        if rng.below(6) == 0 {
            for byte in 0..4 {
                poke(machine, vector * 4 + byte, rng.next_u8());
            }
        }
    }
}

/// Makes a call to a service with any function number and registers; a
/// read that waits for a key has one pressed for it.
fn call(machine: &mut Machine, rng: &mut Prng) {
    let vectors = [
        0x10, 0x10, 0x10, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x1A,
    ];
    let vector = *rng.pick(&vectors);
    let ah = if vector == 0x10 && rng.below(2) == 0 {
        u8::try_from(rng.below(0x20)).unwrap()
    } else {
        rng.next_u8()
    };
    if vector == 0x16 && matches!(ah, 0x00 | 0x10) {
        let key = Scan::plain(*rng.pick(&TYPING));
        machine.board_mut().key(key, true);
        machine.board_mut().key(key, false);
    }
    let (al, rest) = (rng.next_u8(), [0u16; 5].map(|_| word(rng)));
    let offset = u16::try_from(rng.below(0x8000)).unwrap();
    let code = [0xCD, vector, 0xEB, 0xFE];
    for (at, &byte) in (0u32..).zip(&code) {
        poke(machine, u32::from(STUB) * 16 + at, byte);
    }
    machine.cpu_mut().reset();
    let regs = machine.cpu_mut().regs_mut();
    regs.gpr[gpr::AX] = u16::from_le_bytes([al, ah]);
    for (reg, value) in [gpr::BX, gpr::CX, gpr::DX, gpr::SI, gpr::DI]
        .into_iter()
        .zip(rest)
    {
        regs.gpr[reg] = value;
    }
    if rng.below(2) == 0 {
        regs.gpr[gpr::BP] = offset;
        regs.gpr[gpr::BX] = offset;
        regs.gpr[gpr::DX] = offset;
    }
    regs.set_sreg(Seg::Cs, STUB);
    regs.set_sreg(Seg::Ds, AREA);
    regs.set_sreg(Seg::Es, AREA);
    regs.set_sreg(Seg::Ss, STACK);
    regs.gpr[gpr::SP] = 0xFFFE;
    regs.ip = 0;
    let begun = machine.now();
    loop {
        let now = machine.now();
        bios::run_until(machine, now + SECOND / 10);
        let regs = machine.cpu().regs();
        if regs.sreg(Seg::Cs) == STUB && regs.ip == 2 {
            return;
        }
        assert!(
            machine.now() - begun < LONGEST,
            "INT {vector:02X}h AH={ah:02X}h never returned"
        );
    }
}

/// A register's value: often an edge, else anything.
fn word(rng: &mut Prng) -> u16 {
    match rng.below(4) {
        0 => 0,
        1 => 0xFFFF,
        _ => u16::from_le_bytes([rng.next_u8(), rng.next_u8()]),
    }
}

fn run(rng: &mut Prng) {
    let mut machine = machine(rng);
    for _ in 0..CALLS {
        if rng.below(3) == 0 {
            scramble(&mut machine, rng);
        }
        call(&mut machine, rng);
    }
}

#[test]
fn hostile_bios_calls_never_panic_or_hang() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_bios_calls_never_panic_or_hang",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            run(&mut rng);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}
