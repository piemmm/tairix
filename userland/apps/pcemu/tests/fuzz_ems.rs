//! Deterministic fuzz harness for the expanded memory manager, which guest
//! programs call through INT 67h with any registers and point at any bytes.
//! On a machine past its POST, for any sequence of calls:
//!
//! 1. Nothing panics, and every call returns within the longest move's
//!    time, or lands on the routine a map-and-jump names.
//! 2. Every status is one LIM 4.0 defines.
//! 3. The pages free and the pages the handles hold always add up to the
//!    board's.
//!
//! Each run allocates a handle and maps a page first, so the mapping paths
//! are reached by construction; the calls after are hostile, half of them
//! with structures that are well formed but wrong. Runs the fixed smoke
//! sweep under plain `cargo test`; keeps drawing from the same seeded stream
//! until `TAIRIX_FUZZ_BUDGET_SECS` elapses under `cargo xtask fuzz`.

use tairix_fuzzseed::Prng;
use tairix_pcemu::bios::{self, Firmware};
use tairix_pcemu::cpu::{gpr, Bus, Regs, Seg};
use tairix_pcemu::machine::{Machine, Template};
use tairix_pcemu::time::{Nanos, NANOS_PER_SECOND};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 3;
/// Calls a run makes after its prologue.
const CALLS: usize = 150;
const MS: Nanos = 1_000_000;
/// The longest a call may take: an exchange of the whole megabyte.
const LONGEST: Nanos = 3 * NANOS_PER_SECOND;
/// The caller: STI, INT 67h, then halted with interrupts off.
const CALLER: u16 = 0x0600;
const CALLER_CODE: [u8; 7] = [0xFB, 0xCD, 0x67, 0xFA, 0xF4, 0xEB, 0xFD];
/// Where it halts once the call returns.
const RETURNED: core::ops::Range<u16> = 0x0603..0x0607;
/// A map-and-jump's target, which halts, and a map-and-call's, which
/// returns at once.
const JUMP_TARGET: u16 = 0x0700;
const JUMP_CODE: [u8; 4] = [0xFA, 0xF4, 0xEB, 0xFD];
const JUMPED: core::ops::Range<u16> = 0x0701..0x0704;
const CALL_TARGET: u16 = 0x0710;
/// The structures and lists calls point at.
const AREA: u16 = 0x0800;
const AREA_LEN: usize = 0x800;
/// Where a move's conventional side may start: above everything the
/// harness keeps, in the frame, in ROM, or wrapping past the megabyte.
const SAFE_SEGMENTS: [u16; 5] = [0x2000, 0x4000, 0xD000, 0xF000, 0xFFFF];

struct Run {
    machine: Machine,
    handles: Vec<u16>,
}

impl Run {
    fn new() -> Self {
        let config = Template::TurboXt.config();
        let mut machine = Machine::new(&config).unwrap();
        Firmware::new(&config, [None, None])
            .unwrap()
            .install(machine.board_mut())
            .unwrap();
        bios::run_until(&mut machine, 3 * NANOS_PER_SECOND);
        let mut run = Self {
            machine,
            handles: Vec::new(),
        };
        run.poke(CALLER, &CALLER_CODE);
        run.poke(JUMP_TARGET, &JUMP_CODE);
        run.poke(CALL_TARGET, &[0xCB]);
        run
    }

    fn poke(&mut self, at: u16, bytes: &[u8]) {
        for (addr, &byte) in (u32::from(at)..).zip(bytes) {
            self.machine.board_mut().write(addr, byte, 0);
        }
    }

    /// Calls INT 67h with `set`'s registers; returns them as it returned, or
    /// `None` when it jumped to the map-and-jump target.
    fn call(&mut self, set: impl FnOnce(&mut Regs)) -> Option<Regs> {
        let at = self.machine.board().cycles_at(self.machine.now());
        self.machine.board_mut().output(0x21, 0x9C, at);
        let cpu = self.machine.cpu_mut();
        cpu.reset();
        let regs = cpu.regs_mut();
        regs.set_sreg(Seg::Cs, 0);
        regs.ip = CALLER;
        regs.set_sreg(Seg::Ss, 0);
        regs.set_sreg(Seg::Ds, 0);
        regs.set_sreg(Seg::Es, 0);
        regs.gpr[gpr::SP] = 0x7000;
        set(regs);
        regs.gpr[gpr::SP] = 0x7000;
        let begun = self.machine.now();
        loop {
            let now = self.machine.now();
            bios::run_until(&mut self.machine, now + MS);
            let regs = *self.machine.cpu().regs();
            if regs.sreg(Seg::Cs) == 0 && RETURNED.contains(&regs.ip) {
                return Some(regs);
            }
            if regs.sreg(Seg::Cs) == 0 && JUMPED.contains(&regs.ip) {
                return None;
            }
            assert!(
                self.machine.now() - begun < LONGEST,
                "INT 67h returned: {regs:X?}"
            );
        }
    }

    fn function(&mut self, ax: u16, bx: u16, dx: u16) -> Regs {
        self.call(|r| {
            r.set16(0, ax);
            r.set16(3, bx);
            r.set16(2, dx);
        })
        .unwrap()
    }

    /// The pages free and the pages every handle holds make the board's.
    fn check_accounting(&mut self) {
        let pages = self.function(0x4200, 0, 0);
        let (free, total) = (pages.get16(3), pages.get16(2));
        let listed = self
            .call(|r| {
                r.set16(0, 0x4D00);
                r.set16(7, AREA);
            })
            .unwrap();
        let count = listed.get16(3);
        let held: u32 = (0..u32::from(count))
            .map(|n| {
                let at = u32::from(AREA) + 4 * n + 2;
                let board = self.machine.board();
                u32::from(u16::from_le_bytes([board.peek(at), board.peek(at + 1)]))
            })
            .sum();
        assert_eq!(
            u32::from(free) + held,
            u32::from(total),
            "pages lost or doubled"
        );
    }
}

fn defined(status: u8) -> bool {
    matches!(status, 0x00 | 0x80 | 0x83..=0x98 | 0x9C | 0xA0..=0xA4)
}

/// A well-formed but arbitrary structure for function `ah` at the area.
fn structure(rng: &mut Prng, ah: u8, handles: &[u16]) -> Vec<u8> {
    let mut bytes = rng.noise(AREA_LEN);
    let pick_handle = |rng: &mut Prng| {
        handles
            .get(rng.below(handles.len().max(1)))
            .copied()
            .unwrap_or(1)
    };
    let list = AREA + 0x100;
    match ah {
        0x55 | 0x56 => {
            let target = if ah == 0x55 { JUMP_TARGET } else { CALL_TARGET };
            bytes[..4].copy_from_slice(&[target.to_le_bytes()[0], target.to_le_bytes()[1], 0, 0]);
            bytes[4] = u8::try_from(rng.below(5)).unwrap();
            bytes[5..9].copy_from_slice(&[list.to_le_bytes()[0], list.to_le_bytes()[1], 0, 0]);
            bytes[9] = u8::try_from(rng.below(5)).unwrap();
            bytes[10..14].copy_from_slice(&[list.to_le_bytes()[0], list.to_le_bytes()[1], 0, 0]);
        }
        0x57 => {
            let len = *rng.pick(&[0u32, 1, 100, 0x4000, 0x4001, 0x2_0000, 0x10_0000, 0x10_0001]);
            bytes[..4].copy_from_slice(&len.to_le_bytes());
            for side in [4usize, 11] {
                bytes[side] = u8::try_from(rng.below(3)).unwrap();
                bytes[side + 1..side + 3].copy_from_slice(&pick_handle(rng).to_le_bytes());
                let offset = *rng.pick(&[0u16, 0x10, 0x3FFF, 0x4000, 0xFFF0]);
                bytes[side + 3..side + 5].copy_from_slice(&offset.to_le_bytes());
                let base = *rng.pick(&[0u16, 1, 2, 0x2000, 0xD000, 0xFFFF]);
                let base = if bytes[side] == 0 {
                    *rng.pick(&SAFE_SEGMENTS)
                } else {
                    base
                };
                bytes[side + 5..side + 7].copy_from_slice(&base.to_le_bytes());
            }
        }
        _ => {
            for entry in bytes[0x100..0x110].as_chunks_mut::<4>().0 {
                let logical = *rng.pick(&[0u16, 1, 3, 0xFFFF]);
                let window = *rng.pick(&[0u16, 3, 4, 0xD000, 0xD400, 0xDC00, 0xD200]);
                entry[..2].copy_from_slice(&logical.to_le_bytes());
                entry[2..].copy_from_slice(&window.to_le_bytes());
            }
        }
    }
    bytes
}

fn run(rng: &mut Prng) {
    let mut run = Run::new();
    let allocated = run.function(0x4300, 3, 0);
    assert_eq!(allocated.get8(4), 0, "the prologue's allocation");
    run.handles.push(allocated.get16(2));
    let mapped = run.function(0x4400, 1, run.handles[0]);
    assert_eq!(mapped.get8(4), 0, "the prologue's mapping");
    for n in 0..CALLS {
        let ah = if rng.below(10) == 0 {
            rng.next_u8()
        } else {
            0x40 + u8::try_from(rng.below(0x1E)).unwrap()
        };
        let al = if rng.below(3) == 0 {
            rng.next_u8()
        } else {
            u8::try_from(rng.below(10)).unwrap()
        };
        let mut area = if rng.below(2) == 0 {
            structure(rng, ah, &run.handles)
        } else {
            rng.noise(AREA_LEN)
        };
        // A jump or call goes where it is told, so it is told to go to
        // the harness's own routines rather than into random bytes.
        let target = match ah {
            0x55 => Some(JUMP_TARGET),
            0x56 => Some(CALL_TARGET),
            _ => None,
        };
        if let Some(target) = target {
            area[..4].copy_from_slice(&[target.to_le_bytes()[0], target.to_le_bytes()[1], 0, 0]);
        }
        // A move writes where it is told too: never over the vectors, the
        // data area, the harness's code or its stack.
        if ah == 0x57 {
            for side in [4, 11] {
                if area[side] == 0 {
                    let segment = *rng.pick(&SAFE_SEGMENTS);
                    area[side + 5..side + 7].copy_from_slice(&segment.to_le_bytes());
                }
            }
        }
        run.poke(AREA, &area);
        let handle = run
            .handles
            .get(rng.below(run.handles.len() + 1))
            .copied()
            .unwrap_or_else(|| u16::from(rng.next_u8()));
        let (bx, cx) = (rng.next_u16(), u16::try_from(rng.below(6)).unwrap());
        let returned = run.call(|r| {
            r.set8(4, ah);
            r.set8(0, al);
            r.set16(3, if rng.below(2) == 0 { bx } else { bx % 8 });
            r.set16(1, cx);
            r.set16(2, handle);
            r.set16(6, AREA);
            r.set16(7, AREA + 0x400);
        });
        if let Some(regs) = returned {
            let status = regs.get8(4);
            assert!(
                defined(status),
                "AH={ah:02X} AL={al:02X} gave status {status:02X}"
            );
            if status == 0 && matches!(ah, 0x43 | 0x5A) {
                run.handles.push(regs.get16(2));
            }
        }
        if n % 10 == 9 {
            run.check_accounting();
        }
    }
    run.check_accounting();
}

#[test]
fn hostile_calls_never_panic_hang_or_lose_a_page() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_calls_never_panic_hang_or_lose_a_page",
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
