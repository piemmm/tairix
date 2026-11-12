//! Programs whose behaviour is Rust: `COMMAND.COM` and the DOS utilities.
//! A program's file is a stub that jumps to the ROM, and everything it keeps
//! — its frames, a stack for each root, its own state — lies in its own
//! memory, so it is a process like any other: it reaches DOS and the BIOS
//! through their interrupt vectors, owns no memory but its block, and a
//! machine saved while it waits resumes it.
//!
//! A program's main routine is the root of its work and returns the code it
//! ends with. A handler it installs — the shell's INT 23h and 24h — is a
//! root nested on top, switched to a stack of the program's own. Every guest
//! call a routine makes is an interrupt taken from its root's stack that
//! returns to the ROM's landing pad, so the program's segment is SS whenever
//! it resumes.

mod dos;
mod table;
pub mod walk;

pub use dos::{answer_key, hand_back, say, say_console, say_error, Int, Key, Line, ENDED};
pub use table::Id;

use super::engine::Saved;
use super::rom::entry;
use crate::asm::{Asm, AsmError, Reg16};
use crate::bios::guest::Guest;
use crate::bios::layout::{entry as bios, SEGMENT as ROM};
use crate::cpu::Seg;
use crate::frames::{self, decode, encode, Context, Far, Locals, Outcome, FRAME};
use alloc::vec::Vec;

/// Where `reg` of a handler's caller lies in the frame its entry saved: the
/// ROM's save, its DS the program's own, then the caller's DS the stub
/// pushed before making it so, then the interrupt's return.
const fn slot(reg: Saved) -> u16 {
    let n = match reg {
        Saved::Ds => Saved::Ip as u16,
        Saved::Ip | Saved::Cs | Saved::Flags => reg as u16 + 1,
        _ => reg as u16,
    };
    n * 2
}

/// Frames a program holds.
pub const DEPTH: u8 = 16;
/// Roots that may run at once: its main routine and handlers nested on it.
pub const LEVELS: u8 = 3;

/// What function `6523h` answers, as a call returns it.
pub mod answer {
    use crate::dos::country::answer;
    pub const NO: u32 = answer::NO as u32;
    pub const YES: u32 = answer::YES as u32;
    pub const NEITHER: u32 = answer::NEITHER as u32;
}

/// Where things lie in a program's memory, from its PSP.
pub mod at {
    use super::{FRAME, LEVELS};

    /// Where the program starts.
    pub const START: u16 = crate::dos::memory::psp::LEN;
    /// The handlers' stubs, [`HANDLER_STRIDE`] bytes apart.
    pub const HANDLERS: u16 = 0x108;
    pub const HANDLER_STRIDE: u16 = 8;
    /// Handlers a program may have.
    pub const HANDLER_COUNT: u16 = 4;
    /// The program's number.
    pub const NUMBER: u16 = 0x130;
    /// Frames in use, and roots running.
    pub const DEPTH: u16 = 0x131;
    pub const ROOTS: u16 = 0x132;
    /// Set by a handler abandoning the main root's work for it to resume.
    pub const ABANDON: u16 = 0x133;
    /// A record a root: where its caller's registers lie, the ROM entry it
    /// returns through, the frame it is.
    pub const RECORDS: u16 = 0x134;
    pub const RECORD: u16 = 8;
    /// The registers each root's last DOS call returned, in
    /// [`super::Returned`] order.
    pub const RESULTS: u16 = RECORDS + LEVELS as u16 * RECORD;
    pub const RESULT: u16 = 16;
    /// The device's error, as DOS's extended error, that a critical-error
    /// handler of the program last failed a call for.
    pub const CRITICAL: u16 = RESULTS + LEVELS as u16 * RESULT;
    /// The frames.
    pub const FRAMES: u16 = 0x180;
    /// A stack a root.
    pub const STACKS: u16 = FRAMES + super::DEPTH as u16 * FRAME;
    pub const STACK: u16 = 0x200;
    /// Scratch a root: the text it writes, the numbers it spells.
    pub const SCRATCH: u16 = STACKS + LEVELS as u16 * STACK;
    pub const SCRATCH_LEN: u16 = 0x100;
    /// The program's own state.
    pub const STATE: u16 = SCRATCH + LEVELS as u16 * SCRATCH_LEN;

    const _: () = assert!(HANDLERS + HANDLER_COUNT * HANDLER_STRIDE <= NUMBER);
    const _: () = assert!(CRITICAL < FRAMES);
    const _: () = assert!(super::Returned::Ds as u16 * 2 + 2 <= RESULT);
}

/// The registers a DOS call returned, as [`Program::result`] reads them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[allow(missing_docs, reason = "the registers' own names")]
pub enum Returned {
    Ax,
    Bx,
    Cx,
    Dx,
    Si,
    Es,
    Flags,
    Ds,
}

/// A root's record's fields.
mod record {
    pub const CALLER: u16 = 0;
    pub const EXIT: u16 = 4;
    pub const ROOT: u16 = 6;
}

/// A program routine: a step the program host runs each time the routine
/// resumes.
pub trait Routine: Locals {
    /// Its number.
    const ID: Id;
    /// Runs on from `phase`, which it advances; `child` is the outcome of
    /// the routine it last called.
    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow;
}

/// What a program routine's step asks for next.
pub type Flow = frames::Flow<Id>;
/// A program's frame, decoded.
pub type Frame = frames::Frame<Id>;

impl Frame {
    /// A frame starting `routine`.
    pub fn of<R: Routine>(routine: &R) -> Self {
        Self::new(R::ID, routine)
    }
}

/// A program as its routines see it: its memory, the CPU, and the guest
/// calls it makes.
pub struct Program<'g, 'm> {
    /// The machine.
    pub g: &'g mut Guest<'m>,
    /// The program's segment: its PSP.
    pub own: u16,
}

impl Program<'_, '_> {
    /// A byte of the program's memory.
    pub fn get8(&mut self, offset: u16) -> u8 {
        self.g.read8(self.own, offset)
    }

    pub fn set8(&mut self, offset: u16, value: u8) {
        self.g.write8(self.own, offset, value);
    }

    /// A word of the program's memory.
    pub fn get16(&mut self, offset: u16) -> u16 {
        self.g.read16(self.own, offset)
    }

    pub fn set16(&mut self, offset: u16, value: u16) {
        self.g.write16(self.own, offset, value);
    }

    /// A doubleword of the program's memory.
    pub fn get32(&mut self, offset: u16) -> u32 {
        u32::from(self.get16(offset)) | u32::from(self.get16(offset.wrapping_add(2))) << 16
    }

    pub fn set32(&mut self, offset: u16, value: u32) {
        let [a, b, c, d] = value.to_le_bytes();
        self.set16(offset, u16::from_le_bytes([a, b]));
        self.set16(offset.wrapping_add(2), u16::from_le_bytes([c, d]));
    }

    /// The call giving back the memory past what the program keeps: DOS
    /// gives a program every free paragraph, so one that allocates gives
    /// back the rest first.
    pub fn shrink(&mut self) -> Frame {
        let keep = self.paragraphs();
        Frame::of(&Int {
            bx: keep,
            ..Int::dos(self.own, 0x4A00)
        })
    }

    /// The paragraphs the program keeps from its PSP on: all it holds in its
    /// own memory, its frames, stacks and state among them.
    pub fn paragraphs(&mut self) -> u16 {
        let number = u16::from(self.get8(at::NUMBER));
        needs(number).unwrap_or(u16::MAX)
    }

    /// The command line's tail, as the program's PSP holds it.
    pub fn tail(&mut self) -> Vec<u8> {
        use super::memory::psp;
        let len = self.get8(psp::TAIL).min(0x7E);
        let mut out = alloc::vec![0u8; usize::from(len)];
        self.g.read_bytes(self.own, psp::TAIL + 1, &mut out);
        out
    }

    /// A far pointer in the program's memory.
    pub fn get_far(&mut self, offset: u16) -> Far {
        let low = self.get16(offset);
        Far::new(self.get16(offset.wrapping_add(2)), low)
    }

    pub fn set_far(&mut self, offset: u16, pointer: Far) {
        self.set16(offset, pointer.offset);
        self.set16(offset.wrapping_add(2), pointer.segment);
    }

    /// The level of the root running now.
    fn level(&mut self) -> u8 {
        self.get8(at::ROOTS).saturating_sub(1).min(LEVELS - 1)
    }

    fn record(&mut self, field: u16) -> u16 {
        at::RECORDS + u16::from(self.level()) * at::RECORD + field
    }

    fn stack_top(&mut self) -> u16 {
        at::STACKS + (u16::from(self.level()) + 1) * at::STACK
    }

    /// Where the running root's scratch `offset` lies.
    pub fn scratch(&mut self, offset: u16) -> u16 {
        at::SCRATCH + u16::from(self.level()) * at::SCRATCH_LEN + offset
    }

    /// A register the running root's last DOS call returned.
    pub fn result(&mut self, reg: Returned) -> u16 {
        let at = at::RESULTS + u16::from(self.level()) * at::RESULT + reg as u16 * 2;
        self.get16(at)
    }

    /// Keeps the registers a DOS call just returned for the running root.
    fn keep_returned(&mut self) {
        let base = at::RESULTS + u16::from(self.level()) * at::RESULT;
        let regs = [Reg16::Ax, Reg16::Bx, Reg16::Cx, Reg16::Dx, Reg16::Si];
        for (n, reg) in (0u16..).zip(regs) {
            let value = self.g.r16(reg);
            self.set16(base + n * 2, value);
        }
        let es = self.g.seg(Seg::Es);
        self.set16(base + Returned::Es as u16 * 2, es);
        let flags = self.g.regs().flags;
        self.set16(base + Returned::Flags as u16 * 2, flags);
        let ds = self.g.seg(Seg::Ds);
        self.set16(base + Returned::Ds as u16 * 2, ds);
    }

    /// From a handler: the ROM entry it returns to its caller through, in
    /// place of the one its stub came with.
    pub fn set_exit(&mut self, exit: u16) {
        let at = self.record(record::EXIT);
        self.set16(at, exit);
    }

    /// From a handler: the main root's work is abandoned, its frames cut
    /// back to its root, which resumes with `Fault::Break` once the handler
    /// transfers.
    pub fn abandon(&mut self) -> Flow {
        self.set8(at::ABANDON, 1);
        Flow::Transfer
    }

    /// Takes INT `vector` from the running root's stack, with the registers
    /// as they are set; the program resumes once it returns.
    pub fn int(&mut self, vector: u8) -> Flow {
        let top = self.stack_top();
        self.g.set_stack(self.own, top);
        self.g.interrupt(vector, (ROM, entry::PROGRAM_STEP));
        Flow::Wait
    }

    /// The same, the interrupt taken to `target` rather than through its
    /// vector: the handler a program's own handler chains to.
    pub fn int_at(&mut self, target: Far) -> Flow {
        let top = self.stack_top();
        self.g.set_stack(self.own, top);
        self.g
            .interrupt_at((target.segment, target.offset), (ROM, entry::PROGRAM_STEP));
        Flow::Wait
    }

    /// From the main routine: the program ends keeping its first
    /// `paragraphs` and the handlers it installed, which run on as roots of
    /// their own.
    pub fn stay_resident(&mut self, code: u8, paragraphs: u16) -> Flow {
        self.g.set_r16(Reg16::Ax, u16::from_le_bytes([code, 0x31]));
        self.g.set_r16(Reg16::Dx, paragraphs);
        let top = self.stack_top();
        self.g.set_stack(self.own, top);
        self.g.interrupt(0x21, (ROM, bios::HALT));
        Flow::Transfer
    }

    /// From the main routine: the machine restarted as Ctrl+Alt+Del
    /// restarts it, a warm start asked of the POST and the processor sent to
    /// its reset vector with interrupts off; the program's frames end.
    pub fn restart(&mut self) -> Flow {
        self.g
            .set_bda16(crate::bios::bda::RESET_FLAG, crate::bios::bda::WARM);
        self.g.regs_mut().set_flag(crate::cpu::flag::IF, false);
        self.set8(at::DEPTH, 0);
        self.set8(at::ROOTS, 0);
        self.g.jump(ROM, bios::RESET);
        Flow::Transfer
    }

    /// Where the running handler's caller's registers lie.
    fn caller_frame(&mut self) -> Far {
        let at = self.record(record::CALLER);
        self.get_far(at)
    }

    /// A register of the running handler's caller, as its stub saved it.
    pub fn caller(&mut self, reg: Saved) -> u16 {
        let frame = self.caller_frame();
        self.g
            .read16(frame.segment, frame.offset.wrapping_add(slot(reg)))
    }

    /// Sets a register the running handler's caller gets back.
    pub fn set_caller(&mut self, reg: Saved, value: u16) {
        let frame = self.caller_frame();
        self.g
            .write16(frame.segment, frame.offset.wrapping_add(slot(reg)), value);
    }

    /// Begins `root`, returning to `caller`'s registers through `exit` once
    /// it is done; with no level or frame left for it, it is refused at
    /// once.
    fn begin(&mut self, root: &Frame, caller: Option<(Far, u16)>) {
        if let Some((frame, _)) = caller {
            self.drop_unwound(root, frame);
        }
        let (roots, depth) = (self.get8(at::ROOTS), self.get8(at::DEPTH));
        if roots >= LEVELS || depth >= DEPTH {
            return self.refuse(caller);
        }
        self.set8(at::ROOTS, roots + 1);
        self.set8(at::DEPTH, depth + 1);
        let (frame, exit) = caller.unwrap_or_default();
        let at = self.record(record::CALLER);
        self.set_far(at, frame);
        let at = self.record(record::EXIT);
        self.set16(at, exit);
        let at = self.record(record::ROOT);
        self.set8(at, depth);
        self.store(depth, root);
        frames::run(self);
    }

    /// A root that cannot be held: a handler returns to its caller as it
    /// came, and a program with no room to start says so and ends.
    fn refuse(&mut self, caller: Option<(Far, u16)>) {
        match caller {
            Some((frame, exit)) => {
                self.g.set_stack(frame.segment, frame.offset);
                self.g.jump(ROM, exit);
            }
            None => self.g.jump(ROM, entry::PROGRAM_REFUSED),
        }
    }

    /// Drops the running root's frames.
    fn end_root(&mut self) -> u8 {
        let roots = self.get8(at::ROOTS);
        let level = self.level();
        let root = if roots <= 1 {
            0
        } else {
            self.get8(Self::record_at(level, record::ROOT)).min(DEPTH)
        };
        self.set8(at::DEPTH, root);
        self.set8(at::ROOTS, roots.saturating_sub(1));
        level
    }

    fn record_at(level: u8, field: u16) -> u16 {
        at::RECORDS + u16::from(level) * at::RECORD + field
    }

    /// A call returned to the top of root `level`'s stack, `sp` its stack
    /// pointer: the roots above it began on its behalf and can no longer
    /// return — the process they served ended under them, as a break in a
    /// handler may end it — so they are dropped. A stack pointer at no
    /// root's top leaves them as they are.
    fn drop_abandoned(&mut self, sp: u16) {
        let roots = self.get8(at::ROOTS).min(LEVELS);
        let Some(level) =
            (0..roots).find(|&level| sp == at::STACKS + (u16::from(level) + 1) * at::STACK)
        else {
            return;
        };
        if level + 1 < roots {
            let root = self
                .get8(Self::record_at(level + 1, record::ROOT))
                .min(DEPTH);
            self.set8(at::DEPTH, root);
            self.set8(at::ROOTS, level + 1);
        }
    }

    /// A handler about to begin `root` for a caller whose registers lie at
    /// `caller`: an earlier call of the same handler whose caller lay on
    /// that stack no deeper can no longer be waiting, for the stack has
    /// unwound past it — its work was abandoned under it, as a break
    /// abandons the program it interrupts. That root and every one above
    /// it are dropped, so a resident program's levels are never used up by
    /// calls that will not come back.
    fn drop_unwound(&mut self, root: &Frame, caller: Far) {
        let roots = self.get8(at::ROOTS).min(LEVELS);
        for level in 0..roots {
            let earlier = self.get_far(Self::record_at(level, record::CALLER));
            let handled = self.get16(Self::record_at(level, record::EXIT)) != 0;
            let depth = self.get8(Self::record_at(level, record::ROOT)).min(DEPTH);
            let unwound =
                handled && earlier.segment == caller.segment && earlier.offset <= caller.offset;
            if unwound && self.load(depth).is_some_and(|frame| frame.id == root.id) {
                self.set8(at::DEPTH, depth);
                self.set8(at::ROOTS, level);
                return;
            }
        }
    }

    /// The main root ended with `code`: the program ends through DOS.
    fn terminate(&mut self, code: u8) {
        self.set8(at::DEPTH, 0);
        self.set8(at::ROOTS, 0);
        self.g.set_r16(Reg16::Ax, u16::from_le_bytes([code, 0x4C]));
        let top = at::STACKS + at::STACK;
        self.g.set_stack(self.own, top);
        self.g.interrupt(0x21, (ROM, bios::HALT));
    }
}

/// The code a program ends with for its main routine's `outcome`.
fn exit_code(outcome: Outcome) -> u8 {
    match outcome {
        Ok(code) => code.to_le_bytes()[0],
        Err(_) => 0xFF,
    }
}

impl Context for Program<'_, '_> {
    type Id = Id;
    const DEPTH: u8 = DEPTH;

    fn depth(&mut self) -> u8 {
        self.get8(at::DEPTH)
    }

    fn set_depth(&mut self, depth: u8) {
        self.set8(at::DEPTH, depth);
    }

    fn root_index(&mut self) -> u8 {
        let at = self.record(record::ROOT);
        self.get8(at)
    }

    fn load(&mut self, n: u8) -> Option<Frame> {
        let mut raw = [0u8; FRAME as usize];
        self.g
            .read_bytes(self.own, at::FRAMES + u16::from(n) * FRAME, &mut raw);
        decode(&raw)
    }

    fn store(&mut self, n: u8, frame: &Frame) {
        self.g
            .write_bytes(self.own, at::FRAMES + u16::from(n) * FRAME, &encode(frame));
    }

    fn step(&mut self, frame: &mut Frame) -> Flow {
        table::step(self, frame)
    }

    /// A root ended: the main routine ends the program, which alone has no
    /// caller to go back to; a handler returns to its caller, whether or
    /// not the main routine still runs below it.
    fn returned(&mut self, outcome: Outcome) {
        let frame = self.caller_frame();
        let at = self.record(record::EXIT);
        let exit = self.get16(at);
        self.end_root();
        if exit == 0 {
            return self.terminate(exit_code(outcome));
        }
        self.g.set_stack(frame.segment, frame.offset);
        self.g.jump(ROM, exit);
    }

    /// A root's work is over and the CPU set going elsewhere: a handler that
    /// abandoned the main root's work has the main root resume with
    /// [`frames::Fault::Break`]; any other root, the main one staying
    /// resident among them, simply ends.
    fn transferred(&mut self) {
        let level = self.end_root();
        let abandoning = self.get8(at::ABANDON) != 0;
        self.set8(at::ABANDON, 0);
        if level == 0 || !abandoning {
            return;
        }
        self.set8(at::ROOTS, 1);
        self.set8(at::DEPTH, 1);
        if let Some(mut main) = self.load(0) {
            main.child = Err(frames::Fault::Break);
            self.store(0, &main);
        }
        self.g.set_stack(self.own, at::STACKS + at::STACK);
        self.g.jump(ROM, entry::PROGRAM_STEP);
    }

    fn broken(&mut self) {
        self.returned(Err(frames::Fault::Error(frames::GENERAL_FAILURE)));
    }
}

/// The paragraphs program `number` needs from its PSP on, if it is one.
fn needs(number: u16) -> Option<u16> {
    let state = table::state_len(number)?;
    (u32::from(at::STATE) + u32::from(state))
        .div_ceil(16)
        .try_into()
        .ok()
}

/// A program's stub jumped to the ROM, its number in AX: it starts, its
/// stack moved into its own memory, if its block holds all it keeps.
pub fn start(g: &mut Guest<'_>) {
    let own = g.seg(Seg::Ds);
    let number = g.r16(Reg16::Ax);
    let top = g.read16(own, super::memory::psp::TOP);
    let fits =
        needs(number).is_some_and(|paragraphs| top.wrapping_sub(own) >= paragraphs && top > own);
    let root = table::main(number).filter(|_| fits);
    let mut p = Program { g, own };
    let Some(root) = root else {
        return p.refuse(None);
    };
    p.set8(at::NUMBER, number.to_le_bytes()[0]);
    p.set8(at::DEPTH, 0);
    p.set8(at::ROOTS, 0);
    p.set8(at::ABANDON, 0);
    p.begin(&root, None);
}

/// A guest call a routine made has returned: the registers it returned are
/// kept for the routine, which runs on.
pub fn resume(g: &mut Guest<'_>) {
    let own = g.seg(Seg::Ss);
    let sp = g.r16(Reg16::Sp);
    let mut p = Program { g, own };
    if p.get8(at::DEPTH) == 0 || p.get8(at::ROOTS) == 0 {
        // A program jumped to the landing pad with nothing waiting: it stops
        // where it stands.
        p.g.jump(ROM, bios::HALT);
        return;
    }
    p.drop_abandoned(sp);
    p.keep_returned();
    frames::run(&mut p);
}

/// Handler `n`'s stub was entered, the caller's registers saved: its root
/// begins on the program's own stack.
pub fn handler(g: &mut Guest<'_>, n: u16) {
    let own = g.seg(Seg::Ds);
    let caller = Far::new(g.seg(Seg::Ss), g.r16(Reg16::Sp));
    let mut p = Program { g, own };
    let number = p.get8(at::NUMBER);
    match table::handler(number, n) {
        Some(root) => p.begin(&root, Some((caller, entry::PROGRAM_EXIT))),
        None => p.refuse(Some((caller, entry::PROGRAM_EXIT))),
    }
}

/// The file of program `number`: its stub, and the stubs of the handlers
/// it may install.
///
/// # Errors
///
/// [`AsmError`] should its code not assemble, a defect of its own.
pub fn image(number: u16) -> Result<Vec<u8>, AsmError> {
    let mut asm = Asm::new(at::START);
    asm.mov_imm16(Reg16::Ax, number);
    asm.jmp_far(ROM, entry::PROGRAM);
    let mut image = asm.finish()?;
    image.resize(usize::from(at::HANDLERS - at::START), 0x90);
    for n in 0..at::HANDLER_COUNT {
        let mut asm = Asm::new(0);
        asm.push_seg(Seg::Ds);
        asm.push_seg(Seg::Cs);
        asm.pop_seg(Seg::Ds);
        asm.jmp_far(
            ROM,
            entry::PROGRAM_HANDLER + n * entry::PROGRAM_HANDLER_STRIDE,
        );
        let stub = asm.finish()?;
        image.extend_from_slice(&stub);
        image.resize(
            usize::from(at::HANDLERS - at::START + (n + 1) * at::HANDLER_STRIDE),
            0x90,
        );
    }
    Ok(image)
}

/// Where handler `n`'s stub lies in a program, for a vector to point at.
#[must_use]
pub const fn handler_entry(n: u16) -> u16 {
    at::HANDLERS + n * at::HANDLER_STRIDE
}
