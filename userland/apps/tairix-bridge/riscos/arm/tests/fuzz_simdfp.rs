//! Deterministic fuzz harness for VFP and Advanced SIMD: any floating-point
//! or SIMD instruction, A32 or T32, run from any register file, FPSCR (short
//! vectors included), core registers and mode, with the unit granted or
//! withheld.
//!
//! The invariants:
//!
//! * running never panics, and runs alike from alike states;
//! * the FPSCR never holds a bit `VMSR` cannot write, and FPEXC only its
//!   enable;
//! * the cumulative flags only gather: nothing but a write of the FPSCR
//!   clears one;
//! * with the unit withheld every instruction takes the undefined
//!   instruction exception, but for a privileged read or write of an
//!   identification register or FPEXC.
//!
//! Inputs are drawn from a per-run-seeded `Prng`; the smoke sweep runs on a
//! plain `cargo test` and `cargo xtask fuzz --soak` extends it to a
//! wall-clock budget.

use tairix_fuzzseed::Prng;
use tairix_riscos_arm::bus::{Abort, Bus, Fetch, Width};
use tairix_riscos_arm::cond::Condition;
use tairix_riscos_arm::cpu::{Cpu, Exception, State, Stop};
use tairix_riscos_arm::instruction::{Instruction, Op, Set, SimdFp, Structure, StructureLanes};
use tairix_riscos_arm::isa::Isa;
use tairix_riscos_arm::mmu::PAGE_BITS;
use tairix_riscos_arm::profile::BIT32;
use tairix_riscos_arm::psr::{self, Mode};
use tairix_riscos_arm::t32::{self, Position, Size};
use tairix_riscos_arm::vfp::{fpexc, fpscr, System};
use tairix_riscos_arm::{a32, reg::Reg};

/// Fixed-iteration sweep run once by a plain `cargo test` (no budget set).
const SMOKE_ITERATIONS: u64 = 20_000;
/// Where the program the processor runs begins: past the prologue.
const PROGRAM: u32 = 0x100;
/// The prologue that grants coprocessors 10 and 11 and enables the unit:
/// CPACR's access fields and FPEXC.EN, then the program.
const PROLOGUE: [u32; 6] = [
    0xE3A0_060F, // MOV  R0,#&F00000
    0xEE01_0F50, // MCR  CP15,0,R0,C1,C0,2
    0xF57F_F06F, // ISB
    0xE3A0_0101, // MOV  R0,#&40000000
    0xEEE8_0A10, // VMSR FPEXC,R0
    0xEA00_0039, // B    &100
];
const MODES: [Mode; 7] = [
    Mode::User,
    Mode::Fiq,
    Mode::Irq,
    Mode::Supervisor,
    Mode::Abort,
    Mode::Undefined,
    Mode::System,
];

/// One page of words a program runs in and addresses, all of it mapped.
struct Memory {
    words: Vec<u32>,
}

impl Memory {
    fn index(address: u32) -> Result<usize, Abort> {
        let index = usize::try_from(address >> 2).map_err(|_| Abort::Translation)?;
        if index < 1024 {
            Ok(index)
        } else {
            Err(Abort::Translation)
        }
    }
}

impl Bus for Memory {
    fn fetch(&mut self, address: u32, _privileged: bool) -> Result<Fetch, Abort> {
        Ok(Fetch {
            word: self.words[Self::index(address)?],
            host: false,
            page: address >> PAGE_BITS,
        })
    }

    fn read(&mut self, address: u32, width: Width, _privileged: bool) -> Result<u32, Abort> {
        let word = self.words[Self::index(address)?];
        let shift = 8 * (address & 3);
        Ok(match width {
            Width::Byte => (word >> shift) & 0xFF,
            Width::Half => (word >> shift) & 0xFFFF,
            Width::Word => word,
        })
    }

    fn write(&mut self, address: u32, width: Width, value: u32, _p: bool) -> Result<(), Abort> {
        let index = Self::index(address)?;
        let shift = 8 * (address & 3);
        let mask = match width {
            Width::Word => u32::MAX,
            narrow => narrow.mask() << shift,
        };
        self.words[index] = (self.words[index] & !mask) | ((value << shift) & mask);
        Ok(())
    }
}

/// A floating-point or SIMD instruction: A32's spaces, or T32's halfwords in
/// a word, the first halfword high.
fn drawn_instruction(rng: &mut Prng, thumb: bool) -> u32 {
    let spaces: [(u32, u32); 6] = if thumb {
        [
            (0xEF00_0000, 0xEF00_0000),
            (0xFF10_0000, 0xF900_0000),
            (0xFE00_0E00, 0xEC00_0A00),
            (0xFF00_0E00, 0xEE00_0A00),
            (0xFF00_0E00, 0xFE00_0A00),
            (0xFE00_0000, 0xEC00_0000),
        ]
    } else {
        [
            (0xFE00_0000, 0xF200_0000),
            (0xFF10_0000, 0xF400_0000),
            (0x0E00_0E00, 0x0C00_0A00),
            (0x0F00_0E00, 0x0E00_0A00),
            (0xFF00_0E00, 0xFE00_0A00),
            (0x0E00_0000, 0x0C00_0000),
        ]
    };
    let (mask, bits) = *rng.pick(&spaces);
    (rng.next_u32() & !mask) | bits
}

/// A register's value: anything, or one of the floating-point edges.
fn drawn_value(rng: &mut Prng) -> u64 {
    let edges: [u64; 12] = [
        0,
        1 << 63,
        0x7FF0_0000_0000_0000,
        0x7FF8_0000_0000_0001,
        0x7FF0_0000_0000_0001,
        0x0000_0000_0000_0001,
        0x7F80_0000_7FC0_0000,
        0x0040_0000_8000_0001,
        0x3F80_0000_BF80_0000,
        0x7F7F_FFFF_0080_0000,
        0x3FF0_0000_0000_0000,
        u64::MAX,
    ];
    match rng.below(3) {
        0 => *rng.pick(&edges),
        _ => u64::from(rng.next_u32()) << 32 | u64::from(rng.next_u32()),
    }
}

/// A state of the core registers: a valid mode, ARM or Thumb, and registers
/// pointing into the page or anywhere.
fn drawn_state(rng: &mut Prng, thumb: bool) -> State {
    let mode = *rng.pick(&MODES);
    let flags = rng.next_u32() & (psr::NZCV | psr::Q | psr::GE | psr::E);
    let mut state = State {
        pc: PROGRAM,
        cpsr: flags | mode.bits() | psr::I | psr::F | if thumb { psr::T } else { 0 },
        ..State::default()
    };
    let word = |rng: &mut Prng| match rng.below(3) {
        0 => rng.next_u32(),
        1 => 0x200 + 4 * u32::try_from(rng.below(256)).unwrap_or(0),
        _ => 0x200 + u32::try_from(rng.below(1024)).unwrap_or(0),
    };
    for value in state.user.iter_mut().chain(&mut state.fiq) {
        *value = word(rng);
    }
    for pair in &mut state.banked {
        *pair = [word(rng), word(rng)];
    }
    state
}

/// Whether the instruction writes the FPSCR, and so may clear its flags.
fn writes_fpscr(op: &SimdFp) -> bool {
    matches!(
        *op,
        SimdFp::System {
            read: false,
            register: System::Fpscr,
            ..
        }
    )
}

/// The instruction at the program's start, decoded as the processor will.
fn instruction_at(word: u32, thumb: bool, cpsr: u32) -> Option<SimdFp> {
    let op = if thumb {
        let position = Position::Outside;
        t32::decode(
            u16::try_from(word >> 16).unwrap_or(0),
            u16::try_from(word & 0xFFFF).unwrap_or(0),
            BIT32.isa,
            position,
        )?
        .instruction
        .op
    } else {
        let instruction = a32::decode(word, BIT32.isa)?;
        // A conditional instruction whose condition fails does nothing.
        if !instruction.cond.passes(cpsr)
            && instruction.cond != tairix_riscos_arm::cond::Condition::Nv
        {
            return None;
        }
        instruction.op
    };
    match op {
        Op::SimdFp(op) => Some(op),
        _ => None,
    }
}

/// How much of the unit a run is given.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Grant {
    /// CPACR denies coprocessors 10 and 11.
    None,
    /// CPACR grants them, but FPEXC.EN is clear.
    Disabled,
    /// Both.
    Enabled,
}

/// A processor of the 32-bit profile given `grant`, ready to run from the
/// program's start.
fn processor(memory: &mut Memory, grant: Grant) -> Cpu {
    let mut cpu = Cpu::new(&BIT32, 4);
    if grant != Grant::None {
        cpu.set_reg(Reg::PC, 0);
        let run = cpu.run(memory, PROLOGUE.len() as u64);
        assert_eq!(run.stop, Stop::Budget, "the prologue runs");
        assert_eq!(cpu.pc(), PROGRAM);
    }
    if grant == Grant::Disabled {
        cpu.vfp_mut().set_fpexc(0);
    }
    cpu
}

fn check_running(rng: &mut Prng) {
    let thumb = rng.below(3) == 0;
    let mut memory = Memory {
        words: vec![0; 1024],
    };
    for (slot, word) in memory.words.iter_mut().zip(PROLOGUE) {
        *slot = word;
    }
    for word in &mut memory.words[0x80..] {
        *word = rng.next_u32();
    }
    let instruction = drawn_instruction(rng, thumb);
    // T32's first halfword is at the lower address.
    let stored = if thumb {
        instruction.rotate_left(16)
    } else {
        instruction
    };
    memory.words[usize::try_from(PROGRAM >> 2).unwrap_or(0)] = stored;
    let grant = match rng.below(8) {
        0 => Grant::None,
        1 => Grant::Disabled,
        _ => Grant::Enabled,
    };
    let state = drawn_state(rng, thumb);
    let mut registers = [0u64; 32];
    for value in &mut registers {
        *value = drawn_value(rng);
    }
    let fpscr_value = rng.next_u32();
    let mut finals = Vec::new();
    for _ in 0..2 {
        let mut memory = Memory {
            words: memory.words.clone(),
        };
        let mut cpu = processor(&mut memory, grant);
        cpu.set_state(&state).expect("a mode the profile has");
        let vfp = cpu.vfp_mut();
        for (n, &value) in (0u8..).zip(&registers) {
            vfp.set_d(n, value);
        }
        vfp.set_fpscr(fpscr_value);
        let before = cpu.vfp().fpscr();
        let _ = cpu.run(&mut memory, 1);
        let after = *cpu.vfp();
        assert_eq!(after.fpscr() & !fpscr::WRITABLE, 0, "{instruction:08X}");
        assert_eq!(after.fpexc() & !fpexc::EN, 0, "{instruction:08X}");
        let decoded = instruction_at(instruction, thumb, state.cpsr);
        if let Some(op) = decoded {
            if !writes_fpscr(&op) {
                assert_eq!(
                    before & !after.fpscr() & (fpscr::FLAGS | fpscr::QC),
                    0,
                    "{instruction:08X}: a flag cleared"
                );
            }
            // FPEXC.EN does not gate a privileged identification access;
            // the CPACR gates everything.
            let identification = matches!(
                op,
                SimdFp::System { register, .. } if register != System::Fpscr
            );
            let privileged = Mode::from_bits(state.cpsr).is_some_and(Mode::privileged);
            let refused = match grant {
                Grant::None => true,
                Grant::Disabled => !(identification && privileged),
                Grant::Enabled => false,
            };
            if refused {
                assert_eq!(
                    cpu.pc(),
                    BIT32.vectors + Exception::Undefined.offset(),
                    "{instruction:08X} ran without the unit: {grant:?}"
                );
            }
        }
        finals.push((after, cpu.state(), memory.words));
    }
    assert!(finals[0] == finals[1], "{instruction:08X} ran differently");
}

/// `op` with its numbers redrawn from every byte, as an instruction a caller
/// builds may hold them.
#[expect(clippy::too_many_lines, reason = "one arm per form with numbers")]
fn hostile(op: SimdFp, rng: &mut Prng) -> SimdFp {
    let mut byte = || u8::try_from(rng.next_u32() & 0xFF).unwrap_or(0);
    match op {
        SimdFp::CoreScalar {
            to_core,
            unsigned,
            rt,
            ..
        } => SimdFp::CoreScalar {
            to_core,
            unsigned,
            rt,
            size: byte(),
            index: byte(),
            d: byte(),
        },
        SimdFp::CoreDuplicate { q, rt, .. } => SimdFp::CoreDuplicate {
            q,
            rt,
            size: byte(),
            d: byte(),
        },
        SimdFp::Multiple {
            load,
            double,
            rn,
            mode,
            writeback,
            ..
        } => SimdFp::Multiple {
            load,
            double,
            rn,
            mode,
            writeback,
            d: byte(),
            words: byte(),
        },
        SimdFp::Three {
            op, unsigned, q, ..
        } => SimdFp::Three {
            op,
            unsigned,
            q,
            size: byte(),
            d: byte(),
            n: byte(),
            m: byte(),
        },
        SimdFp::Long { op, unsigned, .. } => SimdFp::Long {
            op,
            unsigned,
            size: byte(),
            d: byte(),
            n: byte(),
            m: byte(),
        },
        SimdFp::ByScalar {
            op, unsigned, q, ..
        } => SimdFp::ByScalar {
            op,
            unsigned,
            q,
            size: byte(),
            d: byte(),
            n: byte(),
            m: byte(),
            index: byte(),
        },
        SimdFp::Shift {
            op, unsigned, q, ..
        } => SimdFp::Shift {
            op,
            unsigned,
            q,
            size: byte(),
            d: byte(),
            m: byte(),
            amount: byte(),
        },
        SimdFp::Misc {
            op, unsigned, q, ..
        } => SimdFp::Misc {
            op,
            unsigned,
            q,
            size: byte(),
            d: byte(),
            m: byte(),
        },
        SimdFp::Extract { q, .. } => SimdFp::Extract {
            q,
            d: byte(),
            n: byte(),
            m: byte(),
            start: byte(),
        },
        SimdFp::Table { extend, .. } => SimdFp::Table {
            extend,
            length: byte(),
            d: byte(),
            n: byte(),
            m: byte(),
        },
        SimdFp::Duplicate { q, .. } => SimdFp::Duplicate {
            q,
            size: byte(),
            d: byte(),
            m: byte(),
            index: byte(),
        },
        SimdFp::Structure {
            load,
            shape,
            rn,
            rm,
            ..
        } => SimdFp::Structure {
            load,
            rn,
            rm,
            d: byte(),
            shape: Structure {
                lanes: match shape.lanes {
                    StructureLanes::One(_) => StructureLanes::One(byte()),
                    lanes => lanes,
                },
                elements: byte(),
                parts: byte(),
                spacing: byte(),
                size: byte(),
                align: byte(),
            },
        },
        other => other,
    }
}

/// An instruction built with hostile numbers is encoded, given its text and
/// judged without a panic: refused, or shown as unrepresentable.
fn check_built(rng: &mut Prng) {
    let thumb = rng.below(2) == 0;
    let Some(op) = instruction_at(drawn_instruction(rng, thumb), thumb, 0) else {
        return;
    };
    let built = Instruction {
        cond: Condition::Al,
        op: Op::SimdFp(hostile(op, rng)),
    };
    let _ = a32::encode(&built);
    let _ = t32::encode(&built, Position::Outside, Size::Narrowest);
    let _ = a32::unpredictable(&built, Isa::ALL);
    for set in [Set::A32, Set::T32] {
        assert!(!format!("{}", built.text(set, 0x8000)).is_empty());
    }
}

#[test]
fn any_floating_point_or_simd_instruction_runs_within_the_architecture() {
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "any_floating_point_or_simd_instruction_runs_within_the_architecture",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let mut iterations = 0u64;
    loop {
        match deadline {
            Some(_) if !tairix_fuzzseed::within_budget(deadline) => break,
            None if iterations >= SMOKE_ITERATIONS => break,
            _ => {}
        }
        check_running(&mut rng);
        check_built(&mut rng);
        iterations += 1;
    }
}
