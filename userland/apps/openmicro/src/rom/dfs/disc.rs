//! The 8271: its initialisation, the commands every filing system call
//! makes through it, the transfers between disc and memory with their
//! retries, and OSWORD &7D, &7E and &7F.
//!
//! The chip is programmed as DFS 1.20 programs it, so a drive steps, settles
//! and loads its head in the same time: 24 ms steps, 20 ms settling, the
//! head loaded in 64 ms and unloaded after twelve idle revolutions, no bad
//! tracks, and a byte through an NMI at a time.

use super::nmi::{Nmi, RESULT, STATUS};
use super::ws::{catalogue, page, zp as z};
use crate::machine::bbc::fdc::{input, output, register, result, status};
use crate::rom::asm::{abs, byte, hi, imm, lo, sym, zp, Asm, RTS};
use crate::rom::os::{entry, osbyte, page_d, service, ESCAPE_FLAG};

/// The command register, written where the status is read.
const COMMAND: u16 = STATUS;
/// The parameter register, written where the result is read.
const PARAMETER: u16 = RESULT;

/// Commands, before a drive's select bits are added.
mod command {
    pub const WRITE: u8 = 0x0B;
    pub const READ: u8 = 0x13;
    pub const SEEK: u8 = 0x29;
    pub const DRIVE_STATUS: u8 = 0x2C;
    pub const SPECIFY: u8 = 0x35;
    pub const WRITE_SPECIAL: u8 = 0x3A;
    /// Commands from here act at once, ending with no interrupt.
    pub const IMMEDIATE: u8 = 0x2C;
    /// The bits that name a command's kind, and the kinds that fetch their
    /// data from memory: writes, deleted-data writes and formats.
    pub const GROUP: u8 = 0x3C;
    pub const FETCHING: [u8; 3] = [0x08, 0x0C, 0x20];
}

/// A sector-count parameter's size field for 256-byte sectors.
const SECTOR_256: u8 = 0x20;
/// Sectors a track holds.
pub(super) const SECTORS_PER_TRACK: u8 = 10;
/// Attempts at a command before it is a fault; the head finds track 0 again
/// before the last.
const ATTEMPTS: u8 = 3;
/// The mode register: its two must-be-one bits, both actuators, non-DMA.
const MODE_NON_DMA: u8 = 0xC1;
/// The parameters a command may take.
const PARAMETERS: u8 = 5;
/// No drive's catalogue held at &0E00.
pub(super) const NO_CATALOGUE: u8 = 0xFF;

/// The 8271's initialisation, as each command and its parameters.
const INITIALISE: [(u8, &[u8]); 4] = [
    (command::SPECIFY, &[register::STEP_RATE, 0x0C, 0x0A, 0xC8]),
    (
        command::SPECIFY,
        &[register::BAD_TRACKS_0, 0xFF, 0xFF, 0x00],
    ),
    (
        command::SPECIFY,
        &[register::BAD_TRACKS_1, 0xFF, 0xFF, 0x00],
    ),
    (command::WRITE_SPECIAL, &[register::MODE, MODE_NON_DMA]),
];

pub(super) fn emit(a: &mut Asm, nmi: &Nmi) {
    issue(a);
    initialise(a);
    claiming(a, nmi);
    selecting(a);
    transfer(a, nmi);
    oswords(a);
}

/// `fdc_issue`: command A with X parameters from `page::PARAMS`, each given
/// as the chip takes it. `fdc_immediate` then waits for an immediate one
/// and returns its result in A; `fdc_run` sets the NMI's result running and
/// waits for a disc command's.
fn issue(a: &mut Asm) {
    a.here("fdc_run");
    a.ldy(imm(0xFF));
    a.sty(zp(z::NMI_RESULT));
    a.jsr("fdc_issue");
    let running = a.here_local();
    a.lda(zp(z::NMI_RESULT));
    a.bmi(running);
    a.rts();

    a.here("fdc_immediate");
    a.jsr("fdc_issue");
    let busy = a.here_local();
    a.lda(abs(STATUS));
    a.bmi(busy);
    a.lda(abs(RESULT));
    a.rts();

    a.here("fdc_issue");
    a.stx(zp(z::T0));
    a.tax();
    let idle = a.here_local();
    a.lda(abs(STATUS));
    a.and(imm(status::BUSY | status::COMMAND_FULL));
    a.bne(idle);
    a.stx(abs(COMMAND));
    a.ldy(imm(0));
    let next = a.here_local();
    a.cpy(zp(z::T0));
    a.beq(RTS);
    let full = a.here_local();
    a.lda(abs(STATUS));
    a.and(imm(status::PARAMETER_FULL));
    a.bne(full);
    a.lda(abs(page::PARAMS).y());
    a.sta(abs(PARAMETER));
    a.iny();
    a.bne(next);
}

/// `fdc_initialise`: the chip programmed, and both drives' heads to find
/// track 0 before their next command. The table holds each command, its
/// parameter count and its parameters.
fn initialise(a: &mut Asm) {
    a.here("fdc_initialise");
    a.ldx(imm(0));
    let command = a.here_local();
    a.lda(abs("fdc_table").x());
    a.pha();
    a.lda(abs(sym("fdc_table") + 1).x());
    a.sta(zp(z::T1));
    a.inx();
    a.inx();
    a.ldy(imm(0));
    let parameter = a.here_local();
    a.cpy(zp(z::T1));
    let copied = a.local();
    a.beq(copied);
    a.lda(abs("fdc_table").x());
    a.sta(abs(page::PARAMS).y());
    a.inx();
    a.iny();
    a.bne(parameter);
    a.bind(copied);
    a.stx(zp(z::T2));
    a.pla();
    a.ldx(zp(z::T1));
    a.jsr("fdc_immediate");
    a.ldx(zp(z::T2));
    a.cpx(imm(byte(table_len())));
    a.bne(command);
    a.lda(imm(0));
    a.sta(abs(page::CALIBRATED));
    a.sta(abs(page::CALIBRATED + 1));
    a.lda(imm(NO_CATALOGUE));
    a.sta(abs(page::CAT_DRIVE));
    a.rts();

    a.here("fdc_table");
    for (opcode, parameters) in INITIALISE {
        a.byte(opcode);
        a.byte(byte(parameters.len()));
        a.bytes(parameters);
    }
}

/// The initialisation table's length: each command, its parameter count,
/// then its parameters.
const fn table_len() -> usize {
    let mut len = 0;
    let mut i = 0;
    while i < INITIALISE.len() {
        len += INITIALISE[i].1.len() + 2;
        i += 1;
    }
    len
}

/// `claim_nmi` and `release_nmi`: the NMI routine taken from whoever had it
/// and copied to &0D00, and given back.
fn claiming(a: &mut Asm, nmi: &Nmi) {
    a.here("claim_nmi");
    a.lda(imm(osbyte::SERVICE));
    a.ldx(imm(service::NMI_CLAIM));
    a.ldy(imm(0xFF));
    a.jsr(entry::OSBYTE);
    a.sty(abs(page::NMI_PREVIOUS));
    a.ldx(imm(byte(nmi.bytes.len() - 1)));
    let copy = a.here_local();
    a.lda(abs("nmi_code").x());
    a.sta(abs(page_d::NMI).x());
    a.dex();
    a.bpl(copy);
    a.rts();

    a.here("release_nmi");
    a.lda(imm(osbyte::SERVICE));
    a.ldx(imm(service::NMI_RELEASE));
    a.ldy(abs(page::NMI_PREVIOUS));
    a.jmp(abs(entry::OSBYTE));

    a.here("nmi_code");
    a.bytes(&nmi.bytes);
}

/// `select_drive`: the drive in `OP_DRIVE` selected with its head loaded,
/// waited for until a disc turns in it, which ESCAPE abandons, and its head
/// sent to find track 0 if it has not since the chip was programmed. Leaves
/// the drive's select bits in `OP_SELECT`. `select_ready` selects it and
/// waits for the disc; `select_only` selects it and waits for nothing.
fn selecting(a: &mut Asm) {
    a.here("select_drive");
    a.jsr("select_ready");
    a.ldx(abs(page::OP_PHYSICAL));
    a.lda(abs(page::CALIBRATED).x());
    let uncalibrated = a.local();
    a.beq(uncalibrated);
    a.rts();
    a.bind(uncalibrated);
    a.dec(abs(page::CALIBRATED).x());
    a.jmp("recalibrate");

    a.here("select_ready");
    a.jsr("select_only");
    let wait = a.here_local();
    a.lda(imm(command::DRIVE_STATUS));
    a.ora(abs(page::OP_SELECT));
    a.ldx(imm(0));
    a.jsr("fdc_immediate");
    a.ldx(abs(page::OP_PHYSICAL));
    a.and(abs("ready_bits").x());
    let ready = a.local();
    a.bne(ready);
    // A drive found not ready may hold another disc now.
    a.lda(imm(NO_CATALOGUE));
    a.sta(abs(page::CAT_DRIVE));
    // Called from OSWORD, interrupts are off: they are let in, for the
    // keyboard to raise an Escape.
    a.php();
    a.cli();
    a.nop();
    a.plp();
    a.bit(zp(ESCAPE_FLAG));
    a.bpl(wait);
    a.jsr("release_nmi");
    a.jmp("escape");
    a.bind(ready);
    a.rts();

    // The head out to track 0.
    a.here("recalibrate");
    a.lda(imm(0));
    a.sta(abs(page::PARAMS));
    a.lda(imm(command::SEEK));
    a.ora(abs(page::OP_SELECT));
    a.ldx(imm(1));
    a.jmp("fdc_run");

    a.here("select_only");
    a.lda(abs(page::OP_DRIVE));
    a.and(imm(1));
    a.sta(abs(page::OP_PHYSICAL));
    a.tax();
    a.lda(abs("select_bits").x());
    a.sta(abs(page::OP_SELECT));
    a.ora(imm(output::LOAD_HEAD));
    a.tay();
    a.lda(abs(page::OP_DRIVE));
    a.and(imm(2));
    let side = a.local();
    a.beq(side);
    a.tya();
    a.ora(imm(output::SIDE));
    a.tay();
    a.bind(side);
    a.sty(abs(page::PARAMS + 1));
    a.lda(imm(register::DRIVE_OUTPUT));
    a.sta(abs(page::PARAMS));
    a.lda(imm(command::WRITE_SPECIAL));
    a.ldx(imm(2));
    a.jmp("fdc_immediate");

    a.here("select_bits");
    a.bytes(&[output::SELECT_0, output::SELECT_1]);
    a.here("ready_bits");
    a.bytes(&[input::READY_0, input::READY_1]);
}

/// `transfer`: the sectors `OP_SECTOR` on, `OP_SECTORS` of them, between the
/// disc in `OP_DRIVE` and memory at `OP_ADDRESS`, a read storing
/// `OP_STORE` bytes of them; one command a track. A command that fails is
/// tried again, and a write-protected disc or a fault is an error.
fn transfer(a: &mut Asm, nmi: &Nmi) {
    a.here("transfer");
    a.jsr("claim_nmi");
    a.jsr("select_drive");
    // The track and sector: the first sector divided by ten.
    a.lda(abs(page::OP_SECTOR));
    a.sta(zp(z::T2));
    a.lda(abs(page::OP_SECTOR + 1));
    a.sta(zp(z::T3));
    a.ldx(imm(0));
    let divide = a.here_local();
    a.lda(zp(z::T2));
    a.sec();
    a.sbc(imm(SECTORS_PER_TRACK));
    a.tay();
    a.lda(zp(z::T3));
    a.sbc(imm(0));
    let divided = a.local();
    a.bcc(divided);
    a.sta(zp(z::T3));
    a.sty(zp(z::T2));
    a.inx();
    a.bne(divide);
    a.bind(divided);
    a.stx(abs(page::OP_TRACK));
    a.lda(zp(z::T2));
    a.sta(abs(page::OP_SECTOR_ON_TRACK));

    transfer_tracks(a);
    routing(a, nmi);
    catalogue_io(a);
}

/// Each track's command in turn, tried again on failure.
fn transfer_tracks(a: &mut Asm) {
    a.here("transfer_next");
    a.lda(abs(page::OP_SECTORS));
    a.ora(abs(page::OP_SECTORS + 1));
    let next = a.local();
    a.bne(next);
    a.jmp("release_nmi");
    a.bind(next);
    // As many sectors as are left on the track, or wanted.
    a.lda(imm(SECTORS_PER_TRACK));
    a.sec();
    a.sbc(abs(page::OP_SECTOR_ON_TRACK));
    a.ldx(abs(page::OP_SECTORS + 1));
    let counted = a.local();
    a.bne(counted);
    a.cmp(abs(page::OP_SECTORS));
    a.bcc(counted);
    a.lda(abs(page::OP_SECTORS));
    a.bind(counted);
    a.sta(abs(page::OP_COUNT));
    // A read stores what is wanted of them.
    a.sta(abs(page::OP_BYTES + 1));
    a.lda(imm(0));
    a.sta(abs(page::OP_BYTES));
    a.lda(abs(page::OP_STORE + 2));
    let whole = a.local();
    a.bne(whole);
    a.lda(abs(page::OP_STORE + 1));
    a.cmp(abs(page::OP_COUNT));
    a.bcs(whole);
    a.sta(abs(page::OP_BYTES + 1));
    a.lda(abs(page::OP_STORE));
    a.sta(abs(page::OP_BYTES));
    a.bind(whole);
    a.lda(imm(ATTEMPTS));
    a.sta(abs(page::OP_ATTEMPTS));

    transfer_attempt(a);
}

/// One track's command, tried again on failure, then the next track.
fn transfer_attempt(a: &mut Asm) {
    a.here("transfer_attempt");
    a.jsr("nmi_route");
    a.lda(abs(page::OP_TRACK));
    a.sta(abs(page::PARAMS));
    a.lda(abs(page::OP_SECTOR_ON_TRACK));
    a.sta(abs(page::PARAMS + 1));
    a.lda(abs(page::OP_COUNT));
    a.ora(imm(SECTOR_256));
    a.sta(abs(page::PARAMS + 2));
    a.ldy(abs(page::OP_WRITE));
    a.lda(abs("transfer_commands").y());
    a.ora(abs(page::OP_SELECT));
    a.ldx(imm(3));
    a.jsr("fdc_run");
    let done = a.local();
    a.beq(done);
    a.cmp(imm(result::WRITE_PROTECTED));
    let protected = a.local();
    a.beq(protected);
    a.dec(abs(page::OP_ATTEMPTS));
    let fault = a.local();
    a.beq(fault);
    a.lda(abs(page::OP_ATTEMPTS));
    a.cmp(imm(1));
    let again = a.local();
    a.bne(again);
    a.jsr("recalibrate");
    a.bind(again);
    a.jmp("transfer_attempt");
    a.bind(protected);
    a.jsr("release_nmi");
    a.jmp("err_read_only");
    a.bind(fault);
    a.pha();
    a.jsr("release_nmi");
    a.pla();
    a.jmp("disk_fault");

    // On past the sectors moved.
    a.bind(done);
    a.lda(abs(page::OP_ADDRESS + 1));
    a.clc();
    a.adc(abs(page::OP_COUNT));
    a.sta(abs(page::OP_ADDRESS + 1));
    a.lda(abs(page::OP_STORE));
    a.sec();
    a.sbc(abs(page::OP_BYTES));
    a.sta(abs(page::OP_STORE));
    a.lda(abs(page::OP_STORE + 1));
    a.sbc(abs(page::OP_BYTES + 1));
    a.sta(abs(page::OP_STORE + 1));
    let borrowed = a.local();
    a.bcs(borrowed);
    a.dec(abs(page::OP_STORE + 2));
    a.bind(borrowed);
    a.lda(abs(page::OP_SECTORS));
    a.sec();
    a.sbc(abs(page::OP_COUNT));
    a.sta(abs(page::OP_SECTORS));
    let same = a.local();
    a.bcs(same);
    a.dec(abs(page::OP_SECTORS + 1));
    a.bind(same);
    a.lda(abs(page::OP_SECTOR_ON_TRACK));
    a.clc();
    a.adc(abs(page::OP_COUNT));
    a.cmp(imm(SECTORS_PER_TRACK));
    let on_track = a.local();
    a.bcc(on_track);
    a.inc(abs(page::OP_TRACK));
    a.lda(imm(0));
    a.bind(on_track);
    a.sta(abs(page::OP_SECTOR_ON_TRACK));
    a.jmp("transfer_next");
}

/// The NMI routine routed for a command, and the catalogue's own transfers.
fn routing(a: &mut Asm, nmi: &Nmi) {
    a.here("transfer_commands");
    a.bytes(&[command::READ, command::WRITE]);

    // The NMI routine routed for the command in hand: a write fetching from
    // OP_ADDRESS, or a read storing OP_BYTES there and discarding the rest.
    a.here("nmi_route");
    a.lda(abs(page::OP_ADDRESS));
    a.ldy(abs(page::OP_ADDRESS + 1));
    a.ldx(abs(page::OP_WRITE));
    let read = a.local();
    a.beq(read);
    a.sta(abs(nmi.fetch));
    a.sty(abs(nmi.fetch + 1));
    a.ldx(imm(lo(nmi.write)));
    a.ldy(imm(hi(nmi.write)));
    let routed = a.local();
    a.bne(routed);
    a.bind(read);
    a.sta(abs(nmi.store));
    a.sty(abs(nmi.store + 1));
    a.lda(abs(page::OP_BYTES));
    a.sta(zp(z::NMI_COUNT));
    a.ldx(imm(lo(nmi.read)));
    a.ldy(imm(hi(nmi.read)));
    a.ora(abs(page::OP_BYTES + 1));
    let storing = a.local();
    a.bne(storing);
    a.ldx(imm(lo(nmi.discard)));
    a.ldy(imm(hi(nmi.discard)));
    a.bind(storing);
    a.lda(abs(page::OP_BYTES + 1));
    a.sta(zp(z::NMI_COUNT + 1));
    a.bind(routed);
    a.stx(abs(nmi.route));
    a.sty(abs(nmi.route + 1));
    a.rts();
}

/// The catalogue read, or kept while its drive stays ready, and written.
fn catalogue_io(a: &mut Asm) {
    // The catalogue of the drive in OP_DRIVE into &0E00, or out of it. One
    // already held stays while its drive has stayed ready, which the 8271's
    // latch tells: a disc taken out is noticed even if put back.
    a.here("read_catalogue");
    a.lda(abs(page::OP_DRIVE));
    a.cmp(abs(page::CAT_DRIVE));
    let read = a.local();
    a.bne(read);
    a.jsr("select_only");
    a.lda(imm(command::DRIVE_STATUS));
    a.ora(abs(page::OP_SELECT));
    a.ldx(imm(0));
    a.jsr("fdc_immediate");
    a.ldx(abs(page::OP_PHYSICAL));
    a.and(abs("ready_bits").x());
    a.beq(read);
    a.rts();
    a.bind(read);
    a.lda(imm(0));
    a.beq("catalogue_transfer");
    a.here("write_catalogue");
    a.sed();
    a.lda(abs(catalogue::CYCLE));
    a.clc();
    a.adc(imm(1));
    a.sta(abs(catalogue::CYCLE));
    a.cld();
    a.lda(imm(1));
    a.here("catalogue_transfer");
    a.sta(abs(page::OP_WRITE));
    a.lda(imm(NO_CATALOGUE));
    a.sta(abs(page::CAT_DRIVE));
    a.lda(imm(0));
    a.sta(abs(page::OP_SECTOR));
    a.sta(abs(page::OP_SECTOR + 1));
    a.sta(abs(page::OP_SECTORS + 1));
    a.sta(abs(page::OP_ADDRESS));
    a.sta(abs(page::OP_STORE));
    a.sta(abs(page::OP_STORE + 2));
    a.lda(imm(catalogue::OWN_SECTORS));
    a.sta(abs(page::OP_SECTORS));
    a.sta(abs(page::OP_STORE + 1));
    a.lda(imm(hi(catalogue::NAMES)));
    a.sta(abs(page::OP_ADDRESS + 1));
    a.jsr("transfer");
    a.lda(abs(page::OP_DRIVE));
    a.sta(abs(page::CAT_DRIVE));
    a.rts();
}

/// OSWORD &7D, &7E and &7F, the block at (BLOCK).
fn oswords(a: &mut Asm) {
    // &7D: the current drive's cycle number. &7E: its size in bytes.
    a.here("osword_cycle");
    a.jsr("current_catalogue");
    a.ldy(imm(0));
    a.lda(abs(catalogue::CYCLE));
    a.sta(zp(z::BLOCK).ind_y());
    a.rts();

    a.here("osword_size");
    a.jsr("current_catalogue");
    a.ldy(imm(0));
    a.tya();
    a.sta(zp(z::BLOCK).ind_y());
    a.iny();
    a.lda(abs(catalogue::SECTORS));
    a.sta(zp(z::BLOCK).ind_y());
    a.iny();
    a.lda(abs(catalogue::OPTION));
    a.and(imm(3));
    a.sta(zp(z::BLOCK).ind_y());
    a.iny();
    a.lda(imm(0));
    a.sta(zp(z::BLOCK).ind_y());
    a.rts();

    osword_command(a);
}

/// OSWORD &7F, and the Escape a wait can end in.
fn osword_command(a: &mut Asm) {
    // &7F: the block's command, its drive selected, with its parameters;
    // the result after them. A drive number with its top bit set is the
    // current drive.
    a.here("osword_command");
    a.ldy(imm(0));
    a.lda(zp(z::BLOCK).ind_y());
    let given = a.local();
    a.bpl(given);
    a.lda(abs(page::DRIVE));
    a.bind(given);
    a.and(imm(3));
    a.sta(abs(page::OP_DRIVE));
    a.iny();
    a.lda(zp(z::BLOCK).ind_y());
    a.sta(abs(page::OP_ADDRESS));
    a.iny();
    a.lda(zp(z::BLOCK).ind_y());
    a.sta(abs(page::OP_ADDRESS + 1));
    a.ldy(imm(5));
    a.lda(zp(z::BLOCK).ind_y());
    a.cmp(imm(PARAMETERS + 1));
    let counted = a.local();
    a.bcc(counted);
    a.lda(imm(PARAMETERS));
    a.bind(counted);
    a.sta(zp(z::T5));
    // The drive selected first, since selecting it writes a register
    // through the parameters.
    a.lda(imm(NO_CATALOGUE));
    a.sta(abs(page::CAT_DRIVE));
    a.jsr("claim_nmi");
    a.jsr("select_ready");
    // The parameters, from block offset 7.
    a.ldx(imm(0));
    let copy = a.here_local();
    a.cpx(zp(z::T5));
    let copied = a.local();
    a.beq(copied);
    a.txa();
    a.clc();
    a.adc(imm(7));
    a.tay();
    a.lda(zp(z::BLOCK).ind_y());
    a.sta(abs(page::PARAMS).x());
    a.inx();
    a.bne(copy);
    a.bind(copied);
    a.ldy(imm(6));
    a.lda(zp(z::BLOCK).ind_y());
    a.and(imm(0x3F));
    a.sta(zp(z::T6));
    a.ldx(zp(z::T5));
    a.cmp(imm(command::IMMEDIATE));
    let disc = a.local();
    a.bcc(disc);
    a.ora(abs(page::OP_SELECT));
    a.jsr("fdc_immediate");
    let finished = a.local();
    a.jmp(finished);
    // A disc command: writes and formats fetch, the rest store all they read.
    a.bind(disc);
    a.lda(imm(0xFF));
    a.sta(abs(page::OP_BYTES));
    a.sta(abs(page::OP_BYTES + 1));
    a.lda(zp(z::T6));
    a.and(imm(command::GROUP));
    a.ldy(imm(1));
    let writing = a.local();
    for group in command::FETCHING {
        a.cmp(imm(group));
        a.beq(writing);
    }
    a.dey();
    a.bind(writing);
    a.sty(abs(page::OP_WRITE));
    a.jsr("nmi_route");
    a.lda(zp(z::T6));
    a.ora(abs(page::OP_SELECT));
    a.ldx(zp(z::T5));
    a.jsr("fdc_run");
    a.bind(finished);
    a.pha();
    a.jsr("release_nmi");
    a.pla();
    a.tax();
    a.lda(zp(z::T5));
    a.clc();
    a.adc(imm(7));
    a.tay();
    a.txa();
    a.sta(zp(z::BLOCK).ind_y());
    a.rts();

    // An Escape seen while waiting: acknowledged, then the error.
    a.here("escape");
    a.lda(imm(osbyte::ACKNOWLEDGE_ESCAPE));
    a.jsr(entry::OSBYTE);
    a.jmp("err_escape");
}
