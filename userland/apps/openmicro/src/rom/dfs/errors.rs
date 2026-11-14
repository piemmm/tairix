//! Errors. A sideways ROM's error is copied to the foot of the stack and
//! raised there, since the language that reports it pages this ROM out
//! before it reads the message.

use crate::rom::asm::{abs, imm, zp, Acc, Asm};

use super::ws::zp as z;

/// Where a raised error is built: a BRK, its number, its message, a zero.
const AT: u16 = 0x0100;

/// Each error's routine, number and message, as DFS 1.20 gives them.
const ERRORS: [(&str, u8, &str); 20] = [
    ("err_escape", 17, "Escape"),
    ("err_cat_full", 0xBE, "Cat full"),
    ("err_cant_extend", 0xBF, "Can't extend"),
    ("err_too_many_open", 0xC0, "Too many open"),
    ("err_channel_read_only", 0xC1, "Read only"),
    ("err_open", 0xC2, "Open"),
    ("err_locked", 0xC3, "Locked"),
    ("err_exists", 0xC4, "Exists"),
    ("err_disk_full", 0xC6, "Disk full"),
    ("err_disk_changed", 0xC8, "Disk changed"),
    ("err_read_only", 0xC9, "Disk read only"),
    ("err_bad_option", 0xCB, "Bad option"),
    ("err_bad_name", 0xCC, "Bad name"),
    ("err_bad_drive", 0xCD, "Bad drive"),
    ("err_bad_dir", 0xCE, "Bad dir"),
    ("err_bad_attribute", 0xCF, "Bad attribute"),
    ("err_not_found", 0xD6, "Not found"),
    ("err_channel", 0xDE, "Channel"),
    ("err_eof", 0xDF, "EOF"),
    ("err_bad_command", 0xFE, "Bad command"),
];

/// A disc fault's number, and a syntax error's.
const DISK_FAULT: u8 = 0xC7;
pub(super) const SYNTAX: u8 = 0xDC;

pub(super) fn emit(a: &mut Asm) {
    for (routine, number, message) in ERRORS {
        a.here(routine);
        a.jsr("raise");
        a.byte(number);
        a.text(message);
        a.byte(0);
    }

    // The number and message after the JSR, raised.
    a.here("raise");
    a.pla();
    a.sta(zp(z::PRINT));
    a.pla();
    a.sta(zp(z::PRINT + 1));
    a.ldy(imm(0));
    a.sty(abs(AT));
    let copy = a.here_local();
    a.iny();
    a.lda(zp(z::PRINT).ind_y());
    a.sta(abs(AT).y());
    a.bne(copy);
    a.jmp(abs(AT));

    // An error built a part at a time: `error_start` with its number in A,
    // `error_text` for the text after its JSR, `error_hex` for A in hex,
    // then `error_raise`. X is the next byte's offset.
    a.here("error_start");
    a.sta(abs(AT + 1));
    a.lda(imm(0));
    a.sta(abs(AT));
    a.ldx(imm(2));
    a.rts();

    a.here("error_text");
    a.pla();
    a.sta(zp(z::PRINT));
    a.pla();
    a.sta(zp(z::PRINT + 1));
    a.ldy(imm(0));
    let next = a.here_local();
    a.iny();
    a.lda(zp(z::PRINT).ind_y());
    let done = a.local();
    a.beq(done);
    a.jsr("error_char");
    a.bne(next);
    // On past the zero.
    a.bind(done);
    a.tya();
    a.sec();
    a.adc(zp(z::PRINT));
    a.sta(zp(z::PRINT));
    let same = a.local();
    a.bcc(same);
    a.inc(zp(z::PRINT + 1));
    a.bind(same);
    a.jmp(abs(z::PRINT).ind());

    a.here("error_hex");
    a.pha();
    for _ in 0..4 {
        a.lsr(Acc);
    }
    a.jsr("error_digit");
    a.pla();
    a.here("error_digit");
    a.jsr("hex_digit");
    a.here("error_char");
    a.sta(abs(AT).x());
    a.inx();
    a.rts();

    a.here("error_raise");
    a.lda(imm(0));
    a.sta(abs(AT).x());
    a.jmp(abs(AT));

    // "Disk fault" with the 8271's result in A, at the command's track and
    // first sector.
    a.here("disk_fault");
    a.pha();
    a.lda(imm(DISK_FAULT));
    a.jsr("error_start");
    a.jsr("error_text");
    a.text("Disk fault ");
    a.byte(0);
    a.pla();
    a.jsr("error_hex");
    a.jsr("error_text");
    a.text(" at ");
    a.byte(0);
    a.lda(abs(super::ws::page::OP_TRACK));
    a.jsr("error_hex");
    a.lda(imm(b'/'));
    a.jsr("error_char");
    a.lda(abs(super::ws::page::OP_SECTOR_ON_TRACK));
    a.jsr("error_hex");
    a.jmp("error_raise");
}
