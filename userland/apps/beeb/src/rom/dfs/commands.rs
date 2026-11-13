//! The commands and FSCV: the DFS's own commands, offered by the MOS as the
//! filing system's when no ROM knows them, and the utilities, offered as
//! service calls whichever filing system is current; *OPT, *RUN and `*/`,
//! and a command that names no command running the file it names, from the
//! current directory and then the library.
//!
//! A command missing an argument it needs is a Syntax error showing what
//! *HELP shows for it. *BACKUP and *DESTROY ask "Go (Y/N) ?" unless the
//! command before was *ENABLE.

use super::errors::SYNTAX;
use super::far;
use super::ws::{catalogue as cat, page, zp as z};
use crate::machine::{OS_BASE, SIDEWAYS_BASE};
use crate::rom::asm::{abs, hi, imm, lo, sym, zp, Acc, Asm, RTS};
use crate::rom::os::{entry, fsc, page_d, zp as os};

const CR: u8 = 0x0D;
/// The OSBYTE that closes the *SPOOL and *EXEC files, and the one that
/// flushes the input buffer.
const OSBYTE_CLOSE_SPOOL_EXEC: u8 = 0x77;
const OSBYTE_FLUSH_INPUT: u8 = 0x0F;
/// The *OPT numbers the DFS takes.
const OPT_MESSAGES: u8 = 1;
const OPT_BOOT: u8 = 4;

/// A command: its name, the routine that runs it, and its syntax as *HELP
/// shows it, empty if it takes no argument.
pub(super) struct Command {
    pub name: &'static str,
    pub routine: &'static str,
    pub syntax: &'static str,
    /// It needs an argument.
    pub needs: bool,
}

const fn command(
    name: &'static str,
    routine: &'static str,
    syntax: &'static str,
    needs: bool,
) -> Command {
    Command {
        name,
        routine,
        syntax,
        needs,
    }
}

/// The DFS's commands, as *HELP DFS lists them.
pub(super) const DFS: [Command; 14] = [
    command("ACCESS", "star_access", "<afsp> (L)", true),
    command("BACKUP", "star_backup", "<source> <dest.>", true),
    command("COMPACT", "star_compact", "(<drive>)", false),
    command("COPY", "star_copy", "<source> <dest.> <afsp>", true),
    command("DELETE", "star_delete", "<fsp>", true),
    command("DESTROY", "star_destroy", "<afsp>", true),
    command("DIR", "star_dir", "(<dir>)", false),
    command("DRIVE", "star_drive", "<drive>", true),
    command("ENABLE", "star_enable", "", false),
    command("INFO", "star_info", "<afsp>", true),
    command("LIB", "star_lib", "(<dir>)", false),
    command("RENAME", "star_rename", "<old fsp> <new fsp>", true),
    command("TITLE", "star_title", "<title>", true),
    command("WIPE", "star_wipe", "<afsp>", true),
];

/// The utilities, as *HELP UTILS lists them, and DISC's other spelling,
/// which it does not.
pub(super) const UTILS: [Command; 5] = [
    command("BUILD", "star_build", "<fsp>", true),
    command("DISC", "star_disc", "", false),
    command("DUMP", "star_dump", "<fsp>", true),
    command("LIST", "star_list", "<fsp>", true),
    command("TYPE", "star_type", "<fsp>", true),
];
const DISK: Command = command("DISK", "star_disc", "", false);

pub(super) fn emit(a: &mut Asm) {
    tables(a);
    matching(a);
    fsc_entry(a);
    options(a);
    running(a);
    settings(a);
    files(a);
    confirm(a);
}

/// Every command, by index: the DFS's, the utilities, then DISK.
fn all() -> impl Iterator<Item = &'static Command> {
    DFS.iter()
        .chain(UTILS.iter())
        .chain(core::iter::once(&DISK))
}

/// The two command tables at `command_names`: each name, then its index
/// with the top bit set, a zero ending each table. Each command's routine
/// address less one, whether it needs an argument, where its name starts,
/// and its syntax, by index.
fn tables(a: &mut Asm) {
    a.here("command_names");
    let mut offsets = alloc::vec::Vec::new();
    let mut at = 0;
    for (number, command) in all().enumerate() {
        if number == DFS.len() {
            a.byte(0);
            at += 1;
        }
        offsets.push(byte(at));
        a.text(command.name);
        a.byte(self::index(number) | 0x80);
        at += command.name.len() + 1;
    }
    a.byte(0);
    for (number, keyword) in HELP.iter().enumerate() {
        a.text(keyword);
        a.byte((HELP_FIRST_INDEX + index(number)) | 0x80);
    }
    a.byte(0);
    a.here("name_offsets");
    a.bytes(&offsets);
    a.here("command_low");
    for command in all() {
        a.byte(lo(sym(command.routine) + -1));
    }
    a.here("command_high");
    for command in all() {
        a.byte(hi(sym(command.routine) + -1));
    }
    a.here("command_needs");
    for command in all() {
        a.byte(u8::from(command.needs));
    }
    a.here("syntax_offsets");
    let mut at = 0;
    for command in all() {
        a.byte(byte(at));
        at += command.syntax.len() + 1;
    }
    a.here("syntax_text");
    for command in all() {
        a.text(command.syntax);
        a.byte(0);
    }
}

/// *HELP's keywords, which follow the commands in `command_names` with
/// indices past theirs.
const HELP: [&str; 2] = ["DFS", "UTILS"];
pub(super) const HELP_FIRST_INDEX: u8 = index(DFS.len() + UTILS.len() + 1);

/// Where the utilities' table starts in `command_names`, and the keywords':
/// past each name before, its index byte and each table's zero.
pub(super) const UTIL_TABLE: u8 = byte(names_len(&DFS) + 1);
pub(super) const HELP_TABLE: u8 =
    byte(names_len(&DFS) + 1 + names_len(&UTILS) + DISK.name.len() + 1 + 1);

/// The bytes the names of `commands` take, each with its index byte.
const fn names_len(commands: &[Command]) -> usize {
    let mut len = 0;
    let mut i = 0;
    while i < commands.len() {
        len += commands[i].name.len() + 1;
        i += 1;
    }
    len
}

/// The index of the DFS command named `name`.
///
/// # Panics
///
/// When it is built, if there is none.
pub(super) const fn index_of(name: &str) -> u8 {
    let mut i = 0;
    while i < DFS.len() {
        if eq(DFS[i].name.as_bytes(), name.as_bytes()) {
            return index(i);
        }
        i += 1;
    }
    panic!("no such command");
}

const fn eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut i = 0;
    while i < a.len() {
        if a[i] != b[i] {
            return false;
        }
        i += 1;
    }
    true
}

/// `n` as the byte the image holds it in.
///
/// # Panics
///
/// When it is built, if `n` does not fit.
const fn byte(n: usize) -> u8 {
    assert!(n <= 0xFF, "a table past a byte's reach");
    n.to_le_bytes()[0]
}

/// Command `n`'s index, which shares its byte with the top bit.
///
/// # Panics
///
/// When it is built, if `n` does not fit in seven bits.
const fn index(n: usize) -> u8 {
    assert!(n <= 0x7F, "a command index past seven bits");
    byte(n)
}

/// `match_command`: the command at (&F2),Y against the table from X, as
/// the MOS matches its own: letters in either case, a full stop ending an
/// abbreviation, and the name not run on into a letter. C clear with X its
/// index and Y at its argument; C set with Y kept if none matches.
fn matching(a: &mut Asm) {
    a.here("match_command");
    a.sty(zp(z::T5));
    let command = a.here_local();
    a.ldy(zp(z::T5));
    let letter = a.here_local();
    a.lda(abs("command_names").x());
    let end = a.local();
    a.bmi(end);
    let none = a.local();
    a.beq(none);
    a.lda(zp(os::TEXT).ind_y());
    a.cmp(imm(b'.'));
    let abbreviated = a.local();
    a.beq(abbreviated);
    a.jsr("upper");
    a.cmp(abs("command_names").x());
    let next = a.local();
    a.bne(next);
    a.iny();
    a.inx();
    a.bne(letter);
    a.bind(abbreviated);
    a.iny();
    let skip = a.here_local();
    a.lda(abs("command_names").x());
    let found = a.local();
    a.bmi(found);
    a.inx();
    a.bne(skip);
    // The name ends here, and the line must not run on in a letter.
    a.bind(end);
    a.lda(zp(os::TEXT).ind_y());
    a.jsr("upper");
    a.cmp(imm(b'A'));
    a.bcc(found);
    a.cmp(imm(b'Z' + 1));
    a.bcc(next);
    a.bind(found);
    a.lda(abs("command_names").x());
    a.and(imm(0x7F));
    a.tax();
    a.jsr("next_word");
    a.clc();
    a.rts();
    a.bind(next);
    let past = a.here_local();
    a.lda(abs("command_names").x());
    a.inx();
    a.asl(Acc);
    a.bcc(past);
    a.jmp(command);
    a.bind(none);
    a.ldy(zp(z::T5));
    a.sec();
    a.rts();

    running_commands(a);
}

/// A matched command run, or its Syntax error.
fn running_commands(a: &mut Asm) {
    // Run command X, its argument at (&F2),Y: a Syntax error if it needs
    // one and the line has ended.
    a.here("run_command");
    a.lda(abs("command_needs").x());
    let go = a.local();
    a.beq(go);
    a.lda(zp(os::TEXT).ind_y());
    a.cmp(imm(CR));
    far(a, Asm::bne, "syntax");
    a.bind(go);
    a.lda(abs("command_high").x());
    a.pha();
    a.lda(abs("command_low").x());
    a.pha();
    a.rts();

    // "Syntax: " and command X's name and syntax.
    a.here("syntax");
    a.stx(zp(z::T4));
    a.lda(imm(SYNTAX));
    a.jsr("error_start");
    a.jsr("error_text");
    a.text("Syntax: ");
    a.byte(0);
    a.ldy(zp(z::T4));
    a.lda(abs("name_offsets").y());
    a.tay();
    let name = a.here_local();
    a.lda(abs("command_names").y());
    let spelt = a.local();
    a.bmi(spelt);
    a.jsr("error_char");
    a.iny();
    a.bne(name);
    a.bind(spelt);
    a.ldy(zp(z::T4));
    a.lda(abs("syntax_offsets").y());
    a.tay();
    a.lda(abs("syntax_text").y());
    let raise = a.local();
    a.beq(raise);
    a.lda(imm(b' '));
    a.jsr("error_char");
    let text = a.here_local();
    a.lda(abs("syntax_text").y());
    a.beq(raise);
    a.jsr("error_char");
    a.iny();
    a.bne(text);
    a.bind(raise);
    a.jmp("error_raise");
}

/// FSCV, A its reason: *OPT, EOF, `*/`, an unknown command, *RUN, *CAT,
/// shutting down, the handles, and a command line about to run.
fn fsc_entry(a: &mut Asm) {
    a.here("dfs_fsc");
    a.cmp(imm(fsc::COMMAND_LINE));
    let not_line = a.local();
    a.bne(not_line);
    // *ENABLE's countdown, past zero disabling, kept only while the DFS
    // holds its workspace.
    a.ldx(zp(os::ROM));
    a.lda(abs(page_d::PRIVATE_WORKSPACE).x());
    a.bpl(RTS);
    a.lda(abs(page::ENABLE));
    a.bmi(RTS);
    a.dec(abs(page::ENABLE));
    a.rts();
    a.bind(not_line);
    a.bcs(RTS);
    a.pha();
    a.jsr("claim_workspace");
    a.pla();
    a.cmp(imm(fsc::EOF));
    far(a, Asm::bne, "dfs_eof");
    a.cmp(imm(fsc::OPT));
    far(a, Asm::bne, "dfs_opt");
    a.cmp(imm(fsc::HANDLES));
    let not_handles = a.local();
    a.bne(not_handles);
    a.ldx(imm(super::ws::channel::FIRST_HANDLE));
    a.ldy(imm(super::ws::channel::LAST_HANDLE));
    a.rts();
    a.bind(not_handles);
    a.cmp(imm(fsc::SHUT_DOWN));
    let not_shut = a.local();
    a.bne(not_shut);
    a.lda(imm(OSBYTE_CLOSE_SPOOL_EXEC));
    a.jsr(entry::OSBYTE);
    a.jmp("close_all");
    a.bind(not_shut);
    // The rest read a line at X and Y.
    a.stx(zp(os::TEXT));
    a.sty(zp(os::TEXT + 1));
    a.ldy(imm(0));
    a.cmp(imm(fsc::CAT));
    far(a, Asm::bne, "star_cat");
    a.cmp(imm(fsc::COMMAND));
    far(a, Asm::beq, "run_file");
    a.ldx(imm(0));
    a.jsr("match_command");
    far(a, Asm::bcc, "run_file");
    a.jmp("run_command");
}

/// *OPT X,Y: 1 turns the messages on for Y non-zero, 4 sets the current
/// drive's boot option to Y's low two bits, and 0 turns the messages off.
fn options(a: &mut Asm) {
    a.here("dfs_opt");
    a.cpx(imm(OPT_BOOT));
    let not_boot = a.local();
    a.bne(not_boot);
    a.tya();
    a.and(imm(3));
    for _ in 0..4 {
        a.asl(Acc);
    }
    a.sta(zp(z::T4));
    a.jsr("current_catalogue");
    a.lda(abs(cat::OPTION));
    a.and(imm(0x0F));
    a.ora(zp(z::T4));
    a.sta(abs(cat::OPTION));
    a.jmp("write_catalogue");
    a.bind(not_boot);
    a.cpx(imm(OPT_MESSAGES + 1));
    far(a, Asm::bcc, "err_bad_option");
    a.cpx(imm(OPT_MESSAGES));
    a.lda(imm(0xFF));
    let set = a.local();
    a.bcc(set);
    a.cpy(imm(0));
    a.beq(set);
    a.lda(imm(0));
    a.bind(set);
    a.sta(abs(page::MESSAGES));
    a.rts();
}

/// `run_file`: the file named at (&F2),Y loaded to its own address and
/// entered at its execution address, its line's tail left for OSARGS 1. A
/// name with no directory or drive is looked for in the library too; a file
/// in neither is a Bad command.
///
/// DFS 1.20 enters an address in the sideways window with itself paged
/// there, so a BASIC program, saved to be entered at &8023, does nothing
/// once loaded; this DFS returns rather than enter its own code.
fn running(a: &mut Asm) {
    a.here("run_file");
    a.jsr("parse_name");
    a.tya();
    a.clc();
    a.adc(zp(os::TEXT));
    a.sta(abs(page::TAIL));
    a.lda(zp(os::TEXT + 1));
    a.adc(imm(0));
    a.sta(abs(page::TAIL + 1));
    a.jsr("name_catalogue");
    a.jsr("find_first");
    let found = a.local();
    a.bcc(found);
    // Only a bare name tries the library.
    a.lda(abs(page::QUALIFIED));
    far(a, Asm::beq, "err_bad_command");
    a.lda(abs(page::LIB_DIR));
    a.sta(zp(z::DIR));
    a.lda(abs(page::LIB_DRIVE));
    a.sta(zp(z::DRIVE));
    a.jsr("name_catalogue");
    a.jsr("find_first");
    far(a, Asm::bcc, "err_bad_command");
    a.bind(found);
    a.jsr("entry_info");
    a.lda(abs(page::INFO_LOAD));
    a.sta(abs(page::OP_ADDRESS));
    a.lda(abs(page::INFO_LOAD + 1));
    a.sta(abs(page::OP_ADDRESS + 1));
    a.jsr("load_data");
    a.jsr("message");
    a.lda(abs(page::INFO_EXEC + 1));
    a.cmp(imm(hi(SIDEWAYS_BASE)));
    let enter = a.local();
    a.bcc(enter);
    a.cmp(imm(hi(OS_BASE)));
    a.bcc(RTS);
    a.bind(enter);
    a.jmp(abs(page::INFO_EXEC).ind());
}

/// *DIR, *LIB, *DRIVE, *ENABLE and *TITLE.
fn settings(a: &mut Asm) {
    // *DIR and *LIB: a directory, the current one if none is given, as
    // DFS 1.20 takes it.
    a.here("star_dir");
    a.ldx(imm(0));
    let set = a.local();
    a.beq(set);
    a.here("star_lib");
    a.ldx(imm(2));
    a.bind(set);
    a.stx(zp(z::T4));
    a.lda(abs(page::DIR));
    a.sta(zp(z::DIR));
    a.lda(abs(page::DRIVE));
    a.sta(zp(z::DRIVE));
    a.lda(zp(os::TEXT).ind_y());
    a.cmp(imm(CR));
    let default = a.local();
    a.beq(default);
    a.jsr("dir_argument");
    a.bind(default);
    a.ldx(zp(z::T4));
    a.lda(zp(z::DIR));
    a.sta(abs(page::DIR).x());
    a.lda(zp(z::DRIVE));
    a.sta(abs(page::DRIVE).x());
    a.rts();

    a.here("star_drive");
    a.jsr("drive_argument");
    a.sta(abs(page::DRIVE));
    a.rts();

    a.here("star_enable");
    a.lda(imm(1));
    a.sta(abs(page::ENABLE));
    a.rts();

    // *TITLE: up to twelve characters, padded with spaces.
    a.here("star_title");
    a.tya();
    a.pha();
    a.jsr("current_catalogue");
    a.pla();
    a.tay();
    a.clc();
    a.jsr(entry::GSINIT);
    a.ldx(imm(0));
    let next = a.here_local();
    a.jsr(entry::GSREAD);
    let padded = a.local();
    a.bcs(padded);
    a.jsr("title_char");
    a.cpx(imm(cat::TITLE_LEN));
    a.bne(next);
    let written = a.local();
    a.beq(written);
    a.bind(padded);
    let pad = a.here_local();
    a.lda(imm(b' '));
    a.jsr("title_char");
    a.cpx(imm(cat::TITLE_LEN));
    a.bne(pad);
    a.bind(written);
    a.jmp("write_catalogue");

    // Title character X set to A, X then one on.
    a.here("title_char");
    a.pha();
    a.lda(abs("title_places").x());
    a.sty(zp(z::T3));
    a.tay();
    a.pla();
    a.cpx(imm(cat::TITLE_FIRST));
    let second = a.local();
    a.bcs(second);
    a.sta(abs(cat::NAMES).y());
    let stored = a.local();
    a.bcc(stored);
    a.bind(second);
    a.sta(abs(cat::INFO).y());
    a.bind(stored);
    a.ldy(zp(z::T3));
    a.inx();
    a.rts();
}

/// *INFO, *ACCESS, *DELETE, *RENAME and *WIPE.
fn files(a: &mut Asm) {
    // A name and its catalogue, then the first entry it matches as a
    // pattern, or Not found.
    a.here("first_of_pattern");
    a.jsr("parse_name");
    a.jsr("name_catalogue");
    a.jsr("find_pattern");
    far(a, Asm::bcc, "err_not_found");
    a.rts();

    a.here("star_info");
    a.jsr("first_of_pattern");
    let each = a.here_local();
    a.jsr("print_info");
    a.jsr("find_next");
    a.bcc(each);
    a.rts();

    // *ACCESS: each match locked with L, unlocked with nothing.
    a.here("star_access");
    a.jsr("parse_name");
    a.ldx(imm(0));
    a.jsr("next_word");
    let unlock = a.local();
    a.beq(unlock);
    a.jsr("upper");
    a.cmp(imm(b'L'));
    far(a, Asm::beq, "err_bad_attribute");
    a.iny();
    a.jsr("next_word");
    far(a, Asm::beq, "err_bad_attribute");
    a.ldx(imm(cat::LOCKED));
    a.bind(unlock);
    a.stx(zp(z::T4));
    a.jsr("name_catalogue");
    a.jsr("find_pattern");
    far(a, Asm::bcc, "err_not_found");
    let each = a.here_local();
    a.jsr("check_closed");
    a.ldy(zp(z::ENTRY));
    a.lda(abs(cat::NAMES + u16::from(cat::ENTRY_DIR)).y());
    a.and(imm(!cat::LOCKED));
    a.ora(zp(z::T4));
    a.sta(abs(cat::NAMES + u16::from(cat::ENTRY_DIR)).y());
    a.jsr("message");
    a.jsr("find_next");
    a.bcc(each);
    a.jmp("write_catalogue");

    a.here("star_delete");
    a.jsr("parse_name");
    a.jsr("name_catalogue");
    a.jsr("find_first");
    far(a, Asm::bcc, "err_not_found");
    a.jsr("check_unlocked");
    a.jsr("check_closed");
    a.jsr("message");
    a.jsr("remove_entry");
    a.jmp("write_catalogue");

    renaming(a);
    wiping(a);
}

/// *RENAME.
fn renaming(a: &mut Asm) {
    // *RENAME: the new name on the same drive, not already taken.
    a.here("star_rename");
    a.jsr("parse_name");
    for k in 0..9 {
        a.lda(zp(z::NAME + k));
        a.sta(abs(page::NAME2 + k));
    }
    a.lda(zp(os::TEXT).ind_y());
    a.cmp(imm(CR));
    let second = a.local();
    a.bne(second);
    a.ldx(imm(index_of("RENAME")));
    a.jmp("syntax");
    a.bind(second);
    a.jsr("parse_name");
    a.lda(zp(z::DRIVE));
    a.cmp(abs(page::NAME2 + 8));
    far(a, Asm::beq, "err_bad_drive");
    a.jsr("name_catalogue");
    a.jsr("find_first");
    far(a, Asm::bcs, "err_exists");
    // The old name back, found, and given the new.
    for k in 0..9 {
        a.ldx(zp(z::NAME + k));
        a.lda(abs(page::NAME2 + k));
        a.sta(zp(z::NAME + k));
        a.txa();
        a.sta(abs(page::NAME2 + k));
    }
    a.jsr("find_first");
    far(a, Asm::bcc, "err_not_found");
    a.jsr("check_unlocked");
    a.jsr("check_closed");
    a.ldy(zp(z::ENTRY));
    for k in 0..u16::from(cat::NAME_LEN) {
        a.lda(abs(page::NAME2 + k));
        a.sta(abs(cat::NAMES + k).y());
    }
    a.lda(abs(page::NAME2 + u16::from(cat::NAME_LEN)));
    a.sta(abs(cat::NAMES + u16::from(cat::ENTRY_DIR)).y());
    a.jmp("write_catalogue");
}

/// *WIPE and the column it shows each name in.
fn wiping(a: &mut Asm) {
    // *WIPE: each unlocked match offered, and deleted on Y.
    a.here("star_wipe");
    a.jsr("first_of_pattern");
    let each = a.here_local();
    a.ldy(zp(z::ENTRY));
    a.lda(abs(cat::NAMES + u16::from(cat::ENTRY_DIR)).y());
    let next = a.local();
    a.bmi(next);
    a.jsr("check_closed");
    a.jsr("print_name_column");
    a.jsr("print_inline");
    a.text("   : ");
    a.byte(0);
    a.jsr("yes");
    a.php();
    a.jsr(entry::OSNEWL);
    a.plp();
    a.bne(next);
    a.jsr("remove_entry");
    a.jsr("write_catalogue");
    // The entry removed, the next one now sits where it was.
    a.lda(zp(z::ENTRY));
    a.sec();
    a.sbc(imm(cat::FIRST));
    a.sta(zp(z::ENTRY));
    a.bind(next);
    a.jsr("find_next");
    a.bcc(each);
    a.rts();

    // The entry's name in the twelve columns *INFO gives it before its
    // lock.
    a.here("print_name_column");
    a.ldy(zp(z::ENTRY));
    a.lda(abs(cat::NAMES + u16::from(cat::ENTRY_DIR)).y());
    a.and(imm(!cat::LOCKED));
    a.jsr(entry::OSASCI);
    a.lda(imm(b'.'));
    a.jsr(entry::OSASCI);
    a.jsr("print_entry_name");
    a.ldx(imm(2));
    a.jmp("print_spaces");
}

/// `confirm`, called from a command's own level: unless the command before
/// was *ENABLE, "Go (Y/N) ? " asked, any answer but Y ending the command.
/// `yes`: a key read and shown as Y or N, Z set for Y. `read_key`: a key
/// typed after the question, those typed before it discarded as DFS 1.20
/// does; Escape is an error.
fn confirm(a: &mut Asm) {
    a.here("confirm");
    a.lda(abs(page::ENABLE));
    a.beq(RTS);
    a.here("go");
    a.jsr("print_inline");
    a.text("Go (Y/N) ? ");
    a.byte(0);
    a.jsr("yes");
    a.php();
    a.jsr(entry::OSNEWL);
    a.plp();
    a.beq(RTS);
    // Out of the command: the caller's return discarded.
    a.pla();
    a.pla();
    a.rts();

    a.here("yes");
    a.jsr("read_key");
    a.jsr("upper");
    a.cmp(imm(b'Y'));
    let agreed = a.local();
    a.beq(agreed);
    a.lda(imm(b'N'));
    a.jsr(entry::OSASCI);
    a.lda(imm(1));
    a.rts();
    a.bind(agreed);
    a.jsr(entry::OSASCI);
    a.lda(imm(0));
    a.rts();

    a.here("read_key");
    a.lda(imm(OSBYTE_FLUSH_INPUT));
    a.ldx(imm(1));
    a.jsr(entry::OSBYTE);
    a.jsr(entry::OSRDCH);
    far(a, Asm::bcc, "escape");
    a.rts();
}
