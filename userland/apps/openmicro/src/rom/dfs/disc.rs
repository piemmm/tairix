//! The 8271: its initialisation, the commands every filing system call
//! makes through it, the transfers between disc and memory with their
//! retries, and OSWORD &7D, &7E and &7F.
//!
//! The chip is programmed as DFS 1.20 programs it, so a drive steps, settles
//! and loads its head in the same time: 24 ms steps, 20 ms settling, the
//! head loaded in 64 ms and unloaded after twelve idle revolutions, no bad
//! tracks, and a byte through an NMI at a time.

use super::errors::DISK_FAULT;
use super::nmi::{Held, Nmi, RESULT, STATUS};
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
/// A drive's `CALIBRATED` once its head has found track 0. Zero is not yet
/// since the chip was programmed; bit 7 clear otherwise is found stopped.
const CALIBRATED_YES: u8 = 0xFF;
/// DFS 1.20 gives a drive found stopped 48 ms, its head loaded, before
/// sending the head to track 0: this pause's outer and inner counts.
const PAUSE_OUTER: u8 = 72;
const PAUSE_INNER: u8 = 254;

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
    oswords(a, &nmi.held);
}

/// `fdc_run`: disc command A with X parameters from `page::PARAMS`, run to
/// the result the NMI takes, returned in A. `fdc_immediate`: an immediate
/// command so, its result returned. `fdc_begin`, then `fdc_parameter` for
/// each parameter in A and `fdc_await`, runs a disc command whose
/// parameters are held elsewhere.
fn issue(a: &mut Asm) {
    a.here("fdc_run");
    a.stx(zp(z::T0));
    a.jsr("fdc_begin");
    a.jsr("fdc_parameters");
    a.here("fdc_await");
    a.lda(zp(z::NMI_RESULT));
    a.bmi("fdc_await");
    a.rts();

    a.here("fdc_immediate");
    a.stx(zp(z::T0));
    a.jsr("fdc_command");
    a.jsr("fdc_parameters");
    let busy = a.here_local();
    a.lda(abs(STATUS));
    a.bmi(busy);
    a.lda(abs(RESULT));
    a.rts();

    // The command in A given once the chip takes one; X is lost. A disc
    // command's result is first set running.
    a.here("fdc_begin");
    a.ldy(imm(0xFF));
    a.sty(zp(z::NMI_RESULT));
    a.here("fdc_command");
    a.tax();
    let idle = a.here_local();
    a.lda(abs(STATUS));
    a.and(imm(status::BUSY | status::COMMAND_FULL));
    a.bne(idle);
    a.stx(abs(COMMAND));
    a.rts();

    // T0 parameters from `page::PARAMS`.
    a.here("fdc_parameters");
    a.ldy(imm(0));
    let next = a.here_local();
    a.cpy(zp(z::T0));
    a.beq(RTS);
    a.lda(abs(page::PARAMS).y());
    a.jsr("fdc_parameter");
    a.iny();
    a.bne(next);

    // The parameter in A given once the chip takes one; X is lost.
    a.here("fdc_parameter");
    a.tax();
    let full = a.here_local();
    a.lda(abs(STATUS));
    a.and(imm(status::PARAMETER_FULL));
    a.bne(full);
    a.stx(abs(PARAMETER));
    a.rts();
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
    a.sty(abs(nmi.held.previous));
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
    a.ldy(abs(nmi.held.previous));
    a.jmp(abs(entry::OSBYTE));

    a.here("nmi_code");
    a.bytes(&nmi.bytes);
}

/// `select_drive`: the drive in `OP_DRIVE` selected with its head loaded,
/// waited for until a disc turns in it, which ESCAPE abandons, and its head
/// sent to find track 0 if it has not since the chip was programmed, or
/// after DFS 1.20's pause if the drive has been found stopped since. Leaves
/// the drive's select bits in `OP_SELECT`. `select_ready` selects it and
/// waits for the disc; `select_only` selects it and waits for nothing.
fn selecting(a: &mut Asm) {
    a.here("select_drive");
    a.jsr("select_ready");
    a.ldx(abs(page::OP_PHYSICAL));
    a.lda(abs(page::CALIBRATED).x());
    a.bmi(RTS);
    let uncalibrated = a.local();
    a.beq(uncalibrated);
    a.jsr("spin_up_pause");
    a.ldx(abs(page::OP_PHYSICAL));
    a.bind(uncalibrated);
    a.lda(imm(CALIBRATED_YES));
    a.sta(abs(page::CALIBRATED).x());
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
    found_stopped(a);
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

    // The head out to track 0: `recalibrate` the selected drive's,
    // `recalibrate_drive` that of the drive whose select bits are in A.
    a.here("recalibrate");
    a.lda(abs(page::OP_SELECT));
    a.here("recalibrate_drive");
    a.ora(imm(command::SEEK));
    a.jsr("fdc_begin");
    a.lda(imm(0));
    a.jsr("fdc_parameter");
    a.jmp("fdc_await");

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

    a.here("spin_up_pause");
    a.ldy(imm(PAUSE_OUTER));
    a.here("spin_up_outer");
    a.ldx(imm(PAUSE_INNER));
    a.here("spin_up_inner");
    a.dex();
    a.bne("spin_up_inner");
    a.dey();
    a.bne("spin_up_outer");
    a.rts();
}

/// The drive at `OP_PHYSICAL`, in X, found stopped: bit 7 of `CALIBRATED`
/// cleared, which leaves one never calibrated as it is.
fn found_stopped(a: &mut Asm) {
    a.asl(abs(page::CALIBRATED).x());
    a.lsr(abs(page::CALIBRATED).x());
}

/// `transfer`: the sectors `OP_SECTOR` on, `OP_SECTORS` of them, between the
/// disc in `OP_DRIVE` and memory at `OP_ADDRESS`, a read storing
/// `OP_STORE` bytes of them; one command a track. A command that fails is
/// tried again, and a write-protected disc or a fault is an error. The
/// request is held before any data moves, since a read may land on this
/// workspace.
fn transfer(a: &mut Asm, nmi: &Nmi) {
    let held = &nmi.held;
    a.here("transfer");
    a.jsr("claim_nmi");
    a.jsr("select_drive");
    a.lda(abs(page::OP_SELECT));
    a.sta(abs(held.select));
    a.lda(abs(page::OP_WRITE));
    a.sta(abs(held.write));
    a.ldx(imm(page::OP_RUN_LEN - 1));
    let copy = a.here_local();
    a.lda(abs(page::OP_SECTORS).x());
    a.sta(abs(held.sectors).x());
    a.dex();
    a.bpl(copy);
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
    a.stx(abs(held.track));
    a.lda(zp(z::T2));
    a.sta(abs(held.sector));

    transfer_tracks(a, held);
    transfer_attempt(a, held);
    routing(a, nmi);
    catalogue_io(a);
}

/// Each track's command in turn, tried again on failure.
fn transfer_tracks(a: &mut Asm, held: &Held) {
    a.here("transfer_next");
    a.lda(abs(held.sectors));
    a.ora(abs(held.sectors + 1));
    let next = a.local();
    a.bne(next);
    a.jmp("release_nmi");
    a.bind(next);
    // As many sectors as are left on the track, or wanted.
    a.lda(imm(SECTORS_PER_TRACK));
    a.sec();
    a.sbc(abs(held.sector));
    a.ldx(abs(held.sectors + 1));
    let counted = a.local();
    a.bne(counted);
    a.cmp(abs(held.sectors));
    a.bcc(counted);
    a.lda(abs(held.sectors));
    a.bind(counted);
    a.sta(abs(held.count));
    // A read stores what is wanted of them.
    a.sta(abs(held.bytes + 1));
    a.lda(imm(0));
    a.sta(abs(held.bytes));
    a.lda(abs(held.wanted + 2));
    let whole = a.local();
    a.bne(whole);
    a.lda(abs(held.wanted + 1));
    a.cmp(abs(held.count));
    a.bcs(whole);
    a.sta(abs(held.bytes + 1));
    a.lda(abs(held.wanted));
    a.sta(abs(held.bytes));
    a.bind(whole);
    a.lda(imm(ATTEMPTS));
    a.sta(abs(held.attempts));
    a.fall_into("transfer_attempt");
}

/// One track's command, tried again on failure, then the next track. Its
/// parameters go to the chip from the held state.
fn transfer_attempt(a: &mut Asm, held: &Held) {
    a.here("transfer_attempt");
    a.jsr("nmi_route");
    a.ldy(abs(held.write));
    a.lda(abs("transfer_commands").y());
    a.ora(abs(held.select));
    a.jsr("fdc_begin");
    a.lda(abs(held.track));
    a.jsr("fdc_parameter");
    a.lda(abs(held.sector));
    a.jsr("fdc_parameter");
    a.lda(abs(held.count));
    a.ora(imm(SECTOR_256));
    a.jsr("fdc_parameter");
    a.jsr("fdc_await");
    let done = a.local();
    a.beq(done);
    a.cmp(imm(result::WRITE_PROTECTED));
    let protected = a.local();
    a.beq(protected);
    a.dec(abs(held.attempts));
    let fault = a.local();
    a.beq(fault);
    a.lda(abs(held.attempts));
    a.cmp(imm(1));
    let again = a.local();
    a.bne(again);
    a.lda(abs(held.select));
    a.jsr("recalibrate_drive");
    a.bind(again);
    a.jmp("transfer_attempt");
    a.bind(protected);
    a.jsr("release_nmi");
    a.jmp("err_read_only");

    // "Disk fault" with the 8271's result in A, at the command's track and
    // first sector: built while the NMI page still holds them, raised once
    // the NMI is given back.
    a.bind(fault);
    a.pha();
    a.lda(imm(DISK_FAULT));
    a.jsr("error_start");
    a.jsr("error_text");
    a.text("Disk fault ");
    a.byte(0);
    a.pla();
    a.jsr("error_hex");
    a.jsr("error_text");
    a.text(" at ");
    a.byte(0);
    a.lda(abs(held.track));
    a.jsr("error_hex");
    a.lda(imm(b'/'));
    a.jsr("error_char");
    a.lda(abs(held.sector));
    a.jsr("error_hex");
    a.txa();
    a.pha();
    a.jsr("release_nmi");
    a.pla();
    a.tax();
    a.jmp("error_raise");

    // On past the sectors moved.
    a.bind(done);
    a.lda(abs(held.address + 1));
    a.clc();
    a.adc(abs(held.count));
    a.sta(abs(held.address + 1));
    a.lda(abs(held.wanted));
    a.sec();
    a.sbc(abs(held.bytes));
    a.sta(abs(held.wanted));
    a.lda(abs(held.wanted + 1));
    a.sbc(abs(held.bytes + 1));
    a.sta(abs(held.wanted + 1));
    let borrowed = a.local();
    a.bcs(borrowed);
    a.dec(abs(held.wanted + 2));
    a.bind(borrowed);
    a.lda(abs(held.sectors));
    a.sec();
    a.sbc(abs(held.count));
    a.sta(abs(held.sectors));
    let same = a.local();
    a.bcs(same);
    a.dec(abs(held.sectors + 1));
    a.bind(same);
    a.lda(abs(held.sector));
    a.clc();
    a.adc(abs(held.count));
    a.cmp(imm(SECTORS_PER_TRACK));
    let on_track = a.local();
    a.bcc(on_track);
    a.inc(abs(held.track));
    a.lda(imm(0));
    a.bind(on_track);
    a.sta(abs(held.sector));
    a.jmp("transfer_next");
}

/// The NMI routine routed for a command, and the catalogue's own transfers.
fn routing(a: &mut Asm, nmi: &Nmi) {
    let held = &nmi.held;
    a.here("transfer_commands");
    a.bytes(&[command::READ, command::WRITE]);

    // The NMI routine routed for the command in hand: a write fetching from
    // the held address, or a read storing the held bytes there and
    // discarding the rest.
    a.here("nmi_route");
    a.lda(abs(held.address));
    a.ldy(abs(held.address + 1));
    a.ldx(abs(held.write));
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
    a.lda(abs(held.bytes));
    a.sta(zp(z::NMI_COUNT));
    a.ldx(imm(lo(nmi.read)));
    a.ldy(imm(hi(nmi.read)));
    a.ora(abs(held.bytes + 1));
    let storing = a.local();
    a.bne(storing);
    a.ldx(imm(lo(nmi.discard)));
    a.ldy(imm(hi(nmi.discard)));
    a.bind(storing);
    a.lda(abs(held.bytes + 1));
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
    let stopped = a.local();
    a.beq(stopped);
    a.rts();
    a.bind(stopped);
    found_stopped(a);
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
fn oswords(a: &mut Asm, held: &Held) {
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

    osword_command(a, held);
}

/// OSWORD &7F, and the Escape a wait can end in.
fn osword_command(a: &mut Asm, held: &Held) {
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
    // The address the command moves data at, from block offset 1.
    a.ldy(imm(1));
    a.lda(zp(z::BLOCK).ind_y());
    a.sta(abs(held.address));
    a.iny();
    a.lda(zp(z::BLOCK).ind_y());
    a.sta(abs(held.address + 1));
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
    a.sta(abs(held.bytes));
    a.sta(abs(held.bytes + 1));
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
    a.sty(abs(held.write));
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
