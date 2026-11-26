//! The `LibInit` SWIs, as RISC OS 5.30's library performs each: the stub
//! descriptions read in order, each chunk's vector written and, for an
//! application, its statics copied, and the root stack chunk's words set.
//!
//! A description is taken whole before the next is read, so one the library
//! refuses leaves those before it written, as RISC OS 5.30's does.

use tairix_riscos_arm::instruction::{BranchOp, Op};
use tairix_riscos_kernel::memory::{Bits, MemoryError};
use tairix_riscos_kernel::module::{instruction, jump_through};
use tairix_riscos_kernel::os::{Fail, Os, Regs};

use crate::chunk::{self, Chunk};
use crate::module::{error, raise, SHARED_C_LIBRARY};
use crate::stack_chunk;

/// The client a call initialises.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Form {
    Application,
    Module,
}

/// The calling standard the client's code follows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Standard {
    /// APCS-R, 26-bit: its vector holds a branch an entry.
    Apcs26,
    /// APCS-32: its vector holds `LDR PC` an entry, the entries' addresses
    /// after them.
    Apcs32,
}

/// A stub description's bytes, and the chunk id that ends a list of them.
const DESCRIPTION: u32 = 20;
const END: u32 = u32::MAX;
/// The most descriptions a list is read to: every chunk many times over.
const DESCRIPTIONS_MOST: u32 = 1024;

/// The root stack's size where R6 asks for none.
const DEFAULT_STACK: u32 = 16 * 1024;
/// The library's version, which R6 answers.
const VERSION: u32 = 6;

/// `SharedCLibrary_LibInit…`: the client's vectors written, its statics laid
/// out and its root stack chunk given, for `form` following `standard`.
pub(crate) fn lib_init(
    os: &mut Os<'_>,
    regs: &mut Regs,
    form: Form,
    standard: Standard,
) -> Result<(), Fail> {
    if standard == Standard::Apcs26 && os.caller_bits() == Bits::Bit32 {
        return Err(raise(os, error::NOT_THIRTY_TWO, &[]));
    }
    let Some(template) = os.guest_code_of(&SHARED_C_LIBRARY) else {
        return Err(raise(os, error::NO_MEMORY, &[]));
    };
    let writing = Writing { standard, template };
    match form {
        Form::Application => application(os, regs, writing),
        Form::Module => module(os, regs, writing),
    }
}

/// How a call writes what it writes.
#[derive(Clone, Copy)]
struct Writing {
    standard: Standard,
    /// Where the library statics' template is, which the library static
    /// offset relocates from.
    template: u32,
}

/// An application's library: the root stack claimed from the workspace,
/// after any statics of its own the client asks copied there.
fn application(os: &mut Os<'_>, regs: &mut Regs, writing: Writing) -> Result<(), Fail> {
    let (start, limit) = (regs.r[1], regs.r[2]);
    let client = span(regs.r[4], regs.r[5]);
    let client_bytes = client.map_or(0, |(_, bytes)| bytes);
    let stack = stack_bytes(regs.r[6]);
    if room(start, limit) < u64::from(client_bytes) + u64::from(stack) {
        return Err(raise(os, error::NO_MEMORY, &[]));
    }
    let kernel = described(os, regs.r[0], writing, Statics::Copied)?;
    if let Some((from, bytes)) = client {
        os.as_caller().copy_bytes(from, start, bytes)?;
    }
    let base = start.wrapping_add(client_bytes);
    let client_offset = client.map_or(0, |(from, _)| start.wrapping_sub(from));
    os.as_caller()
        .write_word(base.wrapping_add(stack_chunk::word::SIZE), stack)?;
    os.as_caller().write_word(
        base.wrapping_add(stack_chunk::word::LIBRARY_OFFSET),
        kernel.map_or(0, |at| at.wrapping_sub(writing.template)),
    )?;
    os.as_caller().write_word(
        base.wrapping_add(stack_chunk::word::CLIENT_OFFSET),
        client_offset,
    )?;
    regs.r[0] = limit;
    regs.r[1] = base;
    regs.r[2] = base.wrapping_add(stack);
    regs.r[6] = VERSION;
    Ok(())
}

/// A module's library: its own statics copied to its workspace, the part
/// past R3 cleared, and the supervisor stack its root stack chunk. The
/// library statics come with the module's own, so none are copied here.
fn module(os: &mut Os<'_>, regs: &mut Regs, writing: Writing) -> Result<(), Fail> {
    let (start, limit) = (regs.r[1], regs.r[2]);
    let (cleared_from, from, to) = (regs.r[3], regs.r[4], regs.r[5]);
    let size = signed_span(from, to);
    if size > i64::try_from(room(start, limit)).unwrap_or(i64::MAX) {
        return Err(raise(os, error::NO_MEMORY, &[]));
    }
    let kernel = described(os, regs.r[0], writing, Statics::Left)?;
    let copied = bytes_of(signed_span(from, cleared_from));
    let cleared = bytes_of(signed_span(cleared_from, to));
    os.as_caller().copy_bytes(from, start, copied)?;
    os.as_caller()
        .fill(start.wrapping_add(copied), cleared, 0)?;
    let client_offset = start.wrapping_sub(from);
    let stack = os.layout().stacks.supervisor.start;
    os.as_caller().write_word(
        stack.wrapping_add(stack_chunk::word::LIBRARY_OFFSET),
        kernel.map_or(0, |at| {
            at.wrapping_add(client_offset)
                .wrapping_sub(writing.template)
        }),
    )?;
    os.as_caller().write_word(
        stack.wrapping_add(stack_chunk::word::CLIENT_OFFSET),
        client_offset,
    )?;
    regs.r[0] = limit;
    regs.r[1] = stack;
    regs.r[2] = start.wrapping_add(to.wrapping_sub(from));
    regs.r[6] = VERSION;
    Ok(())
}

/// Whether a call copies each chunk's statics into its description's area.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Statics {
    Copied,
    Left,
}

/// The descriptions at `list` taken in order, each chunk's vector written
/// and, where `statics` says, its statics copied: where the first chunk
/// with statics puts the kernel's, none where no chunk has any.
fn described(
    os: &mut Os<'_>,
    list: u32,
    writing: Writing,
    statics: Statics,
) -> Result<Option<u32>, Fail> {
    let mut kernel = None;
    let mut at = list;
    for _ in 0..DESCRIPTIONS_MOST {
        let id = os.as_caller().read_word(at)?;
        if id == END {
            return Ok(kernel);
        }
        let words: [u32; 4] = [4, 8, 12, 16].map(|offset| at.wrapping_add(offset));
        let mut fields = [0; 4];
        for (field, &address) in fields.iter_mut().zip(&words) {
            *field = os.as_caller().read_word(address)?;
        }
        let [vector, vector_limit, static_base, static_limit] = fields;
        let Some(chunk) = chunk::chunk(id) else {
            return Err(raise(os, error::UNKNOWN_CHUNK, &[]));
        };
        if static_limit.wrapping_sub(static_base) != chunk.statics {
            return Err(raise(os, error::STATIC_SIZE, &[]));
        }
        let Some(asked) = entries_asked(vector, vector_limit, chunk) else {
            return Err(raise(os, error::OUT_OF_DATE, &[]));
        };
        write_vector(os, chunk, vector, asked, writing.standard)?;
        if statics == Statics::Copied {
            os.as_caller().copy_bytes(
                writing.template.wrapping_add(chunk.statics_at),
                static_base,
                chunk.statics,
            )?;
        }
        if kernel.is_none() && chunk.statics != 0 {
            kernel = Some(static_base.wrapping_sub(chunk.statics_at));
        }
        at = at.wrapping_add(DESCRIPTION);
    }
    Err(raise(os, error::UNKNOWN_CHUNK, &[]))
}

/// The entries a vector from `base` to `limit` asks of `chunk`; none past
/// what it holds. A limit below the base is refused with them, where
/// RISC OS 5.30's writes on until it faults.
fn entries_asked(base: u32, limit: u32, chunk: &Chunk) -> Option<u32> {
    let bytes = u32::try_from(signed_span(base, limit)).ok()?;
    let asked = bytes / 4;
    (asked as usize <= chunk.entries()).then_some(asked)
}

/// `chunk`'s first `asked` entries written into the vector at `base`.
fn write_vector(
    os: &mut Os<'_>,
    chunk: &Chunk,
    base: u32,
    asked: u32,
    standard: Standard,
) -> Result<(), Fail> {
    let table = 4 * asked;
    for entry in 0..asked {
        let routine =
            chunk::routine(chunk, entry as usize).and_then(|n| os.routine_of(&SHARED_C_LIBRARY, n));
        let Some(routine) = routine else {
            return Err(raise(os, error::UNKNOWN_CHUNK, &[]));
        };
        let at = base.wrapping_add(4 * entry);
        let jump = match standard {
            Standard::Apcs32 => {
                os.as_caller().write_word(at.wrapping_add(table), routine)?;
                i32::try_from(table).map_or(Err(MemoryError::HostFull), jump_through)
            }
            Standard::Apcs26 => branch_26(at, routine),
        };
        let jump = jump.map_err(|_| os.no_memory())?;
        os.as_caller().write_word(at, jump)?;
        os.yield_point()?;
    }
    Ok(())
}

/// `B routine` from `at`, in a 26-bit mode, whose PC wraps at 64 MiB: the
/// host pages at the window's top lie a short way back from application
/// space.
fn branch_26(at: u32, routine: u32) -> Result<u32, MemoryError> {
    const SPACE: i64 = 1 << 26;
    let ahead = (i64::from(routine) - i64::from(at) - 8).rem_euclid(SPACE);
    let offset = if ahead >= SPACE / 2 {
        ahead - SPACE
    } else {
        ahead
    };
    instruction(Op::Branch {
        op: BranchOp::B,
        offset: i32::try_from(offset).map_err(|_| MemoryError::HostFull)?,
    })
}

/// The root stack's bytes R6 asks for: its top half kilobytes, nought the
/// default.
const fn stack_bytes(r6: u32) -> u32 {
    match r6 >> 16 {
        0 => DEFAULT_STACK,
        kilobytes => kilobytes * 1024,
    }
}

/// The bytes from `start` to `limit`; none where the limit lies below.
fn room(start: u32, limit: u32) -> u64 {
    u64::try_from(signed_span(start, limit)).unwrap_or(0)
}

/// The client's statics from R4 to R5 where R5 lies above R4: their start
/// and their bytes.
fn span(from: u32, to: u32) -> Option<(u32, u32)> {
    let bytes = u32::try_from(signed_span(from, to)).ok()?;
    (bytes > 0).then_some((from, bytes))
}

/// `to` less `from`, the two taken as signed words, as the library compares
/// them.
fn signed_span(from: u32, to: u32) -> i64 {
    i64::from(to.cast_signed()) - i64::from(from.cast_signed())
}

/// A signed count of bytes, nought where it is below.
fn bytes_of(span: i64) -> u32 {
    u32::try_from(span.max(0)).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests;
