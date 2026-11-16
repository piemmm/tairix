//! The command channel's commands: the buffer's text, a RETURN at its end
//! dropped, carried out by its first letter with the error it leaves.
//! A command naming files takes them after its colon, parted by commas
//! and `=`, each after any drive's colon of its own.

use super::errors::number;
use super::files::{mode, named_block, KIND_BITS};
use super::ws::{page, zp, PATTERNS};
use crate::media::c1541::layout::{entry, kind, DIRECTORY_TRACK};
use crate::rom::asm::{abs, byte, hi, imm, lo, sym, zp as z, Asm, RTS};

/// The commands by their first letter.
const COMMANDS: [(u8, &str); 10] = [
    (b'I', "reinitialize"),
    (b'N', "format_command"),
    (b'S', "scratch"),
    (b'R', "rename"),
    (b'V', "validate"),
    (b'C', "copy"),
    (b'U', "user_command"),
    (b'B', "block_command"),
    (b'M', "memory_command"),
    (b'P', "position_command"),
];
/// The block the original reports a copy of a relative file's DIR ERROR
/// at, track and sector alike.
const RELATIVE_COPY_AT: u8 = 2;

/// A letter's table for `dispatch`: its count, its letters, then each
/// routine's address less one, low bytes then high, for the RTS that
/// enters it.
pub(super) fn table(a: &mut Asm, name: &'static str, entries: &[(u8, &'static str)]) {
    a.here(name);
    a.byte(byte(entries.len()));
    for &(letter, _) in entries {
        a.byte(letter);
    }
    for &(_, routine) in entries {
        a.byte(lo(sym(routine) + -1));
    }
    for &(_, routine) in entries {
        a.byte(hi(sym(routine) + -1));
    }
}

pub fn emit(a: &mut Asm) {
    execute(a);
    names(a);
    named(a);
    scratch(a);
    rename(a);
    validate(a);
    copy(a);
    a.later(|a| table(a, "commands", &COMMANDS));
}

/// `execute`: the command in the buffer. Too long a command is LONG LINE;
/// one no letter names, or none at all, INVALID COMMAND. `dispatch`: the
/// routine the table at `POINTER` gives letter A entered, INVALID COMMAND
/// where it gives none. `reinitialize`: `I`, and the start of `V` and `C`:
/// every open file given up unfinished, as the original's are, then the
/// BAM read again.
fn execute(a: &mut Asm) {
    a.here("execute");
    let (kept, invalid, long) = (a.local(), a.local(), a.local());
    a.jsr("set_ok");
    a.ldx(z(zp::COMMAND_LENGTH));
    a.cpx(imm(page::COMMAND_BYTES + 1));
    a.beq(long);
    a.txa();
    a.beq(kept);
    a.lda(abs(page::COMMAND - 1).x());
    a.cmp(imm(b'\r'));
    a.bne(kept);
    a.dec(z(zp::COMMAND_LENGTH));
    a.bind(kept);
    a.lda(z(zp::COMMAND_LENGTH));
    a.beq(invalid);
    a.lda(imm(lo("commands")));
    a.sta(z(zp::POINTER));
    a.lda(imm(hi("commands")));
    a.sta(z(zp::POINTER + 1));
    a.lda(abs(page::COMMAND));
    a.fall_into("dispatch");

    a.here("dispatch");
    let find = a.local();
    a.sta(z(zp::VALUE));
    a.ldy(imm(0));
    a.lda(z(zp::POINTER).ind_y());
    a.sta(z(zp::COUNT));
    a.bind(find);
    a.cpy(z(zp::COUNT));
    a.bcs(invalid);
    a.iny();
    a.lda(z(zp::POINTER).ind_y());
    a.cmp(z(zp::VALUE));
    a.bne(find);
    // The letter's place counted from 1: its address's high byte two
    // counts on, its low byte one.
    a.tya();
    a.clc();
    a.adc(z(zp::COUNT));
    a.adc(z(zp::COUNT));
    a.tay();
    a.lda(z(zp::POINTER).ind_y());
    a.pha();
    a.tya();
    a.sec();
    a.sbc(z(zp::COUNT));
    a.tay();
    a.lda(z(zp::POINTER).ind_y());
    a.pha();
    a.rts();
    a.bind(invalid);
    a.lda(imm(number::INVALID_COMMAND));
    a.skip_over(|a| {
        a.bind(long);
        a.lda(imm(number::LONG_LINE));
    });
    a.jmp("error_at_zero");

    a.here("reinitialize");
    a.jsr("drop_files");
    a.jmp("initialize");
}

/// `parse_names`: the names after the command's colon, parted by commas
/// and `=`, five at most kept, each after any drive's colon of its own:
/// `NAME_STARTS` and `NAME_ENDS`, `NAME_COUNT`, and `NAME_EQUALS` the first
/// after `=`, 0 for none. No colon is SYNTAX ERROR for want of a name,
/// drive 1 DRIVE NOT READY; C set.
fn names(a: &mut Asm) {
    a.here("parse_names");
    let (colon, after, name, scan, end, counted, comma, done) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.ldx(imm(0));
    a.jsr("find_colon");
    a.bcc(colon);
    a.lda(imm(number::NO_NAME));
    a.jmp("error_at_zero");
    a.bind(colon);
    a.jsr("second_drive");
    a.bne(after);
    a.lda(imm(number::NOT_READY));
    a.jmp("error_at_zero");
    a.bind(after);
    a.lda(imm(0));
    a.sta(z(zp::NAME_COUNT));
    a.sta(z(zp::NAME_EQUALS));
    a.inx();
    a.bind(name);
    a.stx(z(zp::PATTERN_FROM));
    a.bind(scan);
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(end);
    a.lda(abs(page::COMMAND).x());
    a.cmp(imm(b','));
    a.beq(end);
    a.cmp(imm(b'='));
    a.beq(end);
    a.inx();
    a.cmp(imm(b':'));
    a.bne(scan);
    a.beq(name);
    a.bind(end);
    a.ldy(z(zp::NAME_COUNT));
    a.cpy(imm(PATTERNS));
    a.bcs(counted);
    a.lda(z(zp::PATTERN_FROM));
    a.sta(abs(zp::NAME_STARTS).y());
    a.txa();
    a.sta(abs(zp::NAME_ENDS).y());
    a.bind(counted);
    a.inc(z(zp::NAME_COUNT));
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(done);
    a.lda(abs(page::COMMAND).x());
    a.cmp(imm(b'='));
    a.bne(comma);
    a.lda(z(zp::NAME_COUNT));
    a.sta(z(zp::NAME_EQUALS));
    a.bind(comma);
    a.inx();
    a.bne(name);
    a.bind(done);
    // Both counts name kept names only: five at most.
    for count in [zp::NAME_COUNT, zp::NAME_EQUALS] {
        let capped = a.local();
        a.lda(z(count));
        a.cmp(imm(PATTERNS + 1));
        a.bcc(capped);
        a.lda(imm(PATTERNS));
        a.sta(z(count));
        a.bind(capped);
    }
    a.clc();
    a.rts();
}

/// `parse_pair`: the names, SYNTAX ERROR without one each side of a lone
/// `=` or with a wildcard in any, as the original refuses them.
/// `name_pattern`: name Y the pattern matched, found whatever its kind;
/// `name_parsed`: name Y as a new file's name. `not_found`: FILE NOT
/// FOUND, where a read has not set its own error; C set.
fn named(a: &mut Asm) {
    a.here("parse_pair");
    let (syntax, each) = (a.local(), a.local());
    a.jsr("parse_names");
    a.bcs(RTS);
    a.lda(z(zp::NAME_COUNT));
    a.cmp(imm(2));
    a.bcc(syntax);
    a.lda(z(zp::NAME_EQUALS));
    a.cmp(imm(1));
    a.bne(syntax);
    a.ldy(imm(0));
    a.bind(each);
    a.lda(abs(zp::NAME_ENDS).y());
    a.sta(z(zp::COUNT));
    a.ldx(abs(zp::NAME_STARTS).y());
    a.jsr("wildcard_in");
    a.bcs(syntax);
    a.iny();
    a.cpy(z(zp::NAME_COUNT));
    a.bne(each);
    a.clc();
    a.rts();
    a.bind(syntax);
    a.lda(imm(number::SYNTAX));
    a.jmp("error_at_zero");

    a.here("name_pattern");
    a.lda(imm(0));
    a.sta(z(zp::FIND_KIND));
    a.ldx(abs(zp::NAME_STARTS).y());
    a.lda(abs(zp::NAME_ENDS).y());
    a.tay();
    a.lda(imm(lo(page::COMMAND)));
    a.jmp("set_pattern");

    a.here("name_parsed");
    a.lda(abs(zp::NAME_STARTS).y());
    a.sta(z(zp::NAME_START));
    a.lda(abs(zp::NAME_ENDS).y());
    a.sta(z(zp::NAME_END));
    a.rts();

    a.here("not_found");
    a.lda(z(zp::ERROR));
    a.bne(RTS);
    a.lda(imm(number::FILE_NOT_FOUND));
    a.jmp("error_at_zero");
}

/// `S`: each file a name before any `=` matches scratched, its type byte
/// zeroed and, if it was closed, its blocks freed, a relative file's side
/// sectors with them; a locked file left. FILES SCRATCHED, with the count
/// as its track, once the BAM is written. A scratch that fails leaves the
/// disk to be read again.
fn scratch(a: &mut Asm) {
    a.here("scratch");
    let (each_name, each_entry, next, scratched, failed, counted) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    let data = a.local();
    a.jsr("parse_names");
    a.bcs(RTS);
    a.ldy(z(zp::NAME_EQUALS));
    a.beq(counted);
    a.sty(z(zp::NAME_COUNT));
    a.bind(counted);
    a.jsr("ensure_ready");
    a.bcs(RTS);
    a.jsr("hold_buffer");
    a.bcs(RTS);
    a.stx(z(zp::SOURCE));
    a.lda(imm(0));
    a.sta(z(zp::DONE_COUNT));
    a.sta(z(zp::PATTERN_INDEX));
    a.bind(each_name);
    a.ldy(z(zp::PATTERN_INDEX));
    a.cpy(z(zp::NAME_COUNT));
    a.beq(scratched);
    a.jsr("name_pattern");
    a.jsr("first_entry");
    a.bcs(failed);
    a.bind(each_entry);
    a.ldy(imm(entry::TYPE));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.beq(next);
    a.and(imm(kind::LOCKED));
    a.bne(next);
    a.jsr("name_matches");
    a.bne(next);
    a.ldy(imm(entry::TYPE));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.pha();
    a.lda(imm(0));
    a.sta(z(zp::DIRECTORY_ENTRY).ind_y());
    a.inc(z(zp::DONE_COUNT));
    a.jsr("write_directory");
    a.pla();
    a.bcs(failed);
    a.bpl(next);
    a.and(imm(KIND_BITS));
    a.cmp(imm(kind::REL));
    a.bne(data);
    a.ldy(imm(entry::SIDE));
    named_block(a, zp::DIRECTORY_ENTRY);
    a.ldx(z(zp::SOURCE));
    a.jsr("free_chain");
    a.bcs(failed);
    a.bind(data);
    a.ldy(imm(entry::TRACK));
    named_block(a, zp::DIRECTORY_ENTRY);
    a.ldx(z(zp::SOURCE));
    a.jsr("free_chain");
    a.bcs(failed);
    a.bind(next);
    a.jsr("next_entry");
    a.bcc(each_entry);
    a.lda(z(zp::ERROR));
    a.bne(failed);
    a.inc(z(zp::PATTERN_INDEX));
    a.jmp(each_name);
    a.bind(scratched);
    a.jsr("write_bam");
    a.bcs(failed);
    a.lda(imm(number::FILES_SCRATCHED));
    a.ldx(z(zp::DONE_COUNT));
    a.ldy(imm(0));
    a.jmp("set_error");
    a.bind(failed);
    a.jmp("forget_disk");
}

/// `R`: the file after `=` given the name before it, sixteen characters at
/// most: SYNTAX ERROR without exactly one each side, FILE EXISTS where the
/// new name is taken, FILE NOT FOUND where the old is absent.
fn rename(a: &mut Asm) {
    a.here("rename");
    let (missing, exists, pair) = (a.local(), a.local(), a.local());
    a.jsr("parse_pair");
    a.bcs(RTS);
    a.lda(z(zp::NAME_COUNT));
    a.cmp(imm(2));
    a.beq(pair);
    a.lda(imm(number::SYNTAX));
    a.jmp("error_at_zero");
    a.bind(pair);
    a.jsr("ensure_ready");
    a.bcs(RTS);
    a.ldy(imm(0));
    a.jsr("name_pattern");
    a.jsr("find_entry");
    a.bcc(exists);
    a.lda(z(zp::ERROR));
    a.bne(RTS);
    a.ldy(imm(1));
    a.jsr("name_pattern");
    a.jsr("find_entry");
    a.bcs(missing);
    a.ldy(imm(0));
    a.jsr("name_parsed");
    a.jsr("put_name");
    a.jsr("write_directory");
    a.rts();
    a.bind(missing);
    a.jmp("not_found");
    a.bind(exists);
    a.lda(imm(number::FILE_EXISTS));
    a.jmp("error_at_zero");
}

/// `V`: the BAM made again from the directory: every block free but the
/// BAM's, then the directory's sectors and each closed file's chain used,
/// a relative file's side sectors with it; a file never closed is deleted.
/// One that fails part-way leaves the disk to be read again, where the
/// original keeps its half-made BAM and lets the next file overwrite one it
/// had yet to reach. `forget_disk`: the disk to be read again, its BAM in
/// memory no longer to be trusted; C set.
fn validate(a: &mut Asm) {
    a.here("validate");
    let (sector, each, closed, next, end) = (a.local(), a.local(), a.local(), a.local(), a.local());
    let data = a.local();
    a.jsr("reinitialize");
    a.bcs(RTS);
    a.jsr("hold_buffer");
    a.bcs(RTS);
    a.stx(z(zp::SOURCE));
    a.jsr("clear_bam");
    a.jsr("first_entry");
    a.bcs("forget_disk");
    a.bind(sector);
    a.lda(imm(DIRECTORY_TRACK));
    a.ldy(z(zp::DIRECTORY_SECTOR));
    a.jsr("use_block");
    a.bind(each);
    a.ldy(imm(entry::TYPE));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.beq(next);
    a.bmi(closed);
    a.lda(imm(0));
    a.sta(z(zp::DIRECTORY_ENTRY).ind_y());
    a.jsr("write_directory");
    a.bcs("forget_disk");
    a.bcc(next);
    a.bind(closed);
    a.and(imm(KIND_BITS));
    a.cmp(imm(kind::REL));
    a.bne(data);
    a.ldy(imm(entry::SIDE));
    named_block(a, zp::DIRECTORY_ENTRY);
    a.ldx(z(zp::SOURCE));
    a.jsr("use_chain");
    a.bcs("forget_disk");
    a.bind(data);
    a.ldy(imm(entry::TRACK));
    named_block(a, zp::DIRECTORY_ENTRY);
    a.ldx(z(zp::SOURCE));
    a.jsr("use_chain");
    a.bcs("forget_disk");
    // A sector just read is at its first entry.
    a.bind(next);
    a.jsr("next_entry");
    a.bcs(end);
    a.lda(z(zp::DIRECTORY_ENTRY));
    a.beq(sector);
    a.bne(each);
    a.bind(end);
    a.lda(z(zp::ERROR));
    a.bne("forget_disk");
    a.jsr("write_bam");
    a.bcc(RTS);
    a.here("forget_disk");
    a.lda(imm(0));
    a.sta(z(zp::DISK_READY));
    a.sec();
    a.rts();
}

/// `C`: a file made of the files after `=` in turn, named before it and of
/// the first one's kind: SYNTAX ERROR without both, FILE NOT FOUND where a
/// source is absent, FILE EXISTS where the name is taken. Every source is
/// found, its first block kept, before the file is made, so a missing one
/// leaves nothing made; a relative one is DIR ERROR, as the original
/// reports it, nothing made. The bytes go through two channel buffers,
/// every file given up first; a copy that fails leaves its file unclosed
/// and the disk to be read again. `copy_sources`: each source written in
/// turn to file TARGET, read through file SOURCE; C set where one cannot
/// be.
fn copy(a: &mut Asm) {
    a.here("copy");
    let (check, checked, missing, failed) = (a.local(), a.local(), a.local(), a.local());
    let relative = a.local();
    a.jsr("parse_pair");
    a.bcs(RTS);
    a.jsr("reinitialize");
    a.bcs(RTS);
    a.jsr("hold_buffer");
    a.bcs(RTS);
    a.stx(z(zp::TARGET));
    // The source's buffer is the target's until it has its own, so a copy
    // that fails before then frees only what it took.
    a.stx(z(zp::SOURCE));
    a.lda(imm(1));
    a.sta(z(zp::PATTERN_INDEX));
    a.bind(check);
    a.ldy(z(zp::PATTERN_INDEX));
    a.jsr("name_pattern");
    a.jsr("find_entry");
    a.bcs(missing);
    a.ldy(imm(entry::TYPE));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.and(imm(KIND_BITS));
    a.cmp(imm(kind::REL));
    a.beq(relative);
    a.ldx(z(zp::PATTERN_INDEX));
    a.ldy(imm(entry::TRACK));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.sta(abs(page::COPY_TRACKS - 1).x());
    a.ldy(imm(entry::SECTOR));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.sta(abs(page::COPY_SECTORS - 1).x());
    a.cpx(imm(1));
    a.bne(checked);
    a.ldy(imm(entry::TYPE));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.and(imm(!kind::CLOSED));
    a.ldx(z(zp::TARGET));
    a.sta(abs(page::FILE_KIND).x());
    a.bind(checked);
    a.inc(z(zp::PATTERN_INDEX));
    a.lda(z(zp::PATTERN_INDEX));
    a.cmp(z(zp::NAME_COUNT));
    a.bcc(check);
    a.ldy(imm(0));
    a.jsr("name_parsed");
    a.ldx(z(zp::TARGET));
    a.jsr("create_file");
    a.bcs(RTS);
    a.jsr("copy_sources");
    a.php();
    a.lda(imm(mode::FREE));
    a.ldx(z(zp::SOURCE));
    a.sta(abs(page::FILE_MODE).x());
    a.ldx(z(zp::TARGET));
    a.plp();
    a.bcs(failed);
    a.jsr("close_write");
    a.lda(imm(mode::FREE));
    a.sta(abs(page::FILE_MODE).x());
    a.rts();
    a.bind(failed);
    a.lda(imm(mode::FREE));
    a.sta(abs(page::FILE_MODE).x());
    a.jmp("forget_disk");
    a.bind(missing);
    a.jmp("not_found");
    a.bind(relative);
    a.lda(imm(number::DIRECTORY));
    a.ldx(imm(RELATIVE_COPY_AT));
    a.ldy(imm(RELATIVE_COPY_AT));
    a.jmp("set_error");

    sources(a);
}

/// `copy` run on: `copy_sources`.
fn sources(a: &mut Asm) {
    a.here("copy_sources");
    let (each, bytes, ended, next) = (a.local(), a.local(), a.local(), a.local());
    a.jsr("hold_buffer");
    a.bcs(RTS);
    a.stx(z(zp::SOURCE));
    a.lda(imm(1));
    a.sta(z(zp::PATTERN_INDEX));
    a.bind(each);
    a.ldy(z(zp::PATTERN_INDEX));
    a.lda(abs(page::COPY_TRACKS - 1).y());
    a.pha();
    a.lda(abs(page::COPY_SECTORS - 1).y());
    a.tay();
    a.pla();
    a.ldx(z(zp::SOURCE));
    a.jsr("read_from");
    a.bcs(RTS);
    a.bind(bytes);
    a.lda(abs(page::FILE_MODE).x());
    a.cmp(imm(mode::READ));
    a.bne(ended);
    a.lda(abs(page::FILE_BYTE).x());
    a.ldx(z(zp::TARGET));
    a.jsr("write_file_byte");
    a.bcs(RTS);
    a.ldx(z(zp::SOURCE));
    a.lda(abs(page::FILE_EOI).x());
    a.bne(next);
    a.jsr("fetch_byte");
    a.jmp(bytes);
    // A block that could not be read ended the file with its error set.
    a.bind(ended);
    a.sec();
    a.rts();
    a.bind(next);
    a.inc(z(zp::PATTERN_INDEX));
    a.lda(z(zp::PATTERN_INDEX));
    a.cmp(z(zp::NAME_COUNT));
    a.bcc(each);
    a.clc();
    a.rts();
}
