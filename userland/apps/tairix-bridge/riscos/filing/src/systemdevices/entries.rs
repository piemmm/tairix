//! SystemDevices' filing system entries, which FileSwitch calls for all six
//! devices, and each device's reads and writes. No device has files: every
//! name reads as nothing there, a directory or a deletion is taken and
//! ignored, and only the devices that write may be saved to.

use alloc::vec::Vec;

use tairix_riscos_arm::psr;
use tairix_riscos_kernel::filing::fs::{self, file_info, Info};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_riscos_kernel::swi::os::{gbpb, BYTE, READ_C, READ_LINE32};
use tairix_riscos_kernel::swi::X;

use super::{called, printer, refused, with, Device, Kbd, Line, LINE_AT, LINE_MOST};

/// `OS_Byte` 3, and its value while `vdu:` and `rawvdu:` write: the screen
/// alone, neither printed nor spooled, as RISC OS 5 sets it.
const OUTPUT_STREAMS: u32 = 3;
const SCREEN_ONLY: u32 = 0x14;

/// Ctrl-D, at which `kbd:`'s input ends.
const END_OF_INPUT: u8 = 0x04;

/// The longest special field taken: a containment bound on text read from
/// guest memory.
const SPECIAL_MOST: usize = 255;

/// `FSEntry_Open`: a stream for FileSwitch's handle R3, to read for reason
/// nought and to write for any other, on a device that does so; R1 its
/// handle, which is FileSwitch's, or nought where `kbd:` has its one or the
/// file `printer:` writes to cannot be opened.
pub(super) fn open(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let device = called(os, regs)?;
    let handle = regs.r[3];
    let reading = regs.r[0] == fs::open::READ;
    let info = match (device, reading) {
        (Device::Null, true) => file_info::READ | file_info::GBPB,
        (Device::Null, false) => file_info::READ | file_info::WRITE | file_info::GBPB,
        (Device::Kbd, true) => {
            let free = with(os, |state| {
                let free = state.kbd.is_none();
                if free {
                    state.kbd = Some(Kbd { handle, line: None });
                }
                free
            })?;
            if !free {
                regs.r[1] = 0;
                return Ok(());
            }
            file_info::READ | file_info::INTERACTIVE
        }
        (Device::RawKbd, true) => file_info::READ | file_info::INTERACTIVE,
        (Device::Vdu | Device::RawVdu, false) => file_info::WRITE,
        (Device::Printer, false) => {
            let special = special_field(os, regs.r[6])?;
            if !printer::open(os, handle, special.as_deref())? {
                regs.r[1] = 0;
                return Ok(());
            }
            file_info::WRITE | file_info::GBPB
        }
        _ => return Err(refused(os, device)),
    };
    regs.r[..5].copy_from_slice(&[info, handle, 0, 0, 0]);
    Ok(())
}

/// The special field at `at`, none for nought.
fn special_field(os: &mut Os<'_>, at: u32) -> Result<Option<Vec<u8>>, Fail> {
    match at {
        0 => Ok(None),
        at => Ok(Some(os.read_until(at, SPECIAL_MOST, |b| b < b' ')?.0)),
    }
}

/// `FSEntry_GetBytes`: the next byte of R1's stream in R0, C set at its end.
pub(super) fn get_bytes(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let device = called(os, regs)?;
    let byte = match device {
        Device::Null => None,
        Device::Kbd => kbd_byte(os, regs.r[1])?,
        Device::RawKbd => Some(key(os)?),
        _ => return Err(refused(os, device)),
    };
    if let Some(byte) = byte {
        regs.r[0] = u32::from(byte);
    }
    regs.set_flag(psr::C, byte.is_none());
    Ok(())
}

/// The next byte of `kbd:`'s line, a line read where none is held; none at
/// a Ctrl-D, which stays where it is, so the stream stays at its end.
fn kbd_byte(os: &mut Os<'_>, handle: u32) -> Result<Option<u8>, Fail> {
    let held = with(os, |state| {
        state
            .kbd
            .filter(|kbd| kbd.handle == handle)
            .map(|kbd| kbd.line)
    })?;
    let line = match held {
        None => return Err(refused(os, Device::Kbd)),
        Some(Some(line)) => line,
        Some(None) => read_line(os)?,
    };
    let workspace = with(os, |state| state.workspace)?;
    let byte = os.read_byte(workspace + LINE_AT + line.taken)?;
    let left = if byte == END_OF_INPUT {
        Some(line)
    } else {
        let taken = line.taken + 1;
        (taken < line.length).then_some(Line {
            length: line.length,
            taken,
        })
    };
    with(os, |state| {
        if let Some(kbd) = state.kbd.as_mut().filter(|kbd| kbd.handle == handle) {
            kbd.line = left;
        }
    })?;
    Ok((byte != END_OF_INPUT).then_some(byte))
}

/// A line read by `OS_ReadLine`, its editing and echo its own, every
/// character kept; "Escape", acknowledged, where Escape ends it.
fn read_line(os: &mut Os<'_>) -> Result<Line, Fail> {
    let workspace = with(os, |state| state.workspace)?;
    let mut regs = Regs::default();
    regs.r[..4].copy_from_slice(&[workspace + LINE_AT, LINE_MOST, 0, 0xFF]);
    os.perform(X | READ_LINE32, &mut regs)?;
    if regs.flag(psr::C) {
        return Err(os.escape_acknowledged());
    }
    Ok(Line {
        length: regs.r[1].min(LINE_MOST) + 1,
        taken: 0,
    })
}

/// A key read by `OS_ReadC`, unechoed; "Escape", acknowledged, for Escape.
fn key(os: &mut Os<'_>) -> Result<u8, Fail> {
    let mut regs = Regs::default();
    os.perform(X | READ_C, &mut regs)?;
    if regs.flag(psr::C) {
        return Err(os.escape_acknowledged());
    }
    Ok(regs.byte(0))
}

/// `FSEntry_PutBytes`: R0's byte to R1's stream.
pub(super) fn put_bytes(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let device = called(os, regs)?;
    let byte = regs.byte(0);
    match device {
        Device::Null => Ok(()),
        Device::Vdu | Device::RawVdu => to_screen(os, device, 1, |_, _| Ok(byte)),
        Device::Printer => printer::put(os, regs.r[1], byte),
        _ => Err(refused(os, device)),
    }
}

/// `count` bytes, each `byte_at` its place, written to the screen alone:
/// by `rawvdu:` as they are, by `vdu:` as its listing shows them.
fn to_screen(
    os: &mut Os<'_>,
    device: Device,
    count: u32,
    mut byte_at: impl FnMut(&mut Os<'_>, u32) -> Result<u8, Fail>,
) -> Result<(), Fail> {
    let old = output_streams(os, SCREEN_ONLY)?;
    let written = (|| {
        let mut shown = Vec::new();
        for place in 0..count {
            let byte = byte_at(os, place)?;
            if device == Device::RawVdu {
                os.write_text(&[byte])?;
                continue;
            }
            let (listing, mut progress) = with(os, |state| (state.listing, state.progress))?;
            shown.clear();
            listing.feed(&mut progress, byte, &mut shown);
            with(os, |state| state.progress = progress)?;
            os.write_text(&shown)?;
        }
        Ok(())
    })();
    let restored = output_streams(os, old).map(drop);
    written.and(restored)
}

/// `OS_Byte` 3 set to `value`: the value it had.
fn output_streams(os: &mut Os<'_>, value: u32) -> Result<u32, Fail> {
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[OUTPUT_STREAMS, value]);
    os.perform(X | BYTE, &mut regs)?;
    Ok(regs.r[1])
}

/// `FSEntry_GBPB`: R3 bytes between R2 and R1's stream, written for 2 and
/// read for 4, R2 and R3 moved on; none at a pointer, which a device has
/// not got. `null:` takes every byte and gives none.
pub(super) fn gbpb(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let device = called(os, regs)?;
    let [reason, handle, at, count, ..] = regs.r;
    let moved = match (device, reason) {
        (Device::Null, gbpb::WRITE) => count,
        (Device::Null, gbpb::READ) => 0,
        (Device::Printer, gbpb::WRITE) => printer::write(os, handle, at, count)?,
        _ => return Err(refused(os, device)),
    };
    regs.r[2] = at.wrapping_add(moved);
    regs.r[3] = count - moved;
    regs.r[4] = 0;
    Ok(())
}

/// `FSEntry_Args`: a pointer, extent and allocation of nought, and nothing
/// stamped, setting any of them taken and ignored; at its end where the
/// device has no more to give.
pub(super) fn args(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let device = called(os, regs)?;
    match regs.r[0] {
        fs::args::READ_POINTER
        | fs::args::READ_EXTENT
        | fs::args::READ_ALLOCATED
        | fs::args::ENSURE => regs.r[2] = 0,
        fs::args::AT_END => {
            regs.r[2] = if at_end(os, device, regs.r[1])? {
                u32::MAX
            } else {
                0
            }
        }
        fs::args::READ_STAMP => {
            regs.r[2] = 0;
            regs.r[3] = 0;
        }
        fs::args::WRITE_POINTER
        | fs::args::WRITE_EXTENT
        | fs::args::FLUSH
        | fs::args::ZEROS
        | fs::args::IMAGE_STAMP => {}
        _ => return Err(refused(os, device)),
    }
    Ok(())
}

/// Whether the stream `handle` is at its end: `kbd:`'s where its next byte
/// is a Ctrl-D, never `rawkbd:`'s, and always the rest's.
fn at_end(os: &mut Os<'_>, device: Device, handle: u32) -> Result<bool, Fail> {
    match device {
        Device::Kbd => {
            let held = with(os, |state| {
                let line = state.kbd.filter(|kbd| kbd.handle == handle)?.line?;
                Some(state.workspace + LINE_AT + line.taken)
            })?;
            match held {
                Some(next) => Ok(os.read_byte(next)? == END_OF_INPUT),
                None => Ok(false),
            }
        }
        Device::RawKbd => Ok(false),
        _ => Ok(true),
    }
}

/// `FSEntry_Close`: R1's stream closed, the rest of a `kbd:` line with it.
pub(super) fn close(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let device = called(os, regs)?;
    let handle = regs.r[1];
    match device {
        Device::Kbd => with(os, |state| {
            if state.kbd.is_some_and(|kbd| kbd.handle == handle) {
                state.kbd = None;
            }
        }),
        Device::Printer => printer::close(os, handle),
        _ => Ok(()),
    }
}

/// `FSEntry_File`: nothing there whatever the name; R4 to R5 saved to a
/// device that writes; information written, a deletion, a directory made
/// and a device that writes made all taken and ignored; a load and anything
/// else refused.
pub(super) fn file(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let device = called(os, regs)?;
    let writes = !matches!(device, Device::Kbd | Device::RawKbd);
    match regs.r[0] {
        fs::file::SAVE if writes => save(os, device, regs),
        fs::file::READ_INFO => {
            Info::default().answer(regs);
            Ok(())
        }
        fs::file::CREATE if writes => Ok(()),
        fs::file::WRITE_INFO..=fs::file::WRITE_ATTRIBUTES
        | fs::file::DELETE
        | fs::file::CREATE_DIRECTORY => Ok(()),
        _ => Err(refused(os, device)),
    }
}

/// `FSEntry_File` 0: R4 to R5 written to the device as a stream on it would
/// write them.
fn save(os: &mut Os<'_>, device: Device, regs: &Regs) -> Result<(), Fail> {
    let start = regs.r[4];
    let count = regs.r[5].wrapping_sub(start);
    match device {
        Device::Vdu | Device::RawVdu => to_screen(os, device, count, |os, place| {
            os.read_byte(start.wrapping_add(place))
        }),
        Device::Printer => {
            let special = special_field(os, regs.r[6])?;
            printer::save(os, special.as_deref(), start, count)
        }
        _ => Ok(()),
    }
}

/// `FSEntry_Func`: a directory set, an option and a shutdown taken, the
/// disc read as having no name, and a directory listed as empty; anything
/// else refused.
pub(super) fn func(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let device = called(os, regs)?;
    match regs.r[0] {
        fs::func::SET_DIRECTORY | fs::func::OPT | fs::func::SHUT_DOWN => Ok(()),
        fs::func::READ_DISC => os.write_bytes(regs.r[2], &[0, 0]),
        fs::func::READ_NAMES | fs::func::READ_ENTRIES | fs::func::READ_FULL_ENTRIES => {
            regs.r[3] = 0;
            regs.r[4] = u32::MAX;
            Ok(())
        }
        _ => Err(refused(os, device)),
    }
}
