//! INT 13h for the diskette drives, driving the µPD765A as the PC's BIOS
//! does: the motor and its start time, recalibrate and seek, DMA channel 2,
//! the command, its interrupt and its result. Each step that waits for the
//! controller's interrupt or the motor hands back to the stub, which halts
//! until the next interrupt. The diskette parameter table INT 1Eh points at
//! sets the timing and the format. A high-density adapter's media are found
//! by trying each data rate the drive can read, and a disk-change line
//! reports a new disk before it is read.

use super::bda;
use super::disk_status as status;
use super::guest::Guest;
use super::layout::{diskette, table, SEGMENT};
use crate::asm::{Reg16, Reg8};
use crate::chips::dma::channel_setup;
use crate::cpu::{flag, Seg};
use crate::storage::fdc;
use tairix_fat::format::FIXED_DRIVE;

/// The stub's state: the stage, recalibrate tries left, the tick a wait
/// gives up at, and the media rate being tried.
pub const STATE: u8 = 8;
const STAGE: u16 = 0;
const TRIES: u16 = 1;
const DEADLINE: u16 = 2;
const CANDIDATE: u16 = 4;
/// Where the CPU's flags lie above the stack pointer.
const FLAGS: u16 = STATE as u16 + 4;

mod stage {
    pub const RESET: u8 = 1;
    pub const MOTOR: u8 = 2;
    pub const CHANGE: u8 = 3;
    pub const CHANGE_BACK: u8 = 4;
    pub const RECALIBRATE: u8 = 5;
    pub const SEEK: u8 = 6;
    pub const TRANSFER: u8 = 7;
}

/// The controller's ports.
const DOR: u16 = 0x3F2;
const MSR: u16 = 0x3F4;
const DATA: u16 = 0x3F5;
const DIR: u16 = 0x3F7;

/// The interrupt has come, in the seek status.
const INTERRUPTED: u8 = 0x80;
/// Two seconds for the controller to interrupt.
const TIMEOUT_TICKS: u16 = 37;

/// The media state at `40:90`: its data rate in bits 6–7, double stepping
/// and established, and the candidate it came from.
mod media {
    pub const DOUBLE_STEP: u8 = 0x20;
    pub const ESTABLISHED: u8 = 0x10;
}

/// What a drive type can hold, as INT 13h numbers the types: the rate to
/// try (as the CCR takes it), whether it is double stepped, its sectors a
/// track and last cylinder, and its parameters' slot in the ROM.
#[derive(Clone, Copy)]
struct Medium {
    rate: u8,
    double: bool,
    sectors: u8,
    last_cylinder: u8,
    slot: u16,
}

/// Each medium's diskette parameters, in their ROM slots.
pub const MEDIA_PARAMETERS: [[u8; diskette::LEN as usize]; 6] = [
    [
        0xDF, 0x02, 0x25, 0x02, 0x09, 0x2A, 0xFF, 0x50, 0xF6, 0x0F, 0x08,
    ],
    [
        0xDF, 0x02, 0x25, 0x02, 0x0F, 0x1B, 0xFF, 0x54, 0xF6, 0x0F, 0x08,
    ],
    [
        0xDF, 0x02, 0x25, 0x02, 0x09, 0x2A, 0xFF, 0x50, 0xF6, 0x0F, 0x08,
    ],
    [
        0xAF, 0x02, 0x25, 0x02, 0x12, 0x1B, 0xFF, 0x6C, 0xF6, 0x0F, 0x08,
    ],
    [
        0xDF, 0x02, 0x25, 0x02, 0x09, 0x23, 0xFF, 0x50, 0xF6, 0x0F, 0x08,
    ],
    [
        0xDF, 0x02, 0x25, 0x02, 0x09, 0x2A, 0xFF, 0x50, 0xF6, 0x0F, 0x08,
    ],
];

const fn media(kind: u8) -> &'static [Medium] {
    const DD525: Medium = Medium {
        rate: 2,
        double: false,
        sectors: 9,
        last_cylinder: 39,
        slot: 0,
    };
    const HD525: Medium = Medium {
        rate: 0,
        double: false,
        sectors: 15,
        last_cylinder: 79,
        slot: 1,
    };
    const DD35: Medium = Medium {
        rate: 2,
        double: false,
        sectors: 9,
        last_cylinder: 79,
        slot: 2,
    };
    const HD35: Medium = Medium {
        rate: 0,
        double: false,
        sectors: 18,
        last_cylinder: 79,
        slot: 3,
    };
    const DD_IN_HD525: Medium = Medium {
        rate: 1,
        double: true,
        sectors: 9,
        last_cylinder: 39,
        slot: 4,
    };
    const DD_IN_HD35: Medium = Medium { slot: 5, ..DD35 };
    match kind {
        1 => &[DD525],
        2 => &[HD525, DD_IN_HD525],
        3 => &[DD35],
        4 => &[HD35, DD_IN_HD35],
        _ => &[],
    }
}

/// The fields of the diskette parameters INT 1Eh points at that the BIOS
/// uses.
#[derive(Clone, Copy)]
struct Parameters {
    specify: [u8; 2],
    motor_off: u8,
    size_code: u8,
    last_sector: u8,
    gap: u8,
    data_length: u8,
    format_gap: u8,
    fill: u8,
    motor_start: u8,
}

fn parameters(g: &mut Guest<'_>) -> Parameters {
    let vector = u16::from(diskette::VECTOR) * 4;
    let (segment, offset) = (g.read16(0, vector + 2), g.read16(0, vector));
    let mut field = |at: u16| g.read8(segment, offset.wrapping_add(at));
    Parameters {
        specify: [field(diskette::SPECIFY), field(diskette::SPECIFY + 1)],
        motor_off: field(diskette::MOTOR_OFF),
        size_code: field(diskette::SIZE_CODE),
        last_sector: field(diskette::LAST_SECTOR),
        gap: field(diskette::GAP),
        data_length: field(diskette::DATA_LENGTH),
        format_gap: field(diskette::FORMAT_GAP),
        fill: field(diskette::FILL),
        motor_start: field(diskette::MOTOR_START),
    }
}

/// Drive `n`'s type from the ROM's table, 0 for none.
fn drive_type(g: &mut Guest<'_>, n: u8) -> u8 {
    if n > 3 {
        return 0;
    }
    g.read8(SEGMENT, table::DRIVES + 1 + u16::from(n))
}

fn high_density(g: &mut Guest<'_>) -> bool {
    g.read8(SEGMENT, table::DRIVES) != 0
}

/// Whether drive `n` reports a new disk on the change line.
fn change_line(g: &mut Guest<'_>, n: u8) -> bool {
    high_density(g) && drive_type(g, n) >= 2
}

/// Waits a little for the controller to ask for the next byte, the way a
/// BIOS polls it.
fn ready(g: &mut Guest<'_>, to_host: bool) -> bool {
    for _ in 0..100 {
        let msr = g.input(MSR);
        if msr & 0x80 != 0 {
            return (msr & 0x40 != 0) == to_host;
        }
        g.spend(10_000);
    }
    false
}

fn send(g: &mut Guest<'_>, bytes: &[u8]) -> bool {
    bytes.iter().all(|&byte| {
        let ok = ready(g, false);
        if ok {
            g.output(DATA, byte);
        }
        ok
    })
}

fn receive<const N: usize>(g: &mut Guest<'_>) -> Option<[u8; N]> {
    let mut out = [0; N];
    for byte in &mut out {
        if !ready(g, true) {
            return None;
        }
        *byte = g.input(DATA);
    }
    Some(out)
}

fn sense(g: &mut Guest<'_>) -> Option<[u8; 2]> {
    if !send(g, &[0x08]) {
        return None;
    }
    receive::<2>(g)
}

/// Whether the controller has interrupted since the flag was cleared, which
/// clears it.
fn interrupted(g: &mut Guest<'_>) -> bool {
    let seek = g.bda8(bda::SEEK_STATUS);
    if seek & INTERRUPTED == 0 {
        return false;
    }
    g.set_bda8(bda::SEEK_STATUS, seek & !INTERRUPTED);
    true
}

/// Waits for the controller's interrupt in `stage`, two seconds at most.
/// The command was just sent, and the stub masks interrupts while the
/// service runs, so a flag already set is a stale one.
fn await_interrupt(g: &mut Guest<'_>, stage: u8) {
    interrupted(g);
    let deadline = g.bda16(bda::TICKS).wrapping_add(TIMEOUT_TICKS);
    wait(g, stage, deadline);
}

fn wait(g: &mut Guest<'_>, stage: u8, deadline: u16) {
    g.set_local(STAGE, stage);
    let [low, high] = deadline.to_le_bytes();
    g.set_local(DEADLINE, low);
    g.set_local(DEADLINE + 1, high);
    g.done(false);
}

fn past_deadline(g: &mut Guest<'_>) -> bool {
    let deadline = u16::from_le_bytes([g.local(DEADLINE), g.local(DEADLINE + 1)]);
    g.bda16(bda::TICKS).wrapping_sub(deadline) < 0x8000
}

/// Ends the call: the status in AH and the data area, the carry if it
/// failed, and the motors left to time out.
fn conclude(g: &mut Guest<'_>, code: u8) {
    g.set_bda8(bda::DISKETTE_STATUS, code);
    g.set_r8(Reg8::Ah, code);
    g.set_returned_flag(FLAGS, flag::CF, code != 0);
    let timeout = parameters(g).motor_off;
    g.set_bda8(bda::MOTOR_COUNT, timeout);
    let motors = g.bda8(bda::MOTOR_STATUS);
    g.set_bda8(bda::MOTOR_STATUS, motors & 0x7F);
    g.done(true);
}

/// Ends a transfer, with the sectors it moved in AL.
fn conclude_transfer(g: &mut Guest<'_>, code: u8, sectors: u8) {
    g.set_r8(Reg8::Al, sectors);
    conclude(g, code);
}

/// Answers a call with registers set and no status kept.
fn answer(g: &mut Guest<'_>, code: u8) {
    g.set_r8(Reg8::Ah, code);
    g.set_returned_flag(FLAGS, flag::CF, code != 0);
    g.done(true);
}

/// INT 0Eh: the controller interrupted.
pub fn interrupt(g: &mut Guest<'_>) {
    let seek = g.bda8(bda::SEEK_STATUS);
    g.set_bda8(bda::SEEK_STATUS, seek | INTERRUPTED);
    g.output(0x20, 0x20);
}

/// INT 13h's first call.
pub fn start(g: &mut Guest<'_>) {
    g.set_local(CANDIDATE, 0);
    let drive = g.r8(Reg8::Dl);
    match g.r8(Reg8::Ah) {
        0x00 => reset(g),
        0x01 => {
            let last = g.bda8(bda::DISKETTE_STATUS);
            g.set_r8(Reg8::Al, last);
            answer(g, last);
        }
        0x02..=0x05 => transfer(g, drive),
        0x08 => drive_parameters(g, drive),
        0x15 => {
            let kind = match drive_type(g, drive) {
                0 => 0,
                _ if change_line(g, drive) => 2,
                _ => 1,
            };
            g.set_r8(Reg8::Ah, kind);
            g.set_returned_flag(FLAGS, flag::CF, false);
            g.done(true);
        }
        0x16 => {
            let changed = !change_line(g, drive) || {
                motor_on(g, drive, false);
                g.input(DIR) & 0x80 != 0
            };
            let code = if changed { status::CHANGED } else { 0 };
            answer(g, code);
        }
        0x17 => {
            let chosen = match (g.r8(Reg8::Al), drive_type(g, drive)) {
                (1, 1) | (4, 3 | 4) => media(drive_type(g, drive)).iter().position(|m| m.rate == 2),
                (2, 2) => Some(1),
                (3, 2) => Some(0),
                _ => None,
            };
            match chosen {
                Some(index) => {
                    establish(g, drive, index);
                    answer(g, 0);
                }
                None => answer(g, status::INVALID),
            }
        }
        0x18 => match format_medium(g, drive) {
            None => answer(g, status::BAD_MEDIA),
            // A drive with a change line is asked whether it holds a disk.
            Some(_) if change_line(g, drive) => spin_up(g, drive, false),
            Some(index) => set_media(g, drive, index),
        },
        _ => answer(g, status::INVALID),
    }
}

/// The type's medium AH 18h's CH and CL describe, as an index into its
/// media.
fn format_medium(g: &mut Guest<'_>, drive: u8) -> Option<usize> {
    let last = g.r8(Reg8::Ch);
    let sectors = g.r8(Reg8::Cl) & 0x3F;
    media(drive_type(g, drive))
        .iter()
        .position(|m| m.last_cylinder == last && m.sectors == sectors)
}

/// AH 18h's medium is established and its parameters handed back; a change
/// the line reported is taken as seen.
fn set_media(g: &mut Guest<'_>, drive: u8, index: usize) {
    establish(g, drive, index);
    let kind = drive_type(g, drive);
    point_at_parameters(g, kind, index);
    conclude(g, 0);
}

/// AH 18h found a disk in the drive: its medium is set.
fn media_present(g: &mut Guest<'_>, drive: u8) {
    match format_medium(g, drive) {
        Some(index) => set_media(g, drive, index),
        None => conclude(g, status::BAD_MEDIA),
    }
}

/// Records drive `drive`'s medium as the type's `index`th.
fn establish(g: &mut Guest<'_>, drive: u8, index: usize) {
    let Some(medium) = media(drive_type(g, drive)).get(index).copied() else {
        return;
    };
    let double = if medium.double { media::DOUBLE_STEP } else { 0 };
    let index = u8::try_from(index).unwrap_or(0);
    g.set_bda8(
        bda::MEDIA_STATE + u16::from(drive & 3),
        (medium.rate << 6) | double | media::ESTABLISHED | index,
    );
}

/// ES:DI to the ROM's parameters for the type's `index`th medium.
fn point_at_parameters(g: &mut Guest<'_>, kind: u8, index: usize) {
    let slot = media(kind).get(index).map_or(0, |medium| medium.slot);
    g.regs_mut().set_sreg(Seg::Es, SEGMENT);
    g.set_r16(Reg16::Di, table::MEDIA + slot * 11);
}

fn drive_parameters(g: &mut Guest<'_>, drive: u8) {
    if drive >= FIXED_DRIVE {
        // A fixed-disk BIOS answers for these, and none took the call: there
        // are no fixed disks.
        g.set_r8(Reg8::Dl, 0);
        return answer(g, status::INVALID);
    }
    let count = (0..4).filter(|&n| drive_type(g, n) != 0).count();
    let count = u8::try_from(count).unwrap_or(0);
    let kind = drive_type(g, drive);
    let Some(medium) = media(kind).first().copied() else {
        for reg in [Reg16::Ax, Reg16::Bx, Reg16::Cx] {
            g.set_r16(reg, 0);
        }
        g.set_r16(Reg16::Dx, u16::from(count));
        g.set_returned_flag(FLAGS, flag::CF, false);
        g.done(true);
        return;
    };
    g.set_r16(Reg16::Bx, u16::from(kind));
    g.set_r16(
        Reg16::Cx,
        u16::from_le_bytes([medium.sectors, medium.last_cylinder]),
    );
    g.set_r16(Reg16::Dx, u16::from_le_bytes([count, 1]));
    point_at_parameters(g, kind, 0);
    g.set_r16(Reg16::Ax, 0);
    g.set_returned_flag(FLAGS, flag::CF, false);
    g.done(true);
}

/// AH 00h: resets the controller, keeping its motors, and specifies it.
fn reset(g: &mut Guest<'_>) {
    let motors = g.bda8(bda::MOTOR_STATUS) & 0x0F;
    g.output(DOR, (motors << 4) | 0x08);
    g.output(DOR, (motors << 4) | 0x0C);
    let seek = g.bda8(bda::SEEK_STATUS);
    g.set_bda8(bda::SEEK_STATUS, seek & 0x70);
    await_interrupt(g, stage::RESET);
}

fn reset_done(g: &mut Guest<'_>) {
    let sensed = (0..4u8).all(|unit| sense(g).is_some_and(|[st0, _]| st0 == 0xC0 | unit));
    let [step, load] = parameters(g).specify;
    if !sensed || !send(g, &[0x03, step, load]) {
        return conclude(g, status::CONTROLLER);
    }
    if high_density(g) {
        let rate = g.bda8(bda::DATA_RATE) >> 6;
        g.output(DIR, rate);
    }
    conclude(g, 0);
}

/// Turns drive `drive`'s motor on and selects it; whether it was already
/// turning.
fn motor_on(g: &mut Guest<'_>, drive: u8, writes: bool) -> bool {
    let motors = g.bda8(bda::MOTOR_STATUS) & 0x0F;
    let bit = 1 << (drive & 3);
    g.output(DOR, 0x0C | (drive & 3) | ((motors | bit) << 4));
    g.set_bda8(
        bda::MOTOR_STATUS,
        motors | bit | if writes { 0x80 } else { 0 },
    );
    let timeout = parameters(g).motor_off;
    g.set_bda8(bda::MOTOR_COUNT, timeout);
    motors & bit != 0
}

/// AH 02h–05h: read, write, verify and format.
fn transfer(g: &mut Guest<'_>, drive: u8) {
    if drive > 3 {
        return conclude_transfer(g, status::INVALID, 0);
    }
    if drive_type(g, drive) == 0 {
        return conclude_transfer(g, status::TIMEOUT, 0);
    }
    let (count, bytes) = dma_extent(g);
    let linear = (u32::from(g.seg(Seg::Es)) << 4) + u32::from(g.r16(Reg16::Bx));
    if count == 0 || (linear & 0xFFFF) + bytes > 0x1_0000 {
        return conclude_transfer(
            g,
            if count == 0 {
                status::INVALID
            } else {
                status::DMA_BOUNDARY
            },
            0,
        );
    }
    let writes = matches!(g.r8(Reg8::Ah), 0x03 | 0x05);
    spin_up(g, drive, writes);
}

/// Brings drive `drive`'s motor to speed, then goes on from
/// [`motor_ready`].
fn spin_up(g: &mut Guest<'_>, drive: u8, writes: bool) {
    let turning = motor_on(g, drive, writes);
    g.set_bda8(bda::MOTOR_COUNT, 0xFF);
    if turning {
        return motor_ready(g, drive);
    }
    let start = parameters(g).motor_start;
    // The motor start time is in eighths of a second; a tick is 1/18.2 s.
    let ticks = (u16::from(start) * 91).div_ceil(40);
    let deadline = g.bda16(bda::TICKS).wrapping_add(ticks.max(1));
    wait(g, stage::MOTOR, deadline);
}

/// The DMA count and bytes the request moves: sectors of the table's size,
/// or a format's four bytes a sector.
fn dma_extent(g: &mut Guest<'_>) -> (u8, u32) {
    let parameters = parameters(g);
    if g.r8(Reg8::Ah) == 0x05 {
        let sectors = parameters.last_sector;
        return (sectors, u32::from(sectors) * 4);
    }
    let count = g.r8(Reg8::Al);
    let size = 128u32 << parameters.size_code.min(3);
    (count, u32::from(count) * size)
}

/// The motor is up to speed: a changed disk is reported, else the data
/// rate is chosen and the head brought to the cylinder — or AH 18h's
/// medium set, the drive holding a disk.
fn motor_ready(g: &mut Guest<'_>, drive: u8) {
    if change_line(g, drive) && g.input(DIR) & 0x80 != 0 {
        g.set_bda8(bda::MEDIA_STATE + u16::from(drive), 0);
        let seek = g.bda8(bda::SEEK_STATUS);
        g.set_bda8(bda::SEEK_STATUS, seek & !(1 << drive));
        // A step with a disk in clears the line.
        if !send(g, &[0x0F, drive, 1]) {
            return conclude_transfer(g, status::CONTROLLER, 0);
        }
        return await_interrupt(g, stage::CHANGE);
    }
    if g.r8(Reg8::Ah) == 0x18 {
        return media_present(g, drive);
    }
    choose_rate(g, drive);
    if g.bda8(bda::SEEK_STATUS) & (1 << drive) == 0 {
        g.set_local(TRIES, 2);
        return recalibrate(g, drive);
    }
    seek(g, drive);
}

/// The medium being tried: the established one, or the next to try.
fn medium(g: &mut Guest<'_>, drive: u8) -> Medium {
    let kind = drive_type(g, drive);
    let state = g.bda8(bda::MEDIA_STATE + u16::from(drive));
    let index = if state & media::ESTABLISHED != 0 {
        usize::from(state & 0x07)
    } else {
        usize::from(g.local(CANDIDATE))
    };
    media(kind)
        .get(index)
        .or_else(|| media(kind).first())
        .copied()
        .unwrap_or(Medium {
            rate: 2,
            double: false,
            sectors: 9,
            last_cylinder: 39,
            slot: 0,
        })
}

fn choose_rate(g: &mut Guest<'_>, drive: u8) {
    if !high_density(g) {
        return;
    }
    let rate = medium(g, drive).rate;
    if g.bda8(bda::DATA_RATE) >> 6 != rate {
        g.output(DIR, rate);
        g.set_bda8(bda::DATA_RATE, rate << 6);
    }
}

fn recalibrate(g: &mut Guest<'_>, drive: u8) {
    if !send(g, &[0x07, drive]) {
        return conclude_transfer(g, status::CONTROLLER, 0);
    }
    await_interrupt(g, stage::RECALIBRATE);
}

fn recalibrated(g: &mut Guest<'_>, drive: u8) {
    match sense(g) {
        Some([st0, 0]) if st0 & 0xF0 == 0x20 => {
            let status = g.bda8(bda::SEEK_STATUS);
            g.set_bda8(bda::SEEK_STATUS, status | (1 << drive));
            set_cylinder(g, drive, 0);
            seek(g, drive);
        }
        // An 80-track drive's head can be further from track 0 than one
        // recalibrate's pulses reach.
        Some(_) if g.local(TRIES) > 1 => {
            let tries = g.local(TRIES);
            g.set_local(TRIES, tries - 1);
            recalibrate(g, drive);
        }
        _ => conclude_transfer(g, status::SEEK, 0),
    }
}

/// The cylinder the data area records for drives 0 and 1; the others are
/// always sought.
fn cylinder(g: &mut Guest<'_>, drive: u8) -> Option<u8> {
    (drive < 2).then(|| g.bda8(bda::CYLINDER + u16::from(drive)))
}

fn set_cylinder(g: &mut Guest<'_>, drive: u8, cylinder: u8) {
    if drive < 2 {
        g.set_bda8(bda::CYLINDER + u16::from(drive), cylinder);
    }
}

fn physical_cylinder(g: &mut Guest<'_>, drive: u8) -> u8 {
    let cylinder = g.r8(Reg8::Ch);
    if medium(g, drive).double {
        cylinder.wrapping_mul(2)
    } else {
        cylinder
    }
}

fn seek(g: &mut Guest<'_>, drive: u8) {
    let target = physical_cylinder(g, drive);
    if cylinder(g, drive) == Some(target) {
        return command(g, drive);
    }
    let head = g.r8(Reg8::Dh) & 1;
    if !send(g, &[0x0F, (head << 2) | drive, target]) {
        return conclude_transfer(g, status::CONTROLLER, 0);
    }
    await_interrupt(g, stage::SEEK);
}

fn sought(g: &mut Guest<'_>, drive: u8) {
    let target = physical_cylinder(g, drive);
    match sense(g) {
        Some([st0, pcn]) if st0 & 0x60 == 0x20 && pcn == target => {
            set_cylinder(g, drive, target);
            command(g, drive);
        }
        _ => conclude_transfer(g, status::SEEK, 0),
    }
}

/// Programs DMA channel 2 for the request and sends the command.
fn command(g: &mut Guest<'_>, drive: u8) {
    let function = g.r8(Reg8::Ah);
    let (_, bytes) = dma_extent(g);
    let linear = (u32::from(g.seg(Seg::Es)) << 4) + u32::from(g.r16(Reg16::Bx));
    let mode = match function {
        0x02 => 0x46,
        0x04 => 0x42,
        _ => 0x4A,
    };
    let last = u16::try_from(bytes.saturating_sub(1)).unwrap_or(u16::MAX);
    for (port, value) in channel_setup(fdc::DMA_CHANNEL, mode, linear, last) {
        g.output(port, value);
    }
    let p = parameters(g);
    let head = g.r8(Reg8::Dh) & 1;
    let unit = (head << 2) | drive;
    let (c, r) = (g.r8(Reg8::Ch), g.r8(Reg8::Cl) & 0x3F);
    let sent = match function {
        0x05 => send(
            g,
            &[0x4D, unit, p.size_code, p.last_sector, p.format_gap, p.fill],
        ),
        0x03 => send(
            g,
            &[
                0xC5,
                unit,
                c,
                head,
                r,
                p.size_code,
                p.last_sector,
                p.gap,
                p.data_length,
            ],
        ),
        _ => send(
            g,
            &[
                0xE6,
                unit,
                c,
                head,
                r,
                p.size_code,
                p.last_sector,
                p.gap,
                p.data_length,
            ],
        ),
    };
    if !sent {
        return conclude_transfer(g, status::CONTROLLER, 0);
    }
    await_interrupt(g, stage::TRANSFER);
}

/// The command's result, as INT 13h reports it, and the sectors it moved.
fn transferred(g: &mut Guest<'_>, drive: u8) {
    let Some(result) = receive::<7>(g) else {
        return conclude_transfer(g, status::CONTROLLER, 0);
    };
    for (i, &byte) in (0u16..).zip(&result) {
        g.set_bda8(bda::FDC_RESULT + i, byte);
    }
    let [st0, st1, ..] = result;
    let code = if st0 & 0xC0 == 0 {
        0
    } else if st1 & 0x80 != 0 {
        status::NOT_FOUND
    } else if st1 & 0x20 != 0 {
        status::CRC
    } else if st1 & 0x10 != 0 {
        status::DMA_OVERRUN
    } else if st1 & 0x04 != 0 {
        status::NOT_FOUND
    } else if st1 & 0x02 != 0 {
        status::WRITE_PROTECTED
    } else if st1 & 0x01 != 0 {
        status::NO_ADDRESS_MARK
    } else {
        status::CONTROLLER
    };
    let established = g.bda8(bda::MEDIA_STATE + u16::from(drive)) & media::ESTABLISHED != 0;
    let next = usize::from(g.local(CANDIDATE)) + 1;
    let reads = matches!(g.r8(Reg8::Ah), 0x02 | 0x04);
    if code != 0 && !established && reads && next < media(drive_type(g, drive)).len() {
        g.set_local(CANDIDATE, u8::try_from(next).unwrap_or(0));
        choose_rate(g, drive);
        return seek(g, drive);
    }
    if code == 0 && !established {
        let index = usize::from(g.local(CANDIDATE));
        establish(g, drive, index);
    }
    let moved = if code == 0 {
        if g.r8(Reg8::Ah) == 0x05 {
            0
        } else {
            g.r8(Reg8::Al)
        }
    } else {
        let sectors = parameters(g).last_sector;
        let (c, h, r) = (g.r8(Reg8::Ch), g.r8(Reg8::Dh), g.r8(Reg8::Cl) & 0x3F);
        let into = |cyl: u8, head: u8, record: u8| {
            (u16::from(cyl) * 2 + u16::from(head & 1)) * u16::from(sectors) + u16::from(record)
        };
        let moved = into(result[3], result[4], result[5]).saturating_sub(into(c, h, r));
        u8::try_from(moved).unwrap_or(0)
    };
    conclude_transfer(g, code, moved);
}

/// INT 13h's later calls: the stage the stub waited in goes on, or gives up.
pub fn step(g: &mut Guest<'_>) {
    let drive = g.r8(Reg8::Dl) & 3;
    let stage = g.local(STAGE);
    if stage == stage::MOTOR {
        if past_deadline(g) {
            return motor_ready(g, drive);
        }
        return g.done(false);
    }
    if !interrupted(g) {
        if past_deadline(g) {
            let seek = g.bda8(bda::SEEK_STATUS);
            g.set_bda8(bda::SEEK_STATUS, seek & 0x70);
            return conclude_transfer(g, status::TIMEOUT, 0);
        }
        return g.done(false);
    }
    match stage {
        stage::RESET => reset_done(g),
        // A seek to the cylinder the head is on steps nothing, so the line
        // is tried again after a seek that must.
        stage::CHANGE | stage::CHANGE_BACK => {
            sense(g);
            set_cylinder(g, drive, u8::from(stage == stage::CHANGE));
            match (g.input(DIR) & 0x80 != 0, stage) {
                (false, _) if g.r8(Reg8::Ah) == 0x18 => media_present(g, drive),
                (false, _) => conclude_transfer(g, status::CHANGED, 0),
                (true, stage::CHANGE) if send(g, &[0x0F, drive, 0]) => {
                    await_interrupt(g, stage::CHANGE_BACK);
                }
                _ => conclude_transfer(g, status::TIMEOUT, 0),
            }
        }
        stage::RECALIBRATE => recalibrated(g, drive),
        stage::SEEK => sought(g, drive),
        stage::TRANSFER => transferred(g, drive),
        _ => conclude_transfer(g, status::CONTROLLER, 0),
    }
}
