//! The directory listing LOAD reads as a BASIC program, as the original
//! lays it out: the disk's name and ID, a line a file, then the blocks
//! free, each line 32 bytes counting the load address before the first and
//! the program's end after the last. It is put together a directory sector
//! at a time in the channel's buffer.

use super::errors::number;
use super::files::{mode, KIND_BITS, LAST_OFFSET, SECOND_DRIVE};
use super::ws::{page, zp, ALL_ENTRIES, BAM, PATTERNS};
use crate::media::c1541::layout::{
    bam, entry, kind, DIRECTORY_TRACK, FIRST_DIRECTORY_SECTOR, NAME_BYTES, PADDING,
};
use crate::rom::asm::{abs, byte, hi, imm, lo, sym, zp as z, Acc, Asm, Label, RTS};
use crate::rom::c64::petscii::REVERSE_ON;

/// A line's bytes.
const LINE_BYTES: u8 = 32;
/// Where the listing loads, and the link every line carries, which LOAD
/// recalculates.
const LOAD_ADDRESS: u16 = 0x0401;
const LINK: u8 = 0x01;
/// A type byte's bits a line names its kind by; past REL a kind is shown
/// as unknown, where the original shows whatever follows its table.
const NAMED_KIND_BITS: u8 = 0x0F;
const KIND_NAMES: [&[u8; 3]; 6] = [b"DEL", b"SEQ", b"PRG", b"USR", b"REL", b"???"];
const UNKNOWN_KIND: u8 = 5;
/// The letters `=` lists a kind by.
const KIND_LETTERS: [(u8, u8); 4] = [
    (b'S', kind::SEQ),
    (b'P', kind::PRG),
    (b'U', kind::USR),
    (b'R', kind::REL),
];
/// The spaces before a line's name, and after its kind, together.
const PADDING_SPACES: u8 = 4;
const BLOCKS_FREE: &[u8] = b"BLOCKS FREE.";
const FREE_LINE_SPACES: u8 = 13;
/// The first pattern whose wildcards the original does not see.
const FIFTH: u8 = PATTERNS - 1;

const _: () = assert!(
    KIND_NAMES.len() == UNKNOWN_KIND as usize + 1 && kind::REL + 1 == UNKNOWN_KIND,
    "every kind past REL shows as unknown"
);

pub fn emit(a: &mut Asm) {
    opening(a);
    patterns(a);
    keeping(a);
    chunks(a);
    listing_entry(a);
    lines(a);
    header_lines(a);
    chunk_puts(a);
    a.later(|a| {
        a.here("kind_names");
        for name in KIND_NAMES {
            a.bytes(name);
        }
        a.here("listing_letters");
        for (letter, _) in KIND_LETTERS {
            a.byte(letter);
        }
        a.here("listing_kinds");
        for (_, kind) in KIND_LETTERS {
            a.byte(kind);
        }
        a.here("blocks_free");
        a.bytes(BLOCKS_FREE);
    });
}

/// `open_listing`: the LOAD channel's `$`: its patterns read, a buffer
/// taken and a second held to read the directory through, as the
/// original's open asks, the disk made ready, the command kept for the
/// patterns, and the header line put in the buffer with the load address
/// before it.
fn opening(a: &mut Asm) {
    a.here("open_listing");
    let (ready, failed, copy) = (a.local(), a.local(), a.local());
    a.jsr("parse_listing");
    a.bcs(RTS);
    a.jsr("take_buffer");
    a.bcs(RTS);
    a.jsr("directory_buffer");
    a.bcs(failed);
    a.jsr("ensure_ready");
    a.bcc(ready);
    a.bind(failed);
    a.jmp("open_failed");
    a.bind(ready);
    a.ldy(imm(page::COMMAND_BYTES - 1));
    a.bind(copy);
    a.lda(abs(page::COMMAND).y());
    a.sta(abs(page::LISTING_TEXT).y());
    a.dey();
    a.bpl(copy);
    a.lda(imm(0));
    a.sta(z(zp::PATTERNS_USED));
    a.lda(imm(mode::LISTING));
    a.sta(abs(page::FILE_MODE).x());
    a.lda(imm(DIRECTORY_TRACK));
    a.sta(abs(page::FILE_TRACK).x());
    a.lda(imm(FIRST_DIRECTORY_SECTOR));
    a.sta(abs(page::FILE_SECTOR).x());
    a.stx(z(zp::LISTING_FILE));
    a.jsr("point_at_file");
    a.jsr("put_header");
    a.ldx(z(zp::LISTING_FILE));
    a.lda(imm(LINE_BYTES - 1));
    a.sta(abs(page::FILE_LAST).x());
    a.lda(imm(LAST_OFFSET));
    a.sta(abs(page::FILE_AT).x());
    a.lda(imm(0));
    a.sta(abs(page::FILE_EOI).x());
    a.jsr("fetch_byte");
    a.clc();
    a.rts();
}

/// `parse_listing`: `$`'s drive and patterns. With no colon, nothing or a
/// lone `0` after the `$` lists every entry, and anything else is the
/// patterns, as `listing_patterns` reads them; with one, they follow it.
/// Drive 1 is DRIVE NOT READY.
fn patterns(a: &mut Asm) {
    a.here("parse_listing");
    let (no_colon, pattern, all, drive_one) = (a.local(), a.local(), a.local(), a.local());
    a.lda(imm(0));
    a.sta(z(zp::PATTERN_COUNT));
    a.sta(z(zp::PATTERNS_WILD));
    a.sta(z(zp::LISTING_KIND));
    a.ldx(imm(1));
    a.jsr("find_colon");
    a.bcs(no_colon);
    // A drive number sits between the `$` and the colon.
    a.jsr("second_drive");
    a.beq(drive_one);
    a.inx();
    a.bne(pattern);
    a.bind(no_colon);
    a.ldx(imm(1));
    a.lda(z(zp::COMMAND_LENGTH));
    a.cmp(imm(1));
    a.beq(all);
    a.cmp(imm(2));
    a.bne(pattern);
    a.lda(abs(page::COMMAND + 1));
    a.cmp(imm(b'0'));
    a.beq(all);
    a.cmp(imm(SECOND_DRIVE));
    a.beq(drive_one);
    listing_patterns(a, pattern, all, drive_one);
}

/// `parse_listing` run on at `pattern`, X at a pattern's first character:
/// patterns parted by commas, an empty one counting unless the command
/// ends with it, five at most kept, the wildcards of only the first four
/// seen, as the original's are. A kind's letter after `=` lists that kind;
/// a comma after it is SYNTAX ERROR. `all` lists every entry and
/// `drive_one` refuses drive 1.
fn listing_patterns(a: &mut Asm, pattern: Label, all: Label, drive_one: Label) {
    let (scan, comma, at_end, equals, letters, rest, parsed, listed_kind) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.bind(pattern);
    a.stx(z(zp::PATTERN_FROM));
    a.bind(scan);
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(at_end);
    a.lda(abs(page::COMMAND).x());
    a.cmp(imm(b','));
    a.beq(comma);
    a.cmp(imm(b'='));
    a.beq(equals);
    a.inx();
    a.bne(scan);
    a.bind(comma);
    a.jsr("keep_pattern");
    a.inx();
    a.bne(pattern);
    a.bind(at_end);
    a.cpx(z(zp::PATTERN_FROM));
    a.beq(parsed);
    a.jsr("keep_pattern");
    a.bind(parsed);
    a.clc();
    a.rts();
    a.bind(equals);
    a.jsr("keep_pattern");
    a.inx();
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(parsed);
    a.lda(abs(page::COMMAND).x());
    a.ldy(imm(byte(KIND_LETTERS.len() - 1)));
    a.bind(letters);
    a.cmp(abs("listing_letters").y());
    a.beq(listed_kind);
    a.dey();
    a.bpl(letters);
    a.bmi(rest);
    a.bind(listed_kind);
    a.lda(abs("listing_kinds").y());
    a.sta(z(zp::LISTING_KIND));
    a.bind(rest);
    a.cpx(z(zp::COMMAND_LENGTH));
    a.bcs(parsed);
    a.lda(abs(page::COMMAND).x());
    a.inx();
    a.cmp(imm(b','));
    a.bne(rest);
    a.lda(imm(number::SYNTAX));
    a.jmp("error_at_zero");
    a.bind(all);
    a.lda(imm(ALL_ENTRIES));
    a.sta(z(zp::PATTERN_COUNT));
    a.clc();
    a.rts();
    a.bind(drive_one);
    a.lda(imm(number::NOT_READY));
    a.jmp("error_at_zero");
}

/// `keep_pattern`: the pattern from `PATTERN_FROM` to X kept where there is
/// room, and whether it has a wildcard; X is kept.
fn keeping(a: &mut Asm) {
    a.here("keep_pattern");
    let (scanned, kept) = (a.local(), a.local());
    a.ldy(z(zp::PATTERN_COUNT));
    a.cpy(imm(PATTERNS));
    a.bcs(RTS);
    a.lda(z(zp::PATTERN_FROM));
    a.sta(abs(zp::PATTERN_STARTS).y());
    a.txa();
    a.sta(abs(zp::PATTERN_ENDS).y());
    a.stx(z(zp::COUNT));
    a.cpy(imm(FIFTH));
    a.bcs(kept);
    a.ldx(z(zp::PATTERN_FROM));
    a.jsr("wildcard_in");
    a.bcc(scanned);
    a.ldy(z(zp::PATTERN_COUNT));
    a.lda(abs("bit_of").y());
    a.ora(z(zp::PATTERNS_WILD));
    a.sta(z(zp::PATTERNS_WILD));
    a.bind(scanned);
    a.ldx(z(zp::COUNT));
    a.bind(kept);
    a.inc(z(zp::PATTERN_COUNT));
    a.rts();
}

/// `next_listing_chunk`: the next directory sector's listed entries as
/// lines in the channel's buffer, sectors listing none passed over; after
/// the last, the blocks-free line, served as a file's last block is. A
/// sector that cannot be read ends the listing.
fn chunks(a: &mut Asm) {
    a.here("next_listing_chunk");
    let (sector, read, each, passed, failed) =
        (a.local(), a.local(), a.local(), a.local(), a.local());
    a.stx(z(zp::LISTING_FILE));
    a.bind(sector);
    a.ldx(z(zp::LISTING_FILE));
    a.lda(abs(page::FILE_TRACK).x());
    a.bne(read);
    a.jsr("point_at_file");
    a.jsr("put_free_line");
    a.ldx(z(zp::LISTING_FILE));
    a.lda(imm(mode::READ));
    a.sta(abs(page::FILE_MODE).x());
    a.lda(imm(LINE_BYTES - 1));
    a.sta(abs(page::FILE_LAST).x());
    a.lda(imm(LAST_OFFSET));
    a.sta(abs(page::FILE_AT).x());
    a.rts();
    a.bind(read);
    a.ldy(abs(page::FILE_SECTOR).x());
    a.jsr("entry_sector");
    a.ldx(z(zp::LISTING_FILE));
    a.bcs(failed);
    a.ldy(imm(0));
    a.lda(z(zp::DIRECTORY_AT).ind_y());
    a.sta(abs(page::FILE_TRACK).x());
    a.iny();
    a.lda(z(zp::DIRECTORY_AT).ind_y());
    a.sta(abs(page::FILE_SECTOR).x());
    a.jsr("point_at_file");
    a.lda(imm(0));
    a.sta(z(zp::LINE_AT));
    a.sta(z(zp::LINES));
    a.bind(each);
    a.jsr("listed");
    a.bcs(passed);
    a.jsr("put_line");
    a.inc(z(zp::LINES));
    a.bind(passed);
    a.lda(z(zp::DIRECTORY_ENTRY));
    a.clc();
    a.adc(imm(entry::BYTES));
    a.sta(z(zp::DIRECTORY_ENTRY));
    a.bcc(each);
    a.lda(z(zp::LINES));
    a.beq(sector);
    a.ldx(z(zp::LISTING_FILE));
    a.ldy(z(zp::LINE_AT));
    a.dey();
    a.tya();
    a.sta(abs(page::FILE_LAST).x());
    a.lda(imm(LAST_OFFSET));
    a.sta(abs(page::FILE_AT).x());
    a.jmp("release_held");
    a.bind(failed);
    a.lda(imm(mode::ENDED));
    a.sta(abs(page::FILE_MODE).x());
    a.jmp("release_held");
}

/// `listed`: C clear where the entry at `DIRECTORY_ENTRY` is listed: in
/// use, of the kind listed, and matching a pattern where there are any. A
/// pattern without a wildcard names one entry, so lists only the first it
/// matches, even where another pattern lists that entry too.
fn listing_entry(a: &mut Asm) {
    a.here("listed");
    let (unlisted, any_kind, listed, each, next, checked) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.ldy(imm(entry::TYPE));
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.beq(unlisted);
    a.ldy(z(zp::LISTING_KIND));
    a.beq(any_kind);
    a.and(imm(KIND_BITS));
    a.cmp(z(zp::LISTING_KIND));
    a.bne(unlisted);
    a.bind(any_kind);
    a.lda(z(zp::PATTERN_COUNT));
    a.cmp(imm(ALL_ENTRIES));
    a.beq(listed);
    a.lda(imm(0));
    a.sta(z(zp::MATCHED));
    a.sta(z(zp::PATTERN_INDEX));
    a.bind(each);
    a.ldy(z(zp::PATTERN_INDEX));
    a.cpy(z(zp::PATTERN_COUNT));
    a.beq(checked);
    a.lda(abs("bit_of").y());
    a.and(z(zp::PATTERNS_USED));
    a.bne(next);
    a.ldx(abs(zp::PATTERN_STARTS).y());
    a.lda(abs(zp::PATTERN_ENDS).y());
    a.tay();
    a.lda(imm(lo(page::LISTING_TEXT)));
    a.jsr("set_pattern");
    a.jsr("name_matches");
    a.bne(next);
    a.inc(z(zp::MATCHED));
    a.ldy(z(zp::PATTERN_INDEX));
    a.lda(abs("bit_of").y());
    a.bit(z(zp::PATTERNS_WILD));
    a.bne(next);
    a.ora(z(zp::PATTERNS_USED));
    a.sta(z(zp::PATTERNS_USED));
    a.bind(next);
    a.inc(z(zp::PATTERN_INDEX));
    a.bne(each);
    a.bind(checked);
    a.lda(z(zp::MATCHED));
    a.beq(unlisted);
    a.bind(listed);
    a.clc();
    a.rts();
    a.bind(unlisted);
    a.sec();
    a.rts();
}

/// The listed entry's field at `offset`, in its copy.
const fn field(offset: u8) -> u16 {
    zp::LISTED + offset as u16
}

/// Lines put at offset Y of the channel's buffer, Y moved past each.
/// `put_line`: the entry at `DIRECTORY_ENTRY`'s line at `LINE_AT`, which
/// moves on a line: its blocks as the line's number, a space or more so
/// short counts line up, its name in quotes, a quote or a shifted space in
/// it closing it, then as `line_end` puts the rest.
fn lines(a: &mut Asm) {
    a.here("put_line");
    let (copy, lead, name, padding, space, put_name) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    // Its own copy, so the line's offset and the entry's need not share Y.
    a.ldy(imm(entry::BYTES - 1));
    a.bind(copy);
    a.lda(z(zp::DIRECTORY_ENTRY).ind_y());
    a.sta(abs(zp::LISTED).y());
    a.dey();
    a.bpl(copy);
    a.ldy(z(zp::LINE_AT));
    a.lda(imm(LINK));
    a.jsr("chunk_put");
    a.jsr("chunk_put");
    a.ldx(imm(0));
    a.lda(abs(field(entry::BLOCKS)).x());
    a.jsr("chunk_put");
    a.lda(abs(field(entry::BLOCKS + 1)).x());
    a.jsr("chunk_put");
    // Three spaces below 10 blocks, two below 100, else one.
    a.lda(imm(1));
    a.sta(z(zp::LEADING));
    a.lda(abs(field(entry::BLOCKS + 1)).x());
    a.bne(lead);
    a.lda(abs(field(entry::BLOCKS)).x());
    a.cmp(imm(100));
    a.bcs(lead);
    a.inc(z(zp::LEADING));
    a.cmp(imm(10));
    a.bcs(lead);
    a.inc(z(zp::LEADING));
    a.bind(lead);
    a.lda(z(zp::LEADING));
    a.jsr("chunk_put_spaces");
    a.lda(imm(b'"'));
    a.jsr("chunk_put");
    a.lda(imm(0));
    a.sta(z(zp::QUOTED));
    a.lda(imm(NAME_BYTES));
    a.sta(z(zp::REMAINING));
    a.bind(name);
    a.lda(abs(field(entry::NAME)).x());
    a.cmp(imm(PADDING));
    a.beq(padding);
    a.cmp(imm(b'"'));
    a.bne(put_name);
    a.sta(z(zp::QUOTED));
    a.jmp(put_name);
    a.bind(padding);
    a.lda(z(zp::QUOTED));
    a.bne(space);
    a.lda(imm(b'"'));
    a.sta(z(zp::QUOTED));
    a.bne(put_name);
    a.bind(space);
    a.lda(imm(b' '));
    a.bind(put_name);
    a.jsr("chunk_put");
    a.inx();
    a.dec(z(zp::REMAINING));
    a.bne(name);
    line_end(a);
}

/// `put_line` run on after the name: its closing quote where nothing in it
/// closed it, a star where it is still being written, its kind and `<`
/// where it is locked, the line padded to its length.
fn line_end(a: &mut Asm) {
    let (closed, shut, starred, known, locked, unlocked) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.lda(imm(b' '));
    a.ldx(z(zp::QUOTED));
    a.bne(closed);
    a.lda(imm(b'"'));
    a.bind(closed);
    a.jsr("chunk_put");
    a.ldx(imm(0));
    a.lda(abs(field(entry::TYPE)).x());
    a.bmi(shut);
    a.lda(imm(b'*'));
    a.bne(starred);
    a.bind(shut);
    a.lda(imm(b' '));
    a.bind(starred);
    a.jsr("chunk_put");
    a.lda(abs(field(entry::TYPE)).x());
    a.and(imm(NAMED_KIND_BITS));
    a.cmp(imm(UNKNOWN_KIND));
    a.bcc(known);
    a.lda(imm(UNKNOWN_KIND));
    a.bind(known);
    // Three letters a kind.
    a.sta(z(zp::COUNT));
    a.asl(Acc);
    a.adc(z(zp::COUNT));
    a.tax();
    for letter in 0..3 {
        a.lda(abs(sym("kind_names") + letter).x());
        a.jsr("chunk_put");
    }
    a.ldx(imm(0));
    a.lda(abs(field(entry::TYPE)).x());
    a.and(imm(kind::LOCKED));
    a.beq(unlocked);
    a.lda(imm(b'<'));
    a.bne(locked);
    a.bind(unlocked);
    a.lda(imm(b' '));
    a.bind(locked);
    a.jsr("chunk_put");
    a.lda(imm(PADDING_SPACES));
    a.sec();
    a.sbc(z(zp::LEADING));
    a.jsr("chunk_put_spaces");
    a.lda(imm(0));
    a.jsr("chunk_put");
    a.sty(z(zp::LINE_AT));
    a.rts();
}

/// `put_header`: the load address and the disk's line. `put_free_line`:
/// the BAM's free blocks on every track but the directory's, and the
/// program's end.
fn header_lines(a: &mut Asm) {
    a.here("put_header");
    let (name, id) = (a.local(), a.local());
    a.ldy(imm(0));
    a.lda(imm(lo(LOAD_ADDRESS)));
    a.jsr("chunk_put");
    a.lda(imm(hi(LOAD_ADDRESS)));
    a.jsr("chunk_put");
    a.lda(imm(LINK));
    a.jsr("chunk_put");
    a.jsr("chunk_put");
    a.lda(imm(0));
    a.jsr("chunk_put");
    a.jsr("chunk_put");
    a.lda(imm(REVERSE_ON));
    a.jsr("chunk_put");
    a.lda(imm(b'"'));
    a.jsr("chunk_put");
    a.ldx(imm(0));
    a.bind(name);
    a.lda(abs(BAM + u16::from(bam::NAME)).x());
    a.jsr("chunk_put_shown");
    a.inx();
    a.cpx(imm(NAME_BYTES));
    a.bne(name);
    a.lda(imm(b'"'));
    a.jsr("chunk_put");
    a.lda(imm(b' '));
    a.jsr("chunk_put");
    a.ldx(imm(0));
    a.bind(id);
    a.lda(abs(BAM + u16::from(bam::ID)).x());
    a.jsr("chunk_put_shown");
    a.inx();
    a.cpx(imm(bam::ID_AND_TYPE_BYTES));
    a.bne(id);
    a.lda(imm(0));
    a.jmp("chunk_put");

    a.here("put_free_line");
    let text = a.local();
    a.ldy(imm(0));
    a.lda(imm(LINK));
    a.jsr("chunk_put");
    a.jsr("chunk_put");
    a.jsr("count_free");
    a.lda(z(zp::FREE));
    a.jsr("chunk_put");
    a.lda(z(zp::FREE + 1));
    a.jsr("chunk_put");
    a.ldx(imm(0));
    a.bind(text);
    a.lda(abs("blocks_free").x());
    a.jsr("chunk_put");
    a.inx();
    a.cpx(imm(byte(BLOCKS_FREE.len())));
    a.bne(text);
    a.lda(imm(FREE_LINE_SPACES));
    a.jsr("chunk_put_spaces");
    a.lda(imm(0));
    for _ in 0..3 {
        a.jsr("chunk_put");
    }
    a.rts();
}

/// `chunk_put_spaces`: A spaces; X kept. `chunk_put_shown`: A, a shifted
/// space shown as a space. `chunk_put`: A.
fn chunk_puts(a: &mut Asm) {
    a.here("chunk_put_spaces");
    let each = a.local();
    a.sta(z(zp::COUNT));
    a.lda(imm(b' '));
    a.bind(each);
    a.jsr("chunk_put");
    a.dec(z(zp::COUNT));
    a.bne(each);
    a.rts();

    a.here("chunk_put_shown");
    a.cmp(imm(PADDING));
    a.bne("chunk_put");
    a.lda(imm(b' '));
    a.here("chunk_put");
    a.sta(z(zp::FILE_POINTER).ind_y());
    a.iny();
    a.rts();
}
