//! `OS_Find` (`plans/RISCOS-EMULATOR.md` §6): files opened and closed.
//!
//! An object is read first, as RISC OS 5 reads it. A file opened to be read
//! or updated must exist; one created is truncated if it does, and made by
//! `FSEntry_File` 7 if not, before the filing system opens it for update —
//! to be written only where the filing system permits it.
//! A filing system that is always called to open a file is called whether
//! or not the file exists, and makes any it creates itself. A directory may
//! be opened to be read, as programs that test for an object that way
//! expect, and is refused for anything else.

use tairix_riscos_kernel::filing::filetype::DATA;
use tairix_riscos_kernel::filing::fs::{self, file_info};
use tairix_riscos_kernel::filing::{BAD_FIND, IS_A_DIRECTORY, NOT_FOUND};
use tairix_riscos_kernel::os::{quietly, Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::find::{
    INPUT, MUST_EXIST, NO_DIRECTORY, OUTPUT, PATH, UPDATE as KIND,
};

use super::call::{self, Need, DIRECTORY, NOT_FOUND as NOTHING};
use super::name::{self, Search};
use super::open::status;
use super::system::info::ALWAYS_OPEN;
use super::{open, with};

/// `FindV`'s default.
pub(crate) fn find(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mode = regs.r[0] & 0xFF;
    if mode & KIND == 0 {
        if mode != 0 {
            return Err(os.error(BAD_FIND));
        }
        return close(os, regs.r[1]);
    }
    let path_string;
    let search = match mode & PATH {
        0 => Search::Variable(b"File$Path"),
        1 => {
            path_string = name::read(os, regs.r[2])?;
            Search::Path(&path_string)
        }
        2 => {
            path_string = name::read(os, regs.r[2])?;
            Search::Variable(&path_string)
        }
        _ => Search::Plain,
    };
    regs.r[0] = match mode & KIND {
        INPUT => existing(os, regs.r[1], search, mode, false)?,
        OUTPUT => create(os, regs.r[1], search, mode)?,
        _ => existing(os, regs.r[1], search, mode, true)?,
    };
    Ok(())
}

/// `OS_Find` 0: the file on `handle` closed, or every file on the temporary
/// filing system for none.
fn close(os: &mut Os<'_>, handle: u32) -> Result<(), Fail> {
    if handle == 0 {
        let temporary = with(os, |state| state.temporary())?;
        return match temporary {
            Some(number) => open::close_all(os, Some(number)),
            None => Ok(()),
        };
    }
    if super::image::holds(os, handle)? {
        return super::image::close_handle(os, handle);
    }
    open::close(os, handle)
}

/// An existing object opened to be read, or updated: its handle, none where
/// there is nothing to open and the mode allows that.
fn existing(
    os: &mut Os<'_>,
    address: u32,
    search: Search<'_>,
    mode: u32,
    update: bool,
) -> Result<u32, Fail> {
    let (resolved, found) = name::find(os, address, search, Need::Kind)?;
    if found.kind == NOTHING && !resolved.calls.has(ALWAYS_OPEN) {
        if mode & MUST_EXIST != 0 {
            return Err(missing(os, address));
        }
        return Ok(0);
    }
    if found.kind == DIRECTORY && (update || mode & NO_DIRECTORY != 0) {
        return Err(refused(os, address, IS_A_DIRECTORY));
    }
    open::holding(os, |os, handle| {
        let opened = if update {
            call::open_for_update(os, &resolved, handle, mode)?
        } else {
            call::open(
                os,
                &resolved.calls,
                fs::open::READ,
                resolved.named(),
                handle,
                mode,
            )?
        };
        match opened {
            Some(opened) => {
                let status = if update { writing(&opened) } else { 0 };
                open::record(os, resolved, opened, status, handle)
            }
            None => unopened(os, address, mode),
        }
    })
}

/// What an open the filing system answered with no handle gives: "unable
/// to open" where `mode` asks for an error, else no handle.
fn unopened(os: &mut Os<'_>, address: u32, mode: u32) -> Result<u32, Fail> {
    if mode & MUST_EXIST == 0 {
        return Ok(0);
    }
    match name::read(os, address) {
        Ok(given) => Err(super::error::fail(os, super::error::CANT_OPEN, &[&given])),
        Err(fail) => Err(fail),
    }
}

/// What a file opened for update may do: be written where its filing system
/// permits it, which a locked file's does not.
const fn writing(opened: &call::Opened) -> u32 {
    if opened.info & file_info::WRITE == 0 {
        0
    } else {
        status::WRITE
    }
}

/// A file created, or truncated where it exists, and opened for update in
/// `mode`: its handle. Where the open answers none, a filing system always
/// called to open gives what [`unopened`] gives, and any other "unable to
/// open" whatever the mode. As on RISC OS 5, a create the filing system
/// refuses is answered by the open after it.
fn create(os: &mut Os<'_>, address: u32, search: Search<'_>, mode: u32) -> Result<u32, Fail> {
    let mut resolved = name::locate(os, address, search)?;
    let found = name::info(os, &mut resolved, Need::Kind)?;
    if found.kind == DIRECTORY || found.kind == call::IMAGE {
        return Err(refused(os, address, IS_A_DIRECTORY));
    }
    let always = resolved.calls.has(ALWAYS_OPEN);
    if found.kind == NOTHING && !always {
        let (load, exec) = super::file::stamp_now(os, DATA);
        let mut make = Regs::default();
        make.r[2] = load;
        make.r[3] = exec;
        quietly(call::file_modifying(
            os,
            &resolved,
            fs::file::CREATE,
            &mut make,
        ))?;
    }
    let handle = open::holding(os, |os, handle| {
        let opened = call::open_for_update(os, &resolved, handle, mode)?;
        let Some(mut opened) = opened else {
            return unopened(os, address, if always { mode } else { MUST_EXIST });
        };
        if opened.buffer != 0 {
            opened.extent = 0;
        } else if let Err(refused) = call::args(
            os,
            &resolved.calls,
            fs::args::WRITE_EXTENT,
            opened.handle,
            0,
            0,
        ) {
            // The truncation's refusal is the one to report.
            let _ = call::close(os, &resolved.calls, opened.handle, 0, 0);
            return Err(refused);
        }
        let status = writing(&opened) | status::MODIFIED;
        open::record(os, resolved, opened, status, handle)
    })?;
    if handle == 0 {
        return Ok(0);
    }
    let mut stream = open::stream(os, handle)?;
    stream.created = true;
    open::put(os, handle, stream)?;
    Ok(handle)
}

/// "File … not found", naming the name given.
fn missing(os: &mut Os<'_>, address: u32) -> Fail {
    refused(os, address, NOT_FOUND)
}

/// `error`, naming the name at `address`.
fn refused(os: &mut Os<'_>, address: u32, error: tairix_riscos_kernel::error::Error) -> Fail {
    match name::read(os, address) {
        Ok(given) => name::named_error(os, error, &given),
        Err(fail) => fail,
    }
}
