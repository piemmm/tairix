//! The catalogue: reading a drive's, finding entries by name, a file's
//! fields packed and unpacked, room found for a new file and entries made
//! or removed, and the two displays of it, *CAT and *INFO.
//!
//! Entries are kept in descending order of first sector. A name is matched
//! as DFS 1.20 matches it: letters in either case, `#` for any one character
//! including a name's padding, `*` for any run, and nothing past the
//! pattern but the name's padding.

use super::far;
use super::text::{MANY, NAME_LEN, ONE};
use super::ws::{catalogue as cat, page, zp as z};
use crate::rom::asm::{abs, imm, zp, Acc, Asm, RTS};
use crate::rom::os::entry;

const NAMES: u16 = cat::NAMES;
const INFO: u16 = cat::INFO;
/// *CAT's second column.
const COLUMN: u8 = 20;
/// No entry chosen, an offset no entry has.
const NONE: u8 = 0xFF;
/// The names of the four boot options.
const OPTIONS: [&str; 4] = ["off", "LOAD", "RUN", "EXEC"];

pub(super) fn emit(a: &mut Asm) {
    reading(a);
    finding(a);
    fields(a);
    allocating(a);
    entries(a);
    info(a);
    catalogue(a);
    heading(a);
    gbpb_info(a);
}

/// `current_catalogue` and `name_catalogue`: the current drive's catalogue,
/// or the named file's drive's, read into &0E00.
fn reading(a: &mut Asm) {
    a.here("current_catalogue");
    a.lda(abs(page::DRIVE));
    let read = a.local();
    a.bpl(read);
    a.here("name_catalogue");
    a.lda(zp(z::DRIVE));
    a.bind(read);
    a.sta(abs(page::OP_DRIVE));
    a.jmp("read_catalogue");
}

/// `find_first`, or `find_pattern` with `*` and `#` wild, and then
/// `find_next`: the next entry from ENTRY on that the name matches, its
/// offset in ENTRY and Y with C clear, or C set if none.
fn finding(a: &mut Asm) {
    a.here("find_pattern");
    a.lda(imm(0xFF));
    let start = a.local();
    a.bne(start);
    a.here("find_first");
    a.lda(imm(0));
    a.bind(start);
    a.sta(abs(page::PATTERN));
    a.lda(imm(0));
    a.sta(zp(z::ENTRY));
    a.here("find_next");
    a.lda(zp(z::ENTRY));
    a.clc();
    a.adc(imm(cat::FIRST));
    let none = a.local();
    a.bcs(none);
    a.sta(zp(z::ENTRY));
    a.cmp(abs(cat::END));
    let test = a.local();
    a.beq(test);
    a.bcs(none);
    a.bind(test);
    a.jsr("match_entry");
    a.bcs("find_next");
    a.ldy(zp(z::ENTRY));
    a.rts();
    a.bind(none);
    a.sec();
    a.rts();

    matching(a);
}

/// Whether an entry matches the name: its directory, then the name, literally or as a pattern.
fn matching(a: &mut Asm) {
    // C clear if the entry at ENTRY is in DIR, or DIR is a wildcard in a
    // pattern, and its name matches NAME.
    a.here("match_entry");
    a.ldy(zp(z::ENTRY));
    a.lda(zp(z::DIR));
    let any_dir = a.local();
    let exact_dir = a.local();
    a.bit(abs(page::PATTERN));
    a.bpl(exact_dir);
    a.jsr("is_wild");
    a.beq(any_dir);
    a.bind(exact_dir);
    a.jsr("upper");
    a.sta(zp(z::T0));
    a.lda(abs(NAMES + u16::from(cat::ENTRY_DIR)).y());
    a.and(imm(!cat::LOCKED));
    a.jsr("upper");
    a.cmp(zp(z::T0));
    a.bne("match_fail");
    a.bind(any_dir);
    a.sty(zp(z::POINTER));
    a.lda(imm(crate::rom::asm::hi(NAMES)));
    a.sta(zp(z::POINTER + 1));
    a.ldx(imm(0));
    a.ldy(imm(0));

    name_matching(a);
}

/// The pattern against a name from given places: `*` tried at each, `#` any one character.
fn name_matching(a: &mut Asm) {
    // The pattern from X against the name at (POINTER) from Y.
    a.here("match_from");
    a.cpx(imm(NAME_LEN));
    let pattern_done = a.local();
    a.beq(pattern_done);
    a.lda(zp(z::NAME).x());
    let name_done = a.local();
    let next = a.local();
    let literal = a.local();
    a.bit(abs(page::PATTERN));
    a.bpl(literal);
    a.cmp(imm(MANY));
    let many = a.local();
    a.beq(many);
    a.cpy(imm(NAME_LEN));
    a.beq(name_done);
    a.cmp(imm(ONE));
    a.beq(next);
    let compare = a.local();
    a.bne(compare);
    a.bind(literal);
    a.cpy(imm(NAME_LEN));
    a.beq(name_done);
    a.bind(compare);
    a.jsr("upper");
    a.sta(zp(z::T1));
    a.lda(zp(z::POINTER).ind_y());
    a.jsr("upper");
    a.cmp(zp(z::T1));
    a.bne("match_fail");
    a.bind(next);
    a.inx();
    a.iny();
    a.bne("match_from");
    // A run: the rest of the pattern tried from each place in the name.
    a.bind(many);
    a.inx();
    let place = a.here_local();
    a.txa();
    a.pha();
    a.tya();
    a.pha();
    a.jsr("match_from");
    a.pla();
    a.tay();
    a.pla();
    a.tax();
    a.bcc("match_pass");
    a.cpy(imm(NAME_LEN));
    a.beq("match_fail");
    a.iny();
    a.bne(place);
    // Past the pattern, only the name's padding.
    a.bind(pattern_done);
    a.cpy(imm(NAME_LEN));
    a.beq("match_pass");
    a.lda(zp(z::POINTER).ind_y());
    a.cmp(imm(b' '));
    a.bne("match_fail");
    a.iny();
    a.bne(pattern_done);
    // Past the name, only the pattern's padding.
    a.bind(name_done);
    a.cpx(imm(NAME_LEN));
    a.beq("match_pass");
    a.lda(zp(z::NAME).x());
    a.cmp(imm(b' '));
    a.bne("match_fail");
    a.inx();
    a.bne(name_done);
    a.here("match_pass");
    a.clc();
    a.rts();
    a.here("match_fail");
    a.sec();
    a.rts();

    // A letter in upper case.
    a.here("upper");
    a.cmp(imm(b'a'));
    a.bcc(RTS);
    a.cmp(imm(b'z' + 1));
    a.bcs(RTS);
    a.and(imm(0xDF));
    a.rts();
}

/// A field's top two bits from the mixed byte at Y.
fn mixed_bits(a: &mut Asm, shift: u8) {
    a.lda(abs(INFO + u16::from(cat::MIXED)).y());
    for _ in 0..shift {
        a.lsr(Acc);
    }
    a.and(imm(3));
}

/// The entry at Y: `entry_start` its first sector into T2 and T3,
/// `entry_sectors` the sectors its length covers into T0 and T1, and
/// `entry_info` its fields unpacked into the INFO record, an address whose
/// top bits are both set reading as &FFFF in its top half.
fn fields(a: &mut Asm) {
    a.here("entry_start");
    a.lda(abs(INFO + u16::from(cat::START)).y());
    a.sta(zp(z::T2));
    mixed_bits(a, cat::START_SHIFT);
    a.sta(zp(z::T3));
    a.rts();

    a.here("entry_sectors");
    mixed_bits(a, cat::LENGTH_SHIFT);
    a.sta(zp(z::T1));
    a.lda(abs(INFO + u16::from(cat::LENGTH) + 1).y());
    a.sta(zp(z::T0));
    a.lda(abs(INFO + u16::from(cat::LENGTH)).y());
    a.beq(RTS);
    a.inc(zp(z::T0));
    a.bne(RTS);
    a.inc(zp(z::T1));
    a.rts();

    a.here("entry_info");
    a.ldy(zp(z::ENTRY));
    for (field, to, shift) in [
        (cat::LOAD, page::INFO_LOAD, cat::LOAD_SHIFT),
        (cat::EXEC, page::INFO_EXEC, cat::EXEC_SHIFT),
        (cat::LENGTH, page::INFO_LENGTH, cat::LENGTH_SHIFT),
    ] {
        a.lda(abs(INFO + u16::from(field)).y());
        a.sta(abs(to));
        a.lda(abs(INFO + u16::from(field) + 1).y());
        a.sta(abs(to + 1));
        mixed_bits(a, shift);
        a.ldx(imm(0));
        if field != cat::LENGTH {
            a.cmp(imm(3));
            let low = a.local();
            a.bne(low);
            a.lda(imm(0xFF));
            a.tax();
            a.bind(low);
        }
        a.sta(abs(to + 2));
        a.stx(abs(to + 3));
    }
    a.jsr("entry_start");
    a.lda(zp(z::T2));
    a.sta(abs(page::INFO_START));
    a.lda(zp(z::T3));
    a.sta(abs(page::INFO_START + 1));
    a.rts();
}

/// `allocate`: room for T4 and T5 sectors, the first gap from the catalogue
/// up that holds them; their first sector in T6 and T7, and in ENTRY the
/// offset the new entry takes to keep the order. Disk full if none.
fn allocating(a: &mut Asm) {
    a.here("allocate");
    a.lda(imm(cat::OWN_SECTORS));
    a.sta(zp(z::T6));
    a.lda(imm(0));
    a.sta(zp(z::T7));
    a.ldy(abs(cat::END));
    let file = a.here_local();
    let last = a.local();
    a.cpy(imm(0));
    a.beq(last);
    a.jsr("entry_start");
    a.lda(zp(z::T2));
    a.sec();
    a.sbc(zp(z::T6));
    a.sta(zp(z::T0));
    a.lda(zp(z::T3));
    a.sbc(zp(z::T7));
    let found_before = a.local();
    a.jsr("room_holds");
    a.bcs(found_before);
    a.jsr("entry_sectors");
    a.lda(zp(z::T2));
    a.clc();
    a.adc(zp(z::T0));
    a.sta(zp(z::T6));
    a.lda(zp(z::T3));
    a.adc(zp(z::T1));
    a.sta(zp(z::T7));
    a.tya();
    a.sec();
    a.sbc(imm(cat::FIRST));
    a.tay();
    a.jmp(file);
    a.bind(found_before);
    a.tya();
    a.clc();
    a.adc(imm(cat::FIRST));
    a.sta(zp(z::ENTRY));
    a.rts();
    // Past the last file, the room to the end of the disc.
    a.bind(last);
    a.lda(abs(cat::SECTORS));
    a.sec();
    a.sbc(zp(z::T6));
    a.sta(zp(z::T0));
    a.lda(abs(cat::OPTION));
    a.and(imm(3));
    a.sbc(zp(z::T7));
    a.jsr("room_holds");
    far(a, Asm::bcs, "err_disk_full");
    a.lda(imm(cat::FIRST));
    a.sta(zp(z::ENTRY));
    a.rts();

    // C set if the room T0 and A, unsigned, holds T4 and T5 sectors; a room
    // that has gone negative holds nothing.
    a.here("room_holds");
    a.bcc(RTS);
    a.cmp(zp(z::T5));
    a.bne(RTS);
    a.lda(zp(z::T0));
    a.cmp(zp(z::T4));
    a.rts();
}

/// `remove_entry` takes the entry at ENTRY out, the place the last one left
/// cleared; `make_room` opens a gap for
/// one there, Cat full if the catalogue holds its 31; `set_entry` fills it
/// from NAME, DIR and the INFO record, and `set_fields` from the record
/// alone.
fn entries(a: &mut Asm) {
    a.here("remove_entry");
    a.ldy(zp(z::ENTRY));
    let move_down = a.here_local();
    let removed = a.local();
    a.cpy(abs(cat::END));
    a.bcs(removed);
    a.lda(abs(NAMES + u16::from(cat::FIRST)).y());
    a.sta(abs(NAMES).y());
    a.lda(abs(INFO + u16::from(cat::FIRST)).y());
    a.sta(abs(INFO).y());
    a.iny();
    a.bne(move_down);
    // The last entry's old place cleared, as DFS 1.20 leaves it.
    a.bind(removed);
    a.lda(imm(0));
    a.ldx(imm(cat::FIRST));
    let clear = a.here_local();
    a.sta(abs(NAMES).y());
    a.sta(abs(INFO).y());
    a.iny();
    a.dex();
    a.bne(clear);
    a.lda(abs(cat::END));
    a.sec();
    a.sbc(imm(cat::FIRST));
    a.sta(abs(cat::END));
    a.rts();

    a.here("make_room");
    a.lda(abs(cat::END));
    a.cmp(imm(cat::LAST));
    far(a, Asm::bcc, "err_cat_full");
    a.cmp(zp(z::ENTRY));
    let made = a.local();
    a.bcc(made);
    a.clc();
    a.adc(imm(cat::FIRST - 1));
    a.tay();
    let move_up = a.here_local();
    a.lda(abs(NAMES).y());
    a.sta(abs(NAMES + u16::from(cat::FIRST)).y());
    a.lda(abs(INFO).y());
    a.sta(abs(INFO + u16::from(cat::FIRST)).y());
    a.cpy(zp(z::ENTRY));
    a.beq(made);
    a.dey();
    a.jmp(move_up);
    a.bind(made);
    a.lda(abs(cat::END));
    a.clc();
    a.adc(imm(cat::FIRST));
    a.sta(abs(cat::END));
    a.rts();

    a.here("set_entry");
    a.ldy(zp(z::ENTRY));
    a.ldx(imm(0));
    let name = a.here_local();
    a.lda(zp(z::NAME).x());
    a.sta(abs(NAMES).y());
    a.iny();
    a.inx();
    a.cpx(imm(NAME_LEN + 1));
    a.bne(name);
    // The fields alone, kept apart for OSFILE's writes to an entry.
    a.here("set_fields");
    a.ldy(zp(z::ENTRY));
    for (field, from) in [
        (cat::LOAD, page::INFO_LOAD),
        (cat::EXEC, page::INFO_EXEC),
        (cat::LENGTH, page::INFO_LENGTH),
    ] {
        a.lda(abs(from));
        a.sta(abs(INFO + u16::from(field)).y());
        a.lda(abs(from + 1));
        a.sta(abs(INFO + u16::from(field) + 1).y());
    }
    a.lda(abs(page::INFO_START));
    a.sta(abs(INFO + u16::from(cat::START)).y());
    // The mixed byte, built from the exec address's top bits down.
    a.lda(abs(page::INFO_EXEC + 2));
    a.and(imm(3));
    a.sta(zp(z::T0));
    for from in [
        page::INFO_LENGTH + 2,
        page::INFO_LOAD + 2,
        page::INFO_START + 1,
    ] {
        a.lda(abs(from));
        a.and(imm(3));
        a.asl(zp(z::T0));
        a.asl(zp(z::T0));
        a.ora(zp(z::T0));
        a.sta(zp(z::T0));
    }
    a.sta(abs(INFO + u16::from(cat::MIXED)).y());
    a.rts();
}

/// `print_info`: the entry at ENTRY as *INFO and the messages show it: its
/// name in nine columns, its lock, its two addresses and length in six hex
/// digits and its first sector in three.
fn info(a: &mut Asm) {
    a.here("print_info");
    a.ldy(zp(z::ENTRY));
    a.lda(abs(NAMES + u16::from(cat::ENTRY_DIR)).y());
    a.and(imm(!cat::LOCKED));
    a.jsr(entry::OSASCI);
    a.lda(imm(b'.'));
    a.jsr(entry::OSASCI);
    a.jsr("print_entry_name");
    a.ldx(imm(2));
    a.jsr("print_spaces");
    a.jsr("print_lock");
    a.ldx(imm(2));
    a.jsr("print_spaces");
    a.jsr("entry_info");
    for field in [page::INFO_LOAD, page::INFO_EXEC, page::INFO_LENGTH] {
        for byte in (0..3).rev() {
            a.lda(abs(field + byte));
            a.jsr("print_hex");
        }
        a.jsr("print_space");
    }
    a.lda(abs(page::INFO_START + 1));
    a.jsr("print_digit");
    a.lda(abs(page::INFO_START));
    a.jsr("print_hex");
    a.jmp(abs(entry::OSNEWL));

    // The entry's seven name characters.
    a.here("print_entry_name");
    a.ldy(zp(z::ENTRY));
    a.ldx(imm(NAME_LEN));
    let name = a.here_local();
    a.lda(abs(NAMES).y());
    a.jsr(entry::OSASCI);
    a.iny();
    a.dex();
    a.bne(name);
    a.rts();

    // L for a locked entry, else a space.
    a.here("print_lock");
    a.ldy(zp(z::ENTRY));
    a.lda(abs(NAMES + u16::from(cat::ENTRY_DIR)).y());
    let unlocked = a.local();
    a.bpl(unlocked);
    a.lda(imm(b'L'));
    a.jmp(abs(entry::OSASCI));
    a.bind(unlocked);
    a.jmp("print_space");
}

/// *CAT: the title and cycle number, the drive and boot option, the
/// current and library directories, then the files two to a line, the
/// current directory's first and each set in order of directory and name.
/// FSCV 5 enters with the command's argument at (&F2),Y.
fn catalogue(a: &mut Asm) {
    a.here("star_cat");
    a.jsr("drive_argument");
    let given = a.local();
    a.bcc(given);
    a.lda(abs(page::DRIVE));
    a.bind(given);
    a.sta(abs(page::OP_DRIVE));
    a.jsr("read_catalogue");
    a.jsr("print_title");
    a.call_with_text("print_inline", &[" ("]);
    a.lda(abs(cat::CYCLE));
    a.jsr("print_hex");
    a.call_with_text("print_inline", &[")\rDrive "]);
    a.lda(abs(page::OP_DRIVE));
    a.jsr("print_digit");
    a.ldx(imm(COLUMN - 7));
    a.jsr("print_spaces");
    a.call_with_text("print_inline", &["Option "]);
    a.lda(abs(cat::OPTION));
    for _ in 0..4 {
        a.lsr(Acc);
    }
    a.and(imm(3));
    a.pha();
    a.jsr("print_digit");
    a.call_with_text("print_inline", &[" ("]);
    a.pla();
    a.tax();
    a.ldy(abs("option_names").x());
    let name = a.here_local();
    a.lda(abs("option_text").y());
    let named = a.local();
    a.beq(named);
    a.jsr(entry::OSASCI);
    a.iny();
    a.bne(name);
    a.bind(named);
    a.call_with_text("print_inline", &[")\rDir. :"]);
    a.lda(abs(page::DRIVE));
    a.ldx(abs(page::DIR));
    a.jsr("print_place");
    a.ldx(imm(COLUMN - 9));
    a.jsr("print_spaces");
    a.call_with_text("print_inline", &["Lib. :"]);
    a.lda(abs(page::LIB_DRIVE));
    a.ldx(abs(page::LIB_DIR));
    a.jsr("print_place");
    a.jsr(entry::OSNEWL);
    a.jsr(entry::OSNEWL);
    cat_files(a);
}

/// The heading's parts: a drive and directory, the title, and the boot
/// options' names.
fn heading(a: &mut Asm) {
    // Drive A, a full stop, directory X.
    a.here("print_place");
    a.jsr("print_digit");
    a.lda(imm(b'.'));
    a.jsr(entry::OSASCI);
    a.txa();
    a.jmp(abs(entry::OSASCI));

    // The title's twelve bytes, a zero among them printing nothing.
    a.here("print_title");
    a.ldx(imm(0));
    let title = a.here_local();
    a.lda(abs("title_places").x());
    a.tay();
    a.lda(abs(NAMES).y());
    a.cpx(imm(cat::TITLE_FIRST));
    let first = a.local();
    a.bcc(first);
    a.lda(abs(INFO).y());
    a.bind(first);
    a.jsr(entry::OSASCI);
    a.inx();
    a.cpx(imm(cat::TITLE_LEN));
    a.bne(title);
    a.rts();
    a.here("title_places");
    for i in 0..cat::TITLE_LEN {
        a.byte(if i < cat::TITLE_FIRST {
            i
        } else {
            i - cat::TITLE_FIRST
        });
    }

    a.here("option_names");
    let mut at = 0;
    for option in OPTIONS {
        a.byte(at);
        at += option.len().to_le_bytes()[0] + 1;
    }
    a.here("option_text");
    for option in OPTIONS {
        a.text(option);
        a.byte(0);
    }
}

/// The files, selected one at a time as the least by group, directory and
/// name of those not yet shown; WORK holds each entry's group, or &FF once
/// it is shown.
fn cat_files(a: &mut Asm) {
    // Each entry's group: 0 in the current directory, 1 otherwise.
    a.lda(abs(page::DIR));
    a.jsr("upper");
    a.sta(zp(z::T7));
    a.ldy(abs(cat::END));
    a.ldx(imm(0));
    let group = a.here_local();
    let grouped = a.local();
    a.cpy(imm(0));
    a.beq(grouped);
    a.lda(abs(NAMES + u16::from(cat::ENTRY_DIR)).y());
    a.and(imm(!cat::LOCKED));
    a.jsr("upper");
    a.cmp(zp(z::T7));
    let current = a.local();
    a.beq(current);
    a.lda(imm(1));
    let grouped_one = a.local();
    a.bne(grouped_one);
    a.bind(current);
    a.lda(imm(0));
    a.bind(grouped_one);
    a.sta(abs(page::WORK).x());
    a.inx();
    a.tya();
    a.sec();
    a.sbc(imm(cat::FIRST));
    a.tay();
    a.jmp(group);
    a.bind(grouped);
    // Column 0 or 1, and the group last shown.
    a.lda(imm(0));
    a.sta(zp(z::UTILITY));
    a.sta(zp(z::UTILITY + 1));

    cat_choose(a);
    cat_order(a);
}

/// Each file in turn, the least not yet shown, printed in its column.
fn cat_choose(a: &mut Asm) {
    a.here("cat_next");
    a.lda(imm(NONE));
    a.sta(zp(z::UTILITY + 2));
    a.ldy(abs(cat::END));
    a.ldx(imm(0));
    let candidate = a.here_local();
    let chosen = a.local();
    a.cpy(imm(0));
    a.beq(chosen);
    a.lda(abs(page::WORK).x());
    let skip = a.local();
    a.bmi(skip);
    a.lda(zp(z::UTILITY + 2));
    a.cmp(imm(NONE));
    let take = a.local();
    a.beq(take);
    a.jsr("cat_less");
    a.bcs(skip);
    a.bind(take);
    a.sty(zp(z::UTILITY + 2));
    a.stx(zp(z::UTILITY + 3));
    a.bind(skip);
    a.inx();
    a.tya();
    a.sec();
    a.sbc(imm(cat::FIRST));
    a.tay();
    a.jmp(candidate);
    a.bind(chosen);
    a.ldy(zp(z::UTILITY + 2));
    a.cpy(imm(NONE));
    let shown = a.local();
    a.bne(shown);
    // All shown: the line ended if a file sits alone on it.
    a.lda(zp(z::UTILITY));
    let alone = a.local();
    a.bne(alone);
    a.rts();
    a.bind(alone);
    a.jmp(abs(entry::OSNEWL));
    a.bind(shown);
    a.sty(zp(z::ENTRY));
    a.ldx(zp(z::UTILITY + 3));
    a.lda(abs(page::WORK).x());
    a.pha();
    a.lda(imm(0xFF));
    a.sta(abs(page::WORK).x());
    a.pla();
    // The other directories follow a blank line.
    a.cmp(zp(z::UTILITY + 1));
    a.sta(zp(z::UTILITY + 1));
    let same = a.local();
    a.beq(same);
    a.lda(zp(z::UTILITY));
    let line_ended = a.local();
    a.beq(line_ended);
    a.jsr(entry::OSNEWL);
    a.bind(line_ended);
    a.jsr(entry::OSNEWL);
    a.lda(imm(0));
    a.sta(zp(z::UTILITY));
    a.bind(same);
    a.ldx(imm(2));
    a.jsr("print_spaces");
    a.lda(zp(z::UTILITY + 1));
    let other = a.local();
    a.bne(other);
    a.ldx(imm(2));
    a.jsr("print_spaces");
    let named = a.local();
    a.jmp(named);
    a.bind(other);
    a.ldy(zp(z::ENTRY));
    a.lda(abs(NAMES + u16::from(cat::ENTRY_DIR)).y());
    a.and(imm(!cat::LOCKED));
    a.jsr(entry::OSASCI);
    a.lda(imm(b'.'));
    a.jsr(entry::OSASCI);
    a.bind(named);
    a.jsr("print_entry_name");
    a.ldx(imm(2));
    a.jsr("print_spaces");
    a.jsr("print_lock");
    a.lda(zp(z::UTILITY));
    a.eor(imm(1));
    a.sta(zp(z::UTILITY));
    let second = a.local();
    a.beq(second);
    a.ldx(imm(COLUMN - 14));
    a.jsr("print_spaces");
    a.jmp("cat_next");
    a.bind(second);
    a.jsr(entry::OSNEWL);
    a.jmp("cat_next");
}

/// The order *CAT shows the files in.
fn cat_order(a: &mut Asm) {
    // C clear if the entry at Y, number X, sorts before the chosen one at
    // UTILITY + 2, number UTILITY + 3: by group, directory, then name.
    a.here("cat_less");
    a.stx(zp(z::T5));
    a.sty(zp(z::T6));
    a.lda(abs(page::WORK).x());
    a.ldx(zp(z::UTILITY + 3));
    a.cmp(abs(page::WORK).x());
    let decided = a.local();
    a.bne(decided);
    a.ldx(imm(NAME_LEN));
    let byte = a.here_local();
    a.ldy(zp(z::UTILITY + 2));
    a.jsr("cat_key");
    a.sta(zp(z::T4));
    a.ldy(zp(z::T6));
    a.jsr("cat_key");
    a.cmp(zp(z::T4));
    a.bne(decided);
    // The directory, at the name's end, is compared first.
    a.cpx(imm(NAME_LEN));
    let dir_done = a.local();
    a.bne(dir_done);
    a.ldx(imm(0));
    a.beq(byte);
    a.bind(dir_done);
    a.inx();
    a.cpx(imm(NAME_LEN));
    a.bne(byte);
    a.sec();
    a.bind(decided);
    a.ldx(zp(z::T5));
    a.ldy(zp(z::T6));
    a.rts();

    // Byte X of the entry at Y, upper case: the name's, or the directory's
    // for X seven.
    a.here("cat_key");
    a.sty(zp(z::T3));
    a.txa();
    a.clc();
    a.adc(zp(z::T3));
    a.tay();
    a.lda(abs(NAMES).y());
    a.and(imm(!cat::LOCKED));
    a.jmp("upper");
}

/// OSGBPB 5 to 8, reason A, the block at (BLOCK): the current drive's title,
/// boot option and number, the current directory, the library, or the names
/// in the current directory from the block's PTR on, the cycle number in
/// its handle byte, C set if they ran out before its count did.
fn gbpb_info(a: &mut Asm) {
    a.here("gbpb_info");
    a.cmp(imm(GBPB_NAMES + 1));
    far(a, Asm::bcc, "gbpb_return");
    a.pha();
    a.ldy(imm(1));
    a.lda(zp(z::BLOCK).ind_y());
    a.sta(zp(z::POINTER));
    a.iny();
    a.lda(zp(z::BLOCK).ind_y());
    a.sta(zp(z::POINTER + 1));
    a.pla();
    a.cmp(imm(GBPB_TITLE));
    let not_title = a.local();
    a.bne(not_title);
    a.jsr("current_catalogue");
    a.ldy(imm(0));
    a.lda(imm(cat::TITLE_LEN));
    a.sta(zp(z::POINTER).ind_y());
    let title = a.here_local();
    a.ldx(abs("title_places").y());
    a.lda(abs(NAMES).x());
    a.cpy(imm(cat::TITLE_FIRST));
    let first = a.local();
    a.bcc(first);
    a.lda(abs(INFO).x());
    a.bind(first);
    a.iny();
    a.sta(zp(z::POINTER).ind_y());
    a.cpy(imm(cat::TITLE_LEN));
    a.bne(title);
    a.lda(abs(cat::OPTION));
    for _ in 0..4 {
        a.lsr(Acc);
    }
    a.and(imm(3));
    a.iny();
    a.sta(zp(z::POINTER).ind_y());
    // Then the drive, as DFS 1.20 gives it.
    a.lda(abs(page::DRIVE));
    a.iny();
    a.sta(zp(z::POINTER).ind_y());
    a.jmp("gbpb_return");
    a.bind(not_title);
    a.cmp(imm(GBPB_NAMES));
    far(a, Asm::bne, "gbpb_names");
    // 6 and 7: a drive and a directory, each a character after its length.
    a.ldx(imm(0));
    a.cmp(imm(GBPB_DIR));
    let current = a.local();
    a.beq(current);
    a.ldx(imm(2));
    a.bind(current);
    a.ldy(imm(0));
    a.lda(imm(1));
    a.sta(zp(z::POINTER).ind_y());
    a.iny();
    a.lda(abs(page::DRIVE).x());
    a.clc();
    a.adc(imm(b'0'));
    a.sta(zp(z::POINTER).ind_y());
    a.iny();
    a.lda(imm(1));
    a.sta(zp(z::POINTER).ind_y());
    a.iny();
    a.lda(abs(page::DIR).x());
    a.sta(zp(z::POINTER).ind_y());
    a.jmp("gbpb_return");

    gbpb_names(a);
}

/// OSGBPB 8: the current directory's names from the block's PTR on.
fn gbpb_names(a: &mut Asm) {
    a.here("gbpb_names");
    a.jsr("current_catalogue");
    a.ldy(imm(0));
    a.lda(abs(cat::CYCLE));
    a.sta(zp(z::BLOCK).ind_y());
    a.lda(abs(page::DIR));
    a.jsr("upper");
    a.sta(zp(z::T7));
    a.ldy(imm(GBPB_PTR));
    a.lda(zp(z::BLOCK).ind_y());
    a.sta(zp(z::ENTRY));
    let name = a.here_local();
    let done = a.local();
    let out = a.local();
    a.ldy(imm(GBPB_COUNT));
    a.lda(zp(z::BLOCK).ind_y());
    for k in 1..4 {
        a.ldy(imm(GBPB_COUNT + k));
        a.ora(zp(z::BLOCK).ind_y());
    }
    a.beq(done);
    let scan = a.here_local();
    a.lda(zp(z::ENTRY));
    a.clc();
    a.adc(imm(cat::FIRST));
    a.bcs(out);
    a.sta(zp(z::ENTRY));
    a.cmp(abs(cat::END));
    let inside = a.local();
    a.beq(inside);
    a.bcs(out);
    a.bind(inside);
    a.tax();
    a.lda(abs(NAMES + u16::from(cat::ENTRY_DIR)).x());
    a.and(imm(!cat::LOCKED));
    a.jsr("upper");
    a.cmp(zp(z::T7));
    a.bne(scan);
    // The name, its length first.
    a.ldy(imm(0));
    a.lda(imm(cat::NAME_LEN));
    a.sta(zp(z::POINTER).ind_y());
    let copy = a.here_local();
    a.lda(abs(NAMES).x());
    a.iny();
    a.sta(zp(z::POINTER).ind_y());
    a.inx();
    a.cpy(imm(cat::NAME_LEN));
    a.bne(copy);
    a.lda(zp(z::POINTER));
    a.clc();
    a.adc(imm(cat::NAME_LEN + 1));
    a.sta(zp(z::POINTER));
    let carried = a.local();
    a.bcc(carried);
    a.inc(zp(z::POINTER + 1));
    a.bind(carried);
    // The count one less.
    a.ldy(imm(GBPB_COUNT));
    a.clc();
    for _ in 0..4 {
        a.lda(zp(z::BLOCK).ind_y());
        a.sbc(imm(0));
        a.sta(zp(z::BLOCK).ind_y());
        a.iny();
    }
    a.jmp(name);
    a.bind(done);
    a.clc();
    let report = a.local();
    a.bcc(report);
    a.bind(out);
    a.sec();
    a.bind(report);
    a.php();
    a.ldy(imm(1));
    a.lda(zp(z::POINTER));
    a.sta(zp(z::BLOCK).ind_y());
    a.iny();
    a.lda(zp(z::POINTER + 1));
    a.sta(zp(z::BLOCK).ind_y());
    a.ldy(imm(GBPB_PTR));
    a.lda(zp(z::ENTRY));
    a.sta(zp(z::BLOCK).ind_y());
    a.lda(imm(0));
    for _ in 0..3 {
        a.iny();
        a.sta(zp(z::BLOCK).ind_y());
    }
    a.plp();
    a.jmp("gbpb_return");
}

/// OSGBPB's reasons and block fields this module reads.
const GBPB_TITLE: u8 = 5;
const GBPB_DIR: u8 = 6;
const GBPB_NAMES: u8 = 8;
const GBPB_COUNT: u8 = 5;
const GBPB_PTR: u8 = 9;
