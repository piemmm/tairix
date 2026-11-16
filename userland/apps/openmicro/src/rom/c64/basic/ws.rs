//! BASIC's workspace where the Programmer's Reference Guide's memory map
//! documents it, under its names there.

/// Zero page, `$03–$8F`.
pub mod zp {
    use crate::machine::c64::zero_page;

    /// ADRAY1: the vector to the routine that makes FAC a signed integer
    /// in A (high) and Y.
    pub const ADRAY1: u16 = 0x03;
    /// ADRAY2: the vector to the routine that makes A (high) and Y a real
    /// in FAC.
    pub const ADRAY2: u16 = 0x05;
    /// CHARAC: the character a scan stops at.
    pub const CHARAC: u16 = 0x07;
    /// ENDCHR: the other character a scan stops at.
    pub const ENDCHR: u16 = 0x08;
    /// TRMPOS: the column TAB and SPC count from.
    pub const TRMPOS: u16 = 0x09;
    /// VERCK: 0 to load, 1 to verify.
    pub const VERCK: u16 = 0x0A;
    /// COUNT: the crunch's offset into the input buffer; the subscripts of
    /// an array reference.
    pub const COUNT: u16 = 0x0B;
    /// DIMFLG: a DIM is creating the array.
    pub const DIMFLG: u16 = 0x0C;
    /// VALTYP: `$FF` for a string, 0 for a number.
    pub const VALTYP: u16 = 0x0D;
    /// INTFLG: `$80` for an integer, 0 for a real.
    pub const INTFLG: u16 = 0x0E;
    /// GARBFL: a DATA statement or quotes while crunching; LIST's quotes;
    /// a collection has run.
    pub const GARBFL: u16 = 0x0F;
    /// SUBFLG: subscripts and FN arguments are not allowed.
    pub const SUBFLG: u16 = 0x10;
    /// INPFLG: 0 for INPUT, `$40` for GET, `$98` for READ.
    pub const INPFLG: u16 = 0x11;
    /// TANSGN: the sign TAN's cosine takes; a comparison's operators.
    pub const TANSGN: u16 = 0x12;
    /// CHANNL: the file INPUT# and PRINT# use, 0 for none.
    pub const CHANNL: u16 = 0x13;
    /// LINNUM: an integer: a line number, an address.
    pub const LINNUM: u16 = 0x14;
    /// TEMPPT: the next free descriptor on the temporary string stack.
    pub const TEMPPT: u16 = 0x16;
    /// LASTPT: the last descriptor pushed.
    pub const LASTPT: u16 = 0x17;
    /// TEMPST: the temporary string stack, three descriptors.
    pub const TEMPST: u16 = 0x19;
    /// INDEX: two pointers.
    pub const INDEX: u16 = 0x22;
    /// INDEX's second pointer.
    pub const INDEX2: u16 = 0x24;
    /// RESHO: a product's five bytes as they accumulate.
    pub const RESHO: u16 = 0x26;
    /// TXTTAB: the program's start.
    pub const TXTTAB: u16 = zero_page::TXTTAB;
    /// VARTAB: the variables' start.
    pub const VARTAB: u16 = zero_page::VARTAB;
    /// ARYTAB: the arrays' start.
    pub const ARYTAB: u16 = zero_page::ARYTAB;
    /// STREND: past the arrays.
    pub const STREND: u16 = zero_page::STREND;
    /// FRETOP: the bottom of the strings.
    pub const FRETOP: u16 = 0x33;
    /// FRESPC: the string being made.
    pub const FRESPC: u16 = 0x35;
    /// MEMSIZ: the top of BASIC's memory.
    pub const MEMSIZ: u16 = 0x37;
    /// CURLIN: the line running, `$FFxx` in direct mode.
    pub const CURLIN: u16 = 0x39;
    /// OLDLIN: the line CONT resumes.
    pub const OLDLIN: u16 = 0x3B;
    /// OLDTXT: the statement CONT resumes.
    pub const OLDTXT: u16 = 0x3D;
    /// DATLIN: the line READ is in.
    pub const DATLIN: u16 = 0x3F;
    /// DATPTR: where READ reads next.
    pub const DATPTR: u16 = 0x41;
    /// INPPTR: where INPUT reads next.
    pub const INPPTR: u16 = 0x43;
    /// VARNAM: the variable's name.
    pub const VARNAM: u16 = 0x45;
    /// VARPNT: the variable's value.
    pub const VARPNT: u16 = 0x47;
    /// FORPNT: the FOR loop's variable; the variable LET assigns.
    pub const FORPNT: u16 = 0x49;
    /// OPPTR: the operator being applied.
    pub const OPPTR: u16 = 0x4B;
    /// OPMASK: the relational operators met.
    pub const OPMASK: u16 = 0x4D;
    /// DEFPNT: the FN being called.
    pub const DEFPNT: u16 = 0x4E;
    /// DSCPNT: a string's descriptor.
    pub const DSCPNT: u16 = 0x50;
    /// FOUR6: a descriptor's size as a collection walks the strings.
    pub const FOUR6: u16 = 0x53;
    /// JMPER: a `JMP` to the function being applied.
    pub const JMPER: u16 = 0x54;
    /// TEMPF1: a real's five bytes.
    pub const TEMPF1: u16 = 0x57;
    /// TEMPF2: another's.
    pub const TEMPF2: u16 = 0x5C;
    /// FACEXP: FAC's exponent.
    pub const FACEXP: u16 = 0x61;
    /// FACHO: FAC's mantissa, its most significant byte first.
    pub const FACHO: u16 = 0x62;
    /// FAC's mantissa's least significant byte.
    pub const FACLO: u16 = 0x65;
    /// FACSGN: FAC's sign, in bit 7.
    pub const FACSGN: u16 = 0x66;
    /// SGNFLG: the terms a polynomial has left.
    pub const SGNFLG: u16 = 0x67;
    /// BITS: a byte shifted out of a mantissa.
    pub const BITS: u16 = 0x68;
    /// ARGEXP: ARG's exponent.
    pub const ARGEXP: u16 = 0x69;
    /// ARGHO: ARG's mantissa.
    pub const ARGHO: u16 = 0x6A;
    /// ARG's mantissa's least significant byte.
    pub const ARGLO: u16 = 0x6D;
    /// ARGSGN: ARG's sign.
    pub const ARGSGN: u16 = 0x6E;
    /// ARISGN: whether FAC's and ARG's signs differ, in bit 7.
    pub const ARISGN: u16 = 0x6F;
    /// FACOV: the byte beneath FAC's mantissa.
    pub const FACOV: u16 = 0x70;
    /// FBUFPT: a polynomial's coefficients.
    pub const FBUFPT: u16 = 0x71;
    /// CHRGET: the routine that steps TXTPTR to the next character.
    pub const CHRGET: u16 = 0x73;
    /// CHRGOT: the routine that reads the character at TXTPTR.
    pub const CHRGOT: u16 = 0x79;
    /// TXTPTR: the program text being read.
    pub const TXTPTR: u16 = 0x7A;
    /// RNDX: RND's seed.
    pub const RNDX: u16 = 0x8B;
    /// The first byte of the number STR$ builds, which runs on into page 1.
    pub const NUMBER_BUFFER: u16 = 0xFF;
}

/// Pages 1 to 3.
pub mod page {
    /// The input buffer, 89 bytes.
    pub const BUF: u16 = 0x0200;
    /// The input buffer's size.
    pub const BUF_BYTES: u8 = 89;
    /// SAREG: the A SYS passes and returns.
    pub const SAREG: u16 = 0x030C;
    /// SXREG: the X.
    pub const SXREG: u16 = 0x030D;
    /// SYREG: the Y.
    pub const SYREG: u16 = 0x030E;
    /// SPREG: the status register.
    pub const SPREG: u16 = 0x030F;
    /// USRPOK: the `JMP` USR makes, its address in USRADD.
    pub const USRPOK: u16 = 0x0310;
}

/// BASIC's vectors at `$0300`, in their order.
pub mod vector {
    /// IERROR: an error's report.
    pub const IERROR: u16 = 0x0300;
    /// IMAIN: the main loop.
    pub const IMAIN: u16 = 0x0302;
    /// ICRNCH: the tokeniser.
    pub const ICRNCH: u16 = 0x0304;
    /// IQPLOP: LIST's printing of a token.
    pub const IQPLOP: u16 = 0x0306;
    /// IGONE: the dispatch of a statement.
    pub const IGONE: u16 = 0x0308;
    /// IEVAL: the evaluation of an operand.
    pub const IEVAL: u16 = 0x030A;
    /// The vectors' bytes.
    pub const BYTES: u8 = 12;
}
