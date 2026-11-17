//! SWIs (`plans/RISCOS-EMULATOR.md` §4.1): a number's fields, the kernel's
//! table, and how a SWI hands its results and errors back.
//!
//! The processor stops at a `SWI` rather than taking the exception, and the
//! kernel performs it in place: the caller's registers are read, the SWI
//! run, and its outputs written back with the caller's flags but those it
//! sets. An error sets V with R0 pointing at its block, or, from the form
//! without X, goes to the environment's error handler.

pub mod names;

use tairix_riscos_arm::cpu::Exception;
use tairix_riscos_arm::psr;
use tairix_riscos_arm::reg::Reg;

use crate::os::{len32, Fail, Os, Regs, Unwind};

/// The X bit: the SWI returns its error rather than raising it.
pub const X: u32 = 1 << 17;
/// `OS_WriteI`'s first number: `&100` to `&1FF` write their low byte.
pub const WRITE_I: u32 = 0x100;
/// The SWIs in a module's chunk.
pub const CHUNK: u32 = 64;
/// `OS_GenerateError`.
const GENERATE_ERROR: u32 = 0x2B;
/// How far a chain of `OS_CallASWI`s is followed.
const CALL_A_SWI_HOPS: u32 = 16;
/// A number no SWI has, for a chain followed too far.
const UNKNOWN: u32 = 0x80;

/// The OS SWIs the kernel itself calls.
pub(crate) mod os {
    /// `OS_ReadC`.
    pub(crate) const READ_C: u32 = 0x04;
    /// `OS_CLI`.
    pub(crate) const CLI: u32 = 0x05;
    /// `OS_Byte`.
    pub(crate) const BYTE: u32 = 0x06;
    /// `OS_Word`.
    pub(crate) const WORD: u32 = 0x07;
    /// `OS_File`.
    pub(crate) const FILE: u32 = 0x08;
    /// `OS_Args`.
    pub(crate) const ARGS: u32 = 0x09;
    /// `OS_BGet`.
    pub(crate) const BGET: u32 = 0x0A;
    /// `OS_BPut`.
    pub(crate) const BPUT: u32 = 0x0B;
    /// `OS_Find`.
    pub(crate) const FIND: u32 = 0x0D;
    /// `OS_Exit`.
    pub(crate) const EXIT: u32 = 0x11;
    /// `OS_Mouse`.
    pub(crate) const MOUSE: u32 = 0x1C;
    /// `OS_UpCall`.
    pub(crate) const UPCALL: u32 = 0x33;
    /// `OS_InstallKeyHandler`.
    pub(crate) const INSTALL_KEY_HANDLER: u32 = 0x3E;
    /// `OS_AddCallBack` and `OS_RemoveCallBack`.
    pub(crate) const ADD_CALLBACK: u32 = 0x54;
    pub(crate) const REMOVE_CALLBACK: u32 = 0x5F;
    /// `OS_Module`.
    pub(crate) const MODULE: u32 = 0x1E;
    /// `OS_ReadUnsigned`.
    pub(crate) const READ_UNSIGNED: u32 = 0x21;
    /// `OS_ReadVarVal`.
    pub(crate) const READ_VAR_VAL: u32 = 0x23;
    /// `OS_SetVarVal`.
    pub(crate) const SET_VAR_VAL: u32 = 0x24;
    /// `OS_GSInit`.
    pub(crate) const GS_INIT: u32 = 0x25;
    /// `OS_GSRead`.
    pub(crate) const GS_READ: u32 = 0x26;
    /// `OS_GSTrans`.
    pub(crate) const GS_TRANS: u32 = 0x27;
    /// `OS_FSControl`.
    pub(crate) const FS_CONTROL: u32 = 0x29;
    /// `OS_ChangeDynamicArea`.
    pub(crate) const CHANGE_DYNAMIC_AREA: u32 = 0x2A;
    /// `OS_EvaluateExpression`.
    pub(crate) const EVALUATE_EXPRESSION: u32 = 0x2D;
    /// `OS_SpriteOp`.
    pub(crate) const SPRITE_OP: u32 = 0x2E;
    /// `OS_ReadVduVariables`.
    pub(crate) const READ_VDU_VARIABLES: u32 = 0x31;
    /// `OS_PrettyPrint`.
    pub(crate) const PRETTY_PRINT: u32 = 0x44;
    /// `OS_ReadArgs`.
    pub(crate) const READ_ARGS: u32 = 0x49;
    /// `OS_ReadDynamicArea`.
    pub(crate) const READ_DYNAMIC_AREA: u32 = 0x5C;
    /// `OS_MMUControl`.
    pub(crate) const MMU_CONTROL: u32 = 0x6B;
    /// `OS_ReadLine32`.
    pub(crate) const READ_LINE32: u32 = 0x7D;
    /// `OS_SubstituteArgs32`.
    pub(crate) const SUBSTITUTE_ARGS32: u32 = 0x7E;

    /// `OS_Find`'s reasons, and the flags that refuse a missing file and a
    /// directory.
    pub(crate) mod find {
        pub(crate) const INPUT: u32 = 0x40;
        pub(crate) const OUTPUT: u32 = 0x80;
        pub(crate) const UPDATE: u32 = 0xC0;
        pub(crate) const MUST_OPEN: u32 = 0x0C;
    }

    /// `OS_Args`'s reasons: write a file's pointer, read its extent.
    pub(crate) mod args {
        pub(crate) const WRITE_POINTER: u32 = 1;
        pub(crate) const READ_EXTENT: u32 = 2;
    }

    /// `OS_FSControl`'s reasons.
    pub(crate) mod fs_control {
        pub(crate) const START_APPLICATION: u32 = 2;
        pub(crate) const RUN: u32 = 4;
        pub(crate) const CAT: u32 = 5;
        pub(crate) const OPT: u32 = 10;
        pub(crate) const PREFIX: u32 = 11;
        pub(crate) const RESTORE: u32 = 19;
        /// The current and the secondary filing systems' modules.
        pub(crate) const MODULE_BASE: u32 = 20;
        pub(crate) const SECONDARY_BASE: u32 = 30;
    }

    /// `OS_File`'s reasons.
    pub(crate) mod file {
        pub(crate) const SAVE: u32 = 0;
        pub(crate) const READ_INFO: u32 = 5;
        pub(crate) const DELETE: u32 = 6;
        pub(crate) const CREATE: u32 = 7;
        pub(crate) const CREATE_TYPED: u32 = 11;
        pub(crate) const MAKE_ERROR: u32 = 19;
        pub(crate) const READ_WITH_TYPE: u32 = 20;
        pub(crate) const LOAD: u32 = 255;
    }
}

/// A SWI the kernel performs in Rust.
pub(crate) type Handler<'o> = for<'a, 'c> fn(&'a mut Os<'o>, &'c mut Regs) -> Result<(), Fail>;

/// The registers a SWI's caller passes, as the current mode sees them.
fn caller_regs(os: &mut Os<'_>) -> Regs {
    let cpu = os.world.processor.cpu();
    let mut regs = Regs::default();
    for (n, slot) in (0..).zip(regs.r.iter_mut()) {
        *slot = cpu.reg(Reg::field(n));
    }
    regs.flags = cpu.cpsr() & psr::NZCV;
    regs
}

impl Os<'_> {
    /// Performs the `SWI` the processor stopped at, from guest code.
    pub(crate) fn swi(&mut self, number: u32) -> Result<(), Unwind> {
        let cpu = self.world.processor.cpu();
        let width = if cpu.cpsr() & psr::T != 0 { 2 } else { 4 };
        let at = cpu.pc().wrapping_sub(width);
        if !self.world.swi_passing && self.swi_vector_claimed() {
            let pc = self.world.processor.cpu().pc();
            self.world.processor.cpu().enter(Exception::Swi, pc);
            return Ok(());
        }
        let outer = core::mem::replace(&mut self.world.swi_at, at);
        let outermost = self.world.depth == 0 && self.world.frame.is_none();
        let mut regs = caller_regs(self);
        if outermost {
            if self.world.resume.take() != Some((at, number)) {
                self.world.continuation = None;
            }
            self.world.guest_called = false;
            let back = self.world.processor.cpu().pc();
            self.world.frame = Some(crate::os::Frame {
                swi: Some(number),
                return_pc: back,
                upper: [regs.r[10], regs.r[11], regs.r[12]],
            });
        }
        let number = through_call_a_swi(number, &regs);
        let result = self.perform_resolved(number, &mut regs);
        let waiting = matches!(result, Err(Fail::Unwind(Unwind::Wait(_))));
        self.world.swi_at = outer;
        let completed = self.complete(number, &regs, result);
        if outermost {
            self.world.frame = None;
            if !waiting {
                self.world.continuation = None;
            }
        }
        completed
    }

    /// Performs SWI `number` where RISC OS ignores its error: whether it
    /// succeeded. Only an unwind comes back.
    pub(crate) fn perform_quietly(&mut self, number: u32, regs: &mut Regs) -> Result<bool, Unwind> {
        match self.perform(number, regs) {
            Ok(()) => Ok(true),
            Err(Fail::Error(_)) => Ok(false),
            Err(Fail::Unwind(unwind)) => Err(unwind),
        }
    }

    /// Hands a SWI's results to its caller: R0 to R9 and the flags it set,
    /// V clear; or its error, returned or raised as its X bit says.
    fn complete(
        &mut self,
        number: u32,
        regs: &Regs,
        result: Result<(), Fail>,
    ) -> Result<(), Unwind> {
        let error = match result {
            Ok(()) => None,
            Err(Fail::Unwind(unwind)) => return Err(unwind),
            Err(Fail::Error(block)) => {
                if number & X == 0 {
                    // An ErrorV claimant that swallows the error leaves the
                    // SWI to return as though it had worked.
                    self.raise(block)?;
                    None
                } else {
                    Some(block)
                }
            }
        };
        let cpu = self.world.processor.cpu();
        for (n, &value) in (0..).zip(&regs.r[..10]) {
            cpu.set_reg(Reg::field(n), value);
        }
        let mut flags = regs.flags & (psr::N | psr::Z | psr::C);
        if let Some(block) = error {
            cpu.set_reg(Reg::field(0), block);
            flags |= psr::V;
        }
        cpu.set_nzcv(flags);
        self.callbacks_after_swi()
    }

    /// Performs SWI `number` with `regs`, from guest code or the kernel; an
    /// error is returned, never raised.
    pub fn perform(&mut self, number: u32, regs: &mut Regs) -> Result<(), Fail> {
        let number = through_call_a_swi(number, regs);
        self.world.kernel_calls += 1;
        let result = self.perform_resolved(number, regs);
        self.world.kernel_calls -= 1;
        result
    }

    /// Performs SWI `number`, any `OS_CallASWI` already resolved, however it
    /// was issued. An error block that cannot be one is replaced, but for
    /// `XOS_GenerateError`'s, which is its caller's own to give.
    fn perform_resolved(&mut self, number: u32, regs: &mut Regs) -> Result<(), Fail> {
        match self.run_handler(number, regs) {
            Err(Fail::Error(block)) if number != X | GENERATE_ERROR => {
                Err(self.checked_error(number, block))
            }
            result => result,
        }
    }

    /// SWI `number`'s handler run.
    fn run_handler(&mut self, number: u32, regs: &mut Regs) -> Result<(), Fail> {
        let base = number & !X;
        if base < WRITE_I {
            let outer = core::mem::replace(&mut self.world.performing, base);
            let result = match table::os(base) {
                Some(handler) => handler(self, regs),
                None => Err(self.no_such_swi(base)),
            };
            self.world.performing = outer;
            return result;
        }
        if base < 2 * WRITE_I {
            return self.write_c((base & 0xFF) as u8);
        }
        if let Some(result) = self.module_swi(number, regs) {
            return result;
        }
        self.unknown_swi(base, regs)
    }

    /// "SWI &… not known", naming `number`.
    pub(crate) fn no_such_swi(&mut self, number: u32) -> Fail {
        let name = crate::env::hex(number);
        self.error_with(crate::env::NO_SUCH_SWI, &[&name])
    }

    /// A SWI no module provides: UKSWIV, with its number in R11; whatever
    /// claims it answers as a module's SWI handler would, every flag its
    /// own.
    fn unknown_swi(&mut self, number: u32, regs: &mut Regs) -> Result<(), Fail> {
        let mut call = *regs;
        call.r[11] = number;
        self.call_vector(
            crate::vectors::UKSWI,
            &mut call,
            crate::os::Entry::Supervisor,
        )?;
        regs.r[..10].copy_from_slice(&call.r[..10]);
        regs.flags = call.flags & (psr::N | psr::Z | psr::C);
        if call.flag(psr::V) {
            return Err(Fail::Error(call.r[0]));
        }
        Ok(())
    }
}

mod table;

impl Os<'_> {
    /// `OS_SWINumberToString`: R0's name into R1's buffer of R2 bytes, R2
    /// the bytes written with the terminator; a buffer too small holds what
    /// fitted, unterminated, and R0 points at "Buffer overflow" with V clear,
    /// as genuine RISC OS 5 returns it.
    pub(crate) fn swi_number_to_string(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        use crate::module::ChunkNames;
        let number = regs.r[0];
        let chunk = number & !X & !(CHUNK - 1);
        let chunk_names = if number & !X < 2 * WRITE_I {
            ChunkNames::User
        } else {
            self.chunk_names(chunk)?
        };
        let naming = match &chunk_names {
            ChunkNames::User => None,
            ChunkNames::Table(table) => Some(names::Naming::Table(table)),
            ChunkNames::Title(title) => Some(names::Naming::Title(title)),
            &ChunkNames::Code { target, private } => {
                return self.name_by_code(number, target, private, regs);
            }
        };
        let mut text = names::to_string(number, naming);
        if text.try_reserve(1).is_err() {
            return Err(self.no_memory());
        }
        text.push(0);
        let room = regs.r[2] as usize;
        if text.len() > room {
            self.write_bytes(regs.r[1], &text[..room])?;
            regs.r[2] = len32(room);
            if let Fail::Error(block) = self.buffer_overflow() {
                regs.r[0] = block;
            }
            return Ok(());
        }
        self.write_bytes(regs.r[1], &text)?;
        regs.r[2] = len32(text.len());
        Ok(())
    }

    /// `OS_SWINumberFromString`: the number of the SWI R1 names.
    pub(crate) fn swi_number_from_string(&mut self, regs: &mut Regs) -> Result<(), Fail> {
        let text = self.read_until(regs.r[1], 256, |b| b <= b' ')?.0;
        let name = names::name_of(&text);
        if let Some(number) = names::os_from_string(name) {
            regs.r[0] = number;
            return Ok(());
        }
        let (x, bare, at) = match name.strip_prefix(b"X") {
            Some(rest) => (X, rest, regs.r[1].wrapping_add(1)),
            None => (0, name, regs.r[1]),
        };
        match self.module_swi_named(bare, at)? {
            Some(number) => {
                regs.r[0] = x | number;
                Ok(())
            }
            None => Err(self.error(NO_SUCH_SWI_NAME)),
        }
    }
}

/// "No SWI has that name".
const NO_SUCH_SWI_NAME: crate::error::Error = crate::error::Error {
    number: 0x1E6,
    token: "UnknownSwiName",
};

/// The SWI `OS_CallASWI` and `OS_CallASWIR12` carry, in R10 and R12, in
/// their place, its own X bit deciding how its error returns. A chain of
/// them is followed a bounded way, past which it is not known, the last
/// one's X bit kept.
fn through_call_a_swi(number: u32, regs: &Regs) -> u32 {
    let target = |number: u32| match number & !X {
        table::CALL_A_SWI => Some(regs.r[10] & 0x00FF_FFFF),
        table::CALL_A_SWI_R12 => Some(regs.r[12] & 0x00FF_FFFF),
        _ => None,
    };
    let mut number = number;
    for _ in 0..CALL_A_SWI_HOPS {
        match target(number) {
            Some(next) => number = next,
            None => return number,
        }
    }
    if target(number).is_some() {
        UNKNOWN | (number & X)
    } else {
        number
    }
}

#[cfg(test)]
mod tests;
