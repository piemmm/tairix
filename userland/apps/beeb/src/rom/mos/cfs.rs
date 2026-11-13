//! The cassette and ROM filing systems: OSFILE, OSFIND, OSBGET, OSBPUT,
//! OSARGS and FSCV over blocks in the Acorn tape format, read from the
//! cassette through the 6850 or from sideways ROMs through service calls 13
//! and 14, and written to the cassette.
//!
//! A block is the sync byte &2A, a name of up to ten characters and a zero,
//! the load and execution addresses, the block number, the data's length, a
//! flag and four spare bytes, the header's CRC, then the data and its CRC;
//! the CRCs are CRC-16/XMODEM, high byte first. A ROM's data may also hold
//! &23, a block whose header is the last one's with the next number, and
//! ends at &2B. Tones are timed in VSYNCs: 5.1 s of leader before a file,
//! 0.9 s between its blocks, 5.3 s after it; a sequentially written block
//! stands alone, 1.1 s of leader before it and 0.2 s after.
//!
//! *OPT keeps two option nibbles in &E3, LOAD and SAVE's high and
//! sequential access's low: bit 3 messages, bit 2 long ones, bits 1–0 the
//! response to an error, 0 to ignore it, 2 to report it and look for the
//! block again, 1 to raise it.

use super::ws::{buffer, cfs, fs, os, var, vector};
use crate::machine::acia::{control, status};
use crate::machine::serial;
use crate::machine::sheila;
use crate::rom::asm::{abs, hi, imm, lo, sym, zp, Acc, Asm, RTS};
use crate::rom::os::{filing_system, fsc, service};

/// &E2's bits.
mod state {
    pub const INPUT: u8 = 0x01;
    pub const OUTPUT: u8 = 0x02;
    pub const CATALOGUE: u8 = 0x08;
    pub const EOF_WARNED: u8 = 0x80;
}

/// An option nibble's fields.
mod option {
    pub const MESSAGES: u8 = 0x08;
    pub const LONG: u8 = 0x04;
    pub const RESPONSE: u8 = 0x03;
    pub const ABORT: u8 = 0x01;
    /// Both nibbles as every reset and *OPT 0 leave them: LOAD and SAVE
    /// with short messages, reporting errors and looking again; sequential
    /// access quiet, raising them.
    pub const DEFAULT: u8 = 0xA1;
}

mod flag {
    pub const LAST: u8 = 0x80;
    pub const LOCKED: u8 = 0x01;
}

/// A header's fields, from its start at &3B2 or, for the file being
/// written, &380.
mod header {
    pub const LOAD: u16 = 12;
    pub const EXEC: u16 = 16;
    pub const BLOCK: u16 = 20;
    pub const LENGTH: u16 = 22;
    pub const FLAG: u16 = 24;
    pub const SPARE: u16 = 25;
    pub const CRC: u16 = 29;
    /// The bytes after the name's zero the header CRC covers.
    pub const FIELDS: u8 = 17;
    pub const NAME_LENGTH: u8 = 10;
}

const SYNC: u8 = 0x2A;
const CONTINUATION: u8 = 0x23;
const END_OF_ROM: u8 = 0x2B;
const DUMMY: u8 = 0xAA;

/// Tones, in VSYNCs.
const LEADER: u8 = 255;
// Loading it branches as nonzero.
const _: () = assert!(LEADER != 0);
const BETWEEN_BLOCKS: u8 = 45;
const TRAILER: [u8; 2] = [255, 9];
const PAUSED_LEADER: u8 = 55;
const PAUSED_TRAILER: u8 = 10;
/// *OPT 3's gap as every reset leaves it.
const DEFAULT_GAP: u8 = 25;

/// The serial ULA giving the cassette the 6850 with its motor running, at
/// the transmit rate the 6850 divides to 1200 or 300 baud.
const ULA_CASSETTE: u8 = serial::MOTOR | 0x05;
/// The 6850's control, with &C6's speed: reading with RTS high and no
/// interrupts, writing with RTS low.
const READING: u8 = 0x50;
const WRITING: u8 = 0x10;
const SPEED_1200: u8 = 0x05;
const SPEED_300: u8 = 0x06;
const _: () = assert!(SPEED_300 > SPEED_1200, "OSARGS tells them apart by order");

/// FSCV's routine for each reason below the command line's, which the
/// cassette ignores: *OPT, EOF, */, an unknown command, *RUN, *CAT, shutting
/// down, and the handles used.
const FSC_REASONS: u8 = fsc::COMMAND_LINE;
const FSC_ROUTINES: [&str; FSC_REASONS as usize] = [
    "cfs_opt",
    "cfs_eof",
    "cfs_run",
    "bad_command",
    "cfs_run",
    "cfs_catalogue",
    "cfs_shut_down",
    "cfs_handles",
];

/// &247 for the ROM filing system; zero is the cassette.
const RFS: u8 = 2;
/// The ROMs the filing system reads, as &F5 counts them.
const ROMS: u8 = 16;

const INPUT_HANDLE: u8 = 1;
const OUTPUT_HANDLE: u8 = 2;

/// The filing system's zero page.
mod z {
    use super::fs;

    const fn at(n: u16) -> u16 {
        fs::SCRATCH + n
    }

    /// The address a block's data goes to or comes from, four bytes.
    pub const ADDRESS: u16 = at(0);
    /// The block wanted next.
    pub const EXPECT: u16 = at(4);
    /// Bytes of a block left to move.
    pub const COUNT: u16 = at(6);
    /// The data's place while a block moves.
    pub const POINTER: u16 = at(8);
    /// What the operation does with blocks.
    pub const MODE: u16 = at(10);
    /// The option nibble in force.
    pub const OPTIONS: u16 = at(11);
    /// The last byte written.
    pub const LAST: u16 = at(12);
    /// The error being dealt with.
    pub const ERROR: u16 = at(13);
    pub const CRC_HIGH: u16 = at(14);
    pub const CRC_LOW: u16 = at(15);
    /// The header being shown.
    pub const SHOWN: u16 = at(16);
    /// X and Y of a sequential call.
    pub const CALLER: u16 = at(18);
    /// A name character being compared.
    pub const COMPARE: u16 = at(20);
    /// The stack where *CAT began, for the ROMs' end.
    pub const STACK: u16 = at(21);
    /// The OSFILE block.
    pub const BLOCK: u16 = at(24);
    /// X and Y of an OSFIND call, kept apart from a sequential call's, which
    /// closing a file may make.
    pub const FIND_CALLER: u16 = at(26);
    /// The length loaded, or the end of the data saved, four bytes.
    pub const END: u16 = at(28);
}

/// MODE's bits.
mod mode {
    /// Into memory at the address the caller gave rather than the file's.
    pub const GIVEN_ADDRESS: u8 = 0x01;
    /// The file is being run, so a locked one may load.
    pub const RUN: u8 = 0x20;
}

mod error {
    pub const LOCKED: u8 = 0xD5;
    pub const NOT_FOUND: u8 = 0xD6;
    pub const DATA: u8 = 0xD8;
    pub const HEADER: u8 = 0xD9;
    pub const BLOCK: u8 = 0xDA;
    pub const FILE: u8 = 0xDB;
    pub const CHANNEL: u8 = 0xDE;
    pub const EOF: u8 = 0xDF;
    pub const ESCAPE: u8 = 0x11;
    /// The block errors, from Data? on, each raised by its own BRK.
    pub const BLOCK_ERRORS: [(&str, u8, &str); 4] = [
        ("cfs_error_data", DATA, "Data?"),
        ("cfs_error_header", HEADER, "Header?"),
        ("cfs_error_block", BLOCK, "Block?"),
        ("cfs_error_file", FILE, "File?"),
    ];
}

/// The sequential buffers, and the bytes left in the input one.
const BGET_BUFFER: u16 = buffer::INPUT;
const BPUT_BUFFER: u16 = buffer::RS423_OUTPUT;
const BGET_LEFT: u16 = cfs::BGET_OFFSET + 1;

pub fn emit(a: &mut Asm) {
    vectors(a);
    file(a);
    sequential(a);
    block_word(a);
    names(a);
    blocks(a);
    transfers(a);
    devices(a);
    messages(a);
    errors(a);
}

/// FILEV, ARGSV, FSCV, *OPT, the motor, *TAPE and *ROM.
fn vectors(a: &mut Asm) {
    // Every reset: the cassette at 1200 baud, the default options and gap,
    // no files open.
    a.here("cfs_init");
    a.lda(imm(SPEED_1200));
    a.sta(zp(fs::BAUD));
    a.lda(imm(option::DEFAULT));
    a.sta(zp(os::CFS_OPTIONS));
    a.lda(imm(DEFAULT_GAP));
    a.sta(abs(cfs::GAP));
    a.lda(imm(0));
    a.sta(zp(os::CFS_STATUS));
    a.rts();

    // OSFILE: A = &FF loads, 0 saves.
    a.here("cfs_file");
    a.stx(zp(z::BLOCK));
    a.sty(zp(z::BLOCK + 1));
    a.cmp(imm(0xFF));
    a.bne("cfs_file_not_load");
    a.jmp("cfs_load_file");
    a.here("cfs_file_not_load");
    a.cmp(imm(0));
    a.bne(RTS);
    a.jmp("cfs_save_file");

    // OSARGS: with A and Y zero, the filing system's number; X and Y kept.
    a.here("cfs_args");
    a.ora(imm(0));
    a.bne(RTS);
    a.cpy(imm(0));
    a.bne(RTS);
    a.lda(abs(var::CFS_OR_RFS));
    a.bne("cfs_args_rfs");
    a.lda(zp(fs::BAUD));
    // C set at 300 baud, the higher of the two settings.
    a.cmp(imm(SPEED_300));
    a.lda(imm(filing_system::CFS_1200));
    a.bcc(RTS);
    a.lda(imm(filing_system::CFS_300));
    a.rts();
    a.here("cfs_args_rfs");
    a.lda(imm(filing_system::RFS));
    a.rts();

    a.here("cfs_fsc");
    a.cmp(imm(FSC_REASONS));
    a.bcs(RTS);
    a.stx(zp(z::COMPARE));
    a.tax();
    a.lda(abs("cfs_fsc_high").x());
    a.pha();
    a.lda(abs("cfs_fsc_low").x());
    a.pha();
    a.ldx(zp(z::COMPARE));
    a.rts();
    a.here("cfs_fsc_low");
    for routine in FSC_ROUTINES {
        a.byte(lo(sym(routine) + -1));
    }
    a.here("cfs_fsc_high");
    for routine in FSC_ROUTINES {
        a.byte(hi(sym(routine) + -1));
    }
    a.here("cfs_handles");
    a.ldx(imm(INPUT_HANDLE));
    a.ldy(imm(OUTPUT_HANDLE));
    a.rts();
    // Another filing system takes over: the files are forgotten.
    a.here("cfs_shut_down");
    a.lda(imm(0));
    a.sta(zp(os::CFS_STATUS));
    a.rts();

    // OSBYTE &89: the motor relay, X non-zero to close it.
    a.here("cassette_motor");
    a.lda(abs(var::SERIAL_ULA));
    a.and(imm(!serial::MOTOR));
    a.cpx(imm(0));
    a.beq("cassette_motor_set");
    a.ora(imm(serial::MOTOR));
    a.here("cassette_motor_set");
    a.sta(abs(var::SERIAL_ULA));
    a.sta(abs(sheila::SERIAL_ULA));
    a.rts();
    select_tape(a);
}

/// OSBYTE &8C: the cassette, at 300 baud for X = 3, otherwise 1200.
fn select_tape(a: &mut Asm) {
    a.here("select_tape");
    a.lda(imm(SPEED_1200));
    a.cpx(imm(3));
    a.bne("select_tape_speed");
    a.lda(imm(SPEED_300));
    a.here("select_tape_speed");
    a.sta(zp(fs::BAUD));
    a.lda(imm(0));
    a.beq("select_filing_system");
    // OSBYTE &8D: the ROMs.
    a.here("select_rom");
    a.lda(imm(RFS));
    // The old filing system shuts down and the vectors become these.
    a.here("select_filing_system");
    a.pha();
    a.lda(imm(fsc::SHUT_DOWN));
    a.jsr("fsc_call");
    // The filing system's vectors, FILEV to FSCV.
    a.ldx(imm(lo(vector::FILE - vector::USER)));
    a.here("select_vectors");
    a.lda(abs("vector_defaults").x());
    a.sta(abs(vector::USER).x());
    a.inx();
    a.cpx(imm(lo(vector::FSC + 2 - vector::USER)));
    a.bne("select_vectors");
    a.pla();
    a.sta(abs(var::CFS_OR_RFS));
    a.lda(imm(0));
    a.sta(zp(os::CFS_STATUS));
    a.lda(imm(service::VECTORS_CLAIMED));
    a.jmp("service_call");

    // FSCV 0: *OPT X,Y. X zero restores both nibbles; 1 sets the messages,
    // 2 the response to errors, in both; 3 the gap.
    a.here("cfs_opt");
    a.txa();
    a.bne("cfs_opt_some");
    a.lda(imm(option::DEFAULT));
    a.sta(zp(os::CFS_OPTIONS));
    a.rts();
    a.here("cfs_opt_some");
    a.cpx(imm(3));
    a.bne("cfs_opt_field");
    a.sty(abs(cfs::GAP));
    a.rts();
    a.here("cfs_opt_field");
    a.cpx(imm(3));
    a.bcs("cfs_opt_bad");
    a.cpy(imm(3));
    a.bcs("cfs_opt_bad");
    a.lda(zp(os::CFS_OPTIONS));
    a.and(abs("cfs_opt_keep").x());
    a.sta(zp(os::CFS_OPTIONS));
    a.txa();
    a.asl(Acc);
    a.asl(Acc);
    a.sta(zp(z::COMPARE));
    a.tya();
    a.ora(zp(z::COMPARE));
    a.tay();
    a.lda(abs("cfs_opt_values").y());
    a.ora(zp(os::CFS_OPTIONS));
    a.sta(zp(os::CFS_OPTIONS));
    a.rts();
    a.here("cfs_opt_bad");
    a.brk();
    a.byte(0xCB);
    a.text("Bad option");
    a.byte(0);
    // What *OPT 1 and 2 leave of &E3, and the bits each Y sets.
    a.here("cfs_opt_keep");
    a.bytes(&[0xFF, 0x33, 0xCC]);
    a.here("cfs_opt_values");
    a.bytes(&[0, 0, 0, 0, 0x00, 0x88, 0xCC, 0, 0x00, 0x22, 0x11]);
}

/// LOAD, SAVE, *RUN and *CAT.
fn file(a: &mut Asm) {
    // OSFILE &FF: the file named in the block, at its own address unless
    // the block's execution address byte is zero; the block then describes
    // the file.
    a.here("cfs_load_file");
    a.lda(imm(0));
    a.sta(zp(z::MODE));
    a.ldy(imm(6));
    a.lda(zp(z::BLOCK).ind_y());
    a.bne("cfs_load_file_own");
    a.ldy(imm(2));
    a.ldx(imm(lo(z::ADDRESS)));
    a.jsr("cfs_block_word");
    a.lda(imm(mode::GIVEN_ADDRESS));
    a.sta(zp(z::MODE));
    a.here("cfs_load_file_own");
    a.jsr("cfs_block_name");
    a.jsr("cfs_load");
    a.ldy(imm(2));
    a.ldx(imm(0));
    a.here("cfs_load_file_report");
    a.lda(abs(cfs::HEADER + header::LOAD).x());
    a.sta(zp(z::BLOCK).ind_y());
    a.iny();
    a.inx();
    a.cpx(imm(8));
    a.bne("cfs_load_file_report");
    for offset in 0..2u16 {
        a.lda(zp(z::END + offset));
        a.sta(zp(z::BLOCK).ind_y());
        a.iny();
    }
    a.lda(imm(0));
    a.here("cfs_load_file_clear");
    a.sta(zp(z::BLOCK).ind_y());
    a.iny();
    a.cpy(imm(18));
    a.bne("cfs_load_file_clear");
    a.lda(imm(1));
    a.rts();

    // FSCV 2 and 4: the file at XY, loaded at its own address and run.
    a.here("cfs_run");
    a.jsr("cfs_name_at_xy");
    a.lda(imm(mode::RUN));
    a.sta(zp(z::MODE));
    a.jsr("cfs_load");
    a.jmp(abs(cfs::HEADER + header::EXEC).ind());
    cfs_load(a);
    cfs_save_file(a);
    cfs_catalogue(a);
}

/// The file named at &3D2, every block in turn, into memory; END left
/// holding its length.
fn cfs_load(a: &mut Asm) {
    a.here("cfs_load");
    a.jsr("cfs_options_file");
    a.lda(imm(0));
    for slot in [z::EXPECT, z::EXPECT + 1, z::END, z::END + 1] {
        a.sta(zp(slot));
    }
    a.jsr("cfs_searching");
    a.jsr("cfs_read_on");
    a.here("cfs_load_next");
    a.jsr("cfs_find_block");
    a.bcs("cfs_load_next");
    a.lda(zp(z::EXPECT));
    a.ora(zp(z::EXPECT + 1));
    a.bne("cfs_load_data");
    // The first block: a locked file loads only to be run, and the data
    // goes to the file's own address unless the caller gave one.
    a.lda(abs(cfs::HEADER + header::FLAG));
    a.and(imm(flag::LOCKED));
    a.beq("cfs_load_unlocked");
    a.lda(zp(z::MODE));
    a.and(imm(mode::RUN));
    a.bne("cfs_load_unlocked");
    a.jsr("cfs_read_off");
    a.brk();
    a.byte(error::LOCKED);
    a.text("Locked");
    a.byte(0);
    a.here("cfs_load_unlocked");
    a.jsr("cfs_loading");
    a.lda(zp(z::MODE));
    a.and(imm(mode::GIVEN_ADDRESS));
    a.bne("cfs_load_data");
    a.ldx(imm(3));
    let next = a.here_local();
    a.lda(abs(cfs::HEADER + header::LOAD).x());
    a.sta(zp(z::ADDRESS).x());
    a.dex();
    a.bpl(next);
    a.here("cfs_load_data");
    a.jsr("cfs_read_data");
    a.bcs("cfs_load_next");
    a.clc();
    for offset in 0..2u16 {
        a.lda(zp(z::END + offset));
        a.adc(abs(cfs::HEADER + header::LENGTH + offset));
        a.sta(zp(z::END + offset));
    }
    a.jsr("cfs_expect_next");
    a.jsr("cfs_show_read");
    a.lda(abs(cfs::HEADER + header::FLAG));
    a.bpl("cfs_load_next");
    a.jmp("cfs_finished");
}

/// OSFILE 0: the memory from the block's start to its end, as the file
/// it names.
fn cfs_save_file(a: &mut Asm) {
    a.here("cfs_save_file");
    a.jsr("cfs_block_name");
    a.jsr("cfs_output_name");
    a.ldy(imm(2));
    a.ldx(imm(0));
    a.here("cfs_save_file_addresses");
    a.lda(zp(z::BLOCK).ind_y());
    a.sta(abs(cfs::BPUT_HEADER + header::LOAD).x());
    a.iny();
    a.inx();
    a.cpx(imm(8));
    a.bne("cfs_save_file_addresses");
    for slot in [z::ADDRESS, z::END] {
        a.ldx(imm(lo(slot)));
        a.jsr("cfs_block_word");
    }
    a.jsr("cfs_options_file");
    a.jsr("cfs_record");
    a.jsr("cfs_leader");
    a.here("cfs_save_block");
    // The block's length: what is left, a full block at most; a full block
    // ending the data is the last.
    a.ldx(imm(flag::LAST));
    a.lda(zp(z::END));
    a.sec();
    a.sbc(zp(z::ADDRESS));
    a.sta(zp(z::COUNT));
    a.lda(zp(z::END + 1));
    a.sbc(zp(z::ADDRESS + 1));
    a.sta(zp(z::COUNT + 1));
    a.bcc("cfs_save_nothing");
    a.cmp(imm(1));
    a.bcc("cfs_save_length");
    a.bne("cfs_save_full");
    a.lda(zp(z::COUNT));
    a.beq("cfs_save_length");
    a.here("cfs_save_full");
    a.ldx(imm(0));
    a.lda(imm(0));
    a.sta(zp(z::COUNT));
    a.lda(imm(1));
    a.sta(zp(z::COUNT + 1));
    a.bne("cfs_save_length");
    a.here("cfs_save_nothing");
    a.lda(imm(0));
    a.sta(zp(z::COUNT));
    a.sta(zp(z::COUNT + 1));
    a.here("cfs_save_length");
    a.stx(abs(cfs::BPUT_HEADER + header::FLAG));
    a.lda(zp(z::COUNT));
    a.sta(abs(cfs::BPUT_HEADER + header::LENGTH));
    a.lda(zp(z::COUNT + 1));
    a.sta(abs(cfs::BPUT_HEADER + header::LENGTH + 1));
    a.jsr("cfs_write_block");
    a.jsr("cfs_show_written");
    a.lda(abs(cfs::BPUT_HEADER + header::FLAG));
    a.bmi("cfs_save_done");
    a.jsr("cfs_next_output_block");
    a.lda(imm(BETWEEN_BLOCKS));
    a.jsr("cfs_wait");
    a.jmp("cfs_save_block");
    a.here("cfs_save_done");
    a.jmp("cfs_trailer");
}

/// FSCV 5: every block's header, a line for each file, until an Escape
/// or, from the ROMs, the last of them.
fn cfs_catalogue(a: &mut Asm) {
    a.here("cfs_catalogue");
    a.jsr("cfs_options_file");
    a.lda(zp(z::OPTIONS));
    a.ora(imm(option::MESSAGES));
    a.sta(zp(z::OPTIONS));
    a.tsx();
    a.stx(zp(z::STACK));
    a.lda(zp(os::CFS_STATUS));
    a.ora(imm(state::CATALOGUE));
    a.sta(zp(os::CFS_STATUS));
    a.jsr("cfs_read_on");
    a.here("cfs_catalogue_next");
    a.jsr("cfs_read_header");
    a.bcs("cfs_catalogue_next");
    a.jsr("cfs_skip_data");
    a.jsr("cfs_show_read");
    a.lda(abs(cfs::HEADER + header::FLAG));
    a.bpl("cfs_catalogue_next");
    a.jsr("osnewl");
    a.jmp("cfs_catalogue_next");
    // The ROMs are done: back to *CAT's caller.
    a.here("cfs_catalogue_end");
    a.ldx(zp(z::STACK));
    a.txs();
    a.lda(zp(os::CFS_STATUS));
    a.and(imm(!state::CATALOGUE));
    a.sta(zp(os::CFS_STATUS));
    a.rts();
}

/// OSFIND, OSBGET, OSBPUT and EOF.
fn sequential(a: &mut Asm) {
    // OSFIND: A = 0 closes Y's file, or both for Y = 0; &40 and &C0 open
    // the file at XY to read, &80 to write. X and Y are kept.
    a.here("cfs_find");
    a.stx(zp(z::FIND_CALLER));
    a.sty(zp(z::FIND_CALLER + 1));
    a.jsr("cfs_find_call");
    a.ldx(zp(z::FIND_CALLER));
    a.ldy(zp(z::FIND_CALLER + 1));
    a.rts();
    a.here("cfs_find_call");
    a.ora(imm(0));
    a.bne("cfs_open");
    a.tya();
    a.beq("cfs_close_both");
    a.cpy(imm(INPUT_HANDLE));
    a.beq("cfs_close_input");
    a.cpy(imm(OUTPUT_HANDLE));
    a.beq("cfs_close_output");
    a.jmp("cfs_channel");
    a.here("cfs_close_both");
    a.jsr("cfs_close_input");
    a.here("cfs_close_output");
    a.lda(zp(os::CFS_STATUS));
    a.and(imm(state::OUTPUT));
    a.beq(RTS);
    a.lda(imm(flag::LAST));
    a.jsr("cfs_bput_flush");
    a.lda(zp(os::CFS_STATUS));
    a.and(imm(!state::OUTPUT));
    a.sta(zp(os::CFS_STATUS));
    a.rts();
    a.here("cfs_close_input");
    a.lda(zp(os::CFS_STATUS));
    a.and(imm(!(state::INPUT | state::EOF_WARNED)));
    a.sta(zp(os::CFS_STATUS));
    a.rts();

    a.here("cfs_open");
    a.pha();
    a.jsr("cfs_name_at_xy");
    a.pla();
    a.bmi("cfs_open_write_or_update");
    // To read: the first block into the buffer.
    a.here("cfs_open_read");
    a.jsr("cfs_close_input");
    a.ldx(imm(header::NAME_LENGTH));
    a.here("cfs_open_read_name");
    a.lda(abs(cfs::SEARCH_NAME).x());
    a.sta(abs(cfs::BGET_NAME).x());
    a.dex();
    a.bpl("cfs_open_read_name");
    a.lda(imm(0));
    a.sta(abs(cfs::BGET_BLOCK));
    a.sta(abs(cfs::BGET_BLOCK + 1));
    a.jsr("cfs_bget_block");
    a.lda(zp(os::CFS_STATUS));
    a.ora(imm(state::INPUT));
    a.sta(zp(os::CFS_STATUS));
    a.lda(imm(INPUT_HANDLE));
    a.rts();
    a.here("cfs_open_write_or_update");
    a.asl(Acc);
    a.bmi("cfs_open_read");
    // To write: the header named, at load and execution addresses
    // &FFFFFFFF as MOS 1.20 gives them, the buffer empty, the tape ready. A
    // file already open to write is abandoned unwritten, its channel this
    // one, so a *SPOOL holding it carries on into the new file.
    a.jsr("cfs_output_name");
    a.lda(imm(0xFF));
    a.ldx(imm(lo(header::BLOCK - header::LOAD - 1)));
    a.here("cfs_open_write_addresses");
    a.sta(abs(cfs::BPUT_HEADER + header::LOAD).x());
    a.dex();
    a.bpl("cfs_open_write_addresses");
    a.inx();
    a.stx(abs(cfs::BPUT_OFFSET));
    a.stx(abs(cfs::BPUT_HEADER + header::BLOCK));
    a.stx(abs(cfs::BPUT_HEADER + header::BLOCK + 1));
    a.jsr("cfs_options_file");
    a.jsr("cfs_record");
    a.jsr("cfs_read_off");
    a.lda(zp(os::CFS_STATUS));
    a.ora(imm(state::OUTPUT));
    a.sta(zp(os::CFS_STATUS));
    a.lda(imm(OUTPUT_HANDLE));
    a.rts();
    cfs_bget(a);
    cfs_bget_block(a);
    cfs_bput_flush(a);
}

/// OSBGET: the next byte of the file open to read. Past its end, &FE
/// with C set once, then an error.
fn cfs_bget(a: &mut Asm) {
    a.here("cfs_bget");
    a.stx(zp(z::CALLER));
    a.sty(zp(z::CALLER + 1));
    a.jsr("cfs_input_open");
    a.jsr("cfs_bget_ready");
    a.bcs("cfs_bget_past_end");
    a.ldx(abs(cfs::BGET_OFFSET));
    a.lda(abs(BGET_BUFFER).x());
    a.inc(abs(cfs::BGET_OFFSET));
    a.ldx(abs(BGET_LEFT));
    a.bne("cfs_bget_taken");
    a.dec(abs(BGET_LEFT + 1));
    a.here("cfs_bget_taken");
    a.dec(abs(BGET_LEFT));
    a.ldx(zp(z::CALLER));
    a.ldy(zp(z::CALLER + 1));
    a.clc();
    a.rts();
    a.here("cfs_bget_past_end");
    a.lda(zp(os::CFS_STATUS));
    a.bmi("cfs_eof_error");
    a.ora(imm(state::EOF_WARNED));
    a.sta(zp(os::CFS_STATUS));
    a.lda(imm(0xFE));
    a.ldx(zp(z::CALLER));
    a.ldy(zp(z::CALLER + 1));
    a.sec();
    a.rts();
    a.here("cfs_eof_error");
    a.brk();
    a.byte(error::EOF);
    a.text("EOF");
    a.byte(0);

    // Y names the file open to read, or a Channel error.
    a.here("cfs_input_open");
    a.cpy(imm(INPUT_HANDLE));
    a.bne("cfs_channel");
    a.lda(zp(os::CFS_STATUS));
    a.and(imm(state::INPUT));
    a.beq("cfs_channel");
    a.rts();
    a.here("cfs_channel");
    a.brk();
    a.byte(error::CHANNEL);
    a.text("Channel");
    a.byte(0);

    // C set if the open file has nothing more to read; otherwise the
    // buffer holds its next byte, the next block read if need be.
    a.here("cfs_bget_ready");
    a.jsr("cfs_input_exhausted");
    a.bne(RTS);
    a.lda(abs(cfs::LAST_FLAGS));
    a.bpl("cfs_bget_ready_more");
    a.sec();
    a.rts();
    a.here("cfs_bget_ready_more");
    a.jsr("cfs_bget_block");
    a.jmp("cfs_bget_ready");
    // Z set and C clear if the buffer has no bytes left.
    a.here("cfs_input_exhausted");
    a.clc();
    a.lda(abs(BGET_LEFT));
    a.ora(abs(BGET_LEFT + 1));
    a.rts();
}

/// The open file's next block into the buffer.
fn cfs_bget_block(a: &mut Asm) {
    a.here("cfs_bget_block");
    a.ldx(imm(header::NAME_LENGTH));
    a.here("cfs_bget_block_name");
    a.lda(abs(cfs::BGET_NAME).x());
    a.sta(abs(cfs::SEARCH_NAME).x());
    a.dex();
    a.bpl("cfs_bget_block_name");
    a.lda(abs(cfs::BGET_BLOCK));
    a.sta(zp(z::EXPECT));
    a.lda(abs(cfs::BGET_BLOCK + 1));
    a.sta(zp(z::EXPECT + 1));
    a.lda(zp(os::CFS_OPTIONS));
    a.and(imm(0x0F));
    a.jsr("cfs_options_set");
    a.lda(imm(0));
    a.sta(zp(z::MODE));
    a.lda(zp(z::EXPECT));
    a.ora(zp(z::EXPECT + 1));
    a.bne("cfs_bget_block_read");
    a.jsr("cfs_searching");
    a.here("cfs_bget_block_read");
    a.jsr("cfs_read_on");
    a.here("cfs_bget_block_find");
    a.jsr("cfs_find_block");
    a.bcs("cfs_bget_block_find");
    a.lda(imm(lo(BGET_BUFFER)));
    a.sta(zp(z::ADDRESS));
    a.lda(imm(hi(BGET_BUFFER)));
    a.sta(zp(z::ADDRESS + 1));
    a.jsr("cfs_read_data");
    a.bcs("cfs_bget_block_find");
    a.jsr("cfs_read_off");
    a.jsr("cfs_show_read");
    a.lda(abs(cfs::HEADER + header::FLAG));
    a.sta(abs(cfs::LAST_FLAGS));
    a.lda(abs(cfs::HEADER + header::LENGTH));
    a.sta(abs(BGET_LEFT));
    a.lda(abs(cfs::HEADER + header::LENGTH + 1));
    a.sta(abs(BGET_LEFT + 1));
    a.lda(imm(0));
    a.sta(abs(cfs::BGET_OFFSET));
    a.inc(abs(cfs::BGET_BLOCK));
    a.bne(RTS);
    a.inc(abs(cfs::BGET_BLOCK + 1));
    a.rts();

    // OSBPUT: A into the file open to write; a full buffer goes to tape.
    a.here("cfs_bput");
    a.pha();
    a.stx(zp(z::CALLER));
    a.sty(zp(z::CALLER + 1));
    a.cpy(imm(OUTPUT_HANDLE));
    a.bne("cfs_bput_channel");
    a.lda(zp(os::CFS_STATUS));
    a.and(imm(state::OUTPUT));
    a.beq("cfs_bput_channel");
    a.ldx(abs(cfs::BPUT_OFFSET));
    a.pla();
    a.pha();
    a.sta(abs(BPUT_BUFFER).x());
    a.inc(abs(cfs::BPUT_OFFSET));
    a.bne("cfs_bput_done");
    a.lda(imm(0));
    a.jsr("cfs_bput_flush");
    a.here("cfs_bput_done");
    a.ldx(zp(z::CALLER));
    a.ldy(zp(z::CALLER + 1));
    a.pla();
    a.rts();
    a.here("cfs_bput_channel");
    a.pla();
    a.jmp("cfs_channel");
}

/// The buffer, flag A, as a block standing alone; the buffer emptied.
/// An empty buffer is a full one unless it ends the file.
fn cfs_bput_flush(a: &mut Asm) {
    a.here("cfs_bput_flush");
    a.sta(abs(cfs::BPUT_HEADER + header::FLAG));
    a.lda(abs(cfs::BPUT_OFFSET));
    a.sta(abs(cfs::BPUT_HEADER + header::LENGTH));
    a.sta(zp(z::COUNT));
    a.ldx(imm(0));
    a.ora(imm(0));
    a.bne("cfs_bput_flush_length");
    a.lda(abs(cfs::BPUT_HEADER + header::FLAG));
    a.bmi("cfs_bput_flush_length");
    a.inx();
    a.here("cfs_bput_flush_length");
    a.stx(abs(cfs::BPUT_HEADER + header::LENGTH + 1));
    a.stx(zp(z::COUNT + 1));
    a.lda(imm(lo(BPUT_BUFFER)));
    a.sta(zp(z::ADDRESS));
    a.lda(imm(hi(BPUT_BUFFER)));
    a.sta(zp(z::ADDRESS + 1));
    a.lda(zp(os::CFS_OPTIONS));
    a.and(imm(0x0F));
    a.sta(zp(z::OPTIONS));
    a.jsr("cfs_write_on");
    a.lda(imm(PAUSED_LEADER));
    a.jsr("cfs_wait");
    a.jsr("cfs_write_block");
    a.jsr("cfs_show_written");
    a.lda(abs(cfs::BPUT_HEADER + header::FLAG));
    a.bmi("cfs_bput_flush_last");
    a.lda(imm(PAUSED_TRAILER));
    a.jsr("cfs_wait");
    a.jsr("cfs_read_off");
    a.jmp("cfs_bput_flush_next");
    a.here("cfs_bput_flush_last");
    a.jsr("cfs_trailer");
    a.here("cfs_bput_flush_next");
    a.lda(imm(0));
    a.sta(abs(cfs::BPUT_OFFSET));

    // The file being written moves on to its next block.
    a.here("cfs_next_output_block");
    a.inc(abs(cfs::BPUT_HEADER + header::BLOCK));
    a.bne(RTS);
    a.inc(abs(cfs::BPUT_HEADER + header::BLOCK + 1));
    a.rts();

    // FSCV 1: X = &FF if handle X's file has no more to read.
    a.here("cfs_eof");
    a.txa();
    a.tay();
    a.jsr("cfs_input_open");
    a.ldx(imm(0));
    a.jsr("cfs_input_exhausted");
    a.bne(RTS);
    a.lda(abs(cfs::LAST_FLAGS));
    a.bpl(RTS);
    a.dex();
    a.rts();
}

/// `cfs_block_word`: the OSFILE block's four bytes from Y into the zero
/// page from X, Y past them.
fn block_word(a: &mut Asm) {
    a.here("cfs_block_word");
    a.lda(imm(4));
    a.sta(zp(z::COUNT));
    let next = a.here_local();
    a.lda(zp(z::BLOCK).ind_y());
    a.sta(zp(0).x());
    a.iny();
    a.inx();
    a.dec(zp(z::COUNT));
    a.bne(next);
    a.rts();
}

/// Names: what a call names, and whether a header matches it.
fn names(a: &mut Asm) {
    a.here("cfs_block_name");
    a.ldy(imm(0));
    a.lda(zp(z::BLOCK).ind_y());
    a.tax();
    a.iny();
    a.lda(zp(z::BLOCK).ind_y());
    a.tay();
    // XY's string, through GSREAD, as the name searched for: at most ten
    // characters.
    a.here("cfs_name_at_xy");
    a.stx(zp(os::TEXT));
    a.sty(zp(os::TEXT + 1));
    a.ldy(imm(0));
    a.clc();
    a.jsr("gsinit");
    a.ldx(imm(0));
    a.here("cfs_name_next");
    a.jsr("gsread");
    a.bcs("cfs_name_end");
    a.cpx(imm(header::NAME_LENGTH));
    a.bcs("cfs_name_bad");
    a.sta(abs(cfs::SEARCH_NAME).x());
    a.inx();
    a.bne("cfs_name_next");
    a.here("cfs_name_end");
    a.lda(imm(0));
    a.sta(abs(cfs::SEARCH_NAME).x());
    a.rts();
    a.here("cfs_name_bad");
    a.brk();
    a.byte(0xCC);
    a.text("Bad filename");
    a.byte(0);

    // The name searched for as the header of the file being written.
    a.here("cfs_output_name");
    a.ldx(imm(lo(header::LOAD - 1)));
    a.here("cfs_output_name_next");
    a.lda(abs(cfs::SEARCH_NAME).x());
    a.cpx(imm(header::NAME_LENGTH + 1));
    a.bcc("cfs_output_name_set");
    a.lda(imm(0));
    a.here("cfs_output_name_set");
    a.sta(abs(cfs::BPUT_HEADER).x());
    a.dex();
    a.bpl("cfs_output_name_next");
    a.lda(imm(0));
    a.ldx(imm(3));
    let next = a.here_local();
    a.sta(abs(cfs::BPUT_HEADER + header::SPARE).x());
    a.dex();
    a.bpl(next);
    a.sta(abs(cfs::BPUT_HEADER + header::BLOCK));
    a.sta(abs(cfs::BPUT_HEADER + header::BLOCK + 1));
    a.rts();

    // C clear if the header read is the file searched for: an empty search
    // name matches any, and letters match in either case.
    a.here("cfs_name_matches");
    a.lda(abs(cfs::SEARCH_NAME));
    a.beq("cfs_name_match");
    a.ldx(imm(0));
    a.here("cfs_name_compare");
    a.lda(abs(cfs::SEARCH_NAME).x());
    a.jsr("cfs_upper");
    a.sta(zp(z::COMPARE));
    a.lda(abs(cfs::HEADER).x());
    a.jsr("cfs_upper");
    a.cmp(zp(z::COMPARE));
    a.bne("cfs_name_differs");
    a.ora(imm(0));
    a.beq("cfs_name_match");
    a.inx();
    a.cpx(imm(header::NAME_LENGTH + 1));
    a.bne("cfs_name_compare");
    a.here("cfs_name_match");
    a.clc();
    a.rts();
    a.here("cfs_name_differs");
    a.sec();
    a.rts();
    a.here("cfs_upper");
    a.cmp(imm(b'a'));
    a.bcc(RTS);
    a.cmp(imm(b'z' + 1));
    a.bcs(RTS);
    a.and(imm(0xDF));
    a.rts();

    // The LOAD and SAVE options in force, for an operation that is not
    // *CAT.
    a.here("cfs_options_file");
    a.lda(zp(os::CFS_OPTIONS));
    for _ in 0..4 {
        a.lsr(Acc);
    }
    a.here("cfs_options_set");
    a.sta(zp(z::OPTIONS));
    a.lda(zp(os::CFS_STATUS));
    a.and(imm(!state::CATALOGUE));
    a.sta(zp(os::CFS_STATUS));
    a.rts();

    a.here("cfs_expect_next");
    a.inc(zp(z::EXPECT));
    a.bne(RTS);
    a.inc(zp(z::EXPECT + 1));
    a.rts();
}

/// Finding blocks and reading their headers.
fn blocks(a: &mut Asm) {
    // The next block of the file searched for with the number wanted; C
    // set after any other block, passed over and shown, or after an error
    // reported so the block is looked for again.
    a.here("cfs_find_block");
    a.jsr("cfs_read_header");
    a.bcc("cfs_find_header");
    a.lda(imm(error::HEADER));
    a.jsr("cfs_error");
    a.bcs(RTS);
    a.here("cfs_find_header");
    a.jsr("cfs_name_matches");
    a.bcs("cfs_find_other_file");
    a.lda(abs(cfs::HEADER + header::BLOCK));
    a.cmp(zp(z::EXPECT));
    a.bne("cfs_find_other_block");
    a.lda(abs(cfs::HEADER + header::BLOCK + 1));
    a.cmp(zp(z::EXPECT + 1));
    a.bne("cfs_find_other_block");
    a.clc();
    a.rts();
    // Until its first block, a file is waited for; after it, another of its
    // blocks is out of order and another file's is a lost end.
    a.here("cfs_find_other_block");
    a.lda(imm(error::BLOCK));
    a.bne("cfs_find_started");
    a.here("cfs_find_other_file");
    a.lda(imm(error::FILE));
    a.here("cfs_find_started");
    a.ldx(zp(z::EXPECT));
    a.bne("cfs_find_misplaced");
    a.ldx(zp(z::EXPECT + 1));
    a.beq("cfs_find_pass");
    a.here("cfs_find_misplaced");
    a.jsr("cfs_error");
    a.bcc(RTS);
    a.here("cfs_find_pass");
    a.jsr("cfs_skip_data");
    a.jsr("cfs_show_read");
    a.lda(abs(cfs::HEADER + header::FLAG));
    a.bpl("cfs_find_passed");
    a.lda(zp(z::OPTIONS));
    a.and(imm(option::MESSAGES));
    a.beq("cfs_find_passed");
    a.jsr("osnewl");
    a.here("cfs_find_passed");
    a.sec();
    a.rts();
    cfs_read_header(a);
}

/// The next block's header into &3B2; C set if it is not one. A ROM's
/// continuation is the last header with the next number.
fn cfs_read_header(a: &mut Asm) {
    a.here("cfs_read_header");
    a.jsr("cfs_byte");
    a.cmp(imm(SYNC));
    a.beq("cfs_read_header_sync");
    a.ldx(abs(var::CFS_OR_RFS));
    a.beq("cfs_read_header");
    a.cmp(imm(END_OF_ROM));
    a.bne("cfs_read_header_rom");
    a.jsr("rfs_next_rom");
    a.jmp("cfs_read_header");
    a.here("cfs_read_header_rom");
    a.cmp(imm(CONTINUATION));
    a.bne("cfs_read_header");
    a.inc(abs(cfs::HEADER + header::BLOCK));
    a.bne("cfs_read_header_continued");
    a.inc(abs(cfs::HEADER + header::BLOCK + 1));
    a.here("cfs_read_header_continued");
    a.clc();
    a.rts();
    a.here("cfs_read_header_sync");
    a.jsr("cfs_crc_start");
    a.ldy(imm(0));
    a.here("cfs_read_header_name");
    a.jsr("cfs_byte_crc");
    a.sta(abs(cfs::HEADER).y());
    a.ora(imm(0));
    a.beq("cfs_read_header_named");
    a.iny();
    a.cpy(imm(header::NAME_LENGTH + 1));
    a.bcc("cfs_read_header_name");
    a.rts();
    a.here("cfs_read_header_named");
    a.iny();
    a.cpy(imm(lo(header::LOAD)));
    a.bcs("cfs_read_header_fields");
    a.sta(abs(cfs::HEADER).y());
    a.bcc("cfs_read_header_named");
    a.here("cfs_read_header_fields");
    a.ldy(imm(0));
    a.here("cfs_read_header_field");
    a.jsr("cfs_byte_crc");
    a.sta(abs(cfs::HEADER + header::LOAD).y());
    a.iny();
    a.cpy(imm(header::FIELDS));
    a.bne("cfs_read_header_field");
    for offset in 0..2u16 {
        a.jsr("cfs_byte");
        a.sta(abs(cfs::HEADER + header::CRC + offset));
    }
    a.lda(abs(cfs::HEADER + header::CRC));
    a.cmp(zp(z::CRC_HIGH));
    a.bne("cfs_read_header_bad");
    a.lda(abs(cfs::HEADER + header::CRC + 1));
    a.cmp(zp(z::CRC_LOW));
    a.bne("cfs_read_header_bad");
    a.clc();
    a.rts();
    a.here("cfs_read_header_bad");
    a.sec();
    a.rts();
}

/// Moving a block's data.
fn transfers(a: &mut Asm) {
    // The block's data to ADDRESS, which moves past it; C set, ADDRESS
    // unmoved, if its CRC is wrong and the error is not ignored.
    a.here("cfs_read_data");
    a.jsr("cfs_count_from_header");
    a.beq("cfs_read_data_done");
    a.jsr("cfs_crc_start");
    a.jsr("cfs_pointer_from_address");
    a.here("cfs_read_data_byte");
    a.jsr("cfs_byte_crc");
    a.sta(zp(z::POINTER).ind_y());
    a.jsr("cfs_count_one");
    a.bne("cfs_read_data_byte");
    a.jsr("cfs_byte");
    a.cmp(zp(z::CRC_HIGH));
    a.php();
    a.jsr("cfs_byte");
    a.plp();
    a.bne("cfs_read_data_bad");
    a.cmp(zp(z::CRC_LOW));
    a.beq("cfs_read_data_good");
    a.here("cfs_read_data_bad");
    a.lda(imm(error::DATA));
    a.jsr("cfs_error");
    a.bcs(RTS);
    a.here("cfs_read_data_good");
    a.jsr("cfs_address_from_pointer");
    a.here("cfs_read_data_done");
    a.clc();
    a.rts();

    // Past the block's data and its CRC.
    a.here("cfs_skip_data");
    a.jsr("cfs_count_from_header");
    a.beq(RTS);
    a.ldy(imm(0));
    a.here("cfs_skip_data_byte");
    a.jsr("cfs_byte");
    a.jsr("cfs_count_one");
    a.bne("cfs_skip_data_byte");
    a.jsr("cfs_byte");
    a.jmp("cfs_byte");

    // COUNT = the header's length; Z set if it is empty.
    a.here("cfs_count_from_header");
    a.lda(abs(cfs::HEADER + header::LENGTH));
    a.sta(zp(z::COUNT));
    a.lda(abs(cfs::HEADER + header::LENGTH + 1));
    a.sta(zp(z::COUNT + 1));
    a.ora(zp(z::COUNT));
    a.rts();

    // One byte moved: Y and POINTER on, COUNT down; Z set when it is done.
    a.here("cfs_count_one");
    a.iny();
    a.bne("cfs_count_one_pointer");
    a.inc(zp(z::POINTER + 1));
    a.here("cfs_count_one_pointer");
    a.lda(zp(z::COUNT));
    a.bne("cfs_count_one_low");
    a.dec(zp(z::COUNT + 1));
    a.here("cfs_count_one_low");
    a.dec(zp(z::COUNT));
    a.lda(zp(z::COUNT));
    a.ora(zp(z::COUNT + 1));
    a.rts();

    a.here("cfs_pointer_from_address");
    a.lda(zp(z::ADDRESS));
    a.sta(zp(z::POINTER));
    a.lda(zp(z::ADDRESS + 1));
    a.sta(zp(z::POINTER + 1));
    a.ldy(imm(0));
    a.rts();

    // ADDRESS = where the data moved to: POINTER plus Y.
    a.here("cfs_address_from_pointer");
    a.tya();
    a.clc();
    a.adc(zp(z::POINTER));
    a.sta(zp(z::ADDRESS));
    a.lda(zp(z::POINTER + 1));
    a.adc(imm(0));
    a.sta(zp(z::ADDRESS + 1));
    a.rts();
    cfs_write_block(a);
    cfs_crc_add(a);
}

/// The block at &380: its header, then COUNT bytes of data from ADDRESS,
/// each with its CRC; then two repeats of the last byte, cut short as
/// the 6850 is held reset and the line idles.
fn cfs_write_block(a: &mut Asm) {
    a.here("cfs_write_block");
    a.lda(imm(WRITING));
    a.ora(zp(fs::BAUD));
    a.sta(abs(sheila::ACIA_CONTROL));
    a.lda(imm(SYNC));
    a.jsr("cfs_write_byte");
    a.jsr("cfs_crc_start");
    a.ldy(imm(0));
    a.here("cfs_write_name");
    a.lda(abs(cfs::BPUT_HEADER).y());
    a.jsr("cfs_write_crc");
    a.lda(abs(cfs::BPUT_HEADER).y());
    a.beq("cfs_write_fields_start");
    a.iny();
    a.bne("cfs_write_name");
    a.here("cfs_write_fields_start");
    a.ldy(imm(0));
    a.here("cfs_write_field");
    a.lda(abs(cfs::BPUT_HEADER + header::LOAD).y());
    a.jsr("cfs_write_crc");
    a.iny();
    a.cpy(imm(header::FIELDS));
    a.bne("cfs_write_field");
    a.jsr("cfs_write_crc_bytes");
    a.lda(abs(cfs::BPUT_HEADER + header::LENGTH));
    a.sta(zp(z::COUNT));
    a.lda(abs(cfs::BPUT_HEADER + header::LENGTH + 1));
    a.sta(zp(z::COUNT + 1));
    a.ora(zp(z::COUNT));
    a.beq("cfs_write_end");
    a.jsr("cfs_crc_start");
    a.jsr("cfs_pointer_from_address");
    a.here("cfs_write_data");
    a.lda(zp(z::POINTER).ind_y());
    a.jsr("cfs_write_crc");
    a.jsr("cfs_count_one");
    a.bne("cfs_write_data");
    a.jsr("cfs_address_from_pointer");
    a.jsr("cfs_write_crc_bytes");
    a.here("cfs_write_end");
    for _ in 0..2 {
        a.lda(zp(z::LAST));
        a.jsr("cfs_write_byte");
    }
    a.lda(imm(control::MASTER_RESET));
    a.sta(abs(sheila::ACIA_CONTROL));
    a.rts();

    a.here("cfs_write_crc_bytes");
    a.lda(zp(z::CRC_HIGH));
    a.jsr("cfs_write_byte");
    a.lda(zp(z::CRC_LOW));
    a.jmp("cfs_write_byte");

    // A to the tape and into the CRC. Y is kept.
    a.here("cfs_write_crc");
    a.pha();
    a.jsr("cfs_crc_add");
    a.pla();
    // A to the tape once the 6850 can take it. Y is kept.
    a.here("cfs_write_byte");
    a.sta(zp(z::LAST));
    a.here("cfs_write_wait");
    a.bit(zp(os::ESCAPE));
    a.bmi("cfs_escape");
    a.lda(abs(sheila::ACIA_CONTROL));
    a.and(imm(status::TDRE));
    a.beq("cfs_write_wait");
    a.lda(zp(z::LAST));
    a.sta(abs(sheila::ACIA_DATA));
    a.rts();

    a.here("cfs_crc_start");
    a.lda(imm(0));
    a.sta(zp(z::CRC_HIGH));
    a.sta(zp(z::CRC_LOW));
    a.rts();

    // A from the device and into the CRC. Y is kept.
    a.here("cfs_byte_crc");
    a.jsr("cfs_byte");
    a.pha();
    a.jsr("cfs_crc_add");
    a.pla();
    a.rts();
}

/// The CRC-16/XMODEM with A added. Y is kept.
fn cfs_crc_add(a: &mut Asm) {
    a.here("cfs_crc_add");
    a.eor(zp(z::CRC_HIGH));
    a.sta(zp(z::CRC_HIGH));
    a.ldx(imm(8));
    a.here("cfs_crc_bit");
    a.asl(zp(z::CRC_LOW));
    a.rol(zp(z::CRC_HIGH));
    a.bcc("cfs_crc_shifted");
    a.lda(zp(z::CRC_HIGH));
    a.eor(imm(0x10));
    a.sta(zp(z::CRC_HIGH));
    a.lda(zp(z::CRC_LOW));
    a.eor(imm(0x21));
    a.sta(zp(z::CRC_LOW));
    a.here("cfs_crc_shifted");
    a.dex();
    a.bne("cfs_crc_bit");
    a.rts();
}

/// The cassette and the ROMs as sources, and the tape as a sink.
fn devices(a: &mut Asm) {
    // The next byte from the filing system's device. Y is kept.
    a.here("cfs_byte");
    a.lda(abs(var::CFS_OR_RFS));
    a.bne("rfs_byte");
    a.here("cassette_byte");
    a.bit(zp(os::ESCAPE));
    a.bmi("cfs_escape");
    a.lda(abs(sheila::ACIA_CONTROL));
    a.and(imm(status::RDRF));
    a.beq("cassette_byte");
    a.lda(abs(sheila::ACIA_DATA));
    a.rts();
    a.here("cfs_escape");
    a.jsr("cfs_read_off");
    a.brk();
    a.byte(error::ESCAPE);
    a.text("Escape");
    a.byte(0);

    // The current ROM's next byte; a ROM that gives none passes to the next.
    // Past every ROM's, *CAT is done and anything else did not find its
    // file.
    a.here("rfs_byte");
    a.tya();
    a.pha();
    a.here("rfs_byte_again");
    a.ldy(zp(os::RFS_ROM));
    a.cpy(imm(ROMS));
    a.bcs("rfs_exhausted");
    a.lda(imm(service::RFS_BYTE));
    a.jsr("service_call");
    a.bne("rfs_byte_next_rom");
    a.sty(zp(z::COMPARE));
    a.pla();
    a.tay();
    a.lda(zp(z::COMPARE));
    a.rts();
    a.here("rfs_byte_next_rom");
    a.jsr("rfs_next_rom");
    a.jmp("rfs_byte_again");
    // The ROM after this one that has data.
    a.here("rfs_next_rom");
    a.inc(zp(os::RFS_ROM));
    a.jmp("rfs_start_rom");
    a.here("rfs_exhausted");
    a.lda(zp(os::CFS_STATUS));
    a.and(imm(state::CATALOGUE));
    a.beq("file_not_found");
    a.jmp("cfs_catalogue_end");
    a.here("file_not_found");
    a.brk();
    a.byte(error::NOT_FOUND);
    a.text("File not found");
    a.byte(0);

    // From the ROM &F5 counts to, the first that answers service call 13,
    // and so has data.
    a.here("rfs_start_rom");
    a.ldy(zp(os::RFS_ROM));
    a.cpy(imm(ROMS));
    a.bcs(RTS);
    a.lda(imm(service::RFS_START));
    a.jsr("service_call");
    a.beq(RTS);
    a.inc(zp(os::RFS_ROM));
    a.bne("rfs_start_rom");
    a.rts();

    // Ready to read: the ROMs from the first, or the cassette running.
    a.here("cfs_read_on");
    a.lda(abs(var::CFS_OR_RFS));
    a.beq("cassette_read_on");
    a.lda(imm(0));
    a.sta(zp(os::RFS_ROM));
    a.beq("rfs_start_rom");
    a.here("cassette_read_on");
    a.jsr("cassette_take");
    a.lda(imm(READING));
    a.ora(zp(fs::BAUD));
    a.sta(abs(sheila::ACIA_CONTROL));
    a.rts();
    cassette_take(a);
    osrdrm(a);
}

/// The cassette given the 6850, its motor running.
fn cassette_take(a: &mut Asm) {
    a.here("cassette_take");
    a.lda(imm(0x80));
    a.sta(zp(os::CFS_CRITICAL));
    a.lda(imm(control::MASTER_RESET));
    a.sta(abs(sheila::ACIA_CONTROL));
    a.lda(imm(ULA_CASSETTE));
    a.sta(abs(sheila::SERIAL_ULA));
    a.rts();

    // Writing begins: the 6850 sending, a dummy byte opening the tone.
    a.here("cfs_write_on");
    a.jsr("cassette_take");
    a.lda(imm(WRITING));
    a.ora(zp(fs::BAUD));
    a.sta(abs(sheila::ACIA_CONTROL));
    a.lda(imm(DUMMY));
    a.jmp("cfs_write_byte");

    // The motor stopped and RS423 given the 6850 back.
    a.here("cfs_read_off");
    a.lda(zp(os::CFS_CRITICAL));
    a.bpl(RTS);
    a.lda(imm(control::MASTER_RESET));
    a.sta(abs(sheila::ACIA_CONTROL));
    // The 6850 stays reset until its control is written back.
    a.lda(imm(0));
    a.sta(zp(os::CFS_CRITICAL));
    a.jmp("serial_restore");

    // A VSYNCs of tone.
    a.here("cfs_wait");
    a.sta(abs(var::CFS_TIMEOUT));
    a.here("cfs_wait_tone");
    a.bit(zp(os::ESCAPE));
    a.bmi("cfs_escape_far");
    a.lda(abs(var::CFS_TIMEOUT));
    a.bne("cfs_wait_tone");
    a.rts();
    a.here("cfs_escape_far");
    a.jmp("cfs_escape");

    a.here("cfs_leader");
    a.jsr("cfs_write_on");
    a.lda(imm(LEADER));
    a.bne("cfs_wait");

    // A written file's trailer tone, then its end reported.
    a.here("cfs_trailer");
    for vsyncs in TRAILER {
        a.lda(imm(vsyncs));
        a.jsr("cfs_wait");
    }
    a.jmp("cfs_finished");

    // "RECORD then RETURN" with the motor running, when messages are on;
    // the RETURN, or an Escape.
    a.here("cfs_record");
    a.lda(zp(z::OPTIONS));
    a.and(imm(option::MESSAGES));
    a.beq(RTS);
    a.lda(abs(var::SERIAL_ULA));
    a.ora(imm(serial::MOTOR));
    a.sta(abs(sheila::SERIAL_ULA));
    a.jsr("print_inline");
    a.text("RECORD then RETURN");
    a.byte(0);
    a.lda(imm(0x0F));
    a.ldx(imm(1));
    a.jsr("osbyte");
    a.here("cfs_record_wait");
    a.jsr("osrdch");
    a.bcs("cfs_escape_far");
    a.cmp(imm(0x0D));
    a.bne("cfs_record_wait");
    a.jmp("osnewl");

    // The 6850 interrupting while the cassette has it: nothing is waited on
    // that way, so the cause is cleared.
    a.here("cfs_interrupt");
    a.lda(abs(sheila::ACIA_CONTROL));
    a.lda(abs(sheila::ACIA_DATA));
    a.rts();
}

/// OSRDRM: the byte at (&F6) in ROM Y.
fn osrdrm(a: &mut Asm) {
    a.here("osrdrm");
    a.lda(zp(os::ROM));
    a.pha();
    a.sty(zp(os::ROM));
    a.sty(abs(sheila::ROMSEL));
    a.ldy(imm(0));
    a.lda(zp(os::RFS_POINTER).ind_y());
    a.tay();
    a.pla();
    a.sta(zp(os::ROM));
    a.sta(abs(sheila::ROMSEL));
    a.tya();
    a.rts();

    // SHIFT-BREAK with no filing system ROM to boot: !BOOT run from the
    // ROMs if one holds it. An error is printed, and the machine carries
    // on as a plain BREAK would.
    a.here("boot_from_rom");
    a.lda(imm(RFS));
    a.sta(abs(var::CFS_OR_RFS));
    a.lda(imm(lo("boot_failed")));
    a.sta(abs(vector::BRK));
    a.lda(imm(hi("boot_failed")));
    a.sta(abs(vector::BRK + 1));
    a.ldx(imm(lo("boot_name")));
    a.ldy(imm(hi("boot_name")));
    a.lda(imm(fsc::RUN));
    a.jsr("fsc_call");
    a.jmp("boot_restore");
    a.here("boot_failed");
    a.ldx(imm(0xFF));
    a.txs();
    a.cli();
    a.jsr("error_text");
    a.jsr("osnewl");
    a.here("boot_restore");
    a.lda(imm(0));
    a.sta(abs(var::CFS_OR_RFS));
    a.lda(imm(lo("brk_default")));
    a.sta(abs(vector::BRK));
    a.lda(imm(hi("brk_default")));
    a.sta(abs(vector::BRK + 1));
    a.jmp("boot_done");
    a.here("boot_name");
    a.text("!BOOT");
    a.byte(0x0D);
}

/// What the filing system tells the user.
fn messages(a: &mut Asm) {
    a.here("cfs_searching");
    a.lda(zp(z::OPTIONS));
    a.and(imm(option::MESSAGES));
    a.beq(RTS);
    a.jsr("print_inline");
    a.text("Searching");
    a.byte(0);
    a.jmp("osnewl");

    a.here("cfs_loading");
    a.lda(zp(z::OPTIONS));
    a.and(imm(option::MESSAGES));
    a.beq(RTS);
    a.jsr("osnewl");
    a.jsr("print_inline");
    a.text("Loading");
    a.byte(0);
    a.jmp("osnewl");
    cfs_show_read(a);
    cfs_finished(a);
}

/// A header on its file's line: a file read starts a new line at its first
/// block, one written follows its prompt's; for its last block, the file's
/// length and, in long messages, its addresses.
fn cfs_show_read(a: &mut Asm) {
    a.here("cfs_show_read");
    a.ldx(imm(lo(cfs::HEADER)));
    a.ldy(imm(hi(cfs::HEADER)));
    a.sec();
    a.bcs("cfs_show");
    a.here("cfs_show_written");
    a.ldx(imm(lo(cfs::BPUT_HEADER)));
    a.ldy(imm(hi(cfs::BPUT_HEADER)));
    a.clc();
    a.here("cfs_show");
    a.lda(zp(z::OPTIONS));
    a.and(imm(option::MESSAGES));
    a.bne("cfs_show_on");
    a.rts();
    a.here("cfs_show_on");
    a.stx(zp(z::SHOWN));
    a.sty(zp(z::SHOWN + 1));
    a.bcc("cfs_show_line");
    a.ldy(imm(lo(header::BLOCK)));
    a.lda(zp(z::SHOWN).ind_y());
    a.iny();
    a.ora(zp(z::SHOWN).ind_y());
    a.bne("cfs_show_line");
    a.jsr("osnewl");
    a.here("cfs_show_line");
    a.lda(imm(0x0D));
    a.jsr("oswrch");
    a.ldy(imm(0));
    a.here("cfs_show_name");
    a.lda(zp(z::SHOWN).ind_y());
    a.beq("cfs_show_pad");
    a.cmp(imm(b' '));
    a.bcs("cfs_show_character");
    a.lda(imm(b'.'));
    a.here("cfs_show_character");
    a.jsr("oswrch");
    a.iny();
    a.cpy(imm(header::NAME_LENGTH));
    a.bne("cfs_show_name");
    a.here("cfs_show_pad");
    a.lda(imm(b' '));
    a.jsr("oswrch");
    a.iny();
    a.cpy(imm(header::NAME_LENGTH + 1));
    a.bcc("cfs_show_pad");
    a.ldy(imm(lo(header::BLOCK)));
    a.lda(zp(z::SHOWN).ind_y());
    a.jsr("print_hex");
    a.ldy(imm(lo(header::FLAG)));
    a.lda(zp(z::SHOWN).ind_y());
    a.bpl(RTS);
    a.lda(imm(b' '));
    a.jsr("oswrch");
    a.ldy(imm(lo(header::BLOCK)));
    a.lda(zp(z::SHOWN).ind_y());
    a.ldy(imm(lo(header::LENGTH + 1)));
    a.clc();
    a.adc(zp(z::SHOWN).ind_y());
    a.jsr("print_hex");
    a.ldy(imm(lo(header::LENGTH)));
    a.lda(zp(z::SHOWN).ind_y());
    a.jsr("print_hex");
    a.lda(zp(z::OPTIONS));
    a.and(imm(option::LONG));
    a.beq(RTS);
    for (spaces, field) in [(4u8, header::LOAD), (1, header::EXEC)] {
        a.ldx(imm(spaces));
        let space = a.here_local();
        a.lda(imm(b' '));
        a.jsr("oswrch");
        a.dex();
        a.bne(space);
        a.ldy(imm(lo(field + 3)));
        let byte = a.here_local();
        a.lda(zp(z::SHOWN).ind_y());
        a.jsr("print_hex");
        a.dey();
        a.cpy(imm(lo(field - 1)));
        a.bne(byte);
    }
    a.rts();
}

/// A file loaded or saved: with messages, the bell and a new line; the
/// motor stopped either way.
fn cfs_finished(a: &mut Asm) {
    a.here("cfs_finished");
    a.lda(zp(z::OPTIONS));
    a.and(imm(option::MESSAGES));
    a.beq("cfs_finished_off");
    a.lda(imm(0x07));
    a.jsr("oswrch");
    a.jsr("osnewl");
    a.here("cfs_finished_off");
    a.jmp("cfs_read_off");

    // A in two hexadecimal digits.
    a.here("print_hex");
    a.pha();
    for _ in 0..4 {
        a.lsr(Acc);
    }
    a.jsr("print_hex_digit");
    a.pla();
    a.here("print_hex_digit");
    a.and(imm(0x0F));
    a.cmp(imm(10));
    a.bcc("print_hex_decimal");
    a.adc(imm(b'A' - b'0' - 10 - 1));
    a.here("print_hex_decimal");
    a.adc(imm(b'0'));
    a.jmp("oswrch");
}

/// Errors on a block, by the option nibble in force: ignored, reported and
/// the block looked for again, or raised.
fn errors(a: &mut Asm) {
    // Error A on the block just read: C set to look for it again, clear to
    // carry on as if it were good.
    a.here("cfs_error");
    a.sta(zp(z::ERROR));
    a.lda(zp(z::OPTIONS));
    a.and(imm(option::RESPONSE));
    a.bne("cfs_error_reported");
    a.clc();
    a.rts();
    a.here("cfs_error_reported");
    a.and(imm(option::ABORT));
    a.bne("cfs_error_raise");
    a.lda(zp(z::OPTIONS));
    a.and(imm(option::MESSAGES));
    a.beq("cfs_error_again");
    a.jsr("osnewl");
    a.jsr("cfs_error_record");
    a.ldy(imm(2));
    a.here("cfs_error_text");
    a.lda(zp(z::SHOWN).ind_y());
    a.beq("cfs_error_shown");
    a.jsr("oswrch");
    a.iny();
    a.bne("cfs_error_text");
    a.here("cfs_error_shown");
    a.jsr("osnewl");
    a.here("cfs_error_again");
    a.sec();
    a.rts();
    a.here("cfs_error_raise");
    a.jsr("cfs_read_off");
    a.jsr("cfs_error_record");
    a.jmp(abs(z::SHOWN).ind());

    // SHOWN = error ERROR's BRK.
    a.here("cfs_error_record");
    a.lda(zp(z::ERROR));
    a.sec();
    a.sbc(imm(error::DATA));
    a.tax();
    a.lda(abs("cfs_error_low").x());
    a.sta(zp(z::SHOWN));
    a.lda(abs("cfs_error_high").x());
    a.sta(zp(z::SHOWN + 1));
    a.rts();
    a.here("cfs_error_low");
    for (record, _, _) in error::BLOCK_ERRORS {
        a.byte(lo(record));
    }
    a.here("cfs_error_high");
    for (record, _, _) in error::BLOCK_ERRORS {
        a.byte(hi(record));
    }
    for (record, number, text) in error::BLOCK_ERRORS {
        a.here(record);
        a.brk();
        a.byte(number);
        a.text(text);
        a.byte(0);
    }
}
