//! The kernel's network redirector: the drives the machine's shared folders
//! are (`crate::folder`, `plans/PCEMU.md` PC16.7).
//!
//! A shared drive's CDS is marked a network drive's, with no DPB, and holds
//! `X:\` and its current directory as a local drive's does, so a path on it
//! is made canonical as any is. Each call on it asks the board's folder at
//! once — the machine stopped until the front-end answers — so a routine
//! reaches it in one step, with no guest code. An open file of a share has an
//! SFT entry marked remote, the share's handle where a local file's directory
//! sector lies; once the share has gone, the handle is cleared and every call
//! through it answers that the network name was deleted.

use super::data::{cds, driver, sft, sysvars, SEGMENT, SYSVARS};
use super::dirops::{dta, finding};
use super::engine::{Far, Fault, Kernel};
use super::file::{access, info};
use super::fs::{cds_at, error};
use super::name::{self, Canonical, NameError};
use crate::bios::guest::Guest;
use crate::folder::{
    wanted, Access, Creation, DosName, Facts, From, Op, Refusal, Reply, SharePath, IO_MOST,
};
use crate::machine::Machine;
use tairix_abi::time::{DosTime, Time64};
use tairix_fat::dir::attr;

/// The drive byte a search of a shared drive keeps — the DTA's first, an
/// FCB's search drive: the drive, from `1` for `A:`, under this bit.
pub const REMOTE_SEARCH: u8 = 0x80;

/// Where in the DTA a search of a shared drive keeps its place, as
/// [`put_place`] writes it.
const LISTING: u16 = 0x0D;
/// The place of a search that found the root's label and has not begun the
/// listing.
pub const LABEL_GIVEN: u32 = u32::MAX;

/// What a refusal is to DOS.
#[must_use]
pub const fn code(refusal: Refusal) -> u16 {
    match refusal {
        Refusal::NotFound => error::FILE_NOT_FOUND,
        Refusal::PathNotFound => error::PATH_NOT_FOUND,
        Refusal::Denied | Refusal::NotEmpty | Refusal::Full => error::ACCESS_DENIED,
        Refusal::Exists => error::FILE_EXISTS,
        Refusal::NoMore => error::NO_MORE_FILES,
        Refusal::TooMany => error::TOO_MANY_OPEN,
        Refusal::Handle => error::INVALID_HANDLE,
        Refusal::Fault => error::NETWORK_ERROR,
        Refusal::Gone => error::NAME_DELETED,
    }
}

/// The fault a refusal is.
const fn refused(refusal: Refusal) -> Fault {
    Fault::Error(code(refusal))
}

/// A reply that is not one to what was asked.
const MISANSWERED: Fault = Fault::Error(error::NETWORK_ERROR);

/// The sector a share's space is counted in.
const SECTOR: u64 = crate::storage::SECTOR as u64;

/// The attribute byte DOS shows for `facts`: what the host keeps of it.
#[must_use]
pub fn attributes(facts: &Facts) -> u8 {
    let mut bits = 0;
    if facts.directory {
        bits |= attr::DIRECTORY;
    } else {
        bits |= attr::ARCHIVE;
    }
    if !facts.writable {
        bits |= attr::READ_ONLY;
    }
    if facts.hidden {
        bits |= attr::HIDDEN;
    }
    bits
}

/// The DOS stamp of an instant on the machine's clock, the nearest DOS holds
/// for one outside its years.
#[must_use]
pub fn stamp(at: Time64) -> DosTime {
    let held = at.clamp(DosTime::EARLIEST, DosTime::LATEST);
    DosTime::from_time64(held).unwrap_or(DosTime {
        date: 0x21,
        time: 0,
    })
}

impl Kernel<'_, '_> {
    /// Whether drive `drive`'s CDS marks it a network drive's.
    pub fn is_network(&mut self, drive: u8) -> bool {
        drive < super::data::DRIVES && self.get16(cds_at(drive) + cds::FLAGS) & cds::NETWORK != 0
    }

    /// The slot of the share drive `drive` is.
    pub fn share_of(&mut self, drive: u8) -> Option<u8> {
        if !self.is_network(drive) {
            return None;
        }
        self.g.machine().board().shared_drive(drive)
    }

    /// Whether drive `drive` is one a call may name: a disk's, with its DPB,
    /// or a share's.
    pub fn is_drive(&mut self, drive: u8) -> bool {
        self.dpb(drive).is_some() || self.share_of(drive).is_some()
    }

    /// The share and its handle an SFT entry marked remote is open through:
    /// `None` for an entry that is not; a share gone is the name deleted.
    pub fn sft_remote(&mut self, entry: Far) -> Option<Result<(u8, u32), Fault>> {
        let word = self.sft16(entry, sft::INFO);
        if word & info::REMOTE == 0 || word & info::DEVICE != 0 {
            return None;
        }
        let drive = u8::try_from(word & info::DRIVE).unwrap_or(0);
        let handle = self.sft32(entry, sft::DIR_SECTOR);
        let slot = self.share_of(drive).filter(|_| handle != 0);
        Some(
            slot.map(|slot| (slot, handle))
                .ok_or(Fault::Error(error::NAME_DELETED)),
        )
    }

    /// Ask share `slot` `op`; `take` makes what is wanted of the reply, `None`
    /// for one not answering the question.
    fn ask<T>(
        &mut self,
        slot: u8,
        op: &Op<'_>,
        take: impl FnOnce(Reply<'_>) -> Option<T>,
    ) -> Result<T, Fault> {
        let share = self
            .g
            .machine()
            .board_mut()
            .share_mut(slot)
            .ok_or(Fault::Error(error::NAME_DELETED))?;
        let reply = share.folder.call(op).map_err(refused)?;
        take(reply).ok_or(MISANSWERED)
    }

    /// Ask share `slot` `op`, wanting it done.
    fn ask_done(&mut self, slot: u8, op: &Op<'_>) -> Result<(), Fault> {
        self.ask(slot, op, |reply| matches!(reply, Reply::Done).then_some(()))
    }

    /// What share `slot` says `path` names.
    fn look(&mut self, slot: u8, path: SharePath) -> Result<Facts, Fault> {
        self.ask(slot, &Op::Look { path }, |reply| match reply {
            Reply::Looked(facts) => Some(facts),
            _ => None,
        })
    }
}

/// A path on a shared drive.
#[derive(Clone, Copy, Debug)]
pub struct Remote {
    /// The drive, `0` for `A:`.
    pub drive: u8,
    /// The share it is.
    pub slot: u8,
    /// The directory its last name lies in.
    pub dir: SharePath,
    /// Its last name — a pattern where the call allows one — or `None` for
    /// the root.
    pub last: Option<DosName>,
}

impl Remote {
    /// The node it names, which no pattern may.
    ///
    /// # Errors
    ///
    /// `file not found` for a pattern.
    pub fn path(&self) -> Result<SharePath, Fault> {
        match self.last {
            None => Ok(SharePath::ROOT),
            Some(last) if last.is_wild() => Err(Fault::Error(error::FILE_NOT_FOUND)),
            Some(last) => self
                .dir
                .child(last)
                .ok_or(Fault::Error(error::PATH_NOT_FOUND)),
        }
    }
}

/// `canonical` on a shared drive, if it is on one: its last name a pattern
/// where `wild`.
///
/// # Errors
///
/// `invalid drive` for a network drive no share is; `path not found` for a
/// name no share path holds, `file not found` for a last one.
pub fn remote(
    k: &mut Kernel<'_, '_>,
    canonical: &Canonical,
    wild: bool,
) -> Result<Option<Remote>, Fault> {
    let drive = canonical.drive();
    if !k.is_network(drive) {
        return Ok(None);
    }
    let slot = k
        .share_of(drive)
        .ok_or(Fault::Error(error::INVALID_DRIVE))?;
    let unfound = Fault::Error(error::PATH_NOT_FOUND);
    let mut dir = SharePath::ROOT;
    let mut last = None;
    for component in canonical.components() {
        if let Some(parent) = last.replace(component) {
            dir = dir
                .child(dos_name(parent, false).ok_or(unfound)?)
                .ok_or(unfound)?;
        }
    }
    let last = match last {
        Some(last) => Some(dos_name(last, wild).ok_or(Fault::Error(error::FILE_NOT_FOUND))?),
        None => None,
    };
    Ok(Some(Remote {
        drive,
        slot,
        dir,
        last,
    }))
}

/// The running call's path `which` on a shared drive, if it is on one: made
/// canonical and written back so. A path on any other drive is left as it
/// is, read no further than its drive letter.
///
/// # Errors
///
/// As [`remote`], and the canonical form's refusal.
pub fn remote_path(
    k: &mut Kernel<'_, '_>,
    which: u16,
    wild: bool,
) -> Result<Option<Remote>, Fault> {
    let (path, len) = k.path(which);
    let current = k.current_drive();
    let (drive, _) = name::drive_of(&path[..len], current);
    if !k.is_network(drive) {
        return Ok(None);
    }
    let canonical = k
        .canonical(&path[..len])
        .map_err(|refusal| Fault::Error(super::dir::name_error(refusal)))?;
    let found = remote(k, &canonical, wild)?;
    if found.is_some() {
        k.set_path(which, canonical.as_bytes());
    }
    Ok(found)
}

/// The call, where the running call's path `which` is on a shared drive:
/// `serve`'s answer; `None` to go on with it on a drive of the machine's.
pub fn served(
    k: &mut Kernel<'_, '_>,
    which: u16,
    wild: bool,
    serve: impl FnOnce(&mut Kernel<'_, '_>, &Remote) -> Result<u32, Fault>,
) -> Option<super::engine::Flow> {
    match remote_path(k, which, wild) {
        Ok(Some(remote)) => Some(super::engine::Flow::Return(serve(k, &remote))),
        Ok(None) => None,
        Err(fault) => Some(super::engine::Flow::Return(Err(fault))),
    }
}

/// The FCB name `field` in drive `drive`'s current directory, where the drive
/// is a share's: a pattern where `wild`.
///
/// # Errors
///
/// `path not found` for a current directory no share path holds; `file not
/// found` for a name none may.
pub fn in_current(
    k: &mut Kernel<'_, '_>,
    drive: u8,
    field: [u8; 11],
    wild: bool,
) -> Result<Option<Remote>, Fault> {
    let Some(slot) = k.share_of(drive) else {
        return Ok(None);
    };
    let canonical = k.canonical(&[b'A' + drive, b':']).map_err(name_refusal)?;
    let here = remote(k, &canonical, false)?.ok_or(Fault::Error(error::INVALID_DRIVE))?;
    let last = DosName::new(field, wild)
        .filter(|name| !name.is_dots())
        .ok_or(Fault::Error(error::FILE_NOT_FOUND))?;
    Ok(Some(Remote {
        drive,
        slot,
        dir: here.path()?,
        last: Some(last),
    }))
}

/// `component` as a share path's name: a pattern only where `wild`.
fn dos_name(component: &[u8], wild: bool) -> Option<DosName> {
    let field = name::field(component, wild).ok()?;
    DosName::new(field, wild).filter(|name| !name.is_dots())
}

/// The error a refused name is, as DOS reports it.
#[must_use]
pub const fn name_refusal(refusal: NameError) -> Fault {
    Fault::Error(super::dir::name_error(refusal))
}

// The drives: given at boot, and as a share comes and goes.

/// Whether the boot has given the shares their drives, so a share attached
/// later takes one at once.
fn drives_given(k: &mut Kernel<'_, '_>) -> bool {
    k.get8(driver::SHARES_GIVEN) != 0
}

/// Give each share without a drive the first letter no drive uses, within
/// `LASTDRIVE`; one finding none stays without.
pub fn assign(k: &mut Kernel<'_, '_>) {
    k.set8(driver::SHARES_GIVEN, 1);
    let at = u16::try_from(i32::from(SYSVARS) + i32::from(sysvars::LASTDRIVE)).unwrap_or(0);
    let drives = k.get8(at).min(super::data::DRIVES);
    let mut slots = [false; crate::folder::SHARES];
    for (slot, share) in k.g.machine().board_mut().shares_mut() {
        slots[usize::from(slot)] = share.drive.is_none();
    }
    for (slot, wanting) in (0u8..).zip(slots) {
        if !wanting {
            continue;
        }
        let free = (0..drives).find(|&drive| {
            let flags = k.get16(cds_at(drive) + cds::FLAGS);
            let taken = flags & cds::PHYSICAL != 0
                || (flags & cds::NETWORK != 0
                    && k.g.machine().board().shared_drive(drive).is_some());
            !taken
        });
        let Some(drive) = free else {
            continue;
        };
        let entry = cds_at(drive);
        let mut root = [0u8; cds::PATH_LEN as usize];
        root[..3].copy_from_slice(&[b'A' + drive, b':', b'\\']);
        k.g.write_bytes(SEGMENT, entry + cds::PATH, &root);
        k.set16(entry + cds::FLAGS, cds::NETWORK);
        k.set_far(entry + cds::DPB, Far::default());
        k.set16(entry + cds::ROOT, 2);
        if let Some(share) = k.g.machine().board_mut().share_mut(slot) {
            share.drive = Some(drive);
            share.announced = false;
        }
    }
}

/// A boot is beginning: no share has a drive until its drivers have taken
/// theirs.
pub fn take_back(k: &mut Kernel<'_, '_>) {
    k.set8(driver::SHARES_GIVEN, 0);
    for (_, share) in k.g.machine().board_mut().shares_mut() {
        share.lose_drive();
    }
}

/// A share was attached: once the boot gave the shares their drives, it
/// takes one at once.
pub fn attach(machine: &mut Machine) {
    let mut guest = Guest::new(machine);
    let mut k = Kernel::new(&mut guest);
    if drives_given(&mut k) {
        assign(&mut k);
    }
}

/// Share `slot` is going: its drive is no longer one, and every file open on
/// it is cut off.
pub fn detach(machine: &mut Machine, slot: u8) {
    let drive = machine
        .board_mut()
        .share_mut(slot)
        .and_then(|share| share.drive);
    let mut guest = Guest::new(machine);
    let mut k = Kernel::new(&mut guest);
    if let Some(drive) = drive.filter(|_| drives_given(&mut k)) {
        release(&mut k, drive);
    }
}

/// Every share is gone — the machine was taken back from a snapshot, which
/// keeps none: each network drive goes and each remote file is cut off.
pub fn forget(machine: &mut Machine) {
    let mut guest = Guest::new(machine);
    let mut k = Kernel::new(&mut guest);
    for drive in 0..super::data::DRIVES {
        if k.is_network(drive) && k.share_of(drive).is_none() {
            release(&mut k, drive);
        }
    }
}

/// Drive `drive` is no longer a share's: its CDS is free again, and every
/// SFT entry open on it loses its handle.
fn release(k: &mut Kernel<'_, '_>, drive: u8) {
    let entry = cds_at(drive);
    k.set16(entry + cds::FLAGS, 0);
    // The handles' table, then the FCBs' own, each walked to its end.
    for first in [0, super::file::FCB_FILES] {
        for index in first..first + super::file::FCB_FILES {
            let Some(at) = k.sft(index) else {
                break;
            };
            let word = k.sft16(at, sft::INFO);
            let remote = word & info::REMOTE != 0 && word & info::DEVICE == 0;
            if remote && u8::try_from(word & info::DRIVE).ok() == Some(drive) {
                k.set_sft32(at, sft::DIR_SECTOR, 0);
            }
        }
    }
}

/// The share whose drive changed since the front-end was last told: its
/// slot and drive, marked told.
pub fn unannounced(machine: &mut Machine) -> Option<(u8, Option<u8>)> {
    machine.board_mut().shares_mut().find_map(|(slot, share)| {
        if share.announced {
            return None;
        }
        share.announced = true;
        Some((slot, share.drive))
    })
}

// The calls.

/// Open what `remote` names in SFT entry `entry` with `mode` (function
/// `3Dh`'s AL), as `creation` asks; answers what it opened.
///
/// # Errors
///
/// The share's refusal as DOS reports it.
pub fn open(
    k: &mut Kernel<'_, '_>,
    remote: &Remote,
    mode: u8,
    creation: Creation,
    entry: Far,
) -> Result<Facts, Fault> {
    let path = remote.path()?;
    let access = match mode & access::MASK {
        access::READ => Access::Read,
        access::WRITE => Access::Write,
        _ => Access::Both,
    };
    let (handle, facts) = k.ask(
        remote.slot,
        &Op::Open {
            path,
            access,
            creation,
        },
        |reply| match reply {
            Reply::Opened { handle, facts, .. } => Some((handle, facts)),
            _ => None,
        },
    )?;
    let at = stamp(facts.modified);
    k.set_sft16(entry, sft::HANDLES, 1);
    k.set_sft16(entry, sft::MODE, u16::from(mode));
    k.set_sft8(entry, sft::ATTRIBUTE, attributes(&facts));
    k.set_sft16(
        entry,
        sft::INFO,
        info::REMOTE | info::CLEAN | u16::from(remote.drive),
    );
    k.set_sft16(entry, sft::DEVICE, cds_at(remote.drive));
    k.set_sft16(entry, sft::DEVICE + 2, SEGMENT);
    k.set_sft16(entry, sft::FIRST, 0);
    k.set_sft16(entry, sft::TIME, at.time);
    k.set_sft16(entry, sft::DATE, at.date);
    k.set_sft32(entry, sft::SIZE, facts.size);
    k.set_sft32(entry, sft::POSITION, 0);
    k.set_sft16(entry, sft::RELATIVE, 0);
    k.set_sft16(entry, sft::CLUSTER, 0);
    k.set_sft32(entry, sft::DIR_SECTOR, handle);
    k.set_sft8(entry, sft::DIR_INDEX, 0);
    let place = entry.add(sft::NAME);
    k.g.write_bytes(place.segment, place.offset, facts.name.bytes());
    let owner = k.psp();
    k.set_sft16(entry, sft::OWNER, owner);
    Ok(facts)
}

/// Read up to `count` bytes from the remote file of SFT entry `entry` at its
/// position into `buffer`; answers the bytes read.
///
/// # Errors
///
/// `access denied` for an entry opened to write alone; the share's refusal.
pub fn read(k: &mut Kernel<'_, '_>, entry: Far, buffer: Far, count: u16) -> Result<u16, Fault> {
    let (slot, handle) = k.sft_remote(entry).ok_or(MISANSWERED)??;
    if k.sft_access(entry) == access::WRITE {
        return Err(Fault::Error(error::ACCESS_DENIED));
    }
    let mut done: u16 = 0;
    while done < count {
        let position = k.sft32(entry, sft::POSITION);
        let want = usize::from(count - done).min(IO_MOST);
        let len = u32::try_from(want).map_err(|_| MISANSWERED)?;
        let mut scratch = take_scratch(k, slot);
        let read = k.ask(
            slot,
            &Op::Read {
                handle,
                offset: position,
                len,
            },
            |reply| match reply {
                Reply::Read(bytes) if bytes.len() <= want => {
                    scratch.clear();
                    scratch.extend_from_slice(bytes);
                    Some(())
                }
                _ => None,
            },
        );
        let got = scratch.len();
        let at = buffer.add(done);
        if read.is_ok() {
            k.g.write_bytes(at.segment, at.offset, &scratch);
        }
        give_scratch(k, slot, scratch);
        read?;
        let got16 = u16::try_from(got).map_err(|_| MISANSWERED)?;
        done += got16;
        k.set_sft32(
            entry,
            sft::POSITION,
            position.wrapping_add(u32::from(got16)),
        );
        if got < want {
            break;
        }
    }
    Ok(done)
}

/// Write `count` bytes from `buffer` to the remote file of SFT entry `entry`
/// at its position, or with none end the file there; answers the bytes
/// written, fewer once the volume is full.
///
/// # Errors
///
/// `access denied` for an entry opened to read alone; the share's refusal.
pub fn write(k: &mut Kernel<'_, '_>, entry: Far, buffer: Far, count: u16) -> Result<u16, Fault> {
    let (slot, handle) = k.sft_remote(entry).ok_or(MISANSWERED)??;
    if k.sft_access(entry) == access::READ {
        return Err(Fault::Error(error::ACCESS_DENIED));
    }
    let word = k.sft16(entry, sft::INFO);
    k.set_sft16(entry, sft::INFO, word & !info::CLEAN);
    if count == 0 {
        let size = k.sft32(entry, sft::POSITION);
        k.ask_done(slot, &Op::Resize { handle, size })?;
        k.set_sft32(entry, sft::SIZE, size);
        return Ok(0);
    }
    let mut done: u16 = 0;
    let mut scratch = take_scratch(k, slot);
    let written = loop {
        if done == count {
            break Ok(());
        }
        let position = k.sft32(entry, sft::POSITION);
        let len = usize::from(count - done).min(IO_MOST);
        scratch.clear();
        if scratch.try_reserve(len).is_err() {
            break Err(MISANSWERED);
        }
        scratch.resize(len, 0);
        let from = buffer.add(done);
        k.g.read_bytes(from.segment, from.offset, &mut scratch);
        let wrote = k.ask(
            slot,
            &Op::Write {
                handle,
                offset: position,
                bytes: &scratch,
            },
            |reply| match reply {
                Reply::Wrote(count) if usize::try_from(count).is_ok_and(|count| count <= len) => {
                    u16::try_from(count).ok()
                }
                _ => None,
            },
        );
        let wrote = match wrote {
            Ok(wrote) => wrote,
            Err(fault) => break Err(fault),
        };
        done += wrote;
        let position = position.wrapping_add(u32::from(wrote));
        k.set_sft32(entry, sft::POSITION, position);
        if k.sft32(entry, sft::SIZE) < position {
            k.set_sft32(entry, sft::SIZE, position);
        }
        if usize::from(wrote) < len {
            break Ok(());
        }
    };
    give_scratch(k, slot, scratch);
    written.map(|()| done)
}

/// Close the remote file of SFT entry `entry`, stamping it first with the
/// date and time function `57h` set.
///
/// # Errors
///
/// The share's refusal; the file is closed either way.
pub fn close(k: &mut Kernel<'_, '_>, entry: Far) -> Result<(), Fault> {
    // A file of a share gone is closed already.
    let Ok((slot, handle)) = k.sft_remote(entry).ok_or(MISANSWERED)? else {
        return Ok(());
    };
    let stamped = k.sft16(entry, sft::INFO) & info::STAMPED != 0;
    let stamp = stamped
        .then(|| {
            DosTime {
                date: k.sft16(entry, sft::DATE),
                time: k.sft16(entry, sft::TIME),
            }
            .to_time64()
        })
        .flatten();
    k.set_sft32(entry, sft::DIR_SECTOR, 0);
    k.ask_done(slot, &Op::Close { handle, stamp })
}

/// Put what was written to the remote file of SFT entry `entry` on its
/// medium.
///
/// # Errors
///
/// The share's refusal.
pub fn commit(k: &mut Kernel<'_, '_>, entry: Far) -> Result<(), Fault> {
    let (slot, handle) = k.sft_remote(entry).ok_or(MISANSWERED)??;
    k.ask_done(slot, &Op::Commit { handle })
}

/// How long the remote file of SFT entry `entry` is now, which a seek from
/// its end goes by; the entry is told it.
///
/// # Errors
///
/// The share's refusal.
pub fn size(k: &mut Kernel<'_, '_>, entry: Far) -> Result<u32, Fault> {
    let (slot, handle) = k.sft_remote(entry).ok_or(MISANSWERED)??;
    let size = k.ask(slot, &Op::Size { handle }, |reply| match reply {
        Reply::Size(size) => Some(size),
        _ => None,
    })?;
    k.set_sft32(entry, sft::SIZE, size);
    Ok(size)
}

/// Whether the share of a remote SFT entry may be changed: its date and time
/// are kept on it at close only where it may.
#[must_use]
pub fn writable(k: &mut Kernel<'_, '_>, entry: Far) -> bool {
    let Some(Ok((slot, _))) = k.sft_remote(entry) else {
        return false;
    };
    k.g.machine()
        .board_mut()
        .share_mut(slot)
        .is_some_and(|share| share.writable)
}

/// Function `39h` on a shared drive.
///
/// # Errors
///
/// The share's refusal.
pub fn make_dir(k: &mut Kernel<'_, '_>, remote: &Remote) -> Result<(), Fault> {
    let path = remote.path()?;
    if path.is_root() {
        return Err(Fault::Error(error::ACCESS_DENIED));
    }
    k.ask_done(remote.slot, &Op::MakeDir { path })
}

/// Function `3Ah` on a shared drive: never the drive's current directory.
///
/// # Errors
///
/// `attempt to remove the current directory`; the share's refusal.
pub fn remove_dir(k: &mut Kernel<'_, '_>, remote: &Remote) -> Result<(), Fault> {
    let path = remote.path()?;
    if path.is_root() {
        return Err(Fault::Error(error::ACCESS_DENIED));
    }
    let (path_now, len) = k.path(super::data::level::PATH);
    let (current, current_len) = k.current_directory(remote.drive);
    if path_now[..len].eq_ignore_ascii_case(&current[..current_len]) {
        return Err(Fault::Error(error::CURRENT_DIRECTORY));
    }
    k.ask_done(
        remote.slot,
        &Op::Remove {
            path,
            directory: true,
        },
    )
}

/// Function `3Bh` on a shared drive: the directory made current where the
/// share has one there.
///
/// # Errors
///
/// `path not found`; the share's refusal.
pub fn change_dir(k: &mut Kernel<'_, '_>, remote: &Remote) -> Result<(), Fault> {
    let path = remote.path()?;
    if !path.is_root() {
        let facts = k.look(remote.slot, path).map_err(|fault| match fault {
            Fault::Error(error::FILE_NOT_FOUND) => Fault::Error(error::PATH_NOT_FOUND),
            other => other,
        })?;
        if !facts.directory {
            return Err(Fault::Error(error::PATH_NOT_FOUND));
        }
    }
    let (canonical, len) = k.path(super::data::level::PATH);
    let entry = cds_at(remote.drive);
    let mut text = [0u8; cds::PATH_LEN as usize];
    let len = len.min(text.len() - 1);
    text[..len].copy_from_slice(&canonical[..len]);
    k.g.write_bytes(SEGMENT, entry + cds::PATH, &text);
    Ok(())
}

/// Function `41h` on a shared drive: a read-only file is kept, as DOS keeps
/// one.
///
/// # Errors
///
/// `access denied`; the share's refusal.
pub fn delete(k: &mut Kernel<'_, '_>, remote: &Remote) -> Result<(), Fault> {
    let facts = look_up(k, remote)?;
    if facts.directory || !facts.writable {
        return Err(Fault::Error(error::ACCESS_DENIED));
    }
    remove_file(k, remote)
}

/// Remove the file `remote` names, which a search found DOS may delete.
///
/// # Errors
///
/// The share's refusal.
pub fn remove_file(k: &mut Kernel<'_, '_>, remote: &Remote) -> Result<(), Fault> {
    let path = remote.path()?;
    k.ask_done(
        remote.slot,
        &Op::Remove {
            path,
            directory: false,
        },
    )
}

/// What the share says `remote` names.
///
/// # Errors
///
/// `file not found` for a pattern; the share's refusal.
pub fn look_up(k: &mut Kernel<'_, '_>, remote: &Remote) -> Result<Facts, Fault> {
    let path = remote.path()?;
    k.look(remote.slot, path)
}

/// Function `43h` on a shared drive: the attributes read, or set where the
/// share keeps them — the archive bit DOS sets and clears freely, the rest
/// only as the host already has them.
///
/// # Errors
///
/// `access denied` for a change the host does not keep; the share's refusal.
pub fn attributes_of(
    k: &mut Kernel<'_, '_>,
    remote: &Remote,
    set: Option<u8>,
) -> Result<u8, Fault> {
    let path = remote.path()?;
    if path.is_root() {
        return Err(Fault::Error(error::FILE_NOT_FOUND));
    }
    let facts = k.look(remote.slot, path)?;
    let held = attributes(&facts);
    if let Some(asked) = set {
        let kept =
            attr::READ_ONLY | attr::HIDDEN | attr::SYSTEM | attr::DIRECTORY | attr::VOLUME_ID;
        let wanted = (asked & kept) | (held & attr::DIRECTORY);
        if wanted != held & kept {
            return Err(Fault::Error(error::ACCESS_DENIED));
        }
    }
    Ok(held)
}

/// Function `56h` with both names on shared drives.
///
/// # Errors
///
/// `not same device` for two drives; the share's refusal.
pub fn rename(k: &mut Kernel<'_, '_>, from: &Remote, to: &Remote) -> Result<(), Fault> {
    if from.drive != to.drive {
        return Err(Fault::Error(error::NOT_SAME_DEVICE));
    }
    let (old, new) = (from.path()?, to.path()?);
    if old.is_root() || new.is_root() {
        return Err(Fault::Error(error::ACCESS_DENIED));
    }
    k.ask_done(from.slot, &Op::Rename { from: old, to: new })
}

/// Function `36h` on a shared drive: sectors a cluster, free clusters, bytes
/// a sector and clusters, as DOS's words hold them.
///
/// # Errors
///
/// The share's refusal.
pub fn space(k: &mut Kernel<'_, '_>, slot: u8) -> Result<(u16, u16, u16, u16), Fault> {
    let stats = k.ask(slot, &Op::Space, |reply| match reply {
        Reply::Space(stats) => Some(stats),
        _ => None,
    })?;
    let bytes = u64::from(stats.block_size).max(SECTOR);
    let total = stats.total_blocks.saturating_mul(bytes) / SECTOR;
    let free = stats.avail_blocks.saturating_mul(bytes) / SECTOR;
    // The fewest sectors a cluster that count the volume in DOS's word.
    let mut per_cluster: u64 = 1;
    while total / per_cluster > 0xFFFE && per_cluster < 128 {
        per_cluster *= 2;
    }
    let word = |n: u64| u16::try_from((n / per_cluster).min(0xFFFE)).unwrap_or(0xFFFE);
    Ok((
        u16::try_from(per_cluster).unwrap_or(128),
        word(free),
        u16::try_from(SECTOR).unwrap_or(u16::MAX),
        word(total),
    ))
}

/// Functions `4Eh` on a shared drive: the first entry `remote`'s pattern
/// names with attributes `attributes`, reported in the DTA at `at`.
///
/// # Errors
///
/// `no more files`; the share's refusal.
pub fn find_first(
    k: &mut Kernel<'_, '_>,
    remote: &Remote,
    attributes: u8,
    at: Far,
) -> Result<(), Fault> {
    let pattern = remote.last.ok_or(Fault::Error(error::NO_MORE_FILES))?;
    let mut head = [0u8; finding::ATTRIBUTE as usize];
    head[usize::from(dta::DRIVE)] = REMOTE_SEARCH | (remote.drive + 1);
    head[1..12].copy_from_slice(pattern.bytes());
    head[usize::from(dta::ATTRIBUTES)] = attributes;
    k.g.write_bytes(at.segment, at.offset, &head);
    // A search that finds nothing leaves no place a next one goes on from.
    put_place(k, at.add(LISTING), u32::MAX, 0);
    if attributes & attr::VOLUME_ID != 0 && remote.dir.is_root() {
        let label = label(k, remote.slot)?;
        if pattern.matches(&label.name) {
            put_place(k, at.add(LISTING), 0, LABEL_GIVEN);
            report(k, at, &label, attr::VOLUME_ID);
            return Ok(());
        }
    }
    if attributes == attr::VOLUME_ID {
        return Err(Fault::Error(error::NO_MORE_FILES));
    }
    search(k, remote.slot, at, From::Start { dir: remote.dir })
}

/// Function `4Fh` for a search of a shared drive.
///
/// # Errors
///
/// `no more files`; the share's refusal.
pub fn find_next(k: &mut Kernel<'_, '_>, at: Far) -> Result<(), Fault> {
    let first = k.g.read8(at.segment, at.offset);
    let drive = (first & !REMOTE_SEARCH).wrapping_sub(1);
    let slot = k
        .share_of(drive)
        .ok_or(Fault::Error(error::NO_MORE_FILES))?;
    let attributes =
        k.g.read8(at.segment, at.offset.wrapping_add(dta::ATTRIBUTES));
    let (listing, place) = place_at(k, at.add(LISTING));
    let from = if place == LABEL_GIVEN {
        if attributes == attr::VOLUME_ID {
            return Err(Fault::Error(error::NO_MORE_FILES));
        }
        From::Start {
            dir: SharePath::ROOT,
        }
    } else {
        From::Next {
            table: listing,
            index: place,
        }
    };
    search(k, slot, at, from)
}

/// Whether the DTA at `at` holds a search of a shared drive.
#[must_use]
pub fn is_remote_search(k: &mut Kernel<'_, '_>, at: Far) -> bool {
    k.g.read8(at.segment, at.offset) & REMOTE_SEARCH != 0
}

/// Go on with the search the DTA at `at` holds from `from`.
fn search(k: &mut Kernel<'_, '_>, slot: u8, at: Far, from: From) -> Result<(), Fault> {
    let mut pattern = [0u8; 11];
    let place = at.add(dta::PATTERN);
    k.g.read_bytes(place.segment, place.offset, &mut pattern);
    let pattern = DosName::new(pattern, true).ok_or(Fault::Error(error::NO_MORE_FILES))?;
    let asked =
        k.g.read8(at.segment, at.offset.wrapping_add(dta::ATTRIBUTES));
    let (facts, table, next) = found(k, slot, pattern, asked, from)?;
    put_place(k, at.add(LISTING), table, next);
    report(k, at, &facts, attributes(&facts));
    Ok(())
}

/// The entry share `slot` finds next from `from` that `pattern` matches and
/// the attribute byte `asked` admits — a directory or a hidden entry only
/// where it asks for one — with the listing and the place a search goes on
/// from after it.
///
/// # Errors
///
/// `no more files`; the share's refusal.
pub fn found(
    k: &mut Kernel<'_, '_>,
    slot: u8,
    pattern: DosName,
    asked: u8,
    from: From,
) -> Result<(Facts, u32, u32), Fault> {
    let mut admitted = 0;
    if asked & attr::DIRECTORY != 0 {
        admitted |= wanted::DIRECTORY;
    }
    if asked & attr::HIDDEN != 0 {
        admitted |= wanted::HIDDEN;
    }
    k.ask(
        slot,
        &Op::Find {
            pattern,
            attributes: admitted,
            from,
        },
        |reply| match reply {
            Reply::Found { facts, table, next } => Some((facts, table, next)),
            _ => None,
        },
    )
}

/// The share `slot`'s label, as a search for one finds it.
///
/// # Errors
///
/// The share's refusal.
pub fn label(k: &mut Kernel<'_, '_>, slot: u8) -> Result<Facts, Fault> {
    let root = k.look(slot, SharePath::ROOT)?;
    Ok(Facts {
        directory: false,
        size: 0,
        ..root
    })
}

/// A search's place — its listing, then the place after the entry found —
/// written at `at`.
pub fn put_place(k: &mut Kernel<'_, '_>, at: Far, listing: u32, place: u32) {
    let (low, high) = super::engine::words(listing);
    k.write16_at(at, low);
    k.write16_at(at.add(2), high);
    let (low, high) = super::engine::words(place);
    k.write16_at(at.add(4), low);
    k.write16_at(at.add(6), high);
}

/// The search place [`put_place`] wrote at `at`.
pub fn place_at(k: &mut Kernel<'_, '_>, at: Far) -> (u32, u32) {
    (read32(k, at), read32(k, at.add(4)))
}

fn read32(k: &mut Kernel<'_, '_>, at: Far) -> u32 {
    let low = k.read16_at(at);
    u32::from(low) | u32::from(k.read16_at(at.add(2))) << 16
}

/// Report `facts`, with attribute byte `attributes`, in the DTA at `at`.
fn report(k: &mut Kernel<'_, '_>, at: Far, facts: &Facts, attributes: u8) {
    let mut record = [0u8; (dta::LEN - finding::ATTRIBUTE) as usize];
    record[0] = attributes;
    let stamp = stamp(facts.modified);
    let time = usize::from(finding::TIME - finding::ATTRIBUTE);
    record[time..time + 2].copy_from_slice(&stamp.time.to_le_bytes());
    record[time + 2..time + 4].copy_from_slice(&stamp.date.to_le_bytes());
    record[time + 4..time + 8].copy_from_slice(&facts.size.to_le_bytes());
    let mut spelt = [0u8; 13];
    let len = name::spell(facts.name.bytes(), &mut spelt);
    let from = usize::from(finding::NAME - finding::ATTRIBUTE);
    record[from..from + len].copy_from_slice(&spelt[..len]);
    let place = at.add(finding::ATTRIBUTE);
    k.g.write_bytes(place.segment, place.offset, &record);
}

/// The share's buffer a read or write passes guest bytes through, taken out
/// so guest memory can be reached while it is held.
fn take_scratch(k: &mut Kernel<'_, '_>, slot: u8) -> alloc::vec::Vec<u8> {
    k.g.machine()
        .board_mut()
        .share_mut(slot)
        .map(|share| core::mem::take(&mut share.scratch))
        .unwrap_or_default()
}

fn give_scratch(k: &mut Kernel<'_, '_>, slot: u8, scratch: alloc::vec::Vec<u8>) {
    if let Some(share) = k.g.machine().board_mut().share_mut(slot) {
        share.scratch = scratch;
    }
}

#[cfg(test)]
#[path = "redirect_tests.rs"]
mod tests;
