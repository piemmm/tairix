//! Acorn DFS disc images (`plans/ARCHIVES.md` AR12): an `.ssd`'s side, or a
//! `.dsd`'s two as the folders `Drive 0` and `Drive 2`, each side's catalogue
//! read into its files. A file is served by its full DFS name, `D.NAME` — its
//! directory, a dot, its name, a `/` in the name served as `.` as every Acorn
//! name crosses — and keeps its load and execution addresses and its lock as
//! its `acorn.*` attributes.
//!
//! A commit changes only what changed, as DFS would: a file whose bytes are
//! unchanged keeps its sectors; one written goes back where it was if it
//! still fits there, else into the first gap from the catalogue up that holds
//! it, and only where no gap does is the side compacted. Every other sector —
//! free space, and whatever a loader keeps outside the catalogue — is copied
//! as it was, and a catalogue holding the same entries is left as it was,
//! its cycle number unstepped.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::{NameMatching, NodeKind, WriteRefusal};
use tairix_abi::volume::{ArchiveName, ARCHIVE_REGION};
use tairix_abi::DriverError;
use tairix_dfs::catalogue::{
    self, Catalogue, Entry, CATALOGUE_LEN, CATALOGUE_SECTORS, MAX_FIELD, MAX_FILES, NAME_LEN,
};
use tairix_dfs::image::{Layout, SECTOR_LEN, SECTOR_SHIFT, TRACK_LEN};
use tairix_dfs::name;
use tairix_fsmeta::preset::acorn;
use tairix_fsserve::VolumeAccess;

use crate::data::{Archived, Check, Coding, Data, Lead};
use crate::format::{leaves_out, put_zeros, Content, Format, Placed, Read, Span};
use crate::region::{Regions, Sink};
use crate::riscos::meta_of;
use crate::tree::{Body, Tree, ROOT};
use crate::zip::directory_meta;

/// The folders a double-sided image's sides are served as: DFS calls the
/// second side of drive 0 drive 2.
const SIDE_NAMES: [&[u8]; 2] = [b"Drive 0", b"Drive 2"];

/// What a Watford DFS disc of 62 files opens its second catalogue with, in
/// the two sectors after the first, which DFS 1.20 counts free.
const WATFORD_MARK: [u8; 8] = [0xAA; 8];

/// The attribute bits every DFS file carries: it can always be read, and
/// written unless locked.
const ALWAYS: u16 = acorn::ATTR_OWNER_READ | acorn::ATTR_OWNER_WRITE;

/// The attribute bits a DFS file's entry keeps.
pub(crate) const ATTR_BITS: u16 = ALWAYS | acorn::ATTR_LOCKED;

/// A file's notes: its name's bytes as its catalogue held them, then the
/// first sector and the side it was last written at.
const NOTES_LEN: usize = NAME_LEN + 3;

/// A DFS disc image: how it holds its sides, and each side it serves, one
/// for each the layout holds, kept off the format's own footprint.
pub(crate) struct Dfs {
    layout: Layout,
    sides: Vec<Option<Side>>,
}

/// A side served.
struct Side {
    /// The directory serving it.
    dir: u64,
    /// Its catalogue as read or last written.
    catalogue: Catalogue,
}

/// Refuse a name no DFS file could be given: `D.NAME`, its directory one
/// character and its name one to seven, none of them a byte DFS reads a
/// meaning into.
pub(crate) fn admits_name(served: &[u8]) -> Result<(), DriverError> {
    dfs_name(served, true).map(|_| ())
}

/// The directory and name a served name spells, as a catalogue holds them:
/// strictly, only where DFS would make that name, as a name given a file
/// must be; otherwise wherever a catalogue read could hold it.
fn dfs_name(served: &[u8], strict: bool) -> Result<(u8, [u8; NAME_LEN]), DriverError> {
    let [dir, b'.', rest @ ..] = served else {
        return Err(DriverError::OutOfRange);
    };
    if rest.len() > NAME_LEN {
        return Err(DriverError::LengthOutOfRange);
    }
    let held = |byte: u8| {
        let byte = acorn::crossed_name_byte(byte);
        let fits = if strict {
            name::admits(byte)
        } else {
            byte.is_ascii() && name::readable(byte)
        };
        fits.then_some(byte)
    };
    let dir = held(*dir)
        .filter(|&dir| dir != name::PAD)
        .ok_or(DriverError::OutOfRange)?;
    let mut spelt = [name::PAD; NAME_LEN];
    for (slot, &byte) in spelt.iter_mut().zip(rest) {
        *slot = held(byte).ok_or(DriverError::OutOfRange)?;
    }
    if rest.is_empty() || spelt[0] == name::PAD {
        return Err(DriverError::OutOfRange);
    }
    Ok((dir, spelt))
}

/// `byte` of a DFS name as a served name holds it, `/` and `.` trading places
/// as every Acorn name crosses; `None` for a `.`, which ends a directory.
fn served_byte(byte: u8) -> Option<u8> {
    (byte != b'.').then(|| acorn::crossed_name_byte(byte))
}

/// The name `entry`'s file is served by, or `None` where its name holds a
/// byte no served name can.
fn served_name(entry: &Entry) -> Option<Vec<u8>> {
    let len = name::unpadded_len(&entry.name);
    let mut served = Vec::new();
    served.try_reserve_exact(len + 2).ok()?;
    served.push(served_byte(entry.dir)?);
    served.push(b'.');
    for &byte in &entry.name[..len] {
        served.push(served_byte(byte & 0x7F)?);
    }
    Some(served)
}

/// Where sector `sector` starts along its side.
fn sector_offset(sector: u16) -> u64 {
    u64::from(sector) << SECTOR_SHIFT
}

/// The sectors `len` bytes take.
fn sectors_of(len: u64) -> u64 {
    len.div_ceil(SECTOR_LEN as u64)
}

/// A file's notes, its name as `name` and its bytes at `start` of `side`.
fn notes(name: [u8; NAME_LEN], start: u16, side: u8) -> Result<Vec<u8>, DriverError> {
    let mut notes = Vec::new();
    notes
        .try_reserve_exact(NOTES_LEN)
        .map_err(|_| DriverError::OutOfMemory)?;
    notes.extend_from_slice(&name);
    notes.extend_from_slice(&start.to_le_bytes());
    notes.push(side);
    Ok(notes)
}

/// The name a file's notes hold, and the side and sector they say it was
/// last written at.
fn noted(notes: &[u8]) -> Option<([u8; NAME_LEN], u8, u16)> {
    let notes: &[u8; NOTES_LEN] = notes.try_into().ok()?;
    let (name, rest) = notes.split_first_chunk::<NAME_LEN>()?;
    let [low, high, side] = *rest else {
        return None;
    };
    Some((*name, side, u16::from_le_bytes([low, high])))
}

/// Read the DFS disc image `regions` holds, attached by `name`.
///
/// # Errors
///
/// [`DriverError::BadMagic`] where the first side holds no catalogue.
pub(crate) fn read<A: VolumeAccess>(
    regions: &Regions<A>,
    name: Option<&ArchiveName>,
) -> Result<Read, DriverError> {
    let first = catalogue_of(regions, Layout::Single, 0)?.ok_or(DriverError::BadMagic)?;
    let layout = layout_of(name, &first, regions.len());
    let mut first = Some(first);
    let mut tree = Tree::new(NameMatching::AsciiCaseInsensitive, directory_meta());
    let mut refusal = None;
    let mut sides = Vec::new();
    sides
        .try_reserve_exact(layout.sides())
        .map_err(|_| DriverError::OutOfMemory)?;
    sides.extend((0..layout.sides()).map(|_| None));
    for (side, slot) in sides.iter_mut().enumerate() {
        let catalogue = match first.take() {
            Some(catalogue) => Some(catalogue),
            None => catalogue_of(regions, layout, side)?,
        };
        let Some(catalogue) = catalogue else {
            continue;
        };
        let dir = match layout {
            Layout::Single => ROOT,
            Layout::Double => tree.add(
                ROOT,
                SIDE_NAMES[side],
                Body::Dir(BTreeMap::new()),
                directory_meta(),
                None,
            )?,
        };
        if watford(regions, layout, side, &catalogue)? {
            refusal = Some(WriteRefusal::UnsupportedFeature);
        }
        for entry in catalogue.files() {
            match admit(&mut tree, dir, (layout, side), entry) {
                Ok(()) => {}
                Err(err) if leaves_out(err) => refusal = Some(WriteRefusal::UnsupportedFeature),
                Err(err) => return Err(err),
            }
        }
        *slot = Some(Side { dir, catalogue });
    }
    Ok(Read {
        tree,
        format: Format::Dfs(Dfs { layout, sides }),
        refusal,
        stream: None,
        rar: None,
    })
}

/// `side`'s catalogue, where the image holds one there.
fn catalogue_of<A: VolumeAccess>(
    regions: &Regions<A>,
    layout: Layout,
    side: usize,
) -> Result<Option<Catalogue>, DriverError> {
    let image_len = usize::try_from(regions.len()).unwrap_or(usize::MAX);
    if layout.side_len(side, image_len) < CATALOGUE_LEN {
        return Ok(None);
    }
    let mut bytes = [0u8; CATALOGUE_LEN];
    regions.read_archive(layout.image_offset(side, 0) as u64, &mut bytes)?;
    Ok(Catalogue::read(&bytes))
}

/// How the image holds its sides: as its name's ending says, or for a name
/// saying nothing, both sides only where the image runs past the sectors
/// its first side counts.
fn layout_of(name: Option<&ArchiveName>, first: &Catalogue, image_len: u64) -> Layout {
    name.and_then(|name| core::str::from_utf8(name.as_bytes()).ok())
        .and_then(|name| name.rsplit_once('.'))
        .and_then(|(_, ending)| Layout::of_ending(ending))
        .unwrap_or(
            if image_len > u64::from(first.sectors) * SECTOR_LEN as u64 {
                Layout::Double
            } else {
                Layout::Single
            },
        )
}

/// Whether `side` holds a Watford DFS second catalogue: its mark in sectors
/// no file of the first catalogue covers, which a write could lay a file
/// over.
fn watford<A: VolumeAccess>(
    regions: &Regions<A>,
    layout: Layout,
    side: usize,
    catalogue: &Catalogue,
) -> Result<bool, DriverError> {
    let reserved = u32::from(CATALOGUE_SECTORS)..2 * u32::from(CATALOGUE_SECTORS);
    if catalogue
        .files()
        .iter()
        .any(|entry| u32::from(entry.start) < reserved.end && entry.end() > reserved.start)
    {
        return Ok(false);
    }
    let image_len = usize::try_from(regions.len()).unwrap_or(usize::MAX);
    if layout.side_len(side, image_len) < CATALOGUE_LEN + WATFORD_MARK.len() {
        return Ok(false);
    }
    let mut mark = [0u8; WATFORD_MARK.len()];
    regions.read_archive(layout.image_offset(side, CATALOGUE_LEN) as u64, &mut mark)?;
    Ok(mark == WATFORD_MARK)
}

/// Put `entry`'s file into `dir`, its bytes on `side` of an image laid out
/// as `layout`.
fn admit(
    tree: &mut Tree,
    dir: u64,
    (layout, side): (Layout, usize),
    entry: &Entry,
) -> Result<(), DriverError> {
    let name = served_name(entry).ok_or(DriverError::OutOfRange)?;
    let side = u8::try_from(side).map_err(|_| DriverError::OutOfRange)?;
    let body = Body::File(Data::Archived(Archived {
        at: sector_offset(entry.start),
        lead: Lead::None,
        len: u64::from(entry.length),
        coding: Coding::Disc { layout, side },
        check: Check::None,
    }));
    let mut meta = meta_of(None, None, NodeKind::RegularFile);
    meta.acorn = Some(acorn::Object {
        load: catalogue::address(entry.load),
        exec: catalogue::address(entry.exec),
        attr: if entry.locked { ATTR_BITS } else { ALWAYS },
    });
    let id = tree.add(dir, &name, body, meta, None)?;
    tree.node_mut(id)?.notes = notes(entry.name, entry.start, side)?;
    Ok(())
}

/// How many bytes the file `id` holds.
fn file_len(tree: &Tree, id: u64) -> Result<u64, DriverError> {
    match &tree.node(id)?.body {
        Body::File(data) => Ok(data.len()),
        _ => Err(DriverError::Unsupported),
    }
}

impl Dfs {
    /// The side `dir` serves.
    fn side_of(&self, dir: u64) -> Option<usize> {
        self.sides
            .iter()
            .position(|side| side.as_ref().is_some_and(|side| side.dir == dir))
    }

    /// Refuse a side's files outgrowing its catalogue or its sectors once the
    /// file `id` — one being made, where `None` — holds `len` bytes there and
    /// `gone` is gone.
    fn fits(
        &self,
        tree: &Tree,
        side: usize,
        (id, len): (Option<u64>, u64),
        gone: Option<u64>,
    ) -> Result<(), DriverError> {
        let state = self
            .sides
            .get(side)
            .and_then(Option::as_ref)
            .ok_or(DriverError::Unsupported)?;
        let mut files = 1usize;
        let mut sectors = sectors_of(len);
        for &other in tree.entries(state.dir)?.values() {
            if Some(other) == id || Some(other) == gone {
                continue;
            }
            files += 1;
            sectors += sectors_of(file_len(tree, other)?);
        }
        let room = u64::from(state.catalogue.sectors.saturating_sub(CATALOGUE_SECTORS));
        if files > usize::from(MAX_FILES) || sectors > room {
            Err(DriverError::NoSpace)
        } else {
            Ok(())
        }
    }

    /// Refuse a node of `kind` put at `name` in `dir` — made, or `moving`
    /// there — where no catalogue can hold it: anything but a file, outside
    /// a side, or past what the side holds.
    pub(crate) fn places(
        &self,
        tree: &Tree,
        dir: u64,
        name: &[u8],
        kind: NodeKind,
        moving: Option<u64>,
    ) -> Result<(), DriverError> {
        let side = self
            .side_of(dir)
            .filter(|_| kind == NodeKind::RegularFile)
            .ok_or(DriverError::Unsupported)?;
        match moving {
            // Renamed within its side, a file takes no more of it.
            Some(id) if tree.node(id)?.parent == dir => Ok(()),
            Some(id) => {
                let gone = tree.lookup(dir, name).ok().filter(|&found| found != id);
                self.fits(tree, side, (Some(id), file_len(tree, id)?), gone)
            }
            None => self.fits(tree, side, (None, 0), None),
        }
    }

    /// Refuse the file `id` holding `len` bytes where its side cannot.
    pub(crate) fn room(&self, tree: &Tree, id: u64, len: u64) -> Result<(), DriverError> {
        let node = tree.node(id)?;
        // A held file no name reaches is written to no side.
        if !node.named {
            return Ok(());
        }
        let side = self.side_of(node.parent).ok_or(DriverError::Unsupported)?;
        self.fits(tree, side, (Some(id), len), None)
    }

    /// Refuse removing `id` where it is a side's folder, which holds the
    /// side.
    pub(crate) fn removable(tree: &Tree, id: u64) -> Result<(), DriverError> {
        match tree.node(id)?.body {
            Body::Dir(_) => Err(DriverError::Unsupported),
            _ => Ok(()),
        }
    }

    /// Refuse RISC OS fields no entry keeps: any on a folder, an address no
    /// eighteen-bit field reads back as, or a file not readable and writable
    /// as every DFS file is.
    pub(crate) fn keeps_object(
        tree: &Tree,
        id: u64,
        object: &acorn::Object,
    ) -> Result<(), DriverError> {
        if !matches!(tree.node(id)?.body, Body::File(_)) {
            return Err(DriverError::Unsupported);
        }
        let addresses = catalogue::field(object.load).and(catalogue::field(object.exec));
        if addresses.is_none() || object.attr & ALWAYS != ALWAYS {
            return Err(DriverError::OutOfRange);
        }
        Ok(())
    }

    /// Write the next image from `tree` into `sink`.
    ///
    /// # Errors
    ///
    /// [`DriverError::NoSpace`] for a side whose files no longer fit it; the
    /// kernel's refusal.
    pub(crate) fn write<A: VolumeAccess>(
        &mut self,
        tree: &Tree,
        content: &mut Content<'_, A>,
        sink: &mut Sink<'_, A>,
    ) -> Result<Vec<Placed>, DriverError> {
        let mut plans: [Option<Plan>; 2] = [None, None];
        for (side, (plan, state)) in plans.iter_mut().zip(&self.sides).enumerate() {
            if let Some(state) = state {
                *plan = Some(self.plan(tree, side, state)?);
            }
        }
        let old_len = content.regions.len();
        let mut image_len = old_len;
        let mut stretches: [Vec<Stretch<'_>>; 2] = [Vec::new(), Vec::new()];
        for (side, plan) in plans.iter().enumerate() {
            let Some(plan) = plan else {
                continue;
            };
            let end = plan
                .files
                .iter()
                .map(|file| file.entry.end())
                .fold(u32::from(CATALOGUE_SECTORS), u32::max);
            let need = usize::try_from(end).map_err(|_| DriverError::NoSpace)? * SECTOR_LEN;
            image_len = image_len.max(self.layout.image_end(side, need) as u64);
            stretches[side] = plan.stretches(tree, content)?;
        }
        let mut out = Next {
            sink,
            old_len,
            pending: None,
        };
        'image: for track in 0.. {
            for (side, stretches) in stretches.iter().enumerate().take(self.layout.sides()) {
                let image_at = ((track * self.layout.sides() + side) * TRACK_LEN) as u64;
                if image_at >= image_len {
                    break 'image;
                }
                let len = (image_len - image_at).min(TRACK_LEN as u64);
                out.side(stretches, (track * TRACK_LEN) as u64, len, image_at)?;
            }
        }
        out.flush()?;
        if out.sink.position() != image_len {
            return Err(DriverError::DeviceFault);
        }
        let mut placed = Vec::new();
        for (side, plan) in plans.iter().enumerate() {
            let Some(plan) = plan else {
                continue;
            };
            let on = u8::try_from(side).map_err(|_| DriverError::OutOfRange)?;
            placed
                .try_reserve(plan.files.len())
                .map_err(|_| DriverError::OutOfMemory)?;
            for file in &plan.files {
                placed.push(Placed {
                    id: file.id,
                    data: Some(Data::Archived(Archived {
                        at: sector_offset(file.entry.start),
                        lead: Lead::None,
                        len: u64::from(file.entry.length),
                        coding: Coding::Disc {
                            layout: self.layout,
                            side: on,
                        },
                        check: Check::None,
                    })),
                    kept: None,
                    notes: Some(notes(file.entry.name, file.entry.start, on)?),
                });
            }
        }
        for (state, plan) in self.sides.iter_mut().zip(plans) {
            if let (Some(state), Some(plan)) = (state, plan) {
                state.catalogue = plan.catalogue;
            }
        }
        Ok(placed)
    }

    /// `side`'s next catalogue, and where each of its files goes.
    fn plan(&self, tree: &Tree, side: usize, state: &Side) -> Result<Plan, DriverError> {
        let ids = tree.entries(state.dir)?;
        let mut files = Vec::new();
        files
            .try_reserve_exact(ids.len())
            .map_err(|_| DriverError::OutOfMemory)?;
        for &id in ids.values() {
            files.push(self.file(tree, id, side)?);
        }
        place(&mut files, state.catalogue.sectors)?;
        let mut entries = [Entry::NONE; MAX_FILES as usize];
        let entries = entries.get_mut(..files.len()).ok_or(DriverError::NoSpace)?;
        for (slot, file) in entries.iter_mut().zip(&files) {
            *slot = file.entry;
        }
        let old = state.catalogue.files();
        if entries.len() == old.len() && entries.iter().all(|entry| old.contains(entry)) {
            return Ok(Plan {
                files,
                catalogue: state.catalogue.clone(),
                written: None,
            });
        }
        // Highest first, a file ending later ahead of one starting with it,
        // as DFS walks its catalogue from the end to find a gap.
        entries.sort_unstable_by(|a, b| {
            b.start
                .cmp(&a.start)
                .then(b.end().cmp(&a.end()))
                .then(a.dir.cmp(&b.dir))
                .then(a.name.cmp(&b.name))
        });
        let mut catalogue = state.catalogue.clone();
        catalogue.cycle = catalogue::next_cycle(catalogue.cycle);
        catalogue.set_files(entries).ok_or(DriverError::NoSpace)?;
        let mut written = [0u8; CATALOGUE_LEN];
        catalogue.write(&mut written);
        Ok(Plan {
            files,
            catalogue,
            written: Some(written),
        })
    }

    /// The file `id` of `side`, as its entry will hold it.
    fn file(&self, tree: &Tree, id: u64, side: usize) -> Result<File, DriverError> {
        let node = tree.node(id)?;
        let Body::File(data) = &node.body else {
            return Err(DriverError::Unsupported);
        };
        let (dir, spelt) = dfs_name(&node.name, false)?;
        let noted = noted(&node.notes);
        // A name kept keeps the bytes it was read with, top bits and all.
        let name = noted
            .map(|(name, _, _)| name)
            .filter(|name| name.iter().map(|&byte| byte & 0x7F).eq(spelt))
            .unwrap_or(spelt);
        let home = noted
            .filter(|&(_, on, _)| usize::from(on) == side)
            .map(|(_, _, start)| start);
        let object = node.meta.acorn.unwrap_or(acorn::Object {
            load: 0,
            exec: 0,
            attr: ALWAYS,
        });
        let field = |address| catalogue::field(address).ok_or(DriverError::OutOfRange);
        let length = u32::try_from(data.len())
            .ok()
            .filter(|&length| length <= MAX_FIELD)
            .ok_or(DriverError::LengthOutOfRange)?;
        let stays = match data {
            Data::Archived(Archived {
                at,
                coding: Coding::Disc { layout, side: on },
                ..
            }) if *layout == self.layout
                && usize::from(*on) == side
                && at % SECTOR_LEN as u64 == 0 =>
            {
                u16::try_from(at >> SECTOR_SHIFT).ok()
            }
            _ => None,
        };
        Ok(File {
            id,
            entry: Entry {
                name,
                dir,
                locked: object.attr & acorn::ATTR_LOCKED != 0,
                load: field(object.load)?,
                exec: field(object.exec)?,
                length,
                start: 0,
            },
            stays,
            home,
            order: node.order,
        })
    }
}

/// One file of a side being written.
struct File {
    id: u64,
    /// Its entry, its first sector once placed.
    entry: Entry,
    /// The sector its unchanged bytes already start at on this side.
    stays: Option<u16>,
    /// The sector it was last written at on this side.
    home: Option<u16>,
    /// The order it was read or made in.
    order: u64,
}

impl File {
    /// Whether its bytes already lie where it is placed.
    fn in_place(&self) -> bool {
        self.stays == Some(self.entry.start)
    }
}

/// A side's next catalogue and its files, placed.
struct Plan {
    files: Vec<File>,
    catalogue: Catalogue,
    /// The catalogue's bytes, where its entries changed.
    written: Option<[u8; CATALOGUE_LEN]>,
}

impl Plan {
    /// What the side holds in the next image where it does not hold what it
    /// held before, in order along it.
    fn stretches<A: VolumeAccess>(
        &self,
        tree: &Tree,
        content: &Content<'_, A>,
    ) -> Result<Vec<Stretch<'_>>, DriverError> {
        let mut stretches = Vec::new();
        stretches
            .try_reserve_exact(self.files.len() + 1)
            .map_err(|_| DriverError::OutOfMemory)?;
        if let Some(written) = &self.written {
            stretches.push(Stretch {
                at: 0,
                len: CATALOGUE_LEN as u64,
                fill: Fill::Catalogue(written),
            });
        }
        for file in &self.files {
            if file.in_place() || file.entry.length == 0 {
                continue;
            }
            let Body::File(data) = &tree.node(file.id)?.body else {
                return Err(DriverError::Unsupported);
            };
            let spans = content.spans(data).ok_or(DriverError::MediumError)?;
            stretches.push(Stretch {
                at: sector_offset(file.entry.start),
                len: sectors_of(u64::from(file.entry.length)) << SECTOR_SHIFT,
                fill: Fill::File(spans),
            });
        }
        stretches.sort_unstable_by_key(|stretch| stretch.at);
        Ok(stretches)
    }
}

/// Give every file a first sector: one whose bytes are unchanged keeps its
/// own, one written goes back where it was if it still fits there and
/// otherwise to the first gap from the catalogue up that holds it, and where
/// some file fits no gap the side is compacted.
fn place(files: &mut [File], sectors: u16) -> Result<(), DriverError> {
    let limit = u32::from(sectors);
    let mut taken = Vec::new();
    let mut movers = Vec::new();
    taken
        .try_reserve_exact(files.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    movers
        .try_reserve_exact(files.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    for (at, file) in files.iter_mut().enumerate() {
        match file.stays {
            Some(start) => {
                file.entry.start = start;
                taken.push((u32::from(start), file.entry.end()));
            }
            None => movers.push(at),
        }
    }
    movers.sort_unstable_by_key(|&at| files[at].order);
    for at in movers {
        let file = &mut files[at];
        let need = file.entry.sectors();
        let start = file
            .home
            .map(u32::from)
            .filter(|&home| free(&taken, home, need, limit))
            .or_else(|| first_fit(&mut taken, need, limit));
        let Some(start) = start else {
            return compact(files, limit);
        };
        file.entry.start = u16::try_from(start).map_err(|_| DriverError::NoSpace)?;
        taken.push((start, start + need));
    }
    Ok(())
}

/// Whether `need` sectors from `start` lie past the catalogue, within the
/// side's `limit` and clear of every run taken.
fn free(taken: &[(u32, u32)], start: u32, need: u32, limit: u32) -> bool {
    start >= u32::from(CATALOGUE_SECTORS)
        && start + need <= limit
        && taken
            .iter()
            .all(|&(from, to)| start + need <= from || to <= start)
}

/// The first sector from the catalogue up starting a gap of `need` sectors
/// within the side's `limit`, as DFS finds room for a file.
fn first_fit(taken: &mut [(u32, u32)], need: u32, limit: u32) -> Option<u32> {
    taken.sort_unstable();
    let mut start = u32::from(CATALOGUE_SECTORS);
    for &(from, to) in taken.iter() {
        if from >= start + need {
            break;
        }
        start = start.max(to);
    }
    (start + need <= limit).then_some(start)
}

/// Lay every file end to end from the catalogue as *COMPACT closes a side's
/// gaps: those whose bytes stay in the order they lie, then those written,
/// in turn.
fn compact(files: &mut [File], limit: u32) -> Result<(), DriverError> {
    let mut order = Vec::new();
    order
        .try_reserve_exact(files.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    order.extend(0..files.len());
    order.sort_unstable_by_key(|&at| (files[at].stays.is_none(), files[at].stays, files[at].order));
    let mut next = u32::from(CATALOGUE_SECTORS);
    for at in order {
        let file = &mut files[at];
        file.entry.start = u16::try_from(next).map_err(|_| DriverError::NoSpace)?;
        next += file.entry.sectors();
    }
    if next > limit {
        return Err(DriverError::NoSpace);
    }
    Ok(())
}

/// A stretch of a side whose next bytes are not those it held.
struct Stretch<'p> {
    /// Where it starts along the side.
    at: u64,
    /// Its length, in whole sectors.
    len: u64,
    fill: Fill<'p>,
}

/// What a stretch holds.
enum Fill<'p> {
    /// The catalogue, written afresh.
    Catalogue(&'p [u8; CATALOGUE_LEN]),
    /// A file's bytes as they lie now, then zeros to its last sector's end.
    File(Vec<Span>),
}

/// The next image as it is written: the bytes put so far, and a run of the
/// old image's own bytes waiting to be copied whole.
struct Next<'s, 'r, A> {
    sink: &'s mut Sink<'r, A>,
    old_len: u64,
    /// The old image's bytes from the first to before the second, which the
    /// next image holds at the same place.
    pending: Option<(u64, u64)>,
}

impl<A: VolumeAccess> Next<'_, '_, A> {
    /// Write `len` bytes of a side from `from` along it, which the image
    /// holds at `image_at`.
    fn side(
        &mut self,
        stretches: &[Stretch<'_>],
        from: u64,
        len: u64,
        image_at: u64,
    ) -> Result<(), DriverError> {
        let end = from + len;
        let first = stretches.partition_point(|stretch| stretch.at + stretch.len <= from);
        let mut at = from;
        for stretch in &stretches[first..] {
            if at >= end {
                break;
            }
            if stretch.at > at {
                let upto = stretch.at.min(end);
                self.old(image_at + (at - from), upto - at)?;
                at = upto;
            }
            if at < end {
                let upto = (stretch.at + stretch.len).min(end);
                self.fill(stretch, at - stretch.at, upto - at)?;
                at = upto;
            }
        }
        if at < end {
            self.old(image_at + (at - from), end - at)?;
        }
        Ok(())
    }

    /// The next `len` bytes, at `at` of the image, as the old image held
    /// them: a sector dump's missing tail is zeros.
    fn old(&mut self, at: u64, len: u64) -> Result<(), DriverError> {
        if let Some((_, end)) = &mut self.pending {
            if *end == at {
                *end += len;
                return Ok(());
            }
        }
        self.flush()?;
        self.pending = Some((at, at + len));
        Ok(())
    }

    /// Copy the waiting run of the old image's bytes.
    fn flush(&mut self) -> Result<(), DriverError> {
        let Some((start, end)) = self.pending.take() else {
            return Ok(());
        };
        let held = end.min(self.old_len);
        if start < held {
            self.sink.copy(ARCHIVE_REGION, start, held - start)?;
        }
        put_zeros(self.sink, end - start.max(held))
    }

    /// The `len` bytes of `stretch` from `skip` along it.
    fn fill(&mut self, stretch: &Stretch<'_>, skip: u64, len: u64) -> Result<(), DriverError> {
        self.flush()?;
        match &stretch.fill {
            Fill::Catalogue(bytes) => {
                let from = usize::try_from(skip).map_err(|_| DriverError::DeviceFault)?;
                let to = usize::try_from(skip + len).map_err(|_| DriverError::DeviceFault)?;
                self.sink
                    .put(bytes.get(from..to).ok_or(DriverError::DeviceFault)?)
            }
            Fill::File(spans) => {
                let (mut skip, mut left) = (skip, len);
                for span in spans {
                    if left == 0 {
                        break;
                    }
                    let (Span::In { len: span_len, .. } | Span::Zeros(span_len)) = *span;
                    if skip >= span_len {
                        skip -= span_len;
                        continue;
                    }
                    let take = (span_len - skip).min(left);
                    match *span {
                        Span::In { region, at, .. } => self.sink.copy(region, at + skip, take)?,
                        Span::Zeros(_) => put_zeros(self.sink, take)?,
                    }
                    skip = 0;
                    left -= take;
                }
                put_zeros(self.sink, left)
            }
        }
    }
}

#[cfg(test)]
#[path = "dfs_tests.rs"]
mod tests;
