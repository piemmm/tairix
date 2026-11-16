//! The error channel: the last error as `nn,MESSAGE,tt,ss` and RETURN,
//! sent once, then `00, OK,00,00`.

use super::ws::{page, zp};
use crate::rom::asm::{abs, hi, imm, lo, zp as z, Asm, RTS};

/// The error numbers the DOS reports.
pub mod number {
    pub const OK: u8 = 0;
    pub const FILES_SCRATCHED: u8 = 1;
    pub const NO_HEADER: u8 = 20;
    pub const NO_SYNC: u8 = 21;
    pub const NO_DATA_BLOCK: u8 = 22;
    pub const DATA_CHECKSUM: u8 = 23;
    pub const BYTE_DECODING: u8 = 24;
    pub const VERIFY: u8 = 25;
    pub const WRITE_PROTECT: u8 = 26;
    pub const HEADER_CHECKSUM: u8 = 27;
    pub const LONG_DATA_BLOCK: u8 = 28;
    pub const ID_MISMATCH: u8 = 29;
    pub const SYNTAX: u8 = 30;
    pub const INVALID_COMMAND: u8 = 31;
    pub const LONG_LINE: u8 = 32;
    pub const INVALID_NAME: u8 = 33;
    pub const NO_NAME: u8 = 34;
    pub const UNRECOGNISED: u8 = 39;
    pub const RECORD_NOT_PRESENT: u8 = 50;
    pub const RECORD_OVERFLOW: u8 = 51;
    pub const FILE_TOO_LARGE: u8 = 52;
    pub const WRITE_FILE_OPEN: u8 = 60;
    pub const FILE_NOT_OPEN: u8 = 61;
    pub const FILE_NOT_FOUND: u8 = 62;
    pub const FILE_EXISTS: u8 = 63;
    pub const TYPE_MISMATCH: u8 = 64;
    pub const NO_BLOCK: u8 = 65;
    pub const ILLEGAL_BLOCK: u8 = 66;
    pub const ILLEGAL_SYSTEM_BLOCK: u8 = 67;
    pub const NO_CHANNEL: u8 = 70;
    pub const DIRECTORY: u8 = 71;
    pub const DISK_FULL: u8 = 72;
    pub const POWER_ON: u8 = 73;
    pub const NOT_READY: u8 = 74;
}

/// Each message with the numbers that report it.
const MESSAGES: [(&[u8], &str); 22] = {
    use number::{
        BYTE_DECODING, DATA_CHECKSUM, DIRECTORY, DISK_FULL, FILES_SCRATCHED, FILE_EXISTS,
        FILE_NOT_FOUND, FILE_NOT_OPEN, FILE_TOO_LARGE, HEADER_CHECKSUM, ID_MISMATCH, ILLEGAL_BLOCK,
        ILLEGAL_SYSTEM_BLOCK, INVALID_COMMAND, INVALID_NAME, LONG_DATA_BLOCK, LONG_LINE, NOT_READY,
        NO_BLOCK, NO_CHANNEL, NO_DATA_BLOCK, NO_HEADER, NO_NAME, NO_SYNC, OK, POWER_ON,
        RECORD_NOT_PRESENT, RECORD_OVERFLOW, SYNTAX, TYPE_MISMATCH, UNRECOGNISED, VERIFY,
        WRITE_FILE_OPEN, WRITE_PROTECT,
    };
    [
        (&[OK], " OK"),
        (&[FILES_SCRATCHED], "FILES SCRATCHED"),
        (
            &[
                NO_HEADER,
                NO_SYNC,
                NO_DATA_BLOCK,
                DATA_CHECKSUM,
                BYTE_DECODING,
                HEADER_CHECKSUM,
            ],
            "READ ERROR",
        ),
        (&[VERIFY, LONG_DATA_BLOCK], "WRITE ERROR"),
        (&[WRITE_PROTECT], "WRITE PROTECT ON"),
        (&[ID_MISMATCH], "DISK ID MISMATCH"),
        (
            &[
                SYNTAX,
                INVALID_COMMAND,
                LONG_LINE,
                INVALID_NAME,
                NO_NAME,
                UNRECOGNISED,
            ],
            "SYNTAX ERROR",
        ),
        (&[RECORD_NOT_PRESENT], "RECORD NOT PRESENT"),
        (&[RECORD_OVERFLOW], "OVERFLOW IN RECORD"),
        (&[FILE_TOO_LARGE], "FILE TOO LARGE"),
        (&[WRITE_FILE_OPEN], "WRITE FILE OPEN"),
        (&[FILE_NOT_OPEN], "FILE NOT OPEN"),
        (&[FILE_NOT_FOUND], "FILE NOT FOUND"),
        (&[FILE_EXISTS], "FILE EXISTS"),
        (&[TYPE_MISMATCH], "FILE TYPE MISMATCH"),
        (&[NO_BLOCK], "NO BLOCK"),
        (
            &[ILLEGAL_BLOCK, ILLEGAL_SYSTEM_BLOCK],
            "ILLEGAL TRACK OR SECTOR",
        ),
        (&[NO_CHANNEL], "NO CHANNEL"),
        (&[DIRECTORY], "DIR ERROR"),
        (&[DISK_FULL], "DISK FULL"),
        (&[POWER_ON], "OPENMICRO DOS 1541"),
        (&[NOT_READY], "DRIVE NOT READY"),
    ]
};

/// A number's byte in the table: its top bit tells it from a message's
/// characters, which follow the numbers that report them.
const NUMBER: u8 = 0x80;
/// The table's end.
const END: u8 = 0xFF;
/// A message's bytes beside its text: three numbers of two digits, each
/// followed by a comma or the RETURN, and the comma after the text.
const NUMBERS_BYTES: usize = 3 * 3 + 1;

const _: () = {
    let mut entry = 0;
    while entry < MESSAGES.len() {
        let (numbers, text) = MESSAGES[entry];
        let mut index = 0;
        while index < numbers.len() {
            assert!(numbers[index] < 100, "an error's number has two digits");
            index += 1;
        }
        let text = text.as_bytes();
        assert!(
            text.len() + NUMBERS_BYTES <= page::MESSAGE_BYTES as usize,
            "a message and its numbers fit the message buffer"
        );
        index = 0;
        while index < text.len() {
            assert!(
                text[index] < NUMBER,
                "a message's characters are below a number's bytes"
            );
            index += 1;
        }
        entry += 1;
    }
};

pub fn emit(a: &mut Asm) {
    record(a);
    channel(a);
    a.later(table);
}

/// `set_error`: error A at track X and sector Y; `set_ok`: no error. The
/// message is made when it is next read.
fn record(a: &mut Asm) {
    a.here("set_ok");
    a.lda(imm(number::OK));
    a.tax();
    a.tay();
    a.here("set_error");
    a.sta(z(zp::ERROR));
    a.stx(z(zp::ERROR_TRACK));
    a.sty(z(zp::ERROR_SECTOR));
    a.lda(imm(0));
    a.sta(z(zp::MESSAGE_LENGTH));
    a.sta(z(zp::MEMORY_STATE));
    a.rts();
}

/// `message_byte`: the message's next character in A, EOI's bit 7 set for
/// its RETURN; the message is made on its first character: the number, the
/// text, the track and the sector. An `M-R` reply goes first: its bytes,
/// then a RETURN with EOI. `message_taken` moves past it, the error
/// cleared once the message's RETURN is taken.
fn channel(a: &mut Asm) {
    a.here("message_byte");
    let (message, memory_end, made, not_last) = (a.local(), a.local(), a.local(), a.local());
    a.bit(z(zp::MEMORY_STATE));
    a.bpl(message);
    a.lda(z(zp::MEMORY_LEFT));
    a.ora(z(zp::MEMORY_LEFT + 1));
    a.beq(memory_end);
    a.lda(imm(0));
    a.sta(z(zp::EOI));
    a.tay();
    a.lda(z(zp::MEMORY_AT).ind_y());
    a.clc();
    a.rts();
    a.bind(memory_end);
    a.lda(imm(0x80));
    a.sta(z(zp::EOI));
    a.lda(imm(b'\r'));
    a.clc();
    a.rts();
    a.bind(message);
    a.lda(z(zp::MESSAGE_LENGTH));
    a.bne(made);
    a.jsr("make_message");
    a.bind(made);
    a.ldx(z(zp::MESSAGE_AT));
    a.inx();
    a.lda(imm(0));
    a.cpx(z(zp::MESSAGE_LENGTH));
    a.bne(not_last);
    a.lda(imm(0x80));
    a.bind(not_last);
    a.sta(z(zp::EOI));
    a.lda(abs(page::MESSAGE - 1).x());
    a.clc();
    a.rts();

    a.here("message_taken");
    let (message, more, low, counted) = (a.local(), a.local(), a.local(), a.local());
    a.bit(z(zp::MEMORY_STATE));
    a.bpl(message);
    a.lda(z(zp::MEMORY_LEFT));
    a.ora(z(zp::MEMORY_LEFT + 1));
    a.bne(more);
    a.sta(z(zp::MEMORY_STATE));
    a.rts();
    a.bind(more);
    a.inc(z(zp::MEMORY_AT));
    a.bne(low);
    a.inc(z(zp::MEMORY_AT + 1));
    a.bind(low);
    a.lda(z(zp::MEMORY_LEFT));
    a.bne(counted);
    a.dec(z(zp::MEMORY_LEFT + 1));
    a.bind(counted);
    a.dec(z(zp::MEMORY_LEFT));
    a.rts();
    a.bind(message);
    a.inc(z(zp::MESSAGE_AT));
    a.lda(z(zp::MESSAGE_AT));
    a.cmp(z(zp::MESSAGE_LENGTH));
    a.bne(RTS);
    a.jmp("set_ok");

    making(a);
}

/// `make_message`: the error's message put in the message buffer: the
/// number, the text, the track and the sector.
fn making(a: &mut Asm) {
    a.here("make_message");
    let (find, found, skip, copy, done) = (a.local(), a.local(), a.local(), a.local(), a.local());
    a.lda(imm(0));
    a.sta(z(zp::MESSAGE_LENGTH));
    a.lda(z(zp::ERROR));
    a.jsr("put_decimal");
    a.lda(imm(lo("error_messages")));
    a.sta(z(zp::POINTER));
    a.lda(imm(hi("error_messages")));
    a.sta(z(zp::POINTER + 1));
    a.ldy(imm(0));
    a.lda(z(zp::ERROR));
    a.ora(imm(NUMBER));
    a.bind(find);
    a.cmp(z(zp::POINTER).ind_y());
    a.beq(found);
    // C from the comparison with END survives the pull.
    a.pha();
    a.lda(z(zp::POINTER).ind_y());
    a.cmp(imm(END));
    a.pla();
    // An unlisted number has no text.
    a.bcs(done);
    a.jsr("next_entry_byte");
    a.jmp(find);
    a.bind(found);
    a.bind(skip);
    a.jsr("next_entry_byte");
    a.lda(z(zp::POINTER).ind_y());
    a.bmi(skip);
    a.bind(copy);
    a.jsr("put_char");
    a.jsr("next_entry_byte");
    a.lda(z(zp::POINTER).ind_y());
    a.bpl(copy);
    a.bind(done);
    a.lda(imm(b','));
    a.jsr("put_char");
    a.lda(z(zp::ERROR_TRACK));
    a.jsr("put_decimal");
    a.lda(z(zp::ERROR_SECTOR));
    a.jsr("put_decimal");
    // The last number's comma becomes the RETURN.
    a.ldx(z(zp::MESSAGE_LENGTH));
    a.lda(imm(b'\r'));
    a.sta(abs(page::MESSAGE - 1).x());
    a.lda(imm(0));
    a.sta(z(zp::MESSAGE_AT));
    a.rts();

    a.here("next_entry_byte");
    a.inc(z(zp::POINTER));
    a.bne(RTS);
    a.inc(z(zp::POINTER + 1));
    a.rts();

    // A's two digits and a comma, each put at the message's end.
    a.here("put_decimal");
    let tens = a.local();
    a.ldy(imm(b'0' - 1));
    a.sec();
    a.bind(tens);
    a.iny();
    a.sbc(imm(10));
    a.bcs(tens);
    a.adc(imm(10 + b'0'));
    a.pha();
    a.tya();
    a.jsr("put_char");
    a.pla();
    a.jsr("put_char");
    a.lda(imm(b','));
    a.here("put_char");
    a.ldx(z(zp::MESSAGE_LENGTH));
    a.sta(abs(page::MESSAGE).x());
    a.inc(z(zp::MESSAGE_LENGTH));
    a.rts();
}

fn table(a: &mut Asm) {
    a.here("error_messages");
    for (numbers, text) in MESSAGES {
        for &number in numbers {
            a.byte(NUMBER | number);
        }
        a.bytes(text.as_bytes());
    }
    a.byte(END);
}
