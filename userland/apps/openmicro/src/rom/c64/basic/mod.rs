//! The clean-room BASIC: an interpreter equivalent to BASIC V2 at its
//! documented interfaces — tokens, vectors, messages, workspace, the
//! five-byte reals to the original's bits — and at the addresses inside
//! the original that software is found calling (`plans/C64.md` §0.4, §5).

pub mod chrget;
mod decimal;
mod errors;
mod exec;
mod expr;
mod float;
mod io;
mod strings;
mod text;
mod tokens;
mod trans;
mod vars;
pub(crate) mod ws;

#[cfg(test)]
mod tests;

use crate::machine::c64::memory::BASIC_BASE;
use crate::rom::asm::{Asm, AsmError};

/// The addresses inside the original its vectors hold by default and
/// software is found calling.
pub(crate) mod fixed {
    /// The cold start, which BASIC's ROM opens with and the KERNAL's
    /// reset reaches.
    pub const COLD: u16 = 0xE394;
    /// The warm start, the ROM's second word, which BRK and
    /// RUN/STOP-RESTORE reach.
    pub const WARM: u16 = 0xE37B;
    /// IERROR's default.
    pub const ERROR: u16 = 0xE38B;
    /// IMAIN's default.
    pub const MAIN: u16 = 0xA483;
    /// ICRNCH's default.
    pub const CRUNCH: u16 = 0xA57C;
    /// IQPLOP's default.
    pub const QPLOP: u16 = 0xA71A;
    /// IGONE's default, the statement loop's step, kept whole here as it
    /// runs once a statement.
    pub const GONE: u16 = 0xA7E4;
    /// IEVAL's default.
    pub const EVAL: u16 = 0xAE86;
    /// ADRAY1's default: FAC to a signed integer in A (high) and Y.
    pub const FAC_TO_AY: u16 = 0xB1AA;
    /// ADRAY2's default: the signed integer in A (high) and Y to FAC.
    pub const AY_TO_FAC: u16 = 0xB391;
    /// USR's default: ILLEGAL QUANTITY.
    pub const ILLEGAL_QUANTITY: u16 = 0xB248;
    /// The routines software calls directly: RUN's start, the program
    /// rewound then CLR; the program rewound; the statement loop's next
    /// statement; a string printed from A (low) and Y; FAC to an unsigned
    /// word; the unsigned word in A (high) and X printed; and the cold
    /// start's two halves, BASIC's RAM and its vectors.
    pub const RUN_START: u16 = 0xA659;
    pub const TEXT_START: u16 = 0xA68E;
    pub const NEXT_STATEMENT: u16 = 0xA7AE;
    pub const PRINT_STRING: u16 = 0xAB1E;
    pub const FAC_TO_WORD: u16 = 0xB7F7;
    pub const PRINT_UNSIGNED: u16 = 0xBDCD;
    pub const INIT_RAM: u16 = 0xE3BF;
    pub const INIT_VECTORS: u16 = 0xE453;
}

/// `main`'s `JMP (IMAIN)`, which IMAIN's default follows.
const MAIN_JUMP: u16 = 3;

/// Each fixed address's routine, pinned there: IERROR's a jump, as the cold
/// start follows it too closely.
pub fn pin(a: &mut Asm) {
    for (address, routine) in [
        (fixed::COLD, "basic_cold"),
        (fixed::RUN_START, "run_start"),
        (fixed::TEXT_START, "text_start"),
        (fixed::NEXT_STATEMENT, "next_statement"),
        (fixed::PRINT_STRING, "print_string"),
        (fixed::FAC_TO_WORD, "fac_to_word"),
        (fixed::PRINT_UNSIGNED, "print_unsigned"),
        (fixed::INIT_RAM, "init_ram"),
        (fixed::INIT_VECTORS, "init_vectors"),
    ] {
        a.pin(address, |a| a.jmp(routine));
    }
    a.pin(fixed::WARM, text::warm_start);
    a.pin(fixed::ERROR, |a| a.jmp("error_report"));
    a.pin(fixed::MAIN - MAIN_JUMP, text::main_loop);
    a.pin(fixed::CRUNCH, text::crunch);
    a.pin(fixed::QPLOP, text::qplop);
    a.pin(fixed::GONE, exec::gone);
    a.pin(fixed::EVAL, expr::eval);
    a.pin(fixed::FAC_TO_AY, vars::fac_to_ay);
    a.pin(fixed::ILLEGAL_QUANTITY, errors::raisers);
    a.pin(fixed::AY_TO_FAC - float::WORD_TO_FAC, float::conversions);
}

/// BASIC from its ROM's base: the cold and warm starts' words, then the
/// interpreter, which runs on wherever the flow takes it.
pub fn emit(a: &mut Asm) -> Result<(), AsmError> {
    a.org(BASIC_BASE);
    a.word(fixed::COLD);
    a.word(fixed::WARM);
    errors::emit(a);
    text::emit(a)?;
    exec::emit(a);
    expr::emit(a);
    vars::emit(a);
    strings::emit(a);
    io::emit(a);
    float::emit(a);
    decimal::emit(a);
    trans::emit(a);
    tokens::emit(a);
    Ok(())
}
