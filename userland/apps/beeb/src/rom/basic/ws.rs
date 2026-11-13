//! BASIC's workspace, where BASIC II keeps it: what programs and machine code
//! read directly, at the addresses they read.

/// Zero page.
pub mod zp {
    /// LOMEM, where the variables start.
    pub const LOMEM: u16 = 0x00;
    /// The top of the variables, where the heap next grows.
    pub const VARTOP: u16 = 0x02;
    /// The BASIC stack's lowest byte; it grows down from HIMEM.
    pub const STACK: u16 = 0x04;
    /// HIMEM.
    pub const HIMEM: u16 = 0x06;
    /// The line the last error was at.
    pub const ERL: u16 = 0x08;
    /// The statement pointer: an offset from a base.
    pub const OFFSET_A: u16 = 0x0A;
    /// The statement pointer's base.
    pub const TEXT_A: u16 = 0x0B;
    /// RND's seed.
    pub const SEED: u16 = 0x0D;
    /// The seed's bytes.
    pub const SEED_LEN: u16 = 5;
    /// TOP, the program's end.
    pub const TOP: u16 = 0x12;
    /// The print field's width, and whether numbers print in hex.
    pub const FIELD: u16 = 0x14;
    /// Whether numbers print in hex.
    pub const HEX: u16 = 0x15;
    /// ON ERROR's statement, or the default.
    pub const ON_ERROR: u16 = 0x16;
    /// PAGE's high byte.
    pub const PAGE: u16 = 0x18;
    /// A second text pointer: a definition's, or an item's being read.
    pub const TEXT_B: u16 = 0x19;
    /// Where READ takes its next item.
    pub const DATA: u16 = 0x1C;
    /// COUNT: characters printed since the last new line.
    pub const COUNT: u16 = 0x1E;
    /// LISTO's options.
    pub const LISTO: u16 = 0x1F;
    /// Whether TRACE is on, and the highest line it reports.
    pub const TRACE: u16 = 0x20;
    /// TRACE's highest line.
    pub const TRACE_LIMIT: u16 = 0x21;
    /// WIDTH less one; &FF for none.
    pub const WIDTH: u16 = 0x23;
    /// The depths of the REPEAT, GOSUB and FOR stacks.
    pub const REPEATS: u16 = 0x24;
    /// GOSUB's depth.
    pub const GOSUBS: u16 = 0x25;
    /// FOR's depth, in bytes.
    pub const FORS: u16 = 0x26;
    /// The last expression's type.
    pub const TYPE: u16 = 0x27;
    /// The assembler's OPT.
    pub const OPT: u16 = 0x28;
    /// The integer accumulator, least significant first.
    pub const IAC: u16 = 0x2A;
    /// The first real accumulator: sign, overflow, exponent, four mantissa
    /// bytes, and a rounding byte.
    pub const FAC: u16 = 0x2E;
    /// The string accumulator's length; its text is at `page::STRING`.
    pub const STRING_LENGTH: u16 = 0x36;
    /// The location a load or store works on, and its `place` type.
    pub const VAR: u16 = 0x37;
    pub const VTYPE: u16 = 0x39;
    /// Whether an absent variable is P%: set only while the assembler reads
    /// an operand with OPT's errors off.
    pub const ASM_QUIET: u16 = 0x3A;
    /// The second real accumulator, laid out as the first.
    pub const FAC2: u16 = 0x3B;
    /// The scratch, &43–&6F. The editor's routines and the interpreter's
    /// never run at once, so each lays out its own; only the error report
    /// runs in both, and it ends whatever it interrupts.
    pub const SCRATCH: u16 = 0x43;
    /// The real operations' multiplier or divisor.
    pub const FP_WORK: u16 = SCRATCH;
    /// Decimal reading and writing.
    pub const DECIMAL: u16 = FP_WORK + 4;
    /// The functions' own, shared with decimal reading and writing, which
    /// never runs inside one.
    pub const TRANS: u16 = DECIMAL;
    /// The integer left operand, and integer multiplication's and
    /// division's work.
    pub const IAC2: u16 = DECIMAL + 8;
    pub const INT_WORK: u16 = IAC2 + 4;
    /// The character after an operand while two are combined.
    pub const OP: u16 = INT_WORK + 4;
    /// Name lookup and string moves.
    pub const RUN: u16 = OP + 1;
    /// The statements' own, to &6F.
    pub const STATEMENT: u16 = RUN + 10;
}

/// The types of a location, as CALL's parameter block gives them.
pub mod place {
    pub const BYTE: u8 = 0x00;
    pub const INTEGER: u8 = 0x04;
    pub const REAL: u8 = 0x05;
    /// A string at an address, ended by a CR.
    pub const FIXED_STRING: u8 = 0x80;
    pub const STRING: u8 = 0x81;
}

/// The value types &27 holds.
pub mod kind {
    pub const STRING: u8 = 0x00;
    pub const INTEGER: u8 = 0x40;
    pub const REAL: u8 = 0xFF;
}

/// Pages 4 to 7.
pub mod page {
    /// `@%`, then A% to Z%, four bytes each.
    pub const RESIDENT: u16 = 0x0400;
    /// The resident integer named by `letter` and `%`, `@` to `Z`.
    pub const fn resident(letter: u8) -> u16 {
        RESIDENT + 4 * (letter - b'@') as u16
    }
    /// Each initial's variable list, from `@`.
    pub const CATALOGUE: u16 = 0x0480;
    /// FN's list.
    pub const FN_LIST: u16 = 0x04F6;
    /// PROC's list.
    pub const PROC_LIST: u16 = 0x04F8;
    /// The control stacks.
    pub const STACKS: u16 = 0x0500;
    /// The string accumulator, and CALL's parameter block.
    pub const STRING: u16 = 0x0600;
    /// The input buffer a typed line is read and tokenised in.
    pub const INPUT: u16 = 0x0700;
    /// Working reals the functions keep, &46C–&47F.
    pub const TEMPS: u16 = 0x046C;
}

/// `@%` as BASIC II starts: general format, nine figures, ten wide.
pub const AT_DEFAULT: u32 = 0x0000_090A;
