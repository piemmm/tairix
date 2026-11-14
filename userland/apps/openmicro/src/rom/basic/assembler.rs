//! The inline assembler: from `[` to `]`, each statement any labels, then
//! a 6502 instruction, OPT, an EQU or nothing, assembled at P% (at O% from
//! OPT 4) and with OPT 1 listed, as BASIC II assembles.
//!
//! A mnemonic's opcodes are its base plus one offset per addressing mode,
//! the same for every mnemonic, so the tables hold each mnemonic's base and
//! its class: the set of modes it has, which also decides how its operand
//! is read. Both come from `cpu::isa`, and building checks that every
//! documented opcode is its mnemonic's base and its mode's offset.
//!
//! An operand can run any BASIC, an FN that assembles included, so a
//! statement keeps its own on the stack across one and takes where its
//! bytes go only once its operand is read.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use super::ws::{kind, page, zp as z};
use crate::cpu::isa::{encode, Mnemonic, Mode, OPCODES};
use crate::rom::asm::byte;
use crate::rom::asm::{abs, hi, imm, zp, Acc, Asm, Label, RTS};
use crate::rom::os::ESCAPE_FLAG;

/// The modes in the order of their bits in a class's set.
const MODES: [Mode; 13] = [
    Mode::Implied,
    Mode::Accumulator,
    Mode::Immediate,
    Mode::ZeroPage,
    Mode::ZeroPageX,
    Mode::ZeroPageY,
    Mode::Absolute,
    Mode::AbsoluteX,
    Mode::AbsoluteY,
    Mode::Indirect,
    Mode::IndexedIndirect,
    Mode::IndirectIndexed,
    Mode::Relative,
];
const IMPLIED: u8 = 0;
const ACCUMULATOR: u8 = 1;
const IMMEDIATE: u8 = 2;
const ZERO_PAGE: u8 = 3;
const ZERO_PAGE_X: u8 = 4;
const ZERO_PAGE_Y: u8 = 5;
const ABSOLUTE: u8 = 6;
const ABSOLUTE_X: u8 = 7;
const ABSOLUTE_Y: u8 = 8;
const INDIRECT: u8 = 9;
const INDEXED_INDIRECT: u8 = 10;
const INDIRECT_INDEXED: u8 = 11;
const RELATIVE: u8 = 12;
/// Each mode's opcode less the base. A class with (zp,X) takes its
/// immediate and abs,Y at the other offsets.
const OFFSETS: [u8; 13] = [0, 8, 0, 4, 0x14, 0x14, 0x0C, 0x1C, 0x1C, 0x2C, 0, 0x10, 0];
const IMMEDIATE_WITH_INDEXED: u8 = 8;
const ABSOLUTE_Y_WITH_INDEXED: u8 = 0x18;
/// An address's absolute modes lie this far past its zero-page ones, and
/// its indexes are X then Y.
const ABSOLUTE_FROM_ZERO_PAGE: u8 = ABSOLUTE - ZERO_PAGE;
const _: () = assert!(
    ABSOLUTE_X - ZERO_PAGE_X == ABSOLUTE_FROM_ZERO_PAGE
        && ABSOLUTE_Y - ZERO_PAGE_Y == ABSOLUTE_FROM_ZERO_PAGE
        && ZERO_PAGE_Y == ZERO_PAGE_X + 1
);
/// The window: a class's modes from the accumulator to abs,Y in a byte,
/// mode n as bit n − 1.
const _: () = assert!(ACCUMULATOR == 1 && ABSOLUTE_Y == 8);
const fn window_bit(mode: u8) -> u8 {
    1 << (mode - ACCUMULATOR)
}
const INDEXED: u8 = window_bit(ZERO_PAGE_X)
    | window_bit(ZERO_PAGE_Y)
    | window_bit(ABSOLUTE_X)
    | window_bit(ABSOLUTE_Y);

/// The pseudo-operations after the mnemonics, and EQU's kinds by their
/// letter: each value's bytes, a string's none.
const PSEUDO: [&str; 2] = ["OPT", "EQU"];
const EQU_KINDS: [(u8, u8); 4] = [(b'B', 1), (b'W', 2), (b'D', 4), (b'S', 0)];
/// The tokens a mnemonic can be spelt with: ORA is OR's and an A.
const TOKENS: [(u8, &str); 3] = [(0x80, "AND"), (0x82, "EOR"), (0x84, "ORA")];
const OR: u8 = 0x84;
const CR: u8 = 0x0D;
/// Where a listed statement's text starts, a continued line of bytes'
/// indent, and the bytes a line shows.
const SOURCE_COLUMN: u8 = 14;
const CONTINUED_COLUMN: u8 = 5;
const BYTES_A_LINE: u8 = 3;
/// OPT's options: listing, errors, and code placed at O%.
const OPT_LIST: u8 = 1;
const OPT_ERRORS: u8 = 2;
const OPT_OFFSET: u8 = 4;
/// P% and O%, resident integers. OPT 4's bit is P%'s distance from O%, so
/// it picks either from O%.
const P_PERCENT: u16 = page::resident(b'P');
const O_PERCENT: u16 = page::resident(b'O');
const P_FROM_O: u8 = (P_PERCENT - O_PERCENT).to_le_bytes()[0];
const _: () = assert!(OPT_OFFSET == P_FROM_O);

/// The statement's own, kept across its operand: its class's modes, its
/// mnemonic's base, and an EQU's size.
const MODE_SET: u16 = z::STATEMENT;
const BASE: u16 = MODE_SET + 2;
const SIZE: u16 = BASE + 1;
const KEPT: u8 = (SIZE + 1 - MODE_SET).to_le_bytes()[0];
/// Where its bytes go, as P% and as stored, and how many it made.
const LISTED: u16 = SIZE + 1;
const STORED: u16 = LISTED + 2;
const MADE: u16 = STORED + 2;
/// Its text's start and end, an address's zero-page mode, and the packed
/// mnemonic.
const SOURCE: u16 = MADE + 1;
const END: u16 = SOURCE + 1;
const MODE: u16 = END + 1;
const PACKED: u16 = MODE + 1;
const _: () = assert!(PACKED < 0x6F);

/// The mnemonics' tables, from `cpu::isa`.
struct Tables {
    /// Each name packed five bits a letter: the mnemonics by class, then
    /// the pseudo-operations.
    names: Vec<u16>,
    bases: Vec<u8>,
    /// Each class's mode set and the index after its last mnemonic.
    classes: Vec<(u16, u8)>,
    /// Each of `TOKENS`' mnemonics' index in `names`.
    tokens: Vec<u8>,
}

/// A name's three letters, five bits each.
fn packed(name: &str) -> u16 {
    name.bytes()
        .fold(0, |packed, letter| packed << 5 | u16::from(letter & 0x1F))
}

/// The offset of mode `bit` in a class of mode set `set`.
fn offset(set: u16, bit: u8) -> u8 {
    let indexed = set & 1 << INDEXED_INDIRECT != 0;
    match bit {
        IMMEDIATE if indexed => IMMEDIATE_WITH_INDEXED,
        ABSOLUTE_Y if indexed => ABSOLUTE_Y_WITH_INDEXED,
        _ => OFFSETS[usize::from(bit)],
    }
}

/// # Panics
///
/// If `cpu::isa` has a documented opcode that is not its mnemonic's base
/// and its mode's offset, or lacks a mnemonic `TOKENS` spells.
fn tables() -> Tables {
    let mut by_class: BTreeMap<u16, Vec<(Mnemonic, u8)>> = BTreeMap::new();
    let mut seen = Vec::new();
    for opcode in OPCODES.iter().filter(|opcode| opcode.documented) {
        let mnemonic = opcode.mnemonic;
        if seen.contains(&mnemonic) {
            continue;
        }
        seen.push(mnemonic);
        let set = (0u8..)
            .zip(MODES)
            .filter(|&(_, mode)| encode(mnemonic, mode).is_some())
            .fold(0u16, |set, (bit, _)| set | 1 << bit);
        let mut base = None;
        for (bit, mode) in (0u8..).zip(MODES) {
            if let Some(code) = encode(mnemonic, mode) {
                let this = code.wrapping_sub(offset(set, bit));
                assert!(
                    *base.get_or_insert(this) == this,
                    "{} has no one base",
                    mnemonic.name()
                );
            }
        }
        let base = base.unwrap_or_else(|| panic!("{} has no mode", mnemonic.name()));
        by_class.entry(set).or_default().push((mnemonic, base));
    }
    let mut names = Vec::new();
    let mut bases = Vec::new();
    let mut classes = Vec::new();
    for (set, members) in by_class {
        for (mnemonic, base) in members {
            names.push(packed(mnemonic.name()));
            bases.push(base);
        }
        classes.push((set, byte(names.len())));
    }
    names.extend(PSEUDO.map(packed));
    let tokens = TOKENS
        .iter()
        .map(|&(_, name)| {
            let index = names.iter().position(|&each| each == packed(name));
            byte(index.unwrap_or_else(|| panic!("{name} is no mnemonic")))
        })
        .collect();
    Tables {
        names,
        bases,
        classes,
        tokens,
    }
}

/// Z clear if the class has `mode`.
fn has(a: &mut Asm, mode: u8) {
    a.lda(zp(MODE_SET + u16::from(mode / 8)));
    a.and(imm(1 << (mode % 8)));
}

/// A the class's window.
fn window(a: &mut Asm) {
    a.lda(zp(MODE_SET + 1));
    a.lsr(Acc);
    a.lda(zp(MODE_SET));
    a.ror(Acc);
}

pub fn emit(a: &mut Asm) {
    let tables = tables();
    statements(a);
    statement_end(a);
    let (syntax, opt, instruction) = mnemonic(a, &tables);
    equ(a, syntax);
    mnemonic_kinds(a, opt, instruction);
    encoding(a);
    let not_branch = operand(a);
    let (expression, byte_error, index_error) = operand_modes(a, not_branch);
    address(a, expression, byte_error, index_error);
    operand_helpers(a);
    listing(a);
    data(a, &tables);
}

fn statements(a: &mut Asm) {
    // `[`: statements until `]`, at OPT 3, and no operand being read.
    a.here("statement_assemble");
    a.lda(imm(OPT_LIST | OPT_ERRORS));
    a.sta(zp(z::OPT));
    a.ldx(imm(0));
    a.stx(zp(z::ASM_QUIET));
    a.here("asm_statement");
    a.bit(zp(ESCAPE_FLAG));
    let going = a.local();
    a.bpl(going);
    a.jmp("escaped");
    a.bind(going);
    a.jsr("next_a");
    a.cmp(imm(b']'));
    let assembling = a.local();
    a.bne(assembling);
    a.jmp("statement_done");
    a.bind(assembling);
    // The statement's start, on the stack until it is listed.
    a.dey();
    a.sty(zp(z::OFFSET_A));
    a.tya();
    a.pha();
    // Labels: the name after each `.` made P%.
    let (labels, not_label) = (a.here_local(), a.local());
    a.jsr("next_a");
    a.cmp(imm(b'.'));
    a.bne(not_label);
    a.jsr("lvalue");
    let named = a.local();
    a.bcc(named);
    a.jmp("err_syntax");
    a.bind(named);
    a.ldx(imm(3));
    let copy = a.here_local();
    a.lda(abs(P_PERCENT).x());
    a.sta(zp(z::IAC).x());
    a.dex();
    a.bpl(copy);
    a.lda(imm(kind::INTEGER));
    a.jsr("store_variable");
    a.jmp(labels);
    // A comment, nothing, or an instruction; then whatever is left of the
    // statement passed over.
    a.bind(not_label);
    a.pha();
    a.jsr("asm_start");
    a.pla();
    let ended = a.local();
    for end in [b'\\', b':', CR] {
        a.cmp(imm(end));
        a.beq(ended);
    }
    a.jsr("asm_instruction");
    a.bind(ended);
    a.ldy(zp(z::OFFSET_A));
    a.dey();
    let (scan, found) = (a.here_local(), a.local());
    a.lda(zp(z::TEXT_A).ind_y());
    a.cmp(imm(b':'));
    a.beq(found);
    a.cmp(imm(CR));
    a.beq(found);
    a.iny();
    a.bne(scan);
    a.bind(found);
    a.sty(zp(END));
}

fn statement_end(a: &mut Asm) {
    // P% on past the bytes, and O% before it when they went there.
    a.jsr("asm_place");
    let (advance, advanced, placed) = (a.here_local(), a.local(), a.local());
    a.lda(abs(O_PERCENT).x());
    a.clc();
    a.adc(zp(MADE));
    a.sta(abs(O_PERCENT).x());
    a.bcc(advanced);
    a.inc(abs(O_PERCENT + 1).x());
    a.bind(advanced);
    a.cpx(imm(P_FROM_O));
    a.beq(placed);
    a.ldx(imm(P_FROM_O));
    a.bne(advance);
    a.bind(placed);
    a.pla();
    a.sta(zp(SOURCE));
    a.lda(zp(z::OPT));
    a.lsr(Acc);
    let unlisted = a.local();
    a.bcc(unlisted);
    a.jsr("asm_list");
    a.bind(unlisted);
    a.ldy(zp(END));
    a.lda(zp(z::TEXT_A).ind_y());
    a.iny();
    a.sty(zp(z::OFFSET_A));
    let (again, finished) = (a.local(), a.local());
    a.cmp(imm(b':'));
    a.beq(again);
    // A typed line ends here; in a program the next line follows.
    a.lda(zp(z::TEXT_A + 1));
    a.cmp(imm(hi(page::INPUT)));
    a.beq(finished);
    a.jsr("next_text_record");
    a.ldy(imm(1));
    a.lda(zp(z::TEXT_A).ind_y());
    a.bmi(finished);
    a.lda(imm(4));
    a.sta(zp(z::OFFSET_A));
    a.bind(again);
    a.jmp("asm_statement");
    a.bind(finished);
    a.jmp("end_run");

    // X where the bytes go, from O%: O% with OPT 4, else P%.
    a.here("asm_place");
    a.lda(zp(z::OPT));
    a.and(imm(OPT_OFFSET));
    a.eor(imm(P_FROM_O));
    a.tax();
    a.rts();
}

/// The Syntax error, OPT's code and an instruction's, which the parts after
/// share.
fn mnemonic(a: &mut Asm, tables: &Tables) -> (Label, Label, Label) {
    // The instruction whose first character is A, Y past it: its mnemonic
    // found and its operand read and made; or OPT set, or an EQU made.
    a.here("asm_instruction");
    let (token, syntax, found) = (a.local(), a.local(), a.local());
    a.cmp(imm(0x80));
    a.bcs(token);
    a.dec(zp(z::OFFSET_A));
    a.lda(imm(0));
    a.sta(zp(PACKED));
    a.sta(zp(PACKED + 1));
    a.ldx(imm(3));
    let letter = a.here_local();
    a.ldy(zp(z::OFFSET_A));
    a.lda(zp(z::TEXT_A).ind_y());
    a.inc(zp(z::OFFSET_A));
    a.and(imm(0x1F));
    a.ldy(imm(5));
    let shift = a.here_local();
    a.asl(zp(PACKED));
    a.rol(zp(PACKED + 1));
    a.dey();
    a.bne(shift);
    a.ora(zp(PACKED));
    a.sta(zp(PACKED));
    a.dex();
    a.bne(letter);
    a.ldx(imm(byte(tables.names.len() - 1)));
    let (name, next) = (a.here_local(), a.local());
    a.lda(zp(PACKED));
    a.cmp(abs("asm_names_low").x());
    a.bne(next);
    a.lda(zp(PACKED + 1));
    a.cmp(abs("asm_names_high").x());
    a.beq(found);
    a.bind(next);
    a.dex();
    a.bpl(name);
    a.bind(syntax);
    a.jmp("err_syntax");
    a.bind(token);
    a.ldx(imm(byte(TOKENS.len() - 1)));
    let (spelt, spelling) = (a.local(), a.local());
    let each = a.here_local();
    a.cmp(abs("asm_tokens").x());
    a.beq(spelt);
    a.dex();
    a.bpl(each);
    a.bmi(syntax);
    a.bind(spelt);
    a.cmp(imm(OR));
    a.bne(spelling);
    a.lda(zp(z::TEXT_A).ind_y());
    a.cmp(imm(b'A'));
    a.bne(syntax);
    a.inc(zp(z::OFFSET_A));
    a.bind(spelling);
    a.lda(abs("asm_token_mnemonics").x());
    a.tax();
    a.bind(found);
    let (instruction, opt) = (a.local(), a.local());
    a.cpx(imm(byte(tables.bases.len())));
    a.bcc(instruction);
    a.beq(opt);
    (syntax, opt, instruction)
}

/// `syntax` is the instruction's Syntax error.
fn equ(a: &mut Asm, syntax: Label) {
    // EQU: its letter's value bytes, or a string's.
    a.ldy(zp(z::OFFSET_A));
    a.lda(zp(z::TEXT_A).ind_y());
    a.inc(zp(z::OFFSET_A));
    a.ldx(imm(byte(EQU_KINDS.len() - 1)));
    let (equ_kind, kinded) = (a.here_local(), a.local());
    a.cmp(abs("equ_letters").x());
    a.beq(kinded);
    a.dex();
    a.bpl(equ_kind);
    a.bmi(syntax);
    a.bind(kinded);
    a.lda(abs("equ_sizes").x());
    let string = a.local();
    a.beq(string);
    a.sta(zp(SIZE));
    a.jsr("asm_operand");
    a.ldx(imm(0));
    let value = a.here_local();
    a.lda(zp(z::IAC).x());
    a.jsr("asm_byte");
    a.inx();
    a.cpx(zp(SIZE));
    a.bne(value);
    a.rts();
    a.bind(string);
    a.jsr("asm_value");
    a.tay();
    let text = a.local();
    a.beq(text);
    a.jmp("err_type_mismatch");
    a.bind(text);
    a.jsr("asm_start");
    a.tax();
    let character = a.here_local();
    a.cpx(zp(z::STRING_LENGTH));
    a.beq(RTS);
    a.lda(abs(page::STRING).x());
    a.jsr("asm_byte");
    a.inx();
    a.bne(character);
    a.rts();
}

fn mnemonic_kinds(a: &mut Asm, opt: Label, instruction: Label) {
    a.bind(opt);
    a.jsr("asm_operand");
    a.lda(zp(z::IAC));
    a.sta(zp(z::OPT));
    a.rts();
    a.bind(instruction);
    a.lda(abs("asm_bases").x());
    a.sta(zp(BASE));
    a.ldy(imm(0));
    a.txa();
    let (class, classed) = (a.here_local(), a.local());
    a.cmp(abs("asm_class_ends").y());
    a.bcc(classed);
    a.iny();
    a.bne(class);
    a.bind(classed);
    a.lda(abs("asm_modes_low").y());
    a.sta(zp(MODE_SET));
    a.lda(abs("asm_modes_high").y());
    a.sta(zp(MODE_SET + 1));
    a.jsr("asm_modes");
    a.fall_into("asm_encode");
}

fn encoding(a: &mut Asm) {
    // The opcode for mode A, the base and the mode's offset, and its
    // operand's bytes.
    a.here("asm_encode");
    a.tax();
    a.ldy(abs("asm_offsets").x());
    has(a, INDEXED_INDIRECT);
    let offset_ready = a.local();
    a.beq(offset_ready);
    a.cpx(imm(IMMEDIATE));
    let not_immediate = a.local();
    a.bne(not_immediate);
    a.ldy(imm(IMMEDIATE_WITH_INDEXED));
    a.bind(not_immediate);
    a.cpx(imm(ABSOLUTE_Y));
    a.bne(offset_ready);
    a.ldy(imm(ABSOLUTE_Y_WITH_INDEXED));
    a.bind(offset_ready);
    a.tya();
    a.clc();
    a.adc(zp(BASE));
    a.jsr("asm_byte");
    a.cpx(imm(IMMEDIATE));
    a.bcc(RTS);
    a.lda(zp(z::IAC));
    a.jsr("asm_byte");
    a.cpx(imm(ABSOLUTE));
    a.bcc(RTS);
    a.cpx(imm(INDEXED_INDIRECT));
    a.bcs(RTS);
    a.lda(zp(z::IAC + 1));
    a.fall_into("asm_byte");

    // A put after the bytes made; X kept.
    a.here("asm_byte");
    a.ldy(zp(MADE));
    a.sta(zp(STORED).ind_y());
    a.inc(zp(MADE));
    a.rts();
}

/// The modes past a branch's, where an operand's characters decide.
fn operand(a: &mut Asm) -> Label {
    // A the mode the class reads its operand in, IAC the operand.
    a.here("asm_modes");
    a.lda(zp(MODE_SET));
    a.lsr(Acc);
    a.lda(imm(IMPLIED));
    a.bcs(RTS);
    has(a, RELATIVE);
    let not_branch = a.local();
    a.beq(not_branch);
    // A branch's offset from the instruction after it; out of range, an
    // error only with OPT's errors on, and otherwise none.
    a.jsr("asm_operand");
    a.lda(zp(z::IAC));
    a.clc();
    a.sbc(zp(LISTED));
    a.tax();
    a.lda(zp(z::IAC + 1));
    a.sbc(zp(LISTED + 1));
    a.cpx(imm(1));
    a.sbc(imm(0));
    a.dex();
    a.stx(zp(z::IAC));
    a.cpx(imm(0x80));
    a.adc(imm(0));
    let reaches = a.local();
    a.beq(reaches);
    a.lda(zp(z::OPT));
    a.and(imm(OPT_ERRORS));
    let quiet = a.local();
    a.beq(quiet);
    a.jmp("err_out_of_range");
    a.bind(quiet);
    a.sta(zp(z::IAC));
    a.bind(reaches);
    a.lda(imm(RELATIVE));
    a.rts();
    not_branch
}

/// The address, Byte and Index exits, which `address` shares.
fn operand_modes(a: &mut Asm, not_branch: Label) -> (Label, Label, Label) {
    a.bind(not_branch);
    let (expression, byte_error, index_error) = (a.local(), a.local(), a.local());
    a.jsr("next_a");
    // `(`: (zp,X) or (zp),Y where the class has them, JMP's (abs).
    a.cmp(imm(b'('));
    let not_bracket = a.local();
    a.bne(not_bracket);
    has(a, INDEXED_INDIRECT);
    let not_indexed = a.local();
    a.beq(not_indexed);
    a.jsr("asm_operand");
    a.lda(zp(z::IAC + 1));
    a.bne(byte_error);
    a.jsr("next_a");
    a.cmp(imm(b','));
    let bracket = a.local();
    a.bne(bracket);
    a.jsr("next_a");
    a.cmp(imm(b'X'));
    a.bne(index_error);
    a.jsr("next_a");
    a.cmp(imm(b')'));
    a.bne(index_error);
    a.lda(imm(INDEXED_INDIRECT));
    a.rts();
    a.bind(bracket);
    a.cmp(imm(b')'));
    a.bne(index_error);
    a.jsr("next_a");
    a.cmp(imm(b','));
    a.bne(index_error);
    a.jsr("next_a");
    a.cmp(imm(b'Y'));
    a.bne(index_error);
    a.lda(imm(INDIRECT_INDEXED));
    a.rts();
    a.bind(not_indexed);
    has(a, INDIRECT);
    a.beq(expression);
    a.jsr("asm_operand");
    a.jsr("next_a");
    a.cmp(imm(b')'));
    a.bne(index_error);
    a.lda(imm(INDIRECT));
    a.rts();
    // `#` where the class has an immediate.
    a.bind(not_bracket);
    a.cmp(imm(b'#'));
    let not_hash = a.local();
    a.bne(not_hash);
    has(a, IMMEDIATE);
    a.beq(expression);
    a.jsr("asm_operand");
    a.lda(zp(z::IAC + 1));
    a.bne(byte_error);
    a.lda(imm(IMMEDIATE));
    a.rts();
    // `A` alone where the class shifts it; Y is still past it.
    a.bind(not_hash);
    a.cmp(imm(b'A'));
    a.bne(expression);
    has(a, ACCUMULATOR);
    a.beq(expression);
    a.lda(zp(z::TEXT_A).ind_y());
    a.jsr("is_name");
    a.bcs(expression);
    a.lda(imm(ACCUMULATOR));
    a.rts();
    a.bind(byte_error);
    a.jmp("err_byte");
    a.bind(index_error);
    a.jmp("err_index");
    (expression, byte_error, index_error)
}

fn address(a: &mut Asm, expression: Label, byte_error: Label, index_error: Label) {
    // An address: indexed by X or Y where the class indexes, its zero-page
    // form where the value is below 256 and the class has one.
    a.bind(expression);
    a.dec(zp(z::OFFSET_A));
    a.jsr("asm_operand");
    let (plain, choose, unread, large) = (a.local(), a.local(), a.local(), a.local());
    window(a);
    a.and(imm(INDEXED));
    a.beq(plain);
    a.jsr("next_a");
    a.cmp(imm(b','));
    a.bne(unread);
    a.jsr("next_a");
    a.sec();
    a.sbc(imm(b'X'));
    a.cmp(imm(2));
    a.bcs(index_error);
    a.adc(imm(ZERO_PAGE_X));
    a.bne(choose);
    a.bind(unread);
    a.dec(zp(z::OFFSET_A));
    a.bind(plain);
    a.lda(imm(ZERO_PAGE));
    a.bind(choose);
    a.sta(zp(MODE));
    a.ldy(zp(z::IAC + 1));
    a.bne(large);
    a.jsr("asm_has");
    a.bcs(RTS);
    a.bind(large);
    a.clc();
    a.adc(imm(ABSOLUTE_FROM_ZERO_PAGE));
    a.jsr("asm_has");
    a.bcs(RTS);
    a.lda(zp(MODE));
    a.jsr("asm_has");
    a.bcs(byte_error);
    a.bcc(index_error);
}

fn operand_helpers(a: &mut Asm) {
    // C set if the class has mode A, from 1 to 8; A and X kept.
    a.here("asm_has");
    a.tay();
    a.pha();
    window(a);
    let bit = a.here_local();
    a.lsr(Acc);
    a.dey();
    a.bne(bit);
    a.pla();
    a.rts();

    // The operand's value, A its kind, its next character given back. It
    // may run any BASIC, so the statement's own is kept across it; with
    // OPT's errors off a name no variable has is P%.
    a.here("asm_value");
    a.ldx(imm(KEPT - 1));
    let keep = a.here_local();
    a.lda(zp(MODE_SET).x());
    a.pha();
    a.dex();
    a.bpl(keep);
    a.lda(zp(z::OPT));
    a.and(imm(OPT_ERRORS));
    let loud = a.local();
    a.bne(loud);
    a.lda(imm(0x80));
    a.sta(zp(z::ASM_QUIET));
    a.bind(loud);
    a.jsr("expr");
    a.tay();
    a.ldx(imm(0));
    a.stx(zp(z::ASM_QUIET));
    let restore = a.here_local();
    a.pla();
    a.sta(zp(MODE_SET).x());
    a.inx();
    a.cpx(imm(KEPT));
    a.bne(restore);
    a.dec(zp(z::OFFSET_A));
    a.tya();
    a.rts();

    // The operand made an integer in IAC, then where the bytes go taken.
    a.here("asm_operand");
    a.jsr("asm_value");
    a.jsr("make_integer");
    a.fall_into("asm_start");

    // LISTED P% and STORED where the bytes go, as they now are, and none
    // made; A 0 and Y kept.
    a.here("asm_start");
    a.lda(abs(P_PERCENT));
    a.sta(zp(LISTED));
    a.lda(abs(P_PERCENT + 1));
    a.sta(zp(LISTED + 1));
    a.jsr("asm_place");
    a.lda(abs(O_PERCENT).x());
    a.sta(zp(STORED));
    a.lda(abs(O_PERCENT + 1).x());
    a.sta(zp(STORED + 1));
    a.lda(imm(0));
    a.sta(zp(MADE));
    a.rts();
}

fn listing(a: &mut Asm) {
    // The statement's address, its bytes three a line, and its text from
    // the fifteenth column.
    a.here("asm_list");
    a.lda(zp(LISTED + 1));
    a.jsr("print_hex");
    a.lda(zp(LISTED));
    a.jsr("print_hex_space");
    a.ldy(imm(0));
    a.ldx(imm(BYTES_A_LINE));
    let (byte, bytes_done, same_line) = (a.here_local(), a.local(), a.local());
    a.cpy(zp(MADE));
    a.beq(bytes_done);
    a.txa();
    a.bne(same_line);
    a.jsr("print_newline");
    a.lda(imm(CONTINUED_COLUMN));
    a.jsr("pad_to");
    a.ldx(imm(BYTES_A_LINE));
    a.bind(same_line);
    a.lda(zp(STORED).ind_y());
    a.jsr("print_hex_space");
    a.iny();
    a.dex();
    a.bpl(byte);
    a.bind(bytes_done);
    a.lda(imm(SOURCE_COLUMN));
    a.jsr("pad_to");
    let (text, character, listed) = (a.here_local(), a.local(), a.local());
    a.ldy(zp(SOURCE));
    a.cpy(zp(END));
    a.beq(listed);
    a.inc(zp(SOURCE));
    a.lda(zp(z::TEXT_A).ind_y());
    a.bpl(character);
    a.jsr("print_token");
    a.jmp(text);
    a.bind(character);
    a.jsr("print_char");
    a.jmp(text);
    a.bind(listed);
    a.jmp("print_newline");

    // Spaces until COUNT reaches A.
    a.here("pad_to");
    a.tax();
    let pad = a.here_local();
    a.cpx(zp(z::COUNT));
    a.beq(RTS);
    a.bcc(RTS);
    a.jsr("print_space");
    a.jmp(pad);

    // A in two hex digits, then a space.
    a.here("print_hex_space");
    a.jsr("print_hex");
    a.jmp("print_space");

    // A in two hex digits.
    a.here("print_hex");
    a.pha();
    for _ in 0..4 {
        a.lsr(Acc);
    }
    a.jsr("print_hex_digit");
    a.pla();
    a.and(imm(0x0F));
    a.here("print_hex_digit");
    a.cmp(imm(10));
    let digit = a.local();
    a.bcc(digit);
    a.adc(imm(b'A' - b'0' - 10 - 1));
    a.bind(digit);
    a.adc(imm(b'0'));
    a.jmp("print_char");
}

fn data(a: &mut Asm, tables: &Tables) {
    a.here("asm_names_low");
    for name in &tables.names {
        a.byte(lo_byte(*name));
    }
    a.here("asm_names_high");
    for name in &tables.names {
        a.byte(hi_byte(*name));
    }
    a.here("asm_bases");
    a.bytes(&tables.bases);
    a.here("asm_class_ends");
    for &(_, end) in &tables.classes {
        a.byte(end);
    }
    a.here("asm_modes_low");
    for &(set, _) in &tables.classes {
        a.byte(lo_byte(set));
    }
    a.here("asm_modes_high");
    for &(set, _) in &tables.classes {
        a.byte(hi_byte(set));
    }
    a.here("asm_offsets");
    a.bytes(&OFFSETS);
    a.here("asm_tokens");
    for (token, _) in TOKENS {
        a.byte(token);
    }
    a.here("asm_token_mnemonics");
    a.bytes(&tables.tokens);
    a.here("equ_letters");
    for (letter, _) in EQU_KINDS {
        a.byte(letter);
    }
    a.here("equ_sizes");
    for (_, size) in EQU_KINDS {
        a.byte(size);
    }
}

fn lo_byte(word: u16) -> u8 {
    word.to_le_bytes()[0]
}

fn hi_byte(word: u16) -> u8 {
    word.to_le_bytes()[1]
}

#[cfg(test)]
mod tests {
    use super::{offset, packed, tables, MODES, TOKENS};
    use crate::cpu::isa::OPCODES;

    #[test]
    fn every_documented_opcode_is_its_base_and_offset() {
        let tables = tables();
        let mut start = 0;
        let mut found = 0;
        for &(set, end) in &tables.classes {
            for index in start..usize::from(end) {
                let base = tables.bases[index];
                for (bit, mode) in (0u8..).zip(MODES) {
                    if set & 1 << bit != 0 {
                        let code = base.wrapping_add(offset(set, bit));
                        assert!(OPCODES[usize::from(code)].documented);
                        assert_eq!(OPCODES[usize::from(code)].mode, mode);
                        found += 1;
                    }
                }
            }
            start = usize::from(end);
        }
        assert_eq!(found, OPCODES.iter().filter(|o| o.documented).count());
    }

    #[test]
    fn each_token_names_its_mnemonic() {
        let tables = tables();
        for (&(_, name), &index) in TOKENS.iter().zip(&tables.tokens) {
            assert_eq!(tables.names[usize::from(index)], packed(name));
        }
    }
}
