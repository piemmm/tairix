//! Stack extension as RISC OS 5.30's library extends a stack: a chunk of at
//! least 4 KiB — the one kept in hand where it is big enough — SL and SP
//! moved into it, and the overflowing frame made to return through the
//! library, which puts the old chunk back and frees what lies past the one
//! it keeps. C style leaves FP in the old chunk; Pascal style moves the
//! frame, and the arguments above it, with SP.
//!
//! The frame's real FP, SP and return go in a frame record of the
//! library's own below it, which the frame's saved FP is pointed at, so a
//! backtrace walks through the extension as through a call.

use tairix_riscos_arm::psr;
use tairix_riscos_kernel::memory::Bits;
use tairix_riscos_kernel::os::{Fail, Os, Regs};

use super::frame::{returning, SAVED_FP, SAVED_LR, SAVED_PC, SAVED_SP};
use super::heap;
use super::statics::{at, Statics};
use super::trap;
use super::{context, template, Routine};
use crate::chunk;
use crate::lap::Lap;
use crate::stack_chunk::{self, word};

/// The least a chunk holds.
const CHUNK_LEAST: u32 = 4096;
/// The extension stack: 159 words claimed above the root chunk.
pub(crate) const EXTENSION_STACK_BYTES: u32 = 159 * 4;
/// An allocator called during extension enters this far below the
/// extension stack's top, as RISC OS 5.30's do.
const ALLOCATOR_BELOW_TOP: u32 = 5 * 4;
/// The frame a small-frame extension makes room for.
const SMALL_FRAME: u32 = 256;
/// The library's frame record lies this far below the overflowing frame's
/// FP, in the slop under the old chunk's limit.
const RECORD_BELOW: u32 = 104;
/// The most chunks a chain holds, each at least [`CHUNK_LEAST`] bytes and
/// disjoint: a longer one is a cycle.
const CHUNKS_MOST: u32 = u32::MAX / CHUNK_LEAST;

/// How an extension moves the frame that overflowed.
#[derive(Clone, Copy)]
enum Style {
    /// FP stays; SP moves to a chunk with room for `frame` bytes.
    C { frame: u32 },
    /// The frame and the `arguments` words above FP move with SP.
    Pascal { arguments: u32 },
}

/// A stack a library-called procedure runs on.
#[derive(Clone, Copy)]
struct Stack {
    sl: u32,
    sp: u32,
}

/// `_kernel_stkovf_split_0frame`: room for a frame of 256 bytes or fewer.
pub(crate) fn split_0frame(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    extend(os, Style::C { frame: SMALL_FRAME })
}

/// `_kernel_stkovf_split`: room down to IP, the SP the frame wants.
pub(crate) fn split_frame(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let sp = os.registers().r[13];
    extend(
        os,
        Style::C {
            frame: sp.saturating_sub(regs.r[12]),
        },
    )
}

/// `_kernel_stkovf_copyargs`: the frame moved with the IP words of
/// arguments above it.
pub(super) fn copyargs(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    extend(
        os,
        Style::Pascal {
            arguments: regs.r[12],
        },
    )
}

/// `_kernel_stkovf_copy0args`: the frame moved, no arguments above it.
pub(super) fn copy0args(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    extend(os, Style::Pascal { arguments: 0 })
}

/// The caller's stack extended as `style` says, or a stack overflow trap at
/// the call where no chunk can be had; in a privileged mode, whose stack
/// cannot grow, the overflow is an error at once.
fn extend(os: &mut Os<'_>, style: Style) -> Result<(), Fail> {
    if os.caller_privileged() {
        return trap::fault(os, trap::STACK_OVERFLOW);
    }
    let mut registers = os.registers();
    let [sl, fp, sp] = [registers.r[10], registers.r[11], registers.r[13]];
    let statics = context::statics_at(os, sl)?;
    let room = match style {
        Style::C { frame } => Some((frame, 0)),
        Style::Pascal { arguments } => {
            moved_bytes(fp, sp, arguments).map(|moved| (moved.saturating_add(SMALL_FRAME), moved))
        }
    };
    let chunk = match room {
        Some((frame, _)) => {
            let size = frame
                .saturating_add(stack_chunk::LIMIT_ABOVE)
                .max(CHUNK_LEAST);
            chunk_for(os, statics, stack_chunk::of_limit(sl), size)?
        }
        None => None,
    };
    let (Some(chunk), Some((_, moved))) = (chunk, room) else {
        return trap::at_call(os, trap::STACK_OVERFLOW);
    };
    let top = chunk.wrapping_add(os.as_caller().read_word(chunk.wrapping_add(word::SIZE))?);
    let (frame, new_sp) = match style {
        Style::C { .. } => (fp, top),
        Style::Pascal { .. } => {
            let base = top.wrapping_sub(moved);
            os.as_caller().copy_bytes(sp, base, moved)?;
            (base.wrapping_add(fp.wrapping_sub(sp)), base)
        }
    };
    divert(os, fp, frame, top)?;
    registers.r[10] = stack_chunk::limit_of(chunk);
    registers.r[11] = frame;
    registers.r[13] = new_sp;
    os.leave_with(registers);
    Ok(())
}

/// The bytes a Pascal-style extension moves: from SP up through the
/// `arguments` words above FP; none where they make no sense.
fn moved_bytes(fp: u32, sp: u32, arguments: u32) -> Option<u32> {
    arguments
        .checked_mul(4)?
        .checked_add(4)?
        .checked_add(fp)?
        .checked_sub(sp)
}

/// The frame now at `frame` made to return through the library: its real
/// saved FP, SP and return copied to the library's record below `original`
/// — where the frame lay before it moved — and the frame pointed at that
/// record, SP the new chunk's `top`.
fn divert(os: &mut Os<'_>, original: u32, frame: u32, top: u32) -> Result<(), Fail> {
    let record = original.wrapping_sub(RECORD_BELOW);
    for saved in [SAVED_FP, SAVED_SP, SAVED_LR] {
        let value = os.as_caller().read_word(frame.wrapping_sub(saved))?;
        os.as_caller()
            .write_word(record.wrapping_sub(saved), value)?;
    }
    let record_pc = template(os)?.wrapping_add(chunk::RECORD_PC);
    os.as_caller()
        .write_word(record.wrapping_sub(SAVED_PC), record_pc)?;
    let lr = os.as_caller().read_word(frame.wrapping_sub(SAVED_LR))?;
    let routine = context::routine(os, Routine::StackReturned)?;
    let returned = returning_through(os.caller_bits(), routine, lr);
    for (saved, value) in [(SAVED_FP, record), (SAVED_SP, top), (SAVED_LR, returned)] {
        os.as_caller()
            .write_word(frame.wrapping_sub(saved), value)?;
    }
    Ok(())
}

/// The return word that sends a frame's return to `routine`: a 26-bit one
/// keeps the caller's flags.
const fn returning_through(bits: Bits, routine: u32, lr: u32) -> u32 {
    match bits {
        Bits::Bit26 => (routine & psr::R15_PC) | (lr & !psr::R15_PC),
        Bits::Bit32 => routine,
    }
}

/// A chunk of at least `size` bytes after `old`: the one kept in hand where
/// it is big enough, else one claimed and linked after it, the kept one
/// freed; none where none can be had.
fn chunk_for(os: &mut Os<'_>, statics: Statics, old: u32, size: u32) -> Result<Option<u32>, Fail> {
    let kept = os.as_caller().read_word(old.wrapping_add(word::NEXT))?;
    if kept != 0 {
        if os.as_caller().read_word(kept.wrapping_add(word::SIZE))? >= size {
            return Ok(Some(kept));
        }
        os.as_caller().write_word(old.wrapping_add(word::NEXT), 0)?;
        let on = extension_stack(os, statics)?;
        release(os, kept, on)?;
    }
    let Some((chunk, deallocate)) = claim(os, statics, size)? else {
        return Ok(None);
    };
    let library = os
        .as_caller()
        .read_word(old.wrapping_add(word::LIBRARY_OFFSET))?;
    let client = os
        .as_caller()
        .read_word(old.wrapping_add(word::CLIENT_OFFSET))?;
    for (offset, value) in [
        (word::MARK, stack_chunk::MARK),
        (word::NEXT, 0),
        (word::PREVIOUS, old),
        (word::SIZE, size),
        (word::DEALLOCATE, deallocate),
        (word::LIBRARY_OFFSET, library),
        (word::CLIENT_OFFSET, client),
    ] {
        os.as_caller()
            .write_word(chunk.wrapping_add(offset), value)?;
    }
    os.as_caller()
        .write_word(old.wrapping_add(word::NEXT), chunk)?;
    Ok(Some(chunk))
}

/// `size` bytes for a chunk and the freer it remembers: through the
/// client's allocator on the extension stack, or the heap's own with the
/// library's freer where none is registered.
fn claim(os: &mut Os<'_>, statics: Statics, size: u32) -> Result<Option<(u32, u32)>, Fail> {
    let alloc = statics.read(os, at::ALLOC)?;
    if alloc == 0 {
        let deallocate = context::routine(os, Routine::DefaultFree)?;
        return Ok(heap::claim(os, statics, size)?.map(|chunk| (chunk, deallocate)));
    }
    let deallocate = statics.read(os, at::FREE)?;
    let on = extension_stack(os, statics)?;
    let chunk = call_on(os, on, alloc, size)?;
    Ok((chunk != 0).then_some((chunk, deallocate)))
}

/// `chunk` and the chunks after it, each given back through the freer it
/// remembers, which runs `on` the stack given. The links are the client's
/// to overwrite: the walk stops at one leading to no chunk, or back to one
/// it has given back.
fn release(os: &mut Os<'_>, first: u32, on: Stack) -> Result<(), Fail> {
    let no_op = context::routine(os, Routine::DefaultFree)?;
    let mut next = first;
    let mut lap = Lap::from(first);
    for _ in 0..CHUNKS_MOST {
        if next == 0
            || os.as_caller().read_word(next.wrapping_add(word::MARK))? != stack_chunk::MARK
        {
            break;
        }
        let after = os.as_caller().read_word(next.wrapping_add(word::NEXT))?;
        let deallocate = os
            .as_caller()
            .read_word(next.wrapping_add(word::DEALLOCATE))?;
        if deallocate != 0 && deallocate != no_op {
            call_on(os, on, deallocate, next)?;
        }
        if lap.repeated(after) {
            break;
        }
        next = after;
        os.yield_point()?;
    }
    Ok(())
}

/// The client's procedure at `address` called with `argument`, on `on`, no
/// frame: its R0 back.
fn call_on(os: &mut Os<'_>, on: Stack, address: u32, argument: u32) -> Result<u32, Fail> {
    let mut call = Regs::default();
    call.r[0] = argument;
    call.r[10] = on.sl;
    crate::client::call(os, address, &mut call, on.sp)?;
    Ok(call.r[0])
}

/// The extension stack an allocator runs on during extension.
fn extension_stack(os: &mut Os<'_>, statics: Statics) -> Result<Stack, Fail> {
    let base = statics.read(os, at::EXTENSION_STACK)?;
    Ok(Stack {
        sl: stack_chunk::limit_of(base),
        sp: base
            .wrapping_add(EXTENSION_STACK_BYTES)
            .wrapping_sub(ALLOCATOR_BELOW_TOP),
    })
}

/// The extension stack laid at `base`, the root chunk's top, as RISC OS
/// 5.30 lays it: a chunk's header — its own address where the mark goes —
/// with the root's static offsets, so an allocator running on it finds its
/// statics through SL.
pub(super) fn lay_extension_stack(os: &mut Os<'_>, root: u32, base: u32) -> Result<(), Fail> {
    let library = os
        .as_caller()
        .read_word(root.wrapping_add(word::LIBRARY_OFFSET))?;
    let client = os
        .as_caller()
        .read_word(root.wrapping_add(word::CLIENT_OFFSET))?;
    for (offset, value) in [
        (word::MARK, base),
        (word::NEXT, 0),
        (word::PREVIOUS, 0),
        (word::SIZE, EXTENSION_STACK_BYTES),
        (word::DEALLOCATE, 0),
        (word::LIBRARY_OFFSET, library),
        (word::CLIENT_OFFSET, client),
    ] {
        os.as_caller()
            .write_word(base.wrapping_add(offset), value)?;
    }
    Ok(())
}

/// The overflowing frame has returned, FP the library's record: SL back in
/// the chunk before, the frame's real FP, SP and return taken from the
/// record, and every chunk past the one now kept in hand freed on the stack
/// returned to.
pub(super) fn stack_returned(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let mut registers = os.registers();
    let record = registers.r[11];
    let chunk = stack_chunk::of_limit(registers.r[10]);
    let previous = os
        .as_caller()
        .read_word(chunk.wrapping_add(word::PREVIOUS))?;
    let caller_frame = os.as_caller().read_word(record.wrapping_sub(SAVED_FP))?;
    let entry_sp = os.as_caller().read_word(record.wrapping_sub(SAVED_SP))?;
    let back = os.as_caller().read_word(record.wrapping_sub(SAVED_LR))?;
    let sl = stack_chunk::limit_of(previous);
    let past = os.as_caller().read_word(chunk.wrapping_add(word::NEXT))?;
    if past != 0 {
        os.as_caller()
            .write_word(chunk.wrapping_add(word::NEXT), 0)?;
        release(os, past, Stack { sl, sp: entry_sp })?;
    }
    registers.r[10] = sl;
    registers.r[11] = caller_frame;
    registers.r[13] = entry_sp;
    (registers.r[15], registers.flags) = returning(os.caller_bits(), back, registers.flags);
    os.leave_with(registers);
    Ok(())
}

/// The library's own freer, which a chunk claimed from the heap remembers:
/// it gives nothing back, as RISC OS 5.30's does not.
#[expect(clippy::unnecessary_wraps, reason = "a routine's signature")]
pub(super) fn default_free(_os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    Ok(())
}

#[cfg(test)]
mod tests {
    extern crate std;

    use std::vec::Vec;

    use tairix_riscos_kernel::memory::Bits;

    use super::{moved_bytes, release, returning_through, stack_chunk, word, Stack};
    use crate::c::testing::{Client, CODE, FREE};

    #[test]
    fn a_chain_is_given_back_no_further_than_its_chunks_nor_round_again() {
        const CURSOR: u32 = FREE + 0x100;
        const LOG: u32 = FREE + 0x104;
        // `LDR R12, cursor; LDR R1, [R12]; STR R0, [R1], #4; STR R1, [R12];
        // MOV PC, R14`: the freer noting each chunk it is given.
        let noter: Vec<u8> = [
            0xE59F_C00C,
            0xE59C_1000,
            0xE481_0004,
            0xE58C_1000,
            0xE1A0_F00E,
            CURSOR,
        ]
        .into_iter()
        .flat_map(u32::to_le_bytes)
        .collect();
        let (first, second) = (FREE + 0x1000, FREE + 0x2000);
        let on = Stack {
            sl: FREE + 0x7000,
            sp: FREE + 0x8000,
        };
        let mut client = Client::new();
        client.put(CODE, &noter);
        let chunk = |client: &mut Client, at: u32, mark: u32, next: u32| {
            let words = [
                (word::MARK, mark),
                (word::NEXT, next),
                (word::DEALLOCATE, CODE),
            ];
            for (offset, value) in words {
                client.put(at + offset, &value.to_le_bytes());
            }
        };
        client.running();
        client.session.allow_words(Some(100));
        for (next, case) in [(second, "a link to no chunk"), (first, "a link round")] {
            client.put(CURSOR, &LOG.to_le_bytes());
            chunk(&mut client, first, stack_chunk::MARK, next);
            chunk(&mut client, second, 0, 0);
            release(&mut client.session.kernel.os(), first, on).expect(case);
            assert_eq!(client.word(CURSOR), LOG + 4, "{case}: one freed");
            assert_eq!(client.word(LOG), first, "{case}");
        }
    }

    #[test]
    fn a_pascal_move_takes_the_frame_and_the_argument_words_above_fp() {
        assert_eq!(moved_bytes(0x9000, 0x8FEC, 0), Some(24));
        assert_eq!(moved_bytes(0x9000, 0x8FEC, 6), Some(48));
        assert_eq!(moved_bytes(0x9000, 0x9010, 0), None, "SP above FP");
        assert_eq!(
            moved_bytes(0xFFFF_FFF0, 0, 8),
            None,
            "past the address space"
        );
        assert_eq!(moved_bytes(0x9000, 0x8000, u32::MAX), None);
    }

    #[test]
    fn a_26_bit_return_through_the_library_keeps_the_caller_s_flags() {
        let routine = 0x03F0_1234 & !3;
        assert_eq!(
            returning_through(Bits::Bit32, 0xFC01_2340, 0x6000_8000),
            0xFC01_2340
        );
        assert_eq!(
            returning_through(Bits::Bit26, routine, 0x6000_8003),
            routine | 0x6000_0003,
            "the address the routine's, the PSR bits the caller's"
        );
    }
}
