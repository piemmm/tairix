//! `_kernel_init`: the client's run-time state laid out as RISC OS 5.30's
//! library lays it out, its handlers set, each language's initialisation
//! procedure called in turn, and the run procedure the first to offer one
//! gave entered — "no main program" where none did. `_kernel_entermodule`
//! does the same for a module run as an application, its root stack chunk
//! at the application space's start; `_kernel_moduleinit` readies a
//! module's library to be called in Supervisor mode.
//!
//! The procedures are entered by leaving with their registers and the
//! library's continuation as their return, one after another.

use tairix_riscos_arm::instruction::{BlockMode, BlockOp, DataOp, Immediate, Op, Operand};
use tairix_riscos_arm::reg::{Reg, RegList};
use tairix_riscos_kernel::memory::layout::APPLICATION;
use tairix_riscos_kernel::module::{instruction, jump_through};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::{fp_emulator, os as kos, X};
use tairix_riscos_kernel::Registers;

use super::environment::{self, handler};
use super::exit;
use super::frame::returning;
use super::language::{self, word};
use super::stack::{self, EXTENSION_STACK_BYTES};
use super::statics::{at, Statics};
use super::trap::state;
use super::{context, template, Routine};
use crate::module::{error, raise};
use crate::stack_chunk;

/// The longest command line copied: what `OS_GetEnv`'s holds.
pub(crate) const COMMAND_MOST: usize = kos::COMMAND_LINE as usize;
/// An exception's trampoline: four words, the first three code.
pub(crate) const TRAMPOLINE: u32 = 16;

/// What a client is started with: its image's base, its language
/// descriptions, its root stack chunk, whether it is a module, and the
/// workspace's end.
#[derive(Clone, Copy)]
struct Start {
    image_base: u32,
    languages: [u32; 2],
    base: u32,
    top: u32,
    module: u32,
    end: u32,
}

/// The init block's words after the image base: the language descriptions'
/// start and end.
fn languages_of(os: &mut Os<'_>, block: u32) -> Result<[u32; 2], Fail> {
    Ok([
        os.as_caller().read_word(block.wrapping_add(4))?,
        os.as_caller().read_word(block.wrapping_add(8))?,
    ])
}

/// `_kernel_init(block, chunk base, chunk top, module, workspace end)`.
pub(super) fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let [block, base, top, module, end] = [regs.r[0], regs.r[1], regs.r[2], regs.r[3], regs.r[4]];
    let start = Start {
        image_base: os.as_caller().read_word(block)?,
        languages: languages_of(os, block)?,
        base,
        top,
        module,
        end,
    };
    started(os, start)
}

/// `_kernel_entermodule(block, …, R6 root stack size, R8 private word)`: the
/// root stack chunk laid at the application space's start with the static
/// offsets the module's workspace holds after its first word, then the
/// client started as `_kernel_init` starts one, a module.
pub(super) fn entermodule(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let [block, size, private] = [regs.r[0], regs.r[6], regs.r[8]];
    let workspace = os.as_caller().read_word(private)?;
    let library = os.as_caller().read_word(workspace.wrapping_add(4))?;
    let client = os.as_caller().read_word(workspace.wrapping_add(8))?;
    let mut env = Regs::default();
    os.perform(X | kos::GET_ENV, &mut env)?;
    let (base, end) = (APPLICATION, env.r[1]);
    let top = base.checked_add(size).filter(|&top| {
        top.checked_add(EXTENSION_STACK_BYTES)
            .is_some_and(|needed| needed <= end)
    });
    let Some(top) = top else {
        let refused = raise(os, error::NO_MEMORY, &[]);
        return exit::generate(os, refused);
    };
    for (offset, value) in [
        (stack_chunk::word::SIZE, size),
        (stack_chunk::word::LIBRARY_OFFSET, library),
        (stack_chunk::word::CLIENT_OFFSET, client),
    ] {
        os.as_caller()
            .write_word(base.wrapping_add(offset), value)?;
    }
    let start = Start {
        image_base: APPLICATION,
        languages: languages_of(os, block)?,
        base,
        top,
        module: 1,
        end,
    };
    started(os, start)
}

/// `_kernel_moduleinit(block, SVC stack base)`, the return pushed: the
/// language descriptions kept, SL above the base, and the return taken
/// from the stack.
pub(super) fn moduleinit(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let [block, base] = [regs.r[0], regs.r[1]];
    let sl = stack_chunk::limit_of(base);
    let statics = context::statics_at(os, sl)?;
    let [languages, languages_end] = languages_of(os, block)?;
    for (field, value) in [
        (at::LANGUAGES, languages),
        (at::LANGUAGES_END, languages_end),
        (at::MODULE, 1),
    ] {
        statics.write(os, field, value)?;
    }
    let mut registers = os.registers();
    let sp = registers.r[13];
    let back = os.as_caller().read_word(sp)?;
    (registers.r[15], registers.flags) = returning(os.caller_bits(), back, registers.flags);
    registers.r[13] = sp.wrapping_add(4);
    registers.r[10] = sl;
    os.leave_with(registers);
    Ok(())
}

/// The client `start` describes laid out and its languages initialised.
fn started(os: &mut Os<'_>, start: Start) -> Result<(), Fail> {
    let Start {
        image_base,
        languages: [languages, languages_end],
        base,
        top,
        module,
        end,
    } = start;
    let template = template(os)?;
    let statics = Statics::of(os, stack_chunk::limit_of(base), template)?;
    for (field, value) in [
        (at::IMAGE_BASE, image_base),
        (at::LANGUAGES, languages),
        (at::LANGUAGES_END, languages_end),
        (at::HEAP, top.wrapping_add(EXTENSION_STACK_BYTES)),
        (at::HEAP_LIMIT, end),
        (at::ROOT_CHUNK, base),
        (at::MODULE, module),
        (at::EXTENSION_STACK, top),
        (at::RETURN_CODE, 0),
        (at::ERROR_PENDING, 0),
        (at::ESCAPE_SEEN, 0),
        (at::TRAP_SEARCH + state::ACTIVE, 0),
        (at::EVENT_SEARCH + state::ACTIVE, 0),
        (at::PENDING, 0),
        (at::ALLOC, 0),
        (at::FREE, 0),
        (at::SLOT_EXTEND, 0),
        (at::INIT_RUN, 0),
        (at::FINAL_NEXT, 0),
    ] {
        statics.write(os, field, value)?;
    }
    for (offset, value) in [
        (stack_chunk::word::MARK, stack_chunk::MARK),
        (stack_chunk::word::NEXT, 0),
        (stack_chunk::word::PREVIOUS, 0),
        (stack_chunk::word::DEALLOCATE, 0),
    ] {
        os.as_caller()
            .write_word(base.wrapping_add(offset), value)?;
    }
    stack::lay_extension_stack(os, base, top)?;
    let command = command_copy(os, top)?;
    statics.write(os, at::COMMAND, command)?;
    let mut version = Regs::default();
    let available = os.perform_quietly(X | fp_emulator::VERSION, &mut version)?;
    statics.write(os, at::FP_AVAILABLE, u32::from(available))?;
    set_handlers(os, statics)?;
    let first = statics.read(os, at::LANGUAGES)?;
    statics.write(os, at::INIT_NEXT, first)?;
    next_init(os, statics)
}

/// The command line copied to the root chunk's top, word-rounded: where it
/// is, which is where the stack starts below.
fn command_copy(os: &mut Os<'_>, top: u32) -> Result<u32, Fail> {
    let mut env = Regs::default();
    os.perform(X | kos::GET_ENV, &mut env)?;
    let line = os
        .as_caller()
        .read_until(env.r[0], COMMAND_MOST, |b| b < b' ')?
        .0;
    let bytes = u32::try_from(line.len()).unwrap_or(0) + 1;
    let at = top.wrapping_sub(bytes.next_multiple_of(4));
    os.as_caller().write_bytes(at, &line)?;
    os.as_caller().write_byte(at.wrapping_add(bytes - 1), 0)?;
    Ok(at)
}

/// The library's handlers set as RISC OS 5.30 sets them: the four
/// exceptions' through trampolines in the statics, the rest its routines
/// with the statics as their R12.
pub(crate) fn set_handlers(os: &mut Os<'_>, statics: Statics) -> Result<(), Fail> {
    let base = statics.base();
    for (place, &number) in (0..).zip(&environment::SET) {
        let routine = |os: &mut Os<'_>, routine| context::routine(os, routine);
        let words = match number {
            handler::UNDEFINED
            | handler::PREFETCH_ABORT
            | handler::DATA_ABORT
            | handler::ADDRESS_EXCEPTION => {
                let at = statics.field(at::TRAMPOLINES + TRAMPOLINE * place);
                let entry = routine(os, Routine::Exception(place))?;
                let Some(words) = trampoline(place, entry) else {
                    return Err(raise(os, error::NO_MEMORY, &[]));
                };
                for (offset, word) in (0..).zip(words) {
                    os.as_caller()
                        .write_word(at.wrapping_add(4 * offset), word)?;
                }
                [at, 0, 0]
            }
            handler::ERROR => [
                routine(os, Routine::Error)?,
                base,
                statics.field(at::ERROR_BUFFER),
            ],
            handler::CALLBACK => [
                routine(os, Routine::CallBack)?,
                base,
                statics.field(at::CALLBACK_REGISTERS),
            ],
            handler::ESCAPE => [routine(os, Routine::Escape)?, base, 0],
            handler::EVENT => [routine(os, Routine::Event)?, base, 0],
            handler::EXIT => [routine(os, Routine::Exit)?, base, 0],
            handler::UPCALL => [routine(os, Routine::UpCall)?, base, 0],
            _ => continue,
        };
        environment::set(os, statics, place, number, words)?;
    }
    Ok(())
}

/// Trampoline `n`: `STMDB R13!, {R0-R4, R12, R14, PC}`; R12 the statics;
/// on to `entry`.
fn trampoline(n: u32, entry: u32) -> Option<[u32; 4]> {
    let back = Immediate::encode(0x18 + TRAMPOLINE * n)?;
    Some([
        instruction(Op::Block {
            op: BlockOp::Stm,
            rn: Reg::SP,
            mode: BlockMode::DecrementBefore,
            writeback: true,
            user: false,
            list: RegList(0b1101_0000_0001_1111),
        })
        .ok()?,
        instruction(Op::Data {
            op: DataOp::Sub,
            s: false,
            rd: Reg::field(12),
            rn: Reg::PC,
            operand: Operand::Immediate(back),
        })
        .ok()?,
        jump_through(4).ok()?,
        entry,
    ])
}

/// The next initialisation procedure entered; past the last, the run
/// procedure, or "no main program".
pub(super) fn next_init(os: &mut Os<'_>, statics: Statics) -> Result<(), Fail> {
    let end = statics.read(os, at::LANGUAGES_END)?;
    let next = statics.read(os, at::INIT_NEXT)?;
    let mut walk = language::from(os, statics, next)?;
    while let Some(language) = walk.next(os)? {
        statics.write(os, at::INIT_NEXT, walk.position())?;
        if let Some(procedure) = language.procedure(os, word::INIT)? {
            let mut registers = entered(os, statics)?;
            registers.r[0] = language.at;
            registers.r[5] = language.at;
            registers.r[6] = end;
            registers.r[7] = language.size();
            registers.r[14] = context::routine(os, Routine::InitReturned)?;
            registers.r[15] = procedure;
            os.leave_with(registers);
            return Ok(());
        }
    }
    let run = statics.read(os, at::INIT_RUN)?;
    if run == 0 {
        environment::restore(os, statics)?;
        let failed = raise(os, error::NO_MAIN, &[]);
        return exit::generate(os, failed);
    }
    let mut registers = entered(os, statics)?;
    registers.r[0] = run;
    registers.r[4] = run;
    registers.r[14] = context::routine(os, Routine::RunReturned)?;
    registers.r[15] = run;
    os.leave_with(registers);
    Ok(())
}

/// An initialisation procedure has returned R0: the run procedure it
/// offers, kept where it is the first offered.
pub(super) fn init_returned(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    if regs.r[0] != 0 && statics.read(os, at::INIT_RUN)? == 0 {
        statics.write(os, at::INIT_RUN, regs.r[0])?;
    }
    next_init(os, statics)
}

/// The registers a procedure the kernel enters starts with: SL above the
/// root chunk's base, no frame, the stack below the command line's copy,
/// R9 the statics.
pub(super) fn entered(os: &mut Os<'_>, statics: Statics) -> Result<Registers, Fail> {
    let mut registers = Registers::default();
    registers.r[9] = statics.base();
    registers.r[10] = stack_chunk::limit_of(statics.read(os, at::ROOT_CHUNK)?);
    registers.r[11] = 0;
    registers.r[13] = statics.read(os, at::COMMAND)?;
    Ok(registers)
}
