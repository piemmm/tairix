//! A PC as the machine library keeps it: its hardware, its fixed disks, its
//! clock card's offset and its tube; what its state blob may hold, and the
//! line that says what it is.

use alloc::format;
use alloc::string::String;
use core::fmt::Write as _;

use tairix_abi::time::CivilTime;
use tairix_crt::Tube;
use tairix_machines::record::{self, Kept};
use tairix_machines::state::Bounds;
use tairix_sandbox::wire::{Reader, WireError, Writer};

use crate::board::{BoardConfig, SoundCard};
use crate::cpu::CpuModel;
use crate::dos::setup::sizes;
use crate::link::{config, most_kept};
use crate::storage::hdc::{DiskType, LowLevel};
use crate::video::{Adapter, LARGEST_FRAME};

/// The word a machine's fixed disks are kept under, drive `n` at index `n`.
const DISK: &str = "disk";

/// What a machine's state may hold: the largest frame a PC shows, and its
/// largest snapshot.
pub const STATE_BOUNDS: Bounds = Bounds {
    largest: LARGEST_FRAME,
    most_kept: most_kept(),
};

/// A fixed disk a machine owns; its image is the blob [`disk_blob`] names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoredDisk {
    /// The drive.
    pub kind: DiskType,
    /// Its low-level format, which the image does not record.
    pub format: LowLevel,
}

/// One machine.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Machine {
    /// Minted once, never reused.
    pub id: u32,
    /// What the user calls it.
    pub name: String,
    /// Its hardware.
    pub config: BoardConfig,
    /// Its fixed disks, which only a machine with a controller has.
    pub disks: [Option<StoredDisk>; 2],
    /// Seconds its clock card runs ahead of the host's clock; behind when
    /// negative.
    pub clock_offset: i64,
    /// The tube its monitor's picture is drawn through.
    pub display: Tube,
}

/// Why the library may not hold a machine.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Invalid {
    /// Fixed disks on a machine with no controller to drive them.
    NoController,
    /// A display effect stronger than full strength.
    Display,
}

impl Machine {
    /// The sectors of fixed disk `drive`'s image: what its drive holds as the
    /// controller records it.
    #[must_use]
    pub fn disk_sectors(&self, drive: usize) -> Option<u64> {
        let disk = self.disks.get(drive)?.as_ref()?;
        let recording = self.config.fixed?.recording;
        Some(u64::from(sizes(disk.kind, recording).0))
    }

    /// What the clock card reads as the machine starts at host time
    /// `host_secs`.
    #[must_use]
    pub fn clock_at(&self, host_secs: i64) -> CivilTime {
        CivilTime::from_unix_secs(host_secs.saturating_add(self.clock_offset))
    }
}

impl Kept for Machine {
    type Invalid = Invalid;
    const MAGIC: u32 = u32::from_le_bytes(*b"PCLB");
    const VERSION: u16 = 2;
    const MAX_MACHINES: usize = record::store_holds(Self::PARTS);
    /// Every machine of the longest name with every part fitted comes to a
    /// few kilobytes.
    const MAX_RECORD: usize = 16 * 1024;
    const PARTS: &'static [(&'static str, usize)] = &[(DISK, 2)];

    fn id(&self) -> u32 {
        self.id
    }

    fn name(&self) -> &str {
        &self.name
    }

    fn put(&self, w: &mut Writer) {
        config::put_config(w, &self.config);
        for disk in &self.disks {
            match disk {
                Some(disk) => {
                    w.u8(1);
                    config::put_fixed(w, &disk.kind, &disk.format);
                }
                None => w.u8(0),
            }
        }
        w.u64(u64::from_le_bytes(self.clock_offset.to_le_bytes()));
        record::put_tube(w, self.display);
    }

    fn get(id: u32, name: String, r: &mut Reader<'_>) -> Result<Option<Self>, WireError> {
        let config = config::config(r)?;
        let mut disks = [None, None];
        for disk in &mut disks {
            *disk = match r.u8()? {
                0 => None,
                1 => {
                    let (kind, format) = config::fixed(r)?;
                    Some(StoredDisk { kind, format })
                }
                _ => return Ok(None),
            };
        }
        Ok(Some(Self {
            id,
            name,
            config,
            disks,
            clock_offset: i64::from_le_bytes(r.u64()?.to_le_bytes()),
            display: record::get_tube(r)?,
        }))
    }

    fn invalid(&self) -> Option<Invalid> {
        if self.config.fixed.is_none() && self.disks.iter().any(Option::is_some) {
            return Some(Invalid::NoController);
        }
        (!record::tube_within(self.display)).then_some(Invalid::Display)
    }

    fn holds(&self, word: &str, index: usize) -> bool {
        word == DISK && self.disks.get(index).is_some_and(Option::is_some)
    }
}

/// The blob machine `id`'s fixed disk `drive` is kept in.
#[must_use]
pub fn disk_blob(id: u32, drive: usize) -> String {
    record::part_blob(DISK, id, drive)
}

/// The offset that has a clock card read `guest` at host time `host_secs`;
/// none for a reading no instant has.
#[must_use]
pub fn clock_offset(guest: &CivilTime, host_secs: i64) -> Option<i64> {
    guest.to_unix_secs()?.checked_sub(host_secs)
}

/// What a processor is called.
const fn cpu_name(cpu: CpuModel) -> &'static str {
    match cpu {
        CpuModel::I8088 => "8088",
        CpuModel::I8086 => "8086",
        CpuModel::V20 => "NEC V20",
        CpuModel::V30 => "NEC V30",
        CpuModel::I80188 => "80188",
        CpuModel::I80186 => "80186",
    }
}

/// What a display adapter is called.
const fn adapter_name(adapter: Adapter) -> &'static str {
    match adapter {
        Adapter::Mda(_) => "MDA",
        Adapter::Hercules(_) => "Hercules",
        Adapter::Cga(_) => "CGA",
        Adapter::Ega(_) => "EGA",
        Adapter::Vga(_) => "VGA",
    }
}

/// The line the library's status band states of `machine`.
#[must_use]
pub fn describe(machine: &Machine) -> String {
    describe_config(&machine.config, machine.disks.iter().flatten().count())
}

/// What a machine of `config` with `disks` hard disks is, in a line.
#[must_use]
pub fn describe_config(config: &BoardConfig, disks: usize) -> String {
    // Hundredths of a megahertz, rounded: ten million millihertz each.
    let hundredths = (config.clock.rate().millihertz() + 5_000_000) / 10_000_000;
    let mut line = format!(
        "{} at {}.{:02} MHz",
        cpu_name(config.cpu),
        hundredths / 100,
        hundredths % 100,
    );
    if config.coprocessor {
        line.push_str(" + 8087");
    }
    let _ = write!(line, " \u{b7} {} KB", config.memory_kib);
    if let Some(ems) = config.ems {
        let kib = u32::from(ems.pages) * 16;
        let _ = write!(line, " + {kib} KB expanded");
    }
    for adapter in config.video.iter().flatten() {
        line.push_str(" \u{b7} ");
        line.push_str(adapter_name(*adapter));
    }
    if disks > 0 {
        let plural = if disks == 1 { "" } else { "s" };
        let _ = write!(line, " \u{b7} {disks} hard disk{plural}");
    }
    match config.sound.card {
        Some(SoundCard::AdLib) => line.push_str(" \u{b7} AdLib"),
        Some(SoundCard::Blaster { .. }) => line.push_str(" \u{b7} Sound Blaster"),
        None => {}
    }
    line
}

#[cfg(test)]
#[path = "library_tests.rs"]
mod tests;
