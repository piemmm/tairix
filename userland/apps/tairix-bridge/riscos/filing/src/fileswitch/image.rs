//! Image filing systems (`plans/RISCOS-EMULATOR.md` §6): a file of a type an
//! image filing system registered for is a directory of that filing
//! system's, as RISC OS 5 makes one.
//!
//! - Its host opens it for update, and the image filing system is handed
//!   FileSwitch's handle for it by `ImageEntry_Func` 21, through which it
//!   reads the image with FileSwitch's own SWIs.
//! - A name beneath it reaches the image filing system relative to it, R6
//!   the image filing system's handle for the image; the image's root is the
//!   empty name.
//! - It stays open, and later names beneath it go straight to the image
//!   filing system, until it is deleted or its image filing system or host
//!   goes, when `ImageEntry_Func` 22 is told first.

use alloc::vec::Vec;

use tairix_riscos_kernel::filing::fs;
use tairix_riscos_kernel::os::{Fail, Os, Regs};

use super::call::{self, Info, FILE, IMAGE};
use super::name::{Inside, Resolved};
use super::open::status;
use super::system::ImageSystem;
use super::{open, with, State};

/// How deep images may lie in images: a containment bound on what a chain of
/// guest filing systems can make FileSwitch open.
pub const NESTED_MOST: usize = 8;

/// An image FileSwitch holds open.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct OpenImage {
    /// The filing system its path is on.
    host: u8,
    /// Its absolute path.
    path: Vec<u8>,
    /// The special field its path was given with.
    special: Option<Vec<u8>>,
    /// FileSwitch's handle for the image file.
    handle: u32,
    /// The file type of the image filing system reading it.
    kind: u32,
    /// The image filing system's handle for it.
    image: u32,
}

impl State {
    /// The image filing system registered for files whose load address is
    /// `load`, if they are typed and one is.
    fn image_for(&self, load: u32) -> Option<&ImageSystem> {
        let kind = fs::stamp::type_of(load)?;
        self.images.iter().find(|image| image.kind == kind)
    }
}

/// `info` as FileSwitch answers it: a file an image filing system reads is
/// an image.
pub(crate) fn kind_of(os: &mut Os<'_>, mut info: Info) -> Result<Info, Fail> {
    if info.kind == FILE && with(os, |state| state.image_for(info.load).is_some())? {
        info.kind = IMAGE;
    }
    Ok(info)
}

/// `resolved` made to name what lies beneath the deepest image FileSwitch
/// holds open over its path, where one is.
pub(crate) fn route(os: &mut Os<'_>, resolved: &mut Resolved) -> Result<(), Fail> {
    let found = with(os, |state| {
        state
            .open_images
            .iter()
            .filter(|image| {
                image.host == resolved.calls.number
                    && image.special == resolved.special
                    && resolved.path.starts_with(&image.path)
                    && resolved.path.get(image.path.len()) == Some(&b'.')
            })
            .max_by_key(|image| image.path.len())
            .map(|image| (image.kind, image.image, image.path.len()))
    })?;
    if let Some((kind, image, length)) = found {
        enter(os, resolved, kind, image, length + 1)?;
    }
    Ok(())
}

/// `resolved` made to name its path from `at` on in the image `image` of the
/// image filing system for `kind`.
fn enter(
    os: &mut Os<'_>,
    resolved: &mut Resolved,
    kind: u32,
    image: u32,
    at: usize,
) -> Result<(), Fail> {
    let number = resolved.calls.number;
    let calls = with(os, |state| {
        state
            .images
            .iter()
            .find(|system| system.kind == kind)
            .map(|system| system.calls(number))
    })?;
    let Some(calls) = calls else {
        return Err(os.error(tairix_riscos_kernel::filing::NOT_PRESENT));
    };
    resolved.calls = calls;
    resolved.inside = Some(Inside { image, at });
    Ok(())
}

/// The image file at `resolved`'s path up to `end` — an object `found`
/// says is one — opened, if FileSwitch has not opened it already, and
/// `resolved` made to name what lies beneath it.
pub(crate) fn open(
    os: &mut Os<'_>,
    resolved: &mut Resolved,
    end: usize,
    found: &Info,
) -> Result<(), Fail> {
    let at = (end + 1).min(resolved.path.len());
    let path = resolved.path[..end].to_vec();
    let host = resolved.calls.number;
    let special = resolved.special.clone();
    let held = with(os, |state| {
        state
            .open_images
            .iter()
            .find(|image| image.host == host && image.special == special && image.path == path)
            .map(|image| (image.kind, image.image))
    })?;
    if let Some((kind, image)) = held {
        return enter(os, resolved, kind, image, at);
    }
    let (depth, kind) = with(os, |state| {
        let depth = state
            .open_images
            .iter()
            .filter(|image| {
                image.host == host
                    && image.special == special
                    && path.starts_with(&image.path)
                    && path.get(image.path.len()) == Some(&b'.')
            })
            .count();
        (depth, state.image_for(found.load).map(|image| image.kind))
    })?;
    let Some(kind) = kind.filter(|_| depth < NESTED_MOST) else {
        return Err(os.error(tairix_riscos_kernel::filing::NOT_PRESENT));
    };
    let mut file = resolved.clone();
    file.path.truncate(end);
    let handle = open::next_handle(os)?;
    let Some(opened) = call::open_for_update(os, &file, handle)? else {
        return Err(super::name::named_error(
            os,
            tairix_riscos_kernel::filing::NOT_FOUND,
            &file.path,
        ));
    };
    let buffer = opened.buffer;
    let handle = open::record(os, file, opened, status::WRITE, handle)?;
    let mut regs = Regs::default();
    regs.r[1] = handle;
    regs.r[2] = buffer;
    regs.r[4] = found.length;
    // RISC OS 5 tells an image filing system every image is open for update.
    regs.r[5] = tairix_riscos_kernel::swi::os::find::UPDATE;
    let image_calls = with(os, |state| {
        state
            .images
            .iter()
            .find(|system| system.kind == kind)
            .map(|system| system.calls(host))
    })?;
    let told = match image_calls {
        Some(calls) => call::func_registers(os, &calls, fs::func::NEW_IMAGE, &mut regs),
        None => Err(os.error(tairix_riscos_kernel::filing::NOT_PRESENT)),
    };
    if let Err(fail) = told {
        open::close_stamped(os, handle, false)?;
        return Err(fail);
    }
    let image = regs.r[1];
    let recorded = with(os, |state| {
        if state.open_images.try_reserve(1).is_err() {
            return false;
        }
        state.open_images.push(OpenImage {
            host,
            path,
            special,
            handle,
            kind,
            image,
        });
        true
    })?;
    if !recorded {
        return Err(os.no_memory());
    }
    enter(os, resolved, kind, image, at)
}

/// `resolved`, which `found` says is an image file, made to name the image's
/// root, the image opened where FileSwitch has not opened it: a directory's
/// listing of an image file is the image's.
pub(crate) fn root(os: &mut Os<'_>, resolved: &mut Resolved, found: &Info) -> Result<(), Fail> {
    if found.kind == IMAGE {
        let end = resolved.path.len();
        open(os, resolved, end, found)?;
    }
    Ok(())
}

/// The images FileSwitch holds open that `keep` does not keep, closed: the
/// image filing system told by `ImageEntry_Func` 22 and the image file closed
/// on its host, innermost first.
pub(crate) fn close_where(os: &mut Os<'_>, keep: impl Fn(&OpenImage) -> bool) -> Result<(), Fail> {
    loop {
        let next = with(os, |state| {
            let place = state
                .open_images
                .iter()
                .enumerate()
                .filter(|(_, image)| !keep(image))
                .max_by_key(|(_, image)| image.path.len())
                .map(|(place, _)| place)?;
            Some(state.open_images.remove(place))
        })?;
        let Some(image) = next else {
            return Ok(());
        };
        let calls = with(os, |state| {
            state
                .images
                .iter()
                .find(|system| system.kind == image.kind)
                .map(|system| system.calls(image.host))
        })?;
        if let Some(calls) = calls {
            let mut regs = Regs::default();
            regs.r[1] = image.image;
            call::func_registers(os, &calls, fs::func::CLOSING_IMAGE, &mut regs)?;
        }
        if with(os, |state| state.files.stream(image.handle).is_some())? {
            open::close_stamped(os, image.handle, false)?;
        }
    }
}

/// The image open at `resolved`'s path, if one is, closed: before its file
/// is deleted or renamed.
pub(crate) fn close_at(os: &mut Os<'_>, resolved: &Resolved) -> Result<(), Fail> {
    let (host, path, special) = (
        resolved.calls.number,
        resolved.path.clone(),
        resolved.special.clone(),
    );
    close_where(os, |image| {
        !(image.host == host
            && image.special == special
            && (image.path == path
                || (image.path.starts_with(&path) && image.path.get(path.len()) == Some(&b'.'))))
    })
}

/// Whether FileSwitch's handle `handle` holds an open image's file.
pub(crate) fn holds(os: &mut Os<'_>, handle: u32) -> Result<bool, Fail> {
    with(os, |state| {
        state.open_images.iter().any(|image| image.handle == handle)
    })
}

/// The image whose file is on FileSwitch's `handle` closed, its image filing
/// system told first.
pub(crate) fn close_handle(os: &mut Os<'_>, handle: u32) -> Result<(), Fail> {
    close_where(os, |image| image.handle != handle)
}

/// `OS_FSControl` 35: the image filing system whose block is R2 past its
/// module at R1 registered, R3 its entries' R12; one registered again for its
/// type takes its old place.
pub(crate) fn register(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let bits = os.caller_bits();
    let system = ImageSystem::read(os, regs.r[1], regs.r[2], regs.r[3], bits)?;
    let block = system.block;
    let placed = with(os, |state| {
        if let Some(slot) = state
            .images
            .iter_mut()
            .find(|image| image.kind == system.kind)
        {
            return Some(core::mem::replace(slot, system).block);
        }
        if state.images.try_reserve(1).is_err() {
            return None;
        }
        state.images.push(system);
        Some(0)
    })?;
    match placed {
        Some(0) => Ok(()),
        Some(old) => os.rma_free(old),
        None => {
            os.rma_free(block)?;
            Err(os.no_memory())
        }
    }
}

/// `OS_FSControl` 36: the image filing system for the file type R1
/// removed, every image it holds open closed first; "not present" for none.
pub(crate) fn remove(os: &mut Os<'_>, kind: u32) -> Result<(), Fail> {
    if !with(os, |state| {
        state.images.iter().any(|image| image.kind == kind)
    })? {
        return Err(os.error(tairix_riscos_kernel::filing::NOT_PRESENT));
    }
    close_where(os, |image| image.kind != kind)?;
    let block = with(os, |state| {
        let place = state.images.iter().position(|image| image.kind == kind)?;
        Some(state.images.remove(place).block)
    })?;
    match block {
        Some(block) => os.rma_free(block),
        None => Ok(()),
    }
}

/// Every image on the filing system `number` closed, before it goes.
pub(crate) fn close_on(os: &mut Os<'_>, number: u8) -> Result<(), Fail> {
    close_where(os, |image| image.host != number)
}
