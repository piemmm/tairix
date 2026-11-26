//! `_kernel_unwind` and `_kernel_procname`: a frame unwound, and a
//! procedure named, by the language whose code holds the PC where it gives
//! a procedure for that — entered as the call's own continuation, as RISC
//! OS 5.30 enters it. Otherwise a frame is unwound as the APCS lays frames
//! out, and no procedure is named.
//!
//! RISC OS 5.30 leaves SL as it was given when unwinding back across a
//! stack extension; here SL follows, as the PRM has it: unwinding the
//! library's own frame record puts SL back in the chunk before.

use tairix_riscos_arm::a32;
use tairix_riscos_arm::cond::Condition;
use tairix_riscos_arm::fpa::Format;
use tairix_riscos_arm::instruction::{
    BlockMode, BlockOp, CoIndex, DataOp, FpaMultipleOp, FpaTransferOp, Op,
};
use tairix_riscos_arm::isa::Isa;
use tairix_riscos_arm::reg::Reg;
use tairix_riscos_kernel::memory::Bits;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::Registers;

use super::frame::{code_address, readable, SAVED_FP, SAVED_LR, SAVED_PC, SAVED_SP};
use super::language::{self, word};
use super::statics::Statics;
use super::{context, template};
use crate::chunk;
use crate::stack_chunk;

/// An unwind block's words.
pub(crate) mod block {
    /// R4 to R9.
    pub(crate) const V_REGISTERS: u32 = 0;
    pub(crate) const FP: u32 = 24;
    pub(crate) const SP: u32 = 28;
    pub(crate) const PC: u32 = 32;
    pub(crate) const SL: u32 = 36;
    /// F4 to F7, three words each.
    pub(crate) const F_REGISTERS: u32 = 40;
}

/// The registers a block holds: R4 to R9, F4 to F7.
const V_REGISTERS: core::ops::RangeInclusive<u32> = 4..=9;
const F_REGISTERS: core::ops::RangeInclusive<u8> = 4..=7;
/// A floating-point register's bytes, stored extended.
const EXTENDED: u32 = 12;

/// `_kernel_unwind`'s answers.
const UNWOUND: u32 = 1;
const STACK_END: u32 = 0;
const CORRUPT: u32 = u32::MAX;

/// How far before a frame's saved PC the instruction that stored it lies:
/// a core stores its own address plus 8, older cores plus 12.
const STORED_AHEAD: [u32; 2] = [8, 12];
/// The registers every frame-storing instruction lists: FP, IP, LR, PC.
const FRAME_REGISTERS: u16 = 0b1101_1000_0000_0000;
/// The instructions after it searched for floating-point stores: the frame
/// pointer's SUB, then one for each of F4 to F7.
const SAVES_MOST: u32 = 5;

/// What unwinding a frame came to.
enum Unwound {
    /// The block now describes the frame's caller, whose PC this is.
    Caller(u32),
    End,
    Corrupt,
}

/// `_kernel_unwind(block, language)`.
pub(super) fn unwind(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let bits = os.caller_bits();
    let (at, language_out) = (regs.r[0], regs.r[1]);
    let pc = code_address(bits, os.as_caller().read_word(at.wrapping_add(block::PC))?);
    if let Some(procedure) = language_procedure(os, statics, pc, word::UNWIND)? {
        continue_in(os, procedure, |_| ());
        return Ok(());
    }
    regs.r[0] = match unwound(os, at, bits)? {
        Unwound::Caller(pc) => {
            let name = match language::holding(os, statics, pc)? {
                Some(found) => found.name(os)?,
                None => 0,
            };
            os.as_caller().write_word(language_out, name)?;
            UNWOUND
        }
        Unwound::End => STACK_END,
        Unwound::Corrupt => CORRUPT,
    };
    Ok(())
}

/// `_kernel_procname(pc)`: the name the language holding `pc` gives, which
/// is entered with the PC in R1 as well as R0, as RISC OS 5.30 enters it.
pub(super) fn procname(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let statics = context::statics_at(os, regs.r[10])?;
    let pc = code_address(os.caller_bits(), regs.r[0]);
    if let Some(procedure) = language_procedure(os, statics, pc, word::NAME_PROC)? {
        continue_in(os, procedure, |registers| registers.r[1] = registers.r[0]);
        return Ok(());
    }
    regs.r[0] = 0;
    Ok(())
}

/// The procedure at `word` of the language whose code holds `pc`.
fn language_procedure(
    os: &mut Os<'_>,
    statics: Statics,
    pc: u32,
    word: u32,
) -> Result<Option<u32>, Fail> {
    match language::holding(os, statics, pc)? {
        Some(found) => found.procedure(os, word),
        None => Ok(None),
    }
}

/// The call carried on in `procedure` with the caller's registers, adjusted,
/// and the caller's return.
fn continue_in(os: &mut Os<'_>, procedure: u32, adjust: impl FnOnce(&mut Registers)) {
    let mut registers = os.registers();
    adjust(&mut registers);
    registers.r[15] = procedure;
    os.leave_with(registers);
}

/// The block at `at` unwound one frame as the APCS lays frames out.
fn unwound(os: &mut Os<'_>, at: u32, bits: Bits) -> Result<Unwound, Fail> {
    let fp = os.as_caller().read_word(at.wrapping_add(block::FP))?;
    if fp == 0 {
        return Ok(Unwound::End);
    }
    if fp % 4 != 0 {
        return Ok(Unwound::Corrupt);
    }
    let mut record = [0; 4];
    for (slot, saved) in record
        .iter_mut()
        .zip([SAVED_PC, SAVED_LR, SAVED_SP, SAVED_FP])
    {
        let Some(value) = readable(os, fp.wrapping_sub(saved)) else {
            return Ok(Unwound::Corrupt);
        };
        *slot = value;
    }
    let [saved_pc, lr, sp, caller] = record;
    let saved_pc = code_address(bits, saved_pc);
    if !restored(os, at, fp, saved_pc)? {
        return Ok(Unwound::Corrupt);
    }
    let mut sl = os.as_caller().read_word(at.wrapping_add(block::SL))?;
    if saved_pc == template(os)?.wrapping_add(chunk::RECORD_PC) {
        let chunk = stack_chunk::of_limit(sl);
        let Some(previous) = readable(os, chunk.wrapping_add(stack_chunk::word::PREVIOUS)) else {
            return Ok(Unwound::Corrupt);
        };
        sl = stack_chunk::limit_of(previous);
    }
    let pc = code_address(bits, lr);
    for (offset, value) in [
        (block::FP, caller),
        (block::SP, sp),
        (block::PC, pc),
        (block::SL, sl),
    ] {
        os.as_caller().write_word(at.wrapping_add(offset), value)?;
    }
    Ok(Unwound::Caller(pc))
}

/// The registers the frame at `fp` saved put in the block: R4 to R9 as the
/// instruction that stored the frame lists them, and F4 to F7 as the stores
/// after it put them; whether each could be read.
fn restored(os: &mut Os<'_>, at: u32, fp: u32, saved_pc: u32) -> Result<bool, Fail> {
    let Some((store, list)) = storing_instruction(os, saved_pc) else {
        return Ok(true);
    };
    let mut slot = fp;
    for n in (0..16).rev().filter(|n| list & 1 << n != 0) {
        if V_REGISTERS.contains(&n) {
            let Some(value) = readable(os, slot) else {
                return Ok(false);
            };
            let into = block::V_REGISTERS + 4 * (n - V_REGISTERS.start());
            os.as_caller().write_word(at.wrapping_add(into), value)?;
        }
        slot = slot.wrapping_sub(4);
    }
    let mut below = slot.wrapping_add(4);
    for k in 1..=SAVES_MOST {
        let Some(word) = readable(os, store.wrapping_add(4 * k)) else {
            break;
        };
        let (first, count) = match floating_store(word) {
            Some(Store::FramePointer) => continue,
            Some(Store::Floating { first, count }) => (first, count),
            None => break,
        };
        below = below.wrapping_sub(EXTENDED * u32::from(count));
        for i in 0..count {
            let register = (first + i) % 8;
            if F_REGISTERS.contains(&register) {
                let from = below.wrapping_add(EXTENDED * u32::from(i));
                let into =
                    block::F_REGISTERS + EXTENDED * u32::from(register - F_REGISTERS.start());
                for w in (0..EXTENDED).step_by(4) {
                    let Some(value) = readable(os, from.wrapping_add(w)) else {
                        return Ok(false);
                    };
                    os.as_caller()
                        .write_word(at.wrapping_add(into + w), value)?;
                }
            }
        }
    }
    Ok(true)
}

/// The instruction that stored a frame whose saved PC is `saved_pc`, and the
/// registers it lists.
pub(crate) fn storing_instruction(os: &mut Os<'_>, saved_pc: u32) -> Option<(u32, u16)> {
    for ahead in STORED_AHEAD {
        let address = saved_pc.wrapping_sub(ahead);
        let Some(word) = readable(os, address) else {
            continue;
        };
        let Some(instruction) = a32::decode(word, Isa::ALL) else {
            continue;
        };
        if let Op::Block {
            op: BlockOp::Stm,
            rn: Reg::SP,
            mode: BlockMode::DecrementBefore,
            writeback: true,
            user: false,
            list,
        } = instruction.op
        {
            if instruction.cond == Condition::Al && list.0 & FRAME_REGISTERS == FRAME_REGISTERS {
                return Some((address, list.0));
            }
        }
    }
    None
}

/// What an instruction after a frame's storing one does there.
enum Store {
    /// `SUB FP, IP, #n`, which stores nothing.
    FramePointer,
    /// `count` floating-point registers from `first` pushed, extended.
    Floating { first: u8, count: u8 },
}

fn floating_store(word: u32) -> Option<Store> {
    let instruction = a32::decode(word, Isa::ALL)?;
    if instruction.cond != Condition::Al {
        return None;
    }
    match instruction.op {
        Op::Data {
            op: DataOp::Sub,
            s: false,
            rd,
            rn,
            ..
        } if rd == Reg::field(11) && rn == Reg::field(12) => Some(Store::FramePointer),
        Op::FpaTransfer {
            op: FpaTransferOp::Stf,
            format: Format::Extended,
            fd,
            rn: Reg::SP,
            up: false,
            index: CoIndex::PreIndexed,
            offset: 3,
        } => Some(Store::Floating {
            first: fd,
            count: 1,
        }),
        Op::FpaMultiple {
            op: FpaMultipleOp::Sfm,
            fd,
            count,
            rn: Reg::SP,
            up: false,
            index: CoIndex::PreIndexed,
            offset,
        } if u32::from(offset) == 3 * u32::from(count) => {
            Some(Store::Floating { first: fd, count })
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use tairix_riscos_kernel::memory::Bits;

    use super::{block, chunk, floating_store, template, unwound, Store, Unwound};
    use crate::c::testing::{Client, FREE};
    use crate::kernel::frame::{SAVED_FP, SAVED_LR, SAVED_PC, SAVED_SP};

    #[test]
    fn an_extension_record_whose_chunk_cannot_be_read_is_a_corrupt_frame() {
        let (at, fp) = (FREE, FREE + 0x800);
        let mut client = Client::new();
        let record_pc =
            template(&mut client.session.kernel.os()).expect("the library") + chunk::RECORD_PC;
        for (offset, value) in [
            (SAVED_PC, record_pc),
            (SAVED_LR, FREE + 0x1000),
            (SAVED_SP, fp + 4),
            (SAVED_FP, 0),
        ] {
            client.put(fp - offset, &value.to_le_bytes());
        }
        client.put(at + block::FP, &fp.to_le_bytes());
        client.put(at + block::SL, &0x7000_0000_u32.to_le_bytes());
        let outcome = unwound(&mut client.session.kernel.os(), at, Bits::Bit32);
        assert!(matches!(outcome, Ok(Unwound::Corrupt)));
    }

    #[test]
    fn the_stores_after_a_frame_s_storing_instruction_are_read() {
        assert!(
            matches!(floating_store(0xE24C_B004), Some(Store::FramePointer)),
            "SUB FP, IP, #4"
        );
        assert!(
            matches!(
                floating_store(0xED6D_4103),
                Some(Store::Floating { first: 4, count: 1 })
            ),
            "STFE F4, [SP, #-12]!"
        );
        assert!(floating_store(0xED6D_4203).is_none(), "STFD: not extended");
        assert!(
            floating_store(0xE1A0_0000).is_none(),
            "MOV R0, R0 ends the walk"
        );
        assert!(
            floating_store(0x0D6D_4103).is_none(),
            "a conditional store is no frame's"
        );
    }
}
