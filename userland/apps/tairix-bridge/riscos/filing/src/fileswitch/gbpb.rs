//! `OS_GBPB` (`plans/RISCOS-EMULATOR.md` §6): bytes moved between memory and
//! an open file, and the names and catalogue information a directory holds.
//!
//! A directory is read through `FSEntry_Func` 14, 15 and 19 into the
//! caller's own buffer, and what a wildcard does not match is taken out of
//! it there; reason 12, which no entry gives, is read an object at a time
//! through `FSEntry_Func` 15 and given its file type.

use alloc::vec::Vec;

use tairix_riscos_arm::psr;
use tairix_riscos_kernel::filing::fs::{self, Layout};
use tairix_riscos_kernel::filing::BAD_GBPB;
use tairix_riscos_kernel::os::{Fail, Os, Regs};

use super::call::{self, Need, DIRECTORY, FILE as A_FILE};
use super::name::{self, Dir, Search};
use super::open::status;
use super::{error, open, with};

/// `GBPBV`'s default.
pub(crate) fn gbpb(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match regs.r[0] {
        1 => write(os, regs, Some(regs.r[4])),
        2 => write(os, regs, None),
        3 => read(os, regs, Some(regs.r[4])),
        4 => read(os, regs, None),
        5 => disc_name(os, regs),
        6 => directory_name(os, regs, Dir::Current),
        7 => directory_name(os, regs, Dir::Library),
        8 => current_entries(os, regs),
        9 => entries(os, regs, Layout::Names),
        10 => entries(os, regs, Layout::Info),
        11 => entries(os, regs, Layout::FullInfo),
        12 => typed_entries(os, regs),
        _ => Err(os.error(BAD_GBPB)),
    }
}

/// What a transfer between memory and an open file did: the address past
/// what moved, the bytes that did not, and the pointer after.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Moved {
    pub(crate) address: u32,
    pub(crate) left: u32,
    pub(crate) pointer: u32,
}

impl Moved {
    /// Into R2 to R4, C set where bytes were left.
    fn answer(self, regs: &mut Regs) {
        regs.r[2] = self.address;
        regs.r[3] = self.left;
        regs.r[4] = self.pointer;
        regs.set_flag(psr::C, self.left != 0);
    }
}

/// `OS_GBPB` 1 and 2: R3 bytes from R2 written at R4, or at the pointer.
fn write(os: &mut Os<'_>, regs: &mut Regs, at: Option<u32>) -> Result<(), Fail> {
    let moved = write_file(os, regs.r[1], regs.r[2], regs.r[3], at)?;
    regs.r[2] = moved.address;
    regs.r[3] = moved.left;
    regs.r[4] = moved.pointer;
    Ok(())
}

/// `OS_GBPB` 3 and 4: R3 bytes read to R2 from R4, or from the pointer, up
/// to the end.
fn read(os: &mut Os<'_>, regs: &mut Regs, at: Option<u32>) -> Result<(), Fail> {
    read_file(os, regs.r[1], regs.r[2], regs.r[3], at)?.answer(regs);
    Ok(())
}

/// `count` bytes from `from` written to the file open on `handle`, at `at`
/// or its pointer. A file not open for update is refused but counted as
/// changed, as RISC OS 5 counts it, so it is restamped when it closes.
pub(crate) fn write_file(
    os: &mut Os<'_>,
    handle: u32,
    from: u32,
    count: u32,
    at: Option<u32>,
) -> Result<Moved, Fail> {
    let mut stream = open::stream(os, handle)?;
    if !stream.has(status::WRITE) {
        stream.mark(status::MODIFIED, true);
        open::put(os, handle, stream)?;
        return Err(error::fail(os, error::NOT_FOR_UPDATE, &[]));
    }
    let moved = match stream.buffer {
        Some(mut buffer) => {
            let start = at.unwrap_or(buffer.pointer);
            let done = open::write_buffered(os, &stream, &mut buffer, from, start, count);
            if done.is_ok() {
                buffer.pointer = start + count;
            }
            let pointer = buffer.pointer;
            stream.buffer = Some(buffer);
            done.map(|()| Moved {
                address: from.wrapping_add(count),
                left: 0,
                pointer,
            })
        }
        None => unbuffered(
            os,
            &stream,
            if at.is_some() { 1 } else { 2 },
            from,
            count,
            at,
        ),
    };
    stream.mark(status::MODIFIED, true);
    stream.mark(status::AT_END, false);
    open::put(os, handle, stream)?;
    moved
}

/// `count` bytes read to `to` from the file open on `handle`, from `at` or
/// its pointer, up to its end.
pub(crate) fn read_file(
    os: &mut Os<'_>,
    handle: u32,
    to: u32,
    count: u32,
    at: Option<u32>,
) -> Result<Moved, Fail> {
    let mut stream = open::stream(os, handle)?;
    if !stream.readable() {
        return Err(error::fail(os, error::NOT_FOR_READING, &[]));
    }
    let moved = match stream.buffer {
        Some(mut buffer) => {
            let start = at.unwrap_or(buffer.pointer);
            let read = open::read_buffered(os, &stream, &mut buffer, to, start, count);
            if let Ok(read) = read {
                buffer.pointer = start + read;
            }
            let pointer = buffer.pointer;
            stream.buffer = Some(buffer);
            read.map(|read| Moved {
                address: to.wrapping_add(read),
                left: count - read,
                pointer,
            })
        }
        None => unbuffered(os, &stream, if at.is_some() { 3 } else { 4 }, to, count, at),
    };
    stream.mark(status::AT_END, false);
    open::put(os, handle, stream)?;
    moved
}

/// A transfer on an unbuffered file: the filing system's own `FSEntry_GBPB`
/// where it serves the file, else a byte at a time from the pointer, set
/// first to `at` where one is given, and the pointer read back after.
fn unbuffered(
    os: &mut Os<'_>,
    stream: &open::Stream,
    reason: u32,
    address: u32,
    count: u32,
    at: Option<u32>,
) -> Result<Moved, Fail> {
    if stream.info & open::file_info::GBPB != 0 {
        let mut call = Regs::default();
        call.r[2] = address;
        call.r[3] = count;
        call.r[4] = at.unwrap_or(0);
        call::gbpb(os, &stream.calls, reason, stream.handle, &mut call)?;
        return Ok(Moved {
            address: call.r[2],
            left: call.r[3],
            pointer: call.r[4],
        });
    }
    if let Some(at) = at {
        call::args(
            os,
            &stream.calls,
            fs::args::WRITE_POINTER,
            stream.handle,
            at,
            0,
        )?;
    }
    let writing = matches!(reason, 1 | 2);
    let mut done = 0;
    while done < count {
        let here = address.wrapping_add(done);
        if writing {
            let byte = os.read_byte(here)?;
            call::put_byte(os, &stream.calls, stream.handle, byte)?;
        } else {
            match call::get_byte(os, &stream.calls, stream.handle)? {
                Some(byte) => os.write_byte(here, byte)?,
                None => break,
            }
        }
        done += 1;
    }
    let (pointer, _) = call::args(
        os,
        &stream.calls,
        fs::args::READ_POINTER,
        stream.handle,
        0,
        0,
    )?;
    Ok(Moved {
        address: address.wrapping_add(done),
        left: count - done,
        pointer,
    })
}

/// What `OS_GBPB` 5 to 7 give for a directory never set.
const UNSET: &[u8] = b"\"Unset\"";
/// The room `OS_GBPB` 8 tells a filing system the caller's buffer has, which
/// the call does not say: as RISC OS 5 tells it.
const CALLER_ROOM: u32 = 0x10_0000;

/// The temporary filing system's directory `which`, as it was set.
fn selection(os: &mut Os<'_>, which: Dir) -> Result<Option<name::Selection>, Fail> {
    let found = with(os, |state| {
        let number = state.temporary()?;
        let (_, directories) = state.directories.iter().find(|(n, _)| *n == number)?;
        directories.get(which).cloned()
    })?;
    Ok(found)
}

/// `bytes` written at `at` after a length byte.
fn counted(os: &mut Os<'_>, at: u32, bytes: &[u8]) -> Result<u32, Fail> {
    let length = u8::try_from(bytes.len()).map_err(|_| error::bad_name(os))?;
    os.write_byte(at, length)?;
    os.write_bytes(at.wrapping_add(1), bytes)?;
    Ok(at.wrapping_add(1 + u32::from(length)))
}

/// `OS_GBPB` 5: the disc the current directory was set on, and a boot
/// option of 0, at R2, asking the filing system nothing, as RISC OS 5 gives
/// them: `"Unset"` where no directory names its disc.
fn disc_name(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let set = selection(os, Dir::Current)?;
    let disc = set
        .as_ref()
        .and_then(|set| set.path.strip_prefix(b":"))
        .map_or(UNSET, first_part)
        .to_vec();
    let after = counted(os, regs.r[2], &disc)?;
    os.write_byte(after, 0)
}

/// `path`'s first part, before its first full stop.
fn first_part(path: &[u8]) -> &[u8] {
    path.split(|&b| b == b'.').next().unwrap_or(path)
}

/// `OS_GBPB` 6 and 7: the directory's leaf name, after a zero byte and its
/// length, and the owner's privilege byte, at R2, asking the filing system
/// nothing.
fn directory_name(os: &mut Os<'_>, regs: &mut Regs, which: Dir) -> Result<(), Fail> {
    let leaf = match selection(os, which)? {
        Some(set) => name::leaf(&set.path).to_vec(),
        None => UNSET.to_vec(),
    };
    os.write_byte(regs.r[2], 0)?;
    let after = counted(os, regs.r[2].wrapping_add(1), &leaf)?;
    os.write_byte(after, 0)
}

/// `OS_GBPB` 8: names in the current directory from R4, each after its
/// length, at R2: read by one `FSEntry_Func` 14 into the caller's own buffer
/// and counted there in place; R3 those not read, R4 where to go on, and C
/// set where any were not.
fn current_entries(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (mut resolved, found) = name::find_named(os, b"@", Search::Plain, Need::Kind)?;
    super::image::root(os, &mut resolved, &found)?;
    let (buffer, wanted, offset) = (regs.r[2], regs.r[3], regs.r[4]);
    let mut call = Regs::default();
    call.r[2] = buffer;
    call.r[3] = wanted;
    call.r[4] = offset;
    call.r[5] = CALLER_ROOM;
    call::func(
        os,
        &resolved.calls,
        fs::func::READ_NAMES,
        resolved.named(),
        0,
        &mut call,
        |_, _, _| Ok(()),
    )?;
    let mut read = 0;
    let mut at = buffer;
    for _ in 0..call.r[3].min(wanted) {
        let (name, ended) = os.read_until(at, name::NAME_MOST, |b| b == 0)?;
        if !ended {
            break;
        }
        at = counted(os, at, &name)?;
        read += 1;
    }
    regs.r[3] = wanted - read;
    regs.r[4] = if read == 0 { offset } else { call.r[4] };
    regs.set_flag(psr::C, regs.r[3] != 0);
    Ok(())
}

/// The directory R1 names, resolved and read — an image file's being the
/// image's root: refused where it is not one.
fn directory(os: &mut Os<'_>, regs: &Regs) -> Result<name::Resolved, Fail> {
    let (mut resolved, found) = name::find(os, regs.r[1], Search::Plain, Need::Kind)?;
    super::image::root(os, &mut resolved, &found)?;
    // An empty name, which only a filing system that takes one is given, is
    // that filing system's own directory, as RISC OS 5 lists `null:`.
    let listed = found.kind == DIRECTORY || found.kind == call::IMAGE || resolved.path.is_empty();
    if !listed {
        let given = name::read(os, regs.r[1])?;
        let error = if found.kind == A_FILE {
            tairix_riscos_kernel::filing::IS_A_FILE
        } else {
            tairix_riscos_kernel::filing::NOT_FOUND
        };
        return Err(name::named_error(os, error, &given));
    }
    Ok(resolved)
}

/// The wildcard R6 gives, none for `*` or no pointer.
fn wildcard(os: &mut Os<'_>, at: u32) -> Result<Option<Vec<u8>>, Fail> {
    if at == 0 {
        return Ok(None);
    }
    let pattern = name::read(os, at)?;
    Ok((pattern != b"*").then_some(pattern))
}

/// `OS_GBPB` 9 to 11: R3 entries of the directory R1 names from R4, read
/// into R2's R5 bytes by the filing system, those R6's wildcard does not
/// match taken out; R3 those read, R4 where to go on, C set where any were.
fn entries(os: &mut Os<'_>, regs: &mut Regs, layout: Layout) -> Result<(), Fail> {
    let resolved = directory(os, regs)?;
    let pattern = wildcard(os, regs.r[6])?;
    let mut call = Regs::default();
    call.r[2..6].copy_from_slice(&regs.r[2..6]);
    call::func(
        os,
        &resolved.calls,
        layout.reason(),
        resolved.named(),
        0,
        &mut call,
        |_, _, _| Ok(()),
    )?;
    let mut count = call.r[3].min(regs.r[3]);
    if let Some(pattern) = pattern {
        count = keep_matching(os, regs.r[2], regs.r[5], count, layout, &pattern)?;
    }
    regs.r[3] = count;
    regs.r[4] = call.r[4];
    regs.set_flag(psr::C, count != 0);
    Ok(())
}

/// The `count` entries at `buffer`, laid out as `layout` lays them,
/// compacted to those whose name `pattern` matches: how many are left.
fn keep_matching(
    os: &mut Os<'_>,
    buffer: u32,
    size: u32,
    count: u32,
    layout: Layout,
    pattern: &[u8],
) -> Result<u32, Fail> {
    let name_at = layout.name_at();
    let (mut from, mut to) = (0u32, 0u32);
    let mut kept = 0;
    let mut entry = Vec::new();
    for _ in 0..count {
        let at = buffer.wrapping_add(from);
        let (name, ended) = os.read_until(at.wrapping_add(name_at), name::NAME_MOST, |b| b == 0)?;
        let length = layout.size(&name);
        if !ended || from.saturating_add(length) > size {
            break;
        }
        if name::matches(pattern, &name) {
            if to != from {
                entry.resize(length as usize, 0);
                os.read_bytes(at, &mut entry)?;
                os.write_bytes(buffer.wrapping_add(to), &entry)?;
            }
            to += length;
            kept += 1;
        }
        from += length;
    }
    Ok(kept)
}

/// The smallest record `OS_GBPB` 12 gives: six words, then a one-character
/// name and its terminator padded to a word.
const SMALLEST_TYPED: u32 = 28;
/// Where `OS_GBPB` 12's record holds its object's file type: after the
/// record reason 10 gives, the name a word later.
const TYPE_AT: u32 = Layout::Info.name_at();

/// `OS_GBPB` 12: as reason 10 with each object's file type after its
/// record, made in place as RISC OS 5 makes it. The filing system's
/// `FSEntry_Func` 15 writes its records into the caller's buffer a word in
/// for each of the most records the buffer could hold, and each is moved
/// down to its final place with its type added; the padding after a name is
/// left as the filing system's record left it.
fn typed_entries(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let resolved = directory(os, regs)?;
    let pattern = wildcard(os, regs.r[6])?;
    let (buffer, wanted, size) = (regs.r[2], regs.r[3], regs.r[5]);
    let headroom = 4 * (size / SMALLEST_TYPED);
    let mut call = Regs::default();
    call.r[2] = buffer.wrapping_add(headroom);
    call.r[3] = wanted;
    call.r[4] = regs.r[4];
    call.r[5] = size - headroom;
    call::func(
        os,
        &resolved.calls,
        fs::func::READ_ENTRIES,
        resolved.named(),
        0,
        &mut call,
        |_, _, _| Ok(()),
    )?;
    let (mut from, mut to, mut kept) = (headroom, 0u32, 0);
    for _ in 0..call.r[3].min(wanted) {
        let record = buffer.wrapping_add(from);
        let info = os.read_listed(record)?;
        let (name, ended) = os.read_until(
            record.wrapping_add(Layout::Info.name_at()),
            name::NAME_MOST,
            |b| b == 0,
        )?;
        let length = Layout::Info.size(&name);
        if !ended || from.saturating_add(length) > size {
            break;
        }
        from += length;
        if pattern.as_deref().is_some_and(|p| !name::matches(p, &name)) {
            continue;
        }
        // Names too short for the headroom's sizing could carry a record
        // over one not yet read, or past the buffer.
        if to.saturating_add(length + 4) > from {
            break;
        }
        let typed = buffer.wrapping_add(to);
        os.write_words(typed, &info.record())?;
        os.write_word(
            typed.wrapping_add(TYPE_AT),
            super::file::object_type(info.kind, info.load, &name),
        )?;
        os.write_terminated(typed.wrapping_add(TYPE_AT + 4), &name, 0, name.len() + 1)?;
        to += length + 4;
        kept += 1;
    }
    regs.r[3] = kept;
    regs.r[4] = call.r[4];
    regs.set_flag(psr::C, kept != 0);
    Ok(())
}
