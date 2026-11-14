//! The Intel 8271 floppy disc controller and the two drives it runs.
//!
//! The chip follows its data sheet (Intel 8271/8271-6, AFN-00223B) as the
//! reverse engineering of its microcode refines it
//! (scarybeastsecurity.blogspot.com, 2020): the parameter count is encoded in
//! the command byte, SPECIFY writes three consecutive special registers, and
//! the special registers are one file of 64. The BBC clocks it at 2 MHz, so
//! its timings are the data sheet's mini-floppy ones, and wires no DMA: in
//! non-DMA mode every data byte raises INT, the BBC's NMI, and a scan, which
//! needs DMA to fetch its key, cannot complete.
//!
//! Commands below `0x2C` act on the disc and end with a result and INT; the
//! rest act at once, and only Read Drive Status and Read Special Register
//! leave a result, with no interrupt. Commands and parameters are accepted as
//! they arrive, so the command-full and parameter-full status bits read clear.
//!
//! The disc turns at 300 rpm, so finding a sector waits for it to come round;
//! the chip steps by what its track register says, not by where the head is,
//! and an ID field naming another track makes it step on and look again.

use alloc::vec;
use alloc::vec::Vec;

use crate::media::disc::{self, Disc, Sector, SectorId, Track};

/// Cycles per byte under the head: 64 µs.
const BYTE_CYCLES: u64 = 128;
/// Cycles per revolution.
const REVOLUTION: u64 = disc::REVOLUTION_BYTES as u64 * BYTE_CYCLES;
/// How long the index hole holds INDEX active: about 4 ms at 300 rpm.
const INDEX_PULSE: u64 = 8_000;
/// Cycles the microcode takes to decode a command before acting on it.
const COMMAND_CYCLES: u64 = 400;
/// The step rate and head settling time count 2 ms, the head load time 8 ms.
const STEP_UNIT: u64 = 4_000;
const LOAD_UNIT: u64 = 16_000;
/// Above this track the chip asks the drive for low write current.
const LOW_CURRENT_TRACK: u8 = 43;
/// Index pulses a search for a sector waits through before it gives up.
const SEARCH_INDEXES: u64 = 2;
/// Times an ID field naming the wrong track makes the chip step on and look
/// again before it gives up.
const MISMATCH_STEPS: u8 = 2;
/// The head-unload index count that keeps the head loaded.
const STAY_LOADED: u8 = 15;
/// A sector formatted but never written holds this byte.
const FORMAT_FILL: u8 = 0xE5;

/// Status register bits.
pub mod status {
    /// A command is executing.
    pub const BUSY: u8 = 0x80;
    /// The command register has not yet taken its byte; this model takes it
    /// at once, so the bit reads clear.
    pub const COMMAND_FULL: u8 = 0x40;
    /// The parameter register has not yet taken its byte; clear here too.
    pub const PARAMETER_FULL: u8 = 0x20;
    /// The result register holds a result.
    pub const RESULT_FULL: u8 = 0x10;
    /// INT is asserted.
    pub const INT: u8 = 0x08;
    /// In non-DMA mode, a data byte awaits transfer through the data register.
    pub const DATA_REQUEST: u8 = 0x04;
}

/// Result codes; `DELETED` is added to any of them.
pub mod result {
    /// The command completed.
    pub const OK: u8 = 0x00;
    /// A data byte was not taken or supplied in time.
    pub const LATE_DMA: u8 = 0x0A;
    /// A data field's CRC did not match what was read.
    pub const DATA_CRC_ERROR: u8 = 0x0E;
    /// The addressed drive is not ready, or has been since the last Read
    /// Drive Status.
    pub const NOT_READY: u8 = 0x10;
    /// A write was refused by the write-protect sensor.
    pub const WRITE_PROTECTED: u8 = 0x12;
    /// No matching ID field passed the head.
    pub const SECTOR_NOT_FOUND: u8 = 0x18;
    /// A deleted-data mark was met.
    pub const DELETED: u8 = 0x20;
}

/// Special register numbers.
pub mod register {
    /// The sector being sought, and the last value a write to a special
    /// register passed through.
    pub const SCAN_SECTOR: u8 = 0x06;
    /// Step rate, in 2 ms units.
    pub const STEP_RATE: u8 = 0x0D;
    /// Head settling time, in 2 ms units.
    pub const SETTLE: u8 = 0x0E;
    /// Index pulses before the idle head unloads (high nibble) and head load
    /// time in 8 ms units (low nibble).
    pub const HEAD_LOAD: u8 = 0x0F;
    /// Surface 0's two bad tracks.
    pub const BAD_TRACKS_0: u8 = 0x10;
    /// Surface 0's track register.
    pub const TRACK_0: u8 = 0x12;
    /// Mode.
    pub const MODE: u8 = 0x17;
    /// Surface 1's two bad tracks.
    pub const BAD_TRACKS_1: u8 = 0x18;
    /// Surface 1's track register.
    pub const TRACK_1: u8 = 0x1A;
    /// The drive inputs.
    pub const DRIVE_INPUT: u8 = 0x22;
    /// The drive outputs.
    pub const DRIVE_OUTPUT: u8 = 0x23;
}

/// Drive output bits.
pub mod output {
    /// Writing.
    pub const WRITE_ENABLE: u8 = 0x01;
    /// The last step was inward.
    pub const DIRECTION: u8 = 0x04;
    /// The head is loaded; on the BBC the drive's motor runs with it.
    pub const LOAD_HEAD: u8 = 0x08;
    /// The head is past track 43.
    pub const LOW_CURRENT: u8 = 0x10;
    /// The side of a double-sided drive: on the BBC, DFS drive 2 rather than 0.
    pub const SIDE: u8 = 0x20;
    /// Drive 0 is selected.
    pub const SELECT_0: u8 = 0x40;
    /// Drive 1 is selected.
    pub const SELECT_1: u8 = 0x80;
    /// Both selects.
    pub const SELECT: u8 = SELECT_0 | SELECT_1;
}

/// Drive input bits, as Read Drive Status reports them.
pub mod input {
    /// The selected drive's head is at track 0.
    pub const TRACK_0: u8 = 0x02;
    /// Drive 0 is ready.
    pub const READY_0: u8 = 0x04;
    /// The selected drive's disc is write protected.
    pub const WRITE_PROTECT: u8 = 0x08;
    /// The selected drive's index hole is passing.
    pub const INDEX: u8 = 0x10;
    /// Drive 1 is ready.
    pub const READY_1: u8 = 0x40;
    /// Both ready bits.
    pub(super) const READY: u8 = READY_0 | READY_1;
}

/// Mode register bits.
mod mode {
    /// Data moves a byte at a time through INT rather than DMA.
    pub const NON_DMA: u8 = 0x01;
    /// Both drives share one actuator, and so one track register.
    pub const SINGLE_ACTUATOR: u8 = 0x02;
}

/// One of the machine's two physical drives. DFS drives 0 and 2 are the two
/// sides of drive 0, and 1 and 3 of drive 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PhysicalDrive {
    /// Drive 0.
    Zero,
    /// Drive 1.
    One,
}

impl PhysicalDrive {
    /// Both drives, in their order.
    pub const ALL: [Self; 2] = [Self::Zero, Self::One];

    /// Its place in [`ALL`](Self::ALL).
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Zero => 0,
            Self::One => 1,
        }
    }
}

/// A drive: a head over some track, and perhaps a disc.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Drive {
    disc: Option<Disc>,
    /// The physical track the head is over.
    head_track: u8,
}

impl Drive {
    /// The disc in the drive, if any.
    #[must_use]
    pub const fn disc(&self) -> Option<&Disc> {
        self.disc.as_ref()
    }

    /// The physical track the head is over.
    #[must_use]
    pub const fn head_track(&self) -> u8 {
        self.head_track
    }
}

/// What a disc command does once its head is on its track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Seek,
    /// Read, verify or scan sectors; a skipping command counts a deleted
    /// sector without moving it.
    Read {
        skip_deleted: bool,
        moves: Moves,
    },
    Write {
        deleted: bool,
    },
    ReadId,
    Format(Gaps),
}

/// Where a read's bytes go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Moves {
    /// To the CPU.
    Data,
    /// Nowhere: the CRC is checked.
    Nothing,
    /// Compared with a key DMA would fetch.
    Key,
}

/// A format's programmed gaps, each in bytes with its sync.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Gaps {
    /// From the index to the first ID field: gap 5 and the index mark, if
    /// any, then gap 1.
    lead: u16,
    /// After each data field.
    gap_3: u16,
}

impl Gaps {
    /// The gaps a format's parameters program: gap 3, gap 5 (zero for no
    /// index mark) and gap 1, each given as its size less its six bytes of
    /// sync.
    fn programmed(gap_3: u8, gap_5: u8, gap_1: u8) -> Self {
        let index = if gap_5 == 0 { 0 } else { u16::from(gap_5) + 7 };
        Self {
            lead: index + u16::from(gap_1) + 6,
            gap_3: u16::from(gap_3) + 6,
        }
    }

    /// Bytes from one formatted ID field to the next.
    fn stride(self, size: usize) -> usize {
        usize::from(disc::ID_TO_DATA_MARK + disc::DATA_MARK_BYTES + disc::CRC_BYTES + self.gap_3)
            + size
    }
}

/// How the chip steps to its track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Steps {
    /// Out until the track-0 sensor answers, which it does within the
    /// drive's travel.
    ToSensor,
    /// So many more, inward or out.
    Count { left: u8, inward: bool },
}

/// Where a disc command has got to; each stage acts at the operation's due
/// cycle.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// Taking the next step.
    Stepping,
    /// Settling and loading the head.
    Preparing,
    /// Waiting for the index to start formatting or reading IDs.
    Index,
    /// Watching ID fields go by; the search gives up at `deadline`.
    Searching { deadline: u64 },
    /// The data field's mark is passing.
    Mark,
    /// Moving byte `byte` of the current field.
    Moving { byte: usize },
    /// The current field's CRC has passed.
    Crc,
}

/// A disc command in progress.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Operation {
    kind: Kind,
    stage: Stage,
    drive: usize,
    side: usize,
    /// The logical track the command names.
    track: u8,
    /// The physical track the chip is stepping to.
    target: u8,
    steps: Steps,
    stepped: bool,
    mismatches: u8,
    sector: u8,
    remaining: u8,
    size: usize,
    /// The sector being moved, by its index in the track's list, and when
    /// its ID field's address mark passed.
    current: usize,
    field_at: u64,
    deleted_seen: bool,
    /// Bytes supplied for a write, or a format's ID fields.
    written: Vec<u8>,
}

/// What an immediate command leaves once its slow entry path has run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Immediate {
    /// Nothing: busy clears.
    Done,
    /// The drive inputs, their ready bits latched.
    DriveStatus,
    /// A special register's value.
    Special(u8),
}

/// What lands when the chip finishes a command.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Completion {
    Immediate(Immediate),
    /// A disc command's result, with INT.
    Result(u8),
}

/// The controller.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Fdc {
    drives: [Drive; 2],
    registers: [u8; 64],
    status: u8,
    result: u8,
    data: u8,
    /// A data byte awaits the CPU: through INT in non-DMA mode, through the
    /// unwired DMA request otherwise.
    data_pending: bool,
    command: u8,
    parameters: [u8; 5],
    received: usize,
    awaiting: usize,
    /// The reset register holds the chip in reset.
    held: bool,
    operation: Option<Operation>,
    /// The cycle the operation next acts.
    op_due: Option<u64>,
    completion: Option<(u64, Completion)>,
    /// The cycle the idle head unloads.
    unload_at: Option<u64>,
    /// Ready inputs seen low since the last Read Drive Status.
    not_ready: u8,
}

impl Default for Fdc {
    fn default() -> Self {
        Self::new()
    }
}

/// How many parameters `command` takes: the low two bits say, except for the
/// five of scan and format and the four of SPECIFY.
const fn parameter_count(command: u8) -> usize {
    if command & 0x18 == 0 {
        5
    } else if command & 0x3C == 0x34 {
        4
    } else {
        (command & 0x03) as usize
    }
}

/// The first index pulse at or after `cycle`.
const fn index_at_or_after(cycle: u64) -> u64 {
    cycle.div_ceil(REVOLUTION) * REVOLUTION
}

/// When the field at `position` next starts passing the head, at or after
/// `cycle`.
const fn next_pass(position: u16, cycle: u64) -> u64 {
    let at = cycle - cycle % REVOLUTION + position as u64 * BYTE_CYCLES;
    if at < cycle {
        at + REVOLUTION
    } else {
        at
    }
}

const fn bytes(count: usize) -> u64 {
    count as u64 * BYTE_CYCLES
}

impl Fdc {
    /// A controller at power-on with empty drives.
    #[must_use]
    pub fn new() -> Self {
        let mut registers = [0; 64];
        for bad in [register::BAD_TRACKS_0, register::BAD_TRACKS_1] {
            registers[usize::from(bad)] = 0xFF;
            registers[usize::from(bad) + 1] = 0xFF;
        }
        Self {
            drives: Default::default(),
            registers,
            status: 0,
            result: 0,
            data: 0,
            data_pending: false,
            command: 0,
            parameters: [0; 5],
            received: 0,
            awaiting: 0,
            held: false,
            operation: None,
            op_due: None,
            completion: None,
            unload_at: None,
            not_ready: 0,
        }
    }

    /// A drive.
    #[must_use]
    pub const fn drive(&self, drive: PhysicalDrive) -> &Drive {
        &self.drives[drive.index()]
    }

    /// Put a disc in `drive`, or take it out, at `cycle`, returning the disc
    /// that was there. Taking one out, or putting another in its place,
    /// which takes it out first, leaves the drive latched not ready and ends
    /// a command using it.
    pub fn set_disc(
        &mut self,
        cycle: u64,
        drive: PhysicalDrive,
        disc: Option<Disc>,
    ) -> Option<Disc> {
        self.advance(cycle);
        let drive = drive.index();
        let slot = &mut self.drives[drive];
        let old = core::mem::replace(&mut slot.disc, disc);
        if old.is_some() {
            self.not_ready |= ready_bit(drive);
            if self.operation.as_ref().is_some_and(|op| op.drive == drive) {
                self.end(cycle, result::NOT_READY);
                self.advance(cycle);
            }
        }
        old
    }

    /// The disc in `drive`, to save or examine.
    pub fn disc_mut(&mut self, drive: PhysicalDrive) -> Option<&mut Disc> {
        self.drives[drive.index()].disc.as_mut()
    }

    /// Whether INT, the BBC's NMI, is asserted.
    #[must_use]
    pub const fn int(&self) -> bool {
        self.status & status::INT != 0
    }

    /// Whether drive `drive` is selected with its head loaded: its activity
    /// light.
    #[must_use]
    pub fn head_loaded(&self, drive: PhysicalDrive) -> bool {
        let out = self.output();
        out & output::LOAD_HEAD != 0 && out & select_bit(drive.index()) != 0
    }

    /// The cycle the controller next acts without being touched, if any.
    #[must_use]
    pub fn next_event(&self) -> Option<u64> {
        let operation = self.operation.as_ref().and(self.op_due);
        [operation, self.completion.map(|(at, _)| at), self.unload_at]
            .into_iter()
            .flatten()
            .min()
    }

    /// The CPU reads register `register`: 0 status, 1 result, 4–7 data. The
    /// reset register's address is not readable, and the chip leaves the bus
    /// alone.
    pub fn read(&mut self, cycle: u64, register: u8) -> Option<u8> {
        self.advance(cycle);
        match register & 0x07 {
            0 => Some(self.status),
            1 => {
                self.status &= !(status::RESULT_FULL | status::INT);
                Some(self.result)
            }
            4..=7 => {
                self.take_data();
                Some(self.data)
            }
            _ => None,
        }
    }

    /// The CPU writes register `register`: 0 command, 1 parameter, 2 reset,
    /// 4–7 data.
    pub fn write(&mut self, cycle: u64, register: u8, value: u8) {
        self.advance(cycle);
        match register & 0x07 {
            0 if !self.held => self.command(cycle, value),
            1 if !self.held => self.parameter(cycle, value),
            2 => {
                self.held = value & 1 != 0;
                if self.held {
                    self.reset();
                }
            }
            4..=7 => {
                self.data = value;
                self.take_data();
            }
            _ => {}
        }
    }

    /// The reset pin's effect: the drive outputs fall, any command is
    /// abandoned, the status clears and the mode returns to DMA with two
    /// actuators.
    pub fn reset(&mut self) {
        self.abandon();
        self.status = 0;
        self.data_pending = false;
        self.completion = None;
        self.unload_at = None;
        self.received = 0;
        self.awaiting = 0;
        self.registers[usize::from(register::DRIVE_OUTPUT)] = 0;
        self.registers[usize::from(register::MODE)] &= !(mode::NON_DMA | mode::SINGLE_ACTUATOR);
    }

    fn output(&self) -> u8 {
        self.registers[usize::from(register::DRIVE_OUTPUT)]
    }

    fn output_mut(&mut self) -> &mut u8 {
        &mut self.registers[usize::from(register::DRIVE_OUTPUT)]
    }

    fn non_dma(&self) -> bool {
        self.registers[usize::from(register::MODE)] & mode::NON_DMA != 0
    }

    fn take_data(&mut self) {
        if self.data_pending {
            self.data_pending = false;
            self.status &= !(status::DATA_REQUEST | status::INT);
        }
    }

    fn command(&mut self, cycle: u64, value: u8) {
        // The microcode takes a command as it comes, abandoning one running.
        self.abandon();
        self.completion = None;
        self.data_pending = false;
        self.command = value;
        self.result = 0;
        self.status = (self.status & !(status::RESULT_FULL | status::INT | status::DATA_REQUEST))
            | status::BUSY;
        self.received = 0;
        self.awaiting = parameter_count(value);
        if self.awaiting == 0 {
            self.execute(cycle);
        }
    }

    fn parameter(&mut self, cycle: u64, value: u8) {
        if self.received == self.awaiting {
            return;
        }
        self.parameters[self.received] = value;
        self.received += 1;
        if self.received == self.awaiting {
            self.execute(cycle);
        }
    }

    fn execute(&mut self, cycle: u64) {
        self.select(self.command & output::SELECT);
        let p = self.parameters;
        let finish = |fdc: &mut Self, immediate| {
            fdc.completion = Some((cycle + COMMAND_CYCLES, Completion::Immediate(immediate)));
        };
        match self.command & 0x3C {
            0x2C => finish(self, Immediate::DriveStatus),
            0x34 => {
                for (offset, &value) in (0..).zip(&p[1..4]) {
                    self.write_special(p[0].wrapping_add(offset), value);
                }
                finish(self, Immediate::Done);
            }
            0x38 => {
                self.write_special(p[0], p[1]);
                finish(self, Immediate::Done);
            }
            0x3C => finish(self, Immediate::Special(p[0])),
            0x28 => self.start(cycle, Kind::Seek),
            0x08 => self.start(cycle, Kind::Write { deleted: false }),
            0x0C => self.start(cycle, Kind::Write { deleted: true }),
            0x10 => self.start(cycle, read(true, Moves::Data)),
            0x14 => self.start(cycle, read(false, Moves::Data)),
            0x18 => self.start(cycle, Kind::ReadId),
            0x1C => self.start(cycle, read(false, Moves::Nothing)),
            0x20 => self.start(cycle, Kind::Format(Gaps::programmed(p[1], p[3], p[4]))),
            0x00 => self.start(cycle, read(true, Moves::Key)),
            0x04 => self.start(cycle, read(false, Moves::Key)),
            _ => finish(self, Immediate::Done),
        }
    }

    /// A command naming other drives than are selected selects them, and
    /// every output but the side falls.
    fn select(&mut self, select: u8) {
        let out = self.output();
        if out & output::SELECT != select {
            *self.output_mut() = select | (out & output::SIDE);
            self.unload_at = None;
        }
    }

    fn write_special(&mut self, at: u8, value: u8) {
        let at = at & 0x3F;
        // The microcode passes the value through the scan sector register.
        self.registers[usize::from(register::SCAN_SECTOR)] = value;
        if at != register::DRIVE_INPUT {
            self.registers[usize::from(at)] = value;
        }
    }

    fn read_special(&mut self, cycle: u64, at: u8) -> u8 {
        let at = at & 0x3F;
        if at == register::DRIVE_INPUT {
            let pins = self.inputs(cycle);
            self.not_ready |= input::READY & !pins;
            pins
        } else if matches!(at, 0x27 | 0x2F | 0x37 | 0x3F) {
            // The bit processor's unused register reads as all ones.
            0xFF
        } else {
            self.registers[usize::from(at)]
        }
    }

    /// The drive inputs: each drive's ready line, and the selected drive's
    /// track-0, write-protect and index lines. An empty drive's index sensor
    /// sees light all the time.
    fn inputs(&self, cycle: u64) -> u8 {
        let out = self.output();
        let mut pins = 0;
        for (drive, state) in self.drives.iter().enumerate() {
            if state.disc.is_some() {
                pins |= ready_bit(drive);
            }
            if out & select_bit(drive) == 0 {
                continue;
            }
            if state.head_track == 0 {
                pins |= input::TRACK_0;
            }
            match &state.disc {
                Some(disc) => {
                    if disc.write_protected() {
                        pins |= input::WRITE_PROTECT;
                    }
                    if cycle % REVOLUTION < INDEX_PULSE {
                        pins |= input::INDEX;
                    }
                }
                None => pins |= input::INDEX,
            }
        }
        pins
    }

    /// Whether `drive` is ready and has stayed so since the last Read Drive
    /// Status; a drive found not ready is latched so.
    fn ready(&mut self, drive: usize) -> bool {
        let bit = ready_bit(drive);
        if self.drives[drive].disc.is_none() {
            self.not_ready |= bit;
        }
        self.not_ready & bit == 0
    }

    /// The track register for `drive`: one for both in single-actuator mode.
    fn track_register(&self, drive: usize) -> usize {
        let single = self.registers[usize::from(register::MODE)] & mode::SINGLE_ACTUATOR != 0;
        usize::from(if drive == 0 || single {
            register::TRACK_0
        } else {
            register::TRACK_1
        })
    }

    /// The physical track logical `track` lies on, past the bad tracks of
    /// `drive`'s surface at or below it.
    fn physical_track(&self, drive: usize, track: u8) -> u8 {
        let first = usize::from(if drive == 0 {
            register::BAD_TRACKS_0
        } else {
            register::BAD_TRACKS_1
        });
        let mut bad = [self.registers[first], self.registers[first + 1]];
        bad.sort_unstable();
        bad.into_iter().fold(track, |physical, bad| {
            if bad <= physical {
                physical.saturating_add(1)
            } else {
                physical
            }
        })
    }

    fn start(&mut self, cycle: u64, kind: Kind) {
        let entry = cycle + COMMAND_CYCLES;
        let select = self.command & output::SELECT;
        if select == 0 {
            // No drive is addressed.
            self.completion = Some((entry, Completion::Result(result::NOT_READY)));
            return;
        }
        let drive = usize::from(select == output::SELECT_1);
        if kind != Kind::Seek && !self.ready(drive) {
            self.completion = Some((entry, Completion::Result(result::NOT_READY)));
            return;
        }
        let writes = matches!(kind, Kind::Write { .. } | Kind::Format(_));
        if writes
            && self.drives[drive]
                .disc
                .as_ref()
                .is_some_and(Disc::write_protected)
        {
            self.completion = Some((entry, Completion::Result(result::WRITE_PROTECTED)));
            return;
        }
        let p = self.parameters;
        let track = p[0];
        // The 128-byte single-record commands take one 128-byte sector.
        let shape = if self.command & 0x03 == 0x02 {
            0x01
        } else {
            p[2]
        };
        let (target, steps) = if track == 0 {
            (0, Steps::ToSensor)
        } else {
            let target = self.physical_track(drive, track);
            let current = self.registers[self.track_register(drive)];
            let steps = Steps::Count {
                left: target.abs_diff(current),
                inward: target > current,
            };
            (target, steps)
        };
        let remaining = match kind {
            // Read ID's count is a whole byte.
            Kind::ReadId => p[2],
            _ => shape & 0x1F,
        };
        self.unload_at = None;
        self.operation = Some(Operation {
            kind,
            stage: Stage::Stepping,
            drive,
            side: usize::from(self.output() & output::SIDE != 0),
            track,
            target,
            steps,
            stepped: false,
            mismatches: 0,
            sector: p[1],
            remaining: remaining.max(1),
            size: disc::field_size(shape >> 5),
            current: 0,
            field_at: 0,
            deleted_seen: false,
            written: Vec::new(),
        });
        self.op_due = Some(entry);
    }
}

/// A read command's kind.
const fn read(skip_deleted: bool, moves: Moves) -> Kind {
    Kind::Read {
        skip_deleted,
        moves,
    }
}

const fn ready_bit(drive: usize) -> u8 {
    if drive == 0 {
        input::READY_0
    } else {
        input::READY_1
    }
}

const fn select_bit(drive: usize) -> u8 {
    if drive == 0 {
        output::SELECT_0
    } else {
        output::SELECT_1
    }
}

impl Fdc {
    /// Run everything due by `cycle`.
    pub fn advance(&mut self, cycle: u64) {
        while let Some(due) = self.next_event().filter(|&due| due <= cycle) {
            if let Some((_, completion)) = self.completion.filter(|&(at, _)| at == due) {
                self.completion = None;
                self.complete(due, completion);
            } else if self.unload_at == Some(due) {
                self.unload_at = None;
                *self.output_mut() &= output::SIDE;
            } else {
                self.op_due = None;
                self.act(due);
            }
        }
    }

    fn complete(&mut self, cycle: u64, completion: Completion) {
        self.status &= !status::BUSY;
        match completion {
            Completion::Immediate(Immediate::Done) => {}
            Completion::Immediate(Immediate::DriveStatus) => {
                let pins = self.inputs(cycle);
                self.result = pins & !(self.not_ready & input::READY);
                self.not_ready = input::READY & !pins;
                self.status |= status::RESULT_FULL;
            }
            Completion::Immediate(Immediate::Special(at)) => {
                self.result = self.read_special(cycle, at);
                self.status |= status::RESULT_FULL;
            }
            Completion::Result(code) => {
                self.result = code;
                self.data_pending = false;
                self.status =
                    (self.status & !status::DATA_REQUEST) | status::RESULT_FULL | status::INT;
            }
        }
        self.schedule_unload(cycle);
    }

    /// The idle head unloads at the programmed index pulse; an index count
    /// of 15 keeps it loaded.
    fn schedule_unload(&mut self, cycle: u64) {
        let count = self.registers[usize::from(register::HEAD_LOAD)] >> 4;
        self.unload_at =
            (self.output() & output::LOAD_HEAD != 0 && count != STAY_LOADED).then(|| match count {
                0 => cycle,
                _ => index_at_or_after(cycle + 1) + u64::from(count - 1) * REVOLUTION,
            });
    }

    /// End the operation at `cycle` with `code`, and a deleted mark met.
    fn end(&mut self, cycle: u64, code: u8) {
        let deleted = self.operation.take().is_some_and(|op| op.deleted_seen);
        self.op_due = None;
        *self.output_mut() &= !output::WRITE_ENABLE;
        let code = if deleted {
            code | result::DELETED
        } else {
            code
        };
        self.completion = Some((cycle, Completion::Result(code)));
    }

    fn act(&mut self, now: u64) {
        let Some(stage) = self.operation.as_ref().map(|op| op.stage) else {
            return;
        };
        match stage {
            Stage::Stepping => self.step(now),
            Stage::Preparing => self.prepared(now),
            Stage::Index => self.at_index(now),
            Stage::Searching { deadline } => self.search(now, deadline),
            Stage::Mark => self.at_mark(now),
            Stage::Moving { byte } => self.move_byte(now, byte),
            Stage::Crc => self.end_field(now),
        }
    }

    fn set_stage(&mut self, stage: Stage, at: u64) {
        if let Some(op) = &mut self.operation {
            op.stage = stage;
        }
        self.op_due = Some(at);
    }

    /// One step, or the stepping done. A seek to track 0 steps out until the
    /// sensor answers; any other steps the difference between the target and
    /// the track register, wherever the head really is.
    fn step(&mut self, now: u64) {
        let step = u64::from(self.registers[usize::from(register::STEP_RATE)]).max(1) * STEP_UNIT;
        let Some(op) = &mut self.operation else {
            return;
        };
        let head = &mut self.drives[op.drive].head_track;
        let inward = match &mut op.steps {
            Steps::ToSensor => (*head != 0).then_some(false),
            Steps::Count { left: 0, .. } => None,
            Steps::Count { left, inward } => {
                *left -= 1;
                Some(*inward)
            }
        };
        let Some(inward) = inward else {
            let (drive, target) = (op.drive, op.target);
            let register = self.track_register(drive);
            self.registers[register] = target;
            self.prepare(now);
            return;
        };
        *head = if inward {
            (*head + 1).min(disc::LAST_TRACK)
        } else {
            head.saturating_sub(1)
        };
        op.stepped = true;
        let high = *head > LOW_CURRENT_TRACK;
        let out = self.output_mut();
        *out = (*out & !(output::DIRECTION | output::LOW_CURRENT))
            | if inward { output::DIRECTION } else { 0 }
            | if high { output::LOW_CURRENT } else { 0 };
        self.set_stage(Stage::Stepping, now + step);
    }

    /// The head settles after stepping and loads if it was not loaded;
    /// whichever takes longer decides when the disc can be read.
    fn prepare(&mut self, now: u64) {
        let stepped = self.operation.as_ref().is_some_and(|op| op.stepped);
        let settle = if stepped {
            u64::from(self.registers[usize::from(register::SETTLE)]) * STEP_UNIT
        } else {
            0
        };
        let load = if self.output() & output::LOAD_HEAD == 0 {
            u64::from(self.registers[usize::from(register::HEAD_LOAD)] & 0x0F) * LOAD_UNIT
        } else {
            0
        };
        *self.output_mut() |= output::LOAD_HEAD;
        if let Some(op) = &mut self.operation {
            op.stepped = false;
        }
        self.set_stage(Stage::Preparing, now + settle.max(load));
    }

    fn prepared(&mut self, now: u64) {
        let Some(op) = &self.operation else {
            return;
        };
        match op.kind {
            Kind::Seek => {
                let drive = op.drive;
                let code = if self.ready(drive) {
                    result::OK
                } else {
                    result::NOT_READY
                };
                self.end(now, code);
            }
            Kind::ReadId | Kind::Format(_) => self.set_stage(Stage::Index, index_at_or_after(now)),
            _ => self.begin_search(now),
        }
    }

    /// Look for the next sector, giving up at the second index pulse.
    fn begin_search(&mut self, now: u64) {
        let deadline = index_at_or_after(now) + (SEARCH_INDEXES - 1) * REVOLUTION;
        if let Some(op) = &self.operation {
            self.registers[usize::from(register::SCAN_SECTOR)] = op.sector;
        }
        self.set_stage(Stage::Searching { deadline }, now);
    }

    fn at_index(&mut self, now: u64) {
        let Some(op) = &mut self.operation else {
            return;
        };
        match op.kind {
            Kind::Format(gaps) => {
                op.field_at = now + u64::from(gaps.lead) * BYTE_CYCLES;
                op.current = 0;
                let first = op.field_at;
                self.set_stage(Stage::Moving { byte: 0 }, first);
            }
            _ => self.begin_search(now),
        }
    }

    fn track(&self, op: &Operation) -> Option<&Track> {
        let drive = &self.drives[op.drive];
        drive
            .disc
            .as_ref()?
            .track(op.side, usize::from(drive.head_track))
    }

    /// Watch the ID fields pass from `now`: one naming another track makes
    /// the chip step in and look again, twice, before it gives up; the
    /// wanted one starts the move.
    fn search(&mut self, now: u64, deadline: u64) {
        let Some(op) = self.operation.as_ref() else {
            return;
        };
        let checks_track = !matches!(op.kind, Kind::ReadId);
        let mut cursor = now;
        let found = loop {
            let next = self.track(op).and_then(|track| {
                track
                    .sectors
                    .iter()
                    .enumerate()
                    .map(|(index, sector)| (next_pass(sector.position, cursor), index))
                    .min()
            });
            let Some((at, index)) = next else {
                break None;
            };
            let read = at + u64::from(disc::ID_FIELD_BYTES) * BYTE_CYCLES;
            if read > deadline {
                break None;
            }
            let id = self.track(op).map(|track| track.sectors[index].id);
            let Some(id) = id else {
                break None;
            };
            if checks_track && id.track != op.track {
                break Some((at, index, false));
            }
            if !checks_track || id.sector == op.sector {
                break Some((at, index, true));
            }
            cursor = at + 1;
        };
        let read_at = |at: u64| at + u64::from(disc::ID_FIELD_BYTES) * BYTE_CYCLES;
        match found {
            None => self.end(deadline, result::SECTOR_NOT_FOUND),
            Some((at, _, false)) => {
                let Some(op) = &mut self.operation else {
                    return;
                };
                if op.mismatches == MISMATCH_STEPS {
                    self.end(read_at(at), result::SECTOR_NOT_FOUND);
                    return;
                }
                op.mismatches += 1;
                op.target = op.target.saturating_add(1);
                op.steps = Steps::Count {
                    left: 1,
                    inward: true,
                };
                self.set_stage(Stage::Stepping, read_at(at));
            }
            Some((at, index, true)) => {
                let Some(op) = &mut self.operation else {
                    return;
                };
                op.current = index;
                op.field_at = at;
                if op.kind == Kind::ReadId {
                    self.set_stage(Stage::Moving { byte: 0 }, at + 2 * BYTE_CYCLES);
                } else {
                    self.set_stage(
                        Stage::Mark,
                        at + u64::from(disc::ID_TO_DATA_MARK) * BYTE_CYCLES,
                    );
                }
            }
        }
    }

    fn current_sector(&self) -> Option<&Sector> {
        let op = self.operation.as_ref()?;
        self.track(op)?.sectors.get(op.current)
    }

    /// The data field's mark passes: a skipping read counts a deleted sector
    /// and moves on, and a write starts asking for bytes.
    fn at_mark(&mut self, now: u64) {
        let deleted = self.current_sector().is_some_and(|sector| sector.deleted);
        let Some(op) = &mut self.operation else {
            return;
        };
        match op.kind {
            Kind::Read { skip_deleted, .. } => {
                if deleted {
                    op.deleted_seen = true;
                }
                if deleted && skip_deleted {
                    let end =
                        now + bytes(op.size + usize::from(disc::DATA_MARK_BYTES + disc::CRC_BYTES));
                    self.next_sector(end);
                } else {
                    self.set_stage(Stage::Moving { byte: 0 }, now + 2 * BYTE_CYCLES);
                }
            }
            Kind::Write { .. } => {
                op.written.clear();
                *self.output_mut() |= output::WRITE_ENABLE;
                self.request();
                self.set_stage(Stage::Moving { byte: 0 }, now + BYTE_CYCLES);
            }
            _ => {}
        }
    }

    /// Ask the CPU for, or hand it, a byte.
    fn request(&mut self) {
        self.data_pending = true;
        if self.non_dma() {
            self.status |= status::DATA_REQUEST | status::INT;
        }
    }

    fn offer(&mut self, value: u8) {
        self.data = value;
        self.request();
    }

    fn move_byte(&mut self, now: u64, byte: usize) {
        let Some((kind, size)) = self.operation.as_ref().map(|op| (op.kind, op.size)) else {
            return;
        };
        match kind {
            Kind::Read { moves, .. } => {
                if moves == Moves::Key {
                    // The key a scan compares with only DMA can fetch.
                    self.end(now, result::LATE_DMA);
                    return;
                }
                if self.data_pending {
                    self.end(now, result::LATE_DMA);
                    return;
                }
                if moves == Moves::Data {
                    let value = self
                        .current_sector()
                        .map_or(disc::GAP_FILL, |sector| sector.field_byte(byte));
                    self.offer(value);
                }
                if byte + 1 < size {
                    self.set_stage(Stage::Moving { byte: byte + 1 }, now + BYTE_CYCLES);
                } else {
                    self.set_stage(Stage::Crc, now + bytes(usize::from(disc::CRC_BYTES)));
                }
            }
            Kind::ReadId => {
                if self.data_pending {
                    self.end(now, result::LATE_DMA);
                    return;
                }
                if let Some(id) = self.current_sector().map(|sector| sector.id) {
                    self.offer([id.track, id.head, id.sector, id.size][byte]);
                }
                if byte < 3 {
                    self.set_stage(Stage::Moving { byte: byte + 1 }, now + BYTE_CYCLES);
                } else {
                    self.set_stage(Stage::Crc, now + bytes(usize::from(disc::CRC_BYTES)));
                }
            }
            Kind::Write { .. } => self.write_byte(now, byte),
            Kind::Format(gaps) => self.format_byte(now, byte, gaps),
            Kind::Seek => {}
        }
    }

    /// A write takes byte `byte` as the head reaches it and asks for the next.
    fn write_byte(&mut self, now: u64, byte: usize) {
        if self.data_pending {
            self.abort_write(now);
            return;
        }
        let data = self.data;
        let Some(op) = &mut self.operation else {
            return;
        };
        op.written.push(data);
        if byte + 1 < op.size {
            self.request();
            self.set_stage(Stage::Moving { byte: byte + 1 }, now + BYTE_CYCLES);
        } else {
            self.set_stage(Stage::Crc, now + bytes(1 + usize::from(disc::CRC_BYTES)));
        }
    }

    /// A write the CPU fell behind stops there.
    fn abort_write(&mut self, now: u64) {
        self.cut_write_short();
        self.end(now, result::LATE_DMA);
    }

    /// A write stopped part way through its data field: what it wrote
    /// replaces the start of the old field, whose CRC no longer matches.
    fn cut_write_short(&mut self) {
        let Some(op) = self.operation.as_mut() else {
            return;
        };
        let Kind::Write { deleted } = op.kind else {
            return;
        };
        if !matches!(op.stage, Stage::Moving { .. } | Stage::Crc) {
            return;
        }
        let (drive, side, current) = (op.drive, op.side, op.current);
        let mut written = core::mem::take(&mut op.written);
        let head = usize::from(self.drives[drive].head_track);
        if let Some(track) = self.drives[drive]
            .disc
            .as_mut()
            .and_then(|disc| disc.track_mut(side, head))
        {
            if let Some(rest) = track
                .sectors
                .get(current)
                .and_then(|old| old.data.get(written.len()..))
            {
                written.extend_from_slice(rest);
            }
            track.write_data(current, written, deleted, true);
        }
    }

    /// Stop the operation without a result: a new command or a reset took
    /// over.
    fn abandon(&mut self) {
        self.cut_write_short();
        self.operation = None;
        self.op_due = None;
        *self.output_mut() &= !output::WRITE_ENABLE;
    }

    /// A format asks for each ID field's four bytes as it writes the address
    /// mark and the bytes before them, and takes each as the head reaches it.
    fn format_byte(&mut self, now: u64, byte: usize, gaps: Gaps) {
        let (sector, within) = (byte / 5, byte % 5);
        if within > 0 {
            if self.data_pending {
                self.end(now, result::LATE_DMA);
                return;
            }
            let data = self.data;
            if let Some(op) = &mut self.operation {
                op.written.push(data);
            }
        }
        if within < 4 {
            self.request();
            self.set_stage(Stage::Moving { byte: byte + 1 }, now + BYTE_CYCLES);
            return;
        }
        let Some(op) = &self.operation else {
            return;
        };
        if sector + 1 < usize::from(op.remaining) {
            let next = op.field_at + bytes((sector + 1) * gaps.stride(op.size));
            self.set_stage(Stage::Moving { byte: byte + 1 }, next);
        } else {
            self.finish_format(now, gaps);
        }
    }

    /// Lay the formatted track down: where the fields ran past the index they
    /// wrote over the first ones, and the format ends at the next index.
    fn finish_format(&mut self, now: u64, gaps: Gaps) {
        let Some(op) = self.operation.as_mut() else {
            return;
        };
        let stride = gaps.stride(op.size);
        let lead = usize::from(gaps.lead);
        let revolution = usize::from(disc::REVOLUTION_BYTES);
        let end = lead + usize::from(op.remaining) * stride;
        let fill = vec![FORMAT_FILL; op.size];
        let sectors: Vec<Sector> = op
            .written
            .as_chunks::<4>()
            .0
            .iter()
            .enumerate()
            .filter_map(|(index, id)| {
                let position = lead + index * stride;
                (position + revolution >= end).then(|| Sector {
                    position: u16::try_from(position % revolution).unwrap_or(0),
                    id: SectorId {
                        track: id[0],
                        head: id[1],
                        sector: id[2],
                        size: id[3],
                    },
                    data: fill.clone(),
                    deleted: false,
                    crc_error: false,
                })
            })
            .collect();
        let started = op.field_at - bytes(lead);
        let (drive, side) = (op.drive, op.side);
        let head = usize::from(self.drives[drive].head_track);
        if let Some(track) = self.drives[drive]
            .disc
            .as_mut()
            .and_then(|disc| disc.track_mut(side, head))
        {
            track.sectors = sectors;
        }
        let finished = index_at_or_after((started + bytes(end)).max(now));
        self.end(finished, result::OK);
    }

    /// A field's CRC has passed: check what was read, or commit what was
    /// written.
    fn end_field(&mut self, now: u64) {
        let Some(op) = self.operation.as_ref() else {
            return;
        };
        match op.kind {
            Kind::Read { moves, .. } => {
                let size = op.size;
                let intact = self.current_sector().is_some_and(|sector| {
                    if size == sector.data.len() {
                        return !sector.crc_error;
                    }
                    // Read at another length, the bytes taken for the CRC
                    // are whatever followed.
                    let crc = disc::field_crc(
                        sector.deleted,
                        (0..size).map(|offset| sector.field_byte(offset)),
                    );
                    crc.to_be_bytes() == [sector.field_byte(size), sector.field_byte(size + 1)]
                });
                if !intact {
                    self.end(now, result::DATA_CRC_ERROR);
                } else if moves == Moves::Data && self.data_pending && op.remaining == 1 {
                    self.end(now, result::LATE_DMA);
                } else {
                    self.next_sector(now);
                }
            }
            Kind::ReadId => self.next_sector(now),
            Kind::Write { deleted } => {
                let (drive, side, current) = (op.drive, op.side, op.current);
                let head = usize::from(self.drives[drive].head_track);
                let written = self
                    .operation
                    .as_mut()
                    .map(|op| core::mem::take(&mut op.written));
                if let (Some(track), Some(written)) = (
                    self.drives[drive]
                        .disc
                        .as_mut()
                        .and_then(|disc| disc.track_mut(side, head)),
                    written,
                ) {
                    track.write_data(current, written, deleted, false);
                }
                *self.output_mut() &= !output::WRITE_ENABLE;
                self.next_sector(now);
            }
            Kind::Seek | Kind::Format(_) => {}
        }
    }

    /// One sector done: post the result, or look for the next.
    fn next_sector(&mut self, now: u64) {
        let Some(op) = &mut self.operation else {
            return;
        };
        op.remaining -= 1;
        if op.remaining == 0 {
            self.end(now, result::OK);
            return;
        }
        if op.kind != Kind::ReadId {
            op.sector = op.sector.wrapping_add(1);
        }
        self.begin_search(now);
    }
}

crate::snapshot::fields!(
    Drive { disc, head_track },
    check | drive | drive.head_track <= disc::LAST_TRACK
);

impl crate::snapshot::Save for Moves {
    fn save(&self, w: &mut crate::snapshot::Writer) {
        w.put(&match self {
            Self::Data => 0u8,
            Self::Nothing => 1,
            Self::Key => 2,
        });
    }
}

impl crate::snapshot::Load for Moves {
    fn load(r: &mut crate::snapshot::Reader<'_>) -> Result<Self, crate::snapshot::SnapshotError> {
        match r.get::<u8>()? {
            0 => Ok(Self::Data),
            1 => Ok(Self::Nothing),
            2 => Ok(Self::Key),
            _ => Err(crate::snapshot::SnapshotError::Malformed),
        }
    }
}

crate::snapshot::fields!(
    Gaps { lead, gap_3 },
    check | gaps | {
        let (least, most) = (
            Gaps::programmed(0, 0, 0),
            Gaps::programmed(u8::MAX, u8::MAX, u8::MAX),
        );
        (least.lead..=most.lead).contains(&gaps.lead)
            && (least.gap_3..=most.gap_3).contains(&gaps.gap_3)
    }
);

impl crate::snapshot::Save for Kind {
    fn save(&self, w: &mut crate::snapshot::Writer) {
        match *self {
            Self::Seek => w.put(&0u8),
            Self::Read {
                skip_deleted,
                moves,
            } => {
                w.put(&1u8);
                w.put(&skip_deleted);
                w.put(&moves);
            }
            Self::Write { deleted } => {
                w.put(&2u8);
                w.put(&deleted);
            }
            Self::ReadId => w.put(&3u8),
            Self::Format(gaps) => {
                w.put(&4u8);
                w.put(&gaps);
            }
        }
    }
}

impl crate::snapshot::Load for Kind {
    fn load(r: &mut crate::snapshot::Reader<'_>) -> Result<Self, crate::snapshot::SnapshotError> {
        let kind = match r.get::<u8>()? {
            0 => Self::Seek,
            1 => Self::Read {
                skip_deleted: r.get()?,
                moves: r.get()?,
            },
            2 => Self::Write { deleted: r.get()? },
            3 => Self::ReadId,
            4 => Self::Format(r.get()?),
            _ => return Err(crate::snapshot::SnapshotError::Malformed),
        };
        Ok(kind)
    }
}

impl crate::snapshot::Save for Steps {
    fn save(&self, w: &mut crate::snapshot::Writer) {
        match *self {
            Self::ToSensor => w.put(&0u8),
            Self::Count { left, inward } => {
                w.put(&1u8);
                w.put(&left);
                w.put(&inward);
            }
        }
    }
}

impl crate::snapshot::Load for Steps {
    fn load(r: &mut crate::snapshot::Reader<'_>) -> Result<Self, crate::snapshot::SnapshotError> {
        match r.get::<u8>()? {
            0 => Ok(Self::ToSensor),
            1 => Ok(Self::Count {
                left: r.get()?,
                inward: r.get()?,
            }),
            _ => Err(crate::snapshot::SnapshotError::Malformed),
        }
    }
}

impl crate::snapshot::Save for Stage {
    fn save(&self, w: &mut crate::snapshot::Writer) {
        match *self {
            Self::Stepping => w.put(&0u8),
            Self::Preparing => w.put(&1u8),
            Self::Index => w.put(&2u8),
            Self::Searching { deadline } => {
                w.put(&3u8);
                w.put(&deadline);
            }
            Self::Mark => w.put(&4u8),
            Self::Moving { byte } => {
                w.put(&5u8);
                w.put(&byte);
            }
            Self::Crc => w.put(&6u8),
        }
    }
}

impl crate::snapshot::Load for Stage {
    fn load(r: &mut crate::snapshot::Reader<'_>) -> Result<Self, crate::snapshot::SnapshotError> {
        let stage = match r.get::<u8>()? {
            0 => Self::Stepping,
            1 => Self::Preparing,
            2 => Self::Index,
            3 => Self::Searching { deadline: r.get()? },
            4 => Self::Mark,
            5 => Self::Moving { byte: r.get()? },
            6 => Self::Crc,
            _ => return Err(crate::snapshot::SnapshotError::Malformed),
        };
        Ok(stage)
    }
}

crate::snapshot::fields!(
    Operation {
        kind,
        stage,
        drive,
        side,
        track,
        target,
        steps,
        stepped,
        mismatches,
        sector,
        remaining,
        size,
        current,
        field_at,
        deleted_seen,
        written <= (disc::LONGEST_FIELD),
    },
    check | op,
    r | op.restorable(r)
);

impl Operation {
    /// Whether a restored command is one the chip could be running: on a
    /// drive and side it has, at a stage its kind passes through, a byte
    /// within its field, and no more bytes held than its field or ID fields
    /// take.
    fn restorable(&self, r: &crate::snapshot::Reader<'_>) -> bool {
        let reads_or_writes = matches!(self.kind, Kind::Read { .. } | Kind::Write { .. });
        let stage = match (self.kind, self.stage) {
            (_, Stage::Stepping | Stage::Preparing)
            | (Kind::ReadId | Kind::Format(_), Stage::Index)
            | (Kind::Read { .. } | Kind::Write { .. } | Kind::ReadId, Stage::Searching { .. }) => {
                true
            }
            (_, Stage::Mark) => reads_or_writes,
            (_, Stage::Crc) => reads_or_writes || self.kind == Kind::ReadId,
            (Kind::Read { .. } | Kind::Write { .. }, Stage::Moving { byte }) => byte < self.size,
            (Kind::ReadId, Stage::Moving { byte }) => byte < 4,
            (Kind::Format(gaps), Stage::Moving { byte }) => {
                byte < 5 * usize::from(self.remaining)
                    && self.field_at >= bytes(usize::from(gaps.lead))
            }
            _ => false,
        };
        let written = match self.kind {
            Kind::Write { .. } => self.written.len() <= self.size,
            Kind::Format(_) => self.written.len() <= 4 * usize::from(self.remaining),
            _ => self.written.is_empty(),
        };
        self.drive < 2
            && self.side < 2
            && self.remaining >= 1
            && self.mismatches <= MISMATCH_STEPS
            && self.size.is_power_of_two()
            && (disc::field_size(0)..=disc::LONGEST_FIELD).contains(&self.size)
            && self.current < usize::from(disc::REVOLUTION_BYTES)
            && self.field_at <= r.horizon()
            && stage
            && written
    }
}

impl crate::snapshot::Save for Immediate {
    fn save(&self, w: &mut crate::snapshot::Writer) {
        match *self {
            Self::Done => w.put(&0u8),
            Self::DriveStatus => w.put(&1u8),
            Self::Special(at) => {
                w.put(&2u8);
                w.put(&at);
            }
        }
    }
}

impl crate::snapshot::Load for Immediate {
    fn load(r: &mut crate::snapshot::Reader<'_>) -> Result<Self, crate::snapshot::SnapshotError> {
        match r.get::<u8>()? {
            0 => Ok(Self::Done),
            1 => Ok(Self::DriveStatus),
            2 => Ok(Self::Special(r.get()?)),
            _ => Err(crate::snapshot::SnapshotError::Malformed),
        }
    }
}

impl crate::snapshot::Save for Completion {
    fn save(&self, w: &mut crate::snapshot::Writer) {
        match *self {
            Self::Immediate(immediate) => {
                w.put(&0u8);
                w.put(&immediate);
            }
            Self::Result(code) => {
                w.put(&1u8);
                w.put(&code);
            }
        }
    }
}

impl crate::snapshot::Load for Completion {
    fn load(r: &mut crate::snapshot::Reader<'_>) -> Result<Self, crate::snapshot::SnapshotError> {
        match r.get::<u8>()? {
            0 => Ok(Self::Immediate(r.get()?)),
            1 => Ok(Self::Result(r.get()?)),
            _ => Err(crate::snapshot::SnapshotError::Malformed),
        }
    }
}

crate::snapshot::fields!(
    Fdc {
        drives,
        registers,
        status,
        result,
        data,
        data_pending,
        command,
        parameters,
        received,
        awaiting,
        held,
        operation,
        op_due,
        completion,
        unload_at,
        not_ready,
    },
    check | fdc,
    r | fdc.restorable(r)
);

impl Fdc {
    /// Whether a restored chip is one the machine could leave: busy exactly
    /// while it awaits parameters, runs a command or owes a completion, a
    /// running command due to act and owing nothing else, a search giving
    /// up within its two revolutions of starting, a data request only for a
    /// byte pending, idle while held in reset, and its times still to come.
    fn restorable(&self, r: &crate::snapshot::Reader<'_>) -> bool {
        let known = status::BUSY | status::RESULT_FULL | status::INT | status::DATA_REQUEST;
        let busy =
            self.received < self.awaiting || self.operation.is_some() || self.completion.is_some();
        let idle =
            self.operation.is_none() && self.completion.is_none() && self.received == self.awaiting;
        let running =
            self.operation.is_none() || (self.completion.is_none() && self.unload_at.is_none());
        self.status & !known == 0
            && (self.status & status::BUSY != 0) == busy
            && (self.status & status::DATA_REQUEST == 0 || self.data_pending)
            && self.received <= self.awaiting
            && (self.awaiting == 0 || self.awaiting == parameter_count(self.command))
            && self.operation.is_some() == self.op_due.is_some()
            && running
            && (!self.held || idle)
            && self.searching_within(SEARCH_INDEXES * REVOLUTION)
            && self.op_due.is_none_or(|at| r.pending(at))
            && self.completion.is_none_or(|(at, _)| r.pending(at))
            && self.unload_at.is_none_or(|at| r.pending(at))
            && self.not_ready & !input::READY == 0
    }
    /// Whether a search under way gives up no later than `longest` after it
    /// began, at its due time.
    fn searching_within(&self, longest: u64) -> bool {
        match (&self.operation, self.op_due) {
            (
                Some(Operation {
                    stage: Stage::Searching { deadline },
                    ..
                }),
                Some(due),
            ) => *deadline >= due && *deadline - due <= longest,
            _ => true,
        }
    }
}

#[cfg(test)]
#[path = "fdc_tests.rs"]
mod tests;
