//! The file layer: jobs given to the controller and waited for, the disk
//! made ready by reading its BAM, names parsed and found in the directory,
//! and the data channels' files. A file is the channel buffer it holds,
//! its state kept beside the buffer; a channel names its file. A file
//! being read holds the next byte to send, fetched a byte ahead, so a
//! block's successor is read before its last byte goes, as the original's
//! is: a successor that cannot be read loses that byte and ends the file.

use super::controller::code;
use super::errors::number;
use super::ws::{page, zp, BAM_BUFFER, BUFFERS, CHANNEL_BUFFERS, NO_FILE};
use crate::machine::c64::serial::channel;
use crate::media::c1541::layout::{
    entry, kind, BAM_SECTOR, DIRECTORY_TRACK, FIRST_DIRECTORY_SECTOR, LINK_BYTES, NAME_BYTES,
    PADDING, TRACKS,
};
use crate::media::c1541::recording::job;
use crate::rom::asm::{abs, byte, hi, imm, lo, zp as z, Acc, Asm, Label, RTS};

/// What a file does.
pub mod mode {
    /// The buffer holds no file.
    pub const FREE: u8 = 0;
    /// A file's blocks read in turn.
    pub const READ: u8 = 1;
    /// The directory listing, put together a chunk of lines at a time.
    pub const LISTING: u8 = 2;
    /// Past its last byte, or stopped by an error: nothing more to send or
    /// take.
    pub const ENDED: u8 = 3;
    /// A file written a block at a time.
    pub const WRITE: u8 = 4;
    /// Held for the request in hand: the directory's sector, or a chain
    /// it walks.
    pub const HELD: u8 = 5;
    /// A buffer read and written directly through its channel.
    pub const DIRECT: u8 = 6;
    /// The relative file, its records read and written through its channel.
    pub const REL: u8 = 7;
    /// The relative file's side sector.
    pub const SIDE: u8 = 8;
}

/// A and Y the block named at `pointer`'s offset Y, track then sector.
pub(super) fn named_block(a: &mut Asm, pointer: u16) {
    a.lda(z(pointer).ind_y());
    a.pha();
    a.iny();
    a.lda(z(pointer).ind_y());
    a.tay();
    a.pla();
}

/// A type byte's bits a type asked for is checked against.
pub const KIND_BITS: u8 = 0x07;
/// The drive a 1541 does not have, as a drive number names it.
pub const SECOND_DRIVE: u8 = b'1';
/// EOI's bit in a byte held.
pub const EOI: u8 = 0x80;
/// A block's last offset.
pub const LAST_OFFSET: u8 = 0xFF;
/// The modes a name's fields ask a file be written or appended to by.
pub const WRITE_MODE: u8 = b'W';
pub const APPEND_MODE: u8 = b'A';
/// The letters a name's fields ask for a type and a mode by.
const TYPE_LETTERS: [(u8, u8); 4] = [
    (b'S', kind::SEQ),
    (b'P', kind::PRG),
    (b'U', kind::USR),
    (b'L', kind::REL),
];
const MODE_LETTERS: [u8; 4] = [b'R', WRITE_MODE, APPEND_MODE, b'M'];
/// Times a job is tried before its error stands.
const JOB_TRIES: u8 = 3;
/// The offset the next byte held is fetched after, at a block's start.
const BEFORE_DATA: u8 = LINK_BYTES - 1;
/// The fields after a name that count.
const NAME_FIELDS: u8 = 2;
/// A pattern's wildcards: one matching the rest of a name, and one any
/// character.
pub const ANY_REST: u8 = b'*';
pub const ANY_ONE: u8 = b'?';

const _: () = assert!(
    page::COMMAND >> 8 == page::LISTING_TEXT >> 8
        && (page::COMMAND & 0xFF) + page::COMMAND_BYTES as u16 <= 0x100
        && (page::LISTING_TEXT & 0xFF) + page::COMMAND_BYTES as u16 <= 0x100,
    "the texts patterns are matched in lie within page two"
);

pub fn emit(a: &mut Asm) {
    jobs(a);
    readiness(a);
    names(a);
    patterns(a);
    directory(a);
    buffers(a);
    opening(a);
    opened(a);
    closing(a);
    reading(a);
    a.later(|a| {
        a.here("type_letters");
        for (letter, _) in TYPE_LETTERS {
            a.byte(letter);
        }
        a.here("type_kinds");
        for (_, kind) in TYPE_LETTERS {
            a.byte(kind);
        }
        a.here("mode_letters");
        a.bytes(&MODE_LETTERS);
        a.here("bit_of");
        for bit in 0..8 {
            a.byte(1 << bit);
        }
    });
}

/// `run_job`: job A for buffer X, its track and sector already in the
/// header table, waited for with interrupts on so the controller runs. A
/// failed job is tried again; one that still fails sets its error at its
/// block and returns C set. `read_block_at` and `write_block_at`: track A,
/// sector Y read into buffer X or written from it, a track no disk has
/// refused as ILLEGAL TRACK OR SECTOR. `set_header`: buffer X's track A
/// and sector Y. Each keeps X.
fn jobs(a: &mut Asm) {
    a.here("run_job");
    let (again, wait, failed) = (a.local(), a.local(), a.local());
    a.sta(z(zp::RETRY_CODE));
    a.lda(imm(JOB_TRIES));
    a.sta(z(zp::RETRIES));
    a.cli();
    a.bind(again);
    a.lda(z(zp::RETRY_CODE));
    a.sta(z(zp::JOBS).x());
    a.bind(wait);
    a.lda(z(zp::JOBS).x());
    a.bmi(wait);
    a.cmp(imm(job::OK));
    a.bne(failed);
    a.clc();
    a.rts();
    a.bind(failed);
    a.dec(z(zp::RETRIES));
    a.bne(again);
    // Return codes 2 to 11 report errors 20 to 29.
    a.clc();
    a.adc(imm(number::NO_HEADER - job::NO_HEADER));
    a.pha();
    a.stx(z(zp::COUNT));
    a.txa();
    a.asl(Acc);
    a.tax();
    a.ldy(z(zp::HEADERS + 1).x());
    a.lda(z(zp::HEADERS).x());
    a.tax();
    a.pla();
    a.jsr("set_error");
    a.ldx(z(zp::COUNT));
    a.sec();
    a.rts();

    a.here("read_block_at");
    let (block_job, illegal) = (a.local(), a.local());
    a.pha();
    a.lda(imm(code::READ));
    a.bne(block_job);
    a.here("write_block_at");
    a.pha();
    a.lda(imm(code::WRITE));
    a.bind(block_job);
    a.sta(z(zp::RETRY_CODE));
    a.pla();
    a.cmp(imm(1));
    a.bcc(illegal);
    a.cmp(imm(TRACKS + 1));
    a.bcs(illegal);
    a.jsr("set_header");
    a.lda(z(zp::RETRY_CODE));
    a.jmp("run_job");
    a.bind(illegal);
    a.stx(z(zp::COUNT));
    a.tax();
    a.lda(imm(number::ILLEGAL_BLOCK));
    a.jsr("set_error");
    a.ldx(z(zp::COUNT));
    a.sec();
    a.rts();

    a.here("set_header");
    a.pha();
    a.stx(z(zp::COUNT));
    a.txa();
    a.asl(Acc);
    a.tax();
    a.pla();
    a.sta(z(zp::HEADERS).x());
    a.sty(z(zp::HEADERS + 1).x());
    a.ldx(z(zp::COUNT));
    a.rts();
}

/// `initialize`: the disk's ID from a header on the directory's track,
/// then the BAM read into its buffer, no track yet touched; C set with the
/// error set where either fails. `ensure_ready`: the same, unless the disk
/// is ready. Each keeps X.
fn readiness(a: &mut Asm) {
    a.here("ensure_ready");
    a.lda(z(zp::DISK_READY));
    a.beq("initialize");
    a.clc();
    a.rts();

    a.here("initialize");
    let done = a.local();
    a.txa();
    a.pha();
    a.lda(imm(0));
    a.sta(z(zp::DISK_READY));
    a.ldx(imm(BAM_BUFFER));
    a.lda(imm(DIRECTORY_TRACK));
    a.ldy(imm(BAM_SECTOR));
    a.jsr("set_header");
    a.lda(imm(code::SEEK));
    a.jsr("run_job");
    a.bcs(done);
    a.lda(imm(DIRECTORY_TRACK));
    a.ldy(imm(BAM_SECTOR));
    a.jsr("read_block_at");
    a.bcs(done);
    a.jsr("untouch_tracks");
    a.inc(z(zp::DISK_READY));
    a.bind(done);
    a.pla();
    a.tax();
    a.rts();
}

/// `find_colon`: X at the first colon from offset X in the command, C
/// clear; C set at the command's end where there is none. `second_drive`:
/// Z set where the colon at X follows the second drive's number, which a
/// 1541 does not have; X kept. `scan_name`: the name from offset X to the
/// first comma; a comma first is the name's one character and parts what
/// follows too, as the original's does: `NAME_START` and `NAME_END` around
/// it, X at the comma parting what follows or at the command's end.
fn names(a: &mut Asm) {
    a.here("find_colon");
    let (look, found) = (a.local(), a.local());
    a.bind(look);
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(RTS);
    a.lda(abs(page::COMMAND).x());
    a.cmp(imm(b':'));
    a.beq(found);
    a.inx();
    a.bne(look);
    a.bind(found);
    a.clc();
    a.rts();

    a.here("second_drive");
    let before = a.local();
    a.txa();
    a.bne(before);
    a.lda(imm(1));
    a.rts();
    a.bind(before);
    a.lda(abs(page::COMMAND - 1).x());
    a.cmp(imm(SECOND_DRIVE));
    a.rts();

    a.here("scan_name");
    let (scan, ended) = (a.local(), a.local());
    a.stx(z(zp::NAME_START));
    a.dex();
    a.bind(scan);
    a.inx();
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(ended);
    a.lda(abs(page::COMMAND).x());
    a.cmp(imm(b','));
    a.bne(scan);
    a.cpx(z(zp::NAME_START));
    a.bne(ended);
    a.inx();
    a.stx(z(zp::NAME_END));
    a.dex();
    a.rts();
    a.bind(ended);
    a.stx(z(zp::NAME_END));
    a.rts();
    parsing(a);
}

/// `parse_name`: the name in the command buffer after the drive's colon
/// where there is one, as `scan_name` finds it, its fields following as
/// `name_fields` reads them. Drive 1 is DRIVE NOT READY, C set.
fn parsing(a: &mut Asm) {
    a.here("parse_name");
    let (colon, after, start) = (a.local(), a.local(), a.local());
    a.ldx(imm(0));
    a.jsr("find_colon");
    a.bcc(colon);
    a.ldx(imm(0));
    a.beq(start);
    a.bind(colon);
    a.jsr("second_drive");
    a.bne(after);
    a.lda(imm(number::NOT_READY));
    a.jmp("error_at_zero");
    a.bind(after);
    a.inx();
    a.bind(start);
    a.jsr("scan_name");
    name_fields(a);
}

/// `parse_name` run on from X at the name's end: the first two fields
/// after it are each a type (`S`, `P`, `U`, `L`) or a mode (`R`, `W`, `A`,
/// `M`) by its first letter, a later replacing an earlier; any other
/// letter, and any field past the second, is ignored. `L`'s record length
/// is the next field's first byte, whatever it is, and ends the fields. A
/// name asking for no type on the LOAD channel asks for a program.
fn name_fields(a: &mut Asm) {
    let (field, letters, modes, a_type, a_mode, skip, next_field, typed) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    let (length, lengthless) = (a.local(), a.local());
    a.lda(imm(0));
    a.sta(z(zp::NAME_TYPE));
    a.sta(z(zp::NAME_MODE));
    a.sta(z(zp::NAME_RECORD));
    a.lda(imm(NAME_FIELDS));
    a.sta(z(zp::COUNT));
    // X at a comma, or the command's end.
    a.bind(field);
    a.inx();
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(typed);
    a.lda(abs(page::COMMAND).x());
    a.ldy(imm(byte(TYPE_LETTERS.len() - 1)));
    a.bind(letters);
    a.cmp(abs("type_letters").y());
    a.beq(a_type);
    a.dey();
    a.bpl(letters);
    a.ldy(imm(byte(MODE_LETTERS.len() - 1)));
    a.bind(modes);
    a.cmp(abs("mode_letters").y());
    a.beq(a_mode);
    a.dey();
    a.bpl(modes);
    a.bmi(skip);
    a.bind(a_type);
    a.lda(abs("type_kinds").y());
    a.sta(z(zp::NAME_TYPE));
    a.cmp(imm(kind::REL));
    a.bne(skip);
    a.bind(length);
    a.inx();
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(lengthless);
    a.lda(abs(page::COMMAND).x());
    a.cmp(imm(b','));
    a.bne(length);
    a.inx();
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(lengthless);
    a.lda(abs(page::COMMAND).x());
    a.sta(z(zp::NAME_RECORD));
    a.bind(lengthless);
    a.jmp(typed);
    a.bind(a_mode);
    a.sta(z(zp::NAME_MODE));
    a.bind(skip);
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(typed);
    a.lda(abs(page::COMMAND).x());
    a.cmp(imm(b','));
    a.beq(next_field);
    a.inx();
    a.bne(skip);
    a.bind(next_field);
    a.dec(z(zp::COUNT));
    a.bne(field);
    let parsed = a.local();
    a.bind(typed);
    a.lda(z(zp::NAME_TYPE));
    a.bne(parsed);
    a.lda(z(zp::CHANNEL));
    a.cmp(imm(channel::LOAD));
    a.bne(parsed);
    a.lda(imm(kind::PRG));
    a.sta(z(zp::NAME_TYPE));
    a.bind(parsed);
    a.clc();
    a.rts();
}

/// `set_pattern`: the pattern matched, the text at page two's offset A from
/// offset X to Y. `name_as_pattern`: the name parsed as the pattern, looked
/// for whatever its kind. `name_matches`: the pattern against the name of
/// the entry at `DIRECTORY_ENTRY`, Z set where it matches: `*` matches what
/// remains, `?` any character, and sixteen characters at most count.
/// `wildcard_in`: C set where the command's text from offset X up to
/// offset `COUNT` holds a wildcard; X changed.
fn patterns(a: &mut Asm) {
    a.here("set_pattern");
    a.stx(z(zp::COUNT));
    a.clc();
    a.adc(z(zp::COUNT));
    a.sta(z(zp::PATTERN));
    a.lda(imm(hi(page::COMMAND)));
    a.sta(z(zp::PATTERN + 1));
    a.tya();
    a.sec();
    a.sbc(z(zp::COUNT));
    a.sta(z(zp::PATTERN_LENGTH));
    a.rts();

    a.here("name_as_pattern");
    a.lda(imm(0));
    a.sta(z(zp::FIND_KIND));
    a.lda(imm(lo(page::COMMAND)));
    a.ldx(z(zp::NAME_START));
    a.ldy(z(zp::NAME_END));
    a.jmp("set_pattern");

    a.here("name_matches");
    let (next, spent, matched, differs, any, step) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.lda(z(zp::DIRECTORY_ENTRY));
    a.clc();
    a.adc(imm(entry::NAME));
    a.sta(z(zp::NAME_AT));
    a.lda(z(zp::DIRECTORY_PAGE));
    a.sta(z(zp::NAME_AT + 1));
    a.ldy(imm(0));
    a.bind(next);
    a.cpy(z(zp::PATTERN_LENGTH));
    a.beq(spent);
    a.cpy(imm(NAME_BYTES));
    a.beq(matched);
    a.lda(z(zp::PATTERN).ind_y());
    a.cmp(imm(ANY_REST));
    a.beq(matched);
    a.cmp(imm(ANY_ONE));
    a.beq(any);
    a.cmp(z(zp::NAME_AT).ind_y());
    a.beq(step);
    a.bind(differs);
    a.lda(imm(1));
    a.rts();
    a.bind(any);
    a.lda(z(zp::NAME_AT).ind_y());
    a.cmp(imm(PADDING));
    a.beq(differs);
    a.bind(step);
    a.iny();
    a.bne(next);
    // The pattern is spent: the name must be too.
    a.bind(spent);
    a.cpy(imm(NAME_BYTES));
    a.beq(matched);
    a.lda(z(zp::NAME_AT).ind_y());
    a.cmp(imm(PADDING));
    a.rts();
    a.bind(matched);
    a.lda(imm(0));
    a.rts();

    a.here("wildcard_in");
    let (scan, none) = (a.local(), a.local());
    a.bind(scan);
    a.cpx(z(zp::COUNT));
    a.bcs(none);
    a.lda(abs(page::COMMAND).x());
    a.inx();
    a.cmp(imm(ANY_REST));
    a.beq(RTS);
    a.cmp(imm(ANY_ONE));
    a.bne(scan);
    a.sec();
    a.rts();
    a.bind(none);
    a.clc();
    a.rts();
}

/// `first_entry`: the directory's first sector read, `DIRECTORY_ENTRY` at
/// its first entry. `next_entry`: the next entry, through the directory's
/// chain. Each returns C set at the directory's end, with the error set
/// where a read failed. `entry_sector`: track A, sector Y read as the
/// directory sector, at its first entry. `write_directory`: the directory
/// sector written back. `find_entry`: the first entry in use matching the
/// pattern, and of `FIND_KIND` unless that is 0; C set where none does.
/// Each changes X.
fn directory(a: &mut Asm) {
    a.here("first_entry");
    a.lda(imm(DIRECTORY_TRACK));
    a.ldy(imm(FIRST_DIRECTORY_SECTOR));
    a.jmp("entry_sector");

    a.here("next_entry");
    let chain = a.local();
    a.lda(z(zp::DIRECTORY_ENTRY));
    a.clc();
    a.adc(imm(entry::BYTES));
    a.sta(z(zp::DIRECTORY_ENTRY));
    a.bcc(RTS);
    a.ldy(imm(0));
    a.lda(z(zp::DIRECTORY_AT).ind_y());
    a.bne(chain);
    a.sec();
    a.rts();
    a.bind(chain);
    a.pha();
    a.iny();
    a.lda(z(zp::DIRECTORY_AT).ind_y());
    a.tay();
    a.pla();
    a.here("entry_sector");
    a.sta(z(zp::DIRECTORY_TRACK));
    a.sty(z(zp::DIRECTORY_SECTOR));
    a.jsr("directory_buffer");
    a.bcs(RTS);
    a.ldx(z(zp::DIRECTORY_HELD));
    a.lda(z(zp::DIRECTORY_TRACK));
    a.ldy(z(zp::DIRECTORY_SECTOR));
    a.jsr("read_block_at");
    a.bcs(RTS);
    a.lda(imm(0));
    a.sta(z(zp::DIRECTORY_ENTRY));
    a.clc();
    a.rts();

    a.here("write_directory");
    a.ldx(z(zp::DIRECTORY_HELD));
    a.lda(z(zp::DIRECTORY_TRACK));
    a.ldy(z(zp::DIRECTORY_SECTOR));
    a.jmp("write_block_at");

    a.here("find_entry");
    let (check, named, next) = (a.local(), a.local(), a.local());
    a.jsr("first_entry");
    a.bcs(RTS);
    a.bind(check);
    a.ldy(imm(entry::TYPE));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.beq(next);
    a.ldy(z(zp::FIND_KIND));
    a.beq(named);
    a.and(imm(KIND_BITS));
    a.cmp(z(zp::FIND_KIND));
    a.bne(next);
    a.bind(named);
    a.jsr("name_matches");
    a.bne(next);
    a.clc();
    a.rts();
    a.bind(next);
    a.jsr("next_entry");
    a.bcc(check);
    a.rts();
}

/// `hold_buffer`: the highest free channel buffer in X, held for the
/// request in hand; NO CHANNEL and C set where none is free. `take_buffer`:
/// the same as the channel's file, ENDED until its open is done.
/// `directory_buffer`: the buffer the request reads directory sectors
/// into, held from the first time it is asked for; X kept. `release_held`:
/// every buffer the request held freed; X kept. `free_buffer`: file X's
/// buffer freed and the channel left with none. `channel_file`: the
/// channel's file in X, C set where it has none. `drop_files`: every file
/// given up unfinished, every channel left with none.
fn buffers(a: &mut Asm) {
    a.here("take_buffer");
    a.jsr("hold_buffer");
    a.bcs(RTS);
    a.lda(imm(mode::ENDED));
    a.sta(abs(page::FILE_MODE).x());
    a.txa();
    a.ldy(z(zp::CHANNEL));
    a.sta(abs(page::CHANNEL_FILE).y());
    a.clc();
    a.rts();

    a.here("hold_buffer");
    let (try_buffer, found) = (a.local(), a.local());
    a.ldx(imm(CHANNEL_BUFFERS - 1));
    a.bind(try_buffer);
    a.lda(abs(page::FILE_MODE).x());
    a.beq(found);
    a.dex();
    a.bpl(try_buffer);
    a.lda(imm(number::NO_CHANNEL));
    a.jmp("error_at_zero");
    a.bind(found);
    a.lda(imm(mode::HELD));
    a.sta(abs(page::FILE_MODE).x());
    a.clc();
    a.rts();

    a.here("directory_buffer");
    let (held, kept) = (a.local(), a.local());
    a.lda(z(zp::DIRECTORY_HELD));
    a.bpl(held);
    a.txa();
    a.pha();
    a.jsr("hold_buffer");
    a.bcs(kept);
    a.stx(z(zp::DIRECTORY_HELD));
    a.txa();
    a.clc();
    a.adc(imm(hi(BUFFERS)));
    a.sta(z(zp::DIRECTORY_PAGE));
    a.sta(z(zp::DIRECTORY_AT + 1));
    a.lda(imm(0));
    a.sta(z(zp::DIRECTORY_AT));
    a.clc();
    a.bind(kept);
    a.pla();
    a.tax();
    a.rts();
    a.bind(held);
    a.clc();
    a.rts();

    a.here("release_held");
    let (each, kept_mode) = (a.local(), a.local());
    a.txa();
    a.pha();
    a.ldx(imm(CHANNEL_BUFFERS - 1));
    a.bind(each);
    a.lda(abs(page::FILE_MODE).x());
    a.cmp(imm(mode::HELD));
    a.bne(kept_mode);
    a.lda(imm(mode::FREE));
    a.sta(abs(page::FILE_MODE).x());
    a.bind(kept_mode);
    a.dex();
    a.bpl(each);
    a.lda(imm(NO_FILE));
    a.sta(z(zp::DIRECTORY_HELD));
    a.pla();
    a.tax();
    a.rts();

    a.here("free_buffer");
    a.lda(imm(mode::FREE));
    a.sta(abs(page::FILE_MODE).x());
    a.lda(imm(NO_FILE));
    a.ldy(z(zp::CHANNEL));
    a.sta(abs(page::CHANNEL_FILE).y());
    a.rts();

    a.here("channel_file");
    a.ldy(z(zp::CHANNEL));
    a.ldx(abs(page::CHANNEL_FILE).y());
    a.cpx(imm(NO_FILE));
    a.rts();

    a.here("drop_files");
    let (each_buffer, each_channel) = (a.local(), a.local());
    a.lda(imm(mode::FREE));
    a.ldx(imm(CHANNEL_BUFFERS - 1));
    a.bind(each_buffer);
    a.sta(abs(page::FILE_MODE).x());
    a.dex();
    a.bpl(each_buffer);
    a.lda(imm(NO_FILE));
    a.sta(z(zp::DIRECTORY_HELD));
    a.sta(abs(page::REL_FILE));
    a.ldx(imm(page::DATA_CHANNELS - 1));
    a.bind(each_channel);
    a.sta(abs(page::CHANNEL_FILE).x());
    a.dex();
    a.bpl(each_channel);
    a.rts();
}

/// `open_file`: the data channel's OPEN with the name in the command
/// buffer, any file it had open closed first, a buffer taken before the
/// name is looked for. `$` on the LOAD channel is the listing; on another
/// it reads the directory as a file from the BAM on, as the original's
/// does without first making the disk ready. A relative file asked for is
/// opened as one. The SAVE channel, and a write or append asked for,
/// write. LOAD's `*` opens the program last loaded again by its first
/// block, or else the first program on the disk. A name is found in the
/// directory, an empty one never. A failure leaves the channel with no
/// file, C set.
fn opening(a: &mut Asm) {
    a.here("open_file");
    let (not_direct, named, raw_directory, sequential, writing, read) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.jsr("set_ok");
    a.jsr("close_channel");
    a.jsr("first_character");
    a.cmp(imm(b'#'));
    a.bne(not_direct);
    a.jmp("open_direct");
    a.bind(not_direct);
    a.cmp(imm(b'$'));
    a.bne(named);
    a.lda(z(zp::CHANNEL));
    a.cmp(imm(channel::LOAD));
    a.bne(raw_directory);
    a.jmp("open_listing");
    a.bind(raw_directory);
    a.jsr("take_buffer");
    a.bcs(RTS);
    a.lda(imm(DIRECTORY_TRACK));
    a.ldy(imm(BAM_SECTOR));
    a.jmp("open_at");

    a.bind(named);
    a.jsr("parse_name");
    a.bcs(RTS);
    a.jsr("take_buffer");
    a.bcs(RTS);
    a.lda(z(zp::NAME_TYPE));
    a.cmp(imm(kind::REL));
    a.bne(sequential);
    a.jmp("open_rel");
    a.bind(sequential);
    a.lda(z(zp::CHANNEL));
    a.cmp(imm(channel::SAVE));
    a.beq(writing);
    a.lda(z(zp::NAME_MODE));
    a.cmp(imm(WRITE_MODE));
    a.beq(writing);
    a.cmp(imm(APPEND_MODE));
    a.bne(read);
    a.jmp("open_append");
    a.bind(writing);
    a.jmp("open_write");
    open_read(a, read);
}

/// `open_file` run on at `read` for a file read: LOAD's `*`, else the name
/// looked for, on into `open_at`, or into `open_found_rel` for a relative
/// file.
fn open_read(a: &mut Asm, read: Label) {
    let (search, first_program, find, found, unnamed, chained, located) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.bind(read);
    a.lda(z(zp::CHANNEL));
    a.cmp(imm(channel::LOAD));
    a.bne(search);
    a.jsr("first_character");
    a.cmp(imm(ANY_REST));
    a.bne(search);
    a.lda(z(zp::LAST_TRACK));
    a.beq(first_program);
    a.ldy(z(zp::LAST_SECTOR));
    a.jmp("open_at");
    // The `*` that names it matches any name.
    a.bind(first_program);
    a.lda(imm(kind::PRG));
    a.sta(z(zp::FIND_KIND));
    a.lda(imm(lo(page::COMMAND)));
    a.ldx(imm(0));
    a.ldy(imm(1));
    a.jsr("set_pattern");
    a.jmp(find);
    a.bind(search);
    a.lda(z(zp::NAME_END));
    a.cmp(z(zp::NAME_START));
    a.beq(unnamed);
    a.jsr("name_as_pattern");
    a.bind(find);
    a.jsr("ensure_ready");
    a.bcs("open_failed");
    a.jsr("find_entry");
    a.bcc(found);
    // A read that failed has set its own error.
    a.lda(z(zp::ERROR));
    a.bne("open_failed");
    a.bind(unnamed);
    a.lda(imm(number::FILE_NOT_FOUND));
    a.jmp("refuse_open");
    a.bind(found);
    a.jsr("found_checked");
    a.bcs(RTS);
    a.jsr("channel_file");
    a.ldy(imm(entry::TYPE));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.and(imm(KIND_BITS));
    a.cmp(imm(kind::REL));
    a.bne(chained);
    a.jmp("open_found_rel");
    a.bind(chained);
    a.ldy(imm(entry::TRACK));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.bne(located);
    a.lda(imm(number::FILE_NOT_FOUND));
    a.jmp("refuse_open");
    a.bind(located);
    a.pha();
    a.ldy(imm(entry::SECTOR));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.tay();
    a.pla();
    a.fall_into("open_at");
}

/// `open_at`: the channel's file X read from track A, sector Y.
/// `found_checked`: the entry found, or WRITE FILE OPEN where it is still
/// being written and FILE TYPE MISMATCH where of another type than asked,
/// C set and the file given back. `refuse_open`: error A set at no block,
/// then, as `open_failed`, the channel's file given back.
fn opened(a: &mut Asm) {
    a.here("open_at");
    let (remembered, opened) = (a.local(), a.local());
    a.pha();
    a.lda(z(zp::CHANNEL));
    a.cmp(imm(channel::LOAD));
    a.bne(remembered);
    a.pla();
    a.pha();
    a.sta(z(zp::LAST_TRACK));
    a.sty(z(zp::LAST_SECTOR));
    a.bind(remembered);
    a.pla();
    a.jsr("read_from");
    a.bcs("open_failed");
    // A block holding a file's one byte sends it, then the block from its
    // start, the byte last again, as the original's does.
    a.lda(abs(page::FILE_EOI).x());
    a.beq(opened);
    a.lda(imm(0));
    a.sta(abs(page::FILE_EOI).x());
    a.lda(imm(LAST_OFFSET));
    a.sta(abs(page::FILE_AT).x());
    a.bind(opened);
    a.clc();
    a.rts();

    a.here("found_checked");
    let (closed, kind_ok) = (a.local(), a.local());
    a.ldy(imm(entry::TYPE));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.bmi(closed);
    a.lda(imm(number::WRITE_FILE_OPEN));
    a.jmp("refuse_open");
    a.bind(closed);
    a.ldy(z(zp::NAME_TYPE));
    a.beq(kind_ok);
    a.and(imm(KIND_BITS));
    a.cmp(z(zp::NAME_TYPE));
    a.beq(kind_ok);
    a.lda(imm(number::TYPE_MISMATCH));
    a.jmp("refuse_open");
    a.bind(kind_ok);
    a.clc();
    a.rts();

    a.here("refuse_open");
    a.jsr("error_at_zero");
    a.here("open_failed");
    a.jsr("channel_file");
    a.bcs(RTS);
    a.jsr("free_buffer");
    a.sec();
    a.rts();
}

/// `first_character`: A the command's first character, 0 for an empty
/// command. `error_at_zero`: error A set at no block, C set.
/// `close_channel`: the channel's file closed, a written or relative one
/// finished on the disk; `close_all`: every data channel's.
fn closing(a: &mut Asm) {
    a.here("first_character");
    a.lda(imm(0));
    a.ldy(z(zp::COMMAND_LENGTH));
    a.beq(RTS);
    a.lda(abs(page::COMMAND));
    a.rts();

    a.here("error_at_zero");
    a.ldx(imm(0));
    a.ldy(imm(0));
    a.jsr("set_error");
    a.sec();
    a.rts();

    a.here("close_channel");
    let (written, closed, free) = (a.local(), a.local(), a.local());
    a.jsr("channel_file");
    a.bcs(RTS);
    a.lda(abs(page::FILE_MODE).x());
    a.cmp(imm(mode::REL));
    a.bne(written);
    a.jsr("close_rel");
    a.jmp(closed);
    a.bind(written);
    a.cmp(imm(mode::WRITE));
    a.bne(free);
    a.jsr("close_write");
    a.bind(closed);
    a.jsr("channel_file");
    a.bind(free);
    a.jmp("free_buffer");

    a.here("close_all");
    let each = a.local();
    a.lda(imm(page::DATA_CHANNELS - 1));
    a.sta(z(zp::CHANNEL));
    a.bind(each);
    a.jsr("close_channel");
    a.dec(z(zp::CHANNEL));
    a.bpl(each);
    a.rts();
}

/// `file_byte`: the channel's byte held, EOI's bit 7 set where it is the
/// last, C set where the channel has none to send. `file_taken`: the
/// channel past it, ended after the last. `fetch_byte`: file X's next
/// byte held; at the end of a block, the block after it read while the
/// byte is held, or the listing's next chunk put together; at the end of
/// the last, EOI. Each keeps X but for a relative file's.
fn reading(a: &mut Asm) {
    a.here("file_byte");
    let (has, none) = (a.local(), a.local());
    a.jsr("channel_file");
    a.bcs(none);
    a.lda(abs(page::FILE_MODE).x());
    a.cmp(imm(mode::READ));
    a.beq(has);
    a.cmp(imm(mode::DIRECT));
    a.beq(has);
    a.cmp(imm(mode::LISTING));
    a.beq(has);
    a.cmp(imm(mode::REL));
    a.bne(none);
    a.jsr("rel_ready");
    a.bcs(none);
    a.ldx(abs(page::REL_FILE));
    a.bind(has);
    a.lda(abs(page::FILE_EOI).x());
    a.sta(z(zp::EOI));
    a.lda(abs(page::FILE_BYTE).x());
    a.clc();
    a.rts();
    a.bind(none);
    a.sec();
    a.rts();

    a.here("file_taken");
    let (not_direct, held) = (a.local(), a.local());
    a.jsr("channel_file");
    a.bcs(RTS);
    a.lda(abs(page::FILE_MODE).x());
    a.cmp(imm(mode::DIRECT));
    a.bne(not_direct);
    a.jmp("direct_taken");
    a.bind(not_direct);
    a.cmp(imm(mode::REL));
    a.bne(held);
    a.jmp("rel_taken");
    a.bind(held);
    a.lda(abs(page::FILE_EOI).x());
    a.beq("fetch_byte");
    a.lda(imm(mode::ENDED));
    a.sta(abs(page::FILE_MODE).x());
    a.rts();

    a.here("fetch_byte");
    let (file_end, next_block) = (a.local(), a.local());
    a.inc(abs(page::FILE_AT).x());
    a.jsr("point_at_file");
    a.ldy(abs(page::FILE_AT).x());
    a.lda(z(zp::FILE_POINTER).ind_y());
    a.sta(abs(page::FILE_BYTE).x());
    a.tya();
    a.cmp(abs(page::FILE_LAST).x());
    a.bne(RTS);
    a.lda(abs(page::FILE_MODE).x());
    a.cmp(imm(mode::LISTING));
    a.bne(file_end);
    a.jmp("next_listing_chunk");
    a.bind(file_end);
    a.lda(abs(page::FILE_TRACK).x());
    a.bne(next_block);
    a.lda(imm(EOI));
    a.sta(abs(page::FILE_EOI).x());
    a.rts();
    a.bind(next_block);
    a.jsr("next_file_block");
    a.bcc(RTS);
    a.lda(imm(mode::ENDED));
    a.sta(abs(page::FILE_MODE).x());
    a.rts();

    chained(a);
}

/// `read_from`: file X read from track A, sector Y, its first byte held,
/// C set where its first block cannot be read. `next_file_block`: the
/// block file X names read into its buffer, its link naming the next, and
/// its last offset: the block's own, or for a last block the one its link
/// names where that is a data byte's; C set where it cannot be read.
/// `point_at_file`: `FILE_POINTER` at file X's buffer. Each keeps X.
fn chained(a: &mut Asm) {
    a.here("read_from");
    a.sta(abs(page::FILE_TRACK).x());
    a.tya();
    a.sta(abs(page::FILE_SECTOR).x());
    a.jsr("next_file_block");
    a.bcs(RTS);
    a.lda(imm(mode::READ));
    a.sta(abs(page::FILE_MODE).x());
    a.lda(imm(0));
    a.sta(abs(page::FILE_EOI).x());
    a.jsr("fetch_byte");
    a.clc();
    a.rts();

    a.here("next_file_block");
    let full = a.local();
    a.ldy(abs(page::FILE_SECTOR).x());
    a.lda(abs(page::FILE_TRACK).x());
    a.jsr("read_block_at");
    a.bcs(RTS);
    a.jsr("point_at_file");
    a.ldy(imm(0));
    a.lda(z(zp::FILE_POINTER).ind_y());
    a.sta(abs(page::FILE_TRACK).x());
    a.iny();
    a.lda(z(zp::FILE_POINTER).ind_y());
    a.sta(abs(page::FILE_SECTOR).x());
    a.ldy(imm(LAST_OFFSET));
    a.lda(abs(page::FILE_TRACK).x());
    a.bne(full);
    a.lda(abs(page::FILE_SECTOR).x());
    a.cmp(imm(LINK_BYTES));
    a.bcc(full);
    a.tay();
    a.bind(full);
    a.tya();
    a.sta(abs(page::FILE_LAST).x());
    a.lda(imm(BEFORE_DATA));
    a.sta(abs(page::FILE_AT).x());
    a.clc();
    a.rts();

    a.here("point_at_file");
    a.lda(imm(0));
    a.sta(z(zp::FILE_POINTER));
    a.txa();
    a.clc();
    a.adc(imm(hi(BUFFERS)));
    a.sta(z(zp::FILE_POINTER + 1));
    a.rts();
}
