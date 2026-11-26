//! `_swi`, `_swix`, `_vswi` and `_vswix`, as RISC OS 5.30's library calls a
//! SWI from a mask: the input registers from the arguments in turn, then a
//! pointer for each output register asked, then one for the PSR, and what is
//! left the block a register points at.

use tairix_riscos_kernel::memory::Bits;
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::X;

use super::stdio::format::{Arguments, VaList, Variadic};

/// The registers a SWI takes and gives through a mask.
const REGISTERS: u32 = 10;
/// The mask's block bit and the four bits above it naming the block's
/// register.
const BLOCK: u32 = 1 << 11;
const BLOCK_REGISTER: u32 = 12;
/// The four bits naming the register `_swi` answers, fifteen the PSR.
const RETURN: u32 = 16;
const RETURN_PSR: u32 = 15;
/// The bit asking the PSR stored.
const PSR: u32 = 1 << 21;
/// A 32-bit caller's User mode, which the PSR given holds with the flags.
const USER_32: u32 = 0x10;

/// The output bit for register `n`: bit 31 for R0, down.
const fn output(n: u32) -> u32 {
    1 << (31 - n)
}

/// Whether the call is `_swix`'s, answering its error rather than raising it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Errors {
    Raised,
    Returned,
}

/// SWI `number` per `mask`, its words from `args`: what `_swi` or `_swix`
/// answers.
fn call(
    os: &mut Os<'_>,
    number: u32,
    mask: u32,
    args: &mut impl Arguments,
    errors: Errors,
) -> Result<u32, Fail> {
    let mut regs = Regs::default();
    for n in 0..REGISTERS {
        if mask & (1 << n) != 0 {
            regs.r[n as usize] = args.word(os)?;
        }
    }
    let mut outputs = [None; REGISTERS as usize];
    for (n, slot) in (0..REGISTERS).zip(&mut outputs) {
        if mask & output(n) != 0 {
            *slot = Some(args.word(os)?);
        }
    }
    let psr_at = if mask & PSR != 0 {
        Some(args.word(os)?)
    } else {
        None
    };
    if mask & BLOCK != 0 {
        let register = (mask >> BLOCK_REGISTER & 0xF) as usize;
        let block = args.rest(os)?;
        if let Some(slot) = regs
            .r
            .get_mut(register)
            .filter(|_| register < REGISTERS as usize)
        {
            *slot = block;
        }
    }
    let swi = match errors {
        Errors::Raised => number & !X,
        Errors::Returned => number | X,
    };
    match os.perform(swi, &mut regs) {
        Ok(()) => {}
        Err(Fail::Error(block)) if errors == Errors::Returned => return Ok(block),
        Err(Fail::Error(block)) => os.raise(block)?,
        Err(unwind) => return Err(unwind),
    }
    for (n, at) in outputs.iter().enumerate() {
        if let Some(at) = *at {
            os.as_caller().write_word(at, regs.r[n])?;
        }
    }
    let psr = regs.flags
        | if os.caller_bits() == Bits::Bit32 {
            USER_32
        } else {
            0
        };
    if let Some(at) = psr_at {
        os.as_caller().write_word(at, psr)?;
    }
    Ok(match (errors, mask >> RETURN & 0xF) {
        (Errors::Returned, _) => 0,
        (Errors::Raised, RETURN_PSR) => psr,
        (Errors::Raised, n) => regs
            .r
            .get(n as usize)
            .copied()
            .filter(|_| n < REGISTERS)
            .unwrap_or(0),
    })
}

/// `_swi(number, mask, ...)`: the register the mask names; an error raised.
pub(super) fn swi(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut args = Variadic::of(os, regs, 2);
    regs.r[0] = call(os, regs.r[0], regs.r[1], &mut args, Errors::Raised)?;
    Ok(())
}

/// `_swix(number, mask, ...)`: nought, or the error.
pub(super) fn swix(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut args = Variadic::of(os, regs, 2);
    regs.r[0] = call(os, regs.r[0], regs.r[1], &mut args, Errors::Returned)?;
    Ok(())
}

/// `_vswi(number, mask, ap)`.
pub(super) fn vswi(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut args = VaList::of(os, regs.r[2])?;
    regs.r[0] = call(os, regs.r[0], regs.r[1], &mut args, Errors::Raised)?;
    Ok(())
}

/// `_vswix(number, mask, ap)`.
pub(super) fn vswix(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut args = VaList::of(os, regs.r[2])?;
    regs.r[0] = call(os, regs.r[0], regs.r[1], &mut args, Errors::Returned)?;
    Ok(())
}
