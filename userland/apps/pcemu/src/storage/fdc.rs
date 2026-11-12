//! The NEC µPD765A floppy disk controller and its drives, behind the PC
//! adapter's digital output register.
//!
//! Everything runs in emulated time. A disk turns at its drive's speed from
//! when its motor starts, a seek steps at the SPECIFY rate, and each byte of a
//! sector moves as it passes the head: over DMA channel 2, or through the data
//! register in non-DMA mode. The adapter, not the controller, selects the
//! drive: the digital output register names it and a drive answers only while
//! its motor runs, so a command's unit number picks only the cylinder register
//! and the status it uses.
//!
//! The media are sector images. Bytes a READ TRACK takes past a sector's
//! recorded data read as gap filler, and a raw image refuses, as a protected
//! disk would, a write it cannot record: a deleted-data mark, or a format
//! other than its own.

use super::floppy::{Density, Encoding, Floppy, Sector, SectorId, Track, MAX_SECTORS};
use crate::chips::dma::{Dma, DmaMemory};
use crate::cpu::OPEN_BUS;
use crate::snapshot::{restore_bytes, save_bytes, In, Out, State, Torn};
use crate::time::{Nanos, NANOS_PER_MILLISECOND as MS, NANOS_PER_SECOND};

/// The DMA channel the controller requests.
pub const DMA_CHANNEL: usize = 2;
/// The IRQ the controller raises.
pub const IRQ: u8 = 6;

/// A drive's speed is in revolutions a minute.
const MINUTE: u128 = 60 * NANOS_PER_SECOND as u128;

/// The adapter the controller sits on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Interface {
    /// The PC and XT adapter: 250 kb/s, and nothing at 3F7h.
    Standard,
    /// An adapter with the AT's data-rate select and disk-change line at
    /// 3F7h, which high-density media need.
    HighDensity,
}

/// A drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DriveKind {
    /// A 5¼" 360 KiB drive: 40 tracks at 300 RPM.
    Dd525,
    /// A 5¼" 1.2 MiB drive: 80 tracks at 360 RPM.
    Hd525,
    /// A 3½" 720 KiB drive: 80 tracks at 300 RPM.
    Dd35,
    /// A 3½" 1.44 MiB drive: 80 tracks at 300 RPM.
    Hd35,
}

impl DriveKind {
    const fn tracks(self) -> u8 {
        match self {
            Self::Dd525 => 40,
            _ => 80,
        }
    }

    /// The last track the head reaches: a couple past the nominal ones, as
    /// drives of the time allowed.
    const fn last_track(self) -> u8 {
        self.tracks() + 1
    }

    const fn rpm(self) -> u64 {
        match self {
            Self::Hd525 => 360,
            _ => 300,
        }
    }

    const fn reads_high_density(self) -> bool {
        matches!(self, Self::Hd525 | Self::Hd35)
    }

    /// A 360 KiB drive has no disk-change line.
    const fn change_line(self) -> bool {
        !matches!(self, Self::Dd525)
    }

    /// The data rate, in kb/s, a medium of `density` is read at in this
    /// drive.
    const fn rate_for(self, density: Density) -> u32 {
        match (density, self.rpm()) {
            (Density::High, _) => 500,
            (Density::Double, 360) => 300,
            (Density::Double, _) => 250,
        }
    }
}

/// The most cylinders a 48 tpi medium holds.
const NARROW_CYLINDERS: u8 = DriveKind::Dd525.last_track() + 1;

/// The main status register's bits.
mod msr {
    pub const RQM: u8 = 0x80;
    pub const DIO: u8 = 0x40;
    pub const NON_DMA: u8 = 0x20;
    pub const BUSY: u8 = 0x10;
}

/// The digital output register's bits.
mod dor {
    pub const RESET_OFF: u8 = 0x04;
    pub const DMA_IRQ: u8 = 0x08;
}

mod st0 {
    pub const ABNORMAL: u8 = 0x40;
    pub const INVALID: u8 = 0x80;
    pub const READY_CHANGED: u8 = 0xC0;
    pub const SEEK_END: u8 = 0x20;
    pub const EQUIPMENT_CHECK: u8 = 0x10;
}

mod st1 {
    pub const END_OF_CYLINDER: u8 = 0x80;
    pub const DATA_ERROR: u8 = 0x20;
    pub const OVERRUN: u8 = 0x10;
    pub const NO_DATA: u8 = 0x04;
    pub const NOT_WRITABLE: u8 = 0x02;
    pub const MISSING_MARK: u8 = 0x01;
}

mod st2 {
    pub const CONTROL_MARK: u8 = 0x40;
    pub const DATA_CRC: u8 = 0x20;
    pub const WRONG_CYLINDER: u8 = 0x10;
    pub const SCAN_HIT: u8 = 0x08;
    pub const SCAN_NOT_SATISFIED: u8 = 0x04;
    pub const BAD_CYLINDER: u8 = 0x02;
    pub const MISSING_DATA_MARK: u8 = 0x01;
}

mod st3 {
    pub const WRITE_PROTECTED: u8 = 0x40;
    pub const READY: u8 = 0x20;
    pub const TRACK_ZERO: u8 = 0x10;
    pub const TWO_SIDED: u8 = 0x08;
}

/// A track's recording, in bytes of its encoding.
struct Layout {
    /// From the index to the first sector's ID field.
    index_gap: u64,
    /// An ID field: sync, mark, C H R N and CRC.
    id_field: u64,
    /// From an ID field's start to its data's.
    data_offset: u64,
    /// Data-rate bits a byte takes: FM carries half the data at one clock.
    cells: u64,
}

const MFM: Layout = Layout {
    index_gap: 146,
    id_field: 22,
    data_offset: 60,
    cells: 8,
};
const FM: Layout = Layout {
    index_gap: 73,
    id_field: 13,
    data_offset: 31,
    cells: 16,
};

const fn layout(mfm: bool) -> &'static Layout {
    if mfm {
        &MFM
    } else {
        &FM
    }
}

/// The CRC after a data field.
const CRC: u64 = 2;
/// The C H R N and CRC that end an ID field.
const ID_BYTES: u64 = 6;
/// Time from a command's last byte to its result, for one that moves no
/// data.
const COMMAND_TIME: Nanos = 20_000;
/// The ready-line poll after a reset, at the 8 MHz clock of 500 kb/s.
const POLL_TIME: Nanos = 1_024_000;
/// A drive reads nothing until its disk is up to speed: within the half
/// second a disk base table allows the motor.
const SPIN_UP: Nanos = 400 * MS;
/// Step pulses a recalibrate issues looking for track 0.
const RECALIBRATE_STEPS: u8 = 77;
/// What a READ TRACK reads past a sector's recorded data.
const GAP_FILLER: u8 = 0x4E;
/// The largest data field the controller moves.
const MAX_DATA: usize = 128 << 6;

/// One drive and its medium.
#[derive(Debug)]
struct Drive {
    kind: Option<DriveKind>,
    /// The track the head is over.
    track: u8,
    floppy: Option<Floppy>,
    /// The disk-change line: set when the medium leaves, cleared by a step
    /// with one in.
    changed: bool,
    /// When the disk in it began to turn.
    turning: Option<Nanos>,
}

/// A turning disk: an index pulse each revolution from when it began.
#[derive(Clone, Copy)]
struct Spin {
    since: Nanos,
    rpm: u64,
}

impl Spin {
    fn index(self, k: u64) -> Nanos {
        let offset = u128::from(k) * MINUTE / u128::from(self.rpm);
        self.since
            .saturating_add(u64::try_from(offset).unwrap_or(u64::MAX))
    }

    /// The revolution under way at `t`.
    fn revolution(self, t: Nanos) -> u64 {
        let k = u128::from(t.saturating_sub(self.since)) * u128::from(self.rpm) / MINUTE;
        let k = u64::try_from(k).unwrap_or(u64::MAX);
        if self.index(k.saturating_add(1)) <= t {
            k.saturating_add(1)
        } else {
            k
        }
    }

    /// The first instant at or after `t` that the point `offset` past the
    /// index passes the head.
    fn passage(self, offset: Nanos, t: Nanos) -> Nanos {
        let k = self.revolution(t);
        let at = self.index(k).saturating_add(offset);
        if at >= t {
            at
        } else {
            self.index(k.saturating_add(1)).saturating_add(offset)
        }
    }

    /// The first index pulse at or after `t`. The hole has just passed the
    /// sensor as the disk starts, so the first comes a revolution later.
    fn next_index(self, t: Nanos) -> Nanos {
        self.passage(0, t.max(self.since + 1))
    }
}

/// What a SCAN command looks for in the disk's bytes against memory's.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Scan {
    #[default]
    Equal,
    LowOrEqual,
    HighOrEqual,
}

/// How a scanned sector's bytes compared with the host's: `FFh` on either
/// side matches anything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Compare {
    equal: bool,
    low: bool,
    high: bool,
}

impl Compare {
    const ANY: Self = Self {
        equal: true,
        low: true,
        high: true,
    };

    fn with(self, disk: u8, host: u8) -> Self {
        if disk == 0xFF || host == 0xFF {
            return self;
        }
        Self {
            equal: self.equal && disk == host,
            low: self.low && disk <= host,
            high: self.high && disk >= host,
        }
    }

    const fn met(self, scan: Scan) -> bool {
        match scan {
            Scan::Equal => self.equal,
            Scan::LowOrEqual => self.low,
            Scan::HighOrEqual => self.high,
        }
    }
}

/// What a data command does with each sector it finds.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Operation {
    /// READ DATA, or READ DELETED DATA.
    Read {
        deleted: bool,
    },
    /// READ TRACK: every sector in the order it passes, from the index.
    #[default]
    Track,
    /// WRITE DATA, or WRITE DELETED DATA.
    Write {
        deleted: bool,
    },
    Scan(Scan),
    ReadId,
    /// FORMAT: `sectors` IDs from the host, `gap` bytes apart, every data
    /// field `filler`.
    Format {
        sectors: u8,
        gap: u8,
        filler: u8,
    },
}

impl Operation {
    const fn to_host(self) -> bool {
        matches!(self, Self::Read { .. } | Self::Track)
    }

    const fn writes(self) -> bool {
        matches!(self, Self::Write { .. } | Self::Format { .. })
    }
}

/// Where an executing command is.
#[derive(Clone, Copy, Debug)]
enum Stage {
    /// Looking, from `from`, for a sector: at `at` the sector in slot
    /// `found` begins its data field, or its ID has been read for READ ID;
    /// with none found the search fails then with ST1 and ST2 bits `fail`.
    /// A disk that is not turning leaves `at` empty.
    Search {
        from: Nanos,
        at: Option<Nanos>,
        found: Option<usize>,
        fail: (u8, u8),
    },
    /// The data field of the sector in `slot` is passing from `start`:
    /// `field` bytes, of which the command moves `len` and byte `done` is
    /// next.
    Data {
        slot: usize,
        start: Nanos,
        len: usize,
        field: usize,
        done: usize,
        compare: Compare,
    },
    /// Formatting from the index at `index`, once `begun`: byte `byte` of
    /// sector `sector`'s ID is next.
    Format {
        index: Option<Nanos>,
        begun: bool,
        sector: u8,
        byte: u8,
    },
}

/// A data command under way.
// MT, MF and SK are the command byte's independent option bits, and terminal
// count a latch beside them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Copy, Debug, Default)]
struct Transfer {
    operation: Operation,
    unit: u8,
    head: u8,
    multitrack: bool,
    mfm: bool,
    skip: bool,
    /// The ID register: the sector sought.
    id: SectorId,
    eot: u8,
    /// The data length when N is 0, or a scan's sector step.
    dtl: u8,
    /// ST1 and ST2 bits gathered on the way.
    st1: u8,
    st2: u8,
    /// Sectors a READ TRACK has read.
    count: u8,
    /// Terminal count has come.
    terminal: bool,
    stage: Stage,
}

/// What the controller is doing.
#[derive(Clone, Copy, Debug)]
enum Phase {
    Idle,
    Command {
        bytes: [u8; 9],
        len: usize,
    },
    Execute(Transfer),
    /// The result, readable from `at` and interrupting then if
    /// `interrupts`.
    Result {
        bytes: [u8; 7],
        len: usize,
        read: usize,
        at: Nanos,
        interrupts: bool,
    },
}

/// A seek or recalibrate stepping a unit.
#[derive(Clone, Copy, Debug, Default)]
struct Seek {
    /// The head bit the command named, which its status reports.
    head: u8,
    /// The cylinder it ends at, or none for a recalibrate.
    target: Option<u8>,
    /// Steps toward track 0, else away from it.
    outward: bool,
    pulses: u8,
    /// The next pulse, or the end once none are left.
    next: Nanos,
    step: Nanos,
}

/// The SPECIFY command's timings and mode.
#[derive(Clone, Copy, Debug)]
struct Specify {
    srt: u8,
    hut: u8,
    hlt: u8,
    non_dma: bool,
}

/// What falls due next.
#[derive(Clone, Copy)]
enum Due {
    Poll,
    Seek(u8),
    Transfer,
}

/// The sector a transfer is moving, or a format's IDs.
struct Buffer([u8; MAX_DATA]);

impl core::fmt::Debug for Buffer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Buffer").finish_non_exhaustive()
    }
}

/// The controller, the adapter's registers and the drives.
#[derive(Debug)]
pub struct Fdc {
    interface: Interface,
    drives: [Drive; 4],
    dor: u8,
    ccr: u8,
    phase: Phase,
    /// Each unit's present cylinder register.
    pcn: [u8; 4],
    /// Each unit's interrupt status; SENSE INTERRUPT reports the lowest
    /// unit's first.
    status: [Option<u8>; 4],
    seeks: [Option<Seek>; 4],
    specify: Specify,
    /// When a reset's ready-line poll reports the units.
    poll: Option<Nanos>,
    /// The head stays loaded until then.
    loaded_until: Nanos,
    /// The byte in the data register during non-DMA execution: one for the
    /// host to take, or one it gave.
    pio: Option<u8>,
    /// What the data register last held, which a read finds when nothing is
    /// ready.
    last: u8,
    now: Nanos,
    buffer: Buffer,
}

/// Bytes the command beginning with `first` takes, itself included.
const fn command_length(first: u8) -> usize {
    match first & 0x1F {
        0x03 | 0x0F => 3,
        0x04 | 0x07 | 0x0A => 2,
        0x0D => 6,
        0x02 | 0x05 | 0x06 | 0x09 | 0x0C | 0x11 | 0x19 | 0x1D => 9,
        _ => 1,
    }
}

/// The medium's cylinder under a head at `track`: a 48 tpi medium in a 96
/// tpi drive lies under the even tracks, and a 48 tpi drive's wide head
/// cannot read a 96 tpi one.
fn under_head(kind: DriveKind, cylinders: u8, track: u8) -> Option<u8> {
    match (kind.tracks() == 80, cylinders <= NARROW_CYLINDERS) {
        (true, true) => track.is_multiple_of(2).then_some(track / 2),
        (false, false) => None,
        _ => Some(track),
    }
}

/// Where each sector's ID field begins, in bytes after the index: the
/// format's gap apart where it fixes one that fits, else spread evenly, and
/// squeezed in where the track could not hold them.
fn id_offsets(track: &Track, layout: &Layout, track_bytes: u64) -> [u64; MAX_SECTORS] {
    let sectors = track.sectors();
    let span = |s: &Sector| layout.data_offset + s.id.bytes() as u64 + CRC;
    let total: u64 = sectors.iter().map(span).sum();
    let count = sectors.len() as u64;
    let room = track_bytes.saturating_sub(layout.index_gap);
    let fits = total <= room;
    let even = room.saturating_sub(total) / count.max(1);
    let gap = track
        .gap
        .map(u64::from)
        .filter(|&g| total + g * count <= room)
        .unwrap_or(even);
    let mut offsets = [0; MAX_SECTORS];
    let mut before = 0;
    for (offset, sector) in offsets.iter_mut().zip(sectors) {
        *offset = layout.index_gap + if fits { before } else { before * room / total };
        before += span(sector) + if fits { gap } else { 0 };
    }
    offsets
}

/// The ID a transfer moves on to after its sector, its head, and whether
/// that is past the cylinder: the next record, the other side's first in a
/// multi-track transfer, or the next cylinder's.
fn next_id(x: &Transfer, step: u8) -> (u8, SectorId, bool) {
    if x.id.r != x.eot {
        return (
            x.head,
            SectorId {
                r: x.id.r.wrapping_add(step),
                ..x.id
            },
            false,
        );
    }
    let flipped = SectorId {
        h: x.id.h ^ 1,
        r: 1,
        ..x.id
    };
    match (x.multitrack, x.head) {
        (true, 0) => (1, flipped, false),
        (true, _) => (
            x.head,
            SectorId {
                c: x.id.c.wrapping_add(1),
                ..flipped
            },
            true,
        ),
        (false, _) => (
            x.head,
            SectorId {
                c: x.id.c.wrapping_add(1),
                r: 1,
                ..x.id
            },
            true,
        ),
    }
}

/// The bytes a command moves of a sector: 128 << N, or DTL when N is 0.
fn data_length(id: SectorId, dtl: u8) -> usize {
    if id.n == 0 {
        usize::from(dtl).min(128)
    } else {
        id.bytes()
    }
}

impl Fdc {
    /// A controller on `interface` with drives of `kinds`, none holding a
    /// disk, held in reset as the adapter powers up.
    #[must_use]
    pub fn new(interface: Interface, kinds: [Option<DriveKind>; 4]) -> Self {
        let drive = |kind| Drive {
            kind,
            track: 0,
            floppy: None,
            changed: true,
            turning: None,
        };
        Self {
            interface,
            drives: kinds.map(drive),
            dor: 0,
            ccr: 2,
            phase: Phase::Idle,
            pcn: [0; 4],
            status: [None; 4],
            seeks: [None; 4],
            specify: Specify {
                srt: 0,
                hut: 0,
                hlt: 0,
                non_dma: false,
            },
            poll: None,
            loaded_until: 0,
            pio: None,
            last: 0,
            now: 0,
            buffer: Buffer([0; MAX_DATA]),
        }
    }

    /// Puts `floppy` in drive `n` at `t`, the controller having been
    /// advanced there, and returns the disk it held. A drive that is not
    /// fitted hands the disk back.
    pub fn insert(&mut self, n: usize, floppy: Floppy, t: Nanos) -> Result<Option<Floppy>, Floppy> {
        if self.drives.get(n).is_none_or(|d| d.kind.is_none()) {
            return Err(floppy);
        }
        Ok(self.change_medium(n, Some(floppy), t))
    }

    /// Takes the disk out of drive `n` at `t`, the controller having been
    /// advanced there.
    pub fn eject(&mut self, n: usize, t: Nanos) -> Option<Floppy> {
        if n >= self.drives.len() {
            return None;
        }
        self.change_medium(n, None, t)
    }

    fn change_medium(&mut self, n: usize, floppy: Option<Floppy>, t: Nanos) -> Option<Floppy> {
        let motor = self.dor & (0x10 << n) != 0;
        let drive = &mut self.drives[n];
        drive.changed = true;
        let old = core::mem::replace(&mut drive.floppy, floppy);
        drive.turning = (motor && drive.floppy.is_some()).then_some(t);
        if self.selected() == Some(n) {
            self.disturbed(t);
        }
        old
    }

    /// The fitted drives whose motors the digital output register runs, a
    /// bit each: what their lamps show.
    #[must_use]
    pub fn motors(&self) -> u8 {
        (0..self.drives.len())
            .filter(|&n| self.drives[n].kind.is_some() && self.dor & (0x10 << n) != 0)
            .fold(0, |on, n| on | 1 << n)
    }

    /// Whether the adapter drives IRQ 6: the controller's interrupt, which
    /// reaches the bus only while the digital output register enables DMA
    /// and interrupts.
    #[must_use]
    pub fn irq(&self) -> bool {
        let pio = matches!(self.phase, Phase::Execute(x) if self.specify.non_dma && self.pio_request(&x) != 0);
        let result = matches!(self.phase, Phase::Result { at, read: 0, interrupts: true, .. } if at <= self.now);
        self.dor & dor::DMA_IRQ != 0 && (result || pio || self.status.iter().any(Option::is_some))
    }

    /// The next instant the controller acts on its own, if it will.
    #[must_use]
    pub fn next_event(&self) -> Option<Nanos> {
        let result = match self.phase {
            Phase::Result {
                at,
                read: 0,
                interrupts: true,
                ..
            } if at > self.now => Some(at),
            _ => None,
        };
        match (self.due().map(|(at, _)| at), result) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    /// Advances the controller to `t`, acting on everything due by then in
    /// the order it falls due; `dma` and `memory` carry the data.
    pub fn advance<M: DmaMemory>(&mut self, t: Nanos, dma: &mut Dma, memory: &mut M) {
        while let Some((at, due)) = self.due().filter(|&(at, _)| at <= t) {
            self.now = self.now.max(at);
            match due {
                Due::Poll => {
                    self.poll = None;
                    for (unit, status) in (0..).zip(self.status.iter_mut()) {
                        *status = Some(st0::READY_CHANGED | unit);
                    }
                }
                Due::Seek(unit) => self.seek_step(unit, at),
                Due::Transfer => self.transfer_step(at, dma, memory),
            }
        }
        self.now = self.now.max(t);
    }

    /// Reads port `port` at `t`: 3F4h the main status, 3F5h data, and on a
    /// high-density adapter 3F7h the disk-change line. Ports the adapter
    /// does not decode answer nothing.
    pub fn input<M: DmaMemory>(
        &mut self,
        port: u16,
        t: Nanos,
        dma: &mut Dma,
        memory: &mut M,
    ) -> Option<u8> {
        self.advance(t, dma, memory);
        match port & 7 {
            4 => Some(self.main_status()),
            5 => Some(self.read_data()),
            7 if self.interface == Interface::HighDensity => Some(self.change_register()),
            _ => None,
        }
    }

    /// Writes port `port` at `t`: 3F2h the digital output register, 3F5h
    /// data, and on a high-density adapter 3F7h the data rate.
    pub fn output<M: DmaMemory>(
        &mut self,
        port: u16,
        value: u8,
        t: Nanos,
        dma: &mut Dma,
        memory: &mut M,
    ) {
        self.advance(t, dma, memory);
        match port & 7 {
            2 => self.write_dor(value, t),
            5 if self.dor & dor::RESET_OFF != 0 => self.write_data(value, t),
            7 if self.interface == Interface::HighDensity && self.ccr != value & 3 => {
                self.ccr = value & 3;
                self.disturbed(t);
            }
            _ => {}
        }
    }

    fn due(&self) -> Option<(Nanos, Due)> {
        let mut next = self.poll.map(|at| (at, Due::Poll));
        let mut consider = |at: Nanos, due: Due| {
            if next.is_none_or(|(n, _)| at < n) {
                next = Some((at, due));
            }
        };
        for (unit, seek) in (0..).zip(&self.seeks) {
            if let Some(seek) = seek {
                consider(seek.next, Due::Seek(unit));
            }
        }
        if let Phase::Execute(x) = self.phase {
            if let Some(at) = self.transfer_due(&x) {
                consider(at, Due::Transfer);
            }
        }
        next
    }

    /// The drive the adapter selects: the digital output register's choice,
    /// if fitted and its motor is on.
    fn selected(&self) -> Option<usize> {
        let n = usize::from(self.dor & 3);
        (self.dor & (0x10 << n) != 0 && self.drives[n].kind.is_some()).then_some(n)
    }

    fn spin(&self, n: usize) -> Option<Spin> {
        let drive = &self.drives[n];
        Some(Spin {
            since: drive.turning?,
            rpm: drive.kind?.rpm(),
        })
    }

    fn data_rate(&self) -> u32 {
        match self.interface {
            Interface::Standard => 250,
            // The fourth is the 1 Mb/s later controllers gave it, at which no
            // medium here is recorded.
            Interface::HighDensity => [500, 300, 250, 1000][usize::from(self.ccr & 3)],
        }
    }

    fn byte_ns(&self, layout: &Layout, bytes: u64) -> Nanos {
        bytes * layout.cells * 1_000_000 / u64::from(self.data_rate())
    }

    fn track_bytes(&self, rpm: u64, layout: &Layout) -> u64 {
        let bytes = MINUTE * u128::from(self.data_rate())
            / (u128::from(rpm) * u128::from(layout.cells) * 1_000_000);
        u64::try_from(bytes).unwrap_or(u64::MAX)
    }

    /// A time the controller counts at 500 kb/s's 8 MHz clock, at the
    /// selected rate's clock.
    fn clocked(&self, at_500k: Nanos) -> Nanos {
        at_500k * 500 / u64::from(self.data_rate())
    }

    fn step_time(&self) -> Nanos {
        self.clocked(u64::from(16 - self.specify.srt) * MS)
    }

    fn head_load_time(&self) -> Nanos {
        let units = if self.specify.hlt == 0 {
            128
        } else {
            self.specify.hlt
        };
        self.clocked(u64::from(units) * 2 * MS)
    }

    fn head_unload_time(&self) -> Nanos {
        let units = if self.specify.hut == 0 {
            16
        } else {
            self.specify.hut
        };
        self.clocked(u64::from(units) * 16 * MS)
    }

    /// The cylinder and track under the head of drive `n` on `head`, if it
    /// can be read at the selected rate in this encoding.
    fn medium_track(&self, n: usize, head: u8, mfm: bool) -> Option<(u8, Track)> {
        let drive = &self.drives[n];
        let kind = drive.kind?;
        let floppy = drive.floppy.as_ref()?;
        let cylinder = under_head(kind, floppy.cylinders(), drive.track)?;
        let track = floppy.track(cylinder, head)?;
        let encoding = if mfm { Encoding::Mfm } else { Encoding::Fm };
        let fits = track.encoding == encoding
            && kind.rate_for(track.density) == self.data_rate()
            && (track.density == Density::Double || kind.reads_high_density());
        fits.then_some((cylinder, track))
    }

    fn main_status(&self) -> u8 {
        if self.dor & dor::RESET_OFF == 0 {
            return 0;
        }
        let seeking = (0..)
            .zip(&self.seeks)
            .filter(|(_, s)| s.is_some())
            .fold(0, |bits, (unit, _)| bits | 1 << unit);
        seeking
            | match self.phase {
                Phase::Idle => msr::RQM,
                Phase::Command { .. } => msr::RQM | msr::BUSY,
                Phase::Execute(x) if self.specify.non_dma => {
                    msr::BUSY | msr::NON_DMA | self.pio_request(&x)
                }
                Phase::Result { at, .. } if at <= self.now => msr::RQM | msr::DIO | msr::BUSY,
                Phase::Execute(_) | Phase::Result { .. } => msr::BUSY,
            }
    }

    /// The request bits non-DMA execution shows: a byte for the host to
    /// take, or room for one from it.
    fn pio_request(&self, x: &Transfer) -> u8 {
        if x.operation.to_host() {
            return if self.pio.is_some() {
                msr::RQM | msr::DIO
            } else {
                0
            };
        }
        let wants = match (x.stage, x.operation) {
            (Stage::Data { len, done, .. }, _) => done < len,
            (Stage::Format { begun, sector, .. }, Operation::Format { sectors, .. }) => {
                begun && sector < sectors
            }
            _ => false,
        };
        if wants && self.pio.is_none() {
            msr::RQM
        } else {
            0
        }
    }

    fn read_data(&mut self) -> u8 {
        match self.phase {
            Phase::Result {
                bytes,
                len,
                read,
                at,
                interrupts,
            } if at <= self.now => {
                self.last = bytes[read];
                self.phase = if read + 1 >= len {
                    Phase::Idle
                } else {
                    Phase::Result {
                        bytes,
                        len,
                        read: read + 1,
                        at,
                        interrupts,
                    }
                };
            }
            Phase::Execute(x) if self.specify.non_dma && x.operation.to_host() => {
                if let Some(byte) = self.pio.take() {
                    self.last = byte;
                }
            }
            _ => {}
        }
        self.last
    }

    fn change_register(&self) -> u8 {
        let changed = self.selected().is_some_and(|n| {
            let drive = &self.drives[n];
            drive.changed && drive.kind.is_some_and(DriveKind::change_line)
        });
        // Only bit 7 is the adapter's.
        (OPEN_BUS & 0x7F) | (u8::from(changed) << 7)
    }

    fn selection(&self) -> Option<(usize, Option<Nanos>)> {
        self.selected().map(|n| (n, self.drives[n].turning))
    }

    fn write_dor(&mut self, value: u8, t: Nanos) {
        let before = self.selection();
        let running = self.dor & dor::RESET_OFF != 0;
        for (n, drive) in self.drives.iter_mut().enumerate() {
            let bit = 0x10 << n;
            match (self.dor & bit != 0, value & bit != 0) {
                (false, true) => drive.turning = drive.floppy.is_some().then_some(t),
                (true, false) => drive.turning = None,
                _ => {}
            }
        }
        self.dor = value;
        match (running, value & dor::RESET_OFF != 0) {
            (true, false) => {
                self.phase = Phase::Idle;
                self.seeks = [None; 4];
                self.status = [None; 4];
                self.poll = None;
                self.pio = None;
            }
            (false, true) => self.poll = Some(t + self.clocked(POLL_TIME)),
            _ => {}
        }
        if self.selection() != before {
            self.disturbed(t);
        }
    }

    fn write_data(&mut self, value: u8, t: Nanos) {
        let (mut bytes, len) = match self.phase {
            Phase::Idle => ([0; 9], 0),
            Phase::Command { bytes, len } => (bytes, len),
            Phase::Execute(x) => {
                if self.specify.non_dma && self.pio_request(&x) == msr::RQM {
                    self.pio = Some(value);
                }
                return;
            }
            Phase::Result { .. } => return,
        };
        bytes[len] = value;
        let len = len + 1;
        if len < command_length(bytes[0]) {
            self.phase = Phase::Command { bytes, len };
        } else {
            self.phase = Phase::Idle;
            self.execute(bytes, t);
        }
    }

    fn result(&mut self, bytes: &[u8], at: Nanos, interrupts: bool) {
        let mut out = [0; 7];
        let len = bytes.len().min(out.len());
        out[..len].copy_from_slice(&bytes[..len]);
        self.phase = Phase::Result {
            bytes: out,
            len,
            read: 0,
            at,
            interrupts,
        };
    }

    fn execute(&mut self, c: [u8; 9], t: Nanos) {
        let unit = c[1] & 3;
        let head = (c[1] >> 2) & 1;
        let at = t + COMMAND_TIME;
        let transfer = |operation| Transfer {
            operation,
            unit,
            head,
            multitrack: c[0] & 0x80 != 0,
            mfm: c[0] & 0x40 != 0,
            skip: c[0] & 0x20 != 0,
            id: SectorId {
                c: c[2],
                h: c[3],
                r: c[4],
                n: c[5],
            },
            eot: c[6],
            dtl: c[8],
            st1: 0,
            st2: 0,
            count: 0,
            terminal: false,
            stage: Stage::Search {
                from: t,
                at: None,
                found: None,
                fail: (0, 0),
            },
        };
        match c[0] & 0x1F {
            0x02 => self.start(transfer(Operation::Track), t),
            0x03 => {
                self.specify = Specify {
                    srt: c[1] >> 4,
                    hut: c[1] & 0x0F,
                    hlt: c[2] >> 1,
                    non_dma: c[2] & 1 != 0,
                }
            }
            0x04 => {
                let st3 = self.drive_status(unit, head);
                self.result(&[st3], at, false);
            }
            0x05 => self.start(transfer(Operation::Write { deleted: false }), t),
            0x06 => self.start(transfer(Operation::Read { deleted: false }), t),
            0x07 => self.start_seek(unit, head, None, t),
            0x08 => self.sense_interrupt(at),
            0x09 => self.start(transfer(Operation::Write { deleted: true }), t),
            0x0A => self.start(transfer(Operation::ReadId), t),
            0x0C => self.start(transfer(Operation::Read { deleted: true }), t),
            0x0D => {
                let format = Operation::Format {
                    sectors: c[3],
                    gap: c[4],
                    filler: c[5],
                };
                self.start(
                    Transfer {
                        id: SectorId {
                            c: 0,
                            h: 0,
                            r: 0,
                            n: c[2],
                        },
                        ..transfer(format)
                    },
                    t,
                );
            }
            0x0F => self.start_seek(unit, head, Some(c[2]), t),
            0x11 => self.start(transfer(Operation::Scan(Scan::Equal)), t),
            0x19 => self.start(transfer(Operation::Scan(Scan::LowOrEqual)), t),
            0x1D => self.start(transfer(Operation::Scan(Scan::HighOrEqual)), t),
            _ => self.result(&[st0::INVALID], at, false),
        }
    }

    fn drive_status(&self, unit: u8, head: u8) -> u8 {
        let drive = self.selected().map(|n| &self.drives[n]);
        let protected = drive
            .and_then(|d| d.floppy.as_ref())
            .is_some_and(Floppy::write_protected);
        let zero = drive.is_some_and(|d| d.track == 0);
        (u8::from(protected) * st3::WRITE_PROTECTED)
            | st3::READY
            | (u8::from(zero) * st3::TRACK_ZERO)
            | st3::TWO_SIDED
            | (head << 2)
            | unit
    }

    fn sense_interrupt(&mut self, at: Nanos) {
        let pending = (0..4).find_map(|unit| Some((self.status[unit].take()?, self.pcn[unit])));
        match pending {
            Some((st0, pcn)) => self.result(&[st0, pcn], at, false),
            None => self.result(&[st0::INVALID], at, false),
        }
    }

    fn start_seek(&mut self, unit: u8, head: u8, target: Option<u8>, t: Nanos) {
        let pcn = self.pcn[usize::from(unit)];
        let (pulses, outward) = match target {
            Some(ncn) => (pcn.abs_diff(ncn), ncn < pcn),
            None => (RECALIBRATE_STEPS, true),
        };
        let step = self.step_time();
        self.seeks[usize::from(unit)] = Some(Seek {
            head,
            target,
            outward,
            pulses,
            next: t + COMMAND_TIME,
            step,
        });
    }

    /// A seek's next pulse, or its end: a recalibrate ends when the selected
    /// drive reports track 0, or fails once its pulses run out.
    fn seek_step(&mut self, unit: u8, at: Nanos) {
        let u = usize::from(unit);
        let Some(mut seek) = self.seeks[u] else {
            return;
        };
        let at_zero =
            seek.target.is_none() && self.selected().is_some_and(|n| self.drives[n].track == 0);
        if seek.pulses == 0 || at_zero {
            self.seeks[u] = None;
            self.pcn[u] = seek.target.unwrap_or(0);
            let failed = if seek.target.is_none() && !at_zero {
                st0::ABNORMAL | st0::EQUIPMENT_CHECK
            } else {
                0
            };
            self.status[u] = Some(st0::SEEK_END | failed | (seek.head << 2) | unit);
            return;
        }
        seek.pulses -= 1;
        seek.next = at + seek.step;
        self.seeks[u] = Some(seek);
        self.step_head(seek.outward, at);
    }

    /// A step pulse reaches the selected drive.
    fn step_head(&mut self, outward: bool, at: Nanos) {
        let Some(n) = self.selected() else { return };
        let drive = &mut self.drives[n];
        let Some(kind) = drive.kind else { return };
        if drive.floppy.is_some() {
            drive.changed = false;
        }
        let track = if outward {
            drive.track.saturating_sub(1)
        } else {
            (drive.track + 1).min(kind.last_track())
        };
        if track != drive.track {
            drive.track = track;
            self.disturbed(at);
        }
    }

    /// Whether a write must be refused before it starts: the selected disk
    /// is protected, or is a raw image that could not record it.
    fn refuses_write(&self, x: &Transfer) -> bool {
        let Some(n) = self.selected() else {
            return false;
        };
        let Some(floppy) = self.drives[n].floppy.as_ref() else {
            return false;
        };
        if !x.operation.writes() {
            return false;
        }
        match x.operation {
            _ if floppy.write_protected() => true,
            Operation::Write { deleted } => deleted,
            Operation::Format { sectors, .. } => {
                self.medium_track(n, x.head, x.mfm)
                    .is_none_or(|(_, track)| {
                        track.sectors().len() != usize::from(sectors)
                            || track.sectors().iter().any(|s| s.id.n != x.id.n)
                    })
            }
            _ => false,
        }
    }

    fn start(&mut self, x: Transfer, t: Nanos) {
        if self.refuses_write(&x) {
            let st0 = st0::ABNORMAL | (x.head << 2) | x.unit;
            let id = x.id;
            self.result(
                &[st0, st1::NOT_WRITABLE, 0, id.c, id.h, id.r, id.n],
                t + COMMAND_TIME,
                true,
            );
            return;
        }
        self.pio = None;
        let from = if t < self.loaded_until {
            t
        } else {
            t + self.head_load_time()
        };
        match x.operation {
            Operation::Format { .. } => {
                let stage = Stage::Format {
                    index: self.format_index(from),
                    begun: false,
                    sector: 0,
                    byte: 0,
                };
                self.phase = Phase::Execute(Transfer { stage, ..x });
            }
            _ => self.search(x, from),
        }
    }

    /// The index pulse a format begins at, from `from`.
    fn format_index(&self, from: Nanos) -> Option<Nanos> {
        let spin = self.selected().and_then(|n| self.spin(n))?;
        Some(spin.next_index(from.max(spin.since + SPIN_UP)))
    }

    fn search(&mut self, x: Transfer, from: Nanos) {
        let stage = self.find(&x, from);
        self.phase = Phase::Execute(Transfer { stage, ..x });
    }

    /// Looks from `from` for the sector a transfer wants next. The search
    /// gives up at the second index pulse; a disk that is not turning gives
    /// none, so the command waits.
    fn find(&self, x: &Transfer, from: Nanos) -> Stage {
        let waits = Stage::Search {
            from,
            at: None,
            found: None,
            fail: (0, 0),
        };
        let Some((n, spin)) = self.selected().and_then(|n| Some((n, self.spin(n)?))) else {
            return waits;
        };
        let first = spin.next_index(from);
        let deadline = spin.next_index(first + 1);
        let missing = Stage::Search {
            from,
            at: Some(deadline),
            found: None,
            fail: (st1::MISSING_MARK, 0),
        };
        let layout = layout(x.mfm);
        let Some((_, track)) = self.medium_track(n, x.head, x.mfm) else {
            return missing;
        };
        let sectors = track.sectors();
        let offsets = id_offsets(&track, layout, self.track_bytes(spin.rpm, layout));
        let mut earliest = from.max(spin.since + SPIN_UP);
        if x.operation == Operation::Track && x.count == 0 {
            earliest = earliest.max(first);
        }
        let mut seen = false;
        let mut best: Option<(Nanos, usize)> = None;
        for (slot, (sector, &offset)) in sectors.iter().zip(&offsets).enumerate() {
            let at = spin.passage(self.byte_ns(layout, offset), earliest);
            if at >= deadline {
                continue;
            }
            seen = true;
            let wanted = match x.operation {
                Operation::ReadId => true,
                Operation::Track => slot == usize::from(x.count) % sectors.len(),
                _ => sector.id == x.id,
            };
            if wanted && best.is_none_or(|(b, _)| at < b) {
                best = Some((at, slot));
            }
        }
        let Some((at, slot)) = best else {
            if !seen {
                return missing;
            }
            let cylinder = |bits, s: &Sector| match s.id.c {
                c if c == x.id.c => bits,
                0xFF => bits | st2::BAD_CYLINDER,
                _ => bits | st2::WRONG_CYLINDER,
            };
            return Stage::Search {
                from,
                at: Some(deadline),
                found: None,
                fail: (st1::NO_DATA, sectors.iter().fold(0, cylinder)),
            };
        };
        let into = if x.operation == Operation::ReadId {
            layout.id_field
        } else {
            layout.data_offset
        };
        Stage::Search {
            from,
            at: Some(at + self.byte_ns(layout, into)),
            found: Some(slot),
            fail: (0, 0),
        }
    }

    /// The selected drive, its disk's rotation, its medium or the data rate
    /// changed under an executing command: a search starts again, and data
    /// already passing is lost to a CRC error.
    fn disturbed(&mut self, t: Nanos) {
        let Phase::Execute(x) = self.phase else {
            return;
        };
        match x.stage {
            Stage::Search { from, .. } => self.search(x, t.max(from)),
            Stage::Format { begun: false, .. } => {
                let stage = Stage::Format {
                    index: self.format_index(t),
                    begun: false,
                    sector: 0,
                    byte: 0,
                };
                self.phase = Phase::Execute(Transfer { stage, ..x });
            }
            Stage::Data { .. } | Stage::Format { .. } => {
                let st2 = if x.operation.to_host() {
                    st2::DATA_CRC
                } else {
                    0
                };
                self.finish(
                    x,
                    st0::ABNORMAL,
                    x.st1 | st1::DATA_ERROR,
                    x.st2 | st2,
                    x.id,
                    t,
                );
            }
        }
    }

    fn transfer_due(&self, x: &Transfer) -> Option<Nanos> {
        let layout = layout(x.mfm);
        match x.stage {
            Stage::Search { at, .. } => at,
            Stage::Data {
                start,
                len,
                field,
                done,
                ..
            } => {
                let bytes = if done < len {
                    done as u64 + 1
                } else {
                    field as u64 + CRC
                };
                Some(start + self.byte_ns(layout, bytes))
            }
            Stage::Format {
                index,
                begun,
                sector,
                byte,
            } => {
                let index = index?;
                let Operation::Format { sectors, gap, .. } = x.operation else {
                    return Some(index);
                };
                if !begun {
                    return Some(index);
                }
                let stride = layout.data_offset + x.id.bytes() as u64 + CRC + u64::from(gap);
                if sector < sectors {
                    let id_byte = layout.index_gap + u64::from(sector) * stride + layout.id_field
                        - ID_BYTES
                        + u64::from(byte);
                    return Some(index + self.byte_ns(layout, id_byte));
                }
                let spin = self.selected().and_then(|n| self.spin(n))?;
                Some(spin.next_index(
                    index + self.byte_ns(layout, layout.index_gap + u64::from(sectors) * stride),
                ))
            }
        }
    }

    fn transfer_step<M: DmaMemory>(&mut self, at: Nanos, dma: &mut Dma, memory: &mut M) {
        let Phase::Execute(x) = self.phase else {
            return;
        };
        match x.stage {
            Stage::Search {
                found: Some(slot), ..
            } => self.found(x, slot, at),
            Stage::Search {
                fail: (st1, st2), ..
            } => self.finish(x, st0::ABNORMAL, x.st1 | st1, x.st2 | st2, x.id, at),
            Stage::Data { len, done, .. } if done < len => self.data_byte(x, at, dma, memory),
            Stage::Data { .. } => self.end_sector(x, at),
            Stage::Format { .. } => self.format_step(x, at, dma, memory),
        }
    }

    /// The sector a search found reaches its data field, or its ID has been
    /// read: the sector is checked and its data begins to move.
    fn found(&mut self, mut x: Transfer, slot: usize, at: Nanos) {
        let located = self
            .selected()
            .and_then(|n| Some((n, self.medium_track(n, x.head, x.mfm)?)));
        let Some((n, (cylinder, track))) = located else {
            return self.finish(x, st0::ABNORMAL, x.st1 | st1::MISSING_MARK, x.st2, x.id, at);
        };
        let Some(&sector) = track.sectors().get(slot) else {
            return self.finish(x, st0::ABNORMAL, x.st1 | st1::MISSING_MARK, x.st2, x.id, at);
        };
        let wants_deleted = match x.operation {
            Operation::ReadId => return self.finish(x, 0, 0, 0, sector.id, at),
            Operation::Read { deleted } => Some(deleted),
            Operation::Scan(_) => Some(false),
            _ => None,
        };
        if wants_deleted.is_some_and(|deleted| deleted != sector.deleted) {
            x.st2 |= st2::CONTROL_MARK;
            if x.skip {
                return match x.operation {
                    Operation::Scan(_) => self.scan_on(x, at),
                    _ => self.after_sector(x, at),
                };
            }
        }
        let len = data_length(x.id, x.dtl);
        let field = if x.operation == Operation::Track {
            len
        } else {
            sector.id.bytes()
        };
        if x.operation.to_host() || matches!(x.operation, Operation::Scan(_)) {
            if !sector.readable && x.operation != Operation::Track {
                return self.finish(
                    x,
                    st0::ABNORMAL,
                    x.st1 | st1::MISSING_MARK,
                    x.st2 | st2::MISSING_DATA_MARK,
                    x.id,
                    at,
                );
            }
            let recorded = if sector.readable {
                sector.id.bytes().min(len)
            } else {
                0
            };
            self.buffer.0[recorded..len].fill(GAP_FILLER);
            let read = self.drives[n].floppy.as_mut().is_some_and(|f| {
                f.read(cylinder, x.head, slot, &mut self.buffer.0[..recorded])
                    .is_ok()
            });
            if !sector.readable {
                x.st2 |= st2::MISSING_DATA_MARK;
            } else if !read
                || sector.bad
                || (x.operation == Operation::Track && len != sector.id.bytes())
            {
                x.st1 |= st1::DATA_ERROR;
                x.st2 |= st2::DATA_CRC;
            }
        }
        x.stage = Stage::Data {
            slot,
            start: at,
            len,
            field,
            done: 0,
            compare: Compare::ANY,
        };
        self.phase = Phase::Execute(x);
    }

    /// Hands the host a byte as it passes: whether terminal count came with
    /// it, or nothing when the host missed it.
    fn give<M: DmaMemory>(&mut self, byte: u8, dma: &mut Dma, memory: &mut M) -> Option<bool> {
        if self.specify.non_dma {
            if self.pio.is_some() {
                return None;
            }
            self.pio = Some(byte);
            return Some(false);
        }
        if self.dor & dor::DMA_IRQ == 0 {
            return None;
        }
        let moved = dma.to_memory(DMA_CHANNEL, memory, &[byte]);
        (moved.bytes == 1).then_some(moved.terminal)
    }

    /// Takes the byte the host has ready, and whether terminal count came
    /// with it.
    fn take<M: DmaMemory>(&mut self, dma: &mut Dma, memory: &mut M) -> Option<(u8, bool)> {
        if self.specify.non_dma {
            return self.pio.take().map(|byte| (byte, false));
        }
        if self.dor & dor::DMA_IRQ == 0 {
            return None;
        }
        let mut byte = [0];
        let moved = dma.from_memory(DMA_CHANNEL, memory, &mut byte);
        (moved.bytes == 1).then_some((byte[0], moved.terminal))
    }

    /// Moves the next byte of the data field passing the head. After
    /// terminal count the rest passes unmoved, a write filling it with
    /// zeros.
    fn data_byte<M: DmaMemory>(
        &mut self,
        mut x: Transfer,
        at: Nanos,
        dma: &mut Dma,
        memory: &mut M,
    ) {
        let Stage::Data {
            slot,
            start,
            len,
            field,
            done,
            mut compare,
        } = x.stage
        else {
            return;
        };
        let terminal = if x.operation.to_host() {
            self.give(self.buffer.0[done], dma, memory)
        } else {
            self.take(dma, memory).map(|(byte, terminal)| {
                match x.operation {
                    Operation::Scan(_) => compare = compare.with(self.buffer.0[done], byte),
                    _ => self.buffer.0[done] = byte,
                }
                terminal
            })
        };
        let Some(terminal) = terminal else {
            return self.finish(x, st0::ABNORMAL, x.st1 | st1::OVERRUN, x.st2, x.id, at);
        };
        let done = if terminal {
            if matches!(x.operation, Operation::Write { .. }) {
                self.buffer.0[done + 1..len].fill(0);
            }
            len
        } else {
            done + 1
        };
        x.terminal |= terminal;
        x.stage = Stage::Data {
            slot,
            start,
            len,
            field,
            done,
            compare,
        };
        self.phase = Phase::Execute(x);
    }

    /// A sector's data field and CRC have passed.
    fn end_sector(&mut self, x: Transfer, at: Nanos) {
        let Stage::Data {
            slot,
            len,
            field,
            compare,
            ..
        } = x.stage
        else {
            return;
        };
        if x.operation.to_host() && self.specify.non_dma && self.pio.is_some() {
            return self.finish(x, st0::ABNORMAL, x.st1 | st1::OVERRUN, x.st2, x.id, at);
        }
        let failed = x.st1 & st1::DATA_ERROR != 0;
        match x.operation {
            Operation::Read { .. } | Operation::Scan(_) if failed => {
                self.finish(x, st0::ABNORMAL, x.st1, x.st2, x.id, at);
            }
            // A mark other than the one asked for ends the command on this
            // sector, its address not advanced.
            Operation::Read { .. } if x.st2 & st2::CONTROL_MARK != 0 && !x.skip => {
                self.finish(x, 0, x.st1, x.st2, x.id, at);
            }
            Operation::Read { .. } => self.after_sector(x, at),
            Operation::Write { .. } => {
                self.buffer.0[len..field].fill(0);
                if self.write_sector(&x, slot, field) {
                    self.after_sector(x, at);
                } else {
                    self.finish(x, st0::ABNORMAL, x.st1 | st1::DATA_ERROR, x.st2, x.id, at);
                }
            }
            Operation::Scan(scan) if compare.met(scan) => {
                let (_, id, _) = next_id(&x, x.dtl);
                let hit = if compare.equal { st2::SCAN_HIT } else { 0 };
                self.finish(x, 0, x.st1, x.st2 | hit, id, at);
            }
            Operation::Scan(_) => self.scan_on(x, at),
            Operation::Track => self.track_on(x, at),
            Operation::ReadId | Operation::Format { .. } => {
                self.finish(x, st0::ABNORMAL, x.st1, x.st2, x.id, at);
            }
        }
    }

    fn write_sector(&mut self, x: &Transfer, slot: usize, field: usize) -> bool {
        let Some(n) = self.selected() else {
            return false;
        };
        let Some((cylinder, _)) = self.medium_track(n, x.head, x.mfm) else {
            return false;
        };
        self.drives[n].floppy.as_mut().is_some_and(|f| {
            f.write(cylinder, x.head, slot, &self.buffer.0[..field])
                .is_ok()
        })
    }

    /// After a sector: the command ends at terminal count or past the
    /// cylinder's last sector, and otherwise looks for the next.
    fn after_sector(&mut self, x: Transfer, at: Nanos) {
        let (head, id, past) = next_id(&x, 1);
        if x.terminal {
            self.finish(x, 0, x.st1, x.st2, id, at);
        } else if past {
            self.finish(
                x,
                st0::ABNORMAL,
                x.st1 | st1::END_OF_CYLINDER,
                x.st2,
                id,
                at,
            );
        } else {
            self.search(Transfer { head, id, ..x }, at);
        }
    }

    /// A scanned sector did not satisfy the scan: it moves on by its step,
    /// unless that was its last.
    fn scan_on(&mut self, x: Transfer, at: Nanos) {
        let (head, id, past) = next_id(&x, x.dtl);
        if x.terminal || past || (x.st2 & st2::CONTROL_MARK != 0 && !x.skip) {
            self.finish(x, 0, x.st1, x.st2 | st2::SCAN_NOT_SATISFIED, id, at);
        } else {
            self.search(Transfer { head, id, ..x }, at);
        }
    }

    /// A READ TRACK has read a sector: it reads on, errors and all, until
    /// terminal count or EOT sectors.
    fn track_on(&mut self, mut x: Transfer, at: Nanos) {
        x.count = x.count.saturating_add(1);
        let id = SectorId {
            r: x.id.r.wrapping_add(1),
            ..x.id
        };
        let failed = x.st1 & (st1::DATA_ERROR | st1::NO_DATA) != 0
            || x.st2 & (st2::DATA_CRC | st2::MISSING_DATA_MARK) != 0;
        if x.terminal {
            self.finish(
                x,
                if failed { st0::ABNORMAL } else { 0 },
                x.st1,
                x.st2,
                id,
                at,
            );
        } else if x.count >= x.eot {
            self.finish(
                x,
                st0::ABNORMAL,
                x.st1 | st1::END_OF_CYLINDER,
                x.st2,
                id,
                at,
            );
        } else {
            self.search(Transfer { id, ..x }, at);
        }
    }

    /// The format reaches its index, an ID byte from the host, or the
    /// index after its last sector, where the medium takes the track.
    fn format_step<M: DmaMemory>(
        &mut self,
        mut x: Transfer,
        at: Nanos,
        dma: &mut Dma,
        memory: &mut M,
    ) {
        let (
            Stage::Format {
                index,
                begun,
                sector,
                byte,
            },
            Operation::Format {
                sectors, filler, ..
            },
        ) = (x.stage, x.operation)
        else {
            return self.finish(x, st0::ABNORMAL, x.st1, x.st2, x.id, at);
        };
        if !begun {
            x.stage = Stage::Format {
                index,
                begun: true,
                sector,
                byte,
            };
            self.phase = Phase::Execute(x);
            return;
        }
        if sector >= sectors {
            return match self.format_medium(&x, sectors, filler) {
                Ok(last) => self.finish(x, 0, 0, 0, last, at),
                Err(st1) => self.finish(x, st0::ABNORMAL, st1, 0, x.id, at),
            };
        }
        let Some((value, _)) = self.take(dma, memory) else {
            return self.finish(x, st0::ABNORMAL, st1::OVERRUN, 0, x.id, at);
        };
        self.buffer.0[usize::from(sector) * 4 + usize::from(byte)] = value;
        let (sector, byte) = if byte == 3 {
            (sector + 1, 0)
        } else {
            (sector, byte + 1)
        };
        x.stage = Stage::Format {
            index,
            begun,
            sector,
            byte,
        };
        self.phase = Phase::Execute(x);
    }

    /// Writes a formatted track to a raw image, which takes it only as its
    /// own layout again: every sector of it, each once. Returns the last ID
    /// written, or the ST1 bit refusing it.
    fn format_medium(&mut self, x: &Transfer, sectors: u8, filler: u8) -> Result<SectorId, u8> {
        let n = self.selected().ok_or(st1::NOT_WRITABLE)?;
        let (cylinder, track) = self
            .medium_track(n, x.head, x.mfm)
            .ok_or(st1::NOT_WRITABLE)?;
        let layout = track.sectors();
        if layout.len() != usize::from(sectors) {
            return Err(st1::NOT_WRITABLE);
        }
        let mut placed = 0u64;
        let mut last = x.id;
        for &[c, h, r, n] in self.buffer.0[..layout.len() * 4].as_chunks::<4>().0 {
            let id = SectorId { c, h, r, n };
            let slot = layout
                .iter()
                .position(|s| s.id == id)
                .filter(|&slot| placed & (1 << slot) == 0)
                .ok_or(st1::NOT_WRITABLE)?;
            placed |= 1 << slot;
            last = id;
        }
        let floppy = self.drives[n].floppy.as_mut().ok_or(st1::NOT_WRITABLE)?;
        for (slot, sector) in layout.iter().enumerate() {
            let bytes = sector.id.bytes();
            self.buffer.0[..bytes].fill(filler);
            floppy
                .write(cylinder, x.head, slot, &self.buffer.0[..bytes])
                .map_err(|_| st1::DATA_ERROR)?;
        }
        Ok(last)
    }

    fn finish(&mut self, x: Transfer, ic: u8, st1: u8, st2: u8, id: SectorId, at: Nanos) {
        let st0 = ic | (x.head << 2) | x.unit;
        self.result(&[st0, st1, st2, id.c, id.h, id.r, id.n], at, true);
        self.loaded_until = at + self.head_unload_time();
        self.pio = None;
    }
}

crate::snapshot::state_enum!(Scan {
    0 => Equal,
    1 => LowOrEqual,
    2 => HighOrEqual,
});

crate::snapshot::state!(Compare { equal, low, high });

crate::snapshot::state_enum!(Operation {
    0 => Read { deleted },
    1 => Track,
    2 => Write { deleted },
    3 => Scan(scan),
    4 => ReadId,
    5 => Format { sectors, gap, filler },
});

impl Default for Stage {
    fn default() -> Self {
        Self::Search {
            from: 0,
            at: None,
            found: None,
            fail: (0, 0),
        }
    }
}

crate::snapshot::state_enum!(Stage {
    0 => Search { from, at, found, fail },
    1 => Data { slot, start, len, field, done, compare },
    2 => Format { index, begun, sector, byte },
});

crate::snapshot::state!(Transfer {
    operation,
    unit,
    head,
    multitrack,
    mfm,
    skip,
    id,
    eot,
    dtl,
    st1,
    st2,
    count,
    terminal,
    stage,
});

crate::snapshot::state_enum!(Phase {
    0 => Idle,
    1 => Command { bytes, len },
    2 => Execute(transfer),
    3 => Result { bytes, len, read, at, interrupts },
});

crate::snapshot::state!(Seek {
    head,
    target,
    outward,
    pulses,
    next,
    step,
});

crate::snapshot::state!(Specify {
    srt,
    hut,
    hlt,
    non_dma
});

/// A drive keeps whether it held a disk, not the disk: the images are the
/// front-end's to hand over again, so a drive that held one is restored
/// empty with its disk-change line set, as if the disk had been taken out.
impl State for Drive {
    fn save(&self, out: &mut Out) {
        let Self {
            kind: _,
            track,
            floppy,
            changed,
            turning,
        } = self;
        track.save(out);
        floppy.is_some().save(out);
        changed.save(out);
        turning.save(out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        let Self {
            kind: _,
            track,
            floppy,
            changed,
            turning,
        } = self;
        track.restore(input)?;
        let mut held = false;
        held.restore(input)?;
        changed.restore(input)?;
        turning.restore(input)?;
        *floppy = None;
        *changed |= held;
        if held {
            *turning = None;
        }
        Ok(())
    }
}

impl State for Buffer {
    fn save(&self, out: &mut Out) {
        save_bytes(&self.0, out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        restore_bytes(&mut self.0, input)
    }
}

/// Whether a restored phase is one the controller can be in: its bytes
/// within their buffers and its transfer on a drive and within the data
/// buffer.
fn possible(phase: &Phase) -> bool {
    match *phase {
        Phase::Idle => true,
        Phase::Command { len, .. } => len <= 9,
        Phase::Result { len, read, .. } => len <= 7 && read <= len,
        Phase::Execute(x) => {
            usize::from(x.unit) < 4
                && match x.stage {
                    Stage::Data {
                        len, field, done, ..
                    } => done <= len && len <= field && field <= MAX_DATA,
                    Stage::Search { .. } | Stage::Format { .. } => true,
                }
        }
    }
}

crate::snapshot::state!(
    Fdc {
        drives,
        dor,
        ccr,
        phase,
        pcn,
        status,
        seeks,
        specify,
        poll,
        loaded_until,
        pio,
        last,
        now,
        buffer,
    }
    as built { interface }
    check |fdc: &Fdc| possible(&fdc.phase)
);

#[cfg(test)]
#[path = "fdc_tests.rs"]
mod tests;
