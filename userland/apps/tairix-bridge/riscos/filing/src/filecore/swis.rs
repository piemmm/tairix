//! FileCore's SWIs: an instance made for a client, and each of a client's
//! discs reached through it, by the private word `FileCore_Create` answered.

use alloc::vec::Vec;

use tairix_filecore::defect::FIRST_END;
use tairix_filecore::format::{lay_out, BootRecord};
use tairix_filecore::newmap::{MappedOut, NewMap};
use tairix_filecore::record::{DiscRecord, DISC_RECORD_SIZE, LOG2SECSIZE, NAME_LEN};
use tairix_filecore::volume::Backing;
use tairix_filecore::Disc;
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs, Target};
use tairix_riscos_kernel::swi::os::MODULE;
use tairix_riscos_kernel::swi::X;

use super::disc::{enter_client, op, with_volume};
use super::{
    capped, code, described_at, fail, handed, names, record_at, space_of, split64, structure, with,
    Client, DRIVES, FIRST_HARD_DISC, RECORD_ROOM,
};

/// Its SWIs, by their offset in its chunk.
mod offset {
    pub(super) const DISC_OP: u32 = 0;
    pub(super) const CREATE: u32 = 1;
    pub(super) const DRIVES: u32 = 2;
    pub(super) const FREE_SPACE: u32 = 3;
    pub(super) const FLOPPY_STRUCTURE: u32 = 4;
    pub(super) const DESCRIBE_DISC: u32 = 5;
    pub(super) const DISCARD_READ_SECTORS_CACHE: u32 = 6;
    pub(super) const DISC_FORMAT: u32 = 7;
    pub(super) const LAYOUT_STRUCTURE: u32 = 8;
    pub(super) const MISC_OP: u32 = 9;
    pub(super) const SECTOR_DISC_OP: u32 = 10;
    pub(super) const FREE_SPACE_64: u32 = 11;
    pub(super) const DISC_OP_64: u32 = 12;
    pub(super) const FEATURES: u32 = 13;
}

/// Its SWIs' numbers, each its offset in FileCore's chunk.
pub mod number {
    use super::offset;
    use crate::filecore::CHUNK;

    /// `FileCore_DiscOp`.
    pub const DISC_OP: u32 = CHUNK + offset::DISC_OP;
    /// `FileCore_Create`.
    pub const CREATE: u32 = CHUNK + offset::CREATE;
    /// `FileCore_Drives`.
    pub const DRIVES: u32 = CHUNK + offset::DRIVES;
    /// `FileCore_FreeSpace`.
    pub const FREE_SPACE: u32 = CHUNK + offset::FREE_SPACE;
    /// `FileCore_FloppyStructure`.
    pub const FLOPPY_STRUCTURE: u32 = CHUNK + offset::FLOPPY_STRUCTURE;
    /// `FileCore_DescribeDisc`.
    pub const DESCRIBE_DISC: u32 = CHUNK + offset::DESCRIBE_DISC;
    /// `FileCore_DiscardReadSectorsCache`.
    pub const DISCARD_READ_SECTORS_CACHE: u32 = CHUNK + offset::DISCARD_READ_SECTORS_CACHE;
    /// `FileCore_DiscFormat`.
    pub const DISC_FORMAT: u32 = CHUNK + offset::DISC_FORMAT;
    /// `FileCore_LayoutStructure`.
    pub const LAYOUT_STRUCTURE: u32 = CHUNK + offset::LAYOUT_STRUCTURE;
    /// `FileCore_MiscOp`.
    pub const MISC_OP: u32 = CHUNK + offset::MISC_OP;
    /// `FileCore_SectorDiscOp`.
    pub const SECTOR_DISC_OP: u32 = CHUNK + offset::SECTOR_DISC_OP;
    /// `FileCore_FreeSpace64`.
    pub const FREE_SPACE_64: u32 = CHUNK + offset::FREE_SPACE_64;
    /// `FileCore_DiscOp64`.
    pub const DISC_OP_64: u32 = CHUNK + offset::DISC_OP_64;
    /// `FileCore_Features`.
    pub const FEATURES: u32 = CHUNK + offset::FEATURES;
}

/// What `FileCore_Features` answers, as RISC OS 5 answers it.
const FEATURES: u32 = 3;

/// `MiscOp` 6: the block a client's descriptor was turned into.
const DESCRIBE: u32 = 6;

/// The SWI handler.
pub(super) fn swi(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    match offset {
        offset::CREATE => create(os, regs),
        offset::FEATURES => {
            regs.r[0] = FEATURES;
            Ok(())
        }
        offset::DISCARD_READ_SECTORS_CACHE => Ok(()),
        offset::FLOPPY_STRUCTURE => floppy_structure(os, regs),
        offset::DISC_FORMAT => disc_format(os, regs),
        offset::LAYOUT_STRUCTURE => layout_structure(os, regs),
        _ => {
            let (workspace, client) = instance_of(os, regs.r[8])?;
            match offset {
                offset::DISC_OP => disc_op(os, workspace, &client, regs, false),
                offset::SECTOR_DISC_OP => disc_op(os, workspace, &client, regs, true),
                offset::DRIVES => {
                    regs.r[0] = u32::from(client.default_drive);
                    regs.r[1] = u32::from(client.floppies);
                    regs.r[2] = u32::from(client.hard_discs);
                    Ok(())
                }
                offset::FREE_SPACE | offset::FREE_SPACE_64 => {
                    free_space(os, workspace, regs, offset == offset::FREE_SPACE_64)
                }
                offset::DESCRIBE_DISC => describe_disc(os, workspace, regs),
                offset::MISC_OP => misc_op(os, workspace, &client, regs),
                offset::DISC_OP_64 => disc_op_64(os, workspace, &client, regs),
                _ => Err(fail(os, Some(&client), code::BAD_PARMS)),
            }
        }
    }
}

/// The instance whose private word is at `private_word`: its workspace and
/// client.
fn instance_of(os: &mut Os<'_>, private_word: u32) -> Result<(u32, Client), Fail> {
    match with(os, |state| {
        state.named(private_word).map(|i| (i.workspace, i.client))
    })? {
        Some(found) => Ok(found),
        None => Err(fail(os, None, code::BAD_PARMS)),
    }
}

/// `FileCore_Create`: an instance for the client R0's descriptor block
/// describes, its module at R1 and its private word at R2, R3 its drives and
/// the drive it starts at — made as an incarnation of FileCore named after
/// its filing system. R0 the instance's private word, R1 to R3 the routines
/// a client calls back.
fn create(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (descriptor, base) = (regs.r[0], regs.r[1]);
    let bits = os.caller_bits();
    let flags = os.read_word(descriptor)? & 0x00FF_FFFF;
    let number = os.read_byte(descriptor.wrapping_add(3))?;
    let at = |offset: u32| base.wrapping_add(offset);
    let client = Client {
        flags,
        number,
        title: at(os.read_word(descriptor.wrapping_add(4))?),
        boot_text: at(os.read_word(descriptor.wrapping_add(8))?),
        disc_op: Target {
            address: at(os.read_word(descriptor.wrapping_add(12))?),
            bits,
        },
        misc_op: Target {
            address: at(os.read_word(descriptor.wrapping_add(16))?),
            bits,
        },
        private_word: regs.r[2],
        floppies: regs.r[3].to_le_bytes()[0].min(FIRST_HARD_DISC),
        hard_discs: regs.r[3].to_le_bytes()[1].min(DRIVES - FIRST_HARD_DISC),
        default_drive: regs.r[3].to_le_bytes()[2] & (DRIVES - 1),
    };
    let (title, _) = os.read_until(client.title, names::NAME_MOST, |b| b <= b' ')?;
    if title.is_empty() || number == 0 {
        return Err(fail(os, None, code::BAD_PARMS));
    }
    with(os, |state| {
        state.pending = Some(client);
        state.created = None;
    })?;
    let mut name = alloc::vec::Vec::new();
    if name
        .try_reserve_exact(b"FileCore%".len() + title.len())
        .is_err()
    {
        return Err(os.no_memory());
    }
    name.extend_from_slice(b"FileCore%");
    name.extend_from_slice(&title);
    let made = os.with_text(&name, 0, |os, frame| {
        let mut module = Regs::default();
        module.r[0] = 14;
        module.r[1] = frame;
        os.perform(X | MODULE, &mut module)
    });
    let created = with(os, |state| {
        state.pending = None;
        state.created.take()
    })?;
    made?;
    let Some(private_word) = created else {
        return Err(fail(os, None, code::BAD_PARMS));
    };
    let [floppy, hard_disc, release] = handed(os);
    regs.r[0] = private_word;
    regs.r[1] = floppy;
    regs.r[2] = hard_disc;
    regs.r[3] = release;
    Ok(())
}

/// The bytes of one sector for a transfer on `drive`: as the record at
/// `alternate` gives them where the caller names one, else as the disc in
/// the drive has them, mounted first where FileCore has not seen it yet.
fn transfer_sector(
    os: &mut Os<'_>,
    workspace: u32,
    client: &Client,
    drive: u8,
    alternate: u32,
) -> Result<u32, Fail> {
    if alternate == 0 {
        return with_volume(os, workspace, drive, |volume, _| {
            Ok(volume.disc_mut().sector())
        });
    }
    let log2 = os.read_byte(alternate)?;
    if LOG2SECSIZE.contains(&log2) {
        Ok(1 << log2)
    } else {
        Err(fail(os, Some(client), code::BAD_PARMS))
    }
}

/// `FileCore_DiscOp` and `FileCore_SectorDiscOp`: the client's `DiscOp`
/// entry called with R1 to R6 — R2 a byte address, or a sector's where
/// `sectors` — the record FileCore keeps for the drive in R5 unless R1 names
/// another.
fn disc_op(
    os: &mut Os<'_>,
    workspace: u32,
    client: &Client,
    regs: &mut Regs,
    sectors: bool,
) -> Result<(), Fail> {
    let drive = (regs.r[2] >> 29).to_le_bytes()[0];
    if drive >= DRIVES || !client.has(drive) {
        return Err(fail(os, Some(client), code::BAD_DRIVE));
    }
    let alternate = (regs.r[1] & 0xFFFF_FF00) >> 6;
    let sector = transfer_sector(os, workspace, client, drive, alternate)?;
    let given = u64::from(regs.r[2] & 0x1FFF_FFFF);
    let by_sector = client.flags & super::flag::SECTOR_ADDRESSING != 0;
    let address = match (sectors, by_sector) {
        (true, false) => given * u64::from(sector),
        (false, true) => given / u64::from(sector),
        _ => given,
    };
    let Some(address) = u32::try_from(address).ok().filter(|&a| a < 1 << 29) else {
        return Err(fail(os, Some(client), code::BAD_PARMS));
    };
    let mut call = *regs;
    call.r[1] = regs.r[1] & 0xFF;
    if call.r[1] & 0xF == 9 {
        call.r[1] = (call.r[1] & !0xF) | op::READ;
    }
    call.r[2] = u32::from(drive) << 29 | address;
    call.r[5] = if alternate != 0 {
        alternate
    } else {
        record_at(workspace, drive)
    };
    enter_client(os, client, client.disc_op, &mut call)?;
    let left = u64::from(call.r[2] & 0x1FFF_FFFF);
    let reached = match (sectors, by_sector) {
        (true, false) => left / u64::from(sector),
        (false, true) => left * u64::from(sector),
        _ => left,
    };
    regs.r[2] = u32::from(drive) << 29 | u32::try_from(reached).unwrap_or(0) & 0x1FFF_FFFF;
    regs.r[3] = call.r[3];
    regs.r[4] = call.r[4];
    Ok(())
}

/// `FileCore_DiscOp64`: as `FileCore_DiscOp`, R2 pointing at the disc
/// address — its drive, then the byte address's low and high words — which
/// is moved on past what was transferred.
fn disc_op_64(
    os: &mut Os<'_>,
    workspace: u32,
    client: &Client,
    regs: &mut Regs,
) -> Result<(), Fail> {
    let block = regs.r[2];
    let drive = os.read_word(block)?.to_le_bytes()[0] & (DRIVES - 1);
    let low = os.read_word(block.wrapping_add(4))?;
    let high = os.read_word(block.wrapping_add(8))?;
    let address = u64::from(low) | u64::from(high) << 32;
    if !client.has(drive) {
        return Err(fail(os, Some(client), code::BAD_DRIVE));
    }
    let sector = u64::from(transfer_sector(os, workspace, client, drive, 0)?);
    let by_sector = client.flags & super::flag::SECTOR_ADDRESSING != 0;
    let given = if by_sector { address / sector } else { address };
    let Some(given) = u32::try_from(given).ok().filter(|&a| a < 1 << 29) else {
        return Err(fail(os, Some(client), code::BAD_PARMS));
    };
    let mut call = *regs;
    call.r[1] = regs.r[1] & 0xFF;
    if call.r[1] & 0xF == 9 {
        call.r[1] = (call.r[1] & !0xF) | op::READ;
    }
    call.r[2] = u32::from(drive) << 29 | given;
    call.r[5] = record_at(workspace, drive);
    enter_client(os, client, client.disc_op, &mut call)?;
    let reached = u64::from(call.r[2] & 0x1FFF_FFFF);
    let reached = if by_sector { reached * sector } else { reached };
    let [low, high] = split64(reached);
    os.write_word(block.wrapping_add(4), low)?;
    os.write_word(block.wrapping_add(8), high)?;
    regs.r[3] = call.r[3];
    regs.r[4] = call.r[4];
    Ok(())
}

/// The drive the disc specifier at `at` names.
fn drive_named(os: &mut Os<'_>, workspace: u32, at: u32) -> Result<u8, Fail> {
    let (text, _) = os.read_until(at, names::NAME_MOST, |b| b < b' ')?;
    let spec = text.strip_prefix(b":").unwrap_or(&text);
    let disc = (!spec.is_empty()).then_some(spec);
    names::drive_of(os, workspace, disc)
}

/// `FileCore_FreeSpace` and `FileCore_FreeSpace64`: the free space of the
/// disc R0 names — R0 free and R1 the largest object, or R0 and R1 free and
/// R2 the largest object — each capped at 2 GiB where 32 bits hold it.
fn free_space(os: &mut Os<'_>, workspace: u32, regs: &mut Regs, wide: bool) -> Result<(), Fail> {
    let drive = drive_named(os, workspace, regs.r[0])?;
    let (free, _) = with_volume(os, workspace, drive, |volume, _| space_of(volume))?;
    if wide {
        [regs.r[0], regs.r[1]] = split64(free);
        regs.r[2] = capped(free);
    } else {
        regs.r[0] = capped(free);
        regs.r[1] = capped(free);
    }
    Ok(())
}

/// `FileCore_DescribeDisc`: the record of the disc R0 names into R1's 64
/// bytes.
fn describe_disc(os: &mut Os<'_>, workspace: u32, regs: &Regs) -> Result<(), Fail> {
    let drive = drive_named(os, workspace, regs.r[0])?;
    let record = with_volume(os, workspace, drive, |volume, _| {
        Ok(match volume.backing() {
            Backing::New { map, .. } => Some(map.record),
            Backing::Old { .. } => None,
        })
    })?;
    let mut raw = [0u8; RECORD_ROOM as usize];
    match record {
        Some(record) => raw[..DISC_RECORD_SIZE].copy_from_slice(&record.encode()),
        None => os.read_bytes(record_at(workspace, drive), &mut raw)?,
    }
    os.write_bytes(regs.r[1], &raw)
}

/// `FileCore_MiscOp`: the client's miscellaneous entry called with R0 to R5,
/// but reason 6, which FileCore answers itself with the block its
/// descriptor became.
fn misc_op(os: &mut Os<'_>, workspace: u32, client: &Client, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[0] == DESCRIBE {
        regs.r[0] = described_at(workspace);
        return Ok(());
    }
    if regs.r[0] == 7 && client.flags & super::flag::MISC_OP_7 == 0 {
        return Err(fail(os, Some(client), code::BAD_PARMS));
    }
    let mut call = *regs;
    enter_client(os, client, client.misc_op, &mut call)?;
    regs.r[..7].copy_from_slice(&call.r[..7]);
    Ok(())
}

/// The disc record at `at`.
fn record_read(os: &mut Os<'_>, at: u32) -> Result<DiscRecord, Fail> {
    let mut raw = [0u8; DISC_RECORD_SIZE];
    os.read_bytes(at, &mut raw)?;
    Ok(DiscRecord::parse(&raw))
}

/// `FileCore_FloppyStructure`: the start of the floppy whose record is at
/// R1, laid out in R0's buffer as [`structure`] lays it, the defects listed
/// at R3 mapped out of a new map; R3 how much of the buffer it fills.
fn floppy_structure(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let record = record_read(os, regs.r[1])?;
    let defects = defect_list(os, regs.r[3])?;
    let [low, high, ..] = os.real_time().to_le_bytes();
    let laid = match structure::floppy(
        &record,
        regs.r[2],
        &defects,
        u16::from_le_bytes([low, high]),
    ) {
        Ok(laid) => laid,
        Err(tairix_filecore::Fault::NoMemory) => return Err(os.no_memory()),
        Err(_) => return Err(fail(os, None, code::BAD_PARMS)),
    };
    for (at, bytes) in laid.image.written_runs() {
        os.write_bytes(regs.r[0].wrapping_add(len32(at)), bytes)?;
    }
    if let Some(at) = laid.cleared {
        let at = regs.r[0].wrapping_add(at);
        let byte = os.read_byte(at)?;
        os.write_byte(at, byte & !(1 << 1))?;
    }
    regs.r[3] = laid.size;
    if laid.refused {
        return Err(fail(os, None, code::DEFECT));
    }
    Ok(())
}

/// The defects listed at `at`, up to the first list's end; none at a list
/// of none.
fn defect_list(os: &mut Os<'_>, at: u32) -> Result<Vec<u32>, Fail> {
    let mut defects = Vec::new();
    for n in 0..DEFECTS_READ {
        let word = os.read_word(at.wrapping_add(4 * n))?;
        if word >= FIRST_END {
            break;
        }
        if defects.try_reserve(1).is_err() {
            return Err(os.no_memory());
        }
        defects.push(word);
    }
    Ok(defects)
}

/// The most defects a structure's list is read for: more than any boot
/// block holds.
const DEFECTS_READ: u32 = 1024;

/// A floppy's track layout as `FileCore_DiscFormat` describes it, before the
/// controller's vetting adjusts it.
struct TrackLayout {
    sector: u32,
    /// The first gap on each side.
    gap1: [u32; 2],
    gap3: u32,
    per_track: u8,
    density: u8,
    options: u8,
    side_skew: u8,
    track_skew: u8,
}

/// Every format R3 may name, as RISC OS 5 lays each out: L, D, E, F and the
/// 20-sector octal-density format.
fn track_layout(format: u32) -> Option<TrackLayout> {
    let double = TrackLayout {
        sector: 1024,
        gap1: [303, 32],
        gap3: 90,
        per_track: 5,
        density: 2,
        options: 0,
        side_skew: 0,
        track_skew: 0,
    };
    Some(match format {
        0x80 => TrackLayout {
            sector: 256,
            gap1: [42, 42],
            gap3: 57,
            per_track: 16,
            options: 3 << 2,
            ..double
        },
        0x81 | 0x82 => double,
        0x83 => TrackLayout {
            gap1: [50, 50],
            per_track: 10,
            density: 4,
            side_skew: 1,
            track_skew: 3,
            ..double
        },
        0x84 => TrackLayout {
            gap1: [50, 50],
            per_track: 20,
            density: 8,
            side_skew: 1,
            track_skew: 5,
            ..double
        },
        _ => return None,
    })
}

/// The byte a floppy format fills its sectors with.
const SECTOR_FILL: u8 = 0xA5;
/// The tracks a floppy format formats: eighty on each side.
const FORMAT_TRACKS: u32 = 160;

/// `FileCore_DiscFormat`: R0's block filled with format R3's layout, then
/// handed to the vetting SWI R1 with R2 in its R1, which may change it.
fn disc_format(os: &mut Os<'_>, regs: &Regs) -> Result<(), Fail> {
    let Some(layout) = track_layout(regs.r[3]) else {
        return Err(fail(os, None, code::BAD_PARMS));
    };
    let block = regs.r[0];
    os.fill(block, 64, 0)?;
    os.write_word(block, layout.sector)?;
    os.write_word(block.wrapping_add(4), layout.gap1[0])?;
    os.write_word(block.wrapping_add(8), layout.gap1[1])?;
    os.write_word(block.wrapping_add(12), layout.gap3)?;
    os.write_byte(block.wrapping_add(16), layout.per_track)?;
    os.write_byte(block.wrapping_add(17), layout.density)?;
    os.write_byte(block.wrapping_add(18), layout.options)?;
    os.write_byte(block.wrapping_add(21), layout.side_skew)?;
    os.write_byte(block.wrapping_add(22), layout.track_skew)?;
    os.write_byte(block.wrapping_add(23), SECTOR_FILL)?;
    os.write_word(block.wrapping_add(24), FORMAT_TRACKS)?;
    let mut vet = Regs::default();
    vet.r[0] = block;
    vet.r[1] = regs.r[2];
    os.perform(X | regs.r[1], &mut vet)
}

/// `FileCore_LayoutStructure`: the empty new-map disc the record at R0
/// describes, named R2, laid out in the image file open on R3 with no boot
/// block, its fixed root named and titled after the disc, and the defects
/// listed at R1 mapped out. RISC OS 5 lays out no fragment in zone 0, which
/// leaves no disc FileCore could mount, and lays an old-map record out as a
/// malformed new map; zone 0 is laid out here, and an old-map record refused,
/// as is a disc past what an image file's 32-bit offsets reach.
fn layout_structure(os: &mut Os<'_>, regs: &Regs) -> Result<(), Fail> {
    let mut record = record_read(os, regs.r[0])?;
    if record.nzones == 0 || record.disc_size > IMAGE_REACH {
        return Err(fail(os, None, code::BAD_PARMS));
    }
    let (name, _) = os.read_until(regs.r[2], NAME_LEN + 1, |b| b <= b' ')?;
    let name = &name[..name.len().min(NAME_LEN)];
    let mut padded = [b' '; NAME_LEN];
    padded[..name.len()].copy_from_slice(name);
    record.disc_name = padded;
    let defects = defect_list(os, regs.r[1])?;
    let root_name: &[u8] = if record.format_version == 0 {
        name
    } else {
        b"$"
    };
    let buffer = os.rma_claim(TRANSFER)?;
    let mut image = ImageDisc {
        os,
        handle: regs.r[3],
        capacity: record.disc_size,
        buffer,
    };
    let laid = lay_out(&mut image, record, BootRecord::Omitted, root_name)
        .and_then(|record| map_out_all(&mut image, record, &defects));
    let os = image.os;
    os.rma_free(buffer)?;
    match laid {
        Ok(()) => Ok(()),
        Err(error) => Err(super::refused(os, None, error)),
    }
}

/// Each of `defects` mapped out of the fresh disc `record` describes.
fn map_out_all(
    image: &mut ImageDisc<'_, '_>,
    record: DiscRecord,
    defects: &[u32],
) -> Result<(), tairix_filecore::Error<Fail>> {
    let map = NewMap::open(image, record)?;
    let sector = record.sector_size();
    let bpmb = record.bytes_per_map_bit();
    for &defect in defects {
        let start = u64::from(defect) - u64::from(defect) % sector;
        if start >= record.disc_size {
            continue;
        }
        if map.map_out(image, start / bpmb, sector.div_ceil(bpmb))? == MappedOut::InUse {
            return Err(tairix_filecore::Fault::DefectInUse.into());
        }
    }
    Ok(())
}

/// An image file open on `handle` as a disc, through `OS_GBPB` and the
/// transfer buffer `buffer`, [`TRANSFER`] bytes of the RMA.
struct ImageDisc<'o, 'w> {
    os: &'o mut Os<'w>,
    handle: u32,
    capacity: u64,
    buffer: u32,
}

/// The most of an image moved through its buffer at once.
const TRANSFER: u32 = 1024;

/// The bytes an image file's offsets reach.
const IMAGE_REACH: u64 = 1 << 32;

impl ImageDisc<'_, '_> {
    /// `length` bytes moved at `at`, a buffer's worth at a time.
    fn gbpb(
        &mut self,
        reason: u32,
        at: u64,
        length: usize,
        mut piece: impl FnMut(&mut Os<'_>, u32, usize, usize) -> Result<(), Fail>,
    ) -> Result<(), Fail> {
        let mut done = 0;
        while done < length {
            let take = (length - done).min(TRANSFER as usize);
            let offset = u32::try_from(at + done as u64).unwrap_or(u32::MAX);
            if reason == 1 {
                piece(self.os, self.buffer, done, take)?;
            }
            let mut regs = Regs::default();
            regs.r[..5].copy_from_slice(&[reason, self.handle, self.buffer, len32(take), offset]);
            self.os
                .perform(X | tairix_riscos_kernel::swi::os::GBPB, &mut regs)?;
            if reason == 3 {
                piece(self.os, self.buffer, done, take)?;
            }
            done += take;
        }
        Ok(())
    }
}

impl Disc for ImageDisc<'_, '_> {
    type Error = Fail;

    fn capacity(&self) -> u64 {
        self.capacity
    }

    fn read(&mut self, at: u64, out: &mut [u8]) -> Result<(), Fail> {
        let length = out.len();
        self.gbpb(3, at, length, |os, frame, done, take| {
            os.read_bytes(frame, &mut out[done..done + take])
        })
    }

    fn write(&mut self, at: u64, bytes: &[u8]) -> Result<(), Fail> {
        self.gbpb(1, at, bytes.len(), |os, frame, done, take| {
            os.write_bytes(frame, &bytes[done..done + take])
        })
    }

    fn zero(&mut self, at: u64, length: u64) -> Result<(), Fail> {
        let length = usize::try_from(length).unwrap_or(usize::MAX);
        self.gbpb(1, at, length, |os, frame, _, take| {
            os.fill(frame, len32(take), 0)
        })
    }

    fn flush(&mut self) -> Result<(), Fail> {
        Ok(())
    }
}
