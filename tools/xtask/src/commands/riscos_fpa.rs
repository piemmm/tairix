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
//! as RISC OS holds it in memory rather than squeezed. Its bytes never enter
//! the tree; only its answers do.

use std::ffi::OsString;
use std::fmt::Write as _;

use tairix_fuzzseed::Prng;
use tairix_riscos_arm::a32;
use tairix_riscos_arm::bus::{Abort, Bus, Fetch, Width};
use tairix_riscos_arm::cond::Condition;
use tairix_riscos_arm::cpu::{Cpu, State, Stop};
use tairix_riscos_arm::fpa::{self, Exception, Format, Fpa, Internal, Precision, Rounding};
use tairix_riscos_arm::instruction::{
    CallOp, CoIndex, FpaMultipleOp, FpaOperand, FpaRegisterOp, FpaTransferOp, Index, Instruction,
    Offset, Op, TransferOp,
};
use tairix_riscos_arm::isa::Feature;
use tairix_riscos_arm::mmu::PAGE_BITS;
use tairix_riscos_arm::profile::{Profile, BIT32};
use tairix_riscos_arm::psr::{self, Mode};
use tairix_riscos_arm::reg::Reg;
use tairix_riscos_arm::vectors::{self, FpaCase, FpaState, Transfer, Vector};
use tairix_softfloat::{Decimal, Packed};

use crate::Context;

mod accuracy;

use accuracy::{Accepted, Difference, Left, Unheld};

/// Where the sample and its record are committed.
const SAMPLE_DIR: &str = "userland/apps/riscos/arm/tests/fpa";
/// The sample's name.
const SAMPLE: &str = "fpemulator-vectors.bin";
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
/// A transfer's base lies this far into the scratch window, at one of so
/// many words.
const BASE: u32 = 0x400;
const BASES: u32 = 128;
/// The furthest a transfer reaches from its base: the largest offset, and an
/// `LFM` of four registers.
const REACH: u32 = 4 * 0xFF + 48;
/// Every word a transfer from a drawn base reaches, so each case starts from
/// the same memory and every word moved is compared.
const SCRATCH_WORDS: u32 = (BASE + 4 * BASES + REACH).div_ceil(4);
/// Likewise every word a PC-relative transfer reaches.
const CODE_WINDOW: u32 = CODE - 0x400;
const CODE_WORDS: u32 = (0x400 + 8 + REACH).div_ceil(4);

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

fn swi(number: u32) -> Result<u32, String> {
    encoded(Op::Call {
        op: CallOp::Swi,
        value: number,
    })
}

/// The 32-bit profile without the FPA, so every FPA instruction traps to the
/// module.
static HOST: Profile = Profile {
    name: "32-bit without the FPA",
    isa: BIT32.isa.without(Feature::Fpa),
    ..BIT32
};

/// The most a module may take: up to the heap above it.
const MODULE_ROOM: usize = (HEAP - MODULE) as usize;

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
            page: address >> PAGE_BITS,
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
        self.transfers.push(Transfer {
            write: true,
            width,
            address,
            data: value & width.mask(),
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
    fn new(module: &[u8]) -> Result<Self, String> {
        if module.len() > MODULE_ROOM {
            return Err("riscos-fpa: the module does not fit below the guest's heap".into());
        }
        let mut memory = Memory::new();
        memory.put(MODULE, module)?;
        memory.set_word(RETURN, swi(RETURNED)?)?;
        memory.set_word(PASS_ALONG, swi(UNCLAIMED)?)?;
        for (n, offset) in (0..).zip([0u32, 8, 12, 16, 20, 24, 28]) {
            memory.set_word(HIGH_BASE + offset, swi(VECTOR + n)?)?;
        }
        // The undefined vector loads its claimant from `HANDLER`.
        let claimant = encoded(Op::Transfer {
            op: TransferOp::Ldr,
            rt: Reg::PC,
            rn: Reg::PC,
            up: true,
            offset: Offset::Immediate(0xF4),
            index: Index::Offset,
        })?;
        memory.set_word(HIGH_BASE + 4, claimant)?;
        memory.set_word(HANDLER, PASS_ALONG)?;
        let mut fpe = Self {
            cpu: Cpu::new(&HOST, 64),
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
                        let end = self
                            .reg(3)
                            .checked_next_multiple_of(16)
                            .and_then(|size| self.heap.checked_add(size)?.checked_add(16))
                            .filter(|&end| usize::try_from(end).is_ok_and(|end| end <= LOW))
                            .ok_or("riscos-fpa: the module claimed more than the guest's heap")?;
                        self.set_reg(2, self.heap);
                        self.heap = end;
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
    memory.set_word(CODE + 4 * end, swi(DONE)?)
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
            memory.set_word(HIGH_BASE + offset, swi(VECTOR + n)?)?;
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
    let base = SCRATCH + BASE + 4 * small(rng, BASES as usize);
    if class >= 2 {
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
    let mut scratch: Vec<u32> = (0..SCRATCH_WORDS).map(|_| rng.next_u32()).collect();
    let packed_load = matches!(
        a32::decode(word, BIT32.isa).map(|i| i.op),
        Some(Op::FpaTransfer {
            op: FpaTransferOp::Ldf,
            format: Format::Packed,
            ..
        })
    );
    if packed_load && rn < 15 {
        let (at, _) = reach(word, base);
        let expanded = fpa.fpsr & fpa::fpsr::EP != 0;
        let words = packed_value(rng, expanded);
        let slots = at
            .checked_sub(SCRATCH)
            .map(|offset| offset as usize / 4)
            .and_then(|n| scratch.get_mut(n..n + words.len()));
        if let (Some(slots), false) = (slots, words.is_empty()) {
            slots.copy_from_slice(&words);
        }
    }
    Some(Case {
        word,
        mode,
        state,
        fpa,
        scratch,
    })
}

/// A packed decimal value for a load to read, in the format the FPSR names:
/// mostly a number the data sheet defines, now and then an infinity or a NaN,
/// and otherwise none, leaving the drawn words and their digits above nine.
fn packed_value(rng: &mut Prng, expanded: bool) -> Vec<u32> {
    fn words<const W: usize>(p: Packed<W>) -> Vec<u32> {
        p.to_words().map_or_else(Vec::new, |words| words.to_vec())
    }
    let negative = chance(rng, 2);
    if chance(rng, 4) {
        return Vec::new();
    }
    let (stored, digits, exponent_digits): (u32, u32, u32) =
        if expanded { (21, 24, 7) } else { (17, 19, 4) };
    let special = chance(rng, 6);
    if special {
        let marker = if chance(rng, 2) { 8u128 } else { 0 };
        let payload = (0..digits - 1).fold(0u128, |n, _| n << 4 | u128::from(small(rng, 10)));
        let digit = if chance(rng, 3) { 0 } else { payload };
        let packed = |exponent| {
            let digits = marker << (4 * (digits - 1)) | digit;
            (exponent, digits)
        };
        let (exponent, digits) = packed((1u32 << (4 * exponent_digits)) - 1);
        return if expanded {
            words(Packed::<4> {
                negative,
                exponent_negative: chance(rng, 2),
                exponent,
                digits,
            })
        } else {
            words(Packed::<3> {
                negative,
                exponent_negative: chance(rng, 2),
                exponent,
                digits,
            })
        };
    }
    let count = if chance(rng, 4) { digits } else { stored };
    let significand = (1..count).fold(1 + u128::from(small(rng, 9)), |n, _| {
        n * 10 + u128::from(small(rng, 10))
    });
    // Mostly within the extended range, now and then past it.
    let reach = if chance(rng, 8) { 9_999 } else { 4_951 };
    let place =
        i32::try_from(small(rng, 2 * reach + 1)).unwrap_or(0) - i32::try_from(reach).unwrap_or(0);
    let decimal = Decimal {
        negative,
        significand,
        exponent: place - i32::try_from(count - 1).unwrap_or(0),
    };
    if expanded {
        Packed::<4>::of(decimal).map_or_else(Vec::new, words)
    } else {
        Packed::<3>::of(decimal).map_or_else(Vec::new, words)
    }
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

/// How a case's two runs compared.
enum Compared {
    /// The same in every respect: the vector.
    Same(Box<Vector>),
    /// Different, but where FPEmulator is no stricter reference.
    Accepted(Accepted),
    /// Different.
    Differs(String),
}

/// Runs `case` natively, its FPSR's traps all disabled: the FPA's state after.
fn untrapped(native: &mut Native, case: &Case, before: &FpaState) -> Result<FpaState, String> {
    fill(&mut native.memory, case)?;
    place(&mut native.memory, &[case.word])?;
    native
        .cpu
        .set_state(&case.state)
        .map_err(|e| format!("riscos-fpa: {e:?}"))?;
    let mut free = *before;
    free.fpsr &= !fpa::fpsr::ENABLES;
    free.set(native.cpu.fpa_mut());
    let mut bus = Recording {
        memory: &mut native.memory,
        transfers: Vec::new(),
    };
    native.cpu.run(&mut bus, 1);
    Ok(FpaState::of(native.cpu.fpa()))
}

/// Runs a data operation of `case` under FPEmulator at extended precision,
/// its FPSR's traps all disabled: the FPA's state after; `None` for any other
/// instruction.
fn extended(
    fpe: &mut Fpe,
    case: &Case,
    instruction: &Instruction,
) -> Result<Option<FpaState>, String> {
    let op = match instruction.op {
        Op::FpaMonadic {
            op,
            rounding,
            fd,
            fm,
            ..
        } => Op::FpaMonadic {
            op,
            precision: Precision::Extended,
            rounding,
            fd,
            fm,
        },
        Op::FpaDyadic {
            op,
            rounding,
            fd,
            fn_,
            fm,
            ..
        } => Op::FpaDyadic {
            op,
            precision: Precision::Extended,
            rounding,
            fd,
            fn_,
            fm,
        },
        _ => return Ok(None),
    };
    let word = a32::encode(&Instruction {
        cond: instruction.cond,
        op,
    })
    .map_err(|e| format!("riscos-fpa: {op:?}: {e:?}"))?;
    let mut free = case.fpa;
    free.fpsr &= !fpa::fpsr::ENABLES;
    fpe.prepare(&free)?;
    fill(&mut fpe.memory, case)?;
    fpe.run(&[word], &case.state)?;
    fpe.read_back().map(Some)
}

/// The windows' index of the word at `address`, if they watch it.
fn window_index(address: u32) -> Option<usize> {
    let word = |base: u32| address.checked_sub(base).map(|offset| offset as usize / 4);
    if let Some(n) = word(SCRATCH).filter(|&n| n < SCRATCH_WORDS as usize) {
        return Some(n);
    }
    let n = word(CODE_WINDOW).filter(|&n| n < CODE_WORDS as usize)?;
    let skipped = usize::from(address >= CODE + 8) * 2;
    (!(CODE..CODE + 8).contains(&address)).then(|| SCRATCH_WORDS as usize + n - skipped)
}

/// What a native run of a case left.
struct NativeRun {
    ran: Ran,
    state: State,
    fpa: FpaState,
    windows: Vec<u32>,
    transfers: Vec<Transfer>,
}

/// What FPEmulator's run of a case left.
struct FpeRun {
    ran: Ran,
    state: State,
    dump: Vec<u32>,
    fpa: FpaState,
    windows: Vec<u32>,
    /// The words at the native run's transfers' addresses, before the read
    /// back's code may overwrite one.
    moved: Vec<u32>,
}

impl Ran {
    const fn trap(self) -> Option<Exception> {
        match self {
            Self::Trapped(exception) => Some(exception),
            Self::Done | Self::Undefined => None,
        }
    }
}

/// Runs `case` on the processor from the FPA state `before`.
fn run_native(native: &mut Native, case: &Case, before: &FpaState) -> Result<NativeRun, String> {
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
    let state = native.cpu.state();
    let ran = match stop {
        Stop::FloatingPoint { exception } => Ran::Trapped(exception),
        Stop::Budget if state.cpsr & psr::M == Mode::Undefined.bits() => Ran::Undefined,
        Stop::Budget => Ran::Done,
        stop => return Err(format!("riscos-fpa: &{:08X} stopped {stop:?}", case.word)),
    };
    Ok(NativeRun {
        ran,
        state,
        fpa: FpaState::of(native.cpu.fpa()),
        windows: windows(&mut native.memory),
        transfers,
    })
}

/// Runs `case` under FPEmulator, its state already prepared, reading memory
/// back where the native run's `transfers` went.
fn run_fpe(fpe: &mut Fpe, case: &Case, transfers: &[Transfer]) -> Result<FpeRun, String> {
    fill(&mut fpe.memory, case)?;
    for n in 0..17 {
        fpe.memory.set_word(DUMP + 4 * n, 0xD0D0_D0D0)?;
    }
    let outcome = fpe.run(&[case.word], &case.state)?;
    let state = fpe.cpu.state();
    let dump = fpe.memory.words(DUMP, 17);
    let windows = windows(&mut fpe.memory);
    let moved = transfers
        .iter()
        .map(|t| fpe.memory.word(t.address))
        .collect();
    let fpa = fpe.read_back()?;
    let ran = match outcome {
        Outcome::Done => Ran::Done,
        Outcome::Unclaimed => Ran::Undefined,
        Outcome::Raised(number) => Ran::Trapped(
            exception_of(number).ok_or_else(|| format!("riscos-fpa: error &{number:08X}"))?,
        ),
    };
    Ok(FpeRun {
        ran,
        state,
        dump,
        fpa,
        windows,
        moved,
    })
}

/// How the two runs differed, and whether everything but the FPA's state
/// and the words the instruction moved agreed.
fn differences(theirs: &FpeRun, ours: &NativeRun) -> (Vec<String>, bool) {
    let mut differences = Vec::new();
    if theirs.ran != ours.ran {
        differences.push(format!(
            "outcome: FPEmulator {:?}, native {:?}",
            theirs.ran, ours.ran
        ));
    }
    if theirs.fpa != ours.fpa {
        differences.push(format!(
            "FPA: FPEmulator {:08X?}, native {:08X?}",
            theirs.fpa, ours.fpa
        ));
    }
    let moved: Vec<usize> = ours
        .transfers
        .iter()
        .filter_map(|t| window_index(t.address))
        .collect();
    let mut rest_same = true;
    let (fpe, native) = (&theirs.windows, &ours.windows);
    if let Some(n) = (0..fpe.len()).find(|&n| fpe[n] != native[n]) {
        differences.push(format!(
            "memory word {n} of the windows: FPEmulator &{:08X}, native &{:08X}",
            fpe[n], native[n]
        ));
        rest_same &= (0..fpe.len()).all(|n| fpe[n] == native[n] || moved.contains(&n));
    }
    let after = &ours.state;
    match ours.ran {
        // The harness's end SWI is one instruction further on.
        Ran::Done
            if visible(&theirs.state) != visible(after) || theirs.state.pc != after.pc + 4 =>
        {
            rest_same = false;
            differences.push(format!(
                "registers: FPEmulator {:08X?}, native {:08X?}",
                visible(&theirs.state),
                visible(after)
            ));
        }
        Ran::Trapped(_) => {
            let mut expected = after.user.to_vec();
            expected.push(after.pc);
            let dump = &theirs.dump;
            if dump[..16] != expected[..] || dump[16] & 0xF000_001F != after.cpsr & 0xF000_001F {
                rest_same = false;
                differences.push(format!(
                    "exception dump: FPEmulator {dump:08X?}, native {expected:08X?} &{:08X}",
                    after.cpsr
                ));
            }
        }
        _ => {}
    }
    (differences, rest_same)
}

/// Runs `case` both ways.
fn compare(fpe: &mut Fpe, native: &mut Native, case: &Case) -> Result<Compared, String> {
    fpe.prepare(&case.fpa)?;
    let before = fpe.read_back()?;
    let ours = run_native(native, case, &before)?;
    let theirs = run_fpe(fpe, case, &ours.transfers)?;
    let (differences, rest_same) = differences(&theirs, &ours);
    if differences.is_empty() {
        return Ok(Compared::Same(Box::new(Vector {
            word: case.word,
            before: case.state,
            after: ours.state,
            psr: u32::MAX,
            transfers: ours.transfers,
            fpa: Some(FpaCase {
                before,
                after: ours.fpa,
                trap: ours.ran.trap(),
            }),
        })));
    }
    let Some(instruction) = a32::decode(case.word, BIT32.isa) else {
        return Ok(Compared::Differs(differences.join("; ")));
    };
    let difference = Difference {
        op: &instruction.op,
        before: &before,
        fpe: Left {
            trap: theirs.ran.trap(),
            fpa: theirs.fpa,
            moved: theirs.moved,
        },
        native: Left {
            trap: ours.ran.trap(),
            fpa: ours.fpa,
            moved: ours.transfers.iter().map(|t| t.data).collect(),
        },
        rest_same,
    };
    let accepted = accuracy::accepted(
        &difference,
        || untrapped(native, case, &before).ok(),
        || extended(fpe, case, &instruction).ok().flatten(),
    );
    Ok(match accepted {
        Some(accepted) => Compared::Accepted(accepted),
        None => Compared::Differs(differences.join("; ")),
    })
}

/// The version number a module's help string gives after its title.
fn version<'a>(help: &'a str, title: &str) -> Option<&'a str> {
    let number = help.strip_prefix(title)?.split_whitespace().next()?;
    (number.bytes().any(|b| b.is_ascii_digit())
        && number.bytes().all(|b| b.is_ascii_digit() || b == b'.'))
    .then_some(number)
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
    if raw
        .get(4..8)
        .is_some_and(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]) & 0x8000_0000 != 0)
    {
        return Err(format!(
            "riscos-fpa: {path} is squeezed: give the harness the module as RISC OS holds it in memory"
        ));
    }
    let (title, help) = identity(&raw)?;
    if title != "FPEmulator" {
        return Err(format!("riscos-fpa: {path} is {title}, not FPEmulator"));
    }
    let version = version(&help, &title)
        .ok_or("riscos-fpa: the module's help string gives no version number")?;
    noteln!("xtask: [riscos-fpa] {help}");
    let mut fpe = Fpe::new(&raw)?;
    let mut native = Native::new()?;
    let (tallies, skipped) = tally(&mut fpe, &mut native, cases)?;
    let mut table = String::new();
    let mut unheld = [0; Unheld::ALL.len()];
    for (tally, &(name, _)) in tallies.iter().zip(&CLASSES) {
        let not_held: usize = tally.unheld.iter().sum();
        noteln!(
            "xtask: [riscos-fpa] {name}: {} cases, {} differ, {} within FPEmulator's error, \
             {not_held} not held, {} trapped, {} undefined",
            tally.ran,
            tally.failed,
            tally.accurate,
            tally.trapped,
            tally.undefined
        );
        let _ = writeln!(
            table,
            "| {name} | {} | {} | {not_held} | {} | {} | {} |",
            tally.ran,
            tally.accurate,
            tally.trapped,
            tally.undefined,
            tally.sample.len()
        );
        for (total, count) in unheld.iter_mut().zip(tally.unheld) {
            *total += count;
        }
    }
    let mut not_held = String::new();
    for (why, count) in Unheld::ALL.iter().zip(unheld) {
        let _ = writeln!(not_held, "- {}: {count}", why.reason());
    }
    let failures: usize = tallies.iter().map(|tally| tally.failed).sum();
    if failures != 0 {
        return Err(format!(
            "riscos-fpa: {failures} cases differ from FPEmulator"
        ));
    }
    if write {
        let record = Record {
            version,
            digest: tairix_util::hex::encode(&tairix_crypto::sha256(&raw)),
            cases,
            skipped,
            table: &table,
            not_held: &not_held,
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
    /// Within FPEmulator's error.
    accurate: usize,
    /// Not held to FPEmulator, by [`Unheld::ALL`]'s order.
    unheld: [usize; Unheld::ALL.len()],
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
            Compared::Same(vector) => {
                if matches!(vector.fpa, Some(FpaCase { trap: Some(_), .. })) {
                    tally.trapped += 1;
                } else if vector.after.cpsr & psr::M == Mode::Undefined.bits() {
                    tally.undefined += 1;
                }
                if tally.sample.len() < CLASSES[class].1 {
                    tally.sample.push(*vector);
                }
            }
            Compared::Accepted(Accepted::Accurate) => tally.accurate += 1,
            Compared::Accepted(Accepted::Unheld(why)) => {
                let n = Unheld::ALL.iter().position(|&w| w == why).unwrap_or(0);
                tally.unheld[n] += 1;
            }
            Compared::Differs(difference) => {
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
    version: &'a str,
    digest: String,
    cases: usize,
    skipped: usize,
    table: &'a str,
    not_held: &'a str,
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
         Source: the answers of FPEmulator {version}, run as an opaque program\n\
         on the emulator's own processor. The module file's\n\
         SHA-256:\n\n    {digest}\n\n\
         Each vector is an FPA instruction run natively by the processor: the\n\
         registers and memory the harness drew for it, the FPA state FPEmulator\n\
         read back once the drawn one was loaded, and what the processor left.\n\
         It is kept only where FPEmulator, given the same, left the same in\n\
         every register, every word of memory, the FPA's registers and FPSR,\n\
         and a trap's exception dump. Those answers are all it takes from\n\
         FPEmulator: no byte of the module is carried here.\n\n\
         The FPA rounds a transcendental value and a packed decimal conversion\n\
         correctly, which FPEmulator does not: such a case agrees, but is not\n\
         kept, where the two lie within FPEmulator's error. A case reading\n\
         what FPEmulator mishandles or the data sheet leaves undefined is not\n\
         held to FPEmulator; those that differed:\n\n{not_held}\n\
         `{SAMPLE}` is the `tairix_riscos_arm::vectors` encoding of the first\n\
         agreeing cases of each class, drawn from seed `{SEED:#X}` over {cases}\n\
         cases, {skipped} of them skipped as PC-relative stores over the\n\
         instruction. Every other case agreed when the sample was written.\n\n\
         | Class | Cases | Within FPEmulator's error | Not held | Trapped | Undefined | Sampled |\n\
         |---|---|---|---|---|---|---|\n\
         {table}",
        version = record.version,
        digest = record.digest,
        cases = record.cases,
        skipped = record.skipped,
        table = record.table,
        not_held = record.not_held,
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

#[cfg(test)]
mod tests {
    use super::*;
    use tairix_riscos_arm::instruction::{Immediate, MoveOp, Operand};

    #[test]
    fn a_module_s_version_is_the_number_after_its_title() {
        let version = |help| version(help, "FPEmulator");
        assert_eq!(
            version("FPEmulator 4.39 (30 Mar 2024) (1.13M)"),
            Some("4.39")
        );
        assert_eq!(version("FPEmulator   4.10 (01 Jan 1999)"), Some("4.10"));
        assert_eq!(version("FPEmulator (30 Mar 2024)"), None);
        assert_eq!(version("FPEmulator ."), None);
        assert_eq!(version("Other 4.39"), None);
    }

    #[test]
    fn a_claim_past_the_guest_s_heap_is_refused() {
        let ops = [
            Op::Move {
                op: MoveOp::Mvn,
                s: false,
                rd: Reg::field(3),
                operand: Operand::Immediate(Immediate {
                    value: 0,
                    rotation: 0,
                }),
            },
            Op::Move {
                op: MoveOp::Mov,
                s: false,
                rd: Reg::field(0),
                operand: Operand::Immediate(Immediate {
                    value: 6,
                    rotation: 0,
                }),
            },
            Op::Call {
                op: CallOp::Swi,
                value: OS_MODULE,
            },
        ];
        let mut module = vec![0; 0x20];
        module[4..8].copy_from_slice(&0x20u32.to_le_bytes());
        for op in ops {
            module.extend(encoded(op).expect("an encodable instruction").to_le_bytes());
        }
        let refused = Fpe::new(&module).err().expect("a refused claim");
        assert!(
            refused.contains("claimed more than the guest's heap"),
            "{refused}"
        );
    }
}
