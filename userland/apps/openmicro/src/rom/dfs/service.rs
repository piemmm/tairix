//! The sideways ROM's side: the service calls the DFS answers, its
//! selection as the filing system, the absolute workspace it shares with
//! other ROMs, the boot, *HELP and OSWORD &7D to &7F.
//!
//! The DFS starts on any BREAK with no key held, or D; with SHIFT it boots
//! from drive 0 by the catalogue's option, first resetting the current and
//! library directories to `:0.$`, as DFS 1.20 does. A soft BREAK keeps the
//! settings, a hard one restores their defaults; every BREAK forgets the
//! open files.

use super::commands::{DFS, UTILS};
use super::ws::{catalogue as cat, channel as ch, page, private, zp as z, ABSOLUTE_END};
use crate::machine::bbc::keyboard::Key;
use crate::rom::asm::{abs, byte, hi, imm, lo, sym, zp, Acc, Asm, RTS};
use crate::rom::os::{entry, fsc, osbyte, page_d, service, vector, zp as os, EXTENDED_ENTRIES};

/// No key down, as OSBYTE &7A reports it.
const NO_KEY: u8 = 0xFF;
/// OSBYTE &FD's soft BREAK.
const SOFT_BREAK: u8 = 0;
/// OSWORD numbers the DFS takes.
const OSWORD_CYCLE: u8 = 0x7D;
const OSWORD_SIZE: u8 = 0x7E;
const OSWORD_COMMAND: u8 = 0x7F;
/// The filing system number service call `SELECT_FILING_SYSTEM` names the
/// DFS by.
const DFS_NUMBER: u8 = crate::rom::os::filing_system::DFS;
/// What the DFS prints as it starts.
const BANNER: &str = "TAIRiX DFS\r\r";
/// The version *HELP shows, of the interface implemented.
const HELP_TITLE: &str = "\rDFS 1.20\r";
/// The routines behind the seven filing system vectors, FILEV to FSCV.
const ROUTINES: [&str; 7] = [
    "dfs_file", "dfs_args", "dfs_bget", "dfs_bput", "dfs_gbpb", "dfs_find", "dfs_fsc",
];
/// The vector number of FILEV, the first of them.
const FIRST_VECTOR: u8 = ((vector::FILE - vector::USER) / 2).to_le_bytes()[0];
/// The boot commands, by option 1 to 3.
const BOOTS: [&str; 3] = ["LOAD !BOOT\r", "RUN !BOOT\r", "EXEC !BOOT\r"];

pub(super) fn emit(a: &mut Asm) {
    dispatch(a);
    workspace(a);
    selection(a);
    boot(a);
    help(a);
    calls(a);
}

/// The service entry: each call the DFS answers, A its number, returned
/// zero once claimed.
fn dispatch(a: &mut Asm) {
    a.here("service");
    for (number, handler) in [
        (service::ABSOLUTE_WORKSPACE, "service_absolute"),
        (service::PRIVATE_WORKSPACE, "service_private"),
        (service::AUTO_BOOT, "service_boot"),
        (service::COMMAND, "service_command"),
        (service::OSWORD, "service_osword"),
        (service::HELP, "service_help"),
        (service::CLAIM_WORKSPACE, "service_lose"),
        (service::SELECT_FILING_SYSTEM, "service_select"),
    ] {
        a.cmp(imm(number));
        let other = a.local();
        a.bne(other);
        a.jmp(handler);
        a.bind(other);
    }
    a.rts();
}

/// The workspace: the absolute pages to &16FF, two private pages, and the
/// absolute workspace claimed back from another ROM when the DFS needs it.
fn workspace(a: &mut Asm) {
    a.here("service_absolute");
    a.cpy(imm(ABSOLUTE_END));
    a.bcs(RTS);
    a.ldy(imm(ABSOLUTE_END));
    a.rts();

    a.here("service_private");
    a.ldx(zp(os::ROM));
    a.tya();
    a.sta(abs(page_d::PRIVATE_WORKSPACE).x());
    a.clc();
    a.adc(imm(private::PAGES));
    a.tay();
    a.lda(imm(service::PRIVATE_WORKSPACE));
    a.rts();

    // `claim_workspace`, every register kept: the absolute workspace this
    // ROM's again, the other ROMs told first so they can keep what they
    // hold there; the settings come back from the private page if they
    // were put there when it was given up.
    a.here("claim_workspace");
    a.php();
    a.pha();
    a.txa();
    a.pha();
    a.tya();
    a.pha();
    a.ldx(zp(os::ROM));
    a.lda(abs(page_d::PRIVATE_WORKSPACE).x());
    let owned = a.local();
    a.bmi(owned);
    a.lda(imm(osbyte::SERVICE));
    a.ldx(imm(service::CLAIM_WORKSPACE));
    a.ldy(imm(0xFF));
    a.jsr(entry::OSBYTE);
    a.ldx(zp(os::ROM));
    a.lda(abs(page_d::PRIVATE_WORKSPACE).x());
    a.ora(imm(private::OWNED));
    a.sta(abs(page_d::PRIVATE_WORKSPACE).x());
    a.jsr("private_pointer");
    a.ldy(imm(private::SAVED));
    a.lda(zp(z::POINTER).ind_y());
    a.beq(owned);
    a.lda(imm(0));
    a.sta(zp(z::POINTER).ind_y());
    a.ldy(imm(page::SETTINGS_LEN));
    let back = a.here_local();
    a.lda(zp(z::POINTER).ind_y());
    a.sta(abs(page::SETTINGS - 1).y());
    a.dey();
    a.bne(back);
    a.bind(owned);
    a.pla();
    a.tay();
    a.pla();
    a.tax();
    a.pla();
    a.plp();
    a.rts();

    // Another ROM claims the absolute workspace: the open files written
    // back and closed, the settings kept in the private page.
    a.here("service_lose");
    a.ldx(zp(os::ROM));
    a.lda(abs(page_d::PRIVATE_WORKSPACE).x());
    let kept = a.local();
    a.bpl(kept);
    a.tya();
    a.pha();
    a.jsr("close_all");
    a.jsr("private_pointer");
    a.ldy(imm(page::SETTINGS_LEN));
    let save = a.here_local();
    a.lda(abs(page::SETTINGS - 1).y());
    a.sta(zp(z::POINTER).ind_y());
    a.dey();
    a.bne(save);
    a.lda(imm(0xFF));
    a.sta(zp(z::POINTER).ind_y());
    a.ldx(zp(os::ROM));
    a.lda(abs(page_d::PRIVATE_WORKSPACE).x());
    a.and(imm(!private::OWNED));
    a.sta(abs(page_d::PRIVATE_WORKSPACE).x());
    a.pla();
    a.tay();
    a.bind(kept);
    a.lda(imm(service::CLAIM_WORKSPACE));
    a.rts();

    // POINTER at the first private page.
    a.here("private_pointer");
    a.ldx(zp(os::ROM));
    a.lda(abs(page_d::PRIVATE_WORKSPACE).x());
    a.and(imm(!private::OWNED));
    a.sta(zp(z::POINTER + 1));
    a.lda(imm(0));
    a.sta(zp(z::POINTER));
    a.rts();
}

/// `select_dfs`: the old filing system shut down, the DFS's workspace
/// claimed, its routines on the seven vectors through the extended vectors,
/// the other ROMs told, and the 8271 programmed.
fn selection(a: &mut Asm) {
    a.here("select_dfs");
    a.lda(imm(fsc::SHUT_DOWN));
    a.jsr("fsc_old");
    a.jsr("claim_workspace");
    a.ldx(imm(0));
    let vector_n = a.here_local();
    // The extended vector: the routine and this ROM.
    a.txa();
    a.sta(zp(z::T0));
    a.asl(Acc);
    a.adc(zp(z::T0));
    a.tay();
    a.lda(abs("vector_low").x());
    a.sta(abs(extended(0)).y());
    a.lda(abs("vector_high").x());
    a.sta(abs(extended(1)).y());
    a.lda(zp(os::ROM));
    a.sta(abs(extended(2)).y());
    // The vector: the extended entry point for its number.
    a.tya();
    a.clc();
    a.adc(imm(lo(EXTENDED_ENTRIES + 3 * u16::from(FIRST_VECTOR))));
    a.pha();
    a.txa();
    a.asl(Acc);
    a.tay();
    a.pla();
    a.sta(abs(vector::FILE).y());
    a.lda(imm(hi(EXTENDED_ENTRIES)));
    a.sta(abs(vector::FILE + 1).y());
    a.inx();
    a.cpx(imm(byte(ROUTINES.len())));
    a.bne(vector_n);
    a.lda(imm(osbyte::SERVICE));
    a.ldx(imm(service::VECTORS_CLAIMED));
    a.ldy(imm(0xFF));
    a.jsr(entry::OSBYTE);
    a.jmp("fdc_initialise");

    a.here("fsc_old");
    a.jmp(abs(vector::FSC).ind());

    a.here("vector_low");
    for routine in ROUTINES {
        a.byte(lo(sym(routine)));
    }
    a.here("vector_high");
    for routine in ROUTINES {
        a.byte(hi(sym(routine)));
    }

    a.here("service_select");
    a.cpy(imm(DFS_NUMBER));
    a.bne(RTS);
    a.jsr("select_dfs");
    a.lda(imm(0));
    a.rts();
}

/// The extended vector table's byte `k` of FILEV's entry.
const fn extended(k: u16) -> u16 {
    page_d::EXTENDED_VECTORS + 3 * FIRST_VECTOR as u16 + k
}

/// The boot: Y zero to boot from drive 0.
fn boot(a: &mut Asm) {
    a.here("service_boot");
    a.tya();
    a.pha();
    a.lda(imm(osbyte::SCAN_FROM_16));
    a.jsr(entry::OSBYTE);
    a.cpx(imm(NO_KEY));
    let start = a.local();
    a.beq(start);
    a.cpx(imm(Key::D.number()));
    a.beq(start);
    a.pla();
    a.tay();
    a.lda(imm(service::AUTO_BOOT));
    a.rts();
    a.bind(start);
    // The open files are forgotten, and nothing is written back.
    a.lda(imm(0));
    a.sta(abs(page::OPEN_COUNT));
    for channel in 1..=(ch::LAST_HANDLE - ch::FIRST_HANDLE + 1) {
        a.sta(abs(ch::CHANNELS + u16::from(channel * ch::SIZE + ch::FLAGS)));
    }
    a.jsr("claim_workspace");
    a.lda(imm(osbyte::LAST_BREAK));
    a.ldx(imm(0));
    a.ldy(imm(0xFF));
    a.jsr(entry::OSBYTE);
    a.cpx(imm(SOFT_BREAK));
    let kept = a.local();
    a.beq(kept);
    a.ldx(imm(page::SETTINGS_LEN - 1));
    let default = a.here_local();
    a.lda(abs("default_settings").x());
    a.sta(abs(page::SETTINGS).x());
    a.dex();
    a.bpl(default);
    a.bind(kept);
    a.lda(imm(0xFF));
    a.sta(abs(page::ENABLE));
    a.pla();
    a.pha();
    let no_reset = a.local();
    a.bne(no_reset);
    for (at, value) in [
        (page::DIR, b'$'),
        (page::DRIVE, 0),
        (page::LIB_DIR, b'$'),
        (page::LIB_DRIVE, 0),
    ] {
        a.lda(imm(value));
        a.sta(abs(at));
    }
    a.bind(no_reset);
    a.jsr("select_dfs");
    a.call_with_text("print_inline", &[BANNER]);
    a.pla();
    let booted = a.local();
    a.bne(booted);
    a.jsr("boot_file");
    a.bind(booted);
    a.lda(imm(0));
    a.rts();

    // The settings a hard BREAK restores, MESSAGES to LIB_DRIVE.
    a.here("default_settings");
    a.bytes(&[0xFF, 0xFF, 0, b'$', 0, b'$', 0]);

    // The boot option's command on drive 0's !BOOT, run from RAM since the
    // MOS reads it as other ROMs are offered it.
    a.here("boot_file");
    a.lda(imm(0));
    a.sta(abs(page::OP_DRIVE));
    a.jsr("read_catalogue");
    a.lda(abs(cat::OPTION));
    for _ in 0..4 {
        a.lsr(Acc);
    }
    a.and(imm(3));
    a.beq(RTS);
    a.tax();
    a.ldy(abs(sym("boot_offsets") + -1).x());
    a.ldx(imm(0));
    let copy = a.here_local();
    a.lda(abs("boot_text").y());
    a.sta(abs(page::WORK).x());
    a.inx();
    a.iny();
    a.cmp(imm(0x0D));
    a.bne(copy);
    a.ldx(imm(lo(page::WORK)));
    a.ldy(imm(hi(page::WORK)));
    a.jmp(abs(entry::OSCLI));

    a.here("boot_offsets");
    let mut at = 0;
    for command in BOOTS {
        a.byte(byte(at));
        at += command.len();
    }
    a.here("boot_text");
    for command in BOOTS {
        a.text(command);
    }
}

/// *HELP: with nothing after it, the ROM's name and its two keywords; each
/// keyword given lists its commands and their syntax.
fn help(a: &mut Asm) {
    a.here("service_help");
    a.tya();
    a.pha();
    a.jsr("next_word");
    let words = a.local();
    a.bne(words);
    a.call_with_text("print_inline", &[HELP_TITLE, "  DFS\r  UTILS\r"]);
    let out = a.local();
    a.jmp(out);
    a.bind(words);
    let word = a.here_local();
    a.jsr("next_word");
    a.beq(out);
    a.ldx(imm(super::commands::HELP_TABLE));
    a.jsr("match_command");
    let known = a.local();
    a.bcc(known);
    // Not ours: past the word.
    let skip = a.here_local();
    a.lda(zp(os::TEXT).ind_y());
    a.cmp(imm(b' '));
    a.beq(word);
    a.cmp(imm(0x0D));
    a.beq(out);
    a.iny();
    a.bne(skip);
    a.bind(known);
    a.sty(zp(z::T6));
    a.call_with_text("print_inline", &[HELP_TITLE]);
    // X is the keyword's index: the first and last command it lists.
    a.lda(abs(sym("help_first") + -i32::from(super::commands::HELP_FIRST_INDEX)).x());
    a.sta(zp(z::T4));
    a.lda(abs(sym("help_last") + -i32::from(super::commands::HELP_FIRST_INDEX)).x());
    a.sta(zp(z::T5));
    let line = a.here_local();
    a.ldx(zp(z::T4));
    a.jsr("help_line");
    a.inc(zp(z::T4));
    a.lda(zp(z::T4));
    a.cmp(zp(z::T5));
    a.bne(line);
    a.ldy(zp(z::T6));
    a.jmp(word);
    a.bind(out);
    a.pla();
    a.tay();
    a.lda(imm(service::HELP));
    a.rts();

    a.here("help_first");
    a.bytes(&[0, byte(DFS.len())]);
    a.here("help_last");
    a.bytes(&[byte(DFS.len()), byte(DFS.len() + UTILS.len())]);

    // Command X's line: two spaces, its name and its syntax.
    a.here("help_line");
    a.stx(zp(z::T3));
    a.ldx(imm(2));
    a.jsr("print_spaces");
    a.ldx(zp(z::T3));
    a.ldy(abs("name_offsets").x());
    let name = a.here_local();
    a.lda(abs("command_names").y());
    let spelt = a.local();
    a.bmi(spelt);
    a.jsr(entry::OSASCI);
    a.iny();
    a.bne(name);
    a.bind(spelt);
    a.ldx(zp(z::T3));
    a.ldy(abs("syntax_offsets").x());
    a.lda(abs("syntax_text").y());
    let ended = a.local();
    a.beq(ended);
    a.jsr("print_space");
    let text = a.here_local();
    a.lda(abs("syntax_text").y());
    a.beq(ended);
    a.jsr(entry::OSASCI);
    a.iny();
    a.bne(text);
    a.bind(ended);
    a.jmp(abs(entry::OSNEWL));
}

/// Service calls 4 and 8: the utilities, then OSWORD &7D to &7F.
fn calls(a: &mut Asm) {
    a.here("service_command");
    a.ldx(imm(super::commands::UTIL_TABLE));
    a.jsr("match_command");
    let ours = a.local();
    a.bcc(ours);
    a.lda(imm(service::COMMAND));
    a.rts();
    a.bind(ours);
    a.jsr("run_command");
    a.lda(imm(0));
    a.rts();

    a.here("service_osword");
    a.lda(zp(os::CALL_A));
    a.cmp(imm(OSWORD_CYCLE));
    let not_ours = a.local();
    a.bcc(not_ours);
    a.cmp(imm(OSWORD_COMMAND + 1));
    a.bcs(not_ours);
    a.jsr("claim_workspace");
    a.lda(zp(os::CALL_X));
    a.sta(zp(z::BLOCK));
    a.lda(zp(os::CALL_Y));
    a.sta(zp(z::BLOCK + 1));
    a.lda(zp(os::CALL_A));
    a.cmp(imm(OSWORD_CYCLE));
    let not_cycle = a.local();
    a.bne(not_cycle);
    a.jsr("osword_cycle");
    let claimed = a.local();
    a.jmp(claimed);
    a.bind(not_cycle);
    a.cmp(imm(OSWORD_SIZE));
    let not_size = a.local();
    a.bne(not_size);
    a.jsr("osword_size");
    a.jmp(claimed);
    a.bind(not_size);
    a.jsr("osword_command");
    a.bind(claimed);
    a.lda(imm(0));
    a.rts();
    a.bind(not_ours);
    a.lda(imm(service::OSWORD));
    a.rts();
}
