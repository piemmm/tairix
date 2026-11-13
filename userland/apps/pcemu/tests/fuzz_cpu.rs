//! Deterministic fuzz harness for the emulated CPU, which runs untrusted
//! guest code by design. For any bytes in any machine state, on every model:
//!
//! 1. [`decode`] never panics and yields a unit of one to [`LONGEST`] bytes,
//!    the longest any modelled encoding is.
//! 2. [`Cpu::step`] never panics, and returns within one unit of work.
//! 3. [`Cpu::run`] stops by the limit it is given, give or take the longest
//!    single unit, however the code loops, repeats strings or halts.
//!
//! Runs the fixed smoke sweep under plain `cargo test`; keeps drawing from the
//! same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

use tairix_fuzzseed::Prng;
use tairix_pcemu::cpu::decode::{decode, SliceFetch, LONGEST};
use tairix_pcemu::cpu::{flag, gpr, Bus, Cpu, CpuModel, Seg, Stop};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 600;

/// The longest any single unit can take: a V-series `ADD4S` or `SUB4S` of
/// 255 digits, 128 bytes at 22 cycles after 12 of setup, entered from an
/// interrupt and ending in a single-step trap, with margin.
const LONGEST_UNIT: u64 = 4_000;

/// A random megabyte whose ports answer random bytes and whose interrupt
/// pins toggle at random.
struct HostileBus {
    ram: Vec<u8>,
    rng: Prng,
}

impl Bus for HostileBus {
    fn read(&mut self, addr: u32, _at: u64) -> u8 {
        self.ram[(addr & 0xF_FFFF) as usize]
    }
    fn peek(&self, addr: u32) -> u8 {
        self.ram[(addr & 0xF_FFFF) as usize]
    }
    fn write(&mut self, addr: u32, value: u8, _at: u64) {
        self.ram[(addr & 0xF_FFFF) as usize] = value;
    }
    fn input(&mut self, _port: u16, _at: u64) -> u8 {
        self.rng.next_u8()
    }
    fn output(&mut self, _port: u16, _value: u8, _at: u64) {}
    fn intr(&self) -> bool {
        self.ram[0] & 1 != 0
    }
    fn acknowledge(&mut self) -> u8 {
        self.rng.next_u8()
    }
    fn nmi(&self) -> bool {
        self.ram[1] == 0xA5
    }
    fn acknowledge_nmi(&mut self) {
        self.ram[1] = 0;
    }
    fn host_code(&self, addr: u32) -> bool {
        addr >= 0xF_0000
    }
    fn escape_traps(&self) -> bool {
        self.ram[2] & 1 != 0
    }
    // No access waits, so none is ever pending.
    fn steal(&mut self, _cycles: u32) {}
}

fn exercise_decoder(rng: &mut Prng) {
    let mut bytes = [0u8; 16];
    for b in &mut bytes {
        *b = rng.next_u8();
    }
    for model in CpuModel::ALL {
        for host in [false, true] {
            let insn = decode(model, host, &mut SliceFetch::new(&bytes));
            assert!(
                (1..=LONGEST).contains(&usize::from(insn.len)),
                "{bytes:02x?} on {model:?} gave {} bytes",
                insn.len
            );
            assert!(insn.head <= insn.len);
        }
    }
}

fn exercise_machine(rng: &mut Prng) {
    let model = CpuModel::ALL[usize::from(rng.next_u8()) % CpuModel::ALL.len()];
    let mut ram = vec![0u8; 1 << 20];
    for chunk in ram.chunks_mut(8) {
        chunk.copy_from_slice(&rng.next_u64().to_le_bytes());
    }
    let mut bus = HostileBus {
        ram,
        rng: Prng::new(rng.next_u64()),
    };
    let mut cpu = Cpu::new(model);
    let regs = cpu.regs_mut();
    for r in &mut regs.gpr {
        *r = rng.next_u16();
    }
    for s in [Seg::Es, Seg::Cs, Seg::Ss, Seg::Ds] {
        regs.set_sreg(s, rng.next_u16());
    }
    regs.ip = rng.next_u16();
    regs.flags = rng.next_u16();
    // Keep a REP's count bounded so the sweep explores breadth.
    regs.gpr[gpr::CX] &= 0x00FF;
    for _ in 0..64 {
        let before = cpu.cycles();
        let _ = cpu.step(&mut bus, before + 4_000);
        assert!(
            cpu.cycles() - before <= 4_000 + LONGEST_UNIT,
            "one unit ran away on {model:?}"
        );
    }
    let limit = cpu.cycles() + 20_000;
    let stop = cpu.run(&mut bus, limit);
    if stop == Stop::Limit {
        assert!(
            cpu.cycles() <= limit + LONGEST_UNIT,
            "{model:?} overran its limit"
        );
    }
}

/// The regression: the bound left out the V-series' packed-BCD strings, each
/// one indivisible unit, so a run that ended on a long one overran it.
#[test]
fn the_longest_unit_holds_a_bcd_string_of_every_digit_on_every_model() {
    for model in CpuModel::ALL {
        for op in [0x20, 0x22, 0x26] {
            let mut ram = vec![0u8; 1 << 20];
            ram[0x100..0x102].copy_from_slice(&[0x0F, op]);
            let mut bus = HostileBus {
                ram,
                rng: Prng::new(0),
            };
            let mut cpu = Cpu::new(model);
            let regs = cpu.regs_mut();
            regs.set_sreg(Seg::Cs, 0);
            regs.ip = 0x100;
            regs.gpr[gpr::CX] = 0x00FF;
            regs.flags |= flag::TF;
            let before = cpu.cycles();
            let _ = cpu.step(&mut bus, u64::MAX);
            let took = cpu.cycles() - before;
            assert!(
                took <= LONGEST_UNIT,
                "0F {op:02X} on {model:?} took {took} cycles"
            );
        }
    }
}

#[test]
fn hostile_code_never_panics_or_runs_away() {
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "hostile_code_never_panics_or_runs_away",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    loop {
        for _ in 0..SMOKE_ITERATIONS {
            exercise_decoder(&mut rng);
            if rng.next_u8() < 16 {
                exercise_machine(&mut rng);
            }
        }
        if !tairix_fuzzseed::within_budget(deadline) {
            break;
        }
    }
}
