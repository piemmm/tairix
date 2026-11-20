//! What the kernel's tests share: a host whose clock and input they set, a
//! session with an application slot, guest code put in it, and ARM code
//! built with the processor's encoder.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use tairix_riscos_arm::cpu::State;
use tairix_riscos_arm::psr::{self, Mode};

use crate::input::InputEvent;
use crate::memory::layout::{Layout, APPLICATION};
use crate::memory::policy::Ceiling;
use crate::memory::Bits;
use crate::os::{len32, Config, Host, Kernel, Outcome, Pulse, Regs};

/// Input events and the times they arrive.
type Arrivals = Rc<RefCell<VecDeque<(u64, InputEvent)>>>;

/// A host whose clock the test sets, whose input arrives when the test
/// says, and which stops a run once told to.
struct TestHost {
    clock: Rc<Cell<u64>>,
    stop_after: Rc<Cell<Option<u32>>>,
    /// A wait with no input left to come stops the run, whatever timers are
    /// armed: a test that types a script ends when the script does.
    stop_without_input: Rc<Cell<bool>>,
    input: Arrivals,
    /// The real time at the monotonic clock's nought.
    epoch: u64,
}

/// 1 January 2000, 00:00:00 UTC, in centiseconds since 1900.
pub(crate) const MILLENNIUM: u64 = 3_155_673_600 * 100;

impl Host for TestHost {
    fn monotonic(&mut self) -> u64 {
        self.clock.get()
    }

    fn real_time(&mut self) -> u64 {
        self.epoch + self.clock.get()
    }

    fn pulse(&mut self) -> Pulse {
        match self.stop_after.get() {
            Some(0) => Pulse::Stop,
            Some(n) => {
                self.stop_after.set(Some(n - 1));
                Pulse::Continue
            }
            None => Pulse::Continue,
        }
    }

    fn input(&mut self) -> Option<InputEvent> {
        let mut input = self.input.borrow_mut();
        let &(at, _) = input.front()?;
        if at > self.clock.get() {
            return None;
        }
        input.pop_front().map(|(_, event)| event)
    }

    fn relay(&mut self, _request: &[u8], _reply: &mut alloc::vec::Vec<u8>) -> bool {
        false
    }

    fn wait(&mut self, until: Option<u64>) -> Pulse {
        let arrival = self.input.borrow().front().map(|&(at, _)| at);
        if arrival.is_none() && self.stop_without_input.get() {
            return Pulse::Stop;
        }
        let next = match (until, arrival) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) | (None, Some(a)) => a,
            (None, None) => return Pulse::Stop,
        };
        self.clock.set(self.clock.get().max(next));
        self.pulse()
    }
}

/// A booted kernel and the handles its tests drive it by.
pub(crate) struct Session {
    pub(crate) kernel: Kernel,
    pub(crate) clock: Rc<Cell<u64>>,
    pub(crate) stop_after: Rc<Cell<Option<u32>>>,
    pub(crate) stop_without_input: Rc<Cell<bool>>,
    input: Arrivals,
}

/// Where [`Session::user_swi`] puts the SWI it runs.
pub(crate) const SWI_AT: u32 = APPLICATION;
/// Where a test's own code may go.
pub(crate) const CODE: u32 = APPLICATION + 0x100;
/// Where a test's own data may go.
pub(crate) const DATA: u32 = APPLICATION + 0x4000;
/// The instructions a run is given: enough for interrupts and callbacks to
/// be done with and the context to settle in its spin.
const SPIN: u64 = 1000;
/// Where [`Session::capture_output`] puts its `WrchV` claimant.
const CAPTURE_CODE: u32 = APPLICATION + 0x2000;
/// Where the output it captures goes: the next free byte's address, then
/// the bytes.
const CAPTURED: u32 = APPLICATION + 0x8000;

/// A session on `layout` with 256 MiB of guest RAM.
pub(crate) fn boot(layout: &'static Layout) -> Session {
    boot_in(layout, tairix_tz::Zone::UTC, 0)
}

/// A session on `layout` in `zone`, its real time `seconds` past 1 January
/// 2000.
pub(crate) fn boot_in(
    layout: &'static Layout,
    zone: tairix_tz::Zone<'static>,
    seconds: u64,
) -> Session {
    boot_with(layout, zone, seconds, &[])
}

/// A session as [`boot_in`] makes one, with the built-in `modules` beside the
/// kernel's own.
pub(crate) fn boot_with(
    layout: &'static Layout,
    zone: tairix_tz::Zone<'static>,
    seconds: u64,
    modules: &'static [&'static crate::module::Builtin],
) -> Session {
    let clock = Rc::new(Cell::new(0));
    let stop_after = Rc::new(Cell::new(None));
    let stop_without_input = Rc::new(Cell::new(false));
    let input = Rc::new(RefCell::new(VecDeque::new()));
    let host = TestHost {
        clock: Rc::clone(&clock),
        stop_after: Rc::clone(&stop_after),
        stop_without_input: Rc::clone(&stop_without_input),
        input: Rc::clone(&input),
        epoch: MILLENNIUM + 100 * seconds,
    };
    let config = Config {
        layout,
        ceiling: Ceiling::derive(256 * 1024 * 1024),
        memo_pages: 16,
        host: Box::new(host),
        vdu: None,
        modules,
        zone,
        pressure: &tairix_reclaim::pressure::Unpressured,
        log: &tairix_log::DiscardSink,
    };
    let kernel = match Kernel::new(config) {
        Ok(kernel) => kernel,
        Err(error) => panic!("boot: {error:?}"),
    };
    Session {
        kernel,
        clock,
        stop_after,
        stop_without_input,
        input,
    }
}

/// A session on `layout` with a 64 KiB application slot of `bits` current.
pub(crate) fn with_application(layout: &'static Layout, bits: Bits) -> Session {
    let mut session = boot(layout);
    session.application(bits, 0x1_0000);
    session
}

impl Session {
    /// Makes a slot of `bits` with `bytes` of memory current, the memory
    /// limit its top, and the processor of `bits` in User mode at its base.
    pub(crate) fn application(&mut self, bits: Bits, bytes: u32) {
        let memory = self.kernel.memory();
        let slot = memory.create_slot(bits).expect("a slot");
        memory.set_slot_size(slot, bytes).expect("its memory");
        memory.switch_slot(slot).expect("made current");
        self.kernel
            .os()
            .world
            .env
            .set_application_top(APPLICATION + bytes);
        self.enter(bits, SWI_AT, [0; 13]);
    }

    /// The processor of `bits` in User mode at `pc` with `registers`, its
    /// stack at the application's top and the privileged stacks empty.
    pub(crate) fn enter(&mut self, bits: Bits, pc: u32, registers: [u32; 13]) {
        let mut os = self.kernel.os();
        let top = os.world.env.slot(crate::env::MEMORY_LIMIT).address;
        let mut state = os.emptied_stacks(State::default());
        state.user[..13].copy_from_slice(&registers);
        state.user[13] = top;
        state.cpsr = match bits {
            Bits::Bit26 => Mode::User26.bits(),
            Bits::Bit32 => Mode::User.bits(),
        };
        state.pc = pc;
        os.world.processor.load(bits, &state).expect("a user state");
    }

    /// Writes `words` at `at`.
    pub(crate) fn poke(&mut self, at: u32, words: &[u32]) {
        let mut os = self.kernel.os();
        for (i, &word) in (0..).zip(words) {
            os.write_word(at + 4 * i, word).expect("writable");
        }
    }

    /// The word at `at`.
    pub(crate) fn peek(&mut self, at: u32) -> u32 {
        self.kernel.os().read_word(at).expect("readable")
    }

    /// Writes `bytes` at `at`.
    pub(crate) fn write(&mut self, at: u32, bytes: &[u8]) {
        self.kernel.os().write_bytes(at, bytes).expect("writable");
    }

    /// The `length` bytes at `at`.
    pub(crate) fn read(&mut self, at: u32, length: usize) -> Vec<u8> {
        let mut bytes = alloc::vec![0; length];
        self.kernel
            .os()
            .read_bytes(at, &mut bytes)
            .expect("readable");
        bytes
    }

    /// The text of the error block at `block`.
    pub(crate) fn error_text(&mut self, block: u32) -> Vec<u8> {
        let (text, _) = self
            .kernel
            .os()
            .read_until(block + 4, 252, |b| b == 0)
            .expect("an error block");
        text
    }

    /// Runs `SWI number` from User mode with `registers` to the spin after
    /// it: R0 to R12 and the flags it left, and where the processor is.
    pub(crate) fn user_swi(&mut self, number: u32, registers: [u32; 13]) -> (Regs, State) {
        let outcome = self.start_swi(number, registers);
        assert_eq!(outcome, Outcome::Budget, "SWI &{number:X} ran to its end");
        let state = self.kernel.state();
        assert_eq!(state.pc, SWI_AT + 4, "SWI &{number:X} returned past itself");
        (self.regs(), state)
    }

    /// Starts `SWI number` from User mode with `registers`, a spin after it:
    /// how the run ended.
    pub(crate) fn start_swi(&mut self, number: u32, registers: [u32; 13]) -> Outcome {
        self.poke(SWI_AT, &[asm::swi(number), asm::b(SWI_AT + 4, SWI_AT + 4)]);
        let bits = self.kernel.bits();
        self.enter(bits, SWI_AT, registers);
        self.resume()
    }

    /// Runs the context on, a SWI it waited in performed again, until it
    /// waits, ends or spins.
    pub(crate) fn resume(&mut self) -> Outcome {
        self.kernel.run(SPIN)
    }

    /// R0 to R12 and the flags as the current mode sees them.
    pub(crate) fn regs(&mut self) -> Regs {
        let state = self.kernel.state();
        let mut regs = Regs::new([0; 13]);
        regs.r.copy_from_slice(&state.user[..13]);
        regs.flags = state.cpsr & psr::NZCV;
        regs
    }

    /// Runs the current context for at most `budget` instructions.
    pub(crate) fn run(&mut self, budget: u64) -> Outcome {
        self.kernel.run(budget)
    }

    /// Claims `WrchV` with code that keeps every character written.
    pub(crate) fn capture_output(&mut self) {
        self.poke(
            CAPTURE_CODE,
            &[
                asm::ldr(11, 12, 0),
                asm::strb_post(0, 11, 1),
                asm::str(11, 12, 0),
                asm::claim(),
            ],
        );
        self.poke(CAPTURED, &[CAPTURED + 4]);
        let (regs, _) = self.user_swi(
            0x2_001F,
            [3, CAPTURE_CODE, CAPTURED, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
        );
        assert!(!regs.flag(psr::V), "WrchV claimed");
    }

    /// What has been written since [`Session::capture_output`], cleared.
    pub(crate) fn output(&mut self) -> Vec<u8> {
        let end = self.peek(CAPTURED);
        let bytes = self.read(CAPTURED + 4, (end - CAPTURED - 4) as usize);
        self.poke(CAPTURED, &[CAPTURED + 4]);
        bytes
    }

    /// Queues `event` to arrive at `at`.
    pub(crate) fn arrive(&mut self, at: u64, event: InputEvent) {
        self.input.borrow_mut().push_back((at, event));
    }

    /// A filing system of one file holding `bytes`, which every name opens:
    /// claimants of `FileV`, `ArgsV`, `BGetV`, `BPutV` and `FindV`, each
    /// keeping what it was asked in the state at [`OneFile::state`].
    pub(crate) fn one_file(&mut self, bytes: &[u8]) -> OneFile {
        let file = OneFile {
            state: ONE_FILE_STATE,
        };
        self.poke(ONE_FILE_STATE, &[ONE_FILE_DATA, len32(bytes.len()), 0, 0]);
        self.write(ONE_FILE_DATA, bytes);
        file.describe(self, 1, (0xFFFF_FD00, 0), 0xFFD);
        let mut at = ONE_FILE_CODE;
        let builders: [(u32, Claimant); 5] = [
            (0x0D, one_file::find),
            (0x0A, one_file::bget),
            (0x0B, one_file::bput),
            (0x09, one_file::args),
            (0x08, one_file::file),
        ];
        for (vector, build) in builders {
            let code = build(at);
            self.poke(at, &code);
            let (claimed, _) = self.user_swi(
                0x2_001F,
                [vector, at, ONE_FILE_STATE, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            );
            assert!(!claimed.flag(psr::V), "vector &{vector:X} claimed");
            at += 4 * len32(code.len());
        }
        file
    }
}

/// A claimant's code, built for the address it is put at.
type Claimant = fn(u32) -> Vec<u32>;

/// [`Session::one_file`]'s claimants, each built for the address it is put
/// at, R12 its state.
mod one_file {
    use alloc::vec::Vec;

    use tairix_riscos_arm::psr;

    use super::asm::{
        self,
        Condition::{Cc, Cs, Eq, Ne},
    };
    use super::OneFile;

    /// R10 and R11, which the claimants work in.
    const SCRATCH: u16 = 0b1100_0000_0000;

    /// `FindV`: a close marks the file closed; an open keeps its reason and
    /// answers handle 77, emptying the file for output.
    pub(super) fn find(at: u32) -> Vec<u32> {
        alloc::vec![
            asm::cmp_imm(0, 0),
            asm::when(Ne, asm::b(at + 4, at + 16)),
            asm::str(0, 12, OneFile::HANDLE),
            asm::claim(),
            asm::str(0, 12, OneFile::OPENED),
            asm::cmp_imm(0, 0x8C),
            asm::mov(0, 0),
            asm::when(Eq, asm::str(0, 12, OneFile::EXTENT)),
            asm::str(0, 12, OneFile::POINTER),
            asm::mov(0, OneFile::OPEN),
            asm::str(0, 12, OneFile::HANDLE),
            asm::claim(),
        ]
    }

    /// `BGetV`: the byte at the pointer, C set at the end.
    pub(super) fn bget(at: u32) -> Vec<u32> {
        let end = at + 4 * 12;
        alloc::vec![
            asm::push(SCRATCH),
            asm::ldr(10, 12, OneFile::POINTER),
            asm::ldr(11, 12, OneFile::EXTENT),
            asm::cmp(10, 11),
            asm::when(Cs, asm::b(at + 16, end)),
            asm::ldr(11, 12, OneFile::BASE),
            asm::add_reg(11, 11, 10),
            asm::ldrb_post(0, 11, 0),
            asm::add(10, 10, 1),
            asm::str(10, 12, OneFile::POINTER),
            asm::pull(SCRATCH),
            asm::claim(),
            asm::pull(SCRATCH),
            asm::set_flags(psr::C),
            asm::claim(),
        ]
    }

    /// `BPutV`: the byte stored at the pointer, the file growing past its end.
    pub(super) fn bput(_at: u32) -> Vec<u32> {
        alloc::vec![
            asm::push(SCRATCH),
            asm::ldr(10, 12, OneFile::POINTER),
            asm::ldr(11, 12, OneFile::BASE),
            asm::add_reg(11, 11, 10),
            asm::strb_post(0, 11, 0),
            asm::add(10, 10, 1),
            asm::str(10, 12, OneFile::POINTER),
            asm::ldr(11, 12, OneFile::EXTENT),
            asm::cmp(11, 10),
            asm::when(Cc, asm::str(10, 12, OneFile::EXTENT)),
            asm::pull(SCRATCH),
            asm::claim(),
        ]
    }

    /// `ArgsV`: the pointer written and read, the extent read.
    pub(super) fn args(_at: u32) -> Vec<u32> {
        alloc::vec![
            asm::cmp_imm(0, 1),
            asm::when(Eq, asm::str(2, 12, OneFile::POINTER)),
            asm::cmp_imm(0, 2),
            asm::when(Eq, asm::ldr(2, 12, OneFile::EXTENT)),
            asm::cmp_imm(0, 0),
            asm::when(Eq, asm::ldr(2, 12, OneFile::POINTER)),
            asm::claim(),
        ]
    }

    /// `FileV`: R0 to R6 kept; 19 gives the error, 20 the object and 6 its
    /// type.
    pub(super) fn file(at: u32) -> Vec<u32> {
        let mut code: Vec<u32> = (0u16..7)
            .map(|n| asm::str(u32::from(n), 12, OneFile::FILE_CALL + 4 * n))
            .collect();
        let branch = at + 4 * 8;
        code.extend_from_slice(&[
            asm::cmp_imm(0, 19),
            asm::when(Ne, asm::b(branch, branch + 16)),
            asm::ldr(0, 12, OneFile::ERROR),
            asm::set_flags(psr::V),
            asm::claim(),
            asm::cmp_imm(0, 20),
            asm::when(Eq, asm::ldr(2, 12, OneFile::LOAD)),
            asm::when(Eq, asm::ldr(3, 12, OneFile::EXEC)),
            asm::when(Eq, asm::ldr(4, 12, OneFile::EXTENT)),
            asm::when(Eq, asm::ldr(6, 12, OneFile::TYPE)),
            asm::when(Eq, asm::ldr(0, 12, OneFile::OBJECT)),
            asm::when(Eq, asm::claim()),
            asm::cmp_imm(0, 6),
            asm::when(Eq, asm::ldr(0, 12, OneFile::OBJECT)),
            asm::claim(),
        ]);
        code
    }
}

/// Where [`Session::one_file`] puts its claimants, their state and the
/// file's bytes.
const ONE_FILE_CODE: u32 = APPLICATION + 0x2800;
const ONE_FILE_STATE: u32 = APPLICATION + 0x3800;
pub(crate) const ONE_FILE_DATA: u32 = APPLICATION + 0x6000;

/// The state of [`Session::one_file`]'s filing system.
pub(crate) struct OneFile {
    pub(crate) state: u32,
}

impl OneFile {
    /// Its state's words: the bytes' address, the extent, the pointer, the
    /// open handle, the last `OS_Find` reason, the last `OS_File` call's R0
    /// to R6, and what it says of the object: its type, load and execution
    /// addresses, file type, and the error `OS_File` 19 gives.
    pub(crate) const BASE: u16 = 0;
    pub(crate) const EXTENT: u16 = 4;
    pub(crate) const POINTER: u16 = 8;
    pub(crate) const HANDLE: u16 = 12;
    pub(crate) const OPENED: u16 = 16;
    pub(crate) const FILE_CALL: u16 = 20;
    pub(crate) const OBJECT: u16 = 48;
    pub(crate) const LOAD: u16 = 52;
    pub(crate) const EXEC: u16 = 56;
    pub(crate) const TYPE: u16 = 60;
    pub(crate) const ERROR: u16 = 64;
    /// The handle an open answers.
    pub(crate) const OPEN: u32 = 77;

    /// The word at `offset` in the state.
    pub(crate) fn word(&self, s: &mut Session, offset: u16) -> u32 {
        s.peek(self.state + u32::from(offset))
    }

    /// The file's bytes.
    pub(crate) fn bytes(&self, s: &mut Session) -> Vec<u8> {
        let extent = self.word(s, Self::EXTENT);
        s.read(ONE_FILE_DATA, extent as usize)
    }

    /// The last `OS_File` call's R0 to R6.
    pub(crate) fn file_call(&self, s: &mut Session) -> [u32; 7] {
        core::array::from_fn(|n| s.peek(self.state + u32::from(Self::FILE_CALL) + 4 * len32(n)))
    }

    /// What `OS_File` says of the object: its type, its load and execution
    /// addresses, and its file type.
    pub(crate) fn describe(
        &self,
        s: &mut Session,
        object: u32,
        (load, exec): (u32, u32),
        file_type: u32,
    ) {
        let at = self.state;
        s.poke(
            at + u32::from(Self::OBJECT),
            &[object, load, exec, file_type],
        );
    }
}

/// ARM instructions, each from the processor's encoder.
pub(crate) mod asm {
    use tairix_riscos_arm::a32;
    pub(crate) use tairix_riscos_arm::cond::Condition;
    use tairix_riscos_arm::instruction::{
        BlockMode, BlockOp, BranchOp, CallOp, CompareOp, DataOp, Immediate, Index, Instruction,
        MoveOp, Offset, Op, Operand, Shift, Source, TransferOp,
    };
    use tairix_riscos_arm::reg::{Reg, RegList};

    fn encode(op: Op) -> u32 {
        match a32::encode(&Instruction {
            cond: Condition::Al,
            op,
        }) {
            Ok(word) => word,
            Err(error) => panic!("unencodable {op:?}: {error:?}"),
        }
    }

    fn reg(n: u32) -> Reg {
        Reg::field(n)
    }

    fn immediate(value: u32) -> Operand {
        match Immediate::encode(value) {
            Some(imm) => Operand::Immediate(imm),
            None => panic!("{value:#X} is no immediate"),
        }
    }

    fn register(rm: u32) -> Operand {
        Operand::Register {
            rm: reg(rm),
            shift: Shift::NONE,
        }
    }

    /// `SWI number`.
    pub(crate) fn swi(number: u32) -> u32 {
        encode(Op::Call {
            op: CallOp::Swi,
            value: number,
        })
    }

    /// `MOV Rd, #value`.
    pub(crate) fn mov(rd: u32, value: u32) -> u32 {
        encode(Op::Move {
            op: MoveOp::Mov,
            s: false,
            rd: reg(rd),
            operand: immediate(value),
        })
    }

    /// `MOV Rd, Rm`.
    pub(crate) fn mov_reg(rd: u32, rm: u32) -> u32 {
        encode(Op::Move {
            op: MoveOp::Mov,
            s: false,
            rd: reg(rd),
            operand: register(rm),
        })
    }

    /// `MOV PC, R14`.
    pub(crate) fn mov_pc_lr() -> u32 {
        mov_reg(15, 14)
    }

    /// `ADD Rd, Rn, #value`.
    pub(crate) fn add(rd: u32, rn: u32, value: u32) -> u32 {
        encode(Op::Data {
            op: DataOp::Add,
            s: false,
            rd: reg(rd),
            rn: reg(rn),
            operand: immediate(value),
        })
    }

    /// `ADD Rd, Rn, Rm`.
    pub(crate) fn add_reg(rd: u32, rn: u32, rm: u32) -> u32 {
        encode(Op::Data {
            op: DataOp::Add,
            s: false,
            rd: reg(rd),
            rn: reg(rn),
            operand: register(rm),
        })
    }

    /// `CMP Rn, Rm`: the flags of `Rn - Rm`.
    pub(crate) fn cmp(rn: u32, rm: u32) -> u32 {
        encode(Op::Compare {
            op: CompareOp::Cmp,
            rd: reg(0),
            rn: reg(rn),
            operand: register(rm),
        })
    }

    /// `CMP Rn, #value`.
    pub(crate) fn cmp_imm(rn: u32, value: u32) -> u32 {
        encode(Op::Compare {
            op: CompareOp::Cmp,
            rd: reg(0),
            rn: reg(rn),
            operand: immediate(value),
        })
    }

    /// `SUBS Rd, Rn, #value`.
    pub(crate) fn subs(rd: u32, rn: u32, value: u32) -> u32 {
        encode(Op::Data {
            op: DataOp::Sub,
            s: true,
            rd: reg(rd),
            rn: reg(rn),
            operand: immediate(value),
        })
    }

    /// `ADR Rd, to` at `from`: `ADD Rd, PC, #offset`.
    pub(crate) fn adr(rd: u32, from: u32, to: u32) -> u32 {
        add(rd, 15, to - from - 8)
    }

    /// `MRS Rd, CPSR`.
    pub(crate) fn mrs(rd: u32) -> u32 {
        encode(Op::Status {
            spsr: false,
            rd: reg(rd),
        })
    }

    /// `MSR CPSR_f, #flags`: the condition flags made `flags`.
    pub(crate) fn set_flags(flags: u32) -> u32 {
        let Some(imm) = Immediate::encode(flags) else {
            panic!("{flags:#X} is no immediate");
        };
        encode(Op::StatusWrite {
            spsr: false,
            fields: 0b1000,
            source: Source::Immediate(imm),
        })
    }

    /// `word` made conditional on `cond`, the field every conditional A32
    /// instruction keeps in its top four bits.
    pub(crate) fn when(cond: Condition, word: u32) -> u32 {
        (word & 0x0FFF_FFFF) | (cond.bits() << 28)
    }

    /// `B` from `from` to `to`.
    pub(crate) fn b(from: u32, to: u32) -> u32 {
        encode(Op::Branch {
            op: BranchOp::B,
            offset: to.wrapping_sub(from.wrapping_add(8)).cast_signed(),
        })
    }

    /// `LDR Rt, [Rn, #offset]`.
    pub(crate) fn ldr(rt: u32, rn: u32, offset: u16) -> u32 {
        transfer(TransferOp::Ldr, rt, rn, offset, Index::Offset)
    }

    /// `STR Rt, [Rn, #offset]`.
    pub(crate) fn str(rt: u32, rn: u32, offset: u16) -> u32 {
        transfer(TransferOp::Str, rt, rn, offset, Index::Offset)
    }

    /// `STRB Rt, [Rn], #offset`.
    pub(crate) fn strb_post(rt: u32, rn: u32, offset: u16) -> u32 {
        transfer(TransferOp::Strb, rt, rn, offset, Index::PostIndexed)
    }

    /// `LDRB Rt, [Rn], #offset`.
    pub(crate) fn ldrb_post(rt: u32, rn: u32, offset: u16) -> u32 {
        transfer(TransferOp::Ldrb, rt, rn, offset, Index::PostIndexed)
    }

    /// `LDR Rt, [Rn], #offset`.
    pub(crate) fn ldr_post(rt: u32, rn: u32, offset: u16) -> u32 {
        transfer(TransferOp::Ldr, rt, rn, offset, Index::PostIndexed)
    }

    /// `STR Rt, [Rn], #offset`.
    pub(crate) fn str_post(rt: u32, rn: u32, offset: u16) -> u32 {
        transfer(TransferOp::Str, rt, rn, offset, Index::PostIndexed)
    }

    /// `LDR PC, [R13], #4`: a 32-bit vector claimant claiming the call.
    pub(crate) fn claim() -> u32 {
        transfer(TransferOp::Ldr, 15, 13, 4, Index::PostIndexed)
    }

    fn transfer(op: TransferOp, rt: u32, rn: u32, offset: u16, index: Index) -> u32 {
        encode(Op::Transfer {
            op,
            rt: reg(rt),
            rn: reg(rn),
            up: true,
            offset: Offset::Immediate(offset),
            index,
        })
    }

    /// `LDMFD R13!, {PC}`: a 26-bit vector claimant claiming the call.
    pub(crate) fn claim_26() -> u32 {
        encode(Op::Block {
            op: BlockOp::Ldm,
            rn: Reg::SP,
            mode: BlockMode::IncrementAfter,
            writeback: true,
            user: false,
            list: RegList(1 << 15),
        })
    }

    /// `STMFD R13!, {list}`.
    pub(crate) fn push(list: u16) -> u32 {
        encode(Op::Block {
            op: BlockOp::Stm,
            rn: Reg::SP,
            mode: BlockMode::DecrementBefore,
            writeback: true,
            user: false,
            list: RegList(list),
        })
    }

    /// `LDMFD R13!, {list}`.
    pub(crate) fn pull(list: u16) -> u32 {
        encode(Op::Block {
            op: BlockOp::Ldm,
            rn: Reg::SP,
            mode: BlockMode::IncrementAfter,
            writeback: true,
            user: false,
            list: RegList(list),
        })
    }
}
