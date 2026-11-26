//! DeviceFS's filing system entries, which FileSwitch calls: `devices:$`
//! the one directory, each device a file in it, a stream opened on a device
//! for its driver, and every change to the catalogue refused but its
//! information and attributes, which are taken and kept by nobody, as RISC
//! OS 5's DeviceFS takes them.

use alloc::vec::Vec;
use tairix_riscos_arm::psr;

use tairix_riscos_kernel::filing::fs::{self, attribute, kind, Info, Layout, Listed};
use tairix_riscos_kernel::os::{quietly, Entry, Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::gbpb;

use super::stream::{self, Opening};
use super::{fail, reason, with, Device, State, Way, BAD_OPERATION, NOT_FOUND};
use crate::fileswitch::name::NAME_MOST;
use tairix_util::fallible;

/// The root's attributes, as RISC OS 5 gives them.
const ROOT_ATTRIBUTES: u32 = attribute::OWNER_READ | attribute::OWNER_WRITE;

/// What a path FileSwitch hands over names.
enum Named {
    Root,
    Device(Device),
    Nothing,
}

/// What the path at `at` names: `$`, or a device as `$.<name>` without case.
fn named(os: &mut Os<'_>, at: u32) -> Result<Named, Fail> {
    let Some(path) = given(os, at)? else {
        return Ok(Named::Nothing);
    };
    if path == b"$" {
        return Ok(Named::Root);
    }
    let Some(name) = path.strip_prefix(b"$.") else {
        return Ok(Named::Nothing);
    };
    let device = with(os, |state| state.device_named(name).cloned())?;
    Ok(device.map_or(Named::Nothing, Named::Device))
}

/// The text FileSwitch hands over at `at`, a path or a special field; none
/// past the longest FileSwitch takes, which it never hands over whole.
fn given(os: &mut Os<'_>, at: u32) -> Result<Option<Vec<u8>>, Fail> {
    let (text, ended) = os.read_until(at, NAME_MOST, |b| b == 0)?;
    Ok(ended.then_some(text))
}

/// What a device's catalogue entry says: a file stamped when it was
/// registered, readable where its driver receives and writable where it
/// sends.
fn described(state: &State, device: &Device) -> Info {
    let most = state.driver(device.driver).map_or([0; 2], |d| d.most);
    let read = if most[0] != 0 {
        attribute::OWNER_READ
    } else {
        0
    };
    let write = if most[1] != 0 {
        attribute::OWNER_WRITE
    } else {
        0
    };
    Info {
        kind: kind::FILE,
        load: device.load,
        exec: device.exec,
        length: 0,
        attributes: read | write,
    }
}

/// `FSEntry_Open`: a stream on the device R1 names, to read for reason
/// nought and to write for any other, for FileSwitch's handle R3, with the
/// special field at R6 and the `OS_Find` mode R7; R1 nought where there is
/// no such device or it takes no more streams that way.
pub(super) fn open(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let named = named(os, regs.r[1])?;
    let way = if regs.r[0] == fs::open::READ {
        Way::Receive
    } else {
        Way::Transmit
    };
    let opening = match named {
        Named::Device(device) => with(os, |state| {
            state.driver(device.driver).map(|driver| Opening {
                target: driver.target,
                duplex: driver.duplex,
                most: driver.most,
                monitored: driver.monitored,
                device,
            })
        })?,
        Named::Root | Named::Nothing => None,
    };
    let Some(opening) = opening else {
        regs.r[1] = 0;
        return Ok(());
    };
    let special = match regs.r[6] {
        0 => None,
        at => Some(given(os, at)?.ok_or_else(|| fail(os, BAD_OPERATION))?),
    };
    let opened = stream::open(os, &opening, way, regs.r[3], special.as_deref(), regs.r[7])?;
    match opened {
        Some(opened) => regs.r[..5].copy_from_slice(&[opened.info, opened.handle, 0, 0, 0]),
        None => regs.r[1] = 0,
    }
    Ok(())
}

/// `FSEntry_GetBytes`: the next byte of R1's stream in R0, C set where none
/// may be waited for.
pub(super) fn get_bytes(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let byte = stream::get(os, regs.r[1])?;
    if let Some(byte) = byte {
        regs.r[0] = u32::from(byte);
    }
    regs.set_flag(psr::C, byte.is_none());
    Ok(())
}

/// `FSEntry_PutBytes`: R0's byte to R1's stream.
pub(super) fn put_bytes(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    stream::put(os, regs.r[1], regs.byte(0))
}

/// `FSEntry_GBPB` on a buffered stream: R3 bytes between R2 and the stream,
/// written for 2 and read for 4, R2 and R3 moved on; none at a pointer,
/// which a stream has not got.
pub(super) fn gbpb(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let [reason, handle, at, count, ..] = regs.r;
    let moved = match reason {
        gbpb::WRITE => stream::write(os, handle, at, count)?,
        gbpb::READ => stream::read(os, handle, at, count)?,
        _ => return Err(fail(os, BAD_OPERATION)),
    };
    regs.r[2] = at.wrapping_add(moved);
    regs.r[3] = count - moved;
    regs.r[4] = 0;
    Ok(())
}

/// `FSEntry_Args`: a stream's pointer nought, its extent and end as its
/// bytes stand, its stamp its device's, an extent set taken, its I/O
/// controls; a flush taken and no stamp read for a handle whose stream
/// closed under it, so it still closes; a pointer set and anything else
/// refused.
pub(super) fn args(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let handle = regs.r[1];
    match regs.r[0] {
        fs::args::READ_POINTER | fs::args::READ_ALLOCATED => {
            stream_device(os, handle)?;
            regs.r[2] = 0;
        }
        fs::args::READ_EXTENT => regs.r[2] = stream::extent(os, handle)?,
        fs::args::AT_END => {
            regs.r[2] = if stream::at_end(os, handle)? {
                u32::MAX
            } else {
                0
            }
        }
        fs::args::READ_STAMP => {
            let device = with(os, |state| {
                let id = state.stream(handle)?.device;
                state.device(id).map(|device| (device.load, device.exec))
            })?;
            [regs.r[2], regs.r[3]] = device.map_or([0, 0], |(load, exec)| [load, exec]);
        }
        fs::args::FLUSH => {}
        fs::args::WRITE_EXTENT | fs::args::IMAGE_STAMP => {
            stream_device(os, handle)?;
        }
        fs::args::IOCTL => stream::ioctl(os, handle, regs.r[2])?,
        _ => return Err(fail(os, BAD_OPERATION)),
    }
    Ok(())
}

/// The device of the stream `handle`; "device not found" for a stream
/// closed under its holder.
fn stream_device(os: &mut Os<'_>, handle: u32) -> Result<Device, Fail> {
    let device = with(os, |state| {
        let id = state.stream(handle)?.device;
        state.device(id).cloned()
    })?;
    device.ok_or_else(|| fail(os, NOT_FOUND))
}

/// `FSEntry_Close`: R1's stream closed.
pub(super) fn close(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    stream::shut(os, regs.r[1], stream::Closer::Holder)
}

/// `FSEntry_File`: an object's information read; its information and
/// attributes taken, and a device made that is there already, as a save
/// through an open asks; a load, delete, creation and anything else
/// refused.
pub(super) fn file(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match regs.r[0] {
        fs::file::CREATE => match named(os, regs.r[1])? {
            Named::Device(_) => Ok(()),
            Named::Root | Named::Nothing => Err(fail(os, BAD_OPERATION)),
        },
        fs::file::READ_INFO => {
            let info = match named(os, regs.r[1])? {
                Named::Root => Info {
                    kind: kind::DIRECTORY,
                    attributes: ROOT_ATTRIBUTES,
                    ..Info::default()
                },
                Named::Device(device) => with(os, |state| described(state, &device))?,
                Named::Nothing => Info::default(),
            };
            info.answer(regs);
            Ok(())
        }
        fs::file::WRITE_INFO..=fs::file::WRITE_ATTRIBUTES => Ok(()),
        _ => Err(fail(os, BAD_OPERATION)),
    }
}

/// `FSEntry_Func`: a directory set, an option and a shutdown taken, the
/// disc read as having no name, `$` listed once every driver has been told
/// it is; a rename and anything else refused.
pub(super) fn func(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match regs.r[0] {
        fs::func::SET_DIRECTORY | fs::func::OPT | fs::func::SHUT_DOWN => Ok(()),
        fs::func::READ_DISC => os.write_bytes(regs.r[2], &[0, 0]),
        fs::func::READ_NAMES => list(os, regs, Layout::Names),
        fs::func::READ_ENTRIES => list(os, regs, Layout::Info),
        fs::func::READ_FULL_ENTRIES => list(os, regs, Layout::FullInfo),
        _ => Err(fail(os, BAD_OPERATION)),
    }
}

/// `FSEntry_Func` 14, 15 and 19: every driver's `EnumDir` with R1's path,
/// then from the device at R4, up to R3 of `$`'s devices into R2's R5 bytes
/// as `layout` lays them out; R3 how many, R4 the next one's place, or -1
/// for none read.
fn list(os: &mut Os<'_>, regs: &mut Regs, layout: Layout) -> Result<(), Fail> {
    let at_path = regs.r[1];
    for target in super::targets(os)? {
        let mut enumerate = Regs::default();
        enumerate.r[2] = at_path;
        quietly(target.call(os, reason::ENUMERATE, &mut enumerate, Entry::Supervisor))?;
    }
    let of_dollar = matches!(named(os, at_path)?, Named::Root);
    let (wanted, offset) = (regs.r[3], regs.r[4]);
    let entries = if of_dollar {
        with(os, |state| {
            let chosen = || {
                state
                    .devices
                    .iter()
                    .skip(offset as usize)
                    .take(wanted as usize)
            };
            let mut entries = Vec::new();
            entries.try_reserve_exact(chosen().count()).ok()?;
            for device in chosen() {
                entries.push((fallible::copied(&device.name)?, described(state, device)));
            }
            Some(entries)
        })?
        .ok_or_else(|| os.no_memory())?
    } else {
        Vec::new()
    };
    let listed = entries.iter().map(|(name, info)| Listed {
        name,
        info: *info,
        internal: 0,
    });
    os.answer_entries(regs, layout, listed)
}
