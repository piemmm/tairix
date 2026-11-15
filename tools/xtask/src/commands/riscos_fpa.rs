//! `cargo xtask riscos-fpa <module> [--cases <n>] [--write-sample]`: holds
//! the RISC OS emulator's FPA to FPEmulator (`plans/RISCOS-EMULATOR.md`
//! §2.4).
//!
//! The module runs as an opaque program on the crate's own processor: the
//! 32-bit profile without the FPA, so every FPA instruction traps to it, with
//! the few SWIs its initialisation and its error path call answered here.
//! Each case, a random FPA instruction from a random caller mode over random
//! registers, FPA state and memory, runs under the module and natively on the
//! 32-bit profile, and any difference is reported: the outcome, the registers
//! or the trap's exception dump, memory, and the FPA's registers and FPSR.
//! `--write-sample` writes the first agreeing cases of each class as the
//! vectors the crate's tests replay, with a provenance record.
//!
//! Developer-run only: `<module>` is a developer's own copy of FPEmulator 4,
//! squeezed or not. Its bytes never enter the tree; only its answers do.

use std::ffi::OsString;
use std::fmt::Write as _;

use tairix_fuzzseed::Prng;
use tairix_riscos_arm::a32;
use tairix_riscos_arm::bus::{Abort, Bus, Fetch, Width};
use tairix_riscos_arm::cond::Condition;
use tairix_riscos_arm::cpu::{Cpu, State, Stop};
use tairix_riscos_arm::fpa::{self, Exception, Format, Fpa, Internal, Precision, Rounding};
use tairix_riscos_arm::instruction::{
    CoIndex, FpaMultipleOp, FpaOperand, FpaRegisterOp, Index, Instruction, Offset, Op, TransferOp,
};
use tairix_riscos_arm::isa::Feature;
use tairix_riscos_arm::profile::{Profile, BIT32};
use tairix_riscos_arm::psr::{self, Mode};
use tairix_riscos_arm::reg::Reg;
use tairix_riscos_arm::vectors::{self, FpaCase, FpaState, Transfer, Vector};

use crate::Context;

/// Where the sample and its record are committed.
const SAMPLE_DIR: &str = "userland/apps/riscos/arm/tests/fpa";
/// The sample's name.
const SAMPLE: &str = "fpemulator.bin";
/// The cases run when `--cases` does not say.
const CASES: usize = 20_000;
/// The seed every run draws its cases from, so a sample is reproducible.
const SEED: u64 = 0x4650_4134;
/// How many failures a run names of each class before summarising.
const REPORTED: usize = 6;
/// The instructions the module may take over one guest call.
const BUDGET: u64 = 50_000_000;

/// The classes a case is drawn from, and the sample's share of each.
const CLASSES: [(&str, usize); 4] = [
    ("data operations", 1200),
    ("register transfers", 400),
    ("coprocessor 1 transfers", 400),
    ("coprocessor 2 transfers", 200),
];

/// The guest's memory: low memory, and the high vectors' page.
const LOW: usize = 8 << 20;
const HIGH_BASE: u32 = 0xFFFF_0000;
const HIGH: usize = 0x1_0000;
const MODULE: u32 = 0x0010_0000;
const PRIVATE: u32 = 0x9000;
const ENV: u32 = 0x9010;
const RETURN: u32 = 0x9020;
const PASS_ALONG: u32 = 0x9030;
const ERROR_BLOCK: u32 = 0x9100;
const DUMP: u32 = 0x9200;
/// The word the undefined vector's instruction loads the handler from.
const HANDLER: u32 = HIGH_BASE + 0x100;
const HEAP: u32 = 0x0040_0000;
const SVC_STACK: u32 = 0x0030_0000;
const UND_STACK: u32 = 0x0031_0000;
const USR_STACK: u32 = 0x0034_0000;
const CODE: u32 = 0x8000;
const PREP: u32 = 0x5_0800;
const OUT: u32 = 0x6_0000;
const SCRATCH: u32 = 0x7_0000;
const SCRATCH_WORDS: u32 = 0x240;
const CODE_WINDOW: u32 = CODE - 0x400;
const CODE_WORDS: u32 = 0x200;

/// The SWIs this harness answers for itself, in the space no RISC OS SWI uses.
const DONE: u32 = 0xF0_FFFE;
const RETURNED: u32 = 0xF0_FFFF;
const UNCLAIMED: u32 = 0xF0_0001;
const VECTOR: u32 = 0xF0_0100;

/// The RISC OS SWIs the module calls.
const OS_GENERATE_ERROR: u32 = 0x2B;
const OS_CHANGE_ENVIRONMENT: u32 = 0x40;
const OS_READ_SYS_INFO: u32 = 0x58;
const OS_CLAIM_PROCESSOR_VECTOR: u32 = 0x69;
const OS_MODULE: u32 = 0x1E;
const OS_SYNCHRONISE_CODE_AREAS: u32 = 0x6E;
const MESSAGE_TRANS_OPEN_FILE: u32 = 0x4_1501;
const MESSAGE_TRANS_CLOSE_FILE: u32 = 0x4_1504;
const MESSAGE_TRANS_ERROR_LOOKUP: u32 = 0x4_1506;
/// `OS_ChangeEnvironment`'s exception register dump area.
const EXCEPTION_DUMP_AREA: u32 = 13;
/// FPEmulator's first error number: the exceptions' follow in the order
/// invalid operation, overflow, division by zero, underflow, inexact.
const ERROR_BASE: u32 = 0x8000_0200;

fn usage() -> String {
    "riscos-fpa: usage: cargo xtask riscos-fpa <module> [--cases <n>] [--write-sample]".to_string()
}

const fn swi(number: u32) -> u32 {
    0xEF00_0000 | number
}

/// The guest's memory.
struct Memory {
    low: Vec<u8>,
    high: Vec<u8>,
}

impl Memory {
    fn new() -> Self {
        Self {
            low: vec![0; LOW],
            high: vec![0; HIGH],
        }
    }

    fn at(&mut self, address: u32, bytes: u32) -> Result<&mut [u8], Abort> {
        let (start, len) = (address as usize, bytes as usize);
        let end = start.checked_add(len).ok_or(Abort::Translation)?;
        if end <= self.low.len() {
            return Ok(&mut self.low[start..end]);
        }
        let offset = start
            .checked_sub(HIGH_BASE as usize)
            .ok_or(Abort::Translation)?;
        self.high
            .get_mut(offset..offset + len)
            .ok_or(Abort::Translation)
    }

    fn word(&mut self, address: u32) -> u32 {
        self.at(address, 4)
            .map_or(0, |b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    fn set_word(&mut self, address: u32, value: u32) -> Result<(), String> {
        self.at(address, 4)
            .map(|b| b.copy_from_slice(&value.to_le_bytes()))
            .map_err(|_| format!("riscos-fpa: &{address:08X} is not guest memory"))
    }

    fn put(&mut self, address: u32, bytes: &[u8]) -> Result<(), String> {
        let len = u32::try_from(bytes.len()).map_err(|_| "riscos-fpa: module too large")?;
        self.at(address, len)
            .map(|b| b.copy_from_slice(bytes))
            .map_err(|_| "riscos-fpa: the module does not fit the guest's memory".to_string())
    }

    fn words(&mut self, address: u32, count: u32) -> Vec<u32> {
        (0..count).map(|n| self.word(address + 4 * n)).collect()
    }
}

impl Bus for Memory {
    fn fetch(&mut self, address: u32, _privileged: bool) -> Result<Fetch, Abort> {
        let b = self.at(address, 4)?;
        Ok(Fetch {
            word: u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            host: false,
        })
    }

    fn read(&mut self, address: u32, width: Width, _privileged: bool) -> Result<u32, Abort> {
        let b = self.at(address, width.bytes())?;
        let mut value = [0; 4];
        value[..b.len()].copy_from_slice(b);
        Ok(u32::from_le_bytes(value))
    }

    fn write(
        &mut self,
        address: u32,
        width: Width,
        value: u32,
        _privileged: bool,
    ) -> Result<(), Abort> {
        let b = self.at(address, width.bytes())?;
        let len = b.len();
        b.copy_from_slice(&value.to_le_bytes()[..len]);
        Ok(())
    }
}

/// The bus the native run reaches memory through, recording each data
/// transfer for the vector.
struct Recording<'a> {
    memory: &'a mut Memory,
    transfers: Vec<Transfer>,
}

impl Bus for Recording<'_> {
    fn fetch(&mut self, address: u32, privileged: bool) -> Result<Fetch, Abort> {
        self.memory.fetch(address, privileged)
    }

    fn read(&mut self, address: u32, width: Width, privileged: bool) -> Result<u32, Abort> {
        let data = self.memory.read(address, width, privileged)?;
        self.transfers.push(Transfer {
            write: false,
            width,
            address,
            data,
        });
        Ok(data)
    }

    fn write(
        &mut self,
        address: u32,
        width: Width,
        value: u32,
        privileged: bool,
    ) -> Result<(), Abort> {
        self.memory.write(address, width, value, privileged)?;
        let mask = match width {
            Width::Byte => 0xFF,
            Width::Half => 0xFFFF,
            Width::Word => u32::MAX,
        };
        self.transfers.push(Transfer {
            write: true,
            width,
            address,
            data: value & mask,
        });
        Ok(())
    }
}

/// How one guest call under the module ended.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Outcome {
    /// The code ran to its end.
    Done,
    /// The module raised this error.
    Raised(u32),
    /// The module passed the instruction on as not its own.
    Unclaimed,
}

/// What an instruction did, compared across the two runs.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Ran {
    Done,
    Trapped(Exception),
    Undefined,
}

/// The module, initialised, on the profile without the FPA.
struct Fpe {
    cpu: Cpu,
    memory: Memory,
    heap: u32,
    looked_up: Option<u32>,
}

impl Fpe {
    fn new(module: &[u8], host: &'static Profile) -> Result<Self, String> {
        let mut memory = Memory::new();
        memory.put(MODULE, module)?;
        memory.set_word(RETURN, swi(RETURNED))?;
        memory.set_word(PASS_ALONG, swi(UNCLAIMED))?;
        for (n, offset) in (0..).zip([0u32, 8, 12, 16, 20, 24, 28]) {
            memory.set_word(HIGH_BASE + offset, swi(VECTOR + n))?;
        }
        // `LDR PC,[PC,#&F4]`: the undefined vector loads its claimant.
        memory.set_word(HIGH_BASE + 4, 0xE59F_F0F4)?;
        memory.set_word(HANDLER, PASS_ALONG)?;
        let mut fpe = Self {
            cpu: Cpu::new(host, 64),
            memory,
            heap: HEAP,
            looked_up: None,
        };
        let init = fpe.memory.word(MODULE + 4);
        if init == 0 || init & 0x8000_0000 != 0 {
            return Err("riscos-fpa: the module has no initialisation entry".into());
        }
        let mut state = State {
            cpsr: Mode::Supervisor.bits(),
            pc: MODULE + init,
            ..State::default()
        };
        state.user[10] = ENV;
        state.user[12] = PRIVATE;
        state.banked = [[0; 2], [SVC_STACK, RETURN], [0; 2], [UND_STACK, 0]];
        fpe.set_state(&state)?;
        if fpe.go()? != Outcome::Done || fpe.cpu.cpsr() & psr::V != 0 {
            return Err("riscos-fpa: the module's initialisation failed: a 26-bit \
                        build needs RISC OS 3's kernel around it, which this does not model"
                .into());
        }
        Ok(fpe)
    }

    fn set_state(&mut self, state: &State) -> Result<(), String> {
        self.cpu
            .set_state(state)
            .map_err(|e| format!("riscos-fpa: {e:?}"))
    }

    fn reg(&self, n: u8) -> u32 {
        Reg::new(n.into()).map_or(0, |reg| self.cpu.reg(reg))
    }

    fn set_reg(&mut self, n: u8, value: u32) {
        if let Some(reg) = Reg::new(n.into()) {
            self.cpu.set_reg(reg, value);
        }
    }

    fn set_v(&mut self, v: bool) -> Result<(), String> {
        let cpsr = self.cpu.cpsr();
        self.cpu
            .set_cpsr(if v { cpsr | psr::V } else { cpsr & !psr::V })
            .map_err(|e| format!("riscos-fpa: {e:?}"))
    }

    /// Runs until the guest code ends, the module raises an error, or the
    /// instruction is passed on.
    fn go(&mut self) -> Result<Outcome, String> {
        loop {
            let run = self.cpu.run(&mut self.memory, BUDGET);
            let number = match run.stop {
                Stop::Swi { number } => number & !0x2_0000,
                stop => {
                    return Err(format!(
                        "riscos-fpa: the module stopped: {stop:?} at &{:08X}",
                        self.cpu.pc()
                    ))
                }
            };
            match number {
                DONE | RETURNED => return Ok(Outcome::Done),
                UNCLAIMED => return Ok(Outcome::Unclaimed),
                OS_MODULE => match self.reg(0) {
                    6 => {
                        let size = (self.reg(3) + 15) & !15;
                        self.set_reg(2, self.heap);
                        self.heap += size + 16;
                        self.set_v(false)?;
                    }
                    7 => self.set_v(false)?,
                    reason => return Err(format!("riscos-fpa: OS_Module {reason}")),
                },
                OS_READ_SYS_INFO if self.reg(0) == 6 && self.reg(2) == 16 => {
                    self.set_reg(2, SVC_STACK);
                    self.set_v(false)?;
                }
                OS_SYNCHRONISE_CODE_AREAS | MESSAGE_TRANS_OPEN_FILE | MESSAGE_TRANS_CLOSE_FILE => {
                    self.set_v(false)?;
                }
                OS_CLAIM_PROCESSOR_VECTOR if self.reg(0) & 0xFF == 1 => {
                    let old = self.memory.word(HANDLER);
                    self.memory.set_word(HANDLER, self.reg(1))?;
                    self.set_reg(1, old);
                    self.set_v(false)?;
                }
                MESSAGE_TRANS_ERROR_LOOKUP => {
                    let number = self.memory.word(self.reg(0));
                    self.looked_up = Some(number);
                    self.memory.set_word(ERROR_BLOCK, number)?;
                    self.set_reg(0, ERROR_BLOCK);
                    self.set_v(true)?;
                }
                OS_CHANGE_ENVIRONMENT if self.reg(0) == EXCEPTION_DUMP_AREA => {
                    self.set_reg(1, DUMP);
                    self.set_reg(2, 0);
                    self.set_reg(3, 0);
                    self.set_v(false)?;
                }
                OS_CHANGE_ENVIRONMENT => self.set_v(true)?,
                OS_GENERATE_ERROR => {
                    let number = self
                        .looked_up
                        .take()
                        .ok_or("riscos-fpa: an error raised that was never looked up")?;
                    return Ok(Outcome::Raised(number));
                }
                n if (VECTOR..VECTOR + 7).contains(&n) => {
                    return Err(format!(
                        "riscos-fpa: the module took vector {} at &{:08X}",
                        n - VECTOR,
                        self.cpu.state().banked[2][1]
                    ))
                }
                n => {
                    return Err(format!(
                        "riscos-fpa: an unanswered SWI &{n:X} at &{:08X}",
                        self.cpu.pc()
                    ))
                }
            }
        }
    }

    /// Runs `code` from `state`, its PC set to the code, then the harness's
    /// end.
    fn run(&mut self, code: &[u32], state: &State) -> Result<Outcome, String> {
        place(&mut self.memory, code)?;
        self.set_state(&State { pc: CODE, ..*state })?;
        self.go()
    }

    /// Runs `code` in User mode with R1 at [`OUT`] and R2 at `r2`.
    fn user(&mut self, code: &[u32], r2: u32) -> Result<Outcome, String> {
        let mut state = State {
            cpsr: Mode::User.bits(),
            ..State::default()
        };
        state.user[1] = OUT;
        state.user[2] = r2;
        state.user[13] = USR_STACK;
        state.banked = [[0; 2], [SVC_STACK, 0], [0; 2], [UND_STACK, 0]];
        self.run(code, &state)
    }

    /// Loads the FPA's registers as `LFM` would from `fpa`'s words, and its
    /// FPSR as `WFS` would.
    fn prepare(&mut self, fpa: &FpaState) -> Result<(), String> {
        for (n, words) in (0..).zip(fpa.registers) {
            for (m, word) in (0..).zip(words) {
                self.memory.set_word(PREP + 12 * n + 4 * m, word)?;
            }
        }
        let load = [
            multiple(FpaMultipleOp::Lfm, 0, 2, 0)?,
            multiple(FpaMultipleOp::Lfm, 4, 2, 12)?,
        ];
        expect_done(self.user(&load, PREP)?)?;
        expect_done(self.user(&[register(FpaRegisterOp::Wfs, 2)?], fpa.fpsr)?)
    }

    /// The FPA's registers and FPSR as `SFM` and `RFS` read them back.
    fn read_back(&mut self) -> Result<FpaState, String> {
        let code = [
            multiple(FpaMultipleOp::Sfm, 0, 1, 0)?,
            multiple(FpaMultipleOp::Sfm, 4, 1, 12)?,
            register(FpaRegisterOp::Rfs, 3)?,
            store_word(3, 1, 96)?,
        ];
        expect_done(self.user(&code, 0)?)?;
        let words = self.memory.words(OUT, 25);
        let mut state = FpaState::default();
        state
            .registers
            .as_flattened_mut()
            .copy_from_slice(&words[..24]);
        state.fpsr = words[24];
        Ok(state)
    }
}

fn expect_done(outcome: Outcome) -> Result<(), String> {
    if outcome == Outcome::Done {
        Ok(())
    } else {
        Err(format!(
            "riscos-fpa: the harness's own code ended {outcome:?}"
        ))
    }
}

/// Writes `code` at [`CODE`], then the SWI that ends the guest call.
fn place(memory: &mut Memory, code: &[u32]) -> Result<(), String> {
    for (n, &word) in (0..).zip(code) {
        memory.set_word(CODE + 4 * n, word)?;
    }
    let end = u32::try_from(code.len()).map_err(|_| "riscos-fpa: code too long")?;
    memory.set_word(CODE + 4 * end, swi(DONE))
}

fn encoded(op: Op) -> Result<u32, String> {
    a32::encode(&Instruction {
        cond: Condition::Al,
        op,
    })
    .map_err(|e| format!("riscos-fpa: {op:?}: {e:?}"))
}

fn reg(n: u8) -> Result<Reg, String> {
    Reg::new(n.into()).ok_or_else(|| format!("riscos-fpa: no register {n}"))
}

/// `LFM` or `SFM` of four registers from `fd` at `[Rn, #4×offset]`.
fn multiple(op: FpaMultipleOp, fd: u8, rn: u8, offset: u8) -> Result<u32, String> {
    encoded(Op::FpaMultiple {
        op,
        fd,
        count: 4,
        rn: reg(rn)?,
        up: true,
        index: CoIndex::Offset,
        offset,
    })
}

/// `WFS` or `RFS` of `rd`.
fn register(op: FpaRegisterOp, rd: u8) -> Result<u32, String> {
    encoded(Op::FpaRegister {
        op,
        precision: Precision::Single,
        rounding: Rounding::Nearest,
        fn_: 0,
        fm: FpaOperand::Register(0),
        rd: reg(rd)?,
    })
}

/// `STR Rt,[Rn,#offset]`.
fn store_word(rt: u8, rn: u8, offset: u16) -> Result<u32, String> {
    encoded(Op::Transfer {
        op: TransferOp::Str,
        rt: reg(rt)?,
        rn: reg(rn)?,
        up: true,
        offset: Offset::Immediate(offset),
        index: Index::Offset,
    })
}

/// The 32-bit profile, running each instruction natively.
struct Native {
    cpu: Cpu,
    memory: Memory,
}

impl Native {
    fn new() -> Result<Self, String> {
        let mut memory = Memory::new();
        for (n, offset) in (0..).zip([0u32, 4, 8, 12, 16, 20, 24, 28]) {
            memory.set_word(HIGH_BASE + offset, swi(VECTOR + n))?;
        }
        Ok(Self {
            cpu: Cpu::new(&BIT32, 64),
            memory,
        })
    }
}

/// One case: an instruction, and everything it starts from.
struct Case {
    word: u32,
    mode: Mode,
    state: State,
    fpa: FpaState,
    scratch: Vec<u32>,
}

fn chance(rng: &mut Prng, n: usize) -> bool {
    rng.below(n) == 0
}

fn small(rng: &mut Prng, n: usize) -> u32 {
    u32::try_from(rng.below(n)).unwrap_or(0)
}

fn sign(rng: &mut Prng) -> u32 {
    (rng.next_u32() & 1) << 31
}

/// A value as `LDF` of `words` in `format` leaves it.
fn loaded(format: Format, words: [u32; 3]) -> Internal {
    let mut fpa = Fpa::new();
    fpa.load(format, 0, [words[0], words[1], words[2], 0])
        .map(|()| fpa.register(0))
        .unwrap_or_default()
}

/// A register value from the kinds the arithmetic treats differently:
/// single and double precision ones with their own denormals, infinities
/// and NaNs, extended ones over the whole exponent range, unnormal patterns
/// `LFM` keeps, ordinary numbers, and small integers.
fn value(rng: &mut Prng) -> Internal {
    match rng.below(11) {
        0 | 1 => {
            let exponent = match rng.below(6) {
                0 => 0,
                1 => 0xFF,
                2 => 1,
                3 => 0xFE,
                _ => small(rng, 0x100),
            };
            let fraction = match rng.below(4) {
                0 => 0,
                1 => 1 + small(rng, 4),
                2 => 0x40_0000 | small(rng, 0x40_0000),
                _ => small(rng, 0x80_0000),
            };
            loaded(
                Format::Single,
                [sign(rng) | exponent << 23 | fraction, 0, 0],
            )
        }
        2 | 3 => {
            let exponent = match rng.below(6) {
                0 => 0,
                1 => 0x7FF,
                2 => 1,
                3 => 0x7FE,
                4 => 0x380 + small(rng, 0x100),
                _ => small(rng, 0x800),
            };
            let (high, low) = match rng.below(4) {
                0 => (0, 0),
                1 => (0, 1 + small(rng, 4)),
                2 => (0x8_0000 | small(rng, 0x8_0000), rng.next_u32()),
                _ => (small(rng, 0x10_0000), rng.next_u32()),
            };
            loaded(Format::Double, [sign(rng) | exponent << 20 | high, low, 0])
        }
        4..=6 => {
            let exponent = match rng.below(8) {
                0 => 0,
                1 => 0x7FFF,
                2 => 1,
                3 => 0x7FFE,
                4 => 0x3FDF + small(rng, 64),
                5 => 0x3F00 + small(rng, 0x200),
                _ => small(rng, 0x8000),
            };
            let (high, low) = match rng.below(6) {
                0 => (0, 0),
                1 => (0x8000_0000, 0),
                2 => (rng.next_u32() & 0x7FFF_FFFF, rng.next_u32()),
                3 => (0x8000_0000 | small(rng, 16) << 28, 0),
                4 => (u32::MAX, u32::MAX),
                _ => (rng.next_u32() | 0x8000_0000, rng.next_u32()),
            };
            loaded(Format::Extended, [sign(rng) | exponent, high, low])
        }
        7 => {
            let exponent = match rng.below(4) {
                0 => 0x3F80,
                1 => 0x3C00,
                2 => 0x407F + small(rng, 2) * 0x380,
                _ => small(rng, 0x8000),
            };
            Internal::from_words([
                sign(rng) | 1 << 30 | exponent,
                rng.next_u32(),
                rng.next_u32(),
            ])
        }
        8 | 9 => loaded(
            Format::Extended,
            [
                sign(rng) | (0x3FF0 + small(rng, 0x14)),
                rng.next_u32() | 0x8000_0000,
                rng.next_u32(),
            ],
        ),
        _ => {
            let mut fpa = Fpa::new();
            let n = small(rng, 64).wrapping_sub(32);
            fpa.float(Precision::Extended, Rounding::Nearest, 0, n)
                .map(|()| fpa.register(0))
                .unwrap_or_default()
        }
    }
}

/// An FPSR: traps enabled now and then, the control bits, and flags.
fn status(rng: &mut Prng) -> u32 {
    let mut fpsr = fpa::fpsr::SYSTEM_ID;
    for exception in Exception::ALL {
        if chance(rng, 4) {
            fpsr |= exception.enable();
        }
    }
    for bit in [fpa::fpsr::ND, fpa::fpsr::NE, fpa::fpsr::AC, fpa::fpsr::EP] {
        if chance(rng, 3) {
            fpsr |= bit;
        }
    }
    if chance(rng, 4) {
        fpsr |= small(rng, 32);
    }
    fpsr
}

/// A word in one class's space, mostly unconditional: data operations,
/// register transfers, coprocessor 1's loads and stores or coprocessor 2's.
fn word(class: usize, rng: &mut Prng) -> u32 {
    let cond = if chance(rng, 6) {
        small(rng, 15) << 28
    } else {
        0xE000_0000
    };
    let bits = rng.next_u32();
    match class {
        0 => cond | 0x0E00_0100 | bits & 0x00FF_F0EF,
        1 => cond | 0x0E00_0110 | bits & 0x00FF_F0EF,
        2 => cond | 0x0C00_0100 | bits & 0x01FF_F0FF,
        _ => cond | 0x0C00_0200 | bits & 0x01FF_F0FF,
    }
}

/// The span of bytes from where a transfer `word` with base `base` starts.
fn reach(word: u32, base: u32) -> (u32, u32) {
    let offset = 4 * (word & 0xFF);
    let moved = if word & 1 << 23 != 0 {
        base.wrapping_add(offset)
    } else {
        base.wrapping_sub(offset)
    };
    let at = if word & 1 << 24 != 0 { moved } else { base };
    (at, at.wrapping_add(48))
}

/// A case of `class`, or `None` where a PC-relative store would overwrite
/// the instruction itself.
///
/// FPEmulator does not survive FIQ mode and runs in Undefined mode itself,
/// so neither is a caller here. R6's sign is clear: FPEmulator signs the NaN
/// of a few invalid operations by it.
fn case(class: usize, rng: &mut Prng) -> Option<Case> {
    let word = word(class, rng);
    let mode = *rng.pick(&[
        Mode::User,
        Mode::Supervisor,
        Mode::System,
        Mode::Irq,
        Mode::Abort,
    ]);
    let mut user = [0; 15];
    for slot in &mut user {
        *slot = match rng.below(4) {
            0 => small(rng, 64),
            1 => small(rng, 1 << 20).wrapping_neg(),
            _ => rng.next_u32(),
        };
    }
    user[6] &= 0x7FFF_FFFF;
    let mut banked = [[0; 2]; 4];
    for pair in &mut banked[..3] {
        *pair = [rng.next_u32(), rng.next_u32()];
    }
    banked[3] = [UND_STACK, 0];
    let mut fiq = [0; 7];
    for slot in &mut fiq {
        *slot = rng.next_u32();
    }
    let rn = ((word >> 16) & 15) as usize;
    if class >= 2 {
        let base = SCRATCH + 0x400 + 4 * small(rng, 128);
        if rn < 15 {
            user[rn] = base;
            let bank = match mode {
                Mode::Irq => Some(0),
                Mode::Supervisor => Some(1),
                Mode::Abort => Some(2),
                _ => None,
            };
            if let (Some(bank), 13 | 14) = (bank, rn) {
                banked[bank][rn - 13] = base;
            }
        } else if word & 1 << 20 == 0 {
            let (from, to) = reach(word, CODE + 8);
            if to > CODE && from < CODE + 8 {
                return None;
            }
        }
    }
    let mut fpa = FpaState::default();
    for slot in &mut fpa.registers {
        *slot = value(rng).words();
    }
    fpa.fpsr = status(rng);
    let state = State {
        user,
        fiq,
        banked,
        pc: CODE,
        cpsr: mode.bits() | small(rng, 16) << 28,
        spsr: [0; 5],
    };
    Some(Case {
        word,
        mode,
        state,
        fpa,
        scratch: (0..SCRATCH_WORDS).map(|_| rng.next_u32()).collect(),
    })
}

/// Fills the scratch and code windows with the case's memory.
fn fill(memory: &mut Memory, case: &Case) -> Result<(), String> {
    for (n, &word) in (0..).zip(&case.scratch) {
        memory.set_word(SCRATCH + 4 * n, word)?;
    }
    for n in 0..CODE_WORDS {
        memory.set_word(CODE_WINDOW + 4 * n, 0xE1A0_0000 ^ n << 4)?;
    }
    Ok(())
}

/// The scratch and code windows, the instruction's own two words left out.
fn windows(memory: &mut Memory) -> Vec<u32> {
    let mut words = memory.words(SCRATCH, SCRATCH_WORDS);
    words.extend(
        (0..CODE_WORDS)
            .map(|n| CODE_WINDOW + 4 * n)
            .filter(|&at| !(CODE..CODE + 8).contains(&at))
            .map(|at| memory.word(at)),
    );
    words
}

/// The registers every caller sees: all but Undefined mode's bank and
/// SPSR, which are FPEmulator's.
#[derive(PartialEq, Eq, Debug)]
struct Visible {
    user: [u32; 15],
    fiq: [u32; 7],
    banked: [[u32; 2]; 3],
    cpsr: u32,
    spsr: [u32; 4],
}

fn visible(state: &State) -> Visible {
    Visible {
        user: state.user,
        fiq: state.fiq,
        banked: [state.banked[0], state.banked[1], state.banked[2]],
        cpsr: state.cpsr,
        spsr: [state.spsr[0], state.spsr[1], state.spsr[2], state.spsr[3]],
    }
}

/// The exception FPEmulator's error `number` reports.
fn exception_of(number: u32) -> Option<Exception> {
    [
        Exception::InvalidOperation,
        Exception::Overflow,
        Exception::DivisionByZero,
        Exception::Underflow,
        Exception::Inexact,
    ]
    .get(number.checked_sub(ERROR_BASE)? as usize)
    .copied()
}

/// Runs `case` both ways: its vector where they agree, or what differed.
fn compare(
    fpe: &mut Fpe,
    native: &mut Native,
    case: &Case,
) -> Result<Result<Vector, String>, String> {
    fpe.prepare(&case.fpa)?;
    let before = fpe.read_back()?;
    fill(&mut fpe.memory, case)?;
    for n in 0..17 {
        fpe.memory.set_word(DUMP + 4 * n, 0xD0D0_D0D0)?;
    }
    let outcome = fpe.run(&[case.word], &case.state)?;
    let fpe_state = fpe.cpu.state();
    let dump = fpe.memory.words(DUMP, 17);
    let fpe_windows = windows(&mut fpe.memory);
    let fpe_after = fpe.read_back()?;
    let fpe_ran = match outcome {
        Outcome::Done => Ran::Done,
        Outcome::Unclaimed => Ran::Undefined,
        Outcome::Raised(number) => Ran::Trapped(
            exception_of(number).ok_or_else(|| format!("riscos-fpa: error &{number:08X}"))?,
        ),
    };

    fill(&mut native.memory, case)?;
    place(&mut native.memory, &[case.word])?;
    native
        .cpu
        .set_state(&case.state)
        .map_err(|e| format!("riscos-fpa: {e:?}"))?;
    before.set(native.cpu.fpa_mut());
    let mut bus = Recording {
        memory: &mut native.memory,
        transfers: Vec::new(),
    };
    let stop = native.cpu.run(&mut bus, 1).stop;
    let transfers = bus.transfers;
    let after = native.cpu.state();
    let native_after = FpaState::of(native.cpu.fpa());
    let native_windows = windows(&mut native.memory);
    let native_ran = match stop {
        Stop::FloatingPoint { exception } => Ran::Trapped(exception),
        Stop::Budget if after.cpsr & psr::M == Mode::Undefined.bits() => Ran::Undefined,
        Stop::Budget => Ran::Done,
        stop => return Err(format!("riscos-fpa: &{:08X} stopped {stop:?}", case.word)),
    };

    let mut differences = Vec::new();
    if fpe_ran != native_ran {
        differences.push(format!(
            "outcome: FPEmulator {fpe_ran:?}, native {native_ran:?}"
        ));
    }
    if fpe_after != native_after {
        differences.push(format!(
            "FPA: FPEmulator {fpe_after:08X?}, native {native_after:08X?}"
        ));
    }
    if let Some(n) = (0..fpe_windows.len()).find(|&n| fpe_windows[n] != native_windows[n]) {
        differences.push(format!(
            "memory word {n} of the windows: FPEmulator &{:08X}, native &{:08X}",
            fpe_windows[n], native_windows[n]
        ));
    }
    match native_ran {
        // The harness's end SWI is one instruction further on.
        Ran::Done if visible(&fpe_state) != visible(&after) || fpe_state.pc != after.pc + 4 => {
            differences.push(format!(
                "registers: FPEmulator {:08X?}, native {:08X?}",
                visible(&fpe_state),
                visible(&after)
            ));
        }
        Ran::Trapped(_) => {
            let mut expected = after.user.to_vec();
            expected.push(after.pc);
            if dump[..16] != expected[..] || dump[16] & 0xF000_001F != after.cpsr & 0xF000_001F {
                differences.push(format!(
                    "exception dump: FPEmulator {dump:08X?}, native {expected:08X?} &{:08X}",
                    after.cpsr
                ));
            }
        }
        _ => {}
    }
    if !differences.is_empty() {
        return Ok(Err(differences.join("; ")));
    }
    Ok(Ok(Vector {
        word: case.word,
        before: case.state,
        after,
        psr: u32::MAX,
        transfers,
        fpa: Some(FpaCase {
            before,
            after: native_after,
            trap: match native_ran {
                Ran::Trapped(exception) => Some(exception),
                Ran::Done | Ran::Undefined => None,
            },
        }),
    }))
}

/// A squeezed module expanded, as the RISC OS kernel's module loader
/// expands one: the initialisation offset's top bit marks it, and the
/// squeezed data ends with its sizes and the two tables of common words.
fn unsqueeze(data: &[u8]) -> Result<Vec<u8>, String> {
    let word = |at: usize| -> Result<usize, String> {
        data.get(at..at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .ok_or_else(malformed)
    };
    let end = word(4)? & 0x7FFF_FFFF;
    let sizes = end.checked_sub(20).ok_or_else(malformed)?;
    let [decoded, encoded, table, shorts, longs] =
        [0, 4, 8, 12, 16].map(|offset| word(sizes + offset));
    let (decoded, encoded, table) = (decoded?, encoded?, table?);
    let table_start = sizes.checked_sub(table).ok_or_else(malformed)?;
    let data_start = table_start.checked_sub(encoded).ok_or_else(malformed)?;
    let mut at = table_start;
    let tables = Tables {
        short: common_words(data, &mut at, shorts?, 4)?,
        long: common_words(data, &mut at, longs?, 3)?,
    };
    let mut out = vec![0; decoded];
    let mut q = decoded;
    let mut r = table_start;
    while r > data_start {
        let pair = back(data, &mut r)?;
        let first = tables.word(data, usize::from(pair & 15), &mut r)?;
        let second = tables.word(data, usize::from(pair >> 4), &mut r)?;
        q = q.checked_sub(8).ok_or_else(malformed)?;
        out[q..q + 4].copy_from_slice(&first.to_le_bytes());
        out[q + 4..q + 8].copy_from_slice(&second.to_le_bytes());
    }
    if q == 0 {
        Ok(out)
    } else {
        Err(malformed())
    }
}

fn malformed() -> String {
    "riscos-fpa: the squeezed module is malformed".to_string()
}

/// The byte before `r`, which the squeezed data is read back from.
fn back(data: &[u8], r: &mut usize) -> Result<u8, String> {
    *r = r.checked_sub(1).ok_or_else(malformed)?;
    data.get(*r).copied().ok_or_else(malformed)
}

/// A squeezed module's two tables of common words.
struct Tables {
    short: Vec<u32>,
    long: Vec<u32>,
}

impl Tables {
    /// The word a nibble names: zero, four bytes spelled whole, a short
    /// table entry by one more byte, or a long one's top three bytes by one
    /// more and its low byte by another.
    fn word(&self, data: &[u8], nibble: usize, r: &mut usize) -> Result<u32, String> {
        const SHORT: usize = 9;
        const LONG: usize = 2;
        Ok(match nibble {
            0 => 0,
            1 => {
                let mut value = 0;
                for shift in [0, 8, 16, 24] {
                    value |= u32::from(back(data, r)?) << shift;
                }
                value
            }
            n if n >= SHORT => *self
                .short
                .get((n - SHORT) << 8 | usize::from(back(data, r)?))
                .ok_or_else(malformed)?,
            n => {
                let high = *self
                    .long
                    .get((n - LONG) << 8 | usize::from(back(data, r)?))
                    .ok_or_else(malformed)?;
                high << 8 | u32::from(back(data, r)?)
            }
        })
    }
}

/// One of a squeezed module's tables of common words: each a delta from the
/// last, short forms for small deltas, `bytes` long when spelled whole.
fn common_words(data: &[u8], at: &mut usize, count: usize, bytes: u32) -> Result<Vec<u32>, String> {
    let next = |at: &mut usize| -> Result<u32, String> {
        let byte = data.get(*at).copied().ok_or_else(malformed)?;
        *at += 1;
        Ok(u32::from(byte))
    };
    let mut words = Vec::with_capacity(count);
    let mut last = u32::MAX;
    while words.len() < count {
        let code = next(at)?;
        let delta = match code {
            0 => {
                let mut value = 0;
                for shift in (0..bytes).map(|n| 8 * n) {
                    value |= next(at)? << shift;
                }
                value
            }
            1..=9 => {
                for _ in 0..code {
                    last = last.wrapping_add(1);
                    words.push(last);
                }
                continue;
            }
            10..=91 => code - 10,
            92..=173 => next(at)? | (code - 92) << 8,
            _ => {
                let low = next(at)?;
                let high = next(at)?;
                (code - 174) << 16 | high << 8 | low
            }
        };
        last = last.wrapping_add(delta);
        words.push(last);
    }
    Ok(words)
}

/// The module's title and help string, from its header.
fn identity(module: &[u8]) -> Result<(String, String), String> {
    let text = |offset_at: usize| -> Result<String, String> {
        let offset = module
            .get(offset_at..offset_at + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as usize)
            .ok_or("riscos-fpa: the module's header is cut short")?;
        let tail = module
            .get(offset..)
            .ok_or("riscos-fpa: the module's header points outside it")?;
        let end = tail.iter().position(|&b| b == 0).unwrap_or(tail.len());
        Ok(tail[..end]
            .iter()
            .map(|&b| if b == b'\t' { ' ' } else { char::from(b) })
            .collect())
    };
    Ok((text(0x10)?, text(0x14)?))
}

pub fn run(ctx: &Context, args: &[OsString]) -> Result<(), String> {
    let args: Vec<&str> = args
        .iter()
        .map(|arg| arg.to_str().ok_or_else(usage))
        .collect::<Result<_, _>>()?;
    let (path, rest) = args.split_first().ok_or_else(usage)?;
    let mut cases = CASES;
    let mut write = false;
    let mut rest = rest.iter();
    while let Some(&arg) = rest.next() {
        match arg {
            "--write-sample" => write = true,
            "--cases" => {
                cases = rest.next().and_then(|n| n.parse().ok()).ok_or_else(usage)?;
            }
            _ => return Err(usage()),
        }
    }
    let raw = std::fs::read(path).map_err(|e| format!("riscos-fpa: {path}: {e}"))?;
    let squeezed = raw
        .get(4..8)
        .is_some_and(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & 0x8000_0000 != 0);
    let module = if squeezed {
        unsqueeze(&raw)?
    } else {
        raw.clone()
    };
    let (title, help) = identity(&module)?;
    if title != "FPEmulator" {
        return Err(format!("riscos-fpa: {path} is {title}, not FPEmulator"));
    }
    noteln!(
        "xtask: [riscos-fpa] {help}{}",
        if squeezed { ", unsqueezed" } else { "" }
    );
    let host: &'static Profile = Box::leak(Box::new(Profile {
        name: "32-bit without the FPA",
        isa: BIT32.isa.without(Feature::Fpa),
        ..BIT32
    }));
    let mut fpe = Fpe::new(&module, host)?;
    let mut native = Native::new()?;
    let (tallies, skipped) = tally(&mut fpe, &mut native, cases)?;
    let mut table = String::new();
    for (tally, &(name, _)) in tallies.iter().zip(&CLASSES) {
        noteln!(
            "xtask: [riscos-fpa] {name}: {} cases, {} differ, {} trapped, {} undefined",
            tally.ran,
            tally.failed,
            tally.trapped,
            tally.undefined
        );
        let _ = writeln!(
            table,
            "| {name} | {} | {} | {} | {} |",
            tally.ran,
            tally.trapped,
            tally.undefined,
            tally.sample.len()
        );
    }
    let failures: usize = tallies.iter().map(|tally| tally.failed).sum();
    if failures != 0 {
        return Err(format!(
            "riscos-fpa: {failures} cases differ from FPEmulator"
        ));
    }
    if write {
        let record = Record {
            help: &help,
            squeezed,
            digest: tairix_util::hex::encode(&tairix_crypto::sha256(&raw)),
            cases,
            skipped,
            table: &table,
        };
        let sample: Vec<Vector> = tallies.into_iter().flat_map(|tally| tally.sample).collect();
        write_sample(ctx, &record, &sample)?;
    }
    Ok(())
}

/// What the cases of one class came to.
#[derive(Default)]
struct Tally {
    ran: usize,
    failed: usize,
    trapped: usize,
    undefined: usize,
    sample: Vec<Vector>,
}

/// Runs `cases` cases both ways, the classes in turn: each class's tally,
/// and how many cases were skipped.
fn tally(fpe: &mut Fpe, native: &mut Native, cases: usize) -> Result<(Vec<Tally>, usize), String> {
    let mut rng = Prng::new(SEED);
    let mut tallies: Vec<Tally> = CLASSES.iter().map(|_| Tally::default()).collect();
    let mut skipped = 0;
    for n in 0..cases {
        let class = n % CLASSES.len();
        let tally = &mut tallies[class];
        let Some(case) = case(class, &mut rng) else {
            skipped += 1;
            continue;
        };
        tally.ran += 1;
        match compare(fpe, native, &case)? {
            Ok(vector) => {
                if matches!(vector.fpa, Some(FpaCase { trap: Some(_), .. })) {
                    tally.trapped += 1;
                } else if vector.after.cpsr & psr::M == Mode::Undefined.bits() {
                    tally.undefined += 1;
                }
                if tally.sample.len() < CLASSES[class].1 {
                    tally.sample.push(vector);
                }
            }
            Err(difference) => {
                tally.failed += 1;
                if tally.failed <= REPORTED {
                    noteln!(
                        "xtask: [riscos-fpa] {} &{:08X} in {:?} mode: {difference}",
                        CLASSES[class].0,
                        case.word,
                        case.mode
                    );
                }
            }
        }
    }
    Ok((tallies, skipped))
}

/// What the sample's provenance file states.
struct Record<'a> {
    help: &'a str,
    squeezed: bool,
    digest: String,
    cases: usize,
    skipped: usize,
    table: &'a str,
}

fn write_sample(ctx: &Context, record: &Record<'_>, sample: &[Vector]) -> Result<(), String> {
    let out = ctx.workspace_root.join(SAMPLE_DIR);
    std::fs::create_dir_all(&out)
        .map_err(|e| format!("riscos-fpa: cannot create {}: {e}", out.display()))?;
    let encoded = vectors::encode(sample).map_err(|e| format!("riscos-fpa: {e:?}"))?;
    std::fs::write(out.join(SAMPLE), encoded).map_err(|e| format!("riscos-fpa: {SAMPLE}: {e}"))?;
    let readme = format!(
        "# FPA conformance vectors\n\n\
         Generated by `cargo xtask riscos-fpa <module> --write-sample`; never edit\n\
         by hand.\n\n\
         Source: the answers of\n\n    {help}\n\n\
         run as an opaque program on the emulator's own processor{squeezed}.\n\
         The module file's SHA-256:\n\n    {digest}\n\n\
         Each vector is an FPA instruction run natively by the processor: the\n\
         registers, FPA state and memory the harness drew for it, and what the\n\
         processor left. It is kept only where FPEmulator, given the same, left\n\
         the same in every register, every word of memory, the FPA's registers\n\
         and FPSR, and a trap's exception dump. FPEmulator's answers are only\n\
         compared, so nothing of the module's is carried here.\n\n\
         `{SAMPLE}` is the `tairix_riscos_arm::vectors` encoding of the first\n\
         agreeing cases of each class, drawn from seed `{SEED:#X}` over {cases}\n\
         cases, {skipped} of them skipped as PC-relative stores over the\n\
         instruction. Every case agreed when the sample was written.\n\n\
         | Class | Cases | Trapped | Undefined | Sampled |\n|---|---|---|---|---|\n\
         {table}",
        help = record.help,
        squeezed = if record.squeezed {
            ",\nunsqueezed as the kernel's module loader does"
        } else {
            ""
        },
        digest = record.digest,
        cases = record.cases,
        skipped = record.skipped,
        table = record.table,
    );
    std::fs::write(out.join("README.md"), readme)
        .map_err(|e| format!("riscos-fpa: README.md: {e}"))?;
    noteln!(
        "xtask: [riscos-fpa] wrote {} vectors to {}",
        sample.len(),
        out.join(SAMPLE).display()
    );
    Ok(())
}
