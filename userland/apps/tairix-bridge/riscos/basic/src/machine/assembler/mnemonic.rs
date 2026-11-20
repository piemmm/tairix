//! Mnemonics: each stem BASIC's assembler knows, the form its operands
//! take, and the reading of a statement's first word into a stem, a
//! condition and whatever suffixes its form allows.
//!
//! BASIC reads a mnemonic as a prefix, needing nothing to end it, so the
//! choice between stems that prefix one another is BASIC's own: a stem
//! followed by a condition wins over a longer stem (`BLE` is `B` if less or
//! equal, `VMOVNE` a conditional `VMOV`), but for `SEVL`, which wins over
//! `SEV` followed by `LE`; failing a condition, the longest stem wins.

use tairix_openbasic::list::{pieces, Piece};
use tairix_riscos_arm::cond::Condition;
use tairix_riscos_arm::instruction::{
    ArithmeticOp, BarrierOp, BitfieldOp, BlockMode, BranchOp, CallOp, CoRegisterOp, CoRegistersOp,
    CoTransferOp, CompareOp, CoprocessorOp, DataOp, ExchangeOp, ExclusiveOp, ExtendOp,
    FpaCompareOp, FpaDyadicOp, FpaMonadicOp, FpaMultipleOp, FpaTransferOp, HintOp, Lanes, LongOp,
    MoveOp, MultiplyOp, PackOp, Prefix, PreloadOp, SaturateOp, UnaryOp, WideOp,
};

/// What a stem's operands are.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Form {
    Data(DataOp),
    Compare(CompareOp),
    Move(MoveOp),
    Wide(WideOp),
    Adr,
    /// `MUL`: Rd, Rm, Rs.
    Mul,
    /// `MLA`: Rd, Rm, Rs, Rn.
    Mla,
    /// `MLS`: Rd, Rm, Rs, Rn.
    Mls,
    /// The long multiplies with an `S`.
    Long(LongOp),
    /// `SMLA<x><y>` and `SMLAW<y>`: Rd, Rm, Rs, Rn; `SMUL<x><y>` and
    /// `SMULW<y>`: Rd, Rm, Rs.
    Halves {
        op: MultiplyOp,
        x: bool,
        y: bool,
    },
    /// `SMLAL<x><y>`: `RdLo`, `RdHi`, `Rm`, `Rs`.
    LongHalves {
        x: bool,
        y: bool,
    },
    /// The ARMv6 and ARMv7 multiplies and divides of three or four
    /// registers, `x` their `X` or `R`.
    Multiply {
        op: MultiplyOp,
        x: bool,
    },
    /// `SMLALD`, `SMLSLD` and `UMAAL`.
    LongDual {
        op: LongOp,
        x: bool,
    },
    Parallel(Prefix, Lanes),
    Arithmetic(ArithmeticOp),
    Unary(UnaryOp),
    Saturate(SaturateOp),
    Extend(ExtendOp),
    Pack(PackOp),
    Bitfield(BitfieldOp),
    Mrs,
    Msr,
    Branch(BranchOp),
    Exchange(ExchangeOp),
    Call(CallOp),
    /// `SMC` spelt `SMI`, whose number takes no `#`.
    Smi,
    Hint(HintOp),
    /// `NOP`: `MOV R0,R0`, taking no condition.
    Nop,
    Barrier(BarrierOp),
    ClearExclusive,
    ExceptionReturn,
    /// `CPS`, `CPSIE` and `CPSID`: the interrupt change they make.
    ChangeState(u8),
    SetEndian,
    SaveState(BlockMode),
    ReturnFromException(BlockMode),
    /// `LDR` and `STR` and their suffixes.
    Transfer {
        load: bool,
    },
    Preload(PreloadOp),
    /// `LDM` and `STM` and their modes.
    Block {
        load: bool,
    },
    /// `POP` and `PUSH`.
    Stack {
        load: bool,
    },
    Swap,
    Exclusive(ExclusiveOp),
    /// `LDA` and `STL`, a `B` or `H` after their condition.
    Ordered {
        load: bool,
    },
    Coprocessor(CoprocessorOp),
    CoRegister(CoRegisterOp),
    CoRegisters(CoRegistersOp),
    CoTransfer(CoTransferOp),
    FpaDyadic(FpaDyadicOp),
    FpaMonadic(FpaMonadicOp),
    Flt,
    Fix,
    /// `WFS` and `RFS`.
    FpaStatus {
        write: bool,
    },
    /// `WFC` and `RFC`: transfers to and from coprocessor 1's opcode 2.
    FpaControl {
        write: bool,
    },
    FpaCompare(FpaCompareOp),
    FpaTransfer(FpaTransferOp),
    FpaMultiple(FpaMultipleOp),
    Equ(Equ),
    Align,
    Opt,
}

/// The data directives.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum Equ {
    /// `EQUB`: a number's low byte.
    Byte,
    /// `DCB` and `=`: a string's bytes, or a number's low byte.
    ByteOrText,
    /// `EQUW` and `DCW`.
    Half,
    /// `EQUD` and `DCD`.
    Word,
    /// `EQUS`.
    Text,
    /// `EQUFS` and `DCFS`.
    Single,
    /// `EQUFD` and `DCFD`.
    Double,
    /// `EQUFE` and `DCFE`.
    Extended,
}

/// A stem, the form of what follows it, and whether a condition may follow
/// it at once.
#[derive(Clone, Copy, Debug)]
pub(super) struct Stem {
    pub(super) text: &'static str,
    pub(super) form: Form,
    pub(super) conditional: bool,
}

pub(super) const fn c(text: &'static str, form: Form) -> Stem {
    Stem {
        text,
        form,
        conditional: true,
    }
}

pub(super) const fn u(text: &'static str, form: Form) -> Stem {
    Stem {
        text,
        form,
        conditional: false,
    }
}

/// The stems tried longest first ahead of every stem a condition follows.
const LONGEST_FIRST: [&str; 1] = ["SEVL"];

/// A statement's first word, its first keyword spelt out, and how far it
/// has been read.
pub(super) struct Word {
    /// The word in capitals: the keyword's spelling, then the bytes after it.
    chars: [u8; Self::ROOM],
    len: usize,
    /// The keyword's spelling's length, and the bytes its token took.
    spelt: usize,
    token_bytes: usize,
    /// The offset in the statement's text the word starts at.
    origin: usize,
    /// How far it has been read.
    pub(super) at: usize,
}

impl Word {
    /// The longest mnemonic with its condition and suffixes, and more.
    const ROOM: usize = 24;

    /// The word `text`, `origin` bytes into a statement's text, begins with:
    /// a leading token spelt as `LIST` spells it, then the bytes after it to
    /// a space, a colon or the line's end.
    pub(super) fn of(text: &[u8], origin: usize) -> Self {
        let mut word = Self {
            chars: [0; Self::ROOM],
            len: 0,
            spelt: 0,
            token_bytes: 0,
            origin,
            at: 0,
        };
        let mut raw = 0;
        if let Some(&(end, Piece::Word(name))) = pieces(text).next().as_ref() {
            for &b in name.as_bytes().iter().take(Self::ROOM) {
                word.push(b);
            }
            word.spelt = word.len;
            word.token_bytes = end;
            raw = end;
        }
        for &b in text.get(raw..).unwrap_or(&[]) {
            if matches!(b, b' ' | b':' | b'\r') || b >= 0x7F || word.len == Self::ROOM {
                break;
            }
            word.push(b.to_ascii_uppercase());
        }
        word
    }

    fn push(&mut self, b: u8) {
        if self.len < Self::ROOM {
            self.chars[self.len] = b;
            self.len += 1;
        }
    }

    /// The characters from `at` on.
    fn rest(&self) -> &[u8] {
        &self.chars[self.at.min(self.len)..self.len]
    }

    /// Whether `text` comes next, taking it if so.
    pub(super) fn take(&mut self, text: &str) -> bool {
        if self.rest().starts_with(text.as_bytes()) {
            self.at += text.len();
            return true;
        }
        false
    }

    /// The condition next, taken; none where no condition is next.
    pub(super) fn condition(&mut self) -> Option<Condition> {
        let cond = condition_of(self.rest())?;
        self.at += 2;
        Some(cond)
    }

    /// The byte next, if any.
    pub(super) fn peek(&self) -> Option<u8> {
        self.rest().first().copied()
    }

    /// The word read past its next byte.
    pub(super) fn bump(&mut self) {
        self.at = (self.at + 1).min(self.len);
    }

    /// Whether `text` comes next.
    pub(super) fn starts_with(&self, text: &str) -> bool {
        self.rest().starts_with(text.as_bytes())
    }

    /// Whether `text` comes next with a condition after it.
    pub(super) fn followed_by_condition(&self, text: &str) -> bool {
        self.starts_with(text) && condition_of(&self.rest()[text.len()..]).is_some()
    }

    /// The offset in the statement's text where reading stopped; none where
    /// it stopped inside the keyword's spelling.
    pub(super) fn text_offset(&self) -> Option<usize> {
        if self.spelt > 0 && self.at < self.spelt {
            return None;
        }
        Some(self.origin + self.token_bytes + (self.at - self.spelt))
    }
}

/// The condition `text` begins with.
fn condition_of(text: &[u8]) -> Option<Condition> {
    let [a, b, ..] = *text else {
        return None;
    };
    Some(match [a, b] {
        [b'E', b'Q'] => Condition::Eq,
        [b'N', b'E'] => Condition::Ne,
        [b'C' | b'H', b'S'] => Condition::Cs,
        [b'C', b'C'] | [b'L', b'O'] => Condition::Cc,
        [b'M', b'I'] => Condition::Mi,
        [b'P', b'L'] => Condition::Pl,
        [b'V', b'S'] => Condition::Vs,
        [b'V', b'C'] => Condition::Vc,
        [b'H', b'I'] => Condition::Hi,
        [b'L', b'S'] => Condition::Ls,
        [b'G', b'E'] => Condition::Ge,
        [b'L', b'T'] => Condition::Lt,
        [b'G', b'T'] => Condition::Gt,
        [b'L', b'E'] => Condition::Le,
        [b'A', b'L'] => Condition::Al,
        [b'N', b'V'] => Condition::Nv,
        _ => return None,
    })
}

/// The stem `word` begins with and the condition after it, `word` read past
/// both; none where no stem begins it.
pub(super) fn matched(word: &mut Word) -> Option<(Stem, Condition)> {
    let rest = word.rest();
    let all = || STEMS.iter();
    let longest = |stems: &mut dyn Iterator<Item = &'static Stem>| {
        stems
            .filter(|stem| rest.starts_with(stem.text.as_bytes()))
            .max_by_key(|stem| stem.text.len())
    };
    let first = longest(&mut all().filter(|stem| LONGEST_FIRST.contains(&stem.text)));
    let conditioned = || {
        longest(&mut all().filter(|stem| {
            stem.conditional
                && rest.starts_with(stem.text.as_bytes())
                && condition_of(&rest[stem.text.len()..]).is_some()
        }))
    };
    if let Some(stem) = first.or_else(conditioned) {
        word.at += stem.text.len();
        let cond = if stem.conditional {
            word.condition().unwrap_or(Condition::Al)
        } else {
            Condition::Al
        };
        return Some((*stem, cond));
    }
    let stem = longest(&mut all())?;
    word.at += stem.text.len();
    Some((*stem, Condition::Al))
}

use Form as F;

/// Every stem.
static STEMS: &[Stem] = &[
    c("AND", F::Data(DataOp::And)),
    c("EOR", F::Data(DataOp::Eor)),
    c("SUB", F::Data(DataOp::Sub)),
    c("RSB", F::Data(DataOp::Rsb)),
    c("ADD", F::Data(DataOp::Add)),
    c("ADC", F::Data(DataOp::Adc)),
    c("SBC", F::Data(DataOp::Sbc)),
    c("RSC", F::Data(DataOp::Rsc)),
    c("ORR", F::Data(DataOp::Orr)),
    c("BIC", F::Data(DataOp::Bic)),
    c("TST", F::Compare(CompareOp::Tst)),
    c("TEQ", F::Compare(CompareOp::Teq)),
    c("CMP", F::Compare(CompareOp::Cmp)),
    c("CMN", F::Compare(CompareOp::Cmn)),
    c("MOV", F::Move(MoveOp::Mov)),
    c("MVN", F::Move(MoveOp::Mvn)),
    c("MOVW", F::Wide(WideOp::Movw)),
    c("MOVT", F::Wide(WideOp::Movt)),
    c("ADR", F::Adr),
    c("MUL", F::Mul),
    c("MLA", F::Mla),
    c("MLS", F::Mls),
    c("UMULL", F::Long(LongOp::Umull)),
    c("UMLAL", F::Long(LongOp::Umlal)),
    c("SMULL", F::Long(LongOp::Smull)),
    c("SMLAL", F::Long(LongOp::Smlal)),
    c("SMLABB", halves(MultiplyOp::Smla, false, false)),
    c("SMLABT", halves(MultiplyOp::Smla, false, true)),
    c("SMLATB", halves(MultiplyOp::Smla, true, false)),
    c("SMLATT", halves(MultiplyOp::Smla, true, true)),
    c("SMLAWB", halves(MultiplyOp::Smlaw, false, false)),
    c("SMLAWT", halves(MultiplyOp::Smlaw, false, true)),
    c("SMULBB", halves(MultiplyOp::Smul, false, false)),
    c("SMULBT", halves(MultiplyOp::Smul, false, true)),
    c("SMULTB", halves(MultiplyOp::Smul, true, false)),
    c("SMULTT", halves(MultiplyOp::Smul, true, true)),
    c("SMULWB", halves(MultiplyOp::Smulw, false, false)),
    c("SMULWT", halves(MultiplyOp::Smulw, false, true)),
    c("SMLALBB", F::LongHalves { x: false, y: false }),
    c("SMLALBT", F::LongHalves { x: false, y: true }),
    c("SMLALTB", F::LongHalves { x: true, y: false }),
    c("SMLALTT", F::LongHalves { x: true, y: true }),
    c("SMLAD", multiply(MultiplyOp::Smlad, false)),
    c("SMLADX", multiply(MultiplyOp::Smlad, true)),
    c("SMLSD", multiply(MultiplyOp::Smlsd, false)),
    c("SMLSDX", multiply(MultiplyOp::Smlsd, true)),
    c("SMUAD", multiply(MultiplyOp::Smuad, false)),
    c("SMUADX", multiply(MultiplyOp::Smuad, true)),
    c("SMUSD", multiply(MultiplyOp::Smusd, false)),
    c("SMUSDX", multiply(MultiplyOp::Smusd, true)),
    c("SMMUL", multiply(MultiplyOp::Smmul, false)),
    c("SMMULR", multiply(MultiplyOp::Smmul, true)),
    c("SMMLA", multiply(MultiplyOp::Smmla, false)),
    c("SMMLAR", multiply(MultiplyOp::Smmla, true)),
    c("SMMLS", multiply(MultiplyOp::Smmls, false)),
    c("SMMLSR", multiply(MultiplyOp::Smmls, true)),
    c("USAD8", multiply(MultiplyOp::Usad8, false)),
    c("USADA8", multiply(MultiplyOp::Usada8, false)),
    c("SDIV", multiply(MultiplyOp::Sdiv, false)),
    c("UDIV", multiply(MultiplyOp::Udiv, false)),
    c(
        "SMLALD",
        F::LongDual {
            op: LongOp::Smlald,
            x: false,
        },
    ),
    c(
        "SMLALDX",
        F::LongDual {
            op: LongOp::Smlald,
            x: true,
        },
    ),
    c(
        "SMLSLD",
        F::LongDual {
            op: LongOp::Smlsld,
            x: false,
        },
    ),
    c(
        "SMLSLDX",
        F::LongDual {
            op: LongOp::Smlsld,
            x: true,
        },
    ),
    c(
        "UMAAL",
        F::LongDual {
            op: LongOp::Umaal,
            x: false,
        },
    ),
    c("SADD16", F::Parallel(Prefix::Signed, Lanes::Add16)),
    c("SASX", F::Parallel(Prefix::Signed, Lanes::Asx)),
    c("SSAX", F::Parallel(Prefix::Signed, Lanes::Sax)),
    c("SSUB16", F::Parallel(Prefix::Signed, Lanes::Sub16)),
    c("SADD8", F::Parallel(Prefix::Signed, Lanes::Add8)),
    c("SSUB8", F::Parallel(Prefix::Signed, Lanes::Sub8)),
    c(
        "QADD16",
        F::Parallel(Prefix::SignedSaturating, Lanes::Add16),
    ),
    c("QASX", F::Parallel(Prefix::SignedSaturating, Lanes::Asx)),
    c("QSAX", F::Parallel(Prefix::SignedSaturating, Lanes::Sax)),
    c(
        "QSUB16",
        F::Parallel(Prefix::SignedSaturating, Lanes::Sub16),
    ),
    c("QADD8", F::Parallel(Prefix::SignedSaturating, Lanes::Add8)),
    c("QSUB8", F::Parallel(Prefix::SignedSaturating, Lanes::Sub8)),
    c("SHADD16", F::Parallel(Prefix::SignedHalving, Lanes::Add16)),
    c("SHASX", F::Parallel(Prefix::SignedHalving, Lanes::Asx)),
    c("SHSAX", F::Parallel(Prefix::SignedHalving, Lanes::Sax)),
    c("SHSUB16", F::Parallel(Prefix::SignedHalving, Lanes::Sub16)),
    c("SHADD8", F::Parallel(Prefix::SignedHalving, Lanes::Add8)),
    c("SHSUB8", F::Parallel(Prefix::SignedHalving, Lanes::Sub8)),
    c("UADD16", F::Parallel(Prefix::Unsigned, Lanes::Add16)),
    c("UASX", F::Parallel(Prefix::Unsigned, Lanes::Asx)),
    c("USAX", F::Parallel(Prefix::Unsigned, Lanes::Sax)),
    c("USUB16", F::Parallel(Prefix::Unsigned, Lanes::Sub16)),
    c("UADD8", F::Parallel(Prefix::Unsigned, Lanes::Add8)),
    c("USUB8", F::Parallel(Prefix::Unsigned, Lanes::Sub8)),
    c(
        "UQADD16",
        F::Parallel(Prefix::UnsignedSaturating, Lanes::Add16),
    ),
    c("UQASX", F::Parallel(Prefix::UnsignedSaturating, Lanes::Asx)),
    c("UQSAX", F::Parallel(Prefix::UnsignedSaturating, Lanes::Sax)),
    c(
        "UQSUB16",
        F::Parallel(Prefix::UnsignedSaturating, Lanes::Sub16),
    ),
    c(
        "UQADD8",
        F::Parallel(Prefix::UnsignedSaturating, Lanes::Add8),
    ),
    c(
        "UQSUB8",
        F::Parallel(Prefix::UnsignedSaturating, Lanes::Sub8),
    ),
    c(
        "UHADD16",
        F::Parallel(Prefix::UnsignedHalving, Lanes::Add16),
    ),
    c("UHASX", F::Parallel(Prefix::UnsignedHalving, Lanes::Asx)),
    c("UHSAX", F::Parallel(Prefix::UnsignedHalving, Lanes::Sax)),
    c(
        "UHSUB16",
        F::Parallel(Prefix::UnsignedHalving, Lanes::Sub16),
    ),
    c("UHADD8", F::Parallel(Prefix::UnsignedHalving, Lanes::Add8)),
    c("UHSUB8", F::Parallel(Prefix::UnsignedHalving, Lanes::Sub8)),
    c("QADD", F::Arithmetic(ArithmeticOp::Qadd)),
    c("QSUB", F::Arithmetic(ArithmeticOp::Qsub)),
    c("QDADD", F::Arithmetic(ArithmeticOp::Qdadd)),
    c("QDSUB", F::Arithmetic(ArithmeticOp::Qdsub)),
    c("SEL", F::Arithmetic(ArithmeticOp::Sel)),
    u("CRC32B", F::Arithmetic(ArithmeticOp::Crc32b)),
    u("CRC32H", F::Arithmetic(ArithmeticOp::Crc32h)),
    u("CRC32W", F::Arithmetic(ArithmeticOp::Crc32w)),
    u("CRC32CB", F::Arithmetic(ArithmeticOp::Crc32cb)),
    u("CRC32CH", F::Arithmetic(ArithmeticOp::Crc32ch)),
    u("CRC32CW", F::Arithmetic(ArithmeticOp::Crc32cw)),
    c("CLZ", F::Unary(UnaryOp::Clz)),
    c("REV", F::Unary(UnaryOp::Rev)),
    c("REV16", F::Unary(UnaryOp::Rev16)),
    c("REVSH", F::Unary(UnaryOp::Revsh)),
    c("RBIT", F::Unary(UnaryOp::Rbit)),
    c("SSAT", F::Saturate(SaturateOp::Ssat)),
    c("USAT", F::Saturate(SaturateOp::Usat)),
    c("SSAT16", F::Saturate(SaturateOp::Ssat16)),
    c("USAT16", F::Saturate(SaturateOp::Usat16)),
    c("SXTAB16", F::Extend(ExtendOp::Sxtab16)),
    c("SXTB16", F::Extend(ExtendOp::Sxtb16)),
    c("SXTAB", F::Extend(ExtendOp::Sxtab)),
    c("SXTB", F::Extend(ExtendOp::Sxtb)),
    c("SXTAH", F::Extend(ExtendOp::Sxtah)),
    c("SXTH", F::Extend(ExtendOp::Sxth)),
    c("UXTAB16", F::Extend(ExtendOp::Uxtab16)),
    c("UXTB16", F::Extend(ExtendOp::Uxtb16)),
    c("UXTAB", F::Extend(ExtendOp::Uxtab)),
    c("UXTB", F::Extend(ExtendOp::Uxtb)),
    c("UXTAH", F::Extend(ExtendOp::Uxtah)),
    c("UXTH", F::Extend(ExtendOp::Uxth)),
    c("PKHBT", F::Pack(PackOp::Pkhbt)),
    c("PKHTB", F::Pack(PackOp::Pkhtb)),
    c("SBFX", F::Bitfield(BitfieldOp::Sbfx)),
    c("UBFX", F::Bitfield(BitfieldOp::Ubfx)),
    c("BFI", F::Bitfield(BitfieldOp::Bfi)),
    c("BFC", F::Bitfield(BitfieldOp::Bfc)),
    c("MRS", F::Mrs),
    c("MSR", F::Msr),
    c("B", F::Branch(BranchOp::B)),
    c("BL", F::Branch(BranchOp::Bl)),
    c("BX", F::Exchange(ExchangeOp::Bx)),
    c("BXJ", F::Exchange(ExchangeOp::Bxj)),
    c("BLX", F::Exchange(ExchangeOp::Blx)),
    c("SWI", F::Call(CallOp::Swi)),
    c("SVC", F::Call(CallOp::Swi)),
    c("BKPT", F::Call(CallOp::Bkpt)),
    u("UDF", F::Call(CallOp::Udf)),
    c("SMC", F::Call(CallOp::Smc)),
    c("SMI", F::Smi),
    c("HVC", F::Call(CallOp::Hvc)),
    c("ERET", F::ExceptionReturn),
    c("YIELD", F::Hint(HintOp::Yield)),
    c("WFE", F::Hint(HintOp::Wfe)),
    c("WFI", F::Hint(HintOp::Wfi)),
    c("SEV", F::Hint(HintOp::Sev)),
    c("SEVL", F::Hint(HintOp::Sevl)),
    c("DBG", F::Hint(HintOp::Dbg)),
    u("NOP", F::Nop),
    u("DMB", F::Barrier(BarrierOp::Dmb)),
    u("DSB", F::Barrier(BarrierOp::Dsb)),
    u("ISB", F::Barrier(BarrierOp::Isb)),
    u("SSBB", F::Barrier(BarrierOp::Ssbb)),
    u("PSSBB", F::Barrier(BarrierOp::Pssbb)),
    u("CLREX", F::ClearExclusive),
    u("CPS", F::ChangeState(0)),
    u("CPSIE", F::ChangeState(2)),
    u("CPSID", F::ChangeState(3)),
    u("SETEND", F::SetEndian),
    u("SRSIA", F::SaveState(BlockMode::IncrementAfter)),
    u("SRSIB", F::SaveState(BlockMode::IncrementBefore)),
    u("SRSDA", F::SaveState(BlockMode::DecrementAfter)),
    u("SRSDB", F::SaveState(BlockMode::DecrementBefore)),
    u("RFEIA", F::ReturnFromException(BlockMode::IncrementAfter)),
    u("RFEIB", F::ReturnFromException(BlockMode::IncrementBefore)),
    u("RFEDA", F::ReturnFromException(BlockMode::DecrementAfter)),
    u("RFEDB", F::ReturnFromException(BlockMode::DecrementBefore)),
    c("LDR", F::Transfer { load: true }),
    c("STR", F::Transfer { load: false }),
    u("PLD", F::Preload(PreloadOp::Pld)),
    u("PLDW", F::Preload(PreloadOp::Pldw)),
    u("PLI", F::Preload(PreloadOp::Pli)),
    c("LDM", F::Block { load: true }),
    c("STM", F::Block { load: false }),
    c("POP", F::Stack { load: true }),
    c("PUSH", F::Stack { load: false }),
    c("SWP", F::Swap),
    c("LDREX", F::Exclusive(ExclusiveOp::Ldrex)),
    c("STREX", F::Exclusive(ExclusiveOp::Strex)),
    c("LDREXB", F::Exclusive(ExclusiveOp::Ldrexb)),
    c("STREXB", F::Exclusive(ExclusiveOp::Strexb)),
    c("LDREXH", F::Exclusive(ExclusiveOp::Ldrexh)),
    c("STREXH", F::Exclusive(ExclusiveOp::Strexh)),
    c("LDREXD", F::Exclusive(ExclusiveOp::Ldrexd)),
    c("STREXD", F::Exclusive(ExclusiveOp::Strexd)),
    c("LDAEX", F::Exclusive(ExclusiveOp::Ldaex)),
    c("LDAEXB", F::Exclusive(ExclusiveOp::Ldaexb)),
    c("LDAEXH", F::Exclusive(ExclusiveOp::Ldaexh)),
    c("LDAEXD", F::Exclusive(ExclusiveOp::Ldaexd)),
    c("STLEX", F::Exclusive(ExclusiveOp::Stlex)),
    c("STLEXB", F::Exclusive(ExclusiveOp::Stlexb)),
    c("STLEXH", F::Exclusive(ExclusiveOp::Stlexh)),
    c("STLEXD", F::Exclusive(ExclusiveOp::Stlexd)),
    c("LDA", F::Ordered { load: true }),
    c("STL", F::Ordered { load: false }),
    c("CDP", F::Coprocessor(CoprocessorOp::Cdp)),
    u("CDP2", F::Coprocessor(CoprocessorOp::Cdp2)),
    c("MCR", F::CoRegister(CoRegisterOp::Mcr)),
    c("MRC", F::CoRegister(CoRegisterOp::Mrc)),
    u("MCR2", F::CoRegister(CoRegisterOp::Mcr2)),
    u("MRC2", F::CoRegister(CoRegisterOp::Mrc2)),
    c("MCRR", F::CoRegisters(CoRegistersOp::Mcrr)),
    c("MRRC", F::CoRegisters(CoRegistersOp::Mrrc)),
    c("LDC", F::CoTransfer(CoTransferOp::Ldc)),
    c("STC", F::CoTransfer(CoTransferOp::Stc)),
    u("LDC2", F::CoTransfer(CoTransferOp::Ldc2)),
    u("STC2", F::CoTransfer(CoTransferOp::Stc2)),
    c("ADF", F::FpaDyadic(FpaDyadicOp::Adf)),
    c("MUF", F::FpaDyadic(FpaDyadicOp::Muf)),
    c("SUF", F::FpaDyadic(FpaDyadicOp::Suf)),
    c("RSF", F::FpaDyadic(FpaDyadicOp::Rsf)),
    c("DVF", F::FpaDyadic(FpaDyadicOp::Dvf)),
    c("RDF", F::FpaDyadic(FpaDyadicOp::Rdf)),
    c("POW", F::FpaDyadic(FpaDyadicOp::Pow)),
    c("RPW", F::FpaDyadic(FpaDyadicOp::Rpw)),
    c("RMF", F::FpaDyadic(FpaDyadicOp::Rmf)),
    c("FML", F::FpaDyadic(FpaDyadicOp::Fml)),
    c("FDV", F::FpaDyadic(FpaDyadicOp::Fdv)),
    c("FRD", F::FpaDyadic(FpaDyadicOp::Frd)),
    c("POL", F::FpaDyadic(FpaDyadicOp::Pol)),
    c("MVF", F::FpaMonadic(FpaMonadicOp::Mvf)),
    c("MNF", F::FpaMonadic(FpaMonadicOp::Mnf)),
    c("ABS", F::FpaMonadic(FpaMonadicOp::Abs)),
    c("RND", F::FpaMonadic(FpaMonadicOp::Rnd)),
    c("SQT", F::FpaMonadic(FpaMonadicOp::Sqt)),
    c("LOG", F::FpaMonadic(FpaMonadicOp::Log)),
    c("LGN", F::FpaMonadic(FpaMonadicOp::Lgn)),
    c("EXP", F::FpaMonadic(FpaMonadicOp::Exp)),
    c("SIN", F::FpaMonadic(FpaMonadicOp::Sin)),
    c("COS", F::FpaMonadic(FpaMonadicOp::Cos)),
    c("TAN", F::FpaMonadic(FpaMonadicOp::Tan)),
    c("ASN", F::FpaMonadic(FpaMonadicOp::Asn)),
    c("ACS", F::FpaMonadic(FpaMonadicOp::Acs)),
    c("ATN", F::FpaMonadic(FpaMonadicOp::Atn)),
    c("URD", F::FpaMonadic(FpaMonadicOp::Urd)),
    c("NRM", F::FpaMonadic(FpaMonadicOp::Nrm)),
    c("FLT", F::Flt),
    c("FIX", F::Fix),
    c("WFS", F::FpaStatus { write: true }),
    c("RFS", F::FpaStatus { write: false }),
    c("WFC", F::FpaControl { write: true }),
    c("RFC", F::FpaControl { write: false }),
    c("CMF", F::FpaCompare(FpaCompareOp::Cmf)),
    c("CNF", F::FpaCompare(FpaCompareOp::Cnf)),
    c("CMFE", F::FpaCompare(FpaCompareOp::Cmfe)),
    c("CNFE", F::FpaCompare(FpaCompareOp::Cnfe)),
    c("LDF", F::FpaTransfer(FpaTransferOp::Ldf)),
    c("STF", F::FpaTransfer(FpaTransferOp::Stf)),
    c("LFM", F::FpaMultiple(FpaMultipleOp::Lfm)),
    c("SFM", F::FpaMultiple(FpaMultipleOp::Sfm)),
    u("EQUB", F::Equ(Equ::Byte)),
    u("EQUW", F::Equ(Equ::Half)),
    u("EQUD", F::Equ(Equ::Word)),
    u("EQUS", F::Equ(Equ::Text)),
    u("EQUFS", F::Equ(Equ::Single)),
    u("EQUFD", F::Equ(Equ::Double)),
    u("EQUFE", F::Equ(Equ::Extended)),
    u("DCB", F::Equ(Equ::ByteOrText)),
    u("=", F::Equ(Equ::ByteOrText)),
    u("DCW", F::Equ(Equ::Half)),
    u("DCD", F::Equ(Equ::Word)),
    u("DCFS", F::Equ(Equ::Single)),
    u("DCFD", F::Equ(Equ::Double)),
    u("DCFE", F::Equ(Equ::Extended)),
    u("ALIGN", F::Align),
    u("OPT", F::Opt),
];

const fn halves(op: MultiplyOp, x: bool, y: bool) -> Form {
    F::Halves { op, x, y }
}

const fn multiply(op: MultiplyOp, x: bool) -> Form {
    F::Multiply { op, x }
}
