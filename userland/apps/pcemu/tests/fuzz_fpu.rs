//! Deterministic fuzz harness for the 8087, which runs guest code's numeric
//! instructions over guest memory. For any instruction stream, over any
//! state, any memory and any control word:
//!
//! 1. Nothing panics, and every instruction ends within the longest any
//!    takes: FYL2X's 950 clocks and the penalties an exception adds.
//! 2. BUSY, B and TEST agree with the instruction in flight and the
//!    interrupt held.
//! 3. A snapshot taken at any point, the instruction in flight included,
//!    restores to the same coprocessor.
//! 4. Its arithmetic at 53 bits, rounding to nearest, gives what IEEE
//!    doubles give, through the whole instruction path.
//!
//! Runs the fixed smoke sweep under plain `cargo test`; keeps drawing from
//! the same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

use tairix_fuzzseed::Prng;
use tairix_pcemu::cpu::bus::Escape;
use tairix_pcemu::fpu::real::Real;
use tairix_pcemu::fpu::{Fpu, Memory, Registers};
use tairix_pcemu::snapshot::{In, Out, State};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 300;
/// Instructions a run issues.
const STREAM: usize = 64;
/// The longest an instruction takes from its issue: FYL2X, with a masked
/// denormal's and an unmasked overflow's and underflow's penalties.
const LONGEST: u64 = 950 + 33 + 14 + 16;

struct Ram(Vec<u8>);

impl Memory for Ram {
    fn read(&mut self, addr: u32) -> u8 {
        self.0[(addr & 0xF_FFFF) as usize]
    }

    fn write(&mut self, addr: u32, value: u8) {
        self.0[(addr & 0xF_FFFF) as usize] = value;
    }
}

impl Ram {
    fn word(&self, addr: u32) -> u16 {
        u16::from_le_bytes([
            self.0[(addr & 0xF_FFFF) as usize],
            self.0[((addr + 1) & 0xF_FFFF) as usize],
        ])
    }
}

/// An 8087 standing in `regs`, through its own snapshot.
fn standing(regs: &Registers) -> Fpu {
    let mut out = Out::refusable();
    regs.save(&mut out);
    out.u8(0);
    let bytes = out.finish().unwrap();
    let mut fpu = Fpu::new();
    fpu.restore(&mut In::new(&bytes)).unwrap();
    fpu
}

fn random_registers(rng: &mut Prng) -> Registers {
    let mut regs = Registers::default();
    for x in &mut regs.data {
        // Edge-heavy exponents: zero, special and the ends of the range.
        let exponent = match rng.below(6) {
            0 => 0,
            1 => 0x7FFF,
            2 => 1,
            3 => 0x7FFE,
            _ => rng.next_u16() & 0x7FFF,
        };
        *x = Real::new(rng.next_u8() & 1 == 1, exponent, rng.next_u64());
    }
    regs.tags = rng.next_u16();
    regs.control = rng.next_u16();
    regs.status = rng.next_u16() & !0x8080;
    regs.ip = rng.next_u32() & 0xF_FFFF;
    regs.opcode = rng.next_u16() & 0x7FF;
    regs.dp = rng.next_u32() & 0xF_FFFF;
    regs
}

fn random_escape(rng: &mut Prng, ram: &Ram) -> Escape {
    let opcode = rng.next_u16() & 0x7FF;
    let memory = (opcode & 0xC0) != 0xC0;
    let addr = rng.next_u32() & 0xF_FFFF;
    Escape {
        opcode,
        address: rng.next_u32() & 0xF_FFFF,
        operand: memory.then(|| (addr, ram.word(addr))),
    }
}

fn check_pins(fpu: &Fpu, at: u64) {
    let busy = fpu.ends().is_some() || fpu.interrupting();
    assert_eq!(fpu.busy(), busy);
    assert_eq!(fpu.status() & 0x8000 != 0, busy, "B is BUSY");
    match fpu.ends() {
        Some(ends) => assert_eq!(fpu.ready_at(at), ends.max(at)),
        None if fpu.interrupting() => assert_eq!(fpu.ready_at(at), u64::MAX),
        None => assert_eq!(fpu.ready_at(at), at),
    }
}

fn check_snapshot(fpu: &Fpu) {
    let mut out = Out::refusable();
    fpu.save(&mut out);
    let bytes = out.finish().unwrap();
    let mut twin = Fpu::new();
    let mut input = In::new(&bytes);
    twin.restore(&mut input).unwrap();
    assert!(input.is_exhausted());
    assert_eq!(&twin, fpu);
}

fn hostile_stream(rng: &mut Prng, ram: &mut Ram) {
    let mut fpu = standing(&random_registers(rng));
    let mut at = rng.next_u32().into();
    for _ in 0..STREAM {
        // As the board does, an instruction is committed once its end has
        // passed.
        if fpu.ends().is_some_and(|ends| ends <= at) {
            fpu.complete(ram);
        }
        let escape = random_escape(rng, ram);
        fpu.escape(escape, at, ram);
        if let Some(ends) = fpu.ends() {
            assert!(
                (at..=at + LONGEST).contains(&ends),
                "{escape:?} ends at {ends}, issued at {at}"
            );
        }
        check_pins(&fpu, at);
        if rng.below(8) == 0 {
            check_snapshot(&fpu);
        }
        // Waited for, or not: the next one then ends it early.
        if rng.next_u8() & 1 == 1 {
            fpu.complete(ram);
        }
        at += u64::from(rng.next_u16() & 0x3FF);
    }
}

fn run(fpu: &mut Fpu, ram: &mut Ram, escape: Escape) {
    fpu.escape(escape, 0, ram);
    fpu.complete(ram);
}

/// Runs memory instruction `D8+group /reg` on the operand at `addr`.
fn run_memory(fpu: &mut Fpu, ram: &mut Ram, (group, reg): (u16, u16), addr: u32) {
    let escape = Escape {
        opcode: (group << 8) | (reg << 3) | 6,
        address: 0,
        operand: Some((addr, ram.word(addr))),
    };
    run(fpu, ram, escape);
}

fn random_double(rng: &mut Prng) -> f64 {
    let field = 1023 - 200 + u64::try_from(rng.below(400)).unwrap();
    f64::from_bits((rng.next_u64() & 0x800F_FFFF_FFFF_FFFF) | (field << 52))
}

fn ieee_agreement(rng: &mut Prng) {
    let mut ram = Ram(vec![0; 1 << 20]);
    let mut fpu = Fpu::new();
    // FLDCW: 53 bits, to nearest, every exception masked.
    ram.0[0x100..0x102].copy_from_slice(&0x027Fu16.to_le_bytes());
    run_memory(&mut fpu, &mut ram, (1, 5), 0x100);
    for _ in 0..16 {
        let (a, b) = (random_double(rng), random_double(rng));
        ram.0[0x200..0x208].copy_from_slice(&a.to_le_bytes());
        ram.0[0x208..0x210].copy_from_slice(&b.to_le_bytes());
        // (reg of DC /r, the IEEE result).
        for (reg, expected) in [
            (0, a + b),
            (1, a * b),
            (4, a - b),
            (5, b - a),
            (6, a / b),
            (7, b / a),
        ] {
            if !expected.is_normal() {
                continue;
            }
            run_memory(&mut fpu, &mut ram, (5, 0), 0x200);
            run_memory(&mut fpu, &mut ram, (4, reg), 0x208);
            run_memory(&mut fpu, &mut ram, (5, 3), 0x300);
            let got = f64::from_le_bytes(ram.0[0x300..0x308].try_into().unwrap());
            assert_eq!(got.to_bits(), expected.to_bits(), "{a:e} op{reg} {b:e}");
        }
        // FSQRT.
        ram.0[0x200..0x208].copy_from_slice(&a.abs().to_le_bytes());
        run_memory(&mut fpu, &mut ram, (5, 0), 0x200);
        run(
            &mut fpu,
            &mut ram,
            Escape {
                opcode: 0x1FA,
                address: 0,
                operand: None,
            },
        );
        run_memory(&mut fpu, &mut ram, (5, 3), 0x300);
        let got = f64::from_le_bytes(ram.0[0x300..0x308].try_into().unwrap());
        assert_eq!(got.to_bits(), a.abs().sqrt().to_bits(), "sqrt {a:e}");
    }
}

#[test]
fn hostile_numeric_code_never_panics_and_ieee_doubles_agree() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_numeric_code_never_panics_and_ieee_doubles_agree",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    // Hostile memory the streams read operands from and store into.
    let mut ram = Ram(rng.noise(1 << 20));
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            hostile_stream(&mut rng, &mut ram);
            ieee_agreement(&mut rng);
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}
