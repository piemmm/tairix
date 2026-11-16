//! The logical file layer: SETLFS and SETNAM, the table of open files,
//! OPEN, CLOSE, CHKIN, CHKOUT, CLRCHN and CLALL, CHRIN, CHROUT and GETIN
//! by device, LOAD and SAVE with the serial bus's own, READST, SETMSG,
//! MEMTOP, MEMBOT and IOBASE, and the KERNAL's errors and messages.

use super::fixed;
use super::ws::{page, vector, zp};
use crate::machine::c64::io;
use crate::machine::c64::serial::{channel, command};
use crate::rom::asm::{abs, byte, imm, zp as z, Acc, Asm, RTS};

/// Devices.
pub mod device {
    /// The keyboard, through the screen editor.
    pub const KEYBOARD: u8 = 0;
    /// The datasette.
    pub const TAPE: u8 = 1;
    /// RS-232 on the user port.
    pub const RS232: u8 = 2;
    /// The screen.
    pub const SCREEN: u8 = 3;
}

/// The error numbers a failed call returns in A with C set.
pub mod error {
    /// STOP was pressed.
    pub const STOPPED: u8 = 0;
    /// Ten files are open already.
    pub const TOO_MANY_FILES: u8 = 1;
    /// The file's number is open already.
    pub const FILE_OPEN: u8 = 2;
    /// No file of that number is open.
    pub const FILE_NOT_OPEN: u8 = 3;
    /// The file was not found.
    pub const FILE_NOT_FOUND: u8 = 4;
    /// The device did not answer.
    pub const DEVICE_NOT_PRESENT: u8 = 5;
    /// The file is not open to read.
    pub const NOT_INPUT_FILE: u8 = 6;
    /// The file is not open to write.
    pub const NOT_OUTPUT_FILE: u8 = 7;
    /// The device needs a file name.
    pub const MISSING_FILE_NAME: u8 = 8;
    /// The device cannot do it.
    pub const ILLEGAL_DEVICE: u8 = 9;
    /// An RS-232 file opened or closed, its buffers taking or giving back
    /// the top of memory where they must: the top in X and Y, which BASIC
    /// follows with a CLR.
    pub const MEMORY_MOVED: u8 = 0xF0;
}

/// STATUS's bits.
pub mod status {
    /// A serial bus listener did not take a byte.
    pub const WRITE_TIMEOUT: u8 = 0x01;
    /// A serial bus talker sent nothing.
    pub const READ_TIMEOUT: u8 = 0x02;
    /// A tape block ended early.
    pub const SHORT_BLOCK: u8 = 0x04;
    /// A tape block ran long.
    pub const LONG_BLOCK: u8 = 0x08;
    /// A tape byte could not be read in either copy, or a verify differed.
    pub const MISMATCH: u8 = 0x10;
    /// A tape block's checksum failed.
    pub const CHECKSUM: u8 = 0x20;
    /// The last byte of a file: EOI, or the tape file's end.
    pub const END: u8 = 0x40;
    /// The device is not there, or the tape ended.
    pub const ABSENT: u8 = 0x80;
}

/// The most files open at once.
const MAX_FILES: u8 = 10;

/// The messages, in the order the offsets below name them; each ends at
/// the character with its top bit set.
const MESSAGES: [&str; 10] = [
    "\rI/O ERROR #",
    "\rSEARCHING ",
    "FOR ",
    "\rPRESS PLAY ON TAPE",
    "\rPRESS RECORD & PLAY ON TAPE",
    "\rLOADING",
    "\rSAVING ",
    "\rVERIFYING",
    "\rFOUND ",
    "\rOK\r",
];

const _: () = {
    let mut message = 0;
    while message < MESSAGES.len() {
        assert!(
            !MESSAGES[message].is_empty(),
            "a message has its last character"
        );
        message += 1;
    }
};

/// Message `index`'s offset in the table.
const fn offset(index: usize) -> u8 {
    let mut total = 0;
    let mut message = 0;
    while message < index {
        total += MESSAGES[message].len();
        message += 1;
    }
    byte(total)
}

/// Each message's offset, for `print_message` and `control_message`.
pub mod message {
    use super::offset;
    /// "I/O ERROR #".
    pub const IO_ERROR: u8 = offset(0);
    /// "SEARCHING ".
    pub const SEARCHING: u8 = offset(1);
    /// "FOR ".
    pub const FOR: u8 = offset(2);
    /// "PRESS PLAY ON TAPE".
    pub const PRESS_PLAY: u8 = offset(3);
    /// "PRESS RECORD & PLAY ON TAPE".
    pub const PRESS_RECORD: u8 = offset(4);
    /// "LOADING".
    pub const LOADING: u8 = offset(5);
    /// "SAVING ".
    pub const SAVING: u8 = offset(6);
    /// "VERIFYING".
    pub const VERIFYING: u8 = offset(7);
    /// "FOUND ".
    pub const FOUND: u8 = offset(8);
    /// "OK".
    pub const OK: u8 = offset(9);
}

/// The secondary address that stands for none.
pub const NO_SECONDARY: u8 = 0xFF;

// The devices below the screen told apart by a shift: the keyboard's 0
// leaves 0 and shifts out 0, the tape's 1 shifts out 1, RS-232's 2 leaves 1.
const _: () = assert!(
    device::KEYBOARD == 0 && device::TAPE == 1 && device::RS232 == 2 && device::SCREEN == 3,
    "the devices a shift tells apart"
);
const _: () = assert!(message::SAVING != 0, "SAVING's offset loads with Z clear");

const _: () = assert!(
    fixed::CLALL + 4 == fixed::CLRCHN,
    "CLALL runs on into CLRCHN"
);

/// CLALL at its address: every file forgotten, then on into CLRCHN.
pub fn pin(a: &mut Asm) {
    a.pin(fixed::CLALL, |a| {
        a.lda(imm(0));
        a.sta(z(zp::LDTND));
        a.jmp("clrchn");
    });
}

pub fn emit(a: &mut Asm) {
    settings(a);
    table(a);
    open(a);
    close(a);
    channels(a);
    clrchn(a);
    characters(a);
    load(a);
    load_serial(a);
    save(a);
    messages(a);
}

/// SETLFS, SETNAM, READST, SETMSG, MEMTOP, MEMBOT and IOBASE.
fn settings(a: &mut Asm) {
    a.here("setlfs");
    a.sta(z(zp::LA));
    a.stx(z(zp::FA));
    a.sty(z(zp::SA));
    a.rts();

    a.here("setnam");
    a.sta(z(zp::FNLEN));
    a.stx(z(zp::FNADR));
    a.sty(z(zp::FNADR + 1));
    a.rts();

    // RS-232's own status, read and cleared, for its device.
    a.here("readst");
    let other = a.local();
    a.lda(z(zp::FA));
    a.cmp(imm(device::RS232));
    a.bne(other);
    a.lda(abs(page::RSSTAT));
    a.pha();
    a.lda(imm(0));
    a.sta(abs(page::RSSTAT));
    a.pla();
    a.rts();
    a.bind(other);
    a.lda(z(zp::STATUS));
    a.rts();

    a.here("setmsg");
    a.sta(z(zp::MSGFLG));
    a.rts();

    // With C clear the pointer set from X and Y, then read back.
    for (name, pointer) in [("memtop", page::MEMSIZ), ("membot", page::MEMSTR)] {
        a.here(name);
        let read = a.local();
        a.bcs(read);
        a.stx(abs(pointer));
        a.sty(abs(pointer + 1));
        a.bind(read);
        a.ldx(abs(pointer));
        a.ldy(abs(pointer + 1));
        a.rts();
    }

    a.here("iobase");
    let [low, high] = io::CIA1.to_le_bytes();
    a.ldx(imm(low));
    a.ldy(imm(high));
    a.rts();
}

/// The open files: file A found at index X with Z set, and its entry made
/// the current file. `find_fresh_file` clears STATUS first, as OPEN, CHKIN
/// and CHKOUT begin and CLOSE does not; Y is kept.
fn table(a: &mut Asm) {
    a.here("find_fresh_file");
    a.ldx(imm(0));
    a.stx(z(zp::STATUS));
    a.here("find_file");
    let search = a.local();
    a.ldx(z(zp::LDTND));
    a.bind(search);
    a.dex();
    a.bmi(RTS);
    a.cmp(abs(page::LAT).x());
    a.bne(search);
    a.rts();

    a.here("set_file");
    a.lda(abs(page::LAT).x());
    a.sta(z(zp::LA));
    a.lda(abs(page::FAT).x());
    a.sta(z(zp::FA));
    a.lda(abs(page::SAT).x());
    a.sta(z(zp::SA));
    a.rts();

    // Each error's entry loads its number, reading past the others'; A's
    // error: the channels cleared, "I/O ERROR #" shown where MSGFLG's bit 6
    // asks for it, and C set.
    let [(first, number), rest @ ..] = ERRORS;
    a.here(first);
    a.lda(imm(number));
    for (name, number) in rest {
        a.skip_over(|a| {
            a.here(name);
            a.lda(imm(number));
        });
    }
    a.here("io_error");
    let quiet = a.local();
    a.pha();
    a.jsr("CLRCHN");
    a.bit(z(zp::MSGFLG));
    a.bvc(quiet);
    a.ldy(imm(message::IO_ERROR));
    a.jsr("print_message");
    a.pla();
    a.pha();
    a.ora(imm(b'0'));
    a.jsr("screen_out");
    a.bind(quiet);
    a.pla();
    a.sec();
    a.rts();
}

/// The KERNAL's errors' entries, by number.
const ERRORS: [(&str, u8); 10] = [
    ("error_stopped", error::STOPPED),
    ("error_file_open", error::FILE_OPEN),
    ("error_too_many_files", error::TOO_MANY_FILES),
    ("error_file_not_open", error::FILE_NOT_OPEN),
    ("error_file_not_found", error::FILE_NOT_FOUND),
    ("error_not_present", error::DEVICE_NOT_PRESENT),
    ("error_not_input", error::NOT_INPUT_FILE),
    ("error_not_output", error::NOT_OUTPUT_FILE),
    ("error_missing_name", error::MISSING_FILE_NAME),
    ("error_illegal_device", error::ILLEGAL_DEVICE),
];

/// OPEN: the file's number, device and secondary address entered in the
/// table, then the device's own opening. A serial device given a secondary
/// address and a name is sent the name to open; one that does not answer
/// leaves its entry made, as the original's does.
fn open(a: &mut Asm) {
    a.here("open");
    let (opened, serial, rs232) = (a.local(), a.local(), a.local());
    a.lda(z(zp::LA));
    a.beq("error_not_input");
    a.jsr("find_fresh_file");
    a.beq("error_file_open");
    a.ldx(z(zp::LDTND));
    a.cpx(imm(MAX_FILES));
    a.bcs("error_too_many_files");
    a.inc(z(zp::LDTND));
    // A the file's number still.
    a.sta(abs(page::LAT).x());
    a.lda(z(zp::SA));
    a.ora(imm(command::DATA));
    a.sta(z(zp::SA));
    a.sta(abs(page::SAT).x());
    a.lda(z(zp::FA));
    a.sta(abs(page::FAT).x());
    a.beq(opened);
    a.cmp(imm(device::SCREEN));
    a.beq(opened);
    a.bcs(serial);
    a.lsr(Acc);
    a.bcc(rs232);
    a.jmp("tape_open");
    a.bind(rs232);
    a.jmp("rs232_open");
    a.bind(opened);
    a.clc();
    a.rts();
    a.bind(serial);
    a.lda(z(zp::SA));
    a.bmi(opened);
    a.ldy(z(zp::FNLEN));
    a.beq(opened);
    a.ora(imm(command::OPEN));
    a.fall_into("open_channel");

    // LISTEN, the channel command in A, the name, and UNLISTEN; C set
    // with the error where the device is absent: no device answers ATN,
    // or none listens for the name, the others on the bus having let DATA
    // go. Y is 0 through LISTEN and SECOND for the name.
    a.here("open_channel");
    let (send, named, listened) = (a.local(), a.local(), a.local());
    a.ldy(imm(0));
    a.sty(z(zp::STATUS));
    a.jsr("listen_channel");
    a.lda(z(zp::STATUS));
    a.bmi("error_not_present");
    a.bind(send);
    a.cpy(z(zp::FNLEN));
    a.beq(named);
    a.lda(z(zp::FNADR).ind_y());
    a.jsr("ciout");
    a.iny();
    a.bne(send);
    a.bind(named);
    a.jsr("unlsn");
    a.lda(z(zp::STATUS));
    a.bpl(listened);
    a.jmp("error_not_present");
    a.bind(listened);
    a.clc();
    a.rts();

    // LISTEN, A's channel closing command, and UNLISTEN; C clear, as at
    // `no_error`.
    a.here("close_command");
    a.jsr("listen_channel");
    a.jsr("unlsn");
    a.here("no_error");
    a.clc();
    a.rts();

    // LISTEN, then the channel command in A.
    a.here("listen_channel");
    a.pha();
    a.lda(z(zp::FA));
    a.jsr("listen");
    a.pla();
    a.jmp("second");
}

/// CLOSE: file A's entry taken out, the last moved into its place, then
/// its device closing it; a file not open closes without an error, A kept,
/// and so does a tape file read; a screen or serial file returns A 0.
fn close(a: &mut Asm) {
    a.here("close");
    let (serial, rs232, closed) = (a.local(), a.local(), a.local());
    a.jsr("find_file");
    a.bne("no_error");
    a.jsr("set_file");
    a.dec(z(zp::LDTND));
    a.ldy(z(zp::LDTND));
    for table in [page::LAT, page::FAT, page::SAT] {
        a.lda(abs(table).y());
        a.sta(abs(table).x());
    }
    a.lda(z(zp::FA));
    a.beq("no_error");
    a.cmp(imm(device::SCREEN));
    a.beq(closed);
    a.bcs(serial);
    // C clear for the tape.
    a.cmp(imm(device::RS232));
    a.beq(rs232);
    a.jmp("tape_close");
    a.bind(rs232);
    a.jmp("rs232_close");
    a.bind(serial);
    a.lda(z(zp::SA));
    a.bmi(closed);
    a.ora(imm(command::CLOSE));
    a.jsr("close_command");
    a.bind(closed);
    a.lda(imm(0));
    a.clc();
    a.rts();
}

/// CHKIN and CHKOUT: file X made the input or the output, a serial device
/// told to talk or to listen on its channel, TKSA and SECOND in halves;
/// RS-232's receiver listening once its file is the input.
fn channels(a: &mut Asm) {
    let (not_open, absent) = (a.local(), a.local());

    a.here("chkin");
    let (found, talker, no_channel, present, device_in, tape) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.txa();
    a.jsr("find_fresh_file");
    a.beq(found);
    a.bind(not_open);
    a.jmp("error_file_not_open");
    a.bind(found);
    a.jsr("set_file");
    a.lda(z(zp::FA));
    a.beq(device_in);
    a.cmp(imm(device::SCREEN));
    a.beq(device_in);
    a.bcs(talker);
    a.cmp(imm(device::TAPE));
    a.beq(tape);
    a.jsr("rs232_listen");
    a.lda(z(zp::FA));
    a.bne(device_in);
    a.bind(tape);
    a.ldx(z(zp::SA));
    a.cpx(imm(command::DATA));
    a.beq(device_in);
    a.jmp("error_not_input");
    a.bind(talker);
    a.jsr("talk");
    a.lda(z(zp::SA));
    a.bmi(no_channel);
    a.jsr("send_a");
    a.bind(no_channel);
    a.jsr("turnaround");
    a.lda(z(zp::STATUS));
    a.bpl(present);
    a.bind(absent);
    a.jmp("error_not_present");
    a.bind(present);
    a.lda(z(zp::FA));
    a.bind(device_in);
    a.sta(z(zp::DFLTN));
    a.clc();
    a.rts();

    a.here("chkout");
    let (not_output, listener, no_channel, device_out) =
        (a.local(), a.local(), a.local(), a.local());
    a.txa();
    a.jsr("find_fresh_file");
    a.bne(not_open);
    a.jsr("set_file");
    a.lda(z(zp::FA));
    a.beq(not_output);
    a.cmp(imm(device::SCREEN));
    a.beq(device_out);
    a.bcs(listener);
    a.cmp(imm(device::TAPE));
    a.bne(device_out);
    a.ldx(z(zp::SA));
    a.cpx(imm(command::DATA));
    a.bne(device_out);
    a.bind(not_output);
    a.jmp("error_not_output");
    a.bind(listener);
    a.jsr("listen");
    a.lda(z(zp::SA));
    a.bmi(no_channel);
    a.jsr("send_a");
    a.bind(no_channel);
    a.jsr("release_atn");
    a.lda(z(zp::STATUS));
    a.bmi(absent);
    a.lda(z(zp::FA));
    a.bind(device_out);
    a.sta(z(zp::DFLTO));
    a.clc();
    a.rts();
}

/// CLRCHN: a serial device unlistened and untalked, the keyboard and the
/// screen made current.
fn clrchn(a: &mut Asm) {
    a.here("clrchn");
    {
        let (output, input) = (a.local(), a.local());
        a.ldx(imm(device::SCREEN));
        a.cpx(z(zp::DFLTO));
        a.bcs(output);
        a.jsr("unlsn");
        a.bind(output);
        a.ldx(imm(device::SCREEN));
        a.cpx(z(zp::DFLTN));
        a.bcs(input);
        a.jsr("untlk");
        a.bind(input);
        a.ldx(imm(device::SCREEN));
        a.stx(z(zp::DFLTO));
        a.lda(imm(device::KEYBOARD));
        a.sta(z(zp::DFLTN));
        a.rts();
    }
}

/// GETIN, CHRIN and CHROUT by the current device, GETIN running on into
/// CHRIN for a device it does not read itself. A serial read with STATUS
/// already set gives RETURN; CHROUT keeps A.
fn characters(a: &mut Asm) {
    a.here("getin");
    let (other, waiting, rs232) = (a.local(), a.local(), a.local());
    a.lda(z(zp::DFLTN));
    a.bne(other);
    a.lda(z(zp::NDX));
    a.bne(waiting);
    a.clc();
    a.rts();
    a.bind(waiting);
    a.sei();
    a.jmp("take_key");
    a.bind(other);
    a.cmp(imm(device::RS232));
    a.beq(rs232);
    a.fall_into("chrin");

    a.here("chrin");
    let (not_keyboard, not_screen, serial, read) = (a.local(), a.local(), a.local(), a.local());
    a.lda(z(zp::DFLTN));
    a.bne(not_keyboard);
    a.jmp("editor_in");
    a.bind(not_keyboard);
    a.cmp(imm(device::SCREEN));
    a.bne(not_screen);
    a.jmp("screen_in");
    a.bind(not_screen);
    a.bcs(serial);
    a.lsr(Acc);
    a.bcc(rs232);
    a.jmp("tape_get");
    a.bind(rs232);
    a.jmp("rs232_get");
    a.bind(serial);
    a.lda(z(zp::STATUS));
    a.beq(read);
    a.lda(imm(b'\r'));
    a.clc();
    a.rts();
    a.bind(read);
    a.jmp("acptr");

    a.here("chrout");
    let (not_screen, low_device, tape, rs232) = (a.local(), a.local(), a.local(), a.local());
    a.pha();
    a.lda(z(zp::DFLTO));
    a.cmp(imm(device::SCREEN));
    a.bne(not_screen);
    a.pla();
    a.jmp("screen_out");
    a.bind(not_screen);
    a.bcc(low_device);
    a.pla();
    a.jmp("ciout");
    a.bind(low_device);
    a.lsr(Acc);
    a.bcs(tape);
    a.bne(rs232);
    // C clear: the keyboard takes nothing.
    a.pla();
    a.rts();
    a.bind(tape);
    a.pla();
    a.jmp("tape_put");
    a.bind(rs232);
    a.pla();
    a.jmp("rs232_put");
}

/// LOAD: the address in X and Y kept in MEMUSS, then ILOAD. Its default
/// loads A 0 or verifies A 1 from the tape or a serial device; a serial
/// load with secondary address 0 relocates to MEMUSS. X and Y return the
/// address past the last byte.
fn load(a: &mut Asm) {
    a.here("load");
    a.stx(z(zp::MEMUSS));
    a.sty(z(zp::MEMUSS + 1));
    a.jmp(abs(vector::ILOAD).ind());

    a.here("load_vectored");
    let (illegal, serial, named) = (a.local(), a.local(), a.local());
    a.sta(z(zp::VERCK));
    a.lda(imm(0));
    a.sta(z(zp::STATUS));
    a.lda(z(zp::FA));
    a.cmp(imm(device::SCREEN + 1));
    a.bcs(serial);
    a.cmp(imm(device::TAPE));
    a.bne(illegal);
    a.jmp("tape_load");
    a.bind(illegal);
    a.jmp("error_illegal_device");
    a.bind(serial);
    a.ldy(z(zp::FNLEN));
    a.bne(named);
    a.jmp("error_missing_name");
    a.bind(named);
    a.jsr("searching");
    a.lda(imm(command::OPEN | channel::LOAD));
    a.jsr("open_channel");
    a.bcs(RTS);
    a.fall_into("load_serial");
}

/// A load from a serial device, its file opened: the address it gives or
/// MEMUSS, then its bytes until EOI, STOP abandoning; `load_close` its
/// UNTALK and its channel closed, C clear.
fn load_serial(a: &mut Asm) {
    a.here("load_serial");
    let (not_found, address, own, bytes, go, store, next, stepped) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.lda(z(zp::FA));
    a.jsr("talk");
    a.lda(imm(command::DATA | channel::LOAD));
    a.jsr("tksa");
    a.jsr("acptr");
    a.sta(z(zp::EAL));
    a.lda(z(zp::STATUS));
    a.lsr(Acc);
    a.lsr(Acc);
    a.bcs(not_found);
    a.jsr("acptr");
    a.sta(z(zp::EAL + 1));
    // MEMUSS in its place with secondary address 0; the start the same.
    a.ldx(imm(1));
    a.bind(address);
    a.ldy(z(zp::SA));
    a.bne(own);
    a.lda(z(zp::MEMUSS).x());
    a.sta(z(zp::EAL).x());
    a.bind(own);
    a.lda(z(zp::EAL).x());
    a.sta(z(zp::STAL).x());
    a.dex();
    a.bpl(address);
    a.jsr("loading_message");
    a.bind(bytes);
    a.lda(imm(!status::READ_TIMEOUT));
    a.and(z(zp::STATUS));
    a.sta(z(zp::STATUS));
    a.jsr("STOP");
    a.bne(go);
    a.jsr("load_close");
    a.jmp("error_stopped");
    a.bind(go);
    a.jsr("acptr");
    a.tax();
    a.lda(z(zp::STATUS));
    a.lsr(Acc);
    a.lsr(Acc);
    a.bcs(bytes);
    a.txa();
    a.ldy(imm(0));
    a.ldx(z(zp::VERCK));
    a.beq(store);
    a.cmp(z(zp::EAL).ind_y());
    a.beq(next);
    a.lda(imm(status::MISMATCH));
    // STATUS is left not zero.
    a.jsr("set_status");
    a.bne(next);
    a.bind(store);
    a.sta(z(zp::EAL).ind_y());
    a.bind(next);
    a.inc(z(zp::EAL));
    a.bne(stepped);
    a.inc(z(zp::EAL + 1));
    a.bind(stepped);
    a.bit(z(zp::STATUS));
    a.bvc(bytes);
    a.jsr("load_close");
    a.ldx(z(zp::EAL));
    a.ldy(z(zp::EAL + 1));
    a.clc();
    a.rts();
    a.bind(not_found);
    a.jsr("load_close");
    a.jmp("error_file_not_found");

    a.here("load_close");
    a.jsr("untlk");
    a.lda(imm(command::CLOSE | channel::LOAD));
    a.jmp("close_command");
}

/// SAVE: the start from the pointer in page zero A names, the end in X and
/// Y, then ISAVE. Its default saves to the tape or a serial device, there
/// as the start's address and the bytes up to the end.
fn save(a: &mut Asm) {
    a.here("save");
    a.stx(z(zp::EAL));
    a.sty(z(zp::EAL + 1));
    a.tax();
    a.lda(z(0).x());
    a.sta(z(zp::STAL));
    a.lda(z(1).x());
    a.sta(z(zp::STAL + 1));
    a.jmp(abs(vector::ISAVE).ind());

    a.here("save_vectored");
    let (illegal, serial, named, send, done, stopped) = (
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
        a.local(),
    );
    a.lda(z(zp::FA));
    a.cmp(imm(device::SCREEN + 1));
    a.bcs(serial);
    a.cmp(imm(device::TAPE));
    a.bne(illegal);
    a.jmp("tape_save");
    a.bind(illegal);
    a.jmp("error_illegal_device");
    a.bind(serial);
    a.lda(z(zp::FNLEN));
    a.bne(named);
    a.jmp("error_missing_name");
    // SAVING once the device has answered, as the original shows it.
    a.bind(named);
    a.lda(imm(command::OPEN | channel::SAVE));
    a.jsr("open_channel");
    a.bcs(RTS);
    a.jsr("saving_message");
    a.lda(imm(command::DATA | channel::SAVE));
    a.jsr("listen_channel");
    a.lda(z(zp::STAL));
    a.sta(z(zp::SAL));
    a.jsr("ciout");
    a.lda(z(zp::STAL + 1));
    a.sta(z(zp::SAL + 1));
    a.jsr("ciout");
    a.ldy(imm(0));
    a.bind(send);
    a.lda(z(zp::SAL));
    a.cmp(z(zp::EAL));
    a.lda(z(zp::SAL + 1));
    a.sbc(z(zp::EAL + 1));
    a.bcs(done);
    a.lda(z(zp::SAL).ind_y());
    a.jsr("ciout");
    a.jsr("STOP");
    a.beq(stopped);
    a.inc(z(zp::SAL));
    a.bne(send);
    a.inc(z(zp::SAL + 1));
    a.bne(send);
    // UNLISTEN, then the save's channel closed; C clear.
    a.bind(done);
    a.jsr("unlsn");
    a.lda(imm(command::CLOSE | channel::SAVE));
    a.jmp("close_command");
    a.bind(stopped);
    a.jsr(done);
    a.jmp("error_stopped");
}

/// The messages: one printed from its offset, or only where MSGFLG's
/// bit 7 shows control messages, and the ones a load and a save show.
fn messages(a: &mut Asm) {
    a.here("loading_message");
    let shown = a.local();
    a.ldy(imm(message::LOADING));
    a.lda(z(zp::VERCK));
    a.beq(shown);
    a.ldy(imm(message::VERIFYING));
    a.bind(shown);
    a.fall_into("control_message");

    a.here("control_message");
    let printed = a.local();
    a.bit(z(zp::MSGFLG));
    a.bpl(printed);
    a.fall_into("print_message");

    a.here("print_message");
    let next = a.local();
    a.bind(next);
    a.lda(abs("message_table").y());
    a.pha();
    a.and(imm(0x7F));
    a.jsr("screen_out");
    a.iny();
    a.pla();
    a.bpl(next);
    a.bind(printed);
    a.clc();
    a.rts();

    // "SEARCHING", and "FOR" and the name where there is one; "SAVING"
    // and the name.
    a.here("searching");
    let with_name = a.local();
    a.bit(z(zp::MSGFLG));
    a.bpl(RTS);
    a.ldy(imm(message::SEARCHING));
    a.jsr("print_message");
    a.lda(z(zp::FNLEN));
    a.beq(RTS);
    a.ldy(imm(message::FOR));
    a.bind(with_name);
    a.jsr("print_message");
    a.here("print_name");
    let (name, done) = (a.local(), a.local());
    a.ldy(imm(0));
    a.bind(name);
    a.cpy(z(zp::FNLEN));
    a.beq(done);
    a.lda(z(zp::FNADR).ind_y());
    a.jsr("screen_out");
    a.iny();
    a.bne(name);
    a.bind(done);
    a.rts();

    a.here("saving_message");
    a.bit(z(zp::MSGFLG));
    a.bpl(RTS);
    a.ldy(imm(message::SAVING));
    a.bne(with_name);

    a.later(|a| {
        a.here("message_table");
        for text in MESSAGES {
            if let Some((&last, body)) = text.as_bytes().split_last() {
                a.bytes(body);
                a.byte(last | 0x80);
            }
        }
    });
}
