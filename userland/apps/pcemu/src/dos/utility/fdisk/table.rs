//! The partition tables `FDISK` edits, held as DOS 4.0's holds them: each
//! fixed disk's master boot record with its four entries, and the logical
//! drives its extended partition's chain of extended boot records holds,
//! every partition whole cylinders. Nothing here touches a disk: the tables
//! are read in from sectors, edited, and turned back into the sectors to
//! write.
//!
//! Where DOS 4.0's code is at odds with what it plainly means, the meaning
//! is followed: free space at the start of the disk never overlaps the next
//! partition, sizes and shares are reckoned without sixteen-bit overflow,
//! a size typed as a share of the disk never reaches past the space it was
//! chosen from, and a chain whose every drive was deleted is written empty
//! rather than left to bring them back.

use crate::dos::formats::{is_dos, FAT12_MOST};
use alloc::vec::Vec;
use tairix_partition::mbr::{
    self, Entry, Geometry, MAX_PRIMARY_PARTITIONS, MBR_SECTOR_LEN, PARTITION_ENTRY_LEN,
    PARTITION_TABLE_OFFSET,
};

/// The logical drives an extended partition holds at most.
pub const MAX_LOGICAL: usize = 23;
/// The letters fixed disks' drives are given, `C` to `Z`.
pub const MAX_LETTERS: usize = 24;
/// The fixed disks `FDISK` sets up.
pub const MAX_DISKS: u8 = 2;
/// The bytes of a megabyte, as `FDISK` counts them.
const MEGABYTE: u64 = 1_048_576;
/// The bytes of a sector.
const SECTOR: u64 = 512;
/// The size a request of 32 megabytes stands for: the largest partition of
/// under 65,536 sectors, which DOS 3's FAT reached.
const SMALL_DOS: u32 = 65_535;
/// The type a partition `FDISK` makes is given; `FORMAT` sets the one its
/// volume needs.
pub const NEW_TYPE: u8 = mbr::PART_TYPE_FAT16_CHS;

/// A fixed disk's shape, as INT 13h function `08h` reports it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Shape {
    pub cylinders: u16,
    pub heads: u8,
    pub sectors: u8,
}

impl Shape {
    /// The shape function `08h` returned in CX and DH.
    #[must_use]
    pub fn reported(cx: u16, dh: u8) -> Self {
        let [cl, ch] = cx.to_le_bytes();
        Self {
            cylinders: ((u16::from(cl & 0xC0) << 2) | u16::from(ch)) + 1,
            heads: dh.wrapping_add(1),
            sectors: cl & 0x3F,
        }
    }

    /// Whether a disk of this shape can be addressed at all.
    #[must_use]
    pub fn usable(self) -> bool {
        self.cylinders > 0 && self.geometry().is_some()
    }

    /// The sectors of one cylinder.
    #[must_use]
    pub fn per_cylinder(self) -> u32 {
        u32::from(self.heads) * u32::from(self.sectors)
    }

    /// The INT 13h geometry its partitions' CHS addresses count in.
    #[must_use]
    pub fn geometry(self) -> Option<Geometry> {
        Geometry::new(self.heads, self.sectors)
    }

    /// The first sector of cylinder `cylinder`.
    #[must_use]
    pub fn sector_of(self, cylinder: u16) -> u32 {
        u32::from(cylinder) * self.per_cylinder()
    }

    /// The cylinder sector `lba` lies in.
    #[must_use]
    pub fn cylinder_of(self, lba: u32) -> u16 {
        u16::try_from(lba / self.per_cylinder().max(1)).unwrap_or(u16::MAX)
    }

    /// `cylinders` cylinders in megabytes, a half rounded up.
    #[must_use]
    pub fn mbytes(self, cylinders: u16) -> u16 {
        let bytes = u64::from(cylinders) * u64::from(self.per_cylinder()) * SECTOR;
        let rounded = bytes / MEGABYTE + u64::from(bytes % MEGABYTE >= MEGABYTE / 2);
        u16::try_from(rounded).unwrap_or(u16::MAX)
    }

    /// The cylinders `mbytes` megabytes take, a part one rounded up; 32
    /// megabytes is the largest size under 65,536 sectors.
    #[must_use]
    pub fn cylinders_for(self, mbytes: u16) -> u16 {
        let (per_track, heads) = (u64::from(self.sectors.max(1)), u64::from(self.heads.max(1)));
        let cylinders = if mbytes == 32 {
            u64::from(SMALL_DOS - u32::from(self.sectors)) / (per_track * heads)
        } else {
            let sectors = (u64::from(mbytes) * MEGABYTE).div_ceil(SECTOR);
            sectors.div_ceil(per_track).div_ceil(heads)
        };
        u16::try_from(cylinders).unwrap_or(u16::MAX)
    }
}

/// `part` of `whole` as a percentage, as DOS 4.0 rounds it: up once the
/// remainder reaches half the whole, at most 100.
#[must_use]
pub fn percent(part: u16, whole: u16) -> u16 {
    if whole == 0 {
        return 0;
    }
    let (part, whole) = (u32::from(part) * 100, u32::from(whole));
    let rounded = part / whole + u32::from(part % whole >= whole / 2);
    u16::try_from(rounded.min(100)).unwrap_or(100)
}

/// `share` percent of `whole` cylinders, a part one rounded up.
#[must_use]
pub fn cylinders_of_percent(share: u16, whole: u16) -> u16 {
    let cylinders = (u32::from(share) * u32::from(whole)).div_ceil(100);
    u16::try_from(cylinders).unwrap_or(u16::MAX)
}

/// A partition's size as it was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Size {
    Mbytes(u16),
    /// A share of `whole` cylinders: the disk's, or for a logical drive its
    /// extended partition's.
    Share {
        share: u16,
        whole: u16,
    },
}

/// The cylinders from `run`'s start a partition of `size` takes: the whole
/// run where the size is at least the run's own as it was offered, else the
/// size's own cylinders, never past the run. None for a size of no
/// cylinders.
#[must_use]
pub fn sized(shape: Shape, run: Extent, size: Size) -> Option<Extent> {
    let whole_run = run.cylinders();
    let cylinders = match size {
        Size::Mbytes(mbytes) if mbytes >= shape.mbytes(whole_run) => whole_run,
        Size::Mbytes(mbytes) => shape.cylinders_for(mbytes),
        Size::Share { share, whole } if share >= percent(whole_run, whole) => whole_run,
        Size::Share { share, whole } => cylinders_of_percent(share, whole),
    };
    Extent::from(run.start, cylinders.min(whole_run))
}

/// What a partition's type byte makes of it, as `FDISK` names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// A DOS volume.
    Dos,
    /// An extended partition, its logical drives within it.
    Extended,
    Xenix,
    PcIx,
    /// A bad-track table.
    Table,
    /// Any other system's.
    Other,
}

impl Kind {
    /// The kind `type_byte` names.
    #[must_use]
    pub const fn of(type_byte: u8) -> Self {
        match type_byte {
            byte if is_dos(byte) => Self::Dos,
            byte if mbr::is_extended(byte) => Self::Extended,
            0x02 | 0x03 => Self::Xenix,
            0x75 => Self::PcIx,
            0xFF => Self::Table,
            _ => Self::Other,
        }
    }

    /// Whether a partition of this kind can be made the one the disk starts:
    /// any but an extended partition or a table.
    #[must_use]
    pub const fn startable(self) -> bool {
        !matches!(self, Self::Extended | Self::Table)
    }
}

/// Whole cylinders, `start` to `end` both included.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Extent {
    pub start: u16,
    pub end: u16,
}

impl Extent {
    /// The cylinders from `start`, `cylinders` of them.
    #[must_use]
    pub fn from(start: u16, cylinders: u16) -> Option<Self> {
        let end = start.checked_add(cylinders.checked_sub(1)?)?;
        Some(Self { start, end })
    }

    /// Its cylinders.
    #[must_use]
    pub const fn cylinders(self) -> u16 {
        self.end - self.start + 1
    }
}

/// A primary table entry, as its slot holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Primary {
    pub slot: usize,
    pub type_byte: u8,
    pub active: bool,
    /// The cylinders it covers, none for an entry with no sectors.
    pub extent: Option<Extent>,
}

impl Primary {
    /// What its type makes of it.
    #[must_use]
    pub const fn kind(&self) -> Kind {
        Kind::of(self.type_byte)
    }
}

/// A logical drive, as the extended boot record that holds it records it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Logical {
    /// The cylinder its extended boot record begins.
    pub record: u16,
    /// Its last cylinder.
    pub end: u16,
    /// Its entry as its record holds it, relative to the record.
    pub slot: [u8; PARTITION_ENTRY_LEN],
    /// Its volume's label, as DOS reports it, and its file system's name.
    pub label: [u8; LABEL_LEN],
    pub system: [u8; SYSTEM_LEN],
    /// Marked for deletion, not yet deleted.
    pub deleted: bool,
    /// Made since the disk was read: its first sector is cleared when the
    /// tables are written.
    pub fresh: bool,
}

pub use tairix_fat::name::LABEL_LEN;
/// The bytes of a file system's name.
pub const SYSTEM_LEN: usize = tairix_fat::boot::Kind::Fat12.type_name().len();
/// The file system a drive that was never formatted shows.
pub const UNFORMATTED: [u8; SYSTEM_LEN] = *b"UNKNOWN ";

impl Logical {
    /// Its type byte.
    #[must_use]
    pub const fn type_byte(&self) -> u8 {
        self.slot[4]
    }

    /// The cylinders it covers, its record's track among them.
    #[must_use]
    pub const fn extent(&self) -> Extent {
        Extent {
            start: self.record,
            end: self.end,
        }
    }
}

/// A fixed disk's tables.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Disk {
    pub shape: Shape,
    /// Its tables were read.
    pub readable: bool,
    /// The master boot record to write: its code, its entries and its
    /// signature.
    pub record: Record,
    /// The entries changed since the disk was read, and those made new,
    /// whose partitions' first sectors are cleared when the tables are
    /// written, a bit a slot.
    pub changed: u8,
    pub fresh: u8,
    /// A logical drive was made or deleted.
    pub chain_changed: bool,
    pub logical: Vec<Logical>,
}

/// A master boot record's sector.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Record(pub [u8; MBR_SECTOR_LEN]);

impl Default for Record {
    fn default() -> Self {
        Self([0; MBR_SECTOR_LEN])
    }
}

/// Where a fixed disk's tables lie in program memory, and its bytes there.
pub mod layout {
    pub const CYLINDERS: usize = 0;
    pub const HEADS: usize = 2;
    pub const SECTORS: usize = 3;
    pub const FLAGS: usize = 4;
    pub const CHANGED: usize = 5;
    pub const FRESH: usize = 6;
    pub const LOGICALS: usize = 7;
    pub const RECORD: usize = 8;
    pub const DRIVES: usize = RECORD + super::MBR_SECTOR_LEN;
    /// A logical drive's bytes: its record's cylinder and its last, its
    /// slot, its label, its system and its flags.
    pub const DRIVE: usize =
        4 + super::PARTITION_ENTRY_LEN + super::LABEL_LEN + super::SYSTEM_LEN + 1;
    pub const LEN: usize = DRIVES + super::MAX_LOGICAL * DRIVE;

    pub const READABLE: u8 = 0x01;
    pub const CHAIN_CHANGED: u8 = 0x02;
    pub const DELETED: u8 = 0x01;
    pub const FRESH_DRIVE: u8 = 0x02;
}

impl Disk {
    /// A disk of `shape` whose master boot record reads as `sector`, its
    /// logical drives still to be read; `base`, the record to start from,
    /// stands in for one without a signature.
    #[must_use]
    pub fn read(shape: Shape, sector: &[u8; MBR_SECTOR_LEN], base: &[u8; MBR_SECTOR_LEN]) -> Self {
        let signed = mbr::has_signature(sector);
        Self {
            shape,
            readable: true,
            record: Record(if signed { *sector } else { *base }),
            ..Self::default()
        }
    }

    /// The disk's bytes in program memory.
    #[must_use]
    pub fn store(&self) -> [u8; layout::LEN] {
        let mut out = [0u8; layout::LEN];
        out[layout::CYLINDERS..layout::CYLINDERS + 2]
            .copy_from_slice(&self.shape.cylinders.to_le_bytes());
        out[layout::HEADS] = self.shape.heads;
        out[layout::SECTORS] = self.shape.sectors;
        out[layout::FLAGS] = (u8::from(self.readable) * layout::READABLE)
            | (u8::from(self.chain_changed) * layout::CHAIN_CHANGED);
        out[layout::CHANGED] = self.changed;
        out[layout::FRESH] = self.fresh;
        out[layout::LOGICALS] = u8::try_from(self.logical.len().min(MAX_LOGICAL)).unwrap_or(0);
        out[layout::RECORD..layout::DRIVES].copy_from_slice(&self.record.0);
        for (drive, bytes) in self
            .logical
            .iter()
            .zip(out[layout::DRIVES..].as_chunks_mut::<{ layout::DRIVE }>().0)
        {
            bytes[..2].copy_from_slice(&drive.record.to_le_bytes());
            bytes[2..4].copy_from_slice(&drive.end.to_le_bytes());
            let (slot, rest) = bytes[4..].split_at_mut(PARTITION_ENTRY_LEN);
            slot.copy_from_slice(&drive.slot);
            let (label, rest) = rest.split_at_mut(LABEL_LEN);
            label.copy_from_slice(&drive.label);
            let (system, rest) = rest.split_at_mut(SYSTEM_LEN);
            system.copy_from_slice(&drive.system);
            rest[0] = (u8::from(drive.deleted) * layout::DELETED)
                | (u8::from(drive.fresh) * layout::FRESH_DRIVE);
        }
        out
    }

    /// The disk `bytes` stores.
    #[must_use]
    pub fn load(bytes: &[u8; layout::LEN]) -> Self {
        let word = |at: usize| u16::from_le_bytes([bytes[at], bytes[at + 1]]);
        let mut record = [0u8; MBR_SECTOR_LEN];
        record.copy_from_slice(&bytes[layout::RECORD..layout::DRIVES]);
        let count = usize::from(bytes[layout::LOGICALS]).min(MAX_LOGICAL);
        let logical = bytes[layout::DRIVES..]
            .as_chunks::<{ layout::DRIVE }>()
            .0
            .iter()
            .take(count)
            .map(|drive| {
                let mut out = Logical {
                    record: u16::from_le_bytes([drive[0], drive[1]]),
                    end: u16::from_le_bytes([drive[2], drive[3]]),
                    ..Logical::default()
                };
                let (slot, rest) = drive[4..].split_at(PARTITION_ENTRY_LEN);
                out.slot.copy_from_slice(slot);
                let (label, rest) = rest.split_at(LABEL_LEN);
                out.label.copy_from_slice(label);
                let (system, rest) = rest.split_at(SYSTEM_LEN);
                out.system.copy_from_slice(system);
                out.deleted = rest[0] & layout::DELETED != 0;
                out.fresh = rest[0] & layout::FRESH_DRIVE != 0;
                out
            })
            .collect();
        Self {
            shape: Shape {
                cylinders: word(layout::CYLINDERS),
                heads: bytes[layout::HEADS],
                sectors: bytes[layout::SECTORS],
            },
            readable: bytes[layout::FLAGS] & layout::READABLE != 0,
            record: Record(record),
            changed: bytes[layout::CHANGED],
            fresh: bytes[layout::FRESH],
            chain_changed: bytes[layout::FLAGS] & layout::CHAIN_CHANGED != 0,
            logical,
        }
    }

    fn slot_bytes(&self, slot: usize) -> &[u8] {
        let at = PARTITION_TABLE_OFFSET + slot * PARTITION_ENTRY_LEN;
        &self.record.0[at..at + PARTITION_ENTRY_LEN]
    }

    /// The entry in `slot`, if one is there.
    #[must_use]
    pub fn primary(&self, slot: usize) -> Option<Primary> {
        if slot >= MAX_PRIMARY_PARTITIONS {
            return None;
        }
        let raw = self.slot_bytes(slot);
        let word = |at: usize| u32::from_le_bytes([raw[at], raw[at + 1], raw[at + 2], raw[at + 3]]);
        let (start, sectors) = (word(8), word(12));
        let extent = sectors.checked_sub(1).and_then(|last| {
            let end = self.shape.cylinder_of(start.checked_add(last)?);
            let start = self.shape.cylinder_of(start);
            (start <= end).then_some(Extent { start, end })
        });
        (raw[4] != mbr::PART_TYPE_UNUSED).then_some(Primary {
            slot,
            type_byte: raw[4],
            active: raw[0] == mbr::ACTIVE,
            extent,
        })
    }

    /// The entries there are, in slot order.
    pub fn primaries(&self) -> impl Iterator<Item = Primary> + '_ {
        (0..MAX_PRIMARY_PARTITIONS).filter_map(|slot| self.primary(slot))
    }

    /// The entries there are, in the order of the cylinders they end on, as
    /// `FDISK` lists and numbers them; an empty one first.
    #[must_use]
    pub fn listed(&self) -> Vec<Primary> {
        let mut listed: Vec<Primary> = self.primaries().collect();
        listed.sort_by_key(|primary| primary.extent.map_or(0, |extent| u32::from(extent.end) + 1));
        listed
    }

    /// The first entry of `kind`.
    #[must_use]
    pub fn first(&self, kind: Kind) -> Option<Primary> {
        self.primaries().find(|primary| primary.kind() == kind)
    }

    /// The extended partition's cylinders, if it has one.
    #[must_use]
    pub fn extended(&self) -> Option<Extent> {
        self.first(Kind::Extended)
            .and_then(|primary| primary.extent)
    }

    /// The active entry.
    #[must_use]
    pub fn active(&self) -> Option<Primary> {
        self.primaries().find(|primary| primary.active)
    }

    /// The first slot no entry is in.
    #[must_use]
    pub fn free_slot(&self) -> Option<usize> {
        (0..MAX_PRIMARY_PARTITIONS).find(|&slot| self.primary(slot).is_none())
    }

    /// The live logical drives, deleted ones aside, in the order they lie.
    pub fn drives(&self) -> impl Iterator<Item = &Logical> + '_ {
        self.logical.iter().filter(|drive| !drive.deleted)
    }

    /// The DOS logical drives, which take letters.
    #[must_use]
    pub fn dos_drives(&self) -> usize {
        self.drives()
            .filter(|drive| is_dos(drive.type_byte()))
            .count()
    }

    /// Whether it has a DOS primary partition, which takes a letter.
    #[must_use]
    pub fn has_dos(&self) -> bool {
        self.first(Kind::Dos).is_some()
    }

    /// The largest run of unused cylinders a primary partition, or with
    /// `extended` an extended one, may be made in, none of `bad` among
    /// them, the first on a tie; an extended partition never starts at
    /// cylinder 0, which holds the master boot record. None where no
    /// cylinder is free.
    #[must_use]
    pub fn free_space(&self, extended: bool, bad: &[Extent]) -> Option<Extent> {
        let lowest = u16::from(extended);
        let used: Vec<Extent> = self
            .primaries()
            .filter_map(|primary| primary.extent)
            .chain(bad.iter().copied())
            .collect();
        largest_gap(&used, lowest, self.shape.cylinders.checked_sub(1)?)
    }

    /// The largest run of the extended partition's cylinders no logical
    /// drive is in, none of `bad` among them, the first on a tie.
    #[must_use]
    pub fn logical_space(&self, bad: &[Extent]) -> Option<Extent> {
        let extended = self.extended()?;
        let used: Vec<Extent> = self
            .drives()
            .map(Logical::extent)
            .chain(bad.iter().copied())
            .collect();
        largest_gap(&used, extended.start, extended.end)
    }

    /// `slot_bytes` written into `slot`, marking it changed.
    fn put(&mut self, slot: usize, slot_bytes: [u8; PARTITION_ENTRY_LEN]) {
        let at = PARTITION_TABLE_OFFSET + slot * PARTITION_ENTRY_LEN;
        self.record.0[at..at + PARTITION_ENTRY_LEN].copy_from_slice(&slot_bytes);
        self.changed |= 1 << slot;
    }

    /// A primary partition of `extent`'s cylinders made in the first free
    /// slot, the active one with `active`: from the second track where it
    /// starts on cylinder 0, whose first holds the master boot record.
    /// Returns its slot.
    pub fn make_primary(&mut self, extent: Extent, active: bool) -> Option<usize> {
        let slot = self.place(extent, NEW_TYPE)?;
        if active {
            self.set_active(slot);
        }
        self.fresh |= 1 << slot;
        Some(slot)
    }

    /// An extended partition of `extent`'s cylinders made in the first free
    /// slot, its chain written empty so no drives of an older one there
    /// come back. Returns its slot.
    pub fn make_extended(&mut self, extent: Extent) -> Option<usize> {
        if extent.start == 0 {
            return None;
        }
        let slot = self.place(extent, mbr::PART_TYPE_EXTENDED_CHS)?;
        self.logical.clear();
        self.chain_changed = true;
        Some(slot)
    }

    fn place(&mut self, extent: Extent, type_byte: u8) -> Option<usize> {
        let slot = self.free_slot()?;
        let first = self.shape.sector_of(extent.start)
            + if extent.start == 0 {
                u32::from(self.shape.sectors)
            } else {
                0
            };
        let end = self.shape.sector_of(extent.end.checked_add(1)?);
        let entry = Entry {
            type_byte,
            active: false,
            start_lba: first,
            sectors: end.checked_sub(first).filter(|&sectors| sectors > 0)?,
        };
        let bytes = mbr::encode_slot(&entry, self.shape.geometry()).ok()?;
        self.put(slot, bytes);
        Some(slot)
    }

    /// `slot` the active entry, every other one not.
    pub fn set_active(&mut self, slot: usize) {
        for other in 0..MAX_PRIMARY_PARTITIONS {
            let at = PARTITION_TABLE_OFFSET + other * PARTITION_ENTRY_LEN;
            let flag = if other == slot { mbr::ACTIVE } else { 0 };
            if self.record.0[at] != flag {
                self.record.0[at] = flag;
                self.changed |= 1 << other;
            }
        }
    }

    /// The entry in `slot` emptied.
    pub fn clear(&mut self, slot: usize) {
        self.put(slot, [0; PARTITION_ENTRY_LEN]);
        self.fresh &= !(1 << slot);
    }

    /// The extended partitions emptied, and the logical drives in them gone.
    pub fn clear_extended(&mut self) {
        for slot in 0..MAX_PRIMARY_PARTITIONS {
            if self
                .primary(slot)
                .is_some_and(|primary| primary.kind() == Kind::Extended)
            {
                self.clear(slot);
            }
        }
        self.logical.clear();
        self.chain_changed = false;
    }

    /// A logical drive of `extent`'s cylinders made, its extended boot
    /// record on the first track and the drive after it. Returns its place
    /// in the drives as they lie.
    pub fn make_logical(&mut self, extent: Extent) -> Option<usize> {
        if self.logical.len() >= MAX_LOGICAL {
            return None;
        }
        let track = u32::from(self.shape.sectors);
        let sectors = u32::from(extent.cylinders())
            .checked_mul(self.shape.per_cylinder())?
            .checked_sub(track)
            .filter(|&sectors| sectors > 0)?;
        let entry = Entry {
            type_byte: NEW_TYPE,
            active: false,
            start_lba: track,
            sectors,
        };
        let drive = Logical {
            record: extent.start,
            end: extent.end,
            slot: self.relative_slot(&entry, extent.start)?,
            system: UNFORMATTED,
            fresh: true,
            ..Logical::default()
        };
        let at = self
            .logical
            .iter()
            .position(|other| other.record > extent.start)
            .unwrap_or(self.logical.len());
        self.logical.insert(at, drive);
        self.chain_changed = true;
        Some(at)
    }

    /// `entry`, whose sectors count from the record at cylinder `record`,
    /// as that record's slot holds it: its offsets relative, its CHS
    /// addresses the disk's own.
    fn relative_slot(&self, entry: &Entry, record: u16) -> Option<[u8; PARTITION_ENTRY_LEN]> {
        let base = self.shape.sector_of(record);
        let absolute = Entry {
            start_lba: base.checked_add(entry.start_lba)?,
            ..*entry
        };
        let mut slot = mbr::encode_slot(&absolute, self.shape.geometry()).ok()?;
        slot[8..12].copy_from_slice(&entry.start_lba.to_le_bytes());
        Some(slot)
    }

    /// The extended boot record `sector`, read from sector `at`, taken in:
    /// its logical drive, if it has one, added as the record holds it.
    /// Returns where the chain's next record lies, if there is one past this
    /// one and room for its drive; a chain that turns back on itself ends.
    pub fn take_record(&mut self, at: u32, sector: &[u8; MBR_SECTOR_LEN]) -> Option<u32> {
        let extended = self.extended()?;
        let record = mbr::ebr(sector).ok()?;
        if let Some(entry) = record.logical {
            let last = at
                .checked_add(entry.start_lba)?
                .checked_add(entry.sectors.checked_sub(1)?)?;
            let mut drive = Logical {
                record: self.shape.cylinder_of(at),
                end: self.shape.cylinder_of(last),
                ..Logical::default()
            };
            drive.slot.copy_from_slice(
                &sector[PARTITION_TABLE_OFFSET..PARTITION_TABLE_OFFSET + PARTITION_ENTRY_LEN],
            );
            self.logical.push(drive);
        }
        let next = self
            .shape
            .sector_of(extended.start)
            .checked_add(record.next?)?;
        (next > at && self.logical.len() < MAX_LOGICAL).then_some(next)
    }

    /// The logical drives marked deleted taken out.
    pub fn sweep(&mut self) {
        let before = self.logical.len();
        self.logical.retain(|drive| !drive.deleted);
        self.chain_changed |= self.logical.len() != before;
    }

    /// The sectors to write and where: the master boot record if an entry
    /// changed; every extended boot record if a logical drive was made or
    /// deleted, an empty one heading a chain whose drives were all deleted;
    /// and a cleared first sector for each partition made, so no volume
    /// that lay there before is found in it.
    #[must_use]
    pub fn writes(&self) -> Vec<(u32, [u8; MBR_SECTOR_LEN])> {
        let mut out = Vec::new();
        if self.changed != 0 {
            out.push((0, self.record.0));
        }
        if self.chain_changed {
            if let Some(extended) = self.extended() {
                out.extend(self.chain(extended));
            }
        }
        for primary in self
            .primaries()
            .filter(|primary| self.fresh & (1 << primary.slot) != 0)
        {
            let raw = self.slot_bytes(primary.slot);
            let start = u32::from_le_bytes([raw[8], raw[9], raw[10], raw[11]]);
            out.push((start, [0; MBR_SECTOR_LEN]));
        }
        for drive in self.drives().filter(|drive| drive.fresh) {
            out.push((
                self.shape.sector_of(drive.record) + u32::from(self.shape.sectors),
                [0; MBR_SECTOR_LEN],
            ));
        }
        out
    }

    /// The extended boot records of the extended partition `extended`, in
    /// the order the drives lie: each its drive's entry and a link to the
    /// next, the link's offset from the partition's start and its extent
    /// the next record's whole cylinders.
    fn chain(&self, extended: Extent) -> Vec<(u32, [u8; MBR_SECTOR_LEN])> {
        let drives: Vec<&Logical> = self.drives().collect();
        let base = self.shape.sector_of(extended.start);
        let link = |drive: &Logical| {
            let start = self.shape.sector_of(drive.record);
            let entry = Entry {
                type_byte: mbr::PART_TYPE_EXTENDED_CHS,
                active: false,
                start_lba: start,
                sectors: u32::from(drive.extent().cylinders()) * self.shape.per_cylinder(),
            };
            let mut slot = mbr::encode_slot(&entry, self.shape.geometry()).ok()?;
            slot[8..12].copy_from_slice(&(start - base).to_le_bytes());
            Some(slot)
        };
        let mut out = Vec::new();
        match drives.first() {
            None => out.push((base, mbr::encode_ebr(None, None))),
            Some(first) if first.record != extended.start => {
                out.push((base, mbr::encode_ebr(None, link(first).as_ref())));
            }
            Some(_) => {}
        }
        for (i, drive) in drives.iter().enumerate() {
            let next = drives.get(i + 1).and_then(|next| link(next));
            out.push((
                self.shape.sector_of(drive.record),
                mbr::encode_ebr(Some(&drive.slot), next.as_ref()),
            ));
        }
        out
    }
}

/// The largest run of cylinders from `lowest` to `highest` none of `used`
/// covers, the first on a tie.
fn largest_gap(used: &[Extent], lowest: u16, highest: u16) -> Option<Extent> {
    let mut used: Vec<Extent> = used.to_vec();
    used.sort_by_key(|extent| extent.start);
    let mut best: Option<Extent> = None;
    let mut from = lowest;
    let mut consider = |start: u16, end: u16| {
        if start <= end && best.is_none_or(|best| end - start + 1 > best.cylinders()) {
            best = Some(Extent { start, end });
        }
    };
    for extent in &used {
        if extent.start > from {
            consider(from, (extent.start - 1).min(highest));
        }
        from = from.max(extent.end.saturating_add(1));
    }
    if from <= highest {
        consider(from, highest);
    }
    best
}

/// The tracks from a free run's start that a volume's system area takes
/// at most, which must be readable for the run to hold a partition: the
/// boot record, two FATs for the run's sectors and the root, and, in a
/// primary partition on the first disk, room for the system's files.
/// DOS 4.0 sizes them so.
#[must_use]
pub fn system_tracks(shape: Shape, run: Extent, primary_on_first: bool) -> u32 {
    let spt = u32::from(shape.sectors.max(1));
    let skipped = if run.start == 0 { spt } else { 0 };
    let total = (u32::from(run.cylinders()) * shape.per_cylinder()).saturating_sub(skipped);
    let fat = if total > FAT12_MOST {
        total.saturating_sub(33) / 1026
    } else {
        total.saturating_sub(33) * 3 / 8198
    } + 1;
    let needed = 2 * fat + 33 + if primary_on_first { 250 } else { 0 };
    needed.div_ceil(spt)
}

/// The letter of the first drive each disk's logical drives are given, as
/// DOS gives them: every disk's DOS primary partition first, from `C`, then
/// each disk's DOS logical drives in turn.
#[must_use]
pub fn first_logical_letter(disks: &[Disk], disk: usize) -> u8 {
    let primaries = disks.iter().filter(|disk| disk.has_dos()).count();
    let before: usize = disks.iter().take(disk).map(Disk::dos_drives).sum();
    b'C'.saturating_add(u8::try_from(primaries + before).unwrap_or(u8::MAX))
}

/// The letter disk `disk`'s DOS primary partition is given.
#[must_use]
pub fn primary_letter(disks: &[Disk], disk: usize) -> u8 {
    let before = disks
        .iter()
        .take(disk)
        .filter(|disk| disk.has_dos())
        .count();
    b'C'.saturating_add(u8::try_from(before).unwrap_or(u8::MAX))
}

/// The letters every disk's DOS partitions take.
#[must_use]
pub fn letters_used(disks: &[Disk]) -> usize {
    disks
        .iter()
        .map(|disk| usize::from(disk.has_dos()) + disk.dos_drives())
        .sum()
}

#[cfg(test)]
#[path = "table_tests.rs"]
mod tests;
