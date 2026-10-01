//! The machine report (`plans/NEW-SWITCHBOARD.md` S14): the readings a
//! Switchboard instance publishes over
//! [`SWITCHBOARD_ENDPOINT`](super::SWITCHBOARD_ENDPOINT) while the desktop's
//! System Monitor screensaver watches it.
//!
//! The screensaver puts the machine's state in front of anyone who can see
//! the screen, over the lock included, so the frame carries readings and
//! never pixels: the session draws them itself. It is a frame of its own,
//! told apart from a [`SwitchboardRequest`](super::SwitchboardRequest) by its
//! magic ([`is_machine_report`]), so the tray summary every publish carries
//! does not grow to its size.
//!
//! An absent reading is absent on the wire — a cleared presence bit over
//! zeroed bytes — never a zero, so a machine whose memory could not be read
//! does not report it empty. Every field is validated at construction and
//! again at decode, through the same constructors, and a decode fails closed
//! on the first fault: an unknown magic or version, a set reserved bit or
//! byte, a fraction above full, a count below the entries it names, or a
//! malformed name refuses the whole report rather than drawing part of one.
//!
//! The session answers a report with the shared status frame: success while
//! a monitor is still watching, and [`Errno::BrokenPipe`] once none is — the
//! reader is gone, so the publisher stops writing, as a pipe's writer does.

use crate::bounded_text::BoundedText;
use crate::le::{put_u16, put_u32, put_u64, read_u16, read_u32, read_u64};
use crate::memory::MEMORY_CLASS_COUNT;
use crate::net_ipc::IF_NAME_LEN;
use crate::sysinfo::{LoadAverage, MemoryBand, MountAvailability, HOSTNAME_MAX};
use crate::time::Duration64;
use crate::Errno;

use super::{Permille, TrayTaskName, SWITCHBOARD_VERSION_V1, TRAY_TASK_NAME_MAX};

/// Magic number identifying a machine-report frame (`"SWM1"` little-endian).
pub const MACHINE_REPORT_MAGIC: u32 = u32::from_le_bytes(*b"SWM1");

/// Most processors one report names.
///
/// A validation bound on the frame, not a capacity: the largest machines
/// TAIRiX targets have hundreds of cores, and a report names every one the
/// monitor read, up to this many.
pub const MACHINE_CORES_MAX: usize = 512;

/// Most readings one history carries.
///
/// A validation bound on the frame: a trace drawn across a panel gains
/// nothing from more readings than it has segments to draw them with.
pub const MACHINE_HISTORY_MAX: usize = 64;

/// Most busy tasks one report names: the handful a reader can act on from
/// across a room. The whole list is the Switchboard's Tasks table.
pub const MACHINE_TASKS_MAX: usize = 5;

/// Most storage devices one report names. A report says how many exist
/// beyond them and names the least healthy first, so none needing attention
/// is left unnamed while a healthy one is shown.
pub const MACHINE_DEVICES_MAX: usize = 8;

/// Most network interfaces one report names; a report says how many exist
/// beyond them.
pub const MACHINE_INTERFACES_MAX: usize = 8;

/// Longest storage-device name a report carries, in bytes.
///
/// A validation bound on the fixed-width frame: the name is display text a
/// row shows, and its producer cuts a longer one with a mark that says so.
pub const MACHINE_DEVICE_NAME_MAX: usize = 64;

/// The shortest sampling period a report may state, in milliseconds.
pub const MACHINE_PERIOD_MIN_MS: u32 = 100;

/// The longest sampling period a report may state, in milliseconds: past a
/// minute a reading is history rather than a monitor's.
pub const MACHINE_PERIOD_MAX_MS: u32 = 60_000;

/// A core's wire value when its share was not measured.
const CORE_UNMEASURED: u16 = u16::MAX;

/// The machine's name, as the report carries it.
pub type MachineHost = BoundedText<1, HOSTNAME_MAX>;

/// A storage device's name, as the report carries it.
pub type MachineDeviceName = BoundedText<1, MACHINE_DEVICE_NAME_MAX>;

/// A network interface's name, as the report carries it.
pub type MachineInterfaceName = BoundedText<1, IF_NAME_LEN>;

/// Whether `bytes` is a machine-report frame rather than a
/// [`SwitchboardRequest`](super::SwitchboardRequest), by its magic alone; the
/// frame is still decoded and validated whole.
#[must_use]
pub fn is_machine_report(bytes: &[u8]) -> bool {
    bytes.len() >= 4 && read_u32(bytes, 0) == MACHINE_REPORT_MAGIC
}

/// How often the publisher samples, so a reader can tell a report that is
/// merely recent from reports that have stopped coming.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct ReportPeriod(u32);

impl ReportPeriod {
    /// A period of `millis`.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] outside
    /// [`MACHINE_PERIOD_MIN_MS`]`..=`[`MACHINE_PERIOD_MAX_MS`].
    pub const fn from_millis(millis: u32) -> Result<Self, Errno> {
        if millis < MACHINE_PERIOD_MIN_MS || millis > MACHINE_PERIOD_MAX_MS {
            return Err(Errno::OutOfRange);
        }
        Ok(Self(millis))
    }

    /// The period in milliseconds.
    #[must_use]
    pub const fn as_millis(self) -> u32 {
        self.0
    }

    /// The period in nanoseconds.
    #[must_use]
    pub const fn as_nanos(self) -> u64 {
        self.0 as u64 * 1_000_000
    }
}

/// Whose processes a report's task readings span. The processor, memory,
/// storage and network readings are the machine's whatever the scope.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum MachineScope {
    /// Every process on the machine.
    Machine,
    /// The publishing principal's own processes alone.
    Own,
}

impl MachineScope {
    const fn as_u8(self) -> u8 {
        match self {
            Self::Machine => 1,
            Self::Own => 2,
        }
    }

    const fn from_u8(byte: u8) -> Result<Self, Errno> {
        match byte {
            1 => Ok(Self::Machine),
            2 => Ok(Self::Own),
            _ => Err(Errno::OutOfRange),
        }
    }
}

/// A bounded series of fractions, oldest first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MachineHistory {
    points: [u16; MACHINE_HISTORY_MAX],
    len: u8,
}

impl MachineHistory {
    /// No readings yet.
    pub const EMPTY: Self = Self {
        points: [0; MACHINE_HISTORY_MAX],
        len: 0,
    };

    /// Length (1), reserved (3), readings.
    const WIRE_LEN: usize = 4 + 2 * MACHINE_HISTORY_MAX;

    /// The series `points`, each a permille fraction, oldest first.
    ///
    /// # Errors
    ///
    /// * [`Errno::LengthOutOfRange`] — more than [`MACHINE_HISTORY_MAX`]
    ///   readings.
    /// * [`Errno::OutOfRange`] — a reading above `1000`.
    pub fn new(points: &[u16]) -> Result<Self, Errno> {
        if points.len() > MACHINE_HISTORY_MAX {
            return Err(Errno::LengthOutOfRange);
        }
        let len = u8::try_from(points.len()).map_err(|_| Errno::LengthOutOfRange)?;
        if points.iter().any(|&point| point > 1000) {
            return Err(Errno::OutOfRange);
        }
        let mut held = Self::EMPTY;
        held.points[..points.len()].copy_from_slice(points);
        held.len = len;
        Ok(held)
    }

    /// The readings, oldest first.
    #[must_use]
    pub fn points(&self) -> &[u16] {
        &self.points[..usize::from(self.len)]
    }

    /// Write the series into `out`, exactly [`Self::WIRE_LEN`] bytes.
    fn encode(&self, out: &mut [u8]) {
        out[0] = self.len;
        for (index, &point) in self.points().iter().enumerate() {
            put_u16(out, 4 + 2 * index, point);
        }
    }

    /// Read a series from `bytes`, exactly [`Self::WIRE_LEN`] bytes.
    fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        if !all_zero(&bytes[1..4]) {
            return Err(Errno::BadMagic);
        }
        let len = usize::from(bytes[0]);
        if len > MACHINE_HISTORY_MAX {
            return Err(Errno::LengthOutOfRange);
        }
        if !all_zero(&bytes[4 + 2 * len..Self::WIRE_LEN]) {
            return Err(Errno::BadMagic);
        }
        let mut points = [0u16; MACHINE_HISTORY_MAX];
        for (index, point) in points[..len].iter_mut().enumerate() {
            *point = read_u16(bytes, 4 + 2 * index);
        }
        Self::new(&points[..len])
    }
}

/// Each processor's busy share, in the machine's own processor order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MachineCores {
    cells: [u16; MACHINE_CORES_MAX],
    len: u16,
}

impl MachineCores {
    /// No processor read.
    pub const EMPTY: Self = Self {
        cells: [0; MACHINE_CORES_MAX],
        len: 0,
    };

    /// One reading per processor.
    const WIRE_LEN: usize = 2 * MACHINE_CORES_MAX;

    /// The processors `readings` names, `None` for one whose share was not
    /// measured this interval.
    ///
    /// # Errors
    ///
    /// [`Errno::LengthOutOfRange`] — more than [`MACHINE_CORES_MAX`].
    pub fn new(readings: impl IntoIterator<Item = Option<Permille>>) -> Result<Self, Errno> {
        let mut held = Self::EMPTY;
        for reading in readings {
            let slot = held
                .cells
                .get_mut(usize::from(held.len))
                .ok_or(Errno::LengthOutOfRange)?;
            *slot = reading.map_or(CORE_UNMEASURED, Permille::as_u16);
            held.len += 1;
        }
        Ok(held)
    }

    /// How many processors the report names.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.len as usize
    }

    /// Whether the report names no processor.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// Each processor's share in order, `None` where it was not measured.
    pub fn readings(&self) -> impl Iterator<Item = Option<Permille>> + '_ {
        self.cells[..usize::from(self.len)]
            .iter()
            .map(|&cell| Permille::new(cell).ok())
    }

    /// Write the readings into `out`, exactly [`Self::WIRE_LEN`] bytes.
    fn encode(&self, out: &mut [u8]) {
        for (index, &cell) in self.cells[..usize::from(self.len)].iter().enumerate() {
            put_u16(out, 2 * index, cell);
        }
    }

    /// Read `len` readings from `bytes`, exactly [`Self::WIRE_LEN`] bytes.
    fn decode(len: u16, bytes: &[u8]) -> Result<Self, Errno> {
        let count = usize::from(len);
        if count > MACHINE_CORES_MAX {
            return Err(Errno::LengthOutOfRange);
        }
        if !all_zero(&bytes[2 * count..Self::WIRE_LEN]) {
            return Err(Errno::BadMagic);
        }
        let mut readings = [None; MACHINE_CORES_MAX];
        for (index, reading) in readings[..count].iter_mut().enumerate() {
            let raw = read_u16(bytes, 2 * index);
            if raw != CORE_UNMEASURED {
                *reading = Some(Permille::new(raw)?);
            }
        }
        Self::new(readings[..count].iter().copied())
    }
}

/// What the processors are doing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MachineCpu {
    /// The machine's busy share over the last interval.
    pub busy: Option<Permille>,
    /// Whether the monitor holds the processors under pressure — the verdict
    /// its tray summary's rail is drawn from.
    pub pressured: bool,
    /// The run-queue load averages, with the thread and user census they are
    /// read beside.
    pub load: Option<LoadAverage>,
    /// Each processor's share.
    pub cores: MachineCores,
    /// The busy share over the readings so far, oldest first.
    pub history: MachineHistory,
}

/// Within the processor block, the offset of the busy share.
const CPU_BUSY: usize = 0;
/// Within the processor block, the offset of the core count.
const CPU_CORE_COUNT: usize = 2;
/// Within the processor block, the offset of the load averages.
const CPU_LOAD: usize = 4;
/// Within the processor block, the offset of the history.
const CPU_HISTORY: usize = CPU_LOAD + LoadAverage::WIRE_LEN;
/// Within the processor block, the offset of the per-core readings.
const CPU_CORES: usize = CPU_HISTORY + MachineHistory::WIRE_LEN;

impl MachineCpu {
    /// Nothing read.
    pub const UNREAD: Self = Self {
        busy: None,
        pressured: false,
        load: None,
        cores: MachineCores::EMPTY,
        history: MachineHistory::EMPTY,
    };

    /// The processor block's wire size.
    const WIRE_LEN: usize = CPU_CORES + MachineCores::WIRE_LEN;

    /// Write the block into `out`, exactly [`Self::WIRE_LEN`] bytes,
    /// answering the frame flags it sets.
    fn encode(&self, out: &mut [u8]) -> u16 {
        let mut flags = 0;
        if let Some(busy) = self.busy {
            flags |= HAS_CPU_BUSY;
            put_u16(out, CPU_BUSY, busy.as_u16());
        }
        if self.pressured {
            flags |= CPU_PRESSURED;
        }
        if let Some(load) = self.load {
            flags |= HAS_LOAD;
            out[CPU_LOAD..CPU_HISTORY].copy_from_slice(&load.to_le_bytes());
        }
        // The count fits: construction admits at most MACHINE_CORES_MAX.
        put_u16(out, CPU_CORE_COUNT, self.cores.len);
        self.history.encode(&mut out[CPU_HISTORY..CPU_CORES]);
        self.cores.encode(&mut out[CPU_CORES..Self::WIRE_LEN]);
        flags
    }

    /// Read the block from `bytes`, exactly [`Self::WIRE_LEN`] bytes, under
    /// the frame's `flags`.
    fn decode(flags: u16, bytes: &[u8]) -> Result<Self, Errno> {
        Ok(Self {
            busy: present(
                flags & HAS_CPU_BUSY != 0,
                &bytes[CPU_BUSY..CPU_CORE_COUNT],
                || Permille::new(read_u16(bytes, CPU_BUSY)),
            )?,
            pressured: flags & CPU_PRESSURED != 0,
            load: present(flags & HAS_LOAD != 0, &bytes[CPU_LOAD..CPU_HISTORY], || {
                LoadAverage::from_bytes(&bytes[CPU_LOAD..CPU_HISTORY])
            })?,
            cores: MachineCores::decode(
                read_u16(bytes, CPU_CORE_COUNT),
                &bytes[CPU_CORES..Self::WIRE_LEN],
            )?,
            history: MachineHistory::decode(&bytes[CPU_HISTORY..CPU_CORES])?,
        })
    }
}

/// The committed share of memory, and the whole it is a share of.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MachineCommitted {
    total_bytes: u64,
    used: Permille,
}

impl MachineCommitted {
    /// `used` of `total_bytes`.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] — a zero total, which no share can be of.
    pub const fn new(total_bytes: u64, used: Permille) -> Result<Self, Errno> {
        if total_bytes == 0 {
            return Err(Errno::OutOfRange);
        }
        Ok(Self { total_bytes, used })
    }

    /// The whole, in bytes.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// The committed share of it.
    #[must_use]
    pub const fn used(&self) -> Permille {
        self.used
    }
}

/// Where the RAM went: the bytes the kernel charges to each memory class,
/// and what is free.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MachineComposition {
    classes: [u64; MEMORY_CLASS_COUNT],
    free: u64,
    total: u64,
}

impl MachineComposition {
    /// `class_bytes`, indexed by [`MemoryClass::index`](crate::MemoryClass::index),
    /// and `free_bytes`, of `total_bytes`.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] — a zero total, or parts summing past it: a
    /// composition accounting for more than the whole is not one.
    pub fn new(
        class_bytes: [u64; MEMORY_CLASS_COUNT],
        free_bytes: u64,
        total_bytes: u64,
    ) -> Result<Self, Errno> {
        let parts = class_bytes
            .iter()
            .try_fold(free_bytes, |sum, &bytes| sum.checked_add(bytes))
            .ok_or(Errno::OutOfRange)?;
        if total_bytes == 0 || parts > total_bytes {
            return Err(Errno::OutOfRange);
        }
        Ok(Self {
            classes: class_bytes,
            free: free_bytes,
            total: total_bytes,
        })
    }

    /// The bytes charged to each class, indexed by
    /// [`MemoryClass::index`](crate::MemoryClass::index).
    #[must_use]
    pub const fn class_bytes(&self) -> &[u64; MEMORY_CLASS_COUNT] {
        &self.classes
    }

    /// The bytes free.
    #[must_use]
    pub const fn free_bytes(&self) -> u64 {
        self.free
    }

    /// The whole the parts are of.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.total
    }
}

/// What memory is doing.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MachineMemory {
    /// The committed share.
    pub committed: Option<MachineCommitted>,
    /// The pressure band the kernel reports.
    pub band: Option<MemoryBand>,
    /// Whether the monitor holds memory under pressure — the verdict its tray
    /// summary is drawn from.
    pub pressured: bool,
    /// Where the RAM went.
    pub composition: Option<MachineComposition>,
    /// The committed share over the readings so far, oldest first.
    pub history: MachineHistory,
}

/// Within the memory block, the offset of the committed whole.
const MEM_TOTAL: usize = 0;
/// Within the memory block, the offset of the committed share.
const MEM_USED: usize = 8;
/// Within the memory block, the offset of the band.
const MEM_BAND: usize = 10;
/// Within the memory block, the offset of the history.
const MEM_HISTORY: usize = 16;
/// Within the memory block, the offset of the class bytes.
const MEM_CLASSES: usize = MEM_HISTORY + MachineHistory::WIRE_LEN + 4;
/// Within the memory block, the offset of the free bytes.
const MEM_FREE: usize = MEM_CLASSES + 8 * MEMORY_CLASS_COUNT;
/// Within the memory block, the offset of the composition's whole.
const MEM_COMPOSITION_TOTAL: usize = MEM_FREE + 8;

impl MachineMemory {
    /// Nothing read.
    pub const UNREAD: Self = Self {
        committed: None,
        band: None,
        pressured: false,
        composition: None,
        history: MachineHistory::EMPTY,
    };

    /// The memory block's wire size.
    const WIRE_LEN: usize = MEM_COMPOSITION_TOTAL + 8;

    /// Write the block into `out`, exactly [`Self::WIRE_LEN`] bytes,
    /// answering the frame flags it sets.
    fn encode(&self, out: &mut [u8]) -> u16 {
        let mut flags = 0;
        if let Some(committed) = self.committed {
            flags |= HAS_COMMITTED;
            put_u64(out, MEM_TOTAL, committed.total_bytes);
            put_u16(out, MEM_USED, committed.used.as_u16());
        }
        if let Some(band) = self.band {
            flags |= HAS_BAND;
            out[MEM_BAND] = band.depth();
        }
        if self.pressured {
            flags |= MEMORY_PRESSURED;
        }
        self.history
            .encode(&mut out[MEM_HISTORY..MEM_HISTORY + MachineHistory::WIRE_LEN]);
        if let Some(composition) = self.composition {
            flags |= HAS_COMPOSITION;
            for (index, &bytes) in composition.classes.iter().enumerate() {
                put_u64(out, MEM_CLASSES + 8 * index, bytes);
            }
            put_u64(out, MEM_FREE, composition.free);
            put_u64(out, MEM_COMPOSITION_TOTAL, composition.total);
        }
        flags
    }

    /// Read the block from `bytes`, exactly [`Self::WIRE_LEN`] bytes, under
    /// the frame's `flags`.
    fn decode(flags: u16, bytes: &[u8]) -> Result<Self, Errno> {
        if !all_zero(&bytes[MEM_BAND + 1..MEM_HISTORY])
            || !all_zero(&bytes[MEM_HISTORY + MachineHistory::WIRE_LEN..MEM_CLASSES])
        {
            return Err(Errno::BadMagic);
        }
        let committed = present(
            flags & HAS_COMMITTED != 0,
            &bytes[MEM_TOTAL..MEM_BAND],
            || {
                MachineCommitted::new(
                    read_u64(bytes, MEM_TOTAL),
                    Permille::new(read_u16(bytes, MEM_USED))?,
                )
            },
        )?;
        let band = present(flags & HAS_BAND != 0, &bytes[MEM_BAND..=MEM_BAND], || {
            MemoryBand::new(bytes[MEM_BAND])
        })?;
        let composition = present(
            flags & HAS_COMPOSITION != 0,
            &bytes[MEM_CLASSES..Self::WIRE_LEN],
            || {
                let mut class_bytes = [0u64; MEMORY_CLASS_COUNT];
                for (index, slot) in class_bytes.iter_mut().enumerate() {
                    *slot = read_u64(bytes, MEM_CLASSES + 8 * index);
                }
                MachineComposition::new(
                    class_bytes,
                    read_u64(bytes, MEM_FREE),
                    read_u64(bytes, MEM_COMPOSITION_TOTAL),
                )
            },
        )?;
        Ok(Self {
            committed,
            band,
            pressured: flags & MEMORY_PRESSURED != 0,
            composition,
            history: MachineHistory::decode(
                &bytes[MEM_HISTORY..MEM_HISTORY + MachineHistory::WIRE_LEN],
            )?,
        })
    }
}

/// One of the busiest tasks.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MachineTask {
    /// The program's name.
    pub name: TrayTaskName,
    /// Its share of one processor over the last interval.
    pub cpu: Permille,
    /// The memory mapped into it.
    pub memory_bytes: u64,
}

/// One named task's wire size: name length (1), name, reserved (1), share
/// (2), reserved (4), memory (8).
const TASK_LEN: usize = 1 + TRAY_TASK_NAME_MAX + 1 + 2 + 4 + 8;
/// Within a named task, the offset of its share.
const TASK_CPU: usize = 1 + TRAY_TASK_NAME_MAX + 1;
/// Within a named task, the offset of its memory.
const TASK_MEMORY: usize = TASK_CPU + 2 + 4;

impl MachineTask {
    /// Write the task into `out`.
    fn encode(&self, out: &mut [u8; TASK_LEN]) {
        out[0] = self.name.len_byte();
        out[1..=TRAY_TASK_NAME_MAX].copy_from_slice(self.name.raw_bytes());
        put_u16(out, TASK_CPU, self.cpu.as_u16());
        put_u64(out, TASK_MEMORY, self.memory_bytes);
    }

    /// Read a task from `bytes`.
    fn decode(bytes: &[u8; TASK_LEN]) -> Result<Self, Errno> {
        if bytes[TASK_CPU - 1] != 0 || !all_zero(&bytes[TASK_CPU + 2..TASK_MEMORY]) {
            return Err(Errno::BadMagic);
        }
        let mut name = [0u8; TRAY_TASK_NAME_MAX];
        name.copy_from_slice(&bytes[1..=TRAY_TASK_NAME_MAX]);
        Ok(Self {
            name: TrayTaskName::from_wire(bytes[0], &name)?,
            cpu: Permille::new(read_u16(bytes, TASK_CPU))?,
            memory_bytes: read_u64(bytes, TASK_MEMORY),
        })
    }
}

/// The task census, and the tasks costing the processors most.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MachineTasks {
    count: Option<u32>,
    stopped: u16,
    recovery: u16,
    busiest: [Option<MachineTask>; MACHINE_TASKS_MAX],
}

/// Within the task block, the offset of the named-task count.
const TASKS_NAMED: usize = 8;
/// Within the task block, the offset of the first named task.
const TASKS_FIRST: usize = 16;

impl MachineTasks {
    /// Nothing read.
    pub const UNREAD: Self = Self {
        count: None,
        stopped: 0,
        recovery: 0,
        busiest: [None; MACHINE_TASKS_MAX],
    };

    /// Count (4), stopped (2), recovery (2), named count (1), reserved (7),
    /// then the named tasks.
    const WIRE_LEN: usize = TASKS_FIRST + MACHINE_TASKS_MAX * TASK_LEN;

    /// `count` tasks, `stopped` of them stopped and `recovery` awaiting
    /// recovery — stopped or no longer answering — the busiest of them
    /// `busiest`, busiest first.
    ///
    /// # Errors
    ///
    /// * [`Errno::LengthOutOfRange`] — more than [`MACHINE_TASKS_MAX`] named.
    /// * [`Errno::OutOfRange`] — more stopped, awaiting recovery or named
    ///   than counted, or any of them with no count at all: an unread list has
    ///   nothing in it to name.
    pub fn new(
        count: Option<u32>,
        stopped: u16,
        recovery: u16,
        busiest: &[MachineTask],
    ) -> Result<Self, Errno> {
        Self::from_slots(count, stopped, recovery, &slots_of(busiest)?)
    }

    /// The one validation both construction and decode pass through.
    fn from_slots(
        count: Option<u32>,
        stopped: u16,
        recovery: u16,
        busiest: &[Option<MachineTask>; MACHINE_TASKS_MAX],
    ) -> Result<Self, Errno> {
        let counted = count.unwrap_or(0);
        let named = u32::try_from(named_prefix(busiest)?).map_err(|_| Errno::LengthOutOfRange)?;
        if u32::from(stopped) > counted || u32::from(recovery) > counted || named > counted {
            return Err(Errno::OutOfRange);
        }
        Ok(Self {
            count,
            stopped,
            recovery,
            busiest: *busiest,
        })
    }

    /// How many tasks there are, when the list could be read.
    #[must_use]
    pub const fn count(&self) -> Option<u32> {
        self.count
    }

    /// How many of them are stopped.
    #[must_use]
    pub const fn stopped(&self) -> u16 {
        self.stopped
    }

    /// How many await recovery.
    #[must_use]
    pub const fn recovery(&self) -> u16 {
        self.recovery
    }

    /// The busiest tasks, busiest first.
    pub fn busiest(&self) -> impl Iterator<Item = &MachineTask> {
        self.busiest.iter().map_while(Option::as_ref)
    }

    /// Write the block into `out`, exactly [`Self::WIRE_LEN`] bytes,
    /// answering the frame flags it sets.
    fn encode(&self, out: &mut [u8]) -> u16 {
        put_u32(out, 0, self.count.unwrap_or(0));
        put_u16(out, 4, self.stopped);
        put_u16(out, 6, self.recovery);
        let mut named = 0u8;
        let (records, _) = out[TASKS_FIRST..].as_chunks_mut::<TASK_LEN>();
        for (task, slot) in self.busiest().zip(records) {
            task.encode(slot);
            named += 1;
        }
        out[TASKS_NAMED] = named;
        if self.count.is_some() {
            HAS_TASK_COUNT
        } else {
            0
        }
    }

    /// Read the block from `bytes`, exactly [`Self::WIRE_LEN`] bytes, under
    /// the frame's `flags`.
    fn decode(flags: u16, bytes: &[u8]) -> Result<Self, Errno> {
        if !all_zero(&bytes[TASKS_NAMED + 1..TASKS_FIRST]) {
            return Err(Errno::BadMagic);
        }
        let named = usize::from(bytes[TASKS_NAMED]);
        if named > MACHINE_TASKS_MAX {
            return Err(Errno::LengthOutOfRange);
        }
        if !all_zero(&bytes[TASKS_FIRST + named * TASK_LEN..Self::WIRE_LEN]) {
            return Err(Errno::BadMagic);
        }
        let count = present(flags & HAS_TASK_COUNT != 0, &bytes[0..4], || {
            Ok(read_u32(bytes, 0))
        })?;
        let mut busiest = [None; MACHINE_TASKS_MAX];
        let (records, _) = bytes[TASKS_FIRST..Self::WIRE_LEN].as_chunks::<TASK_LEN>();
        for (slot, record) in busiest[..named].iter_mut().zip(records) {
            *slot = Some(MachineTask::decode(record)?);
        }
        Self::from_slots(count, read_u16(bytes, 4), read_u16(bytes, 6), &busiest)
    }
}

/// How much a storage device holds, and of what.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct DeviceCapacity {
    total_bytes: u64,
    used_bytes: u64,
}

impl DeviceCapacity {
    /// `used_bytes` of `total_bytes`.
    ///
    /// # Errors
    ///
    /// [`Errno::OutOfRange`] — a zero total, or more used than there is.
    pub const fn new(total_bytes: u64, used_bytes: u64) -> Result<Self, Errno> {
        if total_bytes == 0 || used_bytes > total_bytes {
            return Err(Errno::OutOfRange);
        }
        Ok(Self {
            total_bytes,
            used_bytes,
        })
    }

    /// The whole, in bytes.
    #[must_use]
    pub const fn total_bytes(&self) -> u64 {
        self.total_bytes
    }

    /// The bytes in use.
    #[must_use]
    pub const fn used_bytes(&self) -> u64 {
        self.used_bytes
    }
}

/// One storage device, and how it is faring.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MachineDevice {
    /// The device's name, as the Switchboard names it.
    pub name: MachineDeviceName,
    /// The availability of its least available volume; a reader bands it
    /// through [`MountAvailability::health`].
    pub availability: MountAvailability,
    /// What its volumes hold between them, where they keep a fixed capacity.
    pub capacity: Option<DeviceCapacity>,
    /// How much of the last interval it spent serving requests.
    pub busy: Option<Permille>,
    /// Bytes read per second over the last interval.
    pub read_rate: Option<u64>,
    /// Bytes written per second over the last interval.
    pub write_rate: Option<u64>,
}

/// One device's wire size: name length (1), name, availability (1), flags
/// (1), reserved (1), busy share (2), reserved (2), capacity (16), rates
/// (16).
const DEVICE_LEN: usize = 1 + MACHINE_DEVICE_NAME_MAX + 1 + 1 + 1 + 2 + 2 + 32;
/// Within a device, the offset of its availability.
const DEVICE_AVAILABILITY: usize = 1 + MACHINE_DEVICE_NAME_MAX;
/// Within a device, the offset of its presence flags.
const DEVICE_FLAGS: usize = DEVICE_AVAILABILITY + 1;
/// Within a device, the offset of its busy share.
const DEVICE_BUSY: usize = DEVICE_FLAGS + 2;
/// Within a device, the offset of its capacity's whole.
const DEVICE_TOTAL: usize = DEVICE_BUSY + 4;
/// Within a device, the offset of its capacity's used bytes.
const DEVICE_USED: usize = DEVICE_TOTAL + 8;
/// Within a device, the offset of its read rate.
const DEVICE_READ: usize = DEVICE_USED + 8;
/// Within a device, the offset of its write rate.
const DEVICE_WRITE: usize = DEVICE_READ + 8;

/// A device's capacity is present.
const DEVICE_HAS_CAPACITY: u8 = 1 << 0;
/// A device's busy share is present.
const DEVICE_HAS_BUSY: u8 = 1 << 1;
/// A device's read rate is present.
const DEVICE_HAS_READ: u8 = 1 << 2;
/// A device's write rate is present.
const DEVICE_HAS_WRITE: u8 = 1 << 3;
/// Every device flag this version defines.
const DEVICE_FLAGS_KNOWN: u8 =
    DEVICE_HAS_CAPACITY | DEVICE_HAS_BUSY | DEVICE_HAS_READ | DEVICE_HAS_WRITE;

impl MachineDevice {
    /// Write the device into `out`.
    fn encode(&self, out: &mut [u8; DEVICE_LEN]) {
        out[0] = self.name.len_byte();
        out[1..=MACHINE_DEVICE_NAME_MAX].copy_from_slice(self.name.raw_bytes());
        out[DEVICE_AVAILABILITY] = self.availability.as_u8();
        let mut flags = 0;
        if let Some(capacity) = self.capacity {
            flags |= DEVICE_HAS_CAPACITY;
            put_u64(out, DEVICE_TOTAL, capacity.total_bytes);
            put_u64(out, DEVICE_USED, capacity.used_bytes);
        }
        if let Some(busy) = self.busy {
            flags |= DEVICE_HAS_BUSY;
            put_u16(out, DEVICE_BUSY, busy.as_u16());
        }
        if let Some(read) = self.read_rate {
            flags |= DEVICE_HAS_READ;
            put_u64(out, DEVICE_READ, read);
        }
        if let Some(write) = self.write_rate {
            flags |= DEVICE_HAS_WRITE;
            put_u64(out, DEVICE_WRITE, write);
        }
        out[DEVICE_FLAGS] = flags;
    }

    /// Read a device from `bytes`.
    fn decode(bytes: &[u8; DEVICE_LEN]) -> Result<Self, Errno> {
        let flags = bytes[DEVICE_FLAGS];
        if flags & !DEVICE_FLAGS_KNOWN != 0
            || bytes[DEVICE_FLAGS + 1] != 0
            || !all_zero(&bytes[DEVICE_BUSY + 2..DEVICE_TOTAL])
        {
            return Err(Errno::BadMagic);
        }
        let mut name = [0u8; MACHINE_DEVICE_NAME_MAX];
        name.copy_from_slice(&bytes[1..=MACHINE_DEVICE_NAME_MAX]);
        Ok(Self {
            name: MachineDeviceName::from_wire(bytes[0], &name)?,
            availability: MountAvailability::from_u8(bytes[DEVICE_AVAILABILITY])?,
            capacity: present(
                flags & DEVICE_HAS_CAPACITY != 0,
                &bytes[DEVICE_TOTAL..DEVICE_READ],
                || DeviceCapacity::new(read_u64(bytes, DEVICE_TOTAL), read_u64(bytes, DEVICE_USED)),
            )?,
            busy: present(
                flags & DEVICE_HAS_BUSY != 0,
                &bytes[DEVICE_BUSY..DEVICE_BUSY + 2],
                || Permille::new(read_u16(bytes, DEVICE_BUSY)),
            )?,
            read_rate: present(
                flags & DEVICE_HAS_READ != 0,
                &bytes[DEVICE_READ..DEVICE_WRITE],
                || Ok(read_u64(bytes, DEVICE_READ)),
            )?,
            write_rate: present(
                flags & DEVICE_HAS_WRITE != 0,
                &bytes[DEVICE_WRITE..DEVICE_LEN],
                || Ok(read_u64(bytes, DEVICE_WRITE)),
            )?,
        })
    }
}

/// The storage devices, the least healthy first.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MachineStorage {
    total: u16,
    devices: [Option<MachineDevice>; MACHINE_DEVICES_MAX],
}

impl MachineStorage {
    /// Total (2), named count (1), reserved (5), then the named devices.
    const WIRE_LEN: usize = 8 + MACHINE_DEVICES_MAX * DEVICE_LEN;

    /// `devices` named, of `total` on the machine.
    ///
    /// # Errors
    ///
    /// * [`Errno::LengthOutOfRange`] — more than [`MACHINE_DEVICES_MAX`]
    ///   named.
    /// * [`Errno::OutOfRange`] — a total below the devices named.
    pub fn new(total: u16, devices: &[MachineDevice]) -> Result<Self, Errno> {
        Self::from_slots(total, &slots_of(devices)?)
    }

    /// The one validation both construction and decode pass through.
    fn from_slots(
        total: u16,
        devices: &[Option<MachineDevice>; MACHINE_DEVICES_MAX],
    ) -> Result<Self, Errno> {
        if usize::from(total) < named_prefix(devices)? {
            return Err(Errno::OutOfRange);
        }
        Ok(Self {
            total,
            devices: *devices,
        })
    }

    /// How many storage devices the machine has, named or not.
    #[must_use]
    pub const fn total(&self) -> u16 {
        self.total
    }

    /// The devices named, least healthy first.
    pub fn devices(&self) -> impl Iterator<Item = &MachineDevice> {
        self.devices.iter().map_while(Option::as_ref)
    }

    /// Write the block into `out`, exactly [`Self::WIRE_LEN`] bytes.
    fn encode(&self, out: &mut [u8]) {
        put_u16(out, 0, self.total);
        let mut named = 0u8;
        let (records, _) = out[8..].as_chunks_mut::<DEVICE_LEN>();
        for (device, slot) in self.devices().zip(records) {
            device.encode(slot);
            named += 1;
        }
        out[2] = named;
    }

    /// Read the block from `bytes`, exactly [`Self::WIRE_LEN`] bytes.
    fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        if !all_zero(&bytes[3..8]) {
            return Err(Errno::BadMagic);
        }
        let named = usize::from(bytes[2]);
        if named > MACHINE_DEVICES_MAX {
            return Err(Errno::LengthOutOfRange);
        }
        if !all_zero(&bytes[8 + named * DEVICE_LEN..Self::WIRE_LEN]) {
            return Err(Errno::BadMagic);
        }
        let mut devices = [None; MACHINE_DEVICES_MAX];
        let (records, _) = bytes[8..Self::WIRE_LEN].as_chunks::<DEVICE_LEN>();
        for (slot, record) in devices[..named].iter_mut().zip(records) {
            *slot = Some(MachineDevice::decode(record)?);
        }
        Self::from_slots(read_u16(bytes, 0), &devices)
    }
}

/// One network interface, and what it is carrying.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct MachineInterface {
    /// The interface's name.
    pub name: MachineInterfaceName,
    /// Whether its link carries frames, where the stack says.
    pub link_up: Option<bool>,
    /// Bytes received per second, over the stack's own averaging window.
    pub receive_rate: Option<u64>,
    /// Bytes sent per second, over the stack's own averaging window.
    pub send_rate: Option<u64>,
}

/// One interface's wire size: name length (1), name, link (1), flags (1),
/// reserved (5), rates (16).
const INTERFACE_LEN: usize = 1 + IF_NAME_LEN + 1 + 1 + 5 + 16;
/// Within an interface, the offset of its link state.
const INTERFACE_LINK: usize = 1 + IF_NAME_LEN;
/// Within an interface, the offset of its presence flags.
const INTERFACE_FLAGS: usize = INTERFACE_LINK + 1;
/// Within an interface, the offset of its receive rate.
const INTERFACE_RX: usize = INTERFACE_FLAGS + 6;
/// Within an interface, the offset of its send rate.
const INTERFACE_TX: usize = INTERFACE_RX + 8;

/// A link state the stack did not report.
const LINK_UNKNOWN: u8 = 0;
/// A link carrying frames.
const LINK_UP: u8 = 1;
/// A link carrying none.
const LINK_DOWN: u8 = 2;

/// An interface's receive rate is present.
const INTERFACE_HAS_RX: u8 = 1 << 0;
/// An interface's send rate is present.
const INTERFACE_HAS_TX: u8 = 1 << 1;
/// Every interface flag this version defines.
const INTERFACE_FLAGS_KNOWN: u8 = INTERFACE_HAS_RX | INTERFACE_HAS_TX;

impl MachineInterface {
    /// Write the interface into `out`.
    fn encode(&self, out: &mut [u8; INTERFACE_LEN]) {
        out[0] = self.name.len_byte();
        out[1..=IF_NAME_LEN].copy_from_slice(self.name.raw_bytes());
        out[INTERFACE_LINK] = match self.link_up {
            None => LINK_UNKNOWN,
            Some(true) => LINK_UP,
            Some(false) => LINK_DOWN,
        };
        let mut flags = 0;
        if let Some(rx) = self.receive_rate {
            flags |= INTERFACE_HAS_RX;
            put_u64(out, INTERFACE_RX, rx);
        }
        if let Some(tx) = self.send_rate {
            flags |= INTERFACE_HAS_TX;
            put_u64(out, INTERFACE_TX, tx);
        }
        out[INTERFACE_FLAGS] = flags;
    }

    /// Read an interface from `bytes`.
    fn decode(bytes: &[u8; INTERFACE_LEN]) -> Result<Self, Errno> {
        let flags = bytes[INTERFACE_FLAGS];
        if flags & !INTERFACE_FLAGS_KNOWN != 0
            || !all_zero(&bytes[INTERFACE_FLAGS + 1..INTERFACE_RX])
        {
            return Err(Errno::BadMagic);
        }
        let link_up = match bytes[INTERFACE_LINK] {
            LINK_UNKNOWN => None,
            LINK_UP => Some(true),
            LINK_DOWN => Some(false),
            _ => return Err(Errno::OutOfRange),
        };
        let mut name = [0u8; IF_NAME_LEN];
        name.copy_from_slice(&bytes[1..=IF_NAME_LEN]);
        Ok(Self {
            name: MachineInterfaceName::from_wire(bytes[0], &name)?,
            link_up,
            receive_rate: present(
                flags & INTERFACE_HAS_RX != 0,
                &bytes[INTERFACE_RX..INTERFACE_TX],
                || Ok(read_u64(bytes, INTERFACE_RX)),
            )?,
            send_rate: present(
                flags & INTERFACE_HAS_TX != 0,
                &bytes[INTERFACE_TX..INTERFACE_LEN],
                || Ok(read_u64(bytes, INTERFACE_TX)),
            )?,
        })
    }
}

/// The network interfaces, in the stack's own order.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MachineNetwork {
    total: u16,
    interfaces: [Option<MachineInterface>; MACHINE_INTERFACES_MAX],
}

impl MachineNetwork {
    /// Total (2), named count (1), reserved (5), then the named interfaces.
    const WIRE_LEN: usize = 8 + MACHINE_INTERFACES_MAX * INTERFACE_LEN;

    /// `interfaces` named, of `total` on the machine.
    ///
    /// # Errors
    ///
    /// * [`Errno::LengthOutOfRange`] — more than [`MACHINE_INTERFACES_MAX`]
    ///   named.
    /// * [`Errno::OutOfRange`] — a total below the interfaces named.
    pub fn new(total: u16, interfaces: &[MachineInterface]) -> Result<Self, Errno> {
        Self::from_slots(total, &slots_of(interfaces)?)
    }

    /// The one validation both construction and decode pass through.
    fn from_slots(
        total: u16,
        interfaces: &[Option<MachineInterface>; MACHINE_INTERFACES_MAX],
    ) -> Result<Self, Errno> {
        if usize::from(total) < named_prefix(interfaces)? {
            return Err(Errno::OutOfRange);
        }
        Ok(Self {
            total,
            interfaces: *interfaces,
        })
    }

    /// How many interfaces the machine has, named or not.
    #[must_use]
    pub const fn total(&self) -> u16 {
        self.total
    }

    /// The interfaces named, in the stack's order.
    pub fn interfaces(&self) -> impl Iterator<Item = &MachineInterface> {
        self.interfaces.iter().map_while(Option::as_ref)
    }

    /// Write the block into `out`, exactly [`Self::WIRE_LEN`] bytes.
    fn encode(&self, out: &mut [u8]) {
        put_u16(out, 0, self.total);
        let mut named = 0u8;
        let (records, _) = out[8..].as_chunks_mut::<INTERFACE_LEN>();
        for (interface, slot) in self.interfaces().zip(records) {
            interface.encode(slot);
            named += 1;
        }
        out[2] = named;
    }

    /// Read the block from `bytes`, exactly [`Self::WIRE_LEN`] bytes.
    fn decode(bytes: &[u8]) -> Result<Self, Errno> {
        if !all_zero(&bytes[3..8]) {
            return Err(Errno::BadMagic);
        }
        let named = usize::from(bytes[2]);
        if named > MACHINE_INTERFACES_MAX {
            return Err(Errno::LengthOutOfRange);
        }
        if !all_zero(&bytes[8 + named * INTERFACE_LEN..Self::WIRE_LEN]) {
            return Err(Errno::BadMagic);
        }
        let mut interfaces = [None; MACHINE_INTERFACES_MAX];
        let (records, _) = bytes[8..Self::WIRE_LEN].as_chunks::<INTERFACE_LEN>();
        for (slot, record) in interfaces[..named].iter_mut().zip(records) {
            *slot = Some(MachineInterface::decode(record)?);
        }
        Self::from_slots(read_u16(bytes, 0), &interfaces)
    }
}

/// The frame's uptime is present.
const HAS_UPTIME: u16 = 1 << 0;
/// The processors' busy share is present.
const HAS_CPU_BUSY: u16 = 1 << 1;
/// The load averages are present.
const HAS_LOAD: u16 = 1 << 2;
/// The processors are under pressure.
const CPU_PRESSURED: u16 = 1 << 3;
/// Memory's committed share is present.
const HAS_COMMITTED: u16 = 1 << 4;
/// Memory's pressure band is present.
const HAS_BAND: u16 = 1 << 5;
/// Memory is under pressure.
const MEMORY_PRESSURED: u16 = 1 << 6;
/// Memory's composition is present.
const HAS_COMPOSITION: u16 = 1 << 7;
/// The task count is present.
const HAS_TASK_COUNT: u16 = 1 << 8;
/// The storage devices are present.
const HAS_STORAGE: u16 = 1 << 9;
/// The network interfaces are present.
const HAS_NETWORK: u16 = 1 << 10;
/// Every frame flag this version defines.
const FLAGS_KNOWN: u16 = (1 << 11) - 1;

/// Offset of the frame flags.
const FLAGS_OFFSET: usize = 6;
/// Offset of the sampling period, in milliseconds.
const PERIOD_OFFSET: usize = 8;
/// Offset of the task scope.
const SCOPE_OFFSET: usize = 12;
/// Offset of the machine name's length (`0` = no name).
const HOST_LEN_OFFSET: usize = 13;
/// Offset of the machine name.
const HOST_OFFSET: usize = 16;
/// Offset of the uptime.
const UPTIME_OFFSET: usize = HOST_OFFSET + HOSTNAME_MAX;
/// Offset of the processor block.
const CPU_OFFSET: usize = UPTIME_OFFSET + Duration64::WIRE_LEN;
/// Offset of the memory block.
const MEMORY_OFFSET: usize = CPU_OFFSET + MachineCpu::WIRE_LEN;
/// Offset of the task block.
const TASKS_OFFSET: usize = MEMORY_OFFSET + MachineMemory::WIRE_LEN;
/// Offset of the storage block.
const STORAGE_OFFSET: usize = TASKS_OFFSET + MachineTasks::WIRE_LEN;
/// Offset of the network block.
const NETWORK_OFFSET: usize = STORAGE_OFFSET + MachineStorage::WIRE_LEN;

/// What the machine is doing, as one Switchboard sample saw it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MachineReport {
    /// How often reports come.
    pub period: ReportPeriod,
    /// Whose processes the task readings span.
    pub scope: MachineScope,
    /// The machine's name.
    pub host: Option<MachineHost>,
    /// How long the machine has been up.
    pub uptime: Option<Duration64>,
    /// The processors.
    pub cpu: MachineCpu,
    /// Memory.
    pub memory: MachineMemory,
    /// The tasks.
    pub tasks: MachineTasks,
    /// The storage devices, where the mount table could be read.
    pub storage: Option<MachineStorage>,
    /// The network interfaces, where the stack could be asked.
    pub network: Option<MachineNetwork>,
}

impl MachineReport {
    /// Encoded size on the wire.
    pub const WIRE_LEN: usize = NETWORK_OFFSET + MachineNetwork::WIRE_LEN;

    /// Encode `self` little-endian.
    #[must_use]
    pub fn to_le_bytes(&self) -> [u8; Self::WIRE_LEN] {
        let mut out = [0u8; Self::WIRE_LEN];
        put_u32(&mut out, 0, MACHINE_REPORT_MAGIC);
        put_u16(&mut out, 4, SWITCHBOARD_VERSION_V1);
        put_u32(&mut out, PERIOD_OFFSET, self.period.as_millis());
        out[SCOPE_OFFSET] = self.scope.as_u8();
        let mut flags = 0;
        if let Some(host) = self.host {
            out[HOST_LEN_OFFSET] = host.len_byte();
            out[HOST_OFFSET..UPTIME_OFFSET].copy_from_slice(host.raw_bytes());
        }
        if let Some(uptime) = self.uptime {
            flags |= HAS_UPTIME;
            out[UPTIME_OFFSET..CPU_OFFSET].copy_from_slice(&uptime.to_le_bytes());
        }
        flags |= self.cpu.encode(&mut out[CPU_OFFSET..MEMORY_OFFSET]);
        flags |= self.memory.encode(&mut out[MEMORY_OFFSET..TASKS_OFFSET]);
        flags |= self.tasks.encode(&mut out[TASKS_OFFSET..STORAGE_OFFSET]);
        if let Some(storage) = &self.storage {
            flags |= HAS_STORAGE;
            storage.encode(&mut out[STORAGE_OFFSET..NETWORK_OFFSET]);
        }
        if let Some(network) = &self.network {
            flags |= HAS_NETWORK;
            network.encode(&mut out[NETWORK_OFFSET..Self::WIRE_LEN]);
        }
        put_u16(&mut out, FLAGS_OFFSET, flags);
        out
    }

    /// Decode from `bytes`, failing closed on any malformed input.
    ///
    /// # Errors
    ///
    /// * [`Errno::BufferTooSmall`] — `bytes` cannot hold a whole report.
    /// * [`Errno::BadMagic`] — the wrong magic, a flag this version does not
    ///   define, or a reserved or absent field that is not zero.
    /// * [`Errno::AbiVersionUnsupported`] — not `switchboard-v1`.
    /// * [`Errno::OutOfRange`] — a period, scope, band, availability or link
    ///   outside its closed set, a fraction above full, a zero whole, parts
    ///   summing past their whole, more used than held, or a count below the
    ///   entries it names.
    /// * [`Errno::LengthOutOfRange`] — a list or name longer than its bound.
    /// * [`Errno::TimestampOutOfRange`] — an uptime whose nanoseconds are not
    ///   canonical.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Errno> {
        if bytes.len() < Self::WIRE_LEN {
            return Err(Errno::BufferTooSmall);
        }
        if read_u32(bytes, 0) != MACHINE_REPORT_MAGIC {
            return Err(Errno::BadMagic);
        }
        if read_u16(bytes, 4) != SWITCHBOARD_VERSION_V1 {
            return Err(Errno::AbiVersionUnsupported);
        }
        let flags = read_u16(bytes, FLAGS_OFFSET);
        if flags & !FLAGS_KNOWN != 0 || !all_zero(&bytes[HOST_LEN_OFFSET + 1..HOST_OFFSET]) {
            return Err(Errno::BadMagic);
        }
        let mut host = [0u8; HOSTNAME_MAX];
        host.copy_from_slice(&bytes[HOST_OFFSET..UPTIME_OFFSET]);
        let host = match bytes[HOST_LEN_OFFSET] {
            0 if all_zero(&host) => None,
            0 => return Err(Errno::BadMagic),
            len => Some(MachineHost::from_wire(len, &host)?),
        };
        Ok(Self {
            period: ReportPeriod::from_millis(read_u32(bytes, PERIOD_OFFSET))?,
            scope: MachineScope::from_u8(bytes[SCOPE_OFFSET])?,
            host,
            uptime: present(
                flags & HAS_UPTIME != 0,
                &bytes[UPTIME_OFFSET..CPU_OFFSET],
                || Duration64::from_bytes(&bytes[UPTIME_OFFSET..CPU_OFFSET]),
            )?,
            cpu: MachineCpu::decode(flags, &bytes[CPU_OFFSET..MEMORY_OFFSET])?,
            memory: MachineMemory::decode(flags, &bytes[MEMORY_OFFSET..TASKS_OFFSET])?,
            tasks: MachineTasks::decode(flags, &bytes[TASKS_OFFSET..STORAGE_OFFSET])?,
            storage: present(
                flags & HAS_STORAGE != 0,
                &bytes[STORAGE_OFFSET..NETWORK_OFFSET],
                || MachineStorage::decode(&bytes[STORAGE_OFFSET..NETWORK_OFFSET]),
            )?,
            network: present(
                flags & HAS_NETWORK != 0,
                &bytes[NETWORK_OFFSET..Self::WIRE_LEN],
                || MachineNetwork::decode(&bytes[NETWORK_OFFSET..Self::WIRE_LEN]),
            )?,
        })
    }
}

/// Whether every byte of `bytes` is zero.
fn all_zero(bytes: &[u8]) -> bool {
    bytes.iter().all(|&byte| byte == 0)
}

/// An optional field: `read` when `present`, and otherwise `bytes` must be the
/// zeroes an absent field is written as.
fn present<T>(
    present: bool,
    bytes: &[u8],
    read: impl FnOnce() -> Result<T, Errno>,
) -> Result<Option<T>, Errno> {
    if present {
        return read().map(Some);
    }
    if all_zero(bytes) {
        Ok(None)
    } else {
        Err(Errno::BadMagic)
    }
}

/// `items` as the fixed slots a list holds them in, its prefix filled.
fn slots_of<T: Copy, const N: usize>(items: &[T]) -> Result<[Option<T>; N], Errno> {
    if items.len() > N {
        return Err(Errno::LengthOutOfRange);
    }
    let mut slots = [None; N];
    for (slot, item) in slots.iter_mut().zip(items) {
        *slot = Some(*item);
    }
    Ok(slots)
}

/// How many slots of `slots` are filled, which must be a prefix of them: a
/// list with a gap in it is not one.
fn named_prefix<T>(slots: &[Option<T>]) -> Result<usize, Errno> {
    let named = slots.iter().take_while(|slot| slot.is_some()).count();
    if slots[named..].iter().any(Option::is_some) {
        return Err(Errno::OutOfRange);
    }
    Ok(named)
}

#[cfg(test)]
mod tests;
