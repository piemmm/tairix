//! PipeFS's filing system entries, which FileSwitch calls: `pipe:$` the
//! directory of every pipe, a dotted name's parts directories below it,
//! and each pipe opened once from either end.

use alloc::vec::Vec;

use tairix_riscos_arm::psr;
use tairix_riscos_kernel::filing::fs::{self, file_info, kind, Info, Layout, Listed};
use tairix_riscos_kernel::os::{Fail, Os, Regs};
use tairix_util::fallible;

use super::{
    below, fail, make, post, read_byte, remove, sleep_no_more, with, with_pipe, write_run,
    writer_closed, Pipe, Side, State, ATTRIBUTES, BAD_OPERATION, NAME_MOST, NO_WRITER_WORD,
    OPEN_STAMP, PIPE_OPEN, READER_WORD, WRITER_WORD,
};
use crate::names::NameIndex;

/// The longest path FileSwitch hands over: a containment bound on text read
/// from guest memory.
const PATH_MOST: usize = NAME_MOST + 2;

/// Where a path lies: `$` itself, or a name below it.
enum Place {
    Root,
    Named(Vec<u8>),
}

/// The path at `at`, which FileSwitch gives as `$` or `$.` and the rest;
/// "bad operation" for any other, or a name longer than a pipe's may be.
fn place(os: &mut Os<'_>, at: u32) -> Result<Place, Fail> {
    let (mut path, ended) = os.read_until(at, PATH_MOST, |b| b == 0)?;
    if ended && path == b"$" {
        return Ok(Place::Root);
    }
    let named = ended && path.starts_with(b"$.") && path.len() > 2;
    if !named {
        return Err(fail(os, BAD_OPERATION));
    }
    path.drain(..2);
    Ok(Place::Named(path))
}

/// The pipe `name` names, if there is one.
fn named(os: &mut Os<'_>, name: &[u8]) -> Result<Option<u32>, Fail> {
    with(os, |state| state.named(name).map(|pipe| pipe.id))
}

/// The pipe the stream `handle` is open on, and its side; "bad operation"
/// for a handle no stream has.
fn stream(os: &mut Os<'_>, handle: u32) -> Result<(u32, Side), Fail> {
    let (id, side) = Side::of(handle);
    let open = with(os, |state| {
        state.place(id).is_some_and(|at| {
            let pipe = &state.pipes[at];
            match side {
                Side::Reader => pipe.reader,
                Side::Writer => pipe.writer,
            }
        })
    })?;
    if open {
        Ok((id, side))
    } else {
        Err(fail(os, BAD_OPERATION))
    }
}

/// `FSEntry_Open`: a stream on the pipe R1 names, made empty where there is
/// none, to read for reason nought and to write for any other; R1 nought
/// where the pipe has a stream that way already.
pub(super) fn open(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let Place::Named(name) = place(os, regs.r[1])? else {
        return Err(fail(os, BAD_OPERATION));
    };
    let side = if regs.r[0] == fs::open::READ {
        Side::Reader
    } else {
        Side::Writer
    };
    let id = match named(os, &name)? {
        Some(id) => id,
        None => make(os, &name)?,
    };
    let taken = with_pipe(os, id, |pipe| {
        let end = match side {
            Side::Reader => &mut pipe.reader,
            Side::Writer => &mut pipe.writer,
        };
        let free = !*end;
        *end = true;
        if free && side == Side::Writer {
            pipe.ended = false;
        }
        free
    })?;
    if !taken {
        regs.r[1] = 0;
        return Ok(());
    }
    post(os, id)?;
    let (info, extent, allocated) = with_pipe(os, id, |pipe| match side {
        Side::Reader => (
            file_info::READ | file_info::INTERACTIVE,
            if pipe.writer { 0 } else { pipe.ring.unread },
            pipe.ring.size,
        ),
        Side::Writer => (
            file_info::WRITE | file_info::INTERACTIVE,
            pipe.written,
            pipe.ring.size,
        ),
    })?;
    regs.r[..5].copy_from_slice(&[info, side.handle(id), 0, extent, allocated]);
    Ok(())
}

/// `FSEntry_GetBytes`: the next byte of R1's pipe in R0, C set at its end.
pub(super) fn get_bytes(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (id, side) = stream(os, regs.r[1])?;
    if side != Side::Reader {
        return Err(fail(os, BAD_OPERATION));
    }
    let byte = read_byte(os, id)?;
    if let Some(byte) = byte {
        regs.r[0] = u32::from(byte);
    }
    regs.set_flag(psr::C, byte.is_none());
    Ok(())
}

/// `FSEntry_PutBytes`: R0's byte to R1's pipe.
pub(super) fn put_bytes(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (id, side) = stream(os, regs.r[1])?;
    if side != Side::Writer {
        return Err(fail(os, BAD_OPERATION));
    }
    write_run(os, id, regs.byte(0), 1)
}

/// `FSEntry_Args`: a reader's pointer the bytes it has taken and its extent
/// those left once there is no writer, waited for until then; a writer's
/// pointer and extent the bytes written, an extent set further written as
/// noughts; the pipe's size; a pointer and anything smaller set taken and
/// ignored.
pub(super) fn args(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (id, side) = stream(os, regs.r[1])?;
    let reading = side == Side::Reader;
    match regs.r[0] {
        fs::args::READ_POINTER => {
            regs.r[2] = with_pipe(os, id, |p| if reading { p.consumed } else { p.written })?;
        }
        fs::args::READ_EXTENT if reading => {
            let closed = writer_closed(os, id)?;
            regs.r[2] = if closed {
                with_pipe(os, id, |p| p.ring.unread)?
            } else {
                0
            };
        }
        fs::args::READ_EXTENT => regs.r[2] = with_pipe(os, id, |p| p.written)?,
        fs::args::WRITE_EXTENT if !reading => {
            let written = with_pipe(os, id, |p| p.written)?;
            write_run(os, id, 0, regs.r[2].saturating_sub(written))?;
        }
        fs::args::READ_ALLOCATED => regs.r[2] = with_pipe(os, id, |p| p.ring.size)?,
        fs::args::AT_END => {
            let ended = with_pipe(os, id, |p| !reading || (p.ring.unread == 0 && p.ended))?;
            regs.r[2] = if ended { u32::MAX } else { 0 };
        }
        fs::args::ENSURE => {
            writer_closed(os, id)?;
            regs.r[2] = 0;
        }
        fs::args::READ_STAMP => {
            let (load, exec) = with_pipe(os, id, |p| (p.load, p.exec))?;
            regs.r[2] = load;
            regs.r[3] = exec;
        }
        fs::args::WRITE_POINTER | fs::args::WRITE_EXTENT | fs::args::FLUSH => {}
        _ => return Err(fail(os, BAD_OPERATION)),
    }
    Ok(())
}

/// `FSEntry_Close`: R1's stream closed, its pipe stamped R2 and R3 whatever
/// they are, as RISC OS 5's is. A reader leaving a pipe empty with no writer
/// takes the pipe with it, unless a task switcher objects to the `UpCall` 7
/// telling it so, as it still polls the pipe's words. The stream closes
/// whatever a task switcher answers: FileSwitch keeps a handle whose close
/// failed, and this one would name a stream that is gone.
pub(super) fn close(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (id, side) = stream(os, regs.r[1])?;
    let (load, exec) = (regs.r[2], regs.r[3]);
    let words = with_pipe(os, id, |p| p.words)?;
    match side {
        Side::Reader => {
            sleep_no_more(os, words + READER_WORD)?;
            sleep_no_more(os, words + NO_WRITER_WORD)?;
            let drained = with_pipe(os, id, |p| {
                p.reader = false;
                p.load = load;
                p.exec = exec;
                p.ring.unread == 0 && !p.writer
            })?;
            if drained && sleep_no_more(os, words + NO_WRITER_WORD)?.is_none() {
                return remove(os, id);
            }
            post(os, id)
        }
        Side::Writer => {
            with_pipe(os, id, |p| {
                p.writer = false;
                p.ended = true;
                p.load = load;
                p.exec = exec;
            })?;
            post(os, id)?;
            sleep_no_more(os, words + WRITER_WORD)?;
            Ok(())
        }
    }
}

/// A pipe's catalogue information, its length `length`: an open one's
/// load and execution addresses not yet its own.
fn catalogued(pipe: &Pipe, length: u32) -> Info {
    let (load, exec) = if pipe.reader || pipe.writer {
        (OPEN_STAMP, OPEN_STAMP)
    } else {
        (pipe.load, pipe.exec)
    };
    Info {
        kind: kind::FILE,
        load,
        exec,
        length,
        attributes: pipe.attributes,
    }
}

/// What a directory is catalogued with.
fn directory() -> Info {
    Info {
        kind: kind::DIRECTORY,
        attributes: ATTRIBUTES,
        ..Info::default()
    }
}

/// `FSEntry_File`: a pipe's information read, its length all it has been
/// given, and with `File` 5 once no writer has it; written; a closed pipe
/// deleted; one made, or made empty, its old bytes gone; a directory made
/// as nothing at all; anything else refused.
pub(super) fn file(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    match regs.r[0] {
        fs::file::READ_INFO => read_info(os, regs, true),
        fs::file::READ_INFO_NO_LENGTH => read_info(os, regs, false),
        fs::file::WRITE_INFO..=fs::file::WRITE_ATTRIBUTES => write_info(os, regs),
        fs::file::DELETE => delete(os, regs.r[1]),
        fs::file::CREATE => create(os, regs),
        fs::file::CREATE_DIRECTORY => Ok(()),
        _ => Err(fail(os, BAD_OPERATION)),
    }
}

/// `FSEntry_File` 5, which waits for a pipe's writer to close, or 9, which
/// does not.
fn read_info(os: &mut Os<'_>, regs: &mut Regs, waits: bool) -> Result<(), Fail> {
    let info = match place(os, regs.r[1])? {
        Place::Root => directory(),
        Place::Named(name) => match named(os, &name)? {
            Some(id) => {
                if waits && !writer_closed(os, id)? {
                    return Err(fail(os, PIPE_OPEN));
                }
                with_pipe(os, id, |p| catalogued(p, p.written))?
            }
            None if with(os, |state| state.holds(&name))? => directory(),
            None => Info::default(),
        },
    };
    info.answer(regs);
    Ok(())
}

/// `FSEntry_File` 1 to 4: R2, R3 and R5, or the one part the reason names,
/// written to a pipe; nothing done where there is none.
fn write_info(os: &mut Os<'_>, regs: &Regs) -> Result<(), Fail> {
    let Place::Named(name) = place(os, regs.r[1])? else {
        return Ok(());
    };
    let Some(id) = named(os, &name)? else {
        return Ok(());
    };
    let [reason, _, load, exec, _, attributes, ..] = regs.r;
    with_pipe(os, id, |p| match reason {
        fs::file::WRITE_INFO => {
            p.load = load;
            p.exec = exec;
            p.attributes = attributes;
        }
        fs::file::WRITE_LOAD => p.load = load,
        fs::file::WRITE_EXEC => p.exec = exec,
        _ => p.attributes = attributes,
    })
}

/// `FSEntry_File` 6: a closed pipe deleted, unless a task switcher objects
/// to the `UpCall` 7 that tells it; nothing where there is none.
fn delete(os: &mut Os<'_>, at: u32) -> Result<(), Fail> {
    let Place::Named(name) = place(os, at)? else {
        return Err(fail(os, BAD_OPERATION));
    };
    let Some(id) = named(os, &name)? else {
        return if with(os, |state| state.holds(&name))? {
            Err(fail(os, BAD_OPERATION))
        } else {
            Ok(())
        };
    };
    let (open, words) = with_pipe(os, id, |p| (p.reader || p.writer, p.words))?;
    if open {
        return Err(fail(os, PIPE_OPEN));
    }
    if let Some(objection) = sleep_no_more(os, words + NO_WRITER_WORD)? {
        return Err(objection);
    }
    remove(os, id)
}

/// `FSEntry_File` 7: the pipe R1 names made, or made empty where no writer
/// has it, stamped R2 and R3, after the `UpCall` 6 RISC OS 5 makes on its
/// third poll word.
fn create(os: &mut Os<'_>, regs: &Regs) -> Result<(), Fail> {
    let Place::Named(name) = place(os, regs.r[1])? else {
        return Err(fail(os, BAD_OPERATION));
    };
    let id = match named(os, &name)? {
        Some(id) => {
            if with_pipe(os, id, |p| p.writer)? {
                return Err(fail(os, PIPE_OPEN));
            }
            id
        }
        None => make(os, &name)?,
    };
    let words = with_pipe(os, id, |p| p.words)?;
    os.sleep_on(words + NO_WRITER_WORD)?;
    let (load, exec) = (regs.r[2], regs.r[3]);
    with_pipe(os, id, |p| {
        p.ring.start = 0;
        p.ring.unread = 0;
        p.consumed = 0;
        p.written = 0;
        p.ended = false;
        p.load = load;
        p.exec = exec;
    })?;
    post(os, id)
}

/// `FSEntry_Func`: a directory set, an option and a shutdown taken, the
/// disc read as having no name, a directory listed; a rename and anything
/// else refused.
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

/// `FSEntry_Func` 14, 15 and 19: from R4, up to R3 of the entries of the
/// directory R1 names into R2's R5 bytes as `layout` lays them out — each
/// pipe in it, its length what is left to read, and each directory a dotted
/// name below it gives, once — in the order they were made; R3 how many, R4
/// the next one's place, or -1 for none read.
fn list(os: &mut Os<'_>, regs: &mut Regs, layout: Layout) -> Result<(), Fail> {
    let directory_name = match place(os, regs.r[1])? {
        Place::Root => Vec::new(),
        Place::Named(name) => name,
    };
    let (wanted, offset) = (regs.r[3], regs.r[4]);
    let Some(entries) = with(os, |state| contents(state, &directory_name, offset, wanted))? else {
        return Err(os.no_memory());
    };
    let listed = entries.iter().map(|(name, info)| Listed {
        name,
        info: *info,
        internal: 0,
    });
    os.answer_entries(regs, layout, listed)
}

/// What the directory `name` holds of the pipes, from its `offset`th entry
/// and at most `wanted` of them: each name and its information, a directory
/// in the place of the first pipe below it; none where there is no room to
/// say.
fn contents(state: &State, name: &[u8], offset: u32, wanted: u32) -> Option<Vec<(Vec<u8>, Info)>> {
    let mut directories = NameIndex::<NAME_MOST>::new(state.key);
    let mut entries = Vec::new();
    let mut place = 0;
    for pipe in &state.pipes {
        if entries.len() >= wanted as usize {
            break;
        }
        let Some(rest) = below(&pipe.name, name) else {
            continue;
        };
        let (part, info) = match rest.iter().position(|&b| b == b'.') {
            Some(dot) => {
                let part = &rest[..dot];
                if directories.get(part).is_some() {
                    continue;
                }
                if !directories.insert(part, place) {
                    return None;
                }
                (part, directory())
            }
            None => (rest, catalogued(pipe, pipe.ring.unread)),
        };
        if place >= offset && !fallible::push(&mut entries, (fallible::copied(part)?, info)) {
            return None;
        }
        place += 1;
    }
    Some(entries)
}
