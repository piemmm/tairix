//! FileCore's SWIs: an instance made for a client, and each of a client's
//! discs reached through it, by the private word `FileCore_Create` answered.

use tairix_filecore::format::{format_volume, lay_out, BootRecord, Variant};
use tairix_filecore::record::{DiscRecord, DISC_RECORD_SIZE, LOG2SECSIZE, NAME_LEN};
use tairix_filecore::volume::Backing;
use tairix_filecore::Disc;
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs, Target};
use tairix_riscos_kernel::swi::os::MODULE;
use tairix_riscos_kernel::swi::X;

use super::disc::{enter_client, op, with_volume};
use super::names;
use super::{
    capped, code, described_at, fail, handed, record_at, space_of, split64, with, Client, DRIVES,
    FIRST_HARD_DISC, RECORD_ROOM,
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
                offset::LAYOUT_STRUCTURE => layout_structure(os, &client, regs),
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
    let number = os.read_byte(descriptor + 3)?;
    let at = |offset: u32| base.wrapping_add(offset);
    let client = Client {
        flags,
        number,
        title: at(os.read_word(descriptor + 4)?),
        boot_text: at(os.read_word(descriptor + 8)?),
        disc_op: Target {
            address: at(os.read_word(descriptor + 12)?),
            bits,
        },
        misc_op: Target {
            address: at(os.read_word(descriptor + 16)?),
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
    let low = os.read_word(block + 4)?;
    let high = os.read_word(block + 8)?;
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
    os.write_word(block + 4, low)?;
    os.write_word(block + 8, high)?;
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
/// R1 — its map and root directory, laid out as on the disc — in R0's
/// buffer; R2's bit 6 an old map, bit 7 old directories too; R3 how much of
/// the buffer they fill. A floppy whose map lies in its middle, past the
/// buffer's reach — the F class — is refused.
fn floppy_structure(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let record = record_read(os, regs.r[1])?;
    let old_map = regs.r[2] & 1 << 6 != 0;
    let old_directories = regs.r[2] & 1 << 7 != 0;
    let variant = match (old_map, old_directories) {
        (true, true) => match record.disc_size {
            0x2_8000 => Some(Variant::S),
            0x5_0000 => Some(Variant::M),
            _ => Some(Variant::L),
        },
        (true, false) => Some(Variant::D),
        (false, _) if record.nzones == 1 => None,
        (false, _) => return Err(fail(os, None, code::BAD_PARMS)),
    };
    let capacity = variant
        .and_then(Variant::fixed_size)
        .unwrap_or(record.disc_size);
    let mut buffer = GuestDisc {
        os,
        base: regs.r[0],
        reach: u64::from(STRUCTURE_REACH),
        capacity,
        end: 0,
    };
    let laid = match variant {
        Some(variant) => format_volume(&mut buffer, variant).map(|()| buffer.end),
        None => lay_out(&mut buffer, record, BootRecord::Whole).map(|_| buffer.end),
    };
    let os = buffer.os;
    match laid {
        Ok(end) => {
            regs.r[3] = u32::try_from(end).unwrap_or(STRUCTURE_REACH);
            Ok(())
        }
        Err(tairix_filecore::Error::Disc(fail)) => Err(fail),
        Err(_) => Err(fail(os, None, code::BAD_PARMS)),
    }
}

/// The buffer `FileCore_FloppyStructure` lays a floppy's start out in.
const STRUCTURE_REACH: u32 = 4096;

/// `FileCore_DiscFormat`: R0's block filled with format R3's ideal geometry
/// — the L, D, E or F floppy — then handed to the vetting SWI R1 with R2 in
/// its R1, which may change it.
fn disc_format(os: &mut Os<'_>, regs: &Regs) -> Result<(), Fail> {
    let (sector, per_track, density): (u32, u8, u8) = match regs.r[3] {
        0x80 => (256, 16, 2),
        0x81 | 0x82 => (1024, 5, 2),
        0x83 => (1024, 10, 4),
        _ => return Err(fail(os, None, code::BAD_PARMS)),
    };
    let block = regs.r[0];
    os.fill(block, 64, 0)?;
    os.write_word(block, sector)?;
    os.write_byte(block + 16, per_track)?;
    os.write_byte(block + 17, density)?;
    // Both sides, sequenced by side as ADFS lays its floppies out.
    os.write_byte(block + 18, 3 << 2)?;
    os.write_byte(block + 23, 0xE5)?;
    os.write_word(block + 24, 80)?;
    let mut vet = Regs::default();
    vet.r[0] = block;
    vet.r[1] = regs.r[2];
    os.perform(X | regs.r[1], &mut vet)
}

/// `FileCore_LayoutStructure`: the empty disc the record at R0 describes,
/// named R2, laid out in the image file open on R3.
fn layout_structure(os: &mut Os<'_>, client: &Client, regs: &Regs) -> Result<(), Fail> {
    let mut record = record_read(os, regs.r[0])?;
    let (name, _) = os.read_until(regs.r[2], NAME_LEN + 1, |b| b <= b' ')?;
    let mut padded = [b' '; NAME_LEN];
    padded[..name.len().min(NAME_LEN)].copy_from_slice(&name[..name.len().min(NAME_LEN)]);
    record.disc_name = padded;
    let mut image = ImageDisc {
        os,
        handle: regs.r[3],
        capacity: record.disc_size,
    };
    let laid = lay_out(&mut image, record, BootRecord::Whole);
    let os = image.os;
    match laid {
        Ok(_) => Ok(()),
        Err(error) => Err(super::refused(os, client, error)),
    }
}

/// The first `reach` bytes of a disc of `capacity` bytes, held in the
/// guest's memory from `base`: what is written past them is refused, what is
/// read there is zero. How far the writes went is `end`.
struct GuestDisc<'o, 'w> {
    os: &'o mut Os<'w>,
    base: u32,
    reach: u64,
    capacity: u64,
    end: u64,
}

impl GuestDisc<'_, '_> {
    /// `length` bytes at `at`, inside the reach.
    fn within(&mut self, at: u64, length: u64) -> Result<u32, Fail> {
        match at.checked_add(length).filter(|&end| end <= self.reach) {
            Some(end) => {
                self.end = self.end.max(end);
                Ok(self
                    .base
                    .wrapping_add(u32::try_from(at).unwrap_or(u32::MAX)))
            }
            None => Err(self.os.error(tairix_riscos_kernel::error::BAD_PARAMETERS)),
        }
    }
}

impl Disc for GuestDisc<'_, '_> {
    type Error = Fail;

    fn capacity(&self) -> u64 {
        self.capacity
    }

    fn read(&mut self, at: u64, out: &mut [u8]) -> Result<(), Fail> {
        if at >= self.reach {
            out.fill(0);
            return Ok(());
        }
        let end = self.end;
        let from = self.within(at, out.len() as u64)?;
        self.end = end;
        self.os.read_bytes(from, out)
    }

    fn write(&mut self, at: u64, bytes: &[u8]) -> Result<(), Fail> {
        let to = self.within(at, bytes.len() as u64)?;
        self.os.write_bytes(to, bytes)
    }

    fn zero(&mut self, at: u64, length: u64) -> Result<(), Fail> {
        let to = self.within(at, length)?;
        self.os
            .fill(to, u32::try_from(length).unwrap_or(u32::MAX), 0)
    }

    fn flush(&mut self) -> Result<(), Fail> {
        Ok(())
    }
}

/// An image file open on `handle` as a disc, through `OS_GBPB`.
struct ImageDisc<'o, 'w> {
    os: &'o mut Os<'w>,
    handle: u32,
    capacity: u64,
}

impl ImageDisc<'_, '_> {
    /// `length` bytes moved at `at` through the instance-free scratch below
    /// the supervisor stack, a piece at a time.
    fn gbpb(
        &mut self,
        reason: u32,
        at: u64,
        length: usize,
        mut piece: impl FnMut(&mut Os<'_>, u32, usize, usize) -> Result<(), Fail>,
    ) -> Result<(), Fail> {
        const PIECE: usize = 1024;
        let handle = self.handle;
        let mut done = 0;
        while done < length {
            let take = (length - done).min(PIECE);
            let offset = u32::try_from(at + done as u64).unwrap_or(u32::MAX);
            self.os.below_supervisor_stack(len32(PIECE), |os, frame| {
                if reason == 1 {
                    piece(os, frame, done, take)?;
                }
                let mut regs = Regs::default();
                regs.r[..5].copy_from_slice(&[reason, handle, frame, len32(take), offset]);
                os.perform(X | tairix_riscos_kernel::swi::os::GBPB, &mut regs)?;
                if reason == 3 {
                    piece(os, frame, done, take)?;
                }
                Ok(())
            })?;
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
