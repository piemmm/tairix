//! The drive's memory from the controller, as the original's commands give
//! it: `M-R` reads bytes back through the error channel, `M-W` writes
//! them, `M-E` runs code; and the user commands: `U3` to `U8` run code in
//! buffer 2's jump table and `U9` restarts the drive, while `UI+` and
//! `UI-`, the original's choice of serial speed, are taken and change
//! nothing.

use super::commands::table;
use super::errors::number;
use super::ws::{buffer, page, zp};
use crate::rom::asm::{abs, hi, imm, lo, zp as z, Acc, Asm, RTS};

/// The memory commands by the letter after `M-`.
const MEMORY_COMMANDS: [(u8, &str); 3] = [
    (b'R', "memory_read"),
    (b'W', "memory_write"),
    (b'E', "memory_execute"),
];
/// A memory command's bytes: its letters, the address, low byte first,
/// and the count of bytes.
const ADDRESS_AT: u8 = 3;
const COUNT_AT: u8 = 5;
/// The bytes `M-W` writes, after its count.
const DATA_AT: u8 = 6;
/// The most an `M-W` can carry in the command buffer.
const MOST_WRITTEN: u8 = page::COMMAND_BYTES - DATA_AT;
/// The jump table `U3` runs the first entry of, each three bytes on.
const USER_TABLE: u16 = buffer(2);
const USER_ENTRY_BYTES: u8 = 3;
/// The user command letters by number: `1` to `9`, or `A` to `I`.
const FIRST_DIGIT: u8 = b'0';
const FIRST_LETTER: u8 = b'@';
const USER_COMMANDS: u8 = 9;
/// The first user command that runs a table entry, and `U9`.
const FIRST_JUMP: u8 = 3;
const RESTART: u8 = 9;

const _: () = assert!(
    USER_TABLE.trailing_zeros() >= 8 && USER_ENTRY_BYTES == 3,
    "an entry's address is its number times three into the table's page"
);

pub fn emit(a: &mut Asm) {
    memory(a);
    user(a);
    a.later(|a| table(a, "memory_commands", &MEMORY_COMMANDS));
}

/// `memory_command`: `M-` and the letter after it, the address after
/// them; INVALID COMMAND for another letter. `memory_read`: the error
/// channel's next reply the count of bytes from the address, one where
/// none is given and 256 for 0, then a RETURN with EOI, the error
/// message after it as ever. `memory_write`: the count of bytes after it
/// written from the address. `memory_execute`: the code at the address
/// run, as `call_memory` runs `MEMORY_AT`'s.
fn memory(a: &mut Asm) {
    a.here("memory_command");
    let invalid = a.local();
    a.lda(abs(page::COMMAND + 1));
    a.cmp(imm(b'-'));
    a.bne(invalid);
    a.lda(abs(page::COMMAND + u16::from(ADDRESS_AT)));
    a.sta(z(zp::MEMORY_AT));
    a.lda(abs(page::COMMAND + u16::from(ADDRESS_AT) + 1));
    a.sta(z(zp::MEMORY_AT + 1));
    a.lda(imm(lo("memory_commands")));
    a.sta(z(zp::POINTER));
    a.lda(imm(hi("memory_commands")));
    a.sta(z(zp::POINTER + 1));
    a.lda(abs(page::COMMAND + 2));
    a.jmp("dispatch");
    a.bind(invalid);
    a.lda(imm(number::INVALID_COMMAND));
    a.jmp("error_at_zero");

    a.here("memory_read");
    let (counted, set) = (a.local(), a.local());
    a.lda(imm(1));
    a.ldx(z(zp::COMMAND_LENGTH));
    a.cpx(imm(COUNT_AT + 1));
    a.bcc(counted);
    a.lda(abs(page::COMMAND + u16::from(COUNT_AT)));
    a.bind(counted);
    a.sta(z(zp::MEMORY_LEFT));
    a.ldx(imm(0));
    a.tay();
    a.bne(set);
    a.inx();
    a.bind(set);
    a.stx(z(zp::MEMORY_LEFT + 1));
    a.lda(imm(0x80));
    a.sta(z(zp::MEMORY_STATE));
    a.rts();

    a.here("memory_write");
    let (copy, room) = (a.local(), a.local());
    a.ldx(abs(page::COMMAND + u16::from(COUNT_AT)));
    a.cpx(imm(MOST_WRITTEN + 1));
    a.bcc(room);
    a.ldx(imm(MOST_WRITTEN));
    a.bind(room);
    a.ldy(imm(0));
    a.bind(copy);
    a.dex();
    a.bmi(RTS);
    a.lda(abs(page::COMMAND + u16::from(DATA_AT)).y());
    a.sta(z(zp::MEMORY_AT).ind_y());
    a.iny();
    a.bne(copy);
    a.rts();

    a.here("memory_execute");
    a.here("call_memory");
    a.jmp(abs(zp::MEMORY_AT).ind());
}

/// `user_command`: `U` and a digit or letter: `1` reads a block and `2`
/// writes one, as `B-R` and `B-W` but from their first byte; `3` to `8`
/// run the jump table in buffer 2; `9` restarts the drive, `UI+` and
/// `UI-` taken as they are; `J` and `:` restart it too. INVALID COMMAND
/// for another.
fn user(a: &mut Asm) {
    a.here("user_command");
    let (restart, numbered, not_read, not_write, invalid) =
        (a.local(), a.local(), a.local(), a.local(), a.local());
    a.lda(abs(page::COMMAND + 1));
    a.cmp(imm(b'J'));
    a.beq(restart);
    a.cmp(imm(b':'));
    a.beq(restart);
    a.sec();
    a.sbc(imm(FIRST_DIGIT));
    a.cmp(imm(USER_COMMANDS + 1));
    a.bcc(numbered);
    a.lda(abs(page::COMMAND + 1));
    a.sec();
    a.sbc(imm(FIRST_LETTER));
    a.cmp(imm(USER_COMMANDS + 1));
    a.bcs(invalid);
    a.bind(numbered);
    a.tax();
    a.beq(invalid);
    a.dex();
    a.bne(not_read);
    a.jmp("user_read");
    a.bind(not_read);
    a.dex();
    a.bne(not_write);
    a.jmp("user_write");
    a.bind(not_write);
    a.cmp(imm(RESTART));
    a.bne("user_jump");
    a.lda(abs(page::COMMAND + 2));
    a.cmp(imm(b'+'));
    a.beq(RTS);
    a.cmp(imm(b'-'));
    a.beq(RTS);
    a.bind(restart);
    a.jmp("reset");
    a.bind(invalid);
    a.lda(imm(number::INVALID_COMMAND));
    a.jmp("error_at_zero");

    a.here("user_jump");
    a.sec();
    a.sbc(imm(FIRST_JUMP));
    a.sta(z(zp::VALUE));
    a.asl(Acc);
    a.adc(z(zp::VALUE));
    a.sta(z(zp::MEMORY_AT));
    a.lda(imm(hi(USER_TABLE)));
    a.sta(z(zp::MEMORY_AT + 1));
    a.jmp("call_memory");
}
