//! OSCLI and its commands, GSINIT and GSREAD, and the number reading the
//! commands share.
//!
//! A command is matched against the MOS's own, abbreviated with a full
//! stop as far as it is given; one the MOS does not know is offered to the
//! ROMs, then to the filing system.

use super::ws::{os, page2, var, vector};
use super::{copy, DELETE};
use crate::cpu::STACK_PAGE;
use crate::machine::Model;
use crate::rom::asm::{abs, hi, imm, lo, sym, zp, Acc, Asm, RTS};
use crate::rom::os::{fsc, service};

/// The commands, each with its routine, in the order abbreviations match.
const COMMANDS: [(&str, &str); 17] = [
    ("CAT", "star_cat"),
    ("FX", "star_fx"),
    ("BASIC", "star_basic"),
    ("CODE", "star_code"),
    ("EXEC", "star_exec"),
    ("HELP", "star_help"),
    ("KEY", "key_command"),
    ("LOAD", "star_load"),
    ("LINE", "star_line"),
    ("MOTOR", "star_motor"),
    ("OPT", "star_opt"),
    ("RUN", "star_run"),
    ("ROM", "star_rom"),
    ("SAVE", "star_save"),
    ("SPOOL", "star_spool"),
    ("TAPE", "star_tape"),
    ("TV", "star_tv"),
];

/// Service calls: an unrecognised command, and *HELP.
/// FSCV's reasons.
/// OSFILE's block, as *LOAD and *SAVE build it.
const BLOCK: u16 = page2::OSFILE_BLOCK;
/// GSREAD's flags at &E4: the string is quoted; a space ends it.
const QUOTED: u8 = 0x80;
const SPACE_ENDS: u8 = 0x40;

pub fn emit(a: &mut Asm, model: Model) {
    // Its branches reach across the whole of it.
    a.whole(oscli);
    commands(a, model);
    files(a);
    gs(a);
    numbers(a);
    tables(a);
}

fn oscli(a: &mut Asm) {
    // The filing system hears of every line first, comments included, as
    // MOS 1.20 tells it; the DFS's *ENABLE counts lines by it.
    a.here("oscli_default");
    a.stx(zp(os::TEXT));
    a.sty(zp(os::TEXT + 1));
    a.lda(imm(fsc::COMMAND_LINE));
    a.jsr("fsc_call");
    a.ldy(imm(0));
    a.here("oscli_skip");
    a.lda(zp(os::TEXT).ind_y());
    a.cmp(imm(b' '));
    a.beq("oscli_skip_next");
    a.cmp(imm(b'*'));
    a.bne("oscli_start");
    a.here("oscli_skip_next");
    a.iny();
    a.bne("oscli_skip");
    a.here("oscli_start");
    a.cmp(imm(b'|'));
    a.beq(RTS);
    a.cmp(imm(0x0D));
    a.beq(RTS);
    a.cmp(imm(b'/'));
    a.bne("oscli_match");
    a.iny();
    a.jmp("star_run");
    // Try each command: its letters in turn, case aside, until the command
    // ends or the line gives a full stop.
    a.here("oscli_match");
    a.sty(zp(os::GS_QUOTE));
    a.ldx(imm(0));
    a.here("oscli_command");
    a.ldy(zp(os::GS_QUOTE));
    a.here("oscli_letter");
    a.lda(abs("command_names").x());
    a.bmi("oscli_command_end");
    a.lda(zp(os::TEXT).ind_y());
    a.cmp(imm(b'.'));
    a.beq("oscli_abbreviated");
    a.and(imm(0xDF));
    a.cmp(abs("command_names").x());
    a.bne("oscli_next_command");
    a.iny();
    a.inx();
    a.bne("oscli_letter");
    a.here("oscli_abbreviated");
    a.iny();
    a.here("oscli_skip_name");
    a.lda(abs("command_names").x());
    a.bmi("oscli_found");
    a.inx();
    a.bne("oscli_skip_name");
    // The command's name is done: the line must not run on in letters.
    a.here("oscli_command_end");
    a.lda(zp(os::TEXT).ind_y());
    a.and(imm(0xDF));
    a.cmp(imm(b'A'));
    a.bcc("oscli_found");
    a.cmp(imm(b'Z' + 1));
    a.bcc("oscli_next_command");
    a.here("oscli_found");
    a.lda(abs("command_names").x());
    a.pha();
    a.lda(abs(sym("command_names") + 1).x());
    a.pha();
    a.jsr("skip_spaces");
    a.rts();
    a.here("oscli_next_command");
    a.lda(abs("command_names").x());
    a.inx();
    a.asl(Acc);
    a.bcc("oscli_next_command");
    a.inx();
    a.lda(abs("command_names").x());
    a.bne("oscli_command");
    // None of the MOS's: the ROMs', then the filing system's.
    a.ldy(zp(os::GS_QUOTE));
    a.lda(imm(service::COMMAND));
    a.jsr("service_call");
    a.beq(RTS);
    a.ldy(zp(os::GS_QUOTE));
    a.jsr("text_address");
    a.lda(imm(fsc::COMMAND));
    a.jmp(abs(vector::FSC).ind());

    // X and Y = the address of the text at (&F2),Y.
    a.here("text_address");
    a.tya();
    a.clc();
    a.adc(zp(os::TEXT));
    a.tax();
    a.lda(zp(os::TEXT + 1));
    a.adc(imm(0));
    a.tay();
    a.rts();
}

fn commands(a: &mut Asm, model: Model) {
    a.here("star_cat");
    a.jsr("text_address");
    a.lda(imm(fsc::CAT));
    a.jmp(abs(vector::FSC).ind());

    a.here("star_run");
    a.jsr("text_address");
    a.lda(imm(fsc::SLASH));
    a.jmp(abs(vector::FSC).ind());

    a.here("star_code");
    a.jsr("read_two");
    a.lda(imm(0));
    a.jmp(abs(vector::USER).ind());

    a.here("star_line");
    a.jsr("text_address");
    a.lda(imm(1));
    a.jmp(abs(vector::USER).ind());

    a.here("star_motor");
    a.jsr("read_two");
    a.lda(imm(0x89));
    a.jmp("osbyte");

    a.here("star_opt");
    a.jsr("read_two");
    a.lda(imm(0x8B));
    a.jmp("osbyte");

    a.here("star_rom");
    a.lda(imm(0x8D));
    a.jmp("osbyte");

    a.here("star_tape");
    a.jsr("read_two");
    a.lda(imm(0x8C));
    a.jmp("osbyte");

    a.here("star_tv");
    a.jsr("read_two");
    a.lda(imm(0x90));
    a.jmp("osbyte");

    // The ROMs are told of the topic, Y at it past the word and its spaces.
    // The ROMs answer first, then the OS names itself, as MOS 1.20 and
    // Electron OS 1.00 do.
    a.here("star_help");
    a.lda(imm(service::HELP));
    a.jsr("service_call");
    let version = match model {
        Model::ModelB => "\r\nOS 1.20\r\n",
        Model::Electron => "\r\nOS 1.00\r\n",
    };
    a.call_with_text("print_inline", &[version]);
    a.rts();

    // X, Y = up to two numbers, zero where absent: the first after any
    // spaces, the second after a comma or spaces, and only spaces after it.
    a.here("read_two");
    a.jsr("read_decimal");
    a.bcc("read_two_first");
    a.jsr("command_end");
    a.ldx(imm(0));
    // The second, the first X waiting on the stack while it is read.
    a.here("read_two_first");
    a.txa();
    a.pha();
    a.jsr("read_next");
    a.jsr("command_end");
    a.txa();
    a.tay();
    a.pla();
    a.tax();
    a.rts();

    // X = the number after a comma or spaces, or 0 if there is none; a
    // comma must have one.
    a.here("read_next");
    a.jsr("skip_spaces");
    a.cmp(imm(b','));
    a.bne("read_next_plain");
    a.iny();
    a.jsr("read_decimal");
    a.bcs("bad_command");
    a.rts();
    a.here("read_next_plain");
    a.jsr("read_decimal");
    a.bcc(RTS);
    a.ldx(imm(0));
    a.rts();

    // Nothing but spaces may be left of the line.
    a.here("command_end");
    a.jsr("skip_spaces");
    a.cmp(imm(0x0D));
    a.bne("bad_command");
    a.rts();

    // *FX a[,x[,y]].
    a.here("star_fx");
    a.jsr("read_decimal");
    a.bcs("bad_command");
    a.stx(zp(os::GS_CHAR));
    a.jsr("read_next");
    a.jsr("read_two_first");
    a.lda(zp(os::GS_CHAR));
    a.jsr("osbyte");
    a.bvs("bad_command");
    a.rts();

    a.here("star_basic");
    a.ldx(abs(var::BASIC_ROM));
    a.bmi("bad_command");
    a.jmp("enter_language");
}

/// *LOAD, *SAVE, *SPOOL and *EXEC.
fn files(a: &mut Asm) {
    // Between the commands and the files, so both reach it.
    a.here("bad_command");
    a.error(0xFE, "Bad command");

    // *LOAD name [address]: to its own address unless one is given.
    a.here("star_load");
    a.jsr("filename");
    a.lda(imm(0xFF));
    a.sta(abs(BLOCK + 6));
    a.ldx(imm(2));
    a.jsr("read_hex_into");
    a.bcs("star_load_go");
    a.lda(imm(0));
    a.sta(abs(BLOCK + 6));
    a.here("star_load_go");
    a.lda(imm(0xFF));
    a.bne("osfile_block");

    // *SAVE name start end|+length [exec [reload]].
    a.here("star_save");
    a.jsr("filename");
    a.ldx(imm(10));
    a.jsr("read_hex_into");
    a.bcs("bad_command");
    a.jsr("skip_spaces");
    a.cmp(imm(b'+'));
    a.php();
    a.bne("star_save_end");
    a.iny();
    a.here("star_save_end");
    a.ldx(imm(14));
    a.jsr("read_hex_into");
    a.bcs("bad_command");
    a.plp();
    a.bne("star_save_exec");
    // A length: end = start + length, X counting up to zero so the carry
    // runs through.
    a.ldx(imm(0xFC));
    a.clc();
    a.here("star_save_add");
    a.lda(abs(BLOCK + 10 - 0xFC).x());
    a.adc(abs(BLOCK + 14 - 0xFC).x());
    a.sta(abs(BLOCK + 14 - 0xFC).x());
    a.inx();
    a.bne("star_save_add");
    a.here("star_save_exec");
    // The execution and reload addresses default to the start.
    copy(a, &[(BLOCK + 10, &[BLOCK + 6, BLOCK + 2])], 4);
    a.ldx(imm(6));
    a.jsr("read_hex_into");
    a.bcs("star_save_go");
    a.ldx(imm(2));
    a.jsr("read_hex_into");
    a.here("star_save_go");
    a.lda(imm(0));

    // OSFILE A on the block.
    a.here("osfile_block");
    a.ldx(imm(lo(BLOCK)));
    a.ldy(imm(hi(BLOCK)));
    a.jmp("osfile");

    // The block's filename address: the name at (&F2),Y, which is then
    // passed over.
    a.here("filename");
    a.tya();
    a.pha();
    a.jsr("text_address");
    a.stx(abs(BLOCK));
    a.sty(abs(BLOCK + 1));
    a.pla();
    a.tay();
    a.clc();
    a.jsr("gsinit");
    a.here("filename_skip");
    a.jsr("gsread");
    a.bcc("filename_skip");
    a.rts();
    star_spool(a);
}

/// `*SPOOL [name]`: the old file closed, a new one opened for output.
fn star_spool(a: &mut Asm) {
    a.here("star_spool");
    a.jsr("close_spool");
    a.lda(zp(os::TEXT).ind_y());
    a.cmp(imm(0x0D));
    a.beq(RTS);
    a.jsr("text_address");
    a.lda(imm(0x80));
    a.jsr("osfind");
    a.sta(abs(var::SPOOL_HANDLE));
    a.rts();

    a.here("star_exec");
    a.jsr("close_exec");
    a.lda(zp(os::TEXT).ind_y());
    a.cmp(imm(0x0D));
    a.beq(RTS);
    a.jsr("text_address");
    a.lda(imm(0x40));
    a.jsr("osfind");
    a.sta(abs(var::EXEC_HANDLE));
    // A file not opened is an error, as MOS 1.20 makes it.
    a.tay();
    a.bne(RTS);
    a.jmp("file_not_found");

    a.here("close_spool_exec");
    a.jsr("close_exec");
    a.here("close_spool");
    a.ldx(imm(1));
    a.bne("close_handle");
    a.here("close_exec");
    a.ldx(imm(0));
    // Close the file in &256 + X, if one is open, keeping Y: *SPOOL and
    // *EXEC read on along the line from it.
    a.here("close_handle");
    a.lda(abs(var::EXEC_HANDLE).x());
    a.beq(RTS);
    a.pha();
    a.tya();
    a.pha();
    a.lda(imm(0));
    a.sta(abs(var::EXEC_HANDLE).x());
    // The handle, from beneath Y on the stack.
    a.tsx();
    a.ldy(abs(STACK_PAGE + 2).x());
    a.jsr("osfind");
    a.pla();
    a.tay();
    a.pla();
    a.rts();
}

/// GSINIT and GSREAD: a string with |-escapes and optional quotes.
fn gs(a: &mut Asm) {
    // GSINIT: (&F2),Y at the string's start; C clear lets a space end it.
    // Returns its first character, Z if the string is empty.
    a.here("gsinit");
    a.lda(imm(SPACE_ENDS));
    a.bcc("gsinit_space");
    a.lda(imm(0));
    a.here("gsinit_space");
    a.sta(zp(os::GS_QUOTE));
    a.jsr("skip_spaces");
    a.cmp(imm(b'"'));
    a.bne("gsinit_plain");
    // Spaces are part of a quoted string.
    a.lda(imm(QUOTED));
    a.sta(zp(os::GS_QUOTE));
    a.iny();
    a.here("gsinit_plain");
    a.lda(zp(os::TEXT).ind_y());
    a.cmp(imm(0x0D));
    a.rts();

    // GSREAD: A = the next character; C set at the string's end, with Y at
    // the next word.
    a.here("gsread");
    a.lda(imm(0));
    a.sta(zp(os::GS_CHAR));
    a.here("gsread_next");
    a.lda(zp(os::TEXT).ind_y());
    a.cmp(imm(0x0D));
    a.bne("gsread_not_end");
    a.bit(zp(os::GS_QUOTE));
    a.bmi("gsread_bad");
    a.here("gsread_end");
    a.jsr("skip_spaces");
    a.sec();
    a.rts();
    a.here("gsread_bad");
    a.error(0xFD, "Bad string");
    a.here("gsread_not_end");
    a.cmp(imm(b' '));
    a.bne("gsread_not_space");
    a.bit(zp(os::GS_QUOTE));
    a.bvs("gsread_end");
    a.here("gsread_not_space");
    a.iny();
    a.cmp(imm(b'"'));
    a.bne("gsread_not_quote");
    a.bit(zp(os::GS_QUOTE));
    a.bpl("gsread_char");
    // A doubled quote is a quote; a single one ends the string.
    a.lda(zp(os::TEXT).ind_y());
    a.cmp(imm(b'"'));
    a.bne("gsread_quote_end");
    a.iny();
    a.bne("gsread_char");
    a.here("gsread_quote_end");
    a.lda(imm(0));
    a.sta(zp(os::GS_QUOTE));
    a.beq("gsread_end");
    a.here("gsread_not_quote");
    a.cmp(imm(b'|'));
    a.bne("gsread_char");
    a.lda(zp(os::TEXT).ind_y());
    a.cmp(imm(0x0D));
    a.beq("gsread_bad");
    a.iny();
    a.cmp(imm(b'!'));
    a.bne("gsread_not_top_bit");
    a.lda(imm(0x80));
    a.sta(zp(os::GS_CHAR));
    a.bmi("gsread_next");
    a.here("gsread_not_top_bit");
    a.cmp(imm(b'?'));
    a.bne("gsread_not_delete");
    a.lda(imm(DELETE));
    a.bne("gsread_char");
    a.here("gsread_not_delete");
    a.cmp(imm(b'|'));
    a.beq("gsread_char");
    a.cmp(imm(b'"'));
    a.beq("gsread_char");
    a.cmp(imm(0x40));
    a.bcc("gsread_char");
    a.and(imm(0x1F));
    a.here("gsread_char");
    a.ora(zp(os::GS_CHAR));
    a.clc();
    a.rts();
}

/// Reading the numbers commands are given.
fn numbers(a: &mut Asm) {
    a.here("skip_spaces");
    a.lda(zp(os::TEXT).ind_y());
    a.cmp(imm(b' '));
    a.bne(RTS);
    a.iny();
    a.bne("skip_spaces");
    a.rts();

    a.here("skip_comma");
    a.jsr("skip_spaces");
    a.cmp(imm(b','));
    a.bne(RTS);
    a.iny();
    a.jmp("skip_spaces");

    // X = the decimal number at (&F2),Y after any spaces; C set if there is
    // none, or it passes 255.
    a.here("read_decimal");
    a.jsr("skip_spaces");
    a.jsr("digit_value");
    a.bcs(RTS);
    a.sta(zp(os::TEMP));
    a.here("read_decimal_next");
    a.iny();
    a.jsr("digit_value");
    a.bcs("read_decimal_done");
    a.lda(zp(os::TEMP));
    a.asl(Acc);
    a.bcs(RTS);
    a.asl(Acc);
    a.bcs(RTS);
    a.adc(zp(os::TEMP));
    a.bcs(RTS);
    a.asl(Acc);
    a.bcs(RTS);
    a.sta(zp(os::TEMP));
    a.jsr("digit_value");
    a.adc(zp(os::TEMP));
    a.bcs(RTS);
    a.sta(zp(os::TEMP));
    a.bcc("read_decimal_next");
    a.here("read_decimal_done");
    a.ldx(zp(os::TEMP));
    a.clc();
    a.rts();

    // A = the decimal digit at (&F2),Y; C set if it is not one.
    a.here("digit_value");
    a.lda(zp(os::TEXT).ind_y());
    a.sec();
    a.sbc(imm(b'0'));
    a.cmp(imm(10));
    a.rts();

    // Four bytes at the block offset X = the hexadecimal number at (&F2),Y;
    // C set, and the bytes left as they were, if there is none.
    a.here("read_hex_into");
    a.jsr("skip_spaces");
    a.jsr("hex_digit");
    a.bcs(RTS);
    a.pha();
    a.lda(imm(0));
    for offset in 0..4u16 {
        a.sta(abs(BLOCK + offset).x());
    }
    a.pla();
    a.here("read_hex_digit");
    a.sta(zp(os::TEMP));
    a.tya();
    a.pha();
    a.ldy(imm(4));
    a.here("read_hex_shift");
    a.asl(abs(BLOCK).x());
    for offset in 1..4u16 {
        a.rol(abs(BLOCK + offset).x());
    }
    a.dey();
    a.bne("read_hex_shift");
    a.pla();
    a.tay();
    a.lda(zp(os::TEMP));
    a.ora(abs(BLOCK).x());
    a.sta(abs(BLOCK).x());
    a.iny();
    a.jsr("hex_digit");
    a.bcc("read_hex_digit");
    a.clc();
    a.rts();

    // A = the hexadecimal digit at (&F2),Y; C set if it is not one.
    a.here("hex_digit");
    a.jsr("digit_value");
    a.bcc(RTS);
    a.lda(zp(os::TEXT).ind_y());
    a.and(imm(0xDF));
    a.sec();
    a.sbc(imm(b'A' - 10));
    a.cmp(imm(10));
    a.bcc("hex_not");
    a.cmp(imm(16));
    a.rts();
    a.here("hex_not");
    a.sec();
    a.rts();
}

fn tables(a: &mut Asm) {
    // Each name, then its routine's address less one, high byte first: a
    // routine in this ROM has the top bit set there. A zero ends the table.
    a.later(|a| {
        a.here("command_names");
        for (name, routine) in COMMANDS {
            a.text(name);
            a.byte(hi(sym(routine) + -1));
            a.byte(lo(sym(routine) + -1));
        }
        a.byte(0);
    });
}
