//! Deterministic fuzz harness for `MOUSE.COM`, which programs call through
//! INT 33h with any registers while the mouse moves, its buttons change and
//! the screen changes mode under the cursor. On a booted system with the
//! driver installed, for any calls, motion and modes:
//!
//! 1. Nothing panics, and every call returns.
//! 2. Function `00h` still answers that the driver is there.
//!
//! Pointers in registers point into the harness's own area, handlers at a
//! far return there, and a state restored is one saved, so what is hostile
//! is what a program may say to the driver, not a program overwriting the
//! driver's memory or handing it code to call that is not code. The data
//! area's video fields are scrambled now and then, as a program that writes
//! them scrambles them. Runs the fixed smoke sweep under plain
//! `cargo test`; keeps drawing from the same seeded stream until
//! `TAIRIX_FUZZ_BUDGET_SECS` elapses under `cargo xtask fuzz`.

use tairix_fuzzseed::Prng;
use tairix_pcemu::asm::Asm;
use tairix_pcemu::bios::{self, Firmware};
use tairix_pcemu::cpu::{flag, gpr, Bus, Regs, Seg};
use tairix_pcemu::dos::volume::{self, File};
use tairix_pcemu::machine::{Machine, Template};
use tairix_pcemu::ports::mouse::HostPointer;
use tairix_pcemu::storage::floppy::Floppy;
use tairix_pcemu::storage::MemoryDisk;
use tairix_pcemu::time::{Nanos, NANOS_PER_SECOND};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 3;
/// Calls a run makes.
const CALLS: usize = 150;
/// The longest a call may take.
const LONGEST: Nanos = 5 * NANOS_PER_SECOND;
const SECOND: Nanos = NANOS_PER_SECOND;
/// Where the call stub lies in the shell's segment.
const STUB: u16 = 0x0200;
/// The far return handlers are set to, the area pointers point into, and
/// where functions `16h` and `17h` save and restore the driver's state: a
/// state restored is one saved, so the handlers it holds are the harness's.
const RETURN: u16 = 0x0300;
const AREA: u16 = 0x0400;
const STATE: u16 = 0x0600;
/// The modes a run sets: every one the BIOS has on some card, and some it
/// does not.
const MODES: [u8; 16] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x0D, 0x0E, 0x0F, 0x10, 0x11, 0x12, 0x13, 0x6A,
];
/// The data area's video fields.
const VIDEO_FIELDS: [core::ops::Range<u32>; 2] = [0x449..0x467, 0x484..0x48B];

/// The idle shell: it waits with interrupts on, for ever.
fn idle() -> Vec<u8> {
    let mut asm = Asm::new(0x100);
    let here = asm.label();
    asm.sti();
    asm.bind(&here);
    asm.hlt();
    asm.jmp_short(&here);
    asm.finish().unwrap()
}

/// The volume a run boots: the shell, the driver, and `CONFIG.SYS`
/// installing it.
fn disk() -> Floppy {
    let medium = tairix_fat::format::Medium {
        sectors: 720,
        bytes_per_sector: 512,
        sectors_per_track: 9,
        heads: 2,
        hidden_sectors: 0,
        media: 0xFD,
    };
    let shape = tairix_fat::format::Shape {
        kind: Some(tairix_fat::boot::Kind::Fat12),
        sectors_per_cluster: Some(2),
        root_entries: 112,
        serial: 1,
        label: None,
    };
    let bpb = tairix_fat::format::plan(&medium, &shape).unwrap();
    let (shell, driver) = (idle(), tairix_pcemu::dos::mouse_com().unwrap());
    let files = [
        File {
            path: "COMMAND.COM",
            bytes: &shell,
            attributes: 0,
        },
        File {
            path: "MOUSE.COM",
            bytes: &driver,
            attributes: 0,
        },
        File {
            path: "CONFIG.SYS",
            bytes: b"INSTALL=A:\\MOUSE.COM\r\n",
            attributes: 0,
        },
    ];
    let image = volume::volume(&bpb, None, &files, tairix_fat::stamp::Stamp::NONE).unwrap();
    Floppy::raw(Box::new(MemoryDisk::new(image, false).unwrap())).unwrap()
}

struct Run {
    machine: Machine,
    psp: u16,
}

impl Run {
    fn new(rng: &mut Prng) -> Self {
        let templates = [Template::IbmPc, Template::TurboXt, Template::Workstation186];
        let config = rng.pick(&templates).config();
        let mut machine = Machine::new(&config).unwrap();
        Firmware::new(&config, [None, None])
            .unwrap()
            .install(machine.board_mut())
            .unwrap();
        machine.board_mut().insert_floppy(0, disk()).unwrap();
        let mut deadline = SECOND;
        loop {
            bios::run_until(&mut machine, deadline);
            let regs = machine.cpu().regs();
            if (0x101..0x104).contains(&regs.ip) && regs.sreg(Seg::Cs) > 0x100 {
                let psp = regs.sreg(Seg::Cs);
                let mut run = Self { machine, psp };
                run.put(RETURN, &[0xCB]);
                return run;
            }
            assert!(deadline < 30 * SECOND, "TAIRiX DOS booted");
            deadline += SECOND / 2;
        }
    }

    fn put(&mut self, offset: u16, bytes: &[u8]) {
        for (at, &byte) in (offset..).zip(bytes) {
            let linear = u32::from(self.psp) * 16 + u32::from(at);
            self.machine.board_mut().write(linear, byte, 0);
        }
    }

    /// Makes INT `vector` from the stub with interrupts on, every segment
    /// register the shell's but those `set` changes.
    fn int(&mut self, vector: u8, set: impl FnOnce(&mut Regs)) -> Regs {
        self.put(STUB, &[0xCD, vector, 0xEB, 0xFE]);
        let psp = self.psp;
        let cpu = self.machine.cpu_mut();
        cpu.reset();
        let regs = cpu.regs_mut();
        for seg in [Seg::Ds, Seg::Es] {
            regs.set_sreg(seg, psp);
        }
        set(regs);
        regs.set_sreg(Seg::Cs, psp);
        regs.set_sreg(Seg::Ss, psp);
        regs.ip = STUB;
        regs.gpr[gpr::SP] = 0xFFF0;
        regs.set_flag(flag::IF, true);
        let begun = self.machine.now();
        loop {
            let now = self.machine.now();
            bios::run_until(&mut self.machine, now + SECOND / 20);
            let regs = *self.machine.cpu().regs();
            if regs.sreg(Seg::Cs) == psp && regs.ip == STUB + 2 {
                return regs;
            }
            assert!(
                self.machine.now() - begun < LONGEST,
                "INT {vector:02X}h returned: {regs:X?}"
            );
        }
    }

    /// Lets the machine run, the shell idle, for `span`.
    fn idle(&mut self, span: Nanos) {
        let now = self.machine.now();
        bios::run_until(&mut self.machine, now + span);
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

/// An INT 33h call with any function and registers; pointers into the
/// area, handlers at its far return or none.
fn call(run: &mut Run, rng: &mut Prng) {
    let function = u16::try_from(rng.below(0x28)).unwrap();
    let registers = [0u16; 5].map(|_| word(rng));
    let psp = run.psp;
    let pointer = AREA + u16::try_from(rng.below(0x100)).unwrap();
    let handler = if rng.below(3) == 0 {
        (0, 0)
    } else {
        (psp, RETURN)
    };
    run.int(0x33, |r| {
        r.gpr[gpr::AX] = function;
        for (reg, value) in [gpr::BX, gpr::CX, gpr::DX, gpr::SI, gpr::DI]
            .into_iter()
            .zip(registers)
        {
            r.gpr[reg] = value;
        }
        match function {
            0x09 => r.gpr[gpr::DX] = pointer,
            0x16 | 0x17 => r.gpr[gpr::DX] = STATE,
            0x0C | 0x14 | 0x18 => {
                r.set_sreg(Seg::Es, handler.0);
                r.gpr[gpr::DX] = handler.1;
            }
            _ => {}
        }
    });
}

fn run(rng: &mut Prng) {
    let mut run = Run::new(rng);
    for _ in 0..CALLS {
        match rng.below(10) {
            0..=4 => call(&mut run, rng),
            5 => {
                let dx = i32::try_from(rng.below(301)).unwrap() - 150;
                let dy = i32::try_from(rng.below(301)).unwrap() - 150;
                let buttons = rng.next_u8();
                run.machine.board_mut().mouse(dx, dy, buttons);
                run.idle(u64::try_from(rng.below(80)).unwrap() * 1_000_000);
            }
            // The host's pointer the driver follows, anywhere, under the
            // same hostile modes and ranges as the motion.
            6 => {
                let mut edge_biased = || match rng.below(4) {
                    0 => 0,
                    1 => u16::MAX,
                    _ => rng.next_u16(),
                };
                let at = HostPointer {
                    x: edge_biased(),
                    y: edge_biased(),
                };
                let buttons = rng.next_u8();
                run.machine.board_mut().point(at, buttons);
                run.idle(u64::try_from(rng.below(80)).unwrap() * 1_000_000);
            }
            7 => {
                let mode = *rng.pick(&MODES);
                run.int(0x10, |r| r.gpr[gpr::AX] = u16::from(mode));
            }
            8 => {
                for field in VIDEO_FIELDS {
                    for at in field {
                        if rng.below(8) == 0 {
                            run.machine.board_mut().write(at, rng.edge_biased_u8(), 0);
                        }
                    }
                }
            }
            _ => {
                let answer = run.int(0x33, |r| r.gpr[gpr::AX] = 0);
                assert_eq!(answer.gpr[gpr::AX], 0xFFFF, "the driver still answers");
            }
        }
    }
}

#[test]
fn hostile_mouse_calls_never_panic_or_hang() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_mouse_calls_never_panic_or_hang",
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
