//! The NMOS 6502 instruction set: what each of the 256 opcodes is.
//!
//! [`OPCODES`] and the CPU's dispatch are generated from one table, so the
//! disassembler, the ROM builder's encoder and the CPU cannot disagree about an
//! opcode. Each row names a mnemonic and a mode that are both an enum variant
//! here and a marker type in [`super::ops`] / [`super::modes`]; a row pairing a
//! mnemonic with a mode its semantics cannot take does not compile.

use super::modes::{
    Absolute, AbsoluteX, AbsoluteY, Accumulator, Immediate, Implied, IndexedIndirect, Indirect,
    IndirectIndexed, Relative, ZeroPage, ZeroPageX, ZeroPageY,
};
use super::ops::{self, Execute};
use super::{Bus, Cpu};

/// An operation, documented or not.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[expect(missing_docs, reason = "each variant is the mnemonic its name spells")]
pub enum Mnemonic {
    Adc,
    And,
    Asl,
    Bcc,
    Bcs,
    Beq,
    Bit,
    Bmi,
    Bne,
    Bpl,
    Brk,
    Bvc,
    Bvs,
    Clc,
    Cld,
    Cli,
    Clv,
    Cmp,
    Cpx,
    Cpy,
    Dec,
    Dex,
    Dey,
    Eor,
    Inc,
    Inx,
    Iny,
    Jmp,
    Jsr,
    Lda,
    Ldx,
    Ldy,
    Lsr,
    Nop,
    Ora,
    Pha,
    Php,
    Pla,
    Plp,
    Rol,
    Ror,
    Rti,
    Rts,
    Sbc,
    Sec,
    Sed,
    Sei,
    Sta,
    Stx,
    Sty,
    Tax,
    Tay,
    Tsx,
    Txa,
    Txs,
    Tya,
    Alr,
    Anc,
    Ane,
    Arr,
    Dcp,
    Isc,
    Jam,
    Las,
    Lax,
    Lxa,
    Rla,
    Rra,
    Sax,
    Sbx,
    Sha,
    Shx,
    Shy,
    Slo,
    Sre,
    Tas,
}

impl Mnemonic {
    /// The mnemonic as an assembler spells it.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Adc => "ADC",
            Self::And => "AND",
            Self::Asl => "ASL",
            Self::Bcc => "BCC",
            Self::Bcs => "BCS",
            Self::Beq => "BEQ",
            Self::Bit => "BIT",
            Self::Bmi => "BMI",
            Self::Bne => "BNE",
            Self::Bpl => "BPL",
            Self::Brk => "BRK",
            Self::Bvc => "BVC",
            Self::Bvs => "BVS",
            Self::Clc => "CLC",
            Self::Cld => "CLD",
            Self::Cli => "CLI",
            Self::Clv => "CLV",
            Self::Cmp => "CMP",
            Self::Cpx => "CPX",
            Self::Cpy => "CPY",
            Self::Dec => "DEC",
            Self::Dex => "DEX",
            Self::Dey => "DEY",
            Self::Eor => "EOR",
            Self::Inc => "INC",
            Self::Inx => "INX",
            Self::Iny => "INY",
            Self::Jmp => "JMP",
            Self::Jsr => "JSR",
            Self::Lda => "LDA",
            Self::Ldx => "LDX",
            Self::Ldy => "LDY",
            Self::Lsr => "LSR",
            Self::Nop => "NOP",
            Self::Ora => "ORA",
            Self::Pha => "PHA",
            Self::Php => "PHP",
            Self::Pla => "PLA",
            Self::Plp => "PLP",
            Self::Rol => "ROL",
            Self::Ror => "ROR",
            Self::Rti => "RTI",
            Self::Rts => "RTS",
            Self::Sbc => "SBC",
            Self::Sec => "SEC",
            Self::Sed => "SED",
            Self::Sei => "SEI",
            Self::Sta => "STA",
            Self::Stx => "STX",
            Self::Sty => "STY",
            Self::Tax => "TAX",
            Self::Tay => "TAY",
            Self::Tsx => "TSX",
            Self::Txa => "TXA",
            Self::Txs => "TXS",
            Self::Tya => "TYA",
            Self::Alr => "ALR",
            Self::Anc => "ANC",
            Self::Ane => "ANE",
            Self::Arr => "ARR",
            Self::Dcp => "DCP",
            Self::Isc => "ISC",
            Self::Jam => "JAM",
            Self::Las => "LAS",
            Self::Lax => "LAX",
            Self::Lxa => "LXA",
            Self::Rla => "RLA",
            Self::Rra => "RRA",
            Self::Sax => "SAX",
            Self::Sbx => "SBX",
            Self::Sha => "SHA",
            Self::Shx => "SHX",
            Self::Shy => "SHY",
            Self::Slo => "SLO",
            Self::Sre => "SRE",
            Self::Tas => "TAS",
        }
    }
}

/// How an instruction finds its operand.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Mode {
    /// No operand.
    Implied,
    /// The accumulator: `ASL A`.
    Accumulator,
    /// The byte after the opcode: `LDA #n`.
    Immediate,
    /// A zero-page address: `LDA zp`.
    ZeroPage,
    /// A zero-page address plus X, wrapping in page zero.
    ZeroPageX,
    /// A zero-page address plus Y, wrapping in page zero.
    ZeroPageY,
    /// A 16-bit address.
    Absolute,
    /// A 16-bit address plus X.
    AbsoluteX,
    /// A 16-bit address plus Y.
    AbsoluteY,
    /// `JMP (abs)`: a 16-bit pointer, wrapping within its page.
    Indirect,
    /// `(zp,X)`: a pointer at a zero-page address plus X.
    IndexedIndirect,
    /// `(zp),Y`: a pointer at a zero-page address, plus Y.
    IndirectIndexed,
    /// A branch's signed displacement.
    Relative,
}

impl Mode {
    /// How many operand bytes follow the opcode.
    #[must_use]
    pub const fn operand_bytes(self) -> u16 {
        match self {
            Self::Implied | Self::Accumulator => 0,
            Self::Immediate
            | Self::ZeroPage
            | Self::ZeroPageX
            | Self::ZeroPageY
            | Self::IndexedIndirect
            | Self::IndirectIndexed
            | Self::Relative => 1,
            Self::Absolute | Self::AbsoluteX | Self::AbsoluteY | Self::Indirect => 2,
        }
    }
}

/// One opcode's identity.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Opcode {
    /// What it does.
    pub mnemonic: Mnemonic,
    /// How it addresses its operand.
    pub mode: Mode,
    /// Whether the manufacturer documents it.
    pub documented: bool,
}

impl Opcode {
    /// The instruction's size in bytes, opcode included.
    #[must_use]
    pub const fn size(self) -> u16 {
        1 + self.mode.operand_bytes()
    }
}

/// Generates [`OPCODES`] and the dispatch from one ordered list of rows.
macro_rules! instruction_set {
    ($($code:literal $mnemonic:ident $mode:ident $documented:literal;)*) => {
        /// Every opcode, indexed by its byte.
        pub const OPCODES: [Opcode; 256] = {
            let placeholder = Opcode {
                mnemonic: Mnemonic::Jam,
                mode: Mode::Implied,
                documented: false,
            };
            let mut table = [placeholder; 256];
            let mut seen = [false; 256];
            $(
                assert!(!seen[$code], "an opcode is listed twice");
                seen[$code] = true;
                table[$code] = Opcode {
                    mnemonic: Mnemonic::$mnemonic,
                    mode: Mode::$mode,
                    documented: $documented,
                };
            )*
            let mut code = 0;
            while code < 256 {
                assert!(seen[code], "an opcode is missing");
                code += 1;
            }
            table
        };

        /// Runs the semantics of a fetched opcode.
        pub(super) fn execute<B: Bus>(cpu: &mut Cpu, bus: &mut B, opcode: u8) {
            match opcode {
                $($code => <ops::$mnemonic as Execute<$mode>>::execute(cpu, bus),)*
            }
        }
    };
}

instruction_set! {
    0x00 Brk Implied true;          0x01 Ora IndexedIndirect true;
    0x02 Jam Implied false;         0x03 Slo IndexedIndirect false;
    0x04 Nop ZeroPage false;        0x05 Ora ZeroPage true;
    0x06 Asl ZeroPage true;         0x07 Slo ZeroPage false;
    0x08 Php Implied true;          0x09 Ora Immediate true;
    0x0A Asl Accumulator true;      0x0B Anc Immediate false;
    0x0C Nop Absolute false;        0x0D Ora Absolute true;
    0x0E Asl Absolute true;         0x0F Slo Absolute false;
    0x10 Bpl Relative true;         0x11 Ora IndirectIndexed true;
    0x12 Jam Implied false;         0x13 Slo IndirectIndexed false;
    0x14 Nop ZeroPageX false;       0x15 Ora ZeroPageX true;
    0x16 Asl ZeroPageX true;        0x17 Slo ZeroPageX false;
    0x18 Clc Implied true;          0x19 Ora AbsoluteY true;
    0x1A Nop Implied false;         0x1B Slo AbsoluteY false;
    0x1C Nop AbsoluteX false;       0x1D Ora AbsoluteX true;
    0x1E Asl AbsoluteX true;        0x1F Slo AbsoluteX false;
    0x20 Jsr Absolute true;         0x21 And IndexedIndirect true;
    0x22 Jam Implied false;         0x23 Rla IndexedIndirect false;
    0x24 Bit ZeroPage true;         0x25 And ZeroPage true;
    0x26 Rol ZeroPage true;         0x27 Rla ZeroPage false;
    0x28 Plp Implied true;          0x29 And Immediate true;
    0x2A Rol Accumulator true;      0x2B Anc Immediate false;
    0x2C Bit Absolute true;         0x2D And Absolute true;
    0x2E Rol Absolute true;         0x2F Rla Absolute false;
    0x30 Bmi Relative true;         0x31 And IndirectIndexed true;
    0x32 Jam Implied false;         0x33 Rla IndirectIndexed false;
    0x34 Nop ZeroPageX false;       0x35 And ZeroPageX true;
    0x36 Rol ZeroPageX true;        0x37 Rla ZeroPageX false;
    0x38 Sec Implied true;          0x39 And AbsoluteY true;
    0x3A Nop Implied false;         0x3B Rla AbsoluteY false;
    0x3C Nop AbsoluteX false;       0x3D And AbsoluteX true;
    0x3E Rol AbsoluteX true;        0x3F Rla AbsoluteX false;
    0x40 Rti Implied true;          0x41 Eor IndexedIndirect true;
    0x42 Jam Implied false;         0x43 Sre IndexedIndirect false;
    0x44 Nop ZeroPage false;        0x45 Eor ZeroPage true;
    0x46 Lsr ZeroPage true;         0x47 Sre ZeroPage false;
    0x48 Pha Implied true;          0x49 Eor Immediate true;
    0x4A Lsr Accumulator true;      0x4B Alr Immediate false;
    0x4C Jmp Absolute true;         0x4D Eor Absolute true;
    0x4E Lsr Absolute true;         0x4F Sre Absolute false;
    0x50 Bvc Relative true;         0x51 Eor IndirectIndexed true;
    0x52 Jam Implied false;         0x53 Sre IndirectIndexed false;
    0x54 Nop ZeroPageX false;       0x55 Eor ZeroPageX true;
    0x56 Lsr ZeroPageX true;        0x57 Sre ZeroPageX false;
    0x58 Cli Implied true;          0x59 Eor AbsoluteY true;
    0x5A Nop Implied false;         0x5B Sre AbsoluteY false;
    0x5C Nop AbsoluteX false;       0x5D Eor AbsoluteX true;
    0x5E Lsr AbsoluteX true;        0x5F Sre AbsoluteX false;
    0x60 Rts Implied true;          0x61 Adc IndexedIndirect true;
    0x62 Jam Implied false;         0x63 Rra IndexedIndirect false;
    0x64 Nop ZeroPage false;        0x65 Adc ZeroPage true;
    0x66 Ror ZeroPage true;         0x67 Rra ZeroPage false;
    0x68 Pla Implied true;          0x69 Adc Immediate true;
    0x6A Ror Accumulator true;      0x6B Arr Immediate false;
    0x6C Jmp Indirect true;         0x6D Adc Absolute true;
    0x6E Ror Absolute true;         0x6F Rra Absolute false;
    0x70 Bvs Relative true;         0x71 Adc IndirectIndexed true;
    0x72 Jam Implied false;         0x73 Rra IndirectIndexed false;
    0x74 Nop ZeroPageX false;       0x75 Adc ZeroPageX true;
    0x76 Ror ZeroPageX true;        0x77 Rra ZeroPageX false;
    0x78 Sei Implied true;          0x79 Adc AbsoluteY true;
    0x7A Nop Implied false;         0x7B Rra AbsoluteY false;
    0x7C Nop AbsoluteX false;       0x7D Adc AbsoluteX true;
    0x7E Ror AbsoluteX true;        0x7F Rra AbsoluteX false;
    0x80 Nop Immediate false;       0x81 Sta IndexedIndirect true;
    0x82 Nop Immediate false;       0x83 Sax IndexedIndirect false;
    0x84 Sty ZeroPage true;         0x85 Sta ZeroPage true;
    0x86 Stx ZeroPage true;         0x87 Sax ZeroPage false;
    0x88 Dey Implied true;          0x89 Nop Immediate false;
    0x8A Txa Implied true;          0x8B Ane Immediate false;
    0x8C Sty Absolute true;         0x8D Sta Absolute true;
    0x8E Stx Absolute true;         0x8F Sax Absolute false;
    0x90 Bcc Relative true;         0x91 Sta IndirectIndexed true;
    0x92 Jam Implied false;         0x93 Sha IndirectIndexed false;
    0x94 Sty ZeroPageX true;        0x95 Sta ZeroPageX true;
    0x96 Stx ZeroPageY true;        0x97 Sax ZeroPageY false;
    0x98 Tya Implied true;          0x99 Sta AbsoluteY true;
    0x9A Txs Implied true;          0x9B Tas AbsoluteY false;
    0x9C Shy AbsoluteX false;       0x9D Sta AbsoluteX true;
    0x9E Shx AbsoluteY false;       0x9F Sha AbsoluteY false;
    0xA0 Ldy Immediate true;        0xA1 Lda IndexedIndirect true;
    0xA2 Ldx Immediate true;        0xA3 Lax IndexedIndirect false;
    0xA4 Ldy ZeroPage true;         0xA5 Lda ZeroPage true;
    0xA6 Ldx ZeroPage true;         0xA7 Lax ZeroPage false;
    0xA8 Tay Implied true;          0xA9 Lda Immediate true;
    0xAA Tax Implied true;          0xAB Lxa Immediate false;
    0xAC Ldy Absolute true;         0xAD Lda Absolute true;
    0xAE Ldx Absolute true;         0xAF Lax Absolute false;
    0xB0 Bcs Relative true;         0xB1 Lda IndirectIndexed true;
    0xB2 Jam Implied false;         0xB3 Lax IndirectIndexed false;
    0xB4 Ldy ZeroPageX true;        0xB5 Lda ZeroPageX true;
    0xB6 Ldx ZeroPageY true;        0xB7 Lax ZeroPageY false;
    0xB8 Clv Implied true;          0xB9 Lda AbsoluteY true;
    0xBA Tsx Implied true;          0xBB Las AbsoluteY false;
    0xBC Ldy AbsoluteX true;        0xBD Lda AbsoluteX true;
    0xBE Ldx AbsoluteY true;        0xBF Lax AbsoluteY false;
    0xC0 Cpy Immediate true;        0xC1 Cmp IndexedIndirect true;
    0xC2 Nop Immediate false;       0xC3 Dcp IndexedIndirect false;
    0xC4 Cpy ZeroPage true;         0xC5 Cmp ZeroPage true;
    0xC6 Dec ZeroPage true;         0xC7 Dcp ZeroPage false;
    0xC8 Iny Implied true;          0xC9 Cmp Immediate true;
    0xCA Dex Implied true;          0xCB Sbx Immediate false;
    0xCC Cpy Absolute true;         0xCD Cmp Absolute true;
    0xCE Dec Absolute true;         0xCF Dcp Absolute false;
    0xD0 Bne Relative true;         0xD1 Cmp IndirectIndexed true;
    0xD2 Jam Implied false;         0xD3 Dcp IndirectIndexed false;
    0xD4 Nop ZeroPageX false;       0xD5 Cmp ZeroPageX true;
    0xD6 Dec ZeroPageX true;        0xD7 Dcp ZeroPageX false;
    0xD8 Cld Implied true;          0xD9 Cmp AbsoluteY true;
    0xDA Nop Implied false;         0xDB Dcp AbsoluteY false;
    0xDC Nop AbsoluteX false;       0xDD Cmp AbsoluteX true;
    0xDE Dec AbsoluteX true;        0xDF Dcp AbsoluteX false;
    0xE0 Cpx Immediate true;        0xE1 Sbc IndexedIndirect true;
    0xE2 Nop Immediate false;       0xE3 Isc IndexedIndirect false;
    0xE4 Cpx ZeroPage true;         0xE5 Sbc ZeroPage true;
    0xE6 Inc ZeroPage true;         0xE7 Isc ZeroPage false;
    0xE8 Inx Implied true;          0xE9 Sbc Immediate true;
    0xEA Nop Implied true;          0xEB Sbc Immediate false;
    0xEC Cpx Absolute true;         0xED Sbc Absolute true;
    0xEE Inc Absolute true;         0xEF Isc Absolute false;
    0xF0 Beq Relative true;         0xF1 Sbc IndirectIndexed true;
    0xF2 Jam Implied false;         0xF3 Isc IndirectIndexed false;
    0xF4 Nop ZeroPageX false;       0xF5 Sbc ZeroPageX true;
    0xF6 Inc ZeroPageX true;        0xF7 Isc ZeroPageX false;
    0xF8 Sed Implied true;          0xF9 Sbc AbsoluteY true;
    0xFA Nop Implied false;         0xFB Isc AbsoluteY false;
    0xFC Nop AbsoluteX false;       0xFD Sbc AbsoluteX true;
    0xFE Inc AbsoluteX true;        0xFF Isc AbsoluteX false;
}

/// How many mnemonics and modes there are, from the table itself.
const MNEMONICS: usize = {
    let (mut most, mut code) = (0, 0);
    while code < OPCODES.len() {
        let mnemonic = OPCODES[code].mnemonic as usize;
        if mnemonic > most {
            most = mnemonic;
        }
        code += 1;
    }
    most + 1
};
const MODES: usize = Mode::Relative as usize + 1;

/// The documented opcode of each mnemonic in each mode, the lowest where
/// two are.
const ENCODINGS: [[Option<u8>; MODES]; MNEMONICS] = {
    let mut table = [[None; MODES]; MNEMONICS];
    let mut code: u8 = 0;
    loop {
        let opcode = OPCODES[code as usize];
        let entry = &mut table[opcode.mnemonic as usize][opcode.mode as usize];
        if opcode.documented && entry.is_none() {
            *entry = Some(code);
        }
        if code == u8::MAX {
            break;
        }
        code += 1;
    }
    table
};

/// The documented opcode for a mnemonic in a mode, if the manufacturer
/// documents one.
#[must_use]
pub const fn encode(mnemonic: Mnemonic, mode: Mode) -> Option<u8> {
    ENCODINGS[mnemonic as usize][mode as usize]
}
