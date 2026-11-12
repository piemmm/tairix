//! The XT fixed-disk adapter: a controller taking IBM's six-byte command
//! block, with one sector buffer, and up to two ST-506 drives.
//!
//! The controller reads a sector into its buffer as it passes the head, then
//! hands the buffer to the host over DMA channel 3 at the DMA controller's
//! pace, or a byte at a time through the data port. A disk formatted with too
//! tight an interleave therefore misses its next sector and waits a
//! revolution, as the hardware did. Each command ends with a status byte and
//! IRQ 5, and REQUEST SENSE says what went wrong.
//!
//! An image holds sector data only. The interleave and the tracks a format
//! marked bad are the drive's [`LowLevel`] format, which its owner keeps
//! beside the image; the ECC that READ LONG and WRITE LONG carry reads as
//! zeros and is not kept.

use super::{BlockDevice, SECTOR};
use crate::chips::dma::{Dma, DmaMemory};
use crate::snapshot::{check, restore_fitted, save_fitted, In, Out, State, Torn};
use crate::time::{Hertz, Nanos, NANOS_PER_MILLISECOND as MS, NANOS_PER_SECOND};
use alloc::boxed::Box;
use alloc::vec::Vec;

/// The DMA channel the adapter requests.
pub const DMA_CHANNEL: usize = 3;
/// The IRQ the adapter raises.
pub const IRQ: u8 = 5;

const US: Nanos = NANOS_PER_SECOND / 1_000_000;
/// The drives turn at 3600 RPM.
const RPM: u128 = 3600;
const MINUTE: u128 = 60 * NANOS_PER_SECOND as u128;
/// The most sectors a track holds: RLL's 26.
const MAX_SECTORS: usize = 26;
/// The ECC READ LONG and WRITE LONG carry after a sector.
const ECC: usize = 4;
/// The controller's own work on a command before it acts, and between one
/// sector and the next: set so its best interleave is 3, as the XT adapter's
/// was.
const COMMAND_TIME: Nanos = MS;
const SECTOR_TIME: Nanos = 600 * US;
/// A diagnostic's run.
const DIAGNOSTIC_TIME: Nanos = 50 * MS;
/// The address INITIALIZE DRIVE CHARACTERISTICS has not yet narrowed: the
/// most the command block can name.
const WIDEST: Limits = Limits {
    cylinders: 1024,
    heads: 16,
};

/// The most tracks a drive the adapter addresses has, and so the most a
/// low-level format can mark bad.
pub const MAX_TRACKS: usize = WIDEST.cylinders as usize * WIDEST.heads as usize;

/// The hardware status register's bits.
pub mod status {
    /// A byte is asked for, or offered.
    pub const REQ: u8 = 0x01;
    /// It moves to the host.
    pub const IO: u8 = 0x02;
    /// It is a command or status byte, not data.
    pub const CONTROL: u8 = 0x04;
    /// A command is under way.
    pub const BUSY: u8 = 0x08;
    /// A DMA transfer is asked for.
    pub const DRQ: u8 = 0x10;
    /// The controller interrupts.
    pub const INT: u8 = 0x20;
}

/// The completion status byte's bits.
pub mod completion {
    /// The command failed: REQUEST SENSE says why.
    pub const ERROR: u8 = 0x02;
    /// It was drive 1's.
    pub const UNIT: u8 = 0x20;
}

/// The DMA and interrupt mask register's bits.
mod mask {
    pub const DMA: u8 = 0x01;
    pub const IRQ: u8 = 0x02;
}

/// REQUEST SENSE's error codes.
mod sense {
    pub const WRITE_FAULT: u8 = 0x03;
    pub const NOT_READY: u8 = 0x04;
    pub const DATA_ERROR: u8 = 0x11;
    pub const NO_ADDRESS_MARK: u8 = 0x12;
    pub const SEEK_ERROR: u8 = 0x15;
    pub const BAD_TRACK: u8 = 0x19;
    pub const INVALID_COMMAND: u8 = 0x20;
    pub const ILLEGAL_ADDRESS: u8 = 0x21;
    /// The sense names the address the error happened at.
    pub const ADDRESS_VALID: u8 = 0x80;
}

/// How the controller records a track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Recording {
    /// MFM at 5 Mb/s: 17 sectors a track.
    Mfm,
    /// RLL 2,7 at 7.5 Mb/s: 26 sectors a track.
    Rll,
}

impl Recording {
    /// The sectors a track holds.
    #[must_use]
    pub const fn sectors(self) -> u8 {
        match self {
            Self::Mfm => 17,
            Self::Rll => 26,
        }
    }
}

/// A drive: its geometry and how fast its heads move.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DiskType {
    /// Cylinders, at most 1024.
    pub cylinders: u16,
    /// Heads, at most 16.
    pub heads: u8,
    /// A seek to the next cylinder, settling included.
    pub track_to_track: Nanos,
    /// A seek across a third of the cylinders: the average of random ones.
    pub average_seek: Nanos,
}

/// A drive's low-level format, which a raw image does not record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LowLevel {
    /// Physical sectors from one logical sector to the next.
    pub interleave: u8,
    /// Tracks FORMAT BAD TRACK marked, each `cylinder * heads + head`.
    pub bad_tracks: Vec<u32>,
}

/// Why a drive could not be attached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AttachError {
    /// The adapter has drives 0 and 1 only.
    NoSuchDrive,
    /// The geometry is past what the adapter addresses, or a seek is
    /// faster to the average than to the next track.
    Geometry,
    /// The image is not the size the geometry holds.
    Size,
    /// A bad track lies past the geometry.
    BadTrack,
}

/// The geometry INITIALIZE DRIVE CHARACTERISTICS gave a drive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Limits {
    cylinders: u16,
    heads: u8,
}

/// A sector's address in the command block.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Address {
    cylinder: u16,
    head: u8,
    sector: u8,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Command {
    #[default]
    TestReady,
    Recalibrate,
    Sense,
    FormatDrive,
    Verify,
    FormatTrack,
    FormatBad,
    Read,
    Write,
    Seek,
    Initialize,
    EccBurst,
    ReadBuffer,
    WriteBuffer,
    /// The RAM and controller diagnostics, or with `drive` the drive's.
    Diagnostic {
        drive: bool,
    },
    ReadLong,
    WriteLong,
    /// An opcode the controller does not have.
    Invalid,
}

impl Command {
    const fn decode(op: u8) -> Self {
        match op {
            0x00 => Self::TestReady,
            0x01 => Self::Recalibrate,
            0x03 => Self::Sense,
            0x04 => Self::FormatDrive,
            0x05 => Self::Verify,
            0x06 => Self::FormatTrack,
            0x07 => Self::FormatBad,
            0x08 => Self::Read,
            0x0A => Self::Write,
            0x0B => Self::Seek,
            0x0C => Self::Initialize,
            0x0D => Self::EccBurst,
            0x0E => Self::ReadBuffer,
            0x0F => Self::WriteBuffer,
            0xE0 | 0xE4 => Self::Diagnostic { drive: false },
            0xE3 => Self::Diagnostic { drive: true },
            0xE5 => Self::ReadLong,
            0xE6 => Self::WriteLong,
            _ => Self::Invalid,
        }
    }

    /// Whether it needs its drive to be there.
    const fn uses_drive(self) -> bool {
        !matches!(
            self,
            Self::Sense
                | Self::Initialize
                | Self::EccBurst
                | Self::ReadBuffer
                | Self::WriteBuffer
                | Self::Diagnostic { drive: false }
                | Self::Invalid
        )
    }

    /// Whether it addresses a track, which must lie within the drive's
    /// characteristics.
    const fn addresses(self) -> bool {
        matches!(
            self,
            Self::Seek | Self::FormatTrack | Self::FormatBad | Self::FormatDrive
        ) || self.addresses_sector()
    }

    const fn addresses_sector(self) -> bool {
        matches!(
            self,
            Self::Read | Self::Write | Self::Verify | Self::ReadLong | Self::WriteLong
        )
    }

    const fn to_host(self) -> bool {
        matches!(
            self,
            Self::Read | Self::ReadLong | Self::Sense | Self::EccBurst | Self::ReadBuffer
        )
    }

    /// Bytes it moves to or from the host at a time.
    const fn host_bytes(self) -> usize {
        match self {
            Self::Sense => 4,
            Self::Initialize => 8,
            Self::EccBurst => 1,
            Self::ReadLong | Self::WriteLong => SECTOR + ECC,
            _ => SECTOR,
        }
    }
}

/// Where a command is.
#[derive(Clone, Copy, Debug)]
enum Step {
    /// The controller acts at this instant: a seek has settled, or its own
    /// work is done.
    Wait(Nanos),
    /// The sector at the job's address, or for a format its whole track,
    /// has passed under the head by this instant.
    Media(Nanos),
    /// The buffer's first `len` bytes move to or from the host, `done` so
    /// far. Over DMA a burst moves byte `first` at `start` and one more each
    /// transfer after, stalling when the channel does not answer; through
    /// the data port the host paces it.
    Host {
        len: usize,
        done: usize,
        burst: Option<(Nanos, usize)>,
    },
}

#[derive(Clone, Copy, Debug, Default)]
struct Job {
    command: Command,
    /// The error it ends with once the controller has looked at it.
    refused: Option<u8>,
    drive: usize,
    address: Address,
    /// Sectors left to move.
    count: u16,
    /// The interleave a format lays down.
    interleave: u8,
    control: u8,
    step: Step,
}

#[derive(Clone, Copy, Debug)]
enum Phase {
    Idle,
    /// Selected, taking the command block.
    Command {
        bytes: [u8; 6],
        len: usize,
    },
    Busy(Job),
    /// The completion status, waiting to be read.
    Status(u8),
}

/// A drive and its image.
struct Disk {
    kind: DiskType,
    device: Box<dyn BlockDevice>,
    /// The cylinder the heads are over.
    cylinder: u16,
    format: LowLevel,
    /// Each logical sector's physical slot, as the interleave lays them.
    slots: [u8; MAX_SECTORS],
}

impl core::fmt::Debug for Disk {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Disk")
            .field("kind", &self.kind)
            .field("cylinder", &self.cylinder)
            .field("format", &self.format)
            .finish_non_exhaustive()
    }
}

/// The buffer a sector and its ECC, or a command's data, pass through.
struct Buffer {
    sector: [u8; SECTOR],
    ecc: [u8; ECC],
}

impl Buffer {
    fn byte(&self, k: usize) -> u8 {
        self.sector
            .get(k)
            .copied()
            .unwrap_or_else(|| self.ecc[(k - SECTOR) % ECC])
    }

    fn byte_mut(&mut self, k: usize) -> &mut u8 {
        match self.sector.get_mut(k) {
            Some(byte) => byte,
            None => &mut self.ecc[(k - SECTOR) % ECC],
        }
    }
}

impl core::fmt::Debug for Buffer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Buffer").finish_non_exhaustive()
    }
}

/// Drive `n`'s bit in a status byte and in the sense.
const fn unit_bit(n: usize) -> u8 {
    if n == 1 {
        completion::UNIT
    } else {
        0
    }
}

/// Each logical sector's physical slot on a track of `sectors`, laid
/// `interleave` slots apart and on to the next free one where that is
/// taken. An interleave the track repeats on is 1.
fn interleaved(sectors: u8, interleave: u8) -> [u8; MAX_SECTORS] {
    let n = usize::from(sectors).min(MAX_SECTORS);
    let step = match usize::from(interleave) % n {
        0 => 1,
        step => step,
    };
    let mut slots = [0; MAX_SECTORS];
    let mut taken = [false; MAX_SECTORS];
    let mut slot = 0;
    for (logical, place) in (0u8..).zip(slots.iter_mut().take(n)) {
        while taken[slot] {
            slot = (slot + 1) % n;
        }
        *place = u8::try_from(slot).unwrap_or(logical);
        taken[slot] = true;
        slot = (slot + step) % n;
    }
    slots
}

/// The adapter, its controller and drives.
#[derive(Debug)]
pub struct Hdc {
    recording: Recording,
    /// The drive-type jumpers 322h reads.
    jumpers: u8,
    /// The DMA controller's rate of transfers.
    transfers: Hertz,
    drives: [Option<Disk>; 2],
    limits: [Limits; 2],
    sense: [[u8; 4]; 2],
    mask: u8,
    phase: Phase,
    interrupt: bool,
    /// What the data port last held.
    last: u8,
    now: Nanos,
    buffer: Buffer,
}

impl Hdc {
    /// An adapter recording as `recording`, its jumpers set to `jumpers`
    /// (bits 0–1 drive 1's type, bits 2–3 drive 0's), on a bus whose DMA
    /// controller makes `transfers` a second. It has no drives yet.
    #[must_use]
    pub fn new(recording: Recording, jumpers: u8, transfers: Hertz) -> Self {
        Self {
            recording,
            jumpers: jumpers & 0x0F,
            transfers,
            drives: [None, None],
            limits: [WIDEST; 2],
            sense: [[0; 4]; 2],
            mask: 0,
            phase: Phase::Idle,
            interrupt: false,
            last: 0,
            now: 0,
            buffer: Buffer {
                sector: [0; SECTOR],
                ecc: [0; ECC],
            },
        }
    }

    /// Attaches drive `n`: a drive of `kind` holding `device`, formatted as
    /// `format`.
    pub fn attach(
        &mut self,
        n: usize,
        kind: DiskType,
        device: Box<dyn BlockDevice>,
        mut format: LowLevel,
    ) -> Result<(), AttachError> {
        let slot = self.drives.get_mut(n).ok_or(AttachError::NoSuchDrive)?;
        if !(1..=WIDEST.cylinders).contains(&kind.cylinders)
            || !(1..=WIDEST.heads).contains(&kind.heads)
            || kind.average_seek < kind.track_to_track
        {
            return Err(AttachError::Geometry);
        }
        let tracks = u32::from(kind.cylinders) * u32::from(kind.heads);
        if device.sectors() != u64::from(tracks) * u64::from(self.recording.sectors()) {
            return Err(AttachError::Size);
        }
        format.bad_tracks.sort_unstable();
        format.bad_tracks.dedup();
        if format
            .bad_tracks
            .last()
            .is_some_and(|&track| track >= tracks)
        {
            return Err(AttachError::BadTrack);
        }
        let slots = interleaved(self.recording.sectors(), format.interleave);
        *slot = Some(Disk {
            kind,
            device,
            cylinder: 0,
            format,
            slots,
        });
        Ok(())
    }

    /// Drive `n`'s low-level format as it stands, for its owner to keep.
    #[must_use]
    pub fn low_level(&self, n: usize) -> Option<&LowLevel> {
        self.drives.get(n)?.as_ref().map(|disk| &disk.format)
    }

    /// The DMA controller now makes `transfers` a second.
    pub fn set_transfer_rate(&mut self, transfers: Hertz) {
        self.transfers = transfers;
    }

    /// The drive the command under way is for, while one is: what its lamp
    /// shows.
    #[must_use]
    pub const fn active(&self) -> Option<usize> {
        match self.phase {
            Phase::Busy(job) => Some(job.drive),
            _ => None,
        }
    }

    /// Whether the adapter drives IRQ 5: a command has ended and the mask
    /// register lets the interrupt out.
    #[must_use]
    pub fn irq(&self) -> bool {
        self.interrupt && self.mask & mask::IRQ != 0
    }

    /// The next instant the adapter acts on its own, if it will.
    #[must_use]
    pub fn next_event(&self) -> Option<Nanos> {
        let Phase::Busy(job) = self.phase else {
            return None;
        };
        match job.step {
            Step::Wait(at) | Step::Media(at) => Some(at),
            Step::Host {
                done,
                burst: Some((start, first)),
                ..
            } => Some(start + self.transfers.time_of((done - first) as u64)),
            Step::Host { burst: None, .. } => None,
        }
    }

    /// Advances the adapter to `t`, acting on everything due by then. A DMA
    /// transfer the channel had stalled tries again, so the board calls this
    /// once more after the DMA controller is reprogrammed.
    pub fn advance<M: DmaMemory>(&mut self, t: Nanos, dma: &mut Dma, memory: &mut M) {
        if let Phase::Busy(mut job) = self.phase {
            if let Step::Host {
                len,
                done,
                burst: None,
            } = job.step
            {
                if self.mask & mask::DMA != 0 {
                    job.step = Step::Host {
                        len,
                        done,
                        burst: Some((t.max(self.now), done)),
                    };
                    self.phase = Phase::Busy(job);
                }
            }
        }
        while let Some(at) = self.next_event().filter(|&at| at <= t) {
            self.now = self.now.max(at);
            self.step(at, dma, memory);
        }
        self.now = self.now.max(t);
    }

    /// Reads port `port` at `t`: 320h data, 321h the hardware status, 322h
    /// the drive-type jumpers. 323h is write-only.
    pub fn input<M: DmaMemory>(
        &mut self,
        port: u16,
        t: Nanos,
        dma: &mut Dma,
        memory: &mut M,
    ) -> Option<u8> {
        self.advance(t, dma, memory);
        match port & 3 {
            0 => Some(self.read_data()),
            1 => Some(self.hardware_status()),
            2 => Some(self.jumpers),
            _ => None,
        }
    }

    /// Writes port `port` at `t`: 320h data, 321h a reset, 322h selects the
    /// controller, 323h the DMA and interrupt mask.
    pub fn output<M: DmaMemory>(
        &mut self,
        port: u16,
        value: u8,
        t: Nanos,
        dma: &mut Dma,
        memory: &mut M,
    ) {
        self.advance(t, dma, memory);
        match port & 3 {
            0 => self.write_data(value, t),
            1 => {
                self.phase = Phase::Idle;
                self.interrupt = false;
                self.limits = [WIDEST; 2];
                self.sense = [[0; 4]; 2];
            }
            2 => {
                if matches!(self.phase, Phase::Idle) {
                    self.phase = Phase::Command {
                        bytes: [0; 6],
                        len: 0,
                    };
                }
            }
            _ => {
                self.mask = value & (mask::DMA | mask::IRQ);
                self.advance(t, dma, memory);
            }
        }
    }

    fn hardware_status(&self) -> u8 {
        let int = if self.interrupt { status::INT } else { 0 };
        int | match self.phase {
            Phase::Idle => 0,
            Phase::Command { .. } => status::BUSY | status::CONTROL | status::REQ,
            Phase::Busy(Job {
                step: Step::Host { .. },
                command,
                ..
            }) if self.mask & mask::DMA == 0 => {
                status::BUSY | status::REQ | if command.to_host() { status::IO } else { 0 }
            }
            Phase::Busy(Job {
                step: Step::Host { .. },
                ..
            }) => status::BUSY | status::DRQ,
            Phase::Busy(_) => status::BUSY,
            Phase::Status(_) => status::BUSY | status::CONTROL | status::IO | status::REQ,
        }
    }

    fn read_data(&mut self) -> u8 {
        match self.phase {
            Phase::Status(byte) => {
                self.phase = Phase::Idle;
                self.interrupt = false;
                self.last = byte;
            }
            Phase::Busy(mut job) if self.mask & mask::DMA == 0 && job.command.to_host() => {
                if let Step::Host { len, done, burst } = job.step {
                    self.last = self.buffer.byte(done);
                    job.step = Step::Host {
                        len,
                        done: done + 1,
                        burst,
                    };
                    self.hosted_byte(job, self.now);
                }
            }
            _ => {}
        }
        self.last
    }

    fn write_data(&mut self, value: u8, t: Nanos) {
        match self.phase {
            Phase::Command { mut bytes, len } => {
                bytes[len] = value;
                if len + 1 < bytes.len() {
                    self.phase = Phase::Command {
                        bytes,
                        len: len + 1,
                    };
                } else {
                    self.start(bytes, t);
                }
            }
            Phase::Busy(mut job) if self.mask & mask::DMA == 0 && !job.command.to_host() => {
                if let Step::Host { len, done, burst } = job.step {
                    *self.buffer.byte_mut(done) = value;
                    job.step = Step::Host {
                        len,
                        done: done + 1,
                        burst,
                    };
                    self.hosted_byte(job, t);
                }
            }
            _ => {}
        }
    }

    /// A host byte has moved: the job goes on, or the transfer is done.
    fn hosted_byte(&mut self, job: Job, at: Nanos) {
        match job.step {
            Step::Host { len, done, .. } if done >= len => self.hosted(job, at),
            _ => self.phase = Phase::Busy(job),
        }
    }

    fn start(&mut self, c: [u8; 6], t: Nanos) {
        let drive = usize::from((c[1] >> 5) & 1);
        let address = Address {
            cylinder: (u16::from(c[2] & 0xC0) << 2) | u16::from(c[3]),
            head: c[1] & 0x1F,
            sector: c[2] & 0x3F,
        };
        let at = t + COMMAND_TIME;
        let command = Command::decode(c[0]);
        let limits = self.limits[drive];
        let beyond = address.cylinder >= limits.cylinders
            || address.head >= limits.heads
            || (command.addresses_sector() && address.sector >= self.recording.sectors());
        let refused = match command {
            Command::Invalid => Some(sense::INVALID_COMMAND),
            _ if command.uses_drive() && self.drives[drive].is_none() => Some(sense::NOT_READY),
            _ if command.addresses() && beyond => Some(sense::ILLEGAL_ADDRESS),
            _ => None,
        };
        let count = if c[4] == 0 { 256 } else { u16::from(c[4]) };
        let mut job = Job {
            command,
            refused,
            drive,
            address,
            count,
            interleave: c[4],
            control: c[5],
            step: Step::Wait(at),
        };
        if refused.is_some() {
            self.phase = Phase::Busy(job);
            return;
        }
        let seek = match command {
            Command::Recalibrate => Some(0),
            _ if command.addresses() => Some(address.cylinder),
            _ => None,
        };
        let wait = match (seek, command) {
            (Some(cylinder), _) => self.seek(drive, cylinder, job.control),
            (None, Command::Diagnostic { .. }) => DIAGNOSTIC_TIME,
            _ => 0,
        };
        job.step = Step::Wait(at + wait);
        self.phase = Phase::Busy(job);
    }

    /// Moves drive `n`'s heads toward `cylinder`, as far as the drive goes,
    /// returning how long that takes. Step option 4 and 5 buffer the step
    /// pulses for the drive to seek at its own pace; the rest step it at
    /// ST-506's 3 ms.
    fn seek(&mut self, n: usize, cylinder: u16, control: u8) -> Nanos {
        let Some(disk) = self.drives[n].as_mut() else {
            return 0;
        };
        let target = cylinder.min(disk.kind.cylinders - 1);
        let tracks = u64::from(disk.cylinder.abs_diff(target));
        disk.cylinder = target;
        if tracks == 0 {
            return 0;
        }
        let pulse = match control & 7 {
            4 => 200 * US,
            5 => 70 * US,
            _ => 3 * MS,
        };
        let kind = disk.kind;
        // Seek time grows as the square root of the distance, through the
        // average at a third of the way across.
        let reach = ((tracks - 1) * 3_000_000 / u64::from(kind.cylinders)).isqrt();
        let profile =
            kind.track_to_track + (kind.average_seek - kind.track_to_track) * reach / 1000;
        profile.max(tracks * pulse)
    }

    /// The instant boundary `u` between physical slots passes the head,
    /// counting from when the drives started turning.
    fn boundary(&self, u: u64) -> Nanos {
        let at = u128::from(u) * MINUTE / (RPM * u128::from(self.recording.sectors()));
        u64::try_from(at).unwrap_or(u64::MAX)
    }

    /// The first boundary at or after `t` that begins physical slot `slot`.
    fn slot_boundary(&self, slot: u8, t: Nanos) -> u64 {
        let sectors = u64::from(self.recording.sectors());
        let mut u =
            u64::try_from(u128::from(t) * RPM * u128::from(sectors) / MINUTE).unwrap_or(u64::MAX);
        while self.boundary(u) < t {
            u += 1;
        }
        u + (u64::from(slot) + sectors - u % sectors) % sectors
    }

    fn lba(&self, n: usize, address: Address) -> u64 {
        let heads = self.drives[n].as_ref().map_or(0, |disk| disk.kind.heads);
        (u64::from(address.cylinder) * u64::from(heads) + u64::from(address.head))
            * u64::from(self.recording.sectors())
            + u64::from(address.sector)
    }

    /// Whether drive `n` can reach `address`: the heads stop at its last
    /// cylinder, a head past its own reads nothing, and a track marked bad
    /// is refused.
    fn reach(&self, n: usize, address: Address) -> Result<(), u8> {
        let disk = self.drives[n].as_ref().ok_or(sense::NOT_READY)?;
        if address.cylinder >= disk.kind.cylinders {
            return Err(sense::SEEK_ERROR);
        }
        if address.head >= disk.kind.heads {
            return Err(sense::NO_ADDRESS_MARK);
        }
        let track =
            u32::from(address.cylinder) * u32::from(disk.kind.heads) + u32::from(address.head);
        if disk.format.bad_tracks.binary_search(&track).is_ok() {
            return Err(sense::BAD_TRACK);
        }
        Ok(())
    }

    fn step<M: DmaMemory>(&mut self, at: Nanos, dma: &mut Dma, memory: &mut M) {
        let Phase::Busy(mut job) = self.phase else {
            return;
        };
        match job.step {
            Step::Wait(_) => self.waited(job, at),
            Step::Media(_) => self.passed(job, at),
            Step::Host {
                len,
                done,
                burst: Some(burst),
            } => {
                let moved = if self.mask & mask::DMA == 0 {
                    0
                } else if job.command.to_host() {
                    dma.to_memory(DMA_CHANNEL, memory, &[self.buffer.byte(done)])
                        .bytes
                } else {
                    let mut byte = [0];
                    let moved = dma.from_memory(DMA_CHANNEL, memory, &mut byte).bytes;
                    *self.buffer.byte_mut(done) = byte[0];
                    moved
                };
                job.step = if moved == 0 {
                    Step::Host {
                        len,
                        done,
                        burst: None,
                    }
                } else {
                    Step::Host {
                        len,
                        done: done + 1,
                        burst: Some(burst),
                    }
                };
                self.hosted_byte(job, at);
            }
            Step::Host { burst: None, .. } => {}
        }
    }

    /// The controller's wait is over.
    fn waited(&mut self, mut job: Job, at: Nanos) {
        if let Some(code) = job.refused {
            return self.finish(job.drive, job.address, Err(code));
        }
        match job.command {
            Command::TestReady
            | Command::Recalibrate
            | Command::Seek
            | Command::Diagnostic { .. }
            | Command::Invalid => {
                self.finish(job.drive, job.address, Ok(()));
            }
            Command::Sense | Command::EccBurst => {
                let reply = if job.command == Command::Sense {
                    self.sense[job.drive]
                } else {
                    [0; 4]
                };
                self.buffer.sector[..4].copy_from_slice(&reply);
                self.begin_host(job, at);
            }
            Command::Write
            | Command::WriteLong
            | Command::Initialize
            | Command::ReadBuffer
            | Command::WriteBuffer => {
                self.begin_host(job, at);
            }
            Command::FormatTrack | Command::FormatBad | Command::FormatDrive => {
                let index = self.boundary(self.slot_boundary(0, at));
                job.step = Step::Media(index + self.boundary(u64::from(self.recording.sectors())));
                self.phase = Phase::Busy(job);
            }
            Command::Read | Command::ReadLong | Command::Verify => self.begin_media(job, at),
        }
    }

    fn begin_host(&mut self, mut job: Job, at: Nanos) {
        let burst = (self.mask & mask::DMA != 0).then_some((at, 0));
        job.step = Step::Host {
            len: job.command.host_bytes(),
            done: 0,
            burst,
        };
        self.phase = Phase::Busy(job);
    }

    /// Waits for the sector at the job's address to pass the head.
    fn begin_media(&mut self, mut job: Job, at: Nanos) {
        if let Err(code) = self.reach(job.drive, job.address) {
            return self.finish(job.drive, job.address, Err(code));
        }
        let slot = self.drives[job.drive]
            .as_ref()
            .map_or(0, |disk| disk.slots[usize::from(job.address.sector)]);
        let u = self.slot_boundary(slot, at);
        job.step = Step::Media(self.boundary(u + 1));
        self.phase = Phase::Busy(job);
    }

    /// The sector, or a format's track, has passed the head.
    fn passed(&mut self, job: Job, at: Nanos) {
        let lba = self.lba(job.drive, job.address);
        match job.command {
            Command::Read | Command::ReadLong | Command::Verify => {
                let read = self.drives[job.drive]
                    .as_mut()
                    .is_some_and(|disk| disk.device.read(lba, &mut self.buffer.sector).is_ok());
                if !read {
                    return self.finish(job.drive, job.address, Err(sense::DATA_ERROR));
                }
                self.buffer.ecc = [0; ECC];
                if job.command == Command::Verify {
                    self.next_sector(job, at);
                } else {
                    self.begin_host(job, at);
                }
            }
            Command::Write | Command::WriteLong => {
                let written = self.drives[job.drive]
                    .as_mut()
                    .is_some_and(|disk| disk.device.write(lba, &self.buffer.sector).is_ok());
                if written {
                    self.next_sector(job, at);
                } else {
                    self.finish(job.drive, job.address, Err(sense::WRITE_FAULT));
                }
            }
            Command::FormatTrack | Command::FormatBad | Command::FormatDrive => {
                self.formatted(job, at);
            }
            _ => self.finish(job.drive, job.address, Ok(())),
        }
    }

    /// A track has been formatted: each sector written with the buffer, and
    /// the track marked bad or not.
    fn formatted(&mut self, mut job: Job, at: Nanos) {
        let sectors = self.recording.sectors();
        let first = self.lba(
            job.drive,
            Address {
                sector: 0,
                ..job.address
            },
        );
        let slots = interleaved(sectors, job.interleave);
        let Some(disk) = self.drives[job.drive].as_mut() else {
            return self.finish(job.drive, job.address, Err(sense::NOT_READY));
        };
        if job.address.cylinder >= disk.kind.cylinders {
            return self.finish(job.drive, job.address, Err(sense::SEEK_ERROR));
        }
        if job.address.head >= disk.kind.heads {
            return self.finish(job.drive, job.address, Err(sense::WRITE_FAULT));
        }
        let track = u32::from(job.address.cylinder) * u32::from(disk.kind.heads)
            + u32::from(job.address.head);
        if (0..u64::from(sectors))
            .any(|s| disk.device.write(first + s, &self.buffer.sector).is_err())
        {
            return self.finish(job.drive, job.address, Err(sense::WRITE_FAULT));
        }
        let bad = &mut disk.format.bad_tracks;
        match (bad.binary_search(&track), job.command == Command::FormatBad) {
            (Err(place), true) => bad.insert(place, track),
            (Ok(place), false) => {
                bad.remove(place);
            }
            _ => {}
        }
        disk.format.interleave = job.interleave;
        disk.slots = slots;
        if job.command != Command::FormatDrive {
            return self.finish(job.drive, job.address, Ok(()));
        }
        let limits = self.limits[job.drive];
        let next = if job.address.head + 1 < limits.heads {
            Address {
                head: job.address.head + 1,
                ..job.address
            }
        } else {
            Address {
                cylinder: job.address.cylinder + 1,
                head: 0,
                ..job.address
            }
        };
        if next.cylinder >= limits.cylinders {
            return self.finish(job.drive, job.address, Ok(()));
        }
        let seek = self.seek(job.drive, next.cylinder, job.control);
        job.address = next;
        job.step = Step::Wait(at + SECTOR_TIME + seek);
        self.phase = Phase::Busy(job);
    }

    /// A host transfer is done.
    fn hosted(&mut self, job: Job, at: Nanos) {
        match job.command {
            Command::Read | Command::ReadLong => self.next_sector(job, at),
            Command::Write | Command::WriteLong => self.begin_media(job, at),
            Command::Initialize => {
                let b = &self.buffer.sector;
                self.limits[job.drive] = Limits {
                    cylinders: u16::from_be_bytes([b[0], b[1]]),
                    heads: b[2],
                };
                self.finish(job.drive, job.address, Ok(()));
            }
            Command::Sense => {
                self.sense[job.drive][0] = 0;
                let drive = unit_bit(job.drive);
                self.phase = Phase::Status(drive);
                self.interrupt = true;
            }
            _ => self.finish(job.drive, job.address, Ok(())),
        }
    }

    /// The next sector of a transfer: the command ends with its count, and
    /// otherwise moves on along the track, to the next head, and to the
    /// next cylinder.
    fn next_sector(&mut self, mut job: Job, at: Nanos) {
        job.count -= 1;
        if job.count == 0 {
            return self.finish(job.drive, job.address, Ok(()));
        }
        let limits = self.limits[job.drive];
        let a = job.address;
        let next = if a.sector + 1 < self.recording.sectors() {
            Address {
                sector: a.sector + 1,
                ..a
            }
        } else if a.head + 1 < limits.heads {
            Address {
                head: a.head + 1,
                sector: 0,
                ..a
            }
        } else {
            Address {
                cylinder: a.cylinder + 1,
                head: 0,
                sector: 0,
            }
        };
        if next.cylinder >= limits.cylinders {
            return self.finish(job.drive, a, Err(sense::ILLEGAL_ADDRESS));
        }
        let seek = if next.cylinder == a.cylinder {
            0
        } else {
            self.seek(job.drive, next.cylinder, job.control)
        };
        job.address = next;
        job.step = Step::Wait(at + SECTOR_TIME + seek);
        self.phase = Phase::Busy(job);
    }

    /// Ends a command: its status names the drive and any error, which
    /// REQUEST SENSE then reports with the address it happened at.
    fn finish(&mut self, drive: usize, address: Address, result: Result<(), u8>) {
        let unit = unit_bit(drive);
        let error = result.err();
        let code = error.map_or(0, |code| match code {
            sense::INVALID_COMMAND | sense::NOT_READY => code,
            _ => code | sense::ADDRESS_VALID,
        });
        let cylinder = address.cylinder.to_be_bytes();
        self.sense[drive] = [
            code,
            unit | address.head,
            ((cylinder[0] & 3) << 6) | address.sector,
            cylinder[1],
        ];
        self.phase = Phase::Status(
            unit | if error.is_some() {
                completion::ERROR
            } else {
                0
            },
        );
        self.interrupt = true;
    }
}

crate::snapshot::state!(Limits { cylinders, heads });

crate::snapshot::state!(Address {
    cylinder,
    head,
    sector,
});

crate::snapshot::state_enum!(Command {
    0 => TestReady,
    1 => Recalibrate,
    2 => Sense,
    3 => FormatDrive,
    4 => Verify,
    5 => FormatTrack,
    6 => FormatBad,
    7 => Read,
    8 => Write,
    9 => Seek,
    10 => Initialize,
    11 => EccBurst,
    12 => ReadBuffer,
    13 => WriteBuffer,
    14 => Diagnostic { drive },
    15 => ReadLong,
    16 => WriteLong,
    17 => Invalid,
});

impl Default for Step {
    fn default() -> Self {
        Self::Wait(0)
    }
}

crate::snapshot::state_enum!(Step {
    0 => Wait(at),
    1 => Media(at),
    2 => Host { len, done, burst },
});

crate::snapshot::state!(Job {
    command,
    refused,
    drive,
    address,
    count,
    interleave,
    control,
    step,
});

crate::snapshot::state_enum!(Phase {
    0 => Idle,
    1 => Command { bytes, len },
    2 => Busy(job),
    3 => Status(status),
});

crate::snapshot::state!(Buffer { sector, ecc });

/// A drive keeps where its heads are and the low-level format the guest
/// last laid down; the image is the front-end's, attached as the machine
/// was built.
impl State for Disk {
    fn save(&self, out: &mut Out) {
        let Self {
            kind: _,
            device: _,
            cylinder,
            format,
            slots: _,
        } = self;
        cylinder.save(out);
        format.interleave.save(out);
        out.u32(u32::try_from(format.bad_tracks.len()).unwrap_or(u32::MAX));
        for track in &format.bad_tracks {
            track.save(out);
        }
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        let tracks = u32::from(self.kind.cylinders) * u32::from(self.kind.heads);
        self.cylinder.restore(input)?;
        check(self.cylinder < self.kind.cylinders)?;
        let mut interleave = 0u8;
        interleave.restore(input)?;
        let len = input.u32()?;
        check(len <= tracks)?;
        let mut bad_tracks = Vec::new();
        bad_tracks
            .try_reserve_exact(len as usize)
            .map_err(|_| Torn::Mismatch)?;
        for _ in 0..len {
            let track = input.u32()?;
            check(track < tracks && bad_tracks.last().is_none_or(|&last| last < track))?;
            bad_tracks.push(track);
        }
        self.format = LowLevel {
            interleave,
            bad_tracks,
        };
        Ok(())
    }
}

/// Whether a restored job is one the controller could be running.
fn possible(phase: &Phase, now: Nanos, sectors: u8) -> bool {
    match *phase {
        Phase::Idle | Phase::Status(_) => true,
        Phase::Command { len, .. } => len <= 6,
        Phase::Busy(job) => {
            job.drive < 2
                && job.address.sector < sectors
                && match job.step {
                    Step::Wait(_) | Step::Media(_) => true,
                    Step::Host { len, done, burst } => {
                        done <= len
                            && len <= SECTOR + ECC
                            && burst.is_none_or(|(start, first)| {
                                first <= done && start <= now.saturating_add(NANOS_PER_SECOND)
                            })
                    }
                }
        }
    }
}

impl State for Hdc {
    fn save(&self, out: &mut Out) {
        let Self {
            recording: _,
            jumpers: _,
            transfers: _,
            drives,
            limits,
            sense,
            mask,
            phase,
            interrupt,
            last,
            now,
            buffer,
        } = self;
        for drive in drives {
            save_fitted(drive.as_ref(), out);
        }
        limits.save(out);
        sense.save(out);
        mask.save(out);
        phase.save(out);
        interrupt.save(out);
        last.save(out);
        now.save(out);
        buffer.save(out);
    }

    fn restore(&mut self, input: &mut In<'_>) -> Result<(), Torn> {
        let sectors = self.recording.sectors();
        let Self {
            recording: _,
            jumpers: _,
            transfers: _,
            drives,
            limits,
            sense,
            mask,
            phase,
            interrupt,
            last,
            now,
            buffer,
        } = self;
        for drive in drives.iter_mut() {
            restore_fitted(drive.as_mut(), input)?;
            if let Some(disk) = drive.as_mut() {
                disk.slots = interleaved(sectors, disk.format.interleave);
            }
        }
        limits.restore(input)?;
        sense.restore(input)?;
        mask.restore(input)?;
        phase.restore(input)?;
        interrupt.restore(input)?;
        last.restore(input)?;
        now.restore(input)?;
        buffer.restore(input)?;
        check(possible(phase, *now, sectors))
    }
}

#[cfg(test)]
#[path = "hdc_tests.rs"]
mod tests;
