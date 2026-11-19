//! A disc's objects and directories over its map: object bytes by indirect
//! disc address, allocation and the share rules of fragments, and the
//! directories, fixed or big, read, searched and changed in `FileCore`'s
//! order.
//!
//! A [`Volume`] holds the disc and what describes it — the map engine of a
//! new map, or where an old map's root lies — and the one big directory it
//! last validated whole, so the reads of a listing do not check it again.

use alloc::vec::Vec;
use core::cmp::Ordering;

use tairix_collections::OrderedSet;
use tairix_fsmeta::preset::acorn;

use crate::bigdir::{self, BigDir, BigHeader, DirStore};
use crate::defect::{Defects, FIRST_END};
use crate::dir::{self, FixedDir, FixedFormat, NameEnd, Object};
use crate::newmap::{MappedOut, NewMap};
use crate::oldmap::OldMap;
use crate::record::{
    BOOT_BLOCK_OFFSET, BOOT_BLOCK_SIZE, FRAG_BAD, NAME_LEN, OLD_SECTOR_SIZE, OLD_SECTOR_SIZE_U32,
};
use crate::{Disc, Error, Fault, Result, Structure};

/// A disc's objects and directories.
pub struct Volume<D: Disc> {
    disc: D,
    backing: Backing,
    validated: Option<(u32, BigHeader)>,
    shapes: u32,
    changes: u64,
    stranded: bool,
    moved_on: bool,
}

/// What a volume holds between the operations made on it, apart from the
/// disc they reach: kept by an owner that reaches its disc a call at a time,
/// and attached to the disc again for the next.
pub struct Mount {
    backing: Backing,
    validated: Option<(u32, BigHeader)>,
    shapes: u32,
    changes: u64,
    stranded: bool,
    moved_on: bool,
}

/// What holds a byte of a disc.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Holder {
    /// Free space, or nothing: past the disc.
    Free,
    /// Space mapped out as defective.
    Defect,
    /// The new-map fragment of this id.
    Fragment(u32),
    /// This old-map sector, which the map does not hold free.
    Sector(u32),
}

impl Holder {
    /// Whether `object` lies in what this holds: its fragment, or the run of
    /// sectors an old map gives it.
    #[must_use]
    pub fn holds(self, object: &Object) -> bool {
        match self {
            Self::Free | Self::Defect => false,
            Self::Fragment(id) => object.indaddr >> 8 == id,
            Self::Sector(sector) => sector
                .checked_sub(object.indaddr)
                .is_some_and(|into| into < object.size.div_ceil(OLD_SECTOR_SIZE_U32)),
        }
    }
}

/// One defect a disc records.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Defect {
    /// The defective sector's byte address.
    pub address: u64,
    /// Where the disc records it.
    pub recorded: Recorded,
}

/// Where a disc records a defect.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum Recorded {
    /// The boot block's first list, which counts in bytes.
    First,
    /// A big disc's second list, which counts in sectors.
    Second,
    /// The map alone: a disc with no boot block records its defects there.
    Map,
}

/// The defect lists `disc`'s boot block opens with, a second list read where
/// the disc is `big`.
fn boot_defects<D: Disc>(disc: &mut D, big: bool) -> Result<Defects, D> {
    let mut block = [0u8; BOOT_BLOCK_SIZE];
    disc.read(BOOT_BLOCK_OFFSET, &mut block)
        .map_err(Error::Disc)?;
    Ok(Defects::read(&block, big)?)
}

/// Free and total space, in the units its map counts.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Space {
    /// The bytes one unit is.
    pub unit: u32,
    /// The units the disc holds.
    pub total: u64,
    /// The units free.
    pub free: u64,
}

impl<D: Disc> Volume<D> {
    /// The volume on `disc`, laid out as `backing` describes, its root
    /// directory validated.
    ///
    /// # Errors
    ///
    /// The root broken, or the disc's failure.
    pub fn open(disc: D, backing: Backing) -> Result<Self, D> {
        let mut volume = Self {
            disc,
            backing,
            validated: None,
            shapes: 0,
            changes: 0,
            stranded: false,
            moved_on: false,
        };
        let root = volume.root_addr();
        volume.load_dir(root)?;
        Ok(volume)
    }

    /// The volume `mount` holds, on `disc`, the disc it was opened on.
    pub fn attach(disc: D, mount: Mount) -> Self {
        let Mount {
            backing,
            validated,
            shapes,
            changes,
            stranded,
            moved_on,
        } = mount;
        Self {
            disc,
            backing,
            validated,
            shapes,
            changes,
            stranded,
            moved_on,
        }
    }

    /// The disc, and what the volume holds until it is attached again.
    pub fn detach(self) -> (D, Mount) {
        let Self {
            disc,
            backing,
            validated,
            shapes,
            changes,
            stranded,
            moved_on,
        } = self;
        (
            disc,
            Mount {
                backing,
                validated,
                shapes,
                changes,
                stranded,
                moved_on,
            },
        )
    }

    /// The map is about to change: the disc id moved on first, once a mount,
    /// as `FileCore` marks a disc it changes.
    fn map_changing(&mut self) -> Result<(), D> {
        if self.moved_on {
            return Ok(());
        }
        match &mut self.backing {
            Backing::Old { .. } => {
                let mut map = OldMap::load(&mut self.disc)?;
                map.bump_disc_id();
                map.store(&mut self.disc)?;
            }
            Backing::New { map, .. } => map.bump_disc_id(&mut self.disc)?,
        }
        self.moved_on = true;
        Ok(())
    }

    /// The disc.
    pub const fn disc(&self) -> &D {
        &self.disc
    }

    /// The disc, to reach it directly.
    pub fn disc_mut(&mut self) -> &mut D {
        &mut self.disc
    }

    /// How the volume is laid out.
    pub const fn backing(&self) -> &Backing {
        &self.backing
    }

    /// Bumped by every insertion and removal, which shift a directory's
    /// entries.
    pub const fn shapes(&self) -> u32 {
        self.shapes
    }

    /// Bumped by every write to a directory and every release of space, which
    /// may change or move any entry a caller holds a copy of.
    pub const fn changes(&self) -> u64 {
        self.changes
    }

    /// Whether a directory read so far holds entries past its count that no
    /// check byte vouches for, which writing it back could revive.
    pub const fn stranded(&self) -> bool {
        self.stranded
    }

    /// Whether a new map's two copies both check yet disagree.
    pub fn copies_disagree(&self) -> bool {
        matches!(&self.backing, Backing::New { map, .. } if map.copies_disagree())
    }

    /// Where byte `offset` of the object at `indaddr` lies on the disc, and
    /// how many bytes run on from there before the object leaves the run.
    ///
    /// # Errors
    ///
    /// The map broken, or the disc's failure.
    pub fn locate(&mut self, indaddr: u32, offset: u64) -> Result<(u64, u64), D> {
        match &self.backing {
            Backing::Old { .. } => Ok((u64::from(indaddr) * OLD_SECTOR_SIZE + offset, u64::MAX)),
            Backing::New { map, .. } => map.locate(&mut self.disc, indaddr, offset),
        }
    }

    /// The disc's free and total space.
    ///
    /// # Errors
    ///
    /// The map broken, or the disc's failure.
    pub fn space(&mut self) -> Result<Space, D> {
        match &self.backing {
            Backing::Old { .. } => {
                let map = OldMap::load(&mut self.disc)?;
                Ok(Space {
                    unit: crate::record::OLD_SECTOR_SIZE_U32,
                    total: u64::from(map.disc_sectors()),
                    free: map.free_sectors(),
                })
            }
            Backing::New { map, .. } => {
                let bpmb = map.record.bytes_per_map_bit();
                Ok(Space {
                    unit: u32::try_from(bpmb).unwrap_or(u32::MAX),
                    total: map.record.disc_size / bpmb,
                    free: map.free_bytes(&mut self.disc)? / bpmb,
                })
            }
        }
    }

    /// The disc's name, as its map records it.
    ///
    /// # Errors
    ///
    /// The map broken, or the disc's failure.
    pub fn disc_name(&mut self) -> Result<[u8; NAME_LEN], D> {
        match &self.backing {
            Backing::Old { .. } => Ok(OldMap::load(&mut self.disc)?.name()),
            Backing::New { map, .. } => Ok(map.record.disc_name),
        }
    }

    /// Each free area of the disc in turn, as its byte offset and length.
    ///
    /// # Errors
    ///
    /// The map broken, or the disc's failure.
    pub fn free_areas(&mut self, visit: impl FnMut(u64, u64)) -> Result<(), D> {
        match &self.backing {
            Backing::Old { .. } => {
                OldMap::load(&mut self.disc)?.free_areas(visit);
                Ok(())
            }
            Backing::New { map, .. } => map.free_areas(&mut self.disc, visit),
        }
    }

    /// The `*Opt 4` boot option the disc records.
    ///
    /// # Errors
    ///
    /// The map broken, or the disc's failure.
    pub fn boot_option(&mut self) -> Result<u8, D> {
        match &self.backing {
            Backing::Old { .. } => Ok(OldMap::load(&mut self.disc)?.boot_option()),
            Backing::New { map, .. } => Ok(map.record.bootoption),
        }
    }

    /// The disc's boot option set.
    ///
    /// # Errors
    ///
    /// The map broken, or the disc's failure.
    pub fn set_boot_option(&mut self, option: u8) -> Result<(), D> {
        self.map_changing()?;
        match &mut self.backing {
            Backing::Old { .. } => {
                let mut map = OldMap::load(&mut self.disc)?;
                map.set_boot_option(option);
                map.store(&mut self.disc)
            }
            Backing::New { map, boot_block } => {
                map.set_boot_option(&mut self.disc, *boot_block, option)
            }
        }
    }

    /// Each defect the disc records, in its order, until `visit` declines
    /// another: its boot block's lists where it has one, else each sector of
    /// its map's defects. An old map records none.
    ///
    /// # Errors
    ///
    /// The defect lists or the map broken, or the disc's failure.
    pub fn defects(&mut self, mut visit: impl FnMut(Defect) -> bool) -> Result<(), D> {
        let Backing::New { map, boot_block } = &self.backing else {
            return Ok(());
        };
        let sector = map.record.sector_size();
        if *boot_block {
            let lists = boot_defects(&mut self.disc, map.record.big_flag)?;
            let first = lists.first().iter().map(|&address| Defect {
                address: u64::from(address),
                recorded: Recorded::First,
            });
            let second = lists.second().iter().map(|&number| Defect {
                address: u64::from(number) * sector,
                recorded: Recorded::Second,
            });
            for defect in first.chain(second) {
                if !visit(defect) {
                    break;
                }
            }
            return Ok(());
        }
        let bpmb = map.record.bytes_per_map_bit();
        map.defects(&mut self.disc, |block, blocks| {
            let end = (block + blocks) * bpmb;
            let mut address = block * bpmb;
            while address < end {
                if !visit(Defect {
                    address,
                    recorded: Recorded::Map,
                }) {
                    return false;
                }
                address += sector;
            }
            true
        })
    }

    /// The sector holding byte `address` recorded as a defect, as RISC OS 5
    /// records one: in the boot block's first list, where the disc has one,
    /// and mapped out of the free space it lies in, unless it lies past the
    /// disc, where it is only listed. Nothing changes for a sector recorded
    /// already, nor for a refusal.
    ///
    /// # Errors
    ///
    /// * [`Fault::BadDefect`] for a sector past the first list's reach, or on
    ///   an old map.
    /// * [`Fault::DefectInUse`] where an object or the disc's own
    ///   structures hold the sector.
    /// * [`Fault::TooManyDefects`] where the lists are full.
    /// * The defect lists or the map broken, or the disc's failure.
    pub fn add_defect(&mut self, address: u64) -> Result<(), D> {
        let Backing::New { map, boot_block } = &self.backing else {
            return Err(Fault::BadDefect.into());
        };
        let sector = map.record.sector_size();
        let start = address - address % sector;
        let listed = u32::try_from(start)
            .ok()
            .filter(|&start| start < FIRST_END)
            .ok_or(Fault::BadDefect)?;
        let mut lists = if *boot_block {
            let mut lists = boot_defects(&mut self.disc, map.record.big_flag)?;
            if !lists.insert(listed)? {
                return Ok(());
            }
            Some(lists)
        } else {
            None
        };
        if start < map.record.disc_size {
            let bpmb = map.record.bytes_per_map_bit();
            match map.map_out(&mut self.disc, start / bpmb, sector.div_ceil(bpmb))? {
                MappedOut::InUse => return Err(Fault::DefectInUse.into()),
                MappedOut::Already => {}
                MappedOut::Done => self.map_changing()?,
            }
        }
        if let Some(lists) = lists.as_mut() {
            self.map_changing()?;
            let mut block = [0u8; BOOT_BLOCK_SIZE];
            self.disc
                .read(BOOT_BLOCK_OFFSET, &mut block)
                .map_err(Error::Disc)?;
            lists.write(&mut block);
            self.disc
                .write(BOOT_BLOCK_OFFSET, &block)
                .map_err(Error::Disc)?;
        }
        Ok(())
    }

    /// The disc's sector size, in bytes.
    #[must_use]
    pub fn sector_size(&self) -> u64 {
        match &self.backing {
            Backing::Old { .. } => OLD_SECTOR_SIZE,
            Backing::New { map, .. } => map.record.sector_size(),
        }
    }

    /// What holds the byte `offset` into the disc.
    ///
    /// # Errors
    ///
    /// The map broken, or the disc's failure.
    pub fn holder_at(&mut self, offset: u64) -> Result<Holder, D> {
        match &self.backing {
            Backing::Old { .. } => {
                let map = OldMap::load(&mut self.disc)?;
                Ok(match u32::try_from(offset / OLD_SECTOR_SIZE) {
                    Ok(sector) if sector < map.disc_sectors() && !map.is_free(sector) => {
                        Holder::Sector(sector)
                    }
                    _ => Holder::Free,
                })
            }
            Backing::New { map, .. } => {
                let block = offset >> map.record.log2bpmb;
                Ok(match map.id_at(&mut self.disc, block)? {
                    None => Holder::Free,
                    Some(FRAG_BAD) => Holder::Defect,
                    Some(id) => Holder::Fragment(id),
                })
            }
        }
    }

    /// The disc renamed `name`, space padded.
    ///
    /// # Errors
    ///
    /// The map broken, or the disc's failure.
    pub fn set_disc_name(&mut self, name: [u8; NAME_LEN]) -> Result<(), D> {
        self.map_changing()?;
        match &mut self.backing {
            Backing::Old { .. } => {
                let mut map = OldMap::load(&mut self.disc)?;
                map.set_name(name);
                map.store(&mut self.disc)
            }
            Backing::New { map, boot_block } => {
                map.set_disc_name(&mut self.disc, *boot_block, name)
            }
        }
    }
}

/// A volume's map and where its root lies.
pub enum Backing {
    /// Old-map volume: contiguous objects addressed by start sector.
    Old {
        /// The fixed directory format the volume uses.
        format: FixedFormat,
        /// Start sector of the root directory.
        root_sector: u32,
    },
    /// New-map volume: fragmented objects addressed indirectly.
    New {
        /// The allocation-map engine (owns the validated disc record).
        map: NewMap,
        /// Whether the volume carries a boot block whose disc-record
        /// copy must be kept in step.
        boot_block: bool,
    },
}

impl<D: Disc> Volume<D> {
    /// The root directory's address (old map: start sector).
    pub fn root_addr(&self) -> u32 {
        match &self.backing {
            Backing::Old { root_sector, .. } => *root_sector,
            Backing::New { map, .. } => map.record.root,
        }
    }

    /// The root directory's size, which no entry records: the volume's
    /// fixed directory size, or a big root's in the disc record.
    pub fn root_size(&self) -> u32 {
        match &self.backing {
            Backing::Old { format, .. } => format.bytes(),
            Backing::New { map, .. } if map.record.format_version != 0 => map.record.root_size,
            Backing::New { .. } => dir::NEW_DIR_SIZE_U32,
        }
    }

    /// Refuses `indaddr` as an object's address where it lies in the disc's
    /// own structures, before anything is changed: an old map's map and root
    /// sectors, among which only the root starts, or what a new map
    /// refuses ([`NewMap::may_address_object`]). Only a broken directory
    /// names one.
    fn clear_of_structures(&self, indaddr: u32) -> core::result::Result<(), Fault> {
        let clear = match &self.backing {
            Backing::Old {
                format,
                root_sector,
            } => {
                let root_end =
                    root_sector.saturating_add(format.bytes().div_ceil(OLD_SECTOR_SIZE_U32));
                indaddr == *root_sector || indaddr >= root_end
            }
            Backing::New { .. } => NewMap::may_address_object(indaddr),
        };
        if clear {
            Ok(())
        } else {
            Err(Fault::Broken(Structure::Directory))
        }
    }

    /// Read exactly `buf.len()` bytes at `offset` within the object at
    /// `indaddr` (which must be allocated that far).
    pub fn object_read(&mut self, indaddr: u32, offset: u64, buf: &mut [u8]) -> Result<(), D> {
        if buf.is_empty() {
            return Ok(());
        }
        self.clear_of_structures(indaddr)?;
        match &self.backing {
            Backing::Old { .. } => {
                let at = u64::from(indaddr) * OLD_SECTOR_SIZE + offset;
                self.disc.read(at, buf).map_err(Error::Disc)
            }
            Backing::New { map, .. } => {
                let mut done = 0usize;
                while done < buf.len() {
                    let (at, run) = map.locate(&mut self.disc, indaddr, offset + done as u64)?;
                    let take = usize::try_from(run.min((buf.len() - done) as u64))
                        .map_err(|_| Fault::Broken(Structure::Object))?;
                    if take == 0 {
                        return Err(Fault::Broken(Structure::Directory).into());
                    }
                    self.disc
                        .read(at, &mut buf[done..done + take])
                        .map_err(Error::Disc)?;
                    done += take;
                }
                Ok(())
            }
        }
    }

    /// Write `data` at `offset` within the object at `indaddr` (which
    /// must be allocated that far).
    pub fn object_write(&mut self, indaddr: u32, offset: u64, data: &[u8]) -> Result<(), D> {
        if data.is_empty() {
            return Ok(());
        }
        self.clear_of_structures(indaddr)?;
        match &self.backing {
            Backing::Old { .. } => {
                let at = u64::from(indaddr) * OLD_SECTOR_SIZE + offset;
                self.disc.write(at, data).map_err(Error::Disc)
            }
            Backing::New { map, .. } => {
                let mut done = 0usize;
                while done < data.len() {
                    let (at, run) = map.locate(&mut self.disc, indaddr, offset + done as u64)?;
                    let take = usize::try_from(run.min((data.len() - done) as u64))
                        .map_err(|_| Fault::Broken(Structure::Object))?;
                    if take == 0 {
                        return Err(Fault::Broken(Structure::Directory).into());
                    }
                    self.disc
                        .write(at, &data[done..done + take])
                        .map_err(Error::Disc)?;
                    done += take;
                }
                Ok(())
            }
        }
    }
}

impl<D: Disc> Volume<D> {
    /// Whether the volume uses big (E+/F+) directories.
    pub fn is_big_dir(&self) -> bool {
        matches!(&self.backing, Backing::New { map, .. } if map.record.format_version != 0)
    }

    /// The fixed directory format of a non-big-directory volume.
    pub fn fixed_format(&self) -> FixedFormat {
        match &self.backing {
            Backing::Old { format, .. } => *format,
            Backing::New { .. } => FixedFormat::New,
        }
    }

    /// Load and validate the directory object at `indaddr`.
    ///
    /// A fixed directory's size is dictated by the volume's format and a big
    /// directory's by its own validated header, and the allocation map bounds
    /// every access. A big directory validated whole since its last write is
    /// taken as it stands.
    pub fn load_dir(&mut self, indaddr: u32) -> Result<Directory, D> {
        if self.is_big_dir() {
            if let Some((held, header)) = self.validated {
                if held == indaddr {
                    return Ok(Directory::Big(BigDir { header }));
                }
            }
            let mut store = ObjectStore {
                volume: self,
                indaddr,
                size: u32::MAX,
            };
            let dir = BigDir::load(&mut store, u32::MAX)?;
            self.validated = Some((indaddr, dir.header));
            Ok(Directory::Big(dir))
        } else {
            let format = self.fixed_format();
            let mut data = [0u8; dir::NEW_DIR_SIZE];
            self.object_read(indaddr, 0, &mut data[..format.size()])?;
            let dir = FixedDir::parse(&data, format)?;
            if dir.stranded {
                // Writing it back could revive entries no check byte vouches
                // for, as a later insertion closing the gap would.
                self.stranded = true;
            }
            Ok(Directory::Fixed(dir))
        }
    }

    /// A directory is about to be written: forget what was resolved, and
    /// what was validated, until the write re-seats it.
    fn directories_changing(&mut self) {
        self.validated = None;
        self.changes = self.changes.wrapping_add(1);
    }

    /// The big directory at `indaddr` stands as `dir`, just written.
    fn validated_as(&mut self, indaddr: u32, dir: &BigDir) {
        self.validated = Some((indaddr, dir.header));
    }

    /// Write a mutated fixed directory back to `indaddr`.
    fn store_fixed_dir(&mut self, indaddr: u32, dir: &FixedDir) -> Result<(), D> {
        self.directories_changing();
        let size = dir.format.size();
        // Re-stage through the object map so a fragmented new-map
        // directory lands in the right places.
        let mut done = 0usize;
        while done < size {
            let take = (size - done).min(512);
            let data = &dir.data[done..done + take];
            self.object_write(indaddr, done as u64, data)?;
            done += take;
        }
        Ok(())
    }

    /// The `index`-th entry of the directory at `indaddr`, if any.
    pub fn dir_get(&mut self, indaddr: u32, index: u32) -> Result<Option<Object>, D> {
        let handle = self.load_dir(indaddr)?;
        self.dir_entry(&handle, indaddr, index)
    }

    /// How many entries the loaded directory `handle` holds.
    #[must_use]
    pub fn dir_len(handle: &Directory) -> u32 {
        match handle {
            // A fixed directory holds at most 77 entries.
            Directory::Fixed(dir) => u32::try_from(dir.count()).unwrap_or(u32::MAX),
            Directory::Big(dir) => dir.header.entries,
        }
    }

    /// The `index`-th entry of the loaded directory `handle` at `indaddr`.
    pub fn dir_entry(
        &mut self,
        handle: &Directory,
        indaddr: u32,
        index: u32,
    ) -> Result<Option<Object>, D> {
        match handle {
            Directory::Fixed(dir) => Ok(usize::try_from(index).ok().and_then(|i| dir.entry(i))),
            Directory::Big(dir) => {
                let mut store = ObjectStore {
                    volume: self,
                    indaddr,
                    size: u32::MAX,
                };
                dir.entry(&mut store, index)
            }
        }
    }

    /// Where a listing of `directory`, at `indaddr`, resumes: at `index`
    /// while its shape is as it was at `minted` ([`Self::shapes`]), else just
    /// past `after` in the directory's order, which nothing shifts, so an
    /// entry present throughout is listed exactly once.
    pub fn resume(
        &mut self,
        directory: &Directory,
        indaddr: u32,
        index: u32,
        minted: u32,
        after: &[u8],
    ) -> Result<u32, D> {
        if minted == self.shapes || after.is_empty() {
            return Ok(index);
        }
        let (mut low, mut high) = (0u32, Self::dir_len(directory));
        while low < high {
            let mid = low + (high - low) / 2;
            let object = self
                .dir_entry(directory, indaddr, mid)?
                .ok_or(Fault::Broken(Structure::Directory))?;
            if acorn::name_order(object.name(), after) == Ordering::Greater {
                high = mid;
            } else {
                low = mid + 1;
            }
        }
        Ok(low)
    }

    /// Find the entry named `name` in the directory at `indaddr`.
    pub fn dir_lookup(&mut self, indaddr: u32, name: &[u8]) -> Result<Option<(u32, Object)>, D> {
        match self.load_dir(indaddr)? {
            Directory::Fixed(dir) => Ok(dir
                .find(name)
                // A fixed directory holds at most 77 entries.
                .map(|(index, object)| (u32::try_from(index).unwrap_or(0), object))),
            Directory::Big(dir) => {
                let mut store = ObjectStore {
                    volume: self,
                    indaddr,
                    size: u32::MAX,
                };
                dir.find(&mut store, name)
            }
        }
    }

    /// On-disc bytes allocated to the object at `indaddr` whose recorded
    /// size is `size`.
    pub fn object_allocated(&mut self, indaddr: u32, size: u32) -> Result<u64, D> {
        match &self.backing {
            Backing::Old { .. } => {
                // An old-map object is exactly its contiguous sector run.
                Ok(u64::from(size).div_ceil(OLD_SECTOR_SIZE) * OLD_SECTOR_SIZE)
            }
            Backing::New { map, .. } => {
                if indaddr >> 8 == 0 {
                    return Ok(0);
                }
                if indaddr & 0xFF != 0 {
                    // A shared-fragment object owns only its share
                    // granules; the fragment total belongs to several
                    // objects.
                    let granule = map.record.share_granule();
                    return Ok(u64::from(size).div_ceil(granule) * granule);
                }
                map.object_allocated_bytes(&mut self.disc, indaddr >> 8)
            }
        }
    }
}

impl<D: Disc> Volume<D> {
    /// Allocate a fresh object of `bytes`, returning its indirect disc
    /// address (old map: start sector); a new map's from its central zone
    /// on, as `FileCore` places a directory.
    pub fn allocate(&mut self, bytes: u64) -> Result<u32, D> {
        let home = match &self.backing {
            Backing::New { map, .. } => u32::from(map.record.nzones) >> 1,
            Backing::Old { .. } => 0,
        };
        self.allocate_from(bytes, home)
    }

    /// As [`Self::allocate`], for a file of the directory at `parent`: from
    /// the zone the directory lies in on, as `FileCore` places one.
    pub fn allocate_near(&mut self, parent: u32, bytes: u64) -> Result<u32, D> {
        let home = match &self.backing {
            Backing::New { map, .. } => map.zone_of(parent),
            Backing::Old { .. } => 0,
        };
        self.allocate_from(bytes, home)
    }

    /// A fresh object of `bytes`, a new map's taken from zone `home` on.
    fn allocate_from(&mut self, bytes: u64, home: u32) -> Result<u32, D> {
        self.map_changing()?;
        match &self.backing {
            Backing::Old { .. } => {
                let sectors =
                    u32::try_from(bytes.div_ceil(OLD_SECTOR_SIZE)).map_err(|_| Fault::DiscFull)?;
                let mut map = OldMap::load(&mut self.disc)?;
                let start = map.allocate(sectors)?;
                map.store(&mut self.disc)?;
                Ok(start)
            }
            Backing::New { map, .. } => map.allocate_object(&mut self.disc, bytes, home),
        }
    }

    /// Free the allocation of the object at `indaddr` whose recorded
    /// size is `size` (whole fragments only; the shared-fragment policy
    /// lives in the callers).
    pub fn free(&mut self, indaddr: u32, size: u32) -> Result<(), D> {
        if self.holds_nothing(indaddr) {
            return Ok(());
        }
        self.clear_of_structures(indaddr)?;
        // The root's fragment is never let go: what shares it stays with it.
        if matches!(self.backing, Backing::New { .. }) && !NewMap::is_object_fragment(indaddr >> 8)
        {
            return Err(Fault::Broken(Structure::Directory).into());
        }
        self.map_changing()?;
        self.directories_changing();
        match &self.backing {
            Backing::Old { .. } => {
                // A zero-length object occupies no sectors.
                let sectors = u64::from(size).div_ceil(OLD_SECTOR_SIZE);
                if sectors == 0 {
                    return Ok(());
                }
                // A `u32` size spans at most 2^24 sectors.
                let sectors = u32::try_from(sectors).map_err(|_| Fault::Broken(Structure::Map))?;
                let mut map = OldMap::load(&mut self.disc)?;
                map.free_span(indaddr, sectors)?;
                map.store(&mut self.disc)
            }
            Backing::New { map, .. } => map.free_object(&mut self.disc, indaddr >> 8),
        }
    }

    /// Copy `len` bytes from one object to another through the staging
    /// buffer.
    pub fn copy_object(&mut self, src: u32, dst: u32, len: u64) -> Result<(), D> {
        let mut buf = [0u8; 512];
        let mut done = 0u64;
        while done < len {
            let take = usize::try_from((len - done).min(512)).unwrap_or(512);
            self.object_read(src, done, &mut buf[..take])?;
            self.object_write(dst, done, &buf[..take])?;
            done += take as u64;
        }
        Ok(())
    }

    /// Grow the object at `indaddr` (recorded size `size`), an entry of the
    /// directory at `parent`, so its allocation covers `new_size` bytes:
    /// within its fragment where it has one of its own or shares one with
    /// nothing from its start, which it then takes as its own; else moved to
    /// a fresh object, its old granules left to
    /// [`Self::release_maybe_shared`]. Returns the object's (possibly new)
    /// indirect disc address.
    pub fn grow_object(
        &mut self,
        indaddr: u32,
        size: u32,
        new_size: u64,
        parent: u32,
    ) -> Result<u32, D> {
        if new_size <= u64::from(size) && !self.holds_nothing(indaddr) {
            return Ok(indaddr);
        }
        if !self.holds_nothing(indaddr) {
            self.clear_of_structures(indaddr)?;
        }
        self.map_changing()?;
        match &self.backing {
            Backing::Old { .. } => {
                let old_sectors =
                    u32::try_from(u64::from(size).div_ceil(OLD_SECTOR_SIZE)).unwrap_or(0);
                let new_sectors = u32::try_from(new_size.div_ceil(OLD_SECTOR_SIZE))
                    .map_err(|_| Fault::DiscFull)?;
                if new_sectors <= old_sectors {
                    return Ok(indaddr);
                }
                let mut map = OldMap::load(&mut self.disc)?;
                if indaddr != 0 && old_sectors != 0 {
                    // Try to consume free space directly after the run.
                    if map.try_extend(indaddr + old_sectors, new_sectors - old_sectors) {
                        map.store(&mut self.disc)?;
                        return Ok(indaddr);
                    }
                }
                // Relocate: allocate the full run, copy, free the old.
                let start = map.allocate(new_sectors)?;
                if indaddr != 0 && old_sectors != 0 {
                    map.free_span(indaddr, old_sectors)?;
                }
                map.store(&mut self.disc)?;
                if indaddr != 0 && size != 0 {
                    self.copy_object(indaddr, start, u64::from(size))?;
                }
                Ok(start)
            }
            Backing::New { .. } => {
                if self.holds_nothing(indaddr) {
                    return self.allocate(new_size);
                }
                if self.grows_in_place(indaddr, parent)? {
                    let Backing::New { map, .. } = &self.backing else {
                        return Err(Fault::Unsupported.into());
                    };
                    if map.extend_object(&mut self.disc, indaddr >> 8, new_size)? {
                        return Ok(indaddr & !0xFF);
                    }
                }
                // Relocate to a fresh object of its own, the path a shared
                // object with neighbours takes to grow.
                let new_indaddr = self.copy_out(indaddr, size, new_size)?;
                self.release_maybe_shared(indaddr, size, parent)?;
                Ok(new_indaddr)
            }
        }
    }

    /// Shrink the allocation of the object at `indaddr` from `size` to
    /// `new_size` bytes.
    pub fn shrink_object(&mut self, indaddr: u32, size: u32, new_size: u64) -> Result<(), D> {
        if self.holds_nothing(indaddr) || new_size >= u64::from(size) {
            return Ok(());
        }
        self.clear_of_structures(indaddr)?;
        self.map_changing()?;
        match &self.backing {
            Backing::Old { .. } => {
                let old_sectors =
                    u32::try_from(u64::from(size).div_ceil(OLD_SECTOR_SIZE)).unwrap_or(0);
                let new_sectors =
                    u32::try_from(new_size.div_ceil(OLD_SECTOR_SIZE)).unwrap_or(old_sectors);
                if new_sectors >= old_sectors {
                    return Ok(());
                }
                let mut map = OldMap::load(&mut self.disc)?;
                map.free_span(indaddr + new_sectors, old_sectors - new_sectors)?;
                map.store(&mut self.disc)
            }
            Backing::New { map, .. } => {
                if indaddr & 0xFF != 0 {
                    // A shared-fragment object cannot release granules
                    // without moving its fragment neighbours; the slack
                    // is reclaimed when the object is removed or grows
                    // away.
                    return Ok(());
                }
                map.shrink_object(&mut self.disc, indaddr >> 8, new_size)
            }
        }
    }

    /// Release the allocation behind `indaddr`, an entry of the directory
    /// at `parent`, honouring fragment sharing: a shared fragment is freed
    /// only when nothing else uses it.
    pub fn release_maybe_shared(&mut self, indaddr: u32, size: u32, parent: u32) -> Result<(), D> {
        if self.releases(indaddr, parent)? {
            self.free(indaddr, size)
        } else {
            Ok(())
        }
    }

    /// Whether releasing the allocation behind `indaddr`, an entry of the
    /// directory at `parent`, frees it: a shared fragment is freed only when
    /// nothing else uses it.
    pub fn releases(&mut self, indaddr: u32, parent: u32) -> Result<bool, D> {
        if self.holds_nothing(indaddr) {
            return Ok(false);
        }
        if self.is_shared(indaddr) {
            return Ok(!self.fragment_shared_in(parent, indaddr)?);
        }
        Ok(true)
    }

    /// Whether the object at `indaddr` has nothing allocated: on an old map
    /// no start sector, on a new map no fragment — whatever its share offset,
    /// as RISC OS 5 numbers an empty file `1`.
    #[must_use]
    pub fn holds_nothing(&self, indaddr: u32) -> bool {
        match &self.backing {
            Backing::Old { .. } => indaddr == 0,
            Backing::New { .. } => indaddr >> 8 == 0,
        }
    }

    /// Whether the object at `indaddr` lies in a fragment it may share.
    pub fn is_shared(&self, indaddr: u32) -> bool {
        matches!(&self.backing, Backing::New { .. }) && share_place(indaddr).is_some()
    }

    /// Where a new object of `bytes` for the directory at `parent` goes, as
    /// `FileCore` places one: nothing at all for an empty object; one whose
    /// whole share granules come short of the least fragment shares — room in
    /// a fragment the directory's objects share, else a new fragment of the
    /// least whole granules, from its start; any other a fragment of its own.
    pub fn place_object(&mut self, parent: u32, bytes: u64) -> Result<u32, D> {
        if bytes == 0 {
            return Ok(self.nothing());
        }
        let Some(granules) = self.sharing_granules(bytes) else {
            return self.allocate_near(parent, bytes);
        };
        if let Some(indaddr) = self.room_in_shared(parent, granules)? {
            return Ok(indaddr);
        }
        let fragment = self.shared_fragment_bytes();
        Ok(self.allocate_near(parent, fragment)? | 1)
    }

    /// Where the object at `indaddr`, an entry of the directory at `parent`
    /// holding `allocated` bytes, is left as `FileCore` leaves a file it
    /// closes at `extent` bytes: an empty one holds nothing; one small enough
    /// to share moves to room the directory's objects share, else keeps the
    /// least of its own fragment, shared from its start; a larger one is cut
    /// to its extent. Its address.
    pub fn settle_object(
        &mut self,
        indaddr: u32,
        allocated: u32,
        extent: u64,
        parent: u32,
    ) -> Result<u32, D> {
        if extent == 0 {
            self.release_maybe_shared(indaddr, allocated, parent)?;
            return Ok(self.nothing());
        }
        if self.holds_nothing(indaddr) || self.is_shared(indaddr) {
            return Ok(indaddr);
        }
        let Some(granules) = self.sharing_granules(extent) else {
            self.shrink_object(indaddr, allocated, extent)?;
            return Ok(indaddr);
        };
        if let Some(room) = self.room_in_shared(parent, granules)? {
            self.copy_object(indaddr, room, extent)?;
            self.free(indaddr, allocated)?;
            return Ok(room);
        }
        let fragment = self.shared_fragment_bytes();
        self.shrink_object(indaddr, allocated, fragment)?;
        Ok(indaddr | 1)
    }

    /// The bytes the object at `indaddr`, an entry of the directory at
    /// `parent`, keeps of its allocation as it grows as far as the disc's
    /// free space reaches, as `FileCore` grows an open file: its own
    /// fragment's, or those of a fragment it shares with nothing from its
    /// start; none where growing moves it. `None` on an old map, whose
    /// objects grow only as far as they must.
    pub fn growth_base(&mut self, indaddr: u32, parent: u32) -> Result<Option<u64>, D> {
        if matches!(self.backing, Backing::Old { .. }) {
            return Ok(None);
        }
        if self.holds_nothing(indaddr) || !self.grows_in_place(indaddr, parent)? {
            return Ok(Some(0));
        }
        let Backing::New { map, .. } = &self.backing else {
            return Ok(None);
        };
        map.object_allocated_bytes(&mut self.disc, indaddr >> 8)
            .map(Some)
    }

    /// Whether the object at a new map's `indaddr`, an entry of the directory
    /// at `parent`, grows within its fragment: one of its own, or one it
    /// shares with nothing from the fragment's start.
    fn grows_in_place(&mut self, indaddr: u32, parent: u32) -> Result<bool, D> {
        match share_place(indaddr) {
            None => Ok(true),
            Some((_, 0)) => Ok(!self.fragment_shared_in(parent, indaddr)?),
            Some(_) => Ok(false),
        }
    }

    /// What an object's entry holds where it has nothing allocated.
    fn nothing(&self) -> u32 {
        match self.backing {
            Backing::Old { .. } => 0,
            Backing::New { .. } => EMPTY_OBJECT,
        }
    }

    /// The share granules an object of `bytes` takes where it is small enough
    /// to share a fragment — its whole granules come short of the least
    /// fragment — on a new map; none for any other.
    fn sharing_granules(&self, bytes: u64) -> Option<u64> {
        let Backing::New { map, .. } = &self.backing else {
            return None;
        };
        let granule = map.record.share_granule();
        let granules = bytes.div_ceil(granule);
        (granules * granule < map.record.least_fragment()).then_some(granules)
    }

    /// The bytes of a fragment made for objects to share: the least fragment
    /// in whole granules.
    fn shared_fragment_bytes(&self) -> u64 {
        match &self.backing {
            Backing::New { map, .. } => {
                let granule = map.record.share_granule();
                map.record.least_fragment().div_ceil(granule) * granule
            }
            Backing::Old { .. } => 0,
        }
    }

    /// The first room for `granules` whole share granules in a fragment the
    /// directory at `parent` — or its objects — shares, lowest fragment
    /// first: never the map's, nor one the directory shares from past its
    /// start.
    fn room_in_shared(&mut self, parent: u32, granules: u64) -> Result<Option<u32>, D> {
        let Backing::New { map, .. } = &self.backing else {
            return Ok(None);
        };
        let granule = map.record.share_granule();
        let mut held: Vec<(u32, u64, u64)> = Vec::new();
        let mut note = |indaddr: u32, size: u32| -> core::result::Result<(), Fault> {
            let Some((fragment, first)) = share_place(indaddr) else {
                return Ok(());
            };
            held.try_reserve(1).map_err(|_| Fault::NoMemory)?;
            held.push((fragment, first, u64::from(size).div_ceil(granule)));
            Ok(())
        };
        let parent_size = self.dir_size_of(parent)?;
        note(parent, parent_size)?;
        let directory = self.load_dir(parent)?;
        for index in 0..Self::dir_len(&directory) {
            let Some(object) = self.dir_entry(&directory, parent, index)? else {
                break;
            };
            note(object.indaddr, object.size)?;
        }
        held.sort_unstable();
        // What lies in the map's fragment ahead of a root sharing it is the
        // map, and what shares a fragment with a directory that does not
        // begin it is more than its own entries show.
        let foreign = |fragment: u32| {
            !NewMap::is_object_fragment(fragment)
                || share_place(parent).is_some_and(|(own, first)| own == fragment && first > 0)
        };
        let mut start = 0;
        while start < held.len() {
            let fragment = held[start].0;
            let end = held[start..]
                .iter()
                .position(|&(id, _, _)| id != fragment)
                .map_or(held.len(), |run| start + run);
            if foreign(fragment) {
                start = end;
                continue;
            }
            let Backing::New { map, .. } = &self.backing else {
                return Ok(None);
            };
            let room =
                (map.object_allocated_bytes(&mut self.disc, fragment)? / granule).min(MOST_SHARE);
            let mut free_from = 0;
            for &(_, first, count) in &held[start..end] {
                if first >= free_from + granules {
                    break;
                }
                free_from = free_from.max(first + count);
            }
            if free_from + granules <= room {
                // `room` is at most the 255 offsets an address carries.
                let offset = u32::try_from(free_from + 1).unwrap_or(u32::MAX);
                return Ok(Some(fragment << 8 | offset));
            }
            start = end;
        }
        Ok(None)
    }

    /// The size of the directory at `indaddr`, which only its parent's entry
    /// records — or, for the root, the disc record.
    fn dir_size_of(&mut self, indaddr: u32) -> Result<u32, D> {
        if indaddr == self.root_addr() {
            return Ok(self.root_size());
        }
        match self.load_dir(indaddr)? {
            Directory::Big(dir) => Ok(dir.header.size),
            Directory::Fixed(dir) => Ok(u32::try_from(dir.format.size()).unwrap_or(u32::MAX)),
        }
    }

    /// A copy, for the directory at `to`, of `object`, a file of the
    /// directory at `from` sharing a fragment with others there: `FileCore`
    /// shares a fragment within one directory only, so such a file may not
    /// take it along when it moves, and takes a new fragment of its own to
    /// share from its start, as RISC OS 5 moves one. Its address, `None` for
    /// an object that moves as it is — one alone in its fragment takes the
    /// fragment along. The caller lets the object's old granules go once its
    /// entry has moved, or the copy's where the move fails.
    pub fn copy_for_move(&mut self, object: &Object, from: u32, to: u32) -> Result<Option<u32>, D> {
        if from == to
            || object.is_dir()
            || !self.is_shared(object.indaddr)
            || !self.fragment_shared_in(from, object.indaddr)?
        {
            return Ok(None);
        }
        let size = u64::from(object.size);
        let fresh = if self.sharing_granules(size).is_some() {
            let fragment = self.shared_fragment_bytes();
            self.allocate_near(to, fragment)? | 1
        } else {
            self.allocate_near(to, size)?
        };
        if let Err(err) = self.copy_object(object.indaddr, fresh, u64::from(object.size)) {
            // The copy's failure is the one to report.
            let _ = self.release_maybe_shared(fresh, object.size, to);
            return Err(err);
        }
        Ok(Some(fresh))
    }

    /// A fresh object of its own, `bytes` long, holding the first `size`
    /// bytes of the object at `indaddr`.
    pub fn copy_out(&mut self, indaddr: u32, size: u32, bytes: u64) -> Result<u32, D> {
        let fresh = self.allocate(bytes)?;
        if let Err(err) = self.copy_object(indaddr, fresh, u64::from(size)) {
            // The copy's failure is the one to report.
            let _ = self.free(fresh, u32::try_from(bytes).unwrap_or(u32::MAX));
            return Err(err);
        }
        Ok(fresh)
    }

    /// Whether the directory at `parent`, or another of its entries, uses
    /// the fragment of the object at `indaddr`.
    ///
    /// They are the only objects that can: `FileCore` shares a disc object
    /// only between a directory and the files in it, or files of one
    /// directory (RISC OS PRM, `FileCore`), so one directory answers what a
    /// walk of the tree once did.
    pub fn fragment_shared_in(&mut self, parent: u32, indaddr: u32) -> Result<bool, D> {
        let fragment = indaddr >> 8;
        if parent >> 8 == fragment {
            return Ok(true);
        }
        let handle = self.load_dir(parent)?;
        for index in 0..Self::dir_len(&handle) {
            let Some(object) = self.dir_entry(&handle, parent, index)? else {
                break;
            };
            if object.indaddr >> 8 == fragment && object.indaddr != indaddr {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// What `FileCore` addresses an empty object of a new map by: no fragment, at
/// share offset 1, as RISC OS 5 writes it.
pub const EMPTY_OBJECT: u32 = 1;

/// The most share granules one fragment's objects reach: a share offset's
/// eight bits less the zero that marks a whole fragment.
const MOST_SHARE: u64 = 255;

/// Where the object at a new map's `indaddr` lies in a fragment it shares:
/// the fragment and its first granule there; none for an object with no
/// fragment or a fragment of its own.
fn share_place(indaddr: u32) -> Option<(u32, u64)> {
    let (fragment, offset) = (indaddr >> 8, indaddr & 0xFF);
    (fragment != 0 && offset != 0).then(|| (fragment, u64::from(offset) - 1))
}

/// Largest big directory the format permits (4096 KiB).
const MAX_BIG_DIR: u32 = 4096 * 1024;

impl<D: Disc> Volume<D> {
    /// Validate a name, already spelt as the disc holds it, being written to
    /// a directory: within what its format holds, and in seven bits where it
    /// is a fixed directory, which keeps the top bit of each byte for itself.
    pub fn validate_new_name(&self, name: &[u8]) -> Result<(), D> {
        let big = self.is_big_dir();
        let max = if big {
            dir::MAX_NAME_LEN
        } else {
            dir::FIXED_NAME_LEN
        };
        if name.is_empty() || name.len() > max {
            return Err(Fault::NameTooLong.into());
        }
        if !big && !name.is_ascii() {
            return Err(Fault::BadName.into());
        }
        Ok(())
    }

    /// Grow the object at `indaddr` to hold `new_bytes` **without
    /// relocating it** (directories must keep their address). Returns
    /// whether the growth succeeded.
    pub fn grow_in_place(&mut self, indaddr: u32, new_bytes: u64) -> Result<bool, D> {
        // Old-map directories are fixed-size; nothing ever grows in place.
        if matches!(self.backing, Backing::Old { .. }) {
            return Ok(false);
        }
        self.map_changing()?;
        let Backing::New { map, .. } = &self.backing else {
            return Ok(false);
        };
        let total = map.record.share_base(indaddr) + new_bytes;
        map.extend_object(&mut self.disc, indaddr >> 8, total)
    }

    /// Insert `object` into the directory at `dir_addr`, growing a big
    /// directory in place when it is full.
    pub fn dir_insert(&mut self, dir_addr: u32, object: &Object) -> Result<(), D> {
        self.shapes = self.shapes.wrapping_add(1);
        match self.load_dir(dir_addr)? {
            Directory::Fixed(mut dir) => {
                dir.insert(object)?;
                self.store_fixed_dir(dir_addr, &dir)
            }
            Directory::Big(mut dir) => {
                let mut store = ObjectStore {
                    volume: self,
                    indaddr: dir_addr,
                    size: u32::MAX,
                };
                match dir.insert(&mut store, object) {
                    Err(Error::Fault(Fault::DirectoryFull)) => {}
                    Ok(()) => {
                        self.validated_as(dir_addr, &dir);
                        return Ok(());
                    }
                    Err(err) => return Err(err),
                }
                // Full: grow by one grain and retry.
                let new_size = dir
                    .header
                    .size
                    .checked_add(bigdir::BIG_DIR_GRAIN)
                    .filter(|&s| s <= MAX_BIG_DIR)
                    .ok_or(Fault::DirectoryFull)?;
                if !self.grow_in_place(dir_addr, u64::from(new_size))? {
                    return Err(Fault::DiscFull.into());
                }
                let mut store = ObjectStore {
                    volume: self,
                    indaddr: dir_addr,
                    size: u32::MAX,
                };
                dir.grow(&mut store, new_size)?;
                dir.insert(&mut store, object)?;
                self.validated_as(dir_addr, &dir);
                self.record_dir_size(dir_addr, new_size)
            }
        }
    }

    /// Remove the entry at `index` from the directory at `dir_addr`.
    pub fn dir_remove_at(&mut self, dir_addr: u32, index: u32) -> Result<(), D> {
        self.shapes = self.shapes.wrapping_add(1);
        match self.load_dir(dir_addr)? {
            Directory::Fixed(mut dir) => {
                dir.remove(index as usize);
                self.store_fixed_dir(dir_addr, &dir)
            }
            Directory::Big(mut dir) => {
                let mut store = ObjectStore {
                    volume: self,
                    indaddr: dir_addr,
                    size: u32::MAX,
                };
                dir.remove(&mut store, index)?;
                self.validated_as(dir_addr, &dir);
                Ok(())
            }
        }
    }

    /// Rewrite the entry at `index` of the directory at `dir_addr` with
    /// `object`'s metadata (the name is unchanged).
    pub fn dir_update_at(&mut self, dir_addr: u32, index: u32, object: &Object) -> Result<(), D> {
        match self.load_dir(dir_addr)? {
            Directory::Fixed(mut dir) => {
                dir.update(index as usize, object);
                self.store_fixed_dir(dir_addr, &dir)
            }
            Directory::Big(mut dir) => {
                let mut store = ObjectStore {
                    volume: self,
                    indaddr: dir_addr,
                    size: u32::MAX,
                };
                dir.update(&mut store, index, object)?;
                self.validated_as(dir_addr, &dir);
                Ok(())
            }
        }
    }

    /// As [`Self::dir_update_at`], also giving the entry `object`'s spelling of
    /// its name, which matches the stored one ignoring case.
    pub fn dir_respell_at(&mut self, dir_addr: u32, index: u32, object: &Object) -> Result<(), D> {
        match self.load_dir(dir_addr)? {
            Directory::Fixed(mut dir) => {
                dir.update(index as usize, object);
                self.store_fixed_dir(dir_addr, &dir)
            }
            Directory::Big(mut dir) => {
                let mut store = ObjectStore {
                    volume: self,
                    indaddr: dir_addr,
                    size: u32::MAX,
                };
                dir.update_respelt(&mut store, index, object)?;
                self.validated_as(dir_addr, &dir);
                Ok(())
            }
        }
    }

    /// The parent address recorded inside the directory at `dir_addr`.
    pub fn dir_parent(&mut self, dir_addr: u32) -> Result<u32, D> {
        match self.load_dir(dir_addr)? {
            Directory::Fixed(dir) => Ok(dir.parent()),
            Directory::Big(dir) => Ok(dir.header.parent),
        }
    }

    /// Record a grown big directory's new size where its size is
    /// stored: the parent's entry, or the disc record for the root.
    pub fn record_dir_size(&mut self, dir_addr: u32, new_size: u32) -> Result<(), D> {
        if dir_addr == self.root_addr() {
            // Only big directories grow, and they exist only on new-map
            // volumes.
            if matches!(self.backing, Backing::Old { .. }) {
                return Err(Fault::Broken(Structure::Directory).into());
            }
            self.map_changing()?;
            let Backing::New { map, boot_block } = &mut self.backing else {
                return Err(Fault::Broken(Structure::Directory).into());
            };
            let boot = *boot_block;
            return map.set_root_size(&mut self.disc, boot, new_size);
        }
        let parent = self.dir_parent(dir_addr)?;
        // Find the directory's entry in its parent by address.
        let mut index = 0u32;
        loop {
            let Some(entry) = self.dir_get(parent, index)? else {
                // The tree disagrees with the child's parent pointer.
                return Err(Fault::Broken(Structure::Directory).into());
            };
            if entry.indaddr == dir_addr {
                let mut updated = entry;
                updated.size = new_size;
                return self.dir_update_at(parent, index, &updated);
            }
            index += 1;
        }
    }
}

/// A loaded, validated directory of either shape. A fixed directory is
/// held whole on the stack by design (no read or write path allocates),
/// so the variant size difference is deliberate.
#[allow(clippy::large_enum_variant)]
pub enum Directory {
    /// A 1280- or 2048-byte fixed directory, held in memory.
    Fixed(FixedDir),
    /// A big directory (the engine streams through the object).
    Big(BigDir),
}

impl<D: Disc> Volume<D> {
    /// A new, empty directory named `name` in the one at `parent`, in the
    /// volume's directory form, a fixed one carrying its parent's marker:
    /// its disc address and size. Nothing is left allocated where it cannot
    /// be laid down.
    ///
    /// # Errors
    ///
    /// No room for it, or the disc's failure.
    pub fn new_directory(&mut self, parent: u32, name: &[u8]) -> Result<(u32, u32), D> {
        let bytes = if self.is_big_dir() {
            bigdir::BIG_DIR_GRAIN
        } else {
            self.fixed_format().bytes()
        };
        let child = self.allocate(u64::from(bytes))?;
        let seeded = if self.is_big_dir() {
            let mut store = ObjectStore {
                volume: self,
                indaddr: child,
                size: u32::MAX,
            };
            BigDir::initialise(&mut store, bytes, name, parent)
        } else {
            let format = self.fixed_format();
            self.fixed_marker(parent).and_then(|marker| {
                let fresh = FixedDir::initialise(format, marker, name, parent, NameEnd::Return);
                self.store_fixed_dir(child, &fresh)
            })
        };
        if let Err(error) = seeded {
            self.free(child, bytes)?;
            return Err(error);
        }
        Ok((child, bytes))
    }

    /// The directory at `dir_addr`, loaded as `directory`, given `parent` as
    /// its parent.
    ///
    /// # Errors
    ///
    /// The disc's failure.
    pub fn reparent(&mut self, dir_addr: u32, directory: Directory, parent: u32) -> Result<(), D> {
        match directory {
            Directory::Fixed(mut fixed) => {
                fixed.set_parent(parent);
                self.store_fixed_dir(dir_addr, &fixed)
            }
            Directory::Big(mut big) => {
                let mut store = ObjectStore {
                    volume: self,
                    indaddr: dir_addr,
                    size: u32::MAX,
                };
                big.set_parent(&mut store, parent)
            }
        }
    }

    /// The directory at `dir_addr`, loaded as `directory`, renamed `name`
    /// inside `parent`. A big directory keeps its own name and is rewritten
    /// to follow; a fixed one keeps only its parent.
    ///
    /// # Errors
    ///
    /// * [`Fault::NameTooLong`] or [`Fault::DirectoryFull`] from the big
    ///   directory's rename.
    /// * The disc's failure.
    pub fn rename_dir(
        &mut self,
        dir_addr: u32,
        directory: Directory,
        name: &[u8],
        parent: u32,
    ) -> Result<(), D> {
        match directory {
            Directory::Fixed(_) => self.reparent(dir_addr, directory, parent),
            Directory::Big(mut big) => {
                let mut store = ObjectStore {
                    volume: self,
                    indaddr: dir_addr,
                    size: u32::MAX,
                };
                big.rename(&mut store, name, parent)
            }
        }
    }

    /// A new map's free space gathered together as [`NewMap::compact`]
    /// gathers it: whether anything moved. An old map's is its caller's to
    /// gather, object by object.
    ///
    /// # Errors
    ///
    /// * [`Fault::Unsupported`] for an old map.
    /// * [`Fault::NoMemory`] where the host has no room for the walk.
    /// * A directory or the map broken, or the disc's failure.
    pub fn compact(&mut self) -> Result<bool, D> {
        if !matches!(self.backing, Backing::New { .. }) {
            return Err(Fault::Unsupported.into());
        }
        let directories = self.directory_ids()?;
        let Backing::New { map, .. } = &self.backing else {
            return Err(Fault::Unsupported.into());
        };
        let moved = map.compact(&mut self.disc, &directories)?;
        if moved {
            self.map_changing()?;
        }
        Ok(moved)
    }

    /// Every fragment id a directory lies in, ascending: the root's and each
    /// beneath it, each directory entered once however many entries name it.
    fn directory_ids(&mut self) -> Result<Vec<u32>, D> {
        let root = self.root_addr();
        let mut entered = OrderedSet::new();
        let mut pending = Vec::new();
        let mut ids = Vec::new();
        let mut enter = |addr: u32, pending: &mut Vec<u32>, ids: &mut Vec<u32>| {
            if !entered.try_insert(addr).map_err(|_| Fault::NoMemory)? {
                return Ok(());
            }
            pending.try_reserve(1).map_err(|_| Fault::NoMemory)?;
            ids.try_reserve(1).map_err(|_| Fault::NoMemory)?;
            pending.push(addr);
            ids.push(addr >> 8);
            Ok::<(), Fault>(())
        };
        enter(root, &mut pending, &mut ids)?;
        while let Some(addr) = pending.pop() {
            let dir = self.load_dir(addr)?;
            for index in 0..Self::dir_len(&dir) {
                let object = self
                    .dir_entry(&dir, addr, index)?
                    .ok_or(Fault::Broken(Structure::Directory))?;
                if object.is_dir() {
                    enter(object.indaddr, &mut pending, &mut ids)?;
                }
            }
        }
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// Whether the directory at `dir_addr` has no entries.
    pub fn dir_is_empty(&mut self, dir_addr: u32) -> Result<bool, D> {
        match self.load_dir(dir_addr)? {
            Directory::Fixed(dir) => Ok(dir.count() == 0),
            Directory::Big(dir) => Ok(dir.header.entries == 0),
        }
    }

    /// The `Hugo`/`Nick` marker of the fixed directory at `dir_addr`,
    /// copied onto directories created inside it so a volume stays
    /// marker-consistent.
    pub fn fixed_marker(&mut self, dir_addr: u32) -> Result<[u8; 4], D> {
        match self.load_dir(dir_addr)? {
            Directory::Fixed(dir) => {
                let mut marker = [0u8; 4];
                marker.copy_from_slice(&dir.data[1..5]);
                Ok(marker)
            }
            Directory::Big(_) => Err(Fault::Broken(Structure::Directory).into()),
        }
    }

    /// Zero the byte range `[from, to)` of the object at `indaddr`.
    pub fn zero_object_range(&mut self, indaddr: u32, from: u64, to: u64) -> Result<(), D> {
        let zeroes = [0u8; 512];
        let mut at = from;
        while at < to {
            let take = usize::try_from((to - at).min(512)).unwrap_or(512);
            self.object_write(indaddr, at, &zeroes[..take])?;
            at += take as u64;
        }
        Ok(())
    }

    /// Remove the `src_index`-th entry of the directory at `src_addr` and
    /// point the entry named `dst_name` in the directory at `dst_addr` at
    /// `object` under that spelling.
    pub fn respell_over(
        &mut self,
        src_addr: u32,
        src_index: u32,
        object: &Object,
        dst_addr: u32,
        dst_name: &[u8],
    ) -> Result<(), D> {
        self.dir_remove_at(src_addr, src_index)?;
        // The destination entry holds the replaced object's place in the sort
        // order, which the name asked for shares; indices may have shifted
        // when the source entry left the same directory, so the name is
        // looked up afresh.
        let Some((dst_index, _)) = self.dir_lookup(dst_addr, dst_name)? else {
            return Err(Fault::Broken(Structure::Directory).into());
        };
        self.dir_respell_at(dst_addr, dst_index, &object.renamed(dst_name)?)
    }
}

/// [`DirStore`] adapter exposing one object's bytes to the big-directory
/// engine.
struct ObjectStore<'a, D: Disc> {
    volume: &'a mut Volume<D>,
    indaddr: u32,
    size: u32,
}

impl<D: Disc> DirStore for ObjectStore<'_, D> {
    type Error = D::Error;

    fn read_at(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), D> {
        if u64::from(offset) + buf.len() as u64 > u64::from(self.size) {
            return Err(Fault::Broken(Structure::Directory).into());
        }
        self.volume
            .object_read(self.indaddr, u64::from(offset), buf)
    }

    fn write_at(&mut self, offset: u32, data: &[u8]) -> Result<(), D> {
        if u64::from(offset) + data.len() as u64 > u64::from(self.size) {
            return Err(Fault::Broken(Structure::Directory).into());
        }
        self.volume.directories_changing();
        self.volume
            .object_write(self.indaddr, u64::from(offset), data)
    }
}

#[cfg(test)]
mod tests;
