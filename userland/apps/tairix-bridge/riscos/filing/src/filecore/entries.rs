//! The entries of a filing system of FileCore's, as FileSwitch calls them:
//! objects read, made, changed and deleted, files opened and moved, and the
//! disc's own operations.

use alloc::vec::Vec;

use tairix_filecore::defect::FIRST_END;
use tairix_filecore::dir::Object;
use tairix_filecore::volume::{Directory, Recorded, Volume, EMPTY_OBJECT};
use tairix_filecore::{Error, Fault};
use tairix_fsmeta::preset::acorn;
use tairix_riscos_kernel::filing::filetype::DATA;
use tairix_riscos_kernel::filing::fs::{
    args, attribute, file, file_info, func, kind, open as reasons, opt, Info, Layout,
};
use tairix_riscos_kernel::filing::UNSUPPORTED;
use tairix_riscos_kernel::os::{len32, Fail, Os, Regs};

use super::disc::{with_volume, ClientDisc, Seen};
use super::names::{self, Found, Named};
use super::{capped, code, fail, located, space_of, split64, with_instance, Client, OpenFile};
use crate::fileswitch::file::stamp_now;

/// What a new file is given: read and write for its owner.
const NEW_FILE: u16 = acorn::ATTR_OWNER_READ | acorn::ATTR_OWNER_WRITE;
/// What the root reports: locked.
const ROOT_ATTRIBUTES: u32 = attribute::LOCKED;
/// The type a disc's root reports in its load address: a floppy's where the
/// disc has no boot block, a hard disc's where it has one, as RISC OS 5
/// reports them.
const FLOPPY_DISC: u32 = 0xFCE;
const HARD_DISC: u32 = 0xFCD;
/// The directory an open root is held under: none holds the root's entry,
/// and none lies at address zero.
const ROOT_PARENT: u32 = 0;

/// `object`, an entry of a directory, as FileSwitch reads it.
fn info_of(object: &Object) -> Info {
    Info {
        kind: if object.is_dir() {
            kind::DIRECTORY
        } else {
            kind::FILE
        },
        load: object.load,
        exec: object.exec,
        length: object.size,
        attributes: u32::from(acorn::attr_to_fileswitch(object.attr)),
    }
}

/// The root of `volume`, as RISC OS 5 reports it: its disc's type, the
/// length the found record gives, locked.
fn root_info(volume: &Volume<ClientDisc<'_, '_>>, seen: Seen) -> Info {
    let length = match seen.boot {
        Some(boot) => boot.root_size,
        None => volume.root_size(),
    };
    let disc_type = if seen.boot.is_some() {
        HARD_DISC
    } else {
        FLOPPY_DISC
    };
    Info {
        kind: kind::DIRECTORY,
        load: tairix_riscos_kernel::filing::fs::stamp::typed(disc_type, 0),
        exec: 0,
        length,
        attributes: ROOT_ATTRIBUTES,
    }
}

/// The instance an entry is called for, by its workspace in R12, and its
/// client.
fn context(os: &mut Os<'_>, regs: &Regs) -> Result<(u32, Client), Fail> {
    let workspace = regs.r[12];
    let client = with_instance(os, workspace, |instance| instance.client)?;
    Ok((workspace, client))
}

/// The name at `at`, and the drive it names.
fn resolve(os: &mut Os<'_>, workspace: u32, client: &Client, at: u32) -> Result<(Named, u8), Fail> {
    let named = names::read(os, client, at)?;
    let drive = names::drive_of(os, workspace, named.disc.as_deref())?;
    Ok((named, drive))
}

/// How the instance holds `parent`'s entry `name` on `drive` open: whether
/// on any handle, and whether on one that may write it.
fn held(
    os: &mut Os<'_>,
    workspace: u32,
    drive: u8,
    parent: u32,
    name: &[u8],
) -> Result<(bool, bool), Fail> {
    with_instance(os, workspace, |instance| {
        instance
            .files
            .iter()
            .flatten()
            .filter(|file| {
                file.drive == drive
                    && file.parent == parent
                    && acorn::names_match(file.name.get(), name)
            })
            .fold((false, false), |(_, writing), file| {
                (true, writing || file.writable)
            })
    })
}

/// Whether the instance holds `parent`'s entry `name` open on `drive`.
fn is_open(
    os: &mut Os<'_>,
    workspace: u32,
    drive: u8,
    parent: u32,
    name: &[u8],
) -> Result<bool, Fail> {
    held(os, workspace, drive, parent, name).map(|(open, _)| open)
}

/// `FSEntry_File`.
pub(super) fn file(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, client) = context(os, regs)?;
    match regs.r[0] {
        file::READ_INFO | file::READ_INFO_NO_LENGTH => {
            let (named, drive) = resolve(os, workspace, &client, regs.r[1])?;
            let info = with_volume(os, workspace, drive, |volume, seen| {
                Ok(match names::walk(volume, &named)? {
                    Some(Found::Root) => root_info(volume, seen),
                    Some(Found::Entry { object, .. }) => info_of(&object),
                    Some(Found::Missing { .. }) | None => Info::default(),
                })
            })?;
            info.answer(regs);
            Ok(())
        }
        file::SAVE => save(os, workspace, &client, regs),
        file::CREATE => create(os, workspace, &client, regs),
        file::CREATE_DIRECTORY => create_directory(os, workspace, &client, regs),
        file::WRITE_INFO => restamp(os, workspace, &client, regs, true),
        file::WRITE_ATTRIBUTES => restamp(os, workspace, &client, regs, false),
        file::DELETE => delete(os, workspace, &client, regs),
        file::BLOCK_SIZE => {
            let (_, drive) = resolve(os, workspace, &client, regs.r[1])?;
            regs.r[2] = with_volume(os, workspace, drive, |volume, _| {
                Ok(volume.disc_mut().sector())
            })?;
            Ok(())
        }
        _ => Err(os.error(UNSUPPORTED)),
    }
}

/// `FSEntry_File` 0: R4 to R5 saved as the file R1 names, stamped R2 and R3:
/// a new file read and write for its owner, one saved over keeping its
/// attributes and taking the spelling given.
fn save(os: &mut Os<'_>, workspace: u32, client: &Client, regs: &Regs) -> Result<(), Fail> {
    let (named, drive) = resolve(os, workspace, client, regs.r[1])?;
    let (load, exec, start, end) = (regs.r[2], regs.r[3], regs.r[4], regs.r[5]);
    let size = end.wrapping_sub(start);
    let leaf = names::leaf(&named);
    let (parent, existing) = with_volume(os, workspace, drive, |volume, _| {
        Ok(match names::reach(volume, &named)? {
            Found::Root => (None, None),
            Found::Entry { parent, object, .. } => (Some(parent), Some(object)),
            Found::Missing { parent } => (Some(parent), None),
        })
    })?;
    let Some(parent) = parent else {
        return Err(fail(os, Some(client), code::BAD_NAME));
    };
    if let Some(object) = &existing {
        if object.is_dir() {
            return Err(fail(os, Some(client), code::EXISTS));
        }
        if object.attr & acorn::ATTR_LOCKED != 0 {
            return Err(fail(os, Some(client), code::LOCKED));
        }
        if is_open(os, workspace, drive, parent, leaf)? {
            return Err(fail(os, Some(client), code::OPEN));
        }
    }
    let attr = existing.map_or(NEW_FILE, |object| object.attr);
    with_volume(os, workspace, drive, |volume, _| {
        volume.validate_new_name(leaf)?;
        if let Some(object) = &existing {
            volume.release_maybe_shared(object.indaddr, object.size, parent)?;
            if let Some((index, _)) = volume.dir_lookup(parent, leaf)? {
                volume.dir_remove_at(parent, index)?;
            }
        }
        let indaddr = volume.place_object(parent, u64::from(size))?;
        write_from_guest(volume, indaddr, 0, start, size)?;
        let mut object = Object::named(leaf)?;
        object.load = load;
        object.exec = exec;
        object.size = size;
        object.indaddr = indaddr;
        object.attr = attr;
        volume.dir_insert(parent, &object)
    })
}

/// `size` bytes of the guest's memory from `from` written to the object at
/// `indaddr` from `offset`, a run of the disc at a time.
fn write_from_guest(
    volume: &mut Volume<ClientDisc<'_, '_>>,
    indaddr: u32,
    offset: u64,
    from: u32,
    size: u32,
) -> Result<(), Error<Fail>> {
    transfer(volume, indaddr, offset, from, size, true)
}

/// `size` bytes moved between the guest's memory at `ram` and the object at
/// `indaddr` from `offset`, a run of the disc at a time.
fn transfer(
    volume: &mut Volume<ClientDisc<'_, '_>>,
    indaddr: u32,
    offset: u64,
    ram: u32,
    size: u32,
    write: bool,
) -> Result<(), Error<Fail>> {
    let mut done = 0u32;
    while done < size {
        let (at, run) = volume.locate(indaddr, offset + u64::from(done))?;
        let take = u32::try_from(run.min(u64::from(size - done))).unwrap_or(u32::MAX);
        if take == 0 {
            return Err(Error::Fault(Fault::Broken(
                tairix_filecore::Structure::Object,
            )));
        }
        volume
            .disc_mut()
            .transfer(at, ram.wrapping_add(done), take, write)
            .map_err(Error::Disc)?;
        done += take;
    }
    Ok(())
}

/// `FSEntry_File` 7: an empty file made of R5 less R4 bytes, stamped R2 and
/// R3; one there already is replaced as a save replaces it.
fn create(os: &mut Os<'_>, workspace: u32, client: &Client, regs: &Regs) -> Result<(), Fail> {
    let (named, drive) = resolve(os, workspace, client, regs.r[1])?;
    let (load, exec) = (regs.r[2], regs.r[3]);
    let size = regs.r[5].wrapping_sub(regs.r[4]);
    let leaf = names::leaf(&named);
    let (parent, existing) = with_volume(os, workspace, drive, |volume, _| {
        Ok(match names::reach(volume, &named)? {
            Found::Root => (None, None),
            Found::Entry { parent, object, .. } => (Some(parent), Some(object)),
            Found::Missing { parent } => (Some(parent), None),
        })
    })?;
    let Some(parent) = parent else {
        return Err(fail(os, Some(client), code::BAD_NAME));
    };
    if let Some(object) = &existing {
        if object.is_dir() {
            return Err(fail(os, Some(client), code::EXISTS));
        }
        if object.attr & acorn::ATTR_LOCKED != 0 {
            return Err(fail(os, Some(client), code::LOCKED));
        }
        if is_open(os, workspace, drive, parent, leaf)? {
            return Err(fail(os, Some(client), code::OPEN));
        }
    }
    let attr = existing.map_or(NEW_FILE, |object| object.attr);
    with_volume(os, workspace, drive, |volume, _| {
        volume.validate_new_name(leaf)?;
        if let Some(object) = &existing {
            volume.release_maybe_shared(object.indaddr, object.size, parent)?;
            if let Some((index, _)) = volume.dir_lookup(parent, leaf)? {
                volume.dir_remove_at(parent, index)?;
            }
        }
        let indaddr = volume.place_object(parent, u64::from(size))?;
        let mut object = Object::named(leaf)?;
        object.load = load;
        object.exec = exec;
        object.size = size;
        object.indaddr = indaddr;
        object.attr = attr;
        volume.dir_insert(parent, &object)
    })
}

/// `FSEntry_File` 8: a directory made, stamped R2 and R3; one there already
/// is left as it is.
fn create_directory(
    os: &mut Os<'_>,
    workspace: u32,
    client: &Client,
    regs: &Regs,
) -> Result<(), Fail> {
    let (named, drive) = resolve(os, workspace, client, regs.r[1])?;
    let (load, exec) = (regs.r[2], regs.r[3]);
    let leaf = names::leaf(&named);
    let made = with_volume(os, workspace, drive, |volume, _| {
        let parent = match names::reach(volume, &named)? {
            Found::Root => return Ok(true),
            Found::Entry { object, .. } => return Ok(object.is_dir()),
            Found::Missing { parent } => parent,
        };
        volume.validate_new_name(leaf)?;
        let (indaddr, size) = volume.new_directory(parent, leaf)?;
        let mut object = Object::named(leaf)?;
        object.load = load;
        object.exec = exec;
        object.size = size;
        object.indaddr = indaddr;
        object.attr = acorn::ATTR_DIRECTORY;
        if let Err(error) = volume.dir_insert(parent, &object) {
            // The insertion's refusal is the one to report.
            let _ = volume.free(indaddr, size);
            return Err(error);
        }
        Ok(true)
    })?;
    if made {
        Ok(())
    } else {
        Err(fail(os, Some(client), code::EXISTS))
    }
}

/// `FSEntry_File` 1 and 4: the object R1 names restamped R2 and R3 and given
/// R5's attributes, or given its attributes alone.
fn restamp(
    os: &mut Os<'_>,
    workspace: u32,
    client: &Client,
    regs: &Regs,
    stamps: bool,
) -> Result<(), Fail> {
    let (named, drive) = resolve(os, workspace, client, regs.r[1])?;
    let (load, exec) = (regs.r[2], regs.r[3]);
    let attributes = regs.r[5].to_le_bytes()[0];
    with_volume(os, workspace, drive, |volume, _| {
        let Some(Found::Entry {
            parent,
            index,
            object,
        }) = names::walk(volume, &named)?
        else {
            return Ok(());
        };
        let mut changed = object;
        if stamps {
            changed.load = load;
            changed.exec = exec;
        }
        let kept = object.attr & acorn::ATTR_DIRECTORY;
        changed.attr = kept | (acorn::attr_from_fileswitch(attributes) & acorn::FILECORE_ATTR_BITS);
        volume.dir_update_at(parent, index, &changed)
    })
}

/// `FSEntry_File` 6: the object R1 names deleted, answered as it was read;
/// nothing there is nothing deleted.
fn delete(os: &mut Os<'_>, workspace: u32, client: &Client, regs: &mut Regs) -> Result<(), Fail> {
    let (named, drive) = resolve(os, workspace, client, regs.r[1])?;
    let found = with_volume(os, workspace, drive, |volume, _| {
        match names::walk(volume, &named)? {
            Some(Found::Entry { parent, object, .. }) => {
                let empty = !object.is_dir() || volume.dir_is_empty(object.indaddr)?;
                Ok(Some((parent, object, empty)))
            }
            Some(Found::Root | Found::Missing { .. }) | None => Ok(None),
        }
    })?;
    let Some((parent, object, empty)) = found else {
        Info::default().answer(regs);
        return Ok(());
    };
    if object.attr & acorn::ATTR_LOCKED != 0 {
        return Err(fail(os, Some(client), code::LOCKED));
    }
    if !empty {
        return Err(fail(os, Some(client), code::DIR_NOT_EMPTY));
    }
    if is_open(os, workspace, drive, parent, object.name())? {
        return Err(fail(os, Some(client), code::OPEN));
    }
    with_volume(os, workspace, drive, |volume, _| {
        if let Some((index, _)) = volume.dir_lookup(parent, object.name())? {
            volume.dir_remove_at(parent, index)?;
        }
        volume.release_maybe_shared(object.indaddr, object.size, parent)
    })?;
    info_of(&object).answer(regs);
    Ok(())
}

/// The file the instance holds open on FileSwitch's `handle`.
fn opened(
    os: &mut Os<'_>,
    workspace: u32,
    client: &Client,
    handle: u32,
) -> Result<(usize, OpenFile), Fail> {
    let slot = (handle as usize).wrapping_sub(1);
    let file = with_instance(os, workspace, |instance| {
        instance.files.get(slot).copied().flatten()
    })?;
    match file {
        Some(file) => Ok((slot, file)),
        None => Err(fail(os, Some(client), code::CHANNEL)),
    }
}

/// `FSEntry_Open`: the object R1 names opened to read, made or emptied to
/// write, or opened for update: R0 its information word, R1 its handle — zero
/// where there is nothing to open — R2 its buffer's size, R3 its extent and
/// R4 its allocation. As on RISC OS 5, reading needs the owner's read access
/// and writing needs no write access, but a locked file opened for update is
/// opened only to be read; and a file open on any handle to be written is
/// open on no other, while one open only to be read may be on many.
pub(super) fn open(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, client) = context(os, regs)?;
    let reason = regs.r[0];
    if !matches!(reason, reasons::READ | reasons::CREATE | reasons::UPDATE) {
        return Err(os.error(UNSUPPORTED));
    }
    let (named, drive) = resolve(os, workspace, &client, regs.r[1])?;
    let (sector, found) = with_volume(os, workspace, drive, |volume, seen| {
        let sector = volume.disc_mut().sector();
        let found = match names::walk(volume, &named)? {
            Some(Found::Root) => Some((ROOT_PARENT, Some(root_object(volume, seen)?))),
            Some(Found::Entry { parent, object, .. }) => Some((parent, Some(object))),
            Some(Found::Missing { parent }) => Some((parent, None)),
            None => None,
        };
        Ok((sector, found))
    })?;
    let (parent, object) = match (reason, found) {
        (reasons::CREATE, None) => return Err(fail(os, Some(&client), code::NOT_FOUND)),
        (reasons::CREATE, Some(found)) => found,
        (_, Some((parent, Some(object)))) => (parent, Some(object)),
        _ => {
            regs.r[1] = 0;
            return Ok(());
        }
    };
    let file = match object {
        Some(object) if object.is_dir() => {
            if reason != reasons::READ {
                return Err(fail(os, Some(&client), code::EXISTS));
            }
            OpenFile {
                drive,
                parent,
                name: names::EntryName::of(&object),
                indaddr: object.indaddr,
                extent: object.size,
                allocated: object.size,
                writable: false,
                directory: true,
            }
        }
        Some(object) => opened_file(os, workspace, &client, drive, parent, &object, reason)?,
        None => {
            let stamp = stamp_now(os, DATA);
            let made = with_volume(os, workspace, drive, |volume, _| {
                made(volume, parent, &named, stamp)
            })?;
            OpenFile {
                drive,
                parent,
                name: names::EntryName::of(&made),
                indaddr: made.indaddr,
                extent: 0,
                allocated: 0,
                writable: true,
                directory: false,
            }
        }
    };
    let slot = hold_open(os, workspace, &file)?;
    regs.r[0] = file_info::READ
        | if file.writable { file_info::WRITE } else { 0 }
        | if file.directory {
            file_info::DIRECTORY
        } else {
            0
        };
    regs.r[1] = len32(slot) + 1;
    regs.r[2] = sector;
    regs.r[3] = file.extent;
    regs.r[4] = file.allocated;
    Ok(())
}

/// The file `object`, an entry of the directory at `parent` on `drive`,
/// opened as `reason` asks: refused where its access or its lock forbids
/// that, or where it is open to be written or is to be written and open at
/// all; emptied first where it is opened to be written afresh.
fn opened_file(
    os: &mut Os<'_>,
    workspace: u32,
    client: &Client,
    drive: u8,
    parent: u32,
    object: &Object,
    reason: u32,
) -> Result<OpenFile, Fail> {
    let locked = object.attr & acorn::ATTR_LOCKED != 0;
    if reason == reasons::READ && object.attr & acorn::ATTR_OWNER_READ == 0 {
        return Err(fail(os, Some(client), code::ACCESS));
    }
    if reason == reasons::CREATE && locked {
        return Err(fail(os, Some(client), code::LOCKED));
    }
    let writable = reason != reasons::READ && !locked;
    let (open, writing) = held(os, workspace, drive, parent, object.name())?;
    if writing || (open && writable) {
        return Err(fail(os, Some(client), code::OPEN));
    }
    with_volume(os, workspace, drive, |volume, _| {
        let opened = if reason == reasons::CREATE {
            emptied(volume, parent, object)?
        } else {
            *object
        };
        let allocated = u32::try_from(volume.object_allocated(opened.indaddr, opened.size)?)
            .unwrap_or(u32::MAX);
        Ok(OpenFile {
            drive,
            parent,
            name: names::EntryName::of(&opened),
            indaddr: opened.indaddr,
            extent: opened.size,
            allocated,
            writable,
            directory: false,
        })
    })
}

/// `object`, an entry of the directory at `parent`, emptied for an open that
/// writes it afresh.
fn emptied(
    volume: &mut Volume<ClientDisc<'_, '_>>,
    parent: u32,
    object: &Object,
) -> Result<Object, Error<Fail>> {
    volume.release_maybe_shared(object.indaddr, object.size, parent)?;
    let mut emptied = *object;
    emptied.size = 0;
    emptied.indaddr = EMPTY_OBJECT;
    let (index, _) = volume
        .dir_lookup(parent, object.name())?
        .ok_or(Error::Fault(Fault::Broken(
            tairix_filecore::Structure::Directory,
        )))?;
    volume.dir_update_at(parent, index, &emptied)?;
    Ok(emptied)
}

/// A new, empty file `named` names made in the directory at `parent`,
/// stamped `stamp`, for an open that creates one.
fn made(
    volume: &mut Volume<ClientDisc<'_, '_>>,
    parent: u32,
    named: &Named,
    (load, exec): (u32, u32),
) -> Result<Object, Error<Fail>> {
    let leaf = names::leaf(named);
    volume.validate_new_name(leaf)?;
    let mut object = Object::named(leaf)?;
    object.load = load;
    object.exec = exec;
    object.indaddr = EMPTY_OBJECT;
    object.attr = NEW_FILE;
    volume.dir_insert(parent, &object)?;
    Ok(object)
}

/// `file` held open by the instance: the slot it takes, the first free one
/// or a new one.
fn hold_open(os: &mut Os<'_>, workspace: u32, file: &OpenFile) -> Result<usize, Fail> {
    let slot = with_instance(os, workspace, |instance| {
        if let Some(free) = instance.files.iter().position(Option::is_none) {
            instance.files[free] = Some(*file);
            return Some(free);
        }
        instance.files.try_reserve(1).ok()?;
        instance.files.push(Some(*file));
        Some(instance.files.len() - 1)
    })?;
    slot.ok_or_else(|| os.no_memory())
}

/// The root as an object, which no entry holds.
fn root_object(volume: &Volume<ClientDisc<'_, '_>>, seen: Seen) -> Result<Object, Error<Fail>> {
    let info = root_info(volume, seen);
    let mut root = Object::named(b"$")?;
    root.indaddr = volume.root_addr();
    root.size = info.length;
    root.load = info.load;
    root.attr = acorn::ATTR_DIRECTORY;
    Ok(root)
}

/// `FSEntry_GetBytes`: R3 bytes of the file on R1 from R4 into R2, as its
/// allocation holds them; zeros past it.
pub(super) fn get_bytes(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, client) = context(os, regs)?;
    let (_, file) = opened(os, workspace, &client, regs.r[1])?;
    let (to, count, offset) = (regs.r[2], regs.r[3], regs.r[4]);
    // What was put lies in the allocation before FileSwitch next tells the
    // extent; FileSwitch bounds a read by the extent itself.
    let held = file.allocated.saturating_sub(offset).min(count);
    if held > 0 && !file.directory {
        with_volume(os, workspace, file.drive, |volume, _| {
            transfer(volume, file.indaddr, u64::from(offset), to, held, false)
        })?;
    }
    if held < count {
        os.fill(to.wrapping_add(held), count - held, 0)?;
    }
    Ok(())
}

/// `FSEntry_PutBytes`: R3 bytes from R2 written to the file on R1 at R4, its
/// allocation grown first where they pass it.
pub(super) fn put_bytes(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, client) = context(os, regs)?;
    let (slot, file) = opened(os, workspace, &client, regs.r[1])?;
    if !file.writable {
        return Err(fail(os, Some(&client), code::ACCESS));
    }
    let (from, count, offset) = (regs.r[2], regs.r[3], regs.r[4]);
    let end = offset
        .checked_add(count)
        .ok_or_else(|| fail(os, Some(&client), code::DISC_FULL))?;
    let file = grown(os, workspace, slot, &file, end)?;
    with_volume(os, workspace, file.drive, |volume, _| {
        transfer(volume, file.indaddr, u64::from(offset), from, count, true)
    })
}

/// `file` held on `slot`.
fn store(os: &mut Os<'_>, workspace: u32, slot: usize, file: &OpenFile) -> Result<(), Fail> {
    with_instance(os, workspace, |instance| {
        if let Some(entry) = instance.files.get_mut(slot) {
            *entry = Some(*file);
        }
    })
}

/// The entry of the file open as `file`, and its place in its directory.
fn entry_of(
    volume: &mut Volume<ClientDisc<'_, '_>>,
    file: &OpenFile,
) -> Result<(u32, Object), Error<Fail>> {
    volume
        .dir_lookup(file.parent, file.name.get())?
        .ok_or(Error::Fault(Fault::Broken(
            tairix_filecore::Structure::Directory,
        )))
}

/// `file`, open on `slot`, with room for `size` bytes: past its allocation,
/// as FileCore grows an open file, it takes every byte the disc has free,
/// keeping them while it stays open, its entry's length the allocation's.
fn grown(
    os: &mut Os<'_>,
    workspace: u32,
    slot: usize,
    file: &OpenFile,
    size: u32,
) -> Result<OpenFile, Fail> {
    if size <= file.allocated {
        return Ok(*file);
    }
    let grown = with_volume(os, workspace, file.drive, |volume, _| {
        let (free, _) = space_of(volume)?;
        let wanted = match volume.growth_base(file.indaddr, file.parent)? {
            Some(kept) => (kept + free).max(u64::from(size)),
            None => u64::from(size),
        };
        let indaddr = volume.grow_object(file.indaddr, file.allocated, wanted, file.parent)?;
        let allocated =
            volume.object_allocated(indaddr, u32::try_from(wanted).unwrap_or(u32::MAX))?;
        let allocated = u32::try_from(allocated).unwrap_or(u32::MAX);
        let (index, mut entry) = entry_of(volume, file)?;
        entry.indaddr = indaddr;
        entry.size = allocated;
        volume.dir_update_at(file.parent, index, &entry)?;
        Ok(OpenFile {
            indaddr,
            allocated,
            ..*file
        })
    })?;
    store(os, workspace, slot, &grown)?;
    Ok(grown)
}

/// `file`, open on `slot`, at `extent` bytes, which its entry takes when it
/// closes.
fn extended(
    os: &mut Os<'_>,
    workspace: u32,
    slot: usize,
    file: &OpenFile,
    extent: u32,
) -> Result<(), Fail> {
    let file = grown(os, workspace, slot, file, extent)?;
    store(os, workspace, slot, &OpenFile { extent, ..file })
}

/// `FSEntry_Args`.
pub(super) fn args(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, client) = context(os, regs)?;
    let (slot, file) = opened(os, workspace, &client, regs.r[1])?;
    match regs.r[0] {
        args::WRITE_EXTENT => {
            if !file.writable {
                return Err(fail(os, Some(&client), code::ACCESS));
            }
            extended(os, workspace, slot, &file, regs.r[2])
        }
        args::READ_ALLOCATED => {
            regs.r[2] = file.allocated;
            Ok(())
        }
        args::FLUSH | args::IMAGE_STAMP => Ok(()),
        args::ENSURE => {
            let file = grown(os, workspace, slot, &file, regs.r[2])?;
            regs.r[2] = file.allocated;
            Ok(())
        }
        args::ZEROS => {
            let (offset, count) = (regs.r[2], regs.r[3]);
            let end = offset
                .checked_add(count)
                .ok_or_else(|| fail(os, Some(&client), code::DISC_FULL))?;
            let file = grown(os, workspace, slot, &file, end)?;
            with_volume(os, workspace, file.drive, |volume, _| {
                let (mut done, zeros) = (0u32, [0u8; 512]);
                while done < count {
                    let take = (count - done).min(len32(zeros.len()));
                    volume.object_write(
                        file.indaddr,
                        u64::from(offset + done),
                        &zeros[..take as usize],
                    )?;
                    done += take;
                }
                Ok(())
            })
        }
        args::READ_STAMP => {
            let (load, exec) = with_volume(os, workspace, file.drive, |volume, seen| {
                if file.directory && file.parent == ROOT_PARENT {
                    let root = root_info(volume, seen);
                    return Ok((root.load, root.exec));
                }
                entry_of(volume, &file).map(|(_, entry)| (entry.load, entry.exec))
            })?;
            regs.r[2] = load;
            regs.r[3] = exec;
            Ok(())
        }
        _ => Err(os.error(UNSUPPORTED)),
    }
}

/// `FSEntry_Close`: the file on R1 closed — its allocation settled at its
/// extent and its entry given its length — restamped R2 and R3 where they
/// are not both zero.
pub(super) fn close(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, client) = context(os, regs)?;
    let (slot, file) = opened(os, workspace, &client, regs.r[1])?;
    with_instance(os, workspace, |instance| {
        if let Some(entry) = instance.files.get_mut(slot) {
            *entry = None;
        }
    })?;
    if file.directory || !file.writable {
        return Ok(());
    }
    let stamp = (regs.r[2] != 0 || regs.r[3] != 0).then_some((regs.r[2], regs.r[3]));
    with_volume(os, workspace, file.drive, |volume, _| {
        let indaddr = volume.settle_object(
            file.indaddr,
            file.allocated,
            u64::from(file.extent),
            file.parent,
        )?;
        let (index, mut entry) = entry_of(volume, &file)?;
        entry.size = file.extent;
        entry.indaddr = indaddr;
        if let Some((load, exec)) = stamp {
            entry.load = load;
            entry.exec = exec;
        }
        volume.dir_update_at(file.parent, index, &entry)
    })
}

/// `FSEntry_Func`.
pub(super) fn func(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, client) = context(os, regs)?;
    match regs.r[0] {
        func::SET_DIRECTORY
        | func::SHUT_DOWN
        | func::NEW_IMAGE
        | func::CLOSING_IMAGE
        | func::STAMP_IMAGE => Ok(()),
        func::OPT => opt(os, workspace, regs),
        func::RENAME => rename(os, workspace, &client, regs),
        func::READ_DISC => read_disc(os, workspace, &client, regs),
        func::READ_NAMES => list(os, workspace, &client, regs, Layout::Names),
        func::READ_ENTRIES => list(os, workspace, &client, regs, Layout::Info),
        func::READ_FULL_ENTRIES => list(os, workspace, &client, regs, Layout::FullInfo),
        func::CANONICAL => canonical(os, workspace, regs),
        func::READ_BOOT_OPTION => {
            let (_, drive) = resolve(os, workspace, &client, regs.r[1])?;
            regs.r[2] = with_volume(os, workspace, drive, |volume, _| boot_option(volume))?;
            Ok(())
        }
        func::DEFECT_LIST => defect_list(os, workspace, &client, regs, false),
        func::DEFECT_LIST_64 => defect_list(os, workspace, &client, regs, true),
        func::ADD_DEFECT => add_defect(os, workspace, &client, regs, u64::from(regs.r[2])),
        func::ADD_DEFECT_64 => {
            let address = u64::from(regs.r[2]) | u64::from(regs.r[3]) << 32;
            add_defect(os, workspace, &client, regs, address)
        }
        func::WRITE_BOOT_OPTION => {
            let (_, drive) = resolve(os, workspace, &client, regs.r[1])?;
            let option = regs.r[2].to_le_bytes()[0];
            with_volume(os, workspace, drive, |volume, _| {
                volume.set_boot_option(option)
            })
        }
        func::FREE_SPACE | func::FREE_SPACE_64 => free_space(os, workspace, &client, regs),
        func::NAME_DISC => name_disc(os, workspace, &client, regs),
        func::OBJECT_AT_OFFSET => object_at_offset(os, workspace, &client, regs),
        _ => Err(os.error(UNSUPPORTED)),
    }
}

/// `FSEntry_Func` 25 and 36: the defects the disc R1 names records, written
/// to the R5 bytes at R2 as RISC OS 5 writes them. For 25, a word each — the
/// boot block's byte address, or, on a disc without one, the map's sector
/// number — then the list's end, a big disc's defects past 512 MB left out
/// as past what its end lets a word hold; for 36, each byte address in two
/// words, with no end. What does not fit is FileCore's "Too many defects",
/// what fits before it written.
fn defect_list(
    os: &mut Os<'_>,
    workspace: u32,
    client: &Client,
    regs: &Regs,
    wide: bool,
) -> Result<(), Fail> {
    let (_, drive) = resolve(os, workspace, client, regs.r[1])?;
    let mut list = Listed {
        bytes: Vec::new(),
        room: usize::try_from(regs.r[5]).unwrap_or(usize::MAX),
        stopped: None,
    };
    with_volume(os, workspace, drive, |volume, _| {
        let sector = volume.sector_size().trailing_zeros();
        volume.defects(|defect| {
            if wide {
                return list.add(&defect.address.to_le_bytes());
            }
            let word = match defect.recorded {
                Recorded::First => defect.address,
                Recorded::Map => defect.address >> sector,
                Recorded::Second => return true,
            };
            list.add(&u32::try_from(word).unwrap_or(u32::MAX).to_le_bytes())
        })
    })?;
    if !wide && list.stopped.is_none() {
        list.add(&FIRST_END.to_le_bytes());
    }
    os.write_bytes(regs.r[2], &list.bytes)?;
    match list.stopped {
        None => Ok(()),
        Some(Stop::Full) => Err(fail(os, Some(client), code::TOO_MANY_DEFECTS)),
        Some(Stop::NoMemory) => Err(os.no_memory()),
    }
}

/// A defect list as written so far.
struct Listed {
    bytes: Vec<u8>,
    room: usize,
    stopped: Option<Stop>,
}

/// Why a defect list stopped.
enum Stop {
    /// Its buffer had no room for the next.
    Full,
    /// The host had no memory for it.
    NoMemory,
}

impl Listed {
    /// `item` added where it fits: whether the list may go on.
    fn add(&mut self, item: &[u8]) -> bool {
        if self.bytes.len() + item.len() > self.room {
            self.stopped = Some(Stop::Full);
        } else if self.bytes.try_reserve(item.len()).is_err() {
            self.stopped = Some(Stop::NoMemory);
        } else {
            self.bytes.extend_from_slice(item);
        }
        self.stopped.is_none()
    }
}

/// `FSEntry_Func` 26 and 37: the sector holding byte `address` of the disc
/// R1 names recorded as a defect, as `lib/filecore` records one.
fn add_defect(
    os: &mut Os<'_>,
    workspace: u32,
    client: &Client,
    regs: &Regs,
    address: u64,
) -> Result<(), Fail> {
    let (_, drive) = resolve(os, workspace, client, regs.r[1])?;
    with_volume(os, workspace, drive, |volume, _| volume.add_defect(address))
}

/// The boot option `volume`'s disc records.
fn boot_option(volume: &mut Volume<ClientDisc<'_, '_>>) -> Result<u32, Error<Fail>> {
    volume.boot_option().map(u32::from)
}

/// `FSEntry_Func` 7: `*Opt 4`'s boot option set on the current drive's
/// disc; any other option is FileSwitch's.
fn opt(os: &mut Os<'_>, workspace: u32, regs: &Regs) -> Result<(), Fail> {
    if regs.r[1] != opt::BOOT {
        return Ok(());
    }
    let drive = names::drive_of(os, workspace, None)?;
    let option = regs.r[2].to_le_bytes()[0];
    with_volume(os, workspace, drive, |volume, _| {
        volume.set_boot_option(option)
    })
}

/// `FSEntry_Func` 8: the object R1 names renamed R2, R1 zero where it was;
/// nonzero across discs, which FileSwitch reports as a bad rename. A name
/// that differs only in case is re-spelt where it stands; any other moves
/// the entry to its sorted place, a file sharing a fragment moving out of it
/// into a new directory's. An open object is not renamed.
fn rename(os: &mut Os<'_>, workspace: u32, client: &Client, regs: &mut Regs) -> Result<(), Fail> {
    let (from, from_drive) = resolve(os, workspace, client, regs.r[1])?;
    let (to, to_drive) = resolve(os, workspace, client, regs.r[2])?;
    if from_drive != to_drive {
        regs.r[1] = 1;
        return Ok(());
    }
    let to_leaf = names::leaf(&to);
    let plan = with_volume(os, workspace, from_drive, |volume, _| {
        let Some(Found::Entry {
            parent: from_parent,
            object,
            ..
        }) = names::walk(volume, &from)?
        else {
            return Ok(Err(code::NOT_FOUND));
        };
        let to_parent = match names::reach(volume, &to)? {
            Found::Missing { parent } => parent,
            Found::Entry { parent, index, .. } if parent == from_parent => {
                // The object's own entry, re-spelt.
                let (own, _) = volume
                    .dir_lookup(parent, object.name())?
                    .ok_or(Error::Fault(Fault::Broken(
                        tairix_filecore::Structure::Directory,
                    )))?;
                if own != index {
                    return Ok(Err(code::EXISTS));
                }
                parent
            }
            Found::Entry { .. } => return Ok(Err(code::EXISTS)),
            Found::Root => return Ok(Err(code::BAD_RENAME)),
        };
        if object.is_dir() && to_parent != from_parent {
            let mut cursor = to_parent;
            let root = volume.root_addr();
            while cursor != root {
                if cursor == object.indaddr {
                    return Ok(Err(code::BAD_RENAME));
                }
                cursor = volume.dir_parent(cursor)?;
            }
        }
        Ok(Ok((from_parent, to_parent, object)))
    })?;
    let (from_parent, to_parent, object) = match plan {
        Ok(plan) => plan,
        Err(code) => return Err(fail(os, Some(client), code)),
    };
    if object.attr & acorn::ATTR_LOCKED != 0 {
        return Err(fail(os, Some(client), code::LOCKED));
    }
    if is_open(os, workspace, from_drive, from_parent, object.name())? {
        return Err(fail(os, Some(client), code::OPEN));
    }
    with_volume(os, workspace, from_drive, |volume, _| {
        volume.validate_new_name(to_leaf)?;
        let (index, _) = volume
            .dir_lookup(from_parent, object.name())?
            .ok_or(Error::Fault(Fault::Broken(
                tairix_filecore::Structure::Directory,
            )))?;
        if from_parent == to_parent && acorn::names_match(object.name(), to_leaf) {
            volume.dir_respell_at(from_parent, index, &object.renamed(to_leaf)?)?;
            return follow_rename(volume, &object, to_leaf, from_parent, to_parent);
        }
        let copy = volume.copy_for_move(&object, from_parent, to_parent)?;
        let mut moving = object.renamed(to_leaf)?;
        if let Some(indaddr) = copy {
            moving.indaddr = indaddr;
        }
        volume.dir_remove_at(from_parent, index)?;
        if let Err(error) = volume.dir_insert(to_parent, &moving) {
            volume.dir_insert(from_parent, &object)?;
            if let Some(indaddr) = copy {
                // The insertion's refusal is the one to report.
                let _ = volume.release_maybe_shared(indaddr, object.size, to_parent);
            }
            return Err(error);
        }
        if copy.is_some() {
            volume.release_maybe_shared(object.indaddr, object.size, from_parent)?;
        }
        follow_rename(volume, &moving, to_leaf, from_parent, to_parent)
    })?;
    regs.r[1] = 0;
    Ok(())
}

/// A renamed or moved directory's own record of itself: a big directory
/// rewrites its stored name and parent, a fixed one only its parent when it
/// moves.
fn follow_rename(
    volume: &mut Volume<ClientDisc<'_, '_>>,
    object: &Object,
    leaf: &[u8],
    from_parent: u32,
    to_parent: u32,
) -> Result<(), Error<Fail>> {
    if !object.is_dir() {
        return Ok(());
    }
    match volume.load_dir(object.indaddr)? {
        directory @ Directory::Big(_) => {
            volume.rename_dir(object.indaddr, directory, leaf, to_parent)
        }
        directory @ Directory::Fixed(_) if from_parent != to_parent => {
            volume.reparent(object.indaddr, directory, to_parent)
        }
        Directory::Fixed(_) => Ok(()),
    }
}

/// `FSEntry_Func` 11: the name of the disc R1 names, after its length, and
/// its boot option, at R2.
fn read_disc(os: &mut Os<'_>, workspace: u32, client: &Client, regs: &Regs) -> Result<(), Fail> {
    let disc = if regs.r[1] == 0 || os.read_byte(regs.r[1])? == 0 {
        None
    } else {
        Some(names::read(os, client, regs.r[1])?.disc)
    };
    let drive = names::drive_of(os, workspace, disc.flatten().as_deref())?;
    let ((name, len), option) = with_volume(os, workspace, drive, |volume, _| {
        Ok((names::disc_name(volume)?, volume.boot_option()?))
    })?;
    os.write_byte(regs.r[2], u8::try_from(len).unwrap_or(0))?;
    os.write_bytes(regs.r[2].wrapping_add(1), &name[..len])?;
    os.write_byte(regs.r[2].wrapping_add(1 + len32(len)), option)
}

/// `FSEntry_Func` 14, 15 and 19: from R4, up to R3 of the entries of the
/// directory R1 names into R2's R5 bytes; R3 how many, R4 where to go on,
/// -1 once none are left.
fn list(
    os: &mut Os<'_>,
    workspace: u32,
    client: &Client,
    regs: &mut Regs,
    layout: Layout,
) -> Result<(), Fail> {
    let (named, drive) = resolve(os, workspace, client, regs.r[1])?;
    let (buffer, wanted, offset, room) = (regs.r[2], regs.r[3], regs.r[4], regs.r[5]);
    let entries = with_volume(os, workspace, drive, |volume, _| {
        let directory = match names::walk(volume, &named)? {
            Some(Found::Root) => volume.root_addr(),
            Some(Found::Entry { object, .. }) if object.is_dir() => object.indaddr,
            _ => return Ok(None),
        };
        let handle = volume.load_dir(directory)?;
        let count = Volume::<ClientDisc<'_, '_>>::dir_len(&handle);
        let mut entries = Vec::new();
        let mut index = offset;
        while index < count && len32(entries.len()) < wanted {
            let Some(object) = volume.dir_entry(&handle, directory, index)? else {
                break;
            };
            if entries.try_reserve(1).is_err() {
                return Err(Error::Fault(Fault::NoMemory));
            }
            entries.push(object);
            index += 1;
        }
        Ok(Some((entries, count)))
    })?;
    let Some((entries, count)) = entries else {
        return Err(fail(os, Some(client), code::NOT_FOUND));
    };
    let mut used: u32 = 0;
    let mut read: u32 = 0;
    for object in &entries {
        let name = object.name();
        let size = layout.size(name);
        if used.saturating_add(size) > room {
            break;
        }
        let at = buffer.wrapping_add(used);
        os.write_listed(at, layout, &info_of(object), object.indaddr, name)?;
        used += size;
        read += 1;
    }
    regs.r[3] = read;
    regs.r[4] = if offset + read >= count {
        u32::MAX
    } else {
        offset + read
    };
    Ok(())
}

/// `FSEntry_Func` 23: the disc R2 names, the current drive's for none, made
/// canonical as its disc's name; no special field. Asked for lengths alone
/// where R3 and R4 give no buffers.
fn canonical(os: &mut Os<'_>, workspace: u32, regs: &mut Regs) -> Result<(), Fail> {
    let given = if regs.r[2] == 0 {
        None
    } else {
        Some(os.read_until(regs.r[2], names::NAME_MOST, |b| b < b' ')?.0)
    };
    let drive = names::drive_of(os, workspace, given.as_deref())?;
    let (name, len) = with_volume(os, workspace, drive, |volume, _| names::disc_name(volume))?;
    let name = &name[..len];
    regs.r[1] = 0;
    if regs.r[4] == 0 {
        regs.r[3] = 0;
        regs.r[4] = len32(name.len());
        regs.r[2] = 0;
        return Ok(());
    }
    os.write_terminated(regs.r[4], name, 0, regs.r[6] as usize)?;
    regs.r[2] = regs.r[4];
    regs.r[3] = 0;
    regs.r[4] = len32(name.len()).saturating_sub(regs.r[6].saturating_sub(1));
    Ok(())
}

/// `FSEntry_Func` 30 and 35: the free space of the disc R1 names — R0 free,
/// R1 the largest object that would fit, R2 the disc's size; or, 64 bits
/// wide, R0 and R1 free, R2 the largest object, R3 and R4 the size.
fn free_space(
    os: &mut Os<'_>,
    workspace: u32,
    client: &Client,
    regs: &mut Regs,
) -> Result<(), Fail> {
    let (_, drive) = resolve(os, workspace, client, regs.r[1])?;
    let (free, size) = with_volume(os, workspace, drive, |volume, _| space_of(volume))?;
    if regs.r[0] == func::FREE_SPACE {
        regs.r[0] = capped(free);
        regs.r[1] = capped(free);
        regs.r[2] = capped(size);
    } else {
        let [free_low, free_high] = split64(free);
        let [size_low, size_high] = split64(size);
        regs.r[0] = free_low;
        regs.r[1] = free_high;
        regs.r[2] = capped(free);
        regs.r[3] = size_low;
        regs.r[4] = size_high;
    }
    Ok(())
}

/// `FSEntry_Func` 31: the disc R1 names named R2, space padded.
fn name_disc(os: &mut Os<'_>, workspace: u32, client: &Client, regs: &Regs) -> Result<(), Fail> {
    let (_, drive) = resolve(os, workspace, client, regs.r[1])?;
    let (given, _) = os.read_until(regs.r[2], tairix_filecore::record::NAME_LEN + 1, |b| {
        b <= b' '
    })?;
    if given.is_empty()
        || given.len() > tairix_filecore::record::NAME_LEN
        || !given.iter().all(|&b| acorn::is_name_byte(b))
    {
        return Err(fail(os, Some(client), code::BAD_NAME));
    }
    let mut name = [b' '; tairix_filecore::record::NAME_LEN];
    name[..given.len()].copy_from_slice(&given);
    with_volume(os, workspace, drive, |volume, _| volume.set_disc_name(name))
}

/// `FSEntry_Func` 33: what holds R2, a byte offset into the disc R1 names,
/// its name written to the R4 bytes at R3 as [`located`] writes it: R2 zero
/// for free space or a defect, one for space no object holds, two for an
/// object.
fn object_at_offset(
    os: &mut Os<'_>,
    workspace: u32,
    client: &Client,
    regs: &mut Regs,
) -> Result<(), Fail> {
    let (_, drive) = resolve(os, workspace, client, regs.r[1])?;
    let (system, _) = os.read_until(client.title, names::NAME_MOST, |b| b <= b' ')?;
    let offset = u64::from(regs.r[2]);
    let mut name = located::Name::new(regs.r[4]);
    let answer = with_volume(os, workspace, drive, |volume, _| {
        located::find(volume, &system, offset, &mut name)
    })?;
    os.write_bytes(regs.r[3], name.written())?;
    match answer {
        located::Answer::Kind(kind) => {
            regs.r[2] = kind;
            Ok(())
        }
        located::Answer::RootOverflow => Err(os.buffer_overflow()),
        located::Answer::Overflow => Err(fail(os, Some(client), code::BUFFER)),
    }
}
