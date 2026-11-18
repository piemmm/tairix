//! `OS_File` (`plans/RISCOS-EMULATOR.md` §6): whole-file operations.
//!
//! Every reason reads its object first, as RISC OS 5's FileSwitch does: a
//! missing object is never handed to the filing system, a write of its
//! information is "not found", a delete answers nothing deleted. Loads and
//! saves go through `FSEntry_Open` and the buffer where the filing system's
//! information word asks.

use tairix_riscos_kernel::filing::fs;
use tairix_riscos_kernel::filing::{BAD_FILE, IS_A_DIRECTORY, NOT_FOUND};
use tairix_riscos_kernel::os::{Fail, Os, Regs};

use super::call::{self, Info, Need, DIRECTORY, FILE as A_FILE, NOT_FOUND as NOTHING};
use super::name::{self, Resolved, Search};
use super::open::status;
use super::system::info;
use super::{error, open};

/// The Data file type, which a file no one typed is stamped as.
pub(crate) const DATA: u32 = 0xFFD;
use tairix_riscos_kernel::filing::fs::stamp;
/// `OS_File` 20 to 23's types for what is not a typed file: a directory, an
/// application, and an untyped file or nothing, which RISC OS 5 answers as
/// all ones.
const DIRECTORY_TYPE: u32 = 0x1000;
const APPLICATION_TYPE: u32 = 0x2000;
const UNTYPED: u32 = u32::MAX;

/// The load and execution address stamping a file of type `kind` at the
/// five-byte time `now`.
pub(crate) const fn typed_stamp(kind: u32, now: u64) -> (u32, u32) {
    let bytes = now.to_le_bytes();
    let low = u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
    (stamp::typed(kind, bytes[4] as u32), low)
}

/// [`typed_stamp`] for the guest's time now.
pub(crate) fn stamp_now(os: &mut Os<'_>, kind: u32) -> (u32, u32) {
    typed_stamp(kind, os.real_time())
}

/// The type a file's addresses give it, or `UNTYPED`.
const fn type_of(load: u32) -> u32 {
    match stamp::type_of(load) {
        Some(kind) => kind,
        None => UNTYPED,
    }
}

/// The file type `OS_File` 20 and `OS_GBPB` 12 give an object of `kind`
/// with load address `load` and leaf name `leaf`: a typed file's type,
/// `&2000` and `&1000` for an application and any other directory, and all
/// ones for an untyped file or nothing.
pub(crate) fn object_type(kind: u32, load: u32, leaf: &[u8]) -> u32 {
    match kind {
        NOTHING => UNTYPED,
        A_FILE => type_of(load),
        _ if leaf.starts_with(b"!") => APPLICATION_TYPE,
        _ => DIRECTORY_TYPE,
    }
}

/// `FileV`'s default.
pub(crate) fn file(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let reason = regs.r[0];
    let path_string;
    let search = match reason {
        12 | 13 | 21 => {
            path_string = name::read(os, regs.r[4])?;
            Search::Path(&path_string)
        }
        14 | 15 | 22 => {
            path_string = name::read(os, regs.r[4])?;
            Search::Variable(&path_string)
        }
        16 | 17 | 23 => Search::Plain,
        _ => Search::Variable(b"File$Path"),
    };
    if matches!(reason, 0 | 6..=8 | 10 | 11) && name::wild(&name::given_leaf(os, regs.r[1])?) {
        return Err(error::fail(os, error::WILDCARDS, &[]));
    }
    match reason {
        0 => save(os, regs, regs.r[2], regs.r[3]),
        10 => {
            let (load, exec) = stamp_now(os, regs.r[2]);
            save(os, regs, load, exec)
        }
        1 => write_info(os, regs, None),
        2..=4 => write_info(os, regs, Some(reason)),
        9 => stamp(os, regs, None),
        18 => stamp(os, regs, Some(regs.r[2])),
        5 | 13 | 15 | 17 | 20..=23 => read_info(os, regs, search, reason >= 20),
        6 => delete(os, regs),
        7 => create(os, regs, regs.r[2], regs.r[3]),
        11 => {
            let (load, exec) = stamp_now(os, regs.r[2]);
            create(os, regs, load, exec)
        }
        8 => create_directory(os, regs),
        12 | 14 | 16 | 255 => load(os, regs, search),
        19 => os.object_error(regs.r[1], regs.r[2]),
        24 => block_size(os, regs),
        _ => Err(os.error(BAD_FILE)),
    }
}

/// `error` naming the name in R1.
fn named(os: &mut Os<'_>, regs: &Regs, error: tairix_riscos_kernel::error::Error) -> Fail {
    match name::read(os, regs.r[1]) {
        Ok(given) => name::named_error(os, error, &given),
        Err(fail) => fail,
    }
}

/// The object R1 names, resolved for writing and read as `need` asks.
fn target(os: &mut Os<'_>, regs: &Regs, need: Need) -> Result<(Resolved, Info), Fail> {
    let mut resolved = name::locate(os, regs.r[1], Search::Plain)?;
    let found = name::info(os, &mut resolved, need)?;
    Ok((resolved, found))
}

/// `info` into R0 and R2 to R5.
fn answer(regs: &mut Regs, info: &Info) {
    regs.r[0] = info.kind;
    regs.r[2] = info.load;
    regs.r[3] = info.exec;
    regs.r[4] = info.length;
    regs.r[5] = info.attributes;
}

/// `OS_File` 0 and 10: R4 to R5 saved as the file R1 names, stamped `load`
/// and `exec`.
fn save(os: &mut Os<'_>, regs: &mut Regs, load: u32, exec: u32) -> Result<(), Fail> {
    let (resolved, found) = target(os, regs, Need::Kind)?;
    if found.kind == DIRECTORY {
        return Err(named(os, regs, error::WRONG_KIND));
    }
    let (start, end) = (regs.r[4], regs.r[5]);
    let made = Info {
        kind: A_FILE,
        load,
        exec,
        length: end.wrapping_sub(start),
        attributes: 0,
    };
    if !resolved.calls.has(info::SAVE_BY_OPEN) {
        let mut save = Regs::default();
        save.r[2..6].copy_from_slice(&[load, exec, start, end]);
        call::file_modifying(os, &resolved, fs::file::SAVE, &mut save)?;
        return announce(os, &resolved, &made);
    }
    let mut entry = Regs::default();
    entry.r[2..6].copy_from_slice(&[load, exec, start, end]);
    call::file_modifying(os, &resolved, fs::file::CREATE, &mut entry)?;
    let handle = open::next_handle(os)?;
    let opened = call::open_for_update(os, &resolved, handle)?;
    let Some(opened) = opened else {
        return Err(named(os, regs, NOT_FOUND));
    };
    let handle = open::record(
        os,
        resolved.clone(),
        opened,
        status::WRITE | status::MODIFIED,
        handle,
    )?;
    let written = super::gbpb::write_file(os, handle, start, end.wrapping_sub(start), None);
    let closed = open::close_stamped(os, handle, false);
    written.and(closed)?;
    announce(os, &resolved, &made)
}

/// The message `*Opt 1` asks for of what `resolved` names, `info` its
/// information.
fn announce(os: &mut Os<'_>, resolved: &Resolved, info: &Info) -> Result<(), Fail> {
    super::listing::announce(os, resolved.calls.number, name::leaf(&resolved.path), info)
}

/// `OS_File` 1 to 4: the information R2, R3 and R5 give written to the
/// object R1 names, by `FSEntry_File` 1 as RISC OS 5 writes it — reasons 2
/// to 4, `only` one part, with the rest as the object has it; "not found"
/// for none.
fn write_info(os: &mut Os<'_>, regs: &mut Regs, only: Option<u32>) -> Result<(), Fail> {
    let (resolved, found) = target(os, regs, Need::Kind)?;
    if found.kind == NOTHING {
        return Err(named(os, regs, NOT_FOUND));
    }
    let mut write = Regs::default();
    write.r[2] = if matches!(only, None | Some(2)) {
        regs.r[2]
    } else {
        found.load
    };
    write.r[3] = if matches!(only, None | Some(3)) {
        regs.r[3]
    } else {
        found.exec
    };
    write.r[5] = if matches!(only, None | Some(4)) {
        regs.r[5]
    } else {
        found.attributes
    };
    call::file_modifying(os, &resolved, fs::file::WRITE_INFO, &mut write)
}

/// `OS_File` 9 and 18: the object R1 names stamped now, keeping its type or
/// Data for an untyped one; or typed `kind`, a typed object keeping its
/// date.
fn stamp(os: &mut Os<'_>, regs: &mut Regs, kind: Option<u32>) -> Result<(), Fail> {
    let (resolved, found) = target(os, regs, Need::Kind)?;
    if found.kind == NOTHING {
        return Err(named(os, regs, NOT_FOUND));
    }
    let typed = stamp::type_of(found.load).is_some();
    let (load, exec) = match kind {
        Some(kind) if typed => (stamp::retyped(found.load, kind), found.exec),
        Some(kind) => stamp_now(os, kind),
        None if typed => stamp_now(os, type_of(found.load)),
        None => stamp_now(os, DATA),
    };
    let mut write = Regs::default();
    write.r[2] = load;
    write.r[3] = exec;
    write.r[5] = found.attributes;
    call::file_modifying(os, &resolved, fs::file::WRITE_INFO, &mut write)
}

/// `OS_File` 5, 13, 15, 17 and 20 to 23: the object's information, its type
/// in R6 as well where `typed`.
fn read_info(
    os: &mut Os<'_>,
    regs: &mut Regs,
    search: Search<'_>,
    typed: bool,
) -> Result<(), Fail> {
    let (resolved, found) = name::find(os, regs.r[1], search, Need::Length)?;
    answer(regs, &found);
    if typed {
        regs.r[6] = object_type(found.kind, found.load, name::leaf(&resolved.path));
        // With its type in R6, a typed object's R2 keeps only its stamp's
        // top byte, as RISC OS 5's does.
        if stamp::type_of(found.load).is_some() {
            regs.r[2] = found.load & stamp::HIGH;
        }
    }
    Ok(())
}

/// `OS_File` 6: the object R1 names deleted, its information answered; none
/// for nothing.
fn delete(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (resolved, found) = target(os, regs, Need::Length)?;
    if found.kind == NOTHING {
        regs.r[0] = NOTHING;
        return Ok(());
    }
    super::image::close_at(os, &resolved)?;
    let mut gone = Regs::default();
    gone.r[2] = found.load;
    gone.r[3] = found.exec;
    call::file_modifying(os, &resolved, fs::file::DELETE, &mut gone)?;
    answer(regs, &found);
    Ok(())
}

/// `OS_File` 7 and 11: an empty file of R4 to R5's length made, stamped
/// `load` and `exec`.
fn create(os: &mut Os<'_>, regs: &mut Regs, load: u32, exec: u32) -> Result<(), Fail> {
    let (resolved, found) = target(os, regs, Need::Kind)?;
    if found.kind == DIRECTORY {
        return Err(named(os, regs, error::WRONG_KIND));
    }
    let mut entry = Regs::default();
    entry.r[2..6].copy_from_slice(&[load, exec, regs.r[4], regs.r[5]]);
    call::file_modifying(os, &resolved, fs::file::CREATE, &mut entry)?;
    let made = Info {
        kind: A_FILE,
        load,
        exec,
        length: regs.r[5].wrapping_sub(regs.r[4]),
        attributes: 0,
    };
    announce(os, &resolved, &made)
}

/// `OS_File` 8: a directory made, of R4's entries; nothing where one is
/// there already.
fn create_directory(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (resolved, found) = target(os, regs, Need::Kind)?;
    if found.kind == DIRECTORY {
        return Ok(());
    }
    if found.kind != NOTHING {
        return Err(named(os, regs, error::WRONG_KIND));
    }
    let (load, exec) = stamp_now(os, DATA);
    let mut entry = Regs::default();
    entry.r[2..5].copy_from_slice(&[load, exec, regs.r[4]]);
    call::file_modifying(os, &resolved, fs::file::CREATE_DIRECTORY, &mut entry)
}

/// `OS_File` 12, 14, 16 and 255: the file loaded at R2, or at its own load
/// address where R3's low byte is not zero; its information answered.
fn load(os: &mut Os<'_>, regs: &mut Regs, search: Search<'_>) -> Result<(), Fail> {
    let (resolved, found) = name::find(os, regs.r[1], search, Need::Length)?;
    match found.kind {
        NOTHING => return Err(named(os, regs, NOT_FOUND)),
        A_FILE => {}
        _ => return Err(named(os, regs, IS_A_DIRECTORY)),
    }
    let at = if regs.byte(3) == 0 {
        regs.r[2]
    } else {
        found.load
    };
    if resolved.calls.has(info::LOAD_BY_OPEN) {
        load_by_open(os, regs, &resolved, at, found.length)?;
    } else {
        let mut load = Regs::default();
        load.r[2] = at;
        call::file(
            os,
            &resolved.calls,
            fs::file::LOAD,
            resolved.named(),
            &mut load,
        )?;
    }
    regs.r[2] = found.load;
    regs.r[3] = found.exec;
    regs.r[4] = found.length;
    regs.r[5] = found.attributes;
    announce(os, &resolved, &found)
}

/// A load through `FSEntry_Open`, the file read whole to `at`.
fn load_by_open(
    os: &mut Os<'_>,
    regs: &Regs,
    resolved: &Resolved,
    at: u32,
    length: u32,
) -> Result<(), Fail> {
    let handle = open::next_handle(os)?;
    let opened = call::open(
        os,
        &resolved.calls,
        fs::open::READ,
        resolved.named(),
        handle,
    )?;
    let Some(opened) = opened else {
        return Err(named(os, regs, NOT_FOUND));
    };
    let handle = open::record(os, resolved.clone(), opened, 0, handle)?;
    let read = super::gbpb::read_file(os, handle, at, length, None);
    let closed = open::close_stamped(os, handle, false);
    read.and(closed).map(drop)
}

/// `OS_File` 24: the natural block size of the file R1 names, which RISC OS
/// 5 refuses for a filing system that is not an image filing system's host.
fn block_size(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (resolved, found) = target(os, regs, Need::Kind)?;
    if found.kind == NOTHING {
        return Err(named(os, regs, NOT_FOUND));
    }
    if !resolved.calls.has(info::IMAGES) {
        regs.r[2] = 0;
        return Err(os.error(tairix_riscos_kernel::filing::UNSUPPORTED));
    }
    let mut size = Regs::default();
    call::file(
        os,
        &resolved.calls,
        fs::file::BLOCK_SIZE,
        resolved.named(),
        &mut size,
    )?;
    regs.r[2] = size.r[2];
    Ok(())
}

#[cfg(test)]
mod tests;
