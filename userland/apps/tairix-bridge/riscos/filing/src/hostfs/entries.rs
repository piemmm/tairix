//! HostFS's entries: what FileSwitch calls, each answered by the disc the
//! path names. FileSwitch hands every entry an absolute path — `:disc.$`
//! and the rest — HostFS having made the disc canonical by `FSEntry_Func`
//! 23, as an image host does.

use alloc::vec::Vec;

use tairix_fsmeta::preset::acorn;
use tairix_riscos_kernel::filing::fs::{
    args, file, file_info, func, open as reasons, Info, Layout, Listed,
};
use tairix_riscos_kernel::filing::{NOT_OPEN, UNSUPPORTED};
use tairix_riscos_kernel::os::{len32, quietly, Fail, Os, Regs};

use super::cache::{Name, BLOCK};
use super::protocol::{
    Object, Refusal, Reply, Request, DATA_MOST, ENTRIES_MOST, NAME_MOST, PATH_MOST,
};
use super::{
    ask, exchange, fail, must, refused, with, Opened, Place, BAD_NAME, DISC_FULL, FAILED, NO_DISC,
    TOO_LARGE,
};

/// The buffer FileSwitch keeps for a HostFS file: the largest it allows,
/// so a file crosses the relay in the fewest requests.
const BUFFER: u32 = crate::fileswitch::open::BUFFER_MOST;

/// What a path names: its disc's place and the path beneath its root.
struct Named {
    place: Place,
    path: Vec<u8>,
}

impl Named {
    /// The root a request names on the server that answers its disc.
    const fn root(&self) -> u32 {
        root_of(self.place)
    }
}

const fn root_of(place: Place) -> u32 {
    match place {
        Place::Relay(root) | Place::Memory(root) => root,
    }
}

/// The disc `name` names, the first for none.
fn disc(os: &mut Os<'_>, name: Option<&[u8]>) -> Result<Option<usize>, Fail> {
    with(os, |state| match name {
        None => (!state.discs.is_empty()).then_some(0),
        Some(name) => state
            .discs
            .iter()
            .position(|disc| acorn::names_match(&disc.name, name)),
    })
}

/// The disc and path the absolute path at `at` names.
fn named(os: &mut Os<'_>, at: u32) -> Result<Named, Fail> {
    let (text, ended) = os.read_until(at, PATH_MOST + NAME_MOST + 4, |b| b == 0)?;
    if !ended {
        return Err(fail(os, BAD_NAME));
    }
    let (disc_name, rest) = match text.strip_prefix(b":") {
        Some(rest) => {
            let end = rest.iter().position(|&b| b == b'.').unwrap_or(rest.len());
            (Some(&rest[..end]), rest.get(end + 1..).unwrap_or_default())
        }
        None => (None, &text[..]),
    };
    let path = match rest.strip_prefix(b"$") {
        Some(b"") => &[][..],
        Some(beneath) => match beneath.strip_prefix(b".") {
            Some(path) => path,
            None => return Err(fail(os, BAD_NAME)),
        },
        None => return Err(fail(os, BAD_NAME)),
    };
    let Some(index) = disc(os, disc_name)? else {
        return Err(fail(os, NO_DISC));
    };
    let place = with(os, |state| state.discs[index].place)?;
    Ok(Named {
        place,
        path: path.to_vec(),
    })
}

/// The object `named` names.
fn read_object(os: &mut Os<'_>, named: &Named) -> Result<Object, Fail> {
    let request = Request::Read {
        root: named.root(),
        path: &named.path,
    };
    match must(os, named.place, &request)? {
        Reply::Object(object) => Ok(object),
        _ => Err(fail(os, FAILED)),
    }
}

/// `object`'s catalogue information.
fn info_of(object: &Object) -> Info {
    Info {
        kind: object.kind.number(),
        load: object.load,
        exec: object.exec,
        length: object.length,
        attributes: u32::from(object.attributes),
    }
}

/// A request that answers [`Reply::Done`] made, its refusal an error.
fn done(os: &mut Os<'_>, place: Place, request: &Request<'_>) -> Result<(), Fail> {
    must(os, place, request).map(drop)
}

/// The file FileSwitch's handle `handle` names.
fn opened(os: &mut Os<'_>, handle: u32) -> Result<Opened, Fail> {
    let slot = (handle as usize).wrapping_sub(1);
    with(os, |state| state.files.get(slot).cloned().flatten())?.ok_or_else(|| os.error(NOT_OPEN))
}

/// `FSEntry_Open`: the file R1 names opened to read, or for update, for
/// FileSwitch's handle R3; R1 zero where nothing is there to open.
pub(super) fn open(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let named = named(os, regs.r[1])?;
    let request = Request::Open {
        root: named.root(),
        path: &named.path,
        update: regs.r[0] != reasons::READ,
    };
    let reply = match ask(os, named.place, &request)? {
        Ok(reply) => reply,
        Err(Refusal::NotFound | Refusal::IsADirectory) => {
            regs.r[1] = 0;
            return Ok(());
        }
        Err(refusal) => return Err(refused(os, refusal)),
    };
    let Reply::Opened {
        handle,
        extent,
        writable,
        version,
    } = reply
    else {
        return Err(fail(os, FAILED));
    };
    let Some(allocated) = extent.checked_next_multiple_of(BUFFER) else {
        let too_large = fail(os, TOO_LARGE);
        return Err(abandon(os, named.place, handle, too_large));
    };
    let slot = with(os, |state| {
        let cached = match (named.place, version) {
            (Place::Memory(_), _) => None,
            (Place::Relay(_), Some(version)) => Some(Name::Version(version)),
            (Place::Relay(_), None) => Some(state.mint()),
        };
        let file = Opened {
            place: named.place,
            handle,
            cached,
            path: named.path,
        };
        match state.files.iter().position(Option::is_none) {
            Some(slot) => {
                state.files[slot] = Some(file);
                Some(slot)
            }
            None if state.files.try_reserve(1).is_ok() => {
                state.files.push(Some(file));
                Some(state.files.len() - 1)
            }
            None => None,
        }
    })?;
    let Some(slot) = slot else {
        let no_memory = os.no_memory();
        return Err(abandon(os, named.place, handle, no_memory));
    };
    regs.r[0] = file_info::READ | if writable { file_info::WRITE } else { 0 };
    regs.r[1] = len32(slot) + 1;
    regs.r[2] = BUFFER;
    regs.r[3] = extent;
    regs.r[4] = allocated;
    Ok(())
}

/// `failure`, the server's `handle` on the file it was opening let go first;
/// an unwind while letting it go goes on in its place.
fn abandon(os: &mut Os<'_>, place: Place, handle: u32, failure: Fail) -> Fail {
    let closed = done(
        os,
        place,
        &Request::Close {
            handle,
            stamp: None,
        },
    );
    quietly(closed).map_or_else(Fail::from, |()| failure)
}

/// `FSEntry_GetBytes`: R3 bytes of the file on R1 from R4 into R2, those
/// past its end zeros.
pub(super) fn get_bytes(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let file = opened(os, regs.r[1])?;
    let (to, count, offset) = (regs.r[2], regs.r[3], regs.r[4]);
    let done = match file.cached {
        Some(name) => read_cached(os, &file, name, to, offset, count)?,
        None => read_through(os, &file, to, offset, count)?,
    };
    if done < count {
        os.fill(to.wrapping_add(done), count - done, 0)?;
    }
    Ok(())
}

// A block is fetched in one read.
const _: () = assert!(BLOCK <= DATA_MOST);

/// Up to `count` bytes of `file` from `offset` into `to`, a block at a time
/// through the read cache, under the name `name`: how many it had.
fn read_cached(
    os: &mut Os<'_>,
    file: &Opened,
    name: Name,
    to: u32,
    offset: u32,
    count: u32,
) -> Result<u32, Fail> {
    let (mut done, mut piece, mut reply) = (0, Vec::new(), Vec::new());
    while done < count {
        let at = offset.wrapping_add(done);
        let (block, within) = (at / BLOCK, at % BLOCK);
        let length = (BLOCK - within).min(count - done);
        let got = if with(os, |state| {
            state.cache.copy(name, block, within, length, &mut piece)
        })? {
            os.write_bytes(to.wrapping_add(done), &piece)?;
            len32(piece.len())
        } else {
            let request = Request::ReadBytes {
                handle: file.handle,
                offset: block * BLOCK,
                length: BLOCK,
            };
            let bytes = bytes_of(os, file.place, &request, &mut reply)?;
            let rest = bytes.get(within as usize..).unwrap_or_default();
            let rest = &rest[..rest.len().min(length as usize)];
            os.write_bytes(to.wrapping_add(done), rest)?;
            with(os, |state| state.cache.keep(name, block, bytes))?;
            len32(rest.len())
        };
        if got == 0 {
            break;
        }
        done += got;
    }
    Ok(done)
}

/// Up to `count` bytes of `file`, which is in the worker's own memory, from
/// `offset` into `to`: how many it had.
fn read_through(
    os: &mut Os<'_>,
    file: &Opened,
    to: u32,
    offset: u32,
    count: u32,
) -> Result<u32, Fail> {
    let (mut done, mut reply) = (0, Vec::new());
    while done < count {
        let request = Request::ReadBytes {
            handle: file.handle,
            offset: offset.wrapping_add(done),
            length: (count - done).min(DATA_MOST),
        };
        let bytes = bytes_of(os, file.place, &request, &mut reply)?;
        if bytes.is_empty() {
            break;
        }
        os.write_bytes(to.wrapping_add(done), bytes)?;
        done += len32(bytes.len());
    }
    Ok(done)
}

/// The bytes the read `request` gets, read from `buffer`.
fn bytes_of<'b>(
    os: &mut Os<'_>,
    place: Place,
    request: &Request<'_>,
    buffer: &'b mut Vec<u8>,
) -> Result<&'b [u8], Fail> {
    match exchange(os, place, request, buffer)? {
        Reply::Bytes(bytes) => Ok(bytes),
        Reply::Refused(refusal) => Err(refused(os, refusal)),
        _ => Err(fail(os, FAILED)),
    }
}

/// `FSEntry_PutBytes`: R3 bytes from R2 written to the file on R1 at R4; a
/// write the volume took only part of is "disc full".
pub(super) fn put_bytes(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let file = rekeyed(os, regs.r[1])?;
    let (from, count, offset) = (regs.r[2], regs.r[3], regs.r[4]);
    write_run(os, &file, offset, count, |os, at, out| {
        os.read_bytes(from.wrapping_add(at), out)
    })
}

/// The file on FileSwitch's handle `handle`, its blocks given a name of its
/// own in the read cache because it is about to change.
fn rekeyed(os: &mut Os<'_>, handle: u32) -> Result<Opened, Fail> {
    let slot = (handle as usize).wrapping_sub(1);
    let file = with(os, |state| {
        let renamed = state.mint();
        let file = state.files.get_mut(slot)?.as_mut()?;
        if file.cached.is_some() {
            file.cached = Some(renamed);
        }
        Some(file.clone())
    })?;
    file.ok_or_else(|| os.error(NOT_OPEN))
}

/// `count` bytes written to `file` at `offset`, each piece read by `source`
/// from its offset into the run.
fn write_run(
    os: &mut Os<'_>,
    file: &Opened,
    offset: u32,
    count: u32,
    mut source: impl FnMut(&mut Os<'_>, u32, &mut [u8]) -> Result<(), Fail>,
) -> Result<(), Fail> {
    let mut piece = Vec::new();
    let mut done = 0;
    while done < count {
        let length = (count - done).min(DATA_MOST);
        piece.clear();
        if piece.try_reserve(length as usize).is_err() {
            return Err(os.no_memory());
        }
        piece.resize(length as usize, 0);
        source(os, done, &mut piece)?;
        let request = Request::WriteBytes {
            handle: file.handle,
            offset: offset.wrapping_add(done),
            bytes: &piece,
        };
        match must(os, file.place, &request)? {
            Reply::Wrote(wrote) if wrote == length => done += length,
            Reply::Wrote(_) => return Err(fail(os, DISC_FULL)),
            _ => return Err(fail(os, FAILED)),
        }
    }
    Ok(())
}

/// `FSEntry_Args`: the extent set, a flush and an ensure, zeros written, the
/// stamp read; an unbuffered file's reasons are none HostFS's files need.
pub(super) fn args(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let changes = matches!(regs.r[0], args::WRITE_EXTENT | args::ZEROS);
    let file = if changes {
        rekeyed(os, regs.r[1])?
    } else {
        opened(os, regs.r[1])?
    };
    match regs.r[0] {
        args::WRITE_EXTENT => done(
            os,
            file.place,
            &Request::SetExtent {
                handle: file.handle,
                extent: regs.r[2],
            },
        ),
        args::FLUSH => done(
            os,
            file.place,
            &Request::Sync {
                handle: file.handle,
            },
        ),
        args::ENSURE => {
            regs.r[2] = regs.r[2]
                .checked_next_multiple_of(BUFFER)
                .unwrap_or(regs.r[2]);
            Ok(())
        }
        args::ZEROS => write_run(os, &file, regs.r[2], regs.r[3], |_, _, out| {
            out.fill(0);
            Ok(())
        }),
        args::READ_STAMP => {
            let named = Named {
                place: file.place,
                path: file.path,
            };
            let object = read_object(os, &named)?;
            regs.r[2] = object.load;
            regs.r[3] = object.exec;
            Ok(())
        }
        args::IMAGE_STAMP => Ok(()),
        _ => Err(os.error(UNSUPPORTED)),
    }
}

/// `FSEntry_Close`: the file on R1 closed, restamped R2 and R3 where they
/// are not both zero.
pub(super) fn close(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let file = opened(os, regs.r[1])?;
    let slot = (regs.r[1] as usize).wrapping_sub(1);
    with(os, |state| {
        if let Some(entry) = state.files.get_mut(slot) {
            *entry = None;
        }
    })?;
    let stamp = (regs.r[2] != 0 || regs.r[3] != 0).then_some((regs.r[2], regs.r[3]));
    done(
        os,
        file.place,
        &Request::Close {
            handle: file.handle,
            stamp,
        },
    )
}

/// `FSEntry_File`: an object read, made, restamped or deleted; its natural
/// block size. HostFS loads and saves through its open files.
pub(super) fn file(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let named = named(os, regs.r[1])?;
    let (root, path) = (named.root(), &named.path[..]);
    match regs.r[0] {
        file::READ_INFO | file::READ_INFO_NO_LENGTH => {
            let object = read_object(os, &named)?;
            info_of(&object).answer(regs);
            Ok(())
        }
        file::CREATE => done(
            os,
            named.place,
            &Request::Create {
                root,
                path,
                load: regs.r[2],
                exec: regs.r[3],
                length: regs.r[5].wrapping_sub(regs.r[4]),
            },
        ),
        file::CREATE_DIRECTORY => done(
            os,
            named.place,
            &Request::CreateDir {
                root,
                path,
                load: regs.r[2],
                exec: regs.r[3],
            },
        ),
        file::WRITE_INFO => done(
            os,
            named.place,
            &Request::WriteInfo {
                root,
                path,
                load: regs.r[2],
                exec: regs.r[3],
                attributes: regs.byte(5),
            },
        ),
        file::WRITE_ATTRIBUTES => {
            let object = read_object(os, &named)?;
            done(
                os,
                named.place,
                &Request::WriteInfo {
                    root,
                    path,
                    load: object.load,
                    exec: object.exec,
                    attributes: regs.byte(5),
                },
            )
        }
        file::DELETE => match must(os, named.place, &Request::Delete { root, path })? {
            Reply::Object(object) => {
                info_of(&object).answer(regs);
                Ok(())
            }
            _ => Err(fail(os, FAILED)),
        },
        file::BLOCK_SIZE => {
            regs.r[2] = BUFFER;
            Ok(())
        }
        _ => Err(os.error(UNSUPPORTED)),
    }
}

/// `FSEntry_Func`: a rename, a disc's name and boot option, directories
/// listed, a disc made canonical; the rest taken or refused.
pub(super) fn func(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match regs.r[0] {
        func::SET_DIRECTORY
        | func::OPT
        | func::SHUT_DOWN
        | func::NEW_IMAGE
        | func::CLOSING_IMAGE => Ok(()),
        func::RENAME => rename(os, regs),
        func::READ_DISC => read_disc(os, regs),
        func::READ_NAMES => list(os, regs, Layout::Names),
        func::READ_ENTRIES => list(os, regs, Layout::Info),
        func::READ_FULL_ENTRIES => list(os, regs, Layout::FullInfo),
        func::CANONICAL => canonical(os, regs),
        func::READ_BOOT_OPTION => {
            regs.r[2] = 0;
            Ok(())
        }
        _ => Err(os.error(UNSUPPORTED)),
    }
}

/// `FSEntry_Func` 8: R1's object renamed R2, R1 zero where it was; nonzero
/// across discs, which FileSwitch reports as a bad rename.
fn rename(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let from = named(os, regs.r[1])?;
    let to = named(os, regs.r[2])?;
    if from.place != to.place {
        regs.r[1] = 1;
        return Ok(());
    }
    done(
        os,
        from.place,
        &Request::Rename {
            root: from.root(),
            from: &from.path,
            to: &to.path,
        },
    )?;
    regs.r[1] = 0;
    Ok(())
}

/// `FSEntry_Func` 11: the name of the disc R1 names, after its length, and
/// a boot option of 0, at R2.
fn read_disc(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let at = regs.r[1];
    let name = if at == 0 || os.read_byte(at)? == 0 {
        with(os, |state| {
            state.discs.first().map(|disc| disc.name.clone())
        })?
    } else {
        let named_disc = named(os, at)?.place;
        with(os, |state| {
            state
                .discs
                .iter()
                .find(|disc| disc.place == named_disc)
                .map(|disc| disc.name.clone())
        })?
    };
    let name = name.unwrap_or_default();
    let length = u8::try_from(name.len()).map_err(|_| fail(os, BAD_NAME))?;
    os.write_byte(regs.r[2], length)?;
    os.write_bytes(regs.r[2].wrapping_add(1), &name)?;
    os.write_byte(regs.r[2].wrapping_add(1 + u32::from(length)), 0)
}

/// `FSEntry_Func` 14, 15 and 19: from R4, up to R3 of the entries of the
/// directory R1 names into R2's R5 bytes; R3 how many, R4 where to go on,
/// -1 once none are left.
fn list(os: &mut Os<'_>, regs: &mut Regs, layout: Layout) -> Result<(), Fail> {
    let named = named(os, regs.r[1])?;
    let (buffer, wanted, offset, room) = (regs.r[2], regs.r[3], regs.r[4], regs.r[5]);
    let request = Request::List {
        root: named.root(),
        path: &named.path,
        offset,
        count: wanted.min(ENTRIES_MOST),
    };
    let Reply::Entries { next, entries } = must(os, named.place, &request)? else {
        return Err(fail(os, FAILED));
    };
    let listed = entries.iter().map(|(name, object)| Listed {
        name,
        info: info_of(object),
        internal: 0,
    });
    let read = os.write_entries(buffer, room, layout, listed)?;
    regs.r[3] = read;
    regs.r[4] = if read as usize == entries.len() {
        next
    } else {
        offset.wrapping_add(read)
    };
    Ok(())
}

/// `FSEntry_Func` 23: the disc R2 names, the first for none, made canonical
/// as its exact name; HostFS has no special fields. Asked for lengths alone
/// where R3 and R4 give no buffers.
fn canonical(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let given = if regs.r[2] == 0 {
        None
    } else {
        Some(os.read_until(regs.r[2], NAME_MOST + 1, |b| b < b' ')?.0)
    };
    let Some(index) = disc(os, given.as_deref())? else {
        return Err(fail(os, NO_DISC));
    };
    let name = with(os, |state| state.discs[index].name.clone())?;
    regs.r[1] = 0;
    if regs.r[4] == 0 {
        regs.r[3] = 0;
        regs.r[4] = len32(name.len());
        regs.r[2] = 0;
        return Ok(());
    }
    os.write_terminated(regs.r[4], &name, 0, regs.r[6] as usize)?;
    regs.r[2] = regs.r[4];
    regs.r[3] = 0;
    regs.r[4] = len32(name.len()).saturating_sub(regs.r[6].saturating_sub(1));
    Ok(())
}
