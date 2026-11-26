//! PipeFS 0.25 (`plans/RISCOS-EMULATOR.md` §10): `pipe:`, named pipes a
//! reader and a writer open from either end, which wait through `UpCall` 6
//! as the PRM's PipeFS chapter and RISC OS 5's module, as recorded, describe
//! them, and `*PipeCopy`.
//!
//! - `entries`: the filing system's entries, which FileSwitch calls.
//!
//! A pipe keeps what it is given until its reader takes it. Without a task
//! switcher to take the `UpCall` a wait makes, a read with nothing to take
//! fails, and a write with no room makes more. A pipe's bytes and its three
//! poll words are blocks of the RMA, so a guest that fills one pays for it
//! in its own memory.

mod entries;

use alloc::vec::Vec;

use tairix_hash::HashSeed;
use tairix_riscos_kernel::error::Error;
use tairix_riscos_kernel::filing::fs::{attribute, info};
use tairix_riscos_kernel::module::{Builtin, Command, Resource, Services};
use tairix_riscos_kernel::msgtrans::{Own, OwnFile};
use tairix_riscos_kernel::os::{len32, quietly, Fail, Os, Regs, Unwind, World};
use tairix_riscos_kernel::service;
use tairix_riscos_kernel::swi::os::{find, upcall, BGET, BPUT, FIND, UPCALL};
use tairix_riscos_kernel::swi::X;
use tairix_util::fallible;

use crate::declared::Declared;
use crate::names::NameIndex;

/// The filing system's number and name.
const NUMBER: u32 = 0x2F;
const NAME: &[u8] = b"Pipe";

/// Its information word: called to open whatever is not there, answering
/// `FSEntry_File` 9, and loaded and saved through its open files, as RISC OS
/// 5's PipeFS declares itself.
const INFO: u32 =
    NUMBER | info::ALWAYS_OPEN | info::FILE_9 | info::LOAD_BY_OPEN | info::SAVE_BY_OPEN;

/// Its messages, in the emulator's own words.
const MESSAGES: &[u8] = include_bytes!("../resources/PipeFS/Messages");

/// Its filing system, whose entries come first among its routines.
static DECLARED: Declared = Declared {
    module: &PIPE_FS,
    messages: OwnFile::named(b"Resources:$.Resources.PipeFS.Messages\0"),
    name: NAME,
    first_entry: 0,
    gbpb: false,
    info: INFO,
    extra: 0,
};

/// "A pipe can only be waited on in a task window", "The pipe is open" and
/// "Pipes cannot do that", numbered as RISC OS 5's are.
const NO_TASK_WINDOW: Error = Error {
    number: 0x1_2F00,
    token: "NoTaskWindow",
};
const PIPE_OPEN: Error = Error {
    number: 0x1_2FC2,
    token: "PipeOpen",
};
const BAD_OPERATION: Error = Error {
    number: 0x1_2FF8,
    token: "BadOperation",
};

/// The bytes a pipe holds at first, and gains each time it is full and no
/// task switcher waits for its reader to make room, as RISC OS 5's grows.
const GROWTH: u32 = 256;

/// A pipe's poll words, in its block of them: its reader's, nought while it
/// would wait for a byte; its writer's, nought while it would wait for room;
/// and one nought while a writer has it open.
const READER_WORD: u32 = 0;
const WRITER_WORD: u32 = 4;
const NO_WRITER_WORD: u32 = 8;
const WORDS_BYTES: u32 = 12;

/// The load and execution addresses an open pipe is catalogued with.
const OPEN_STAMP: u32 = 0xDEAD_DEAD;

/// A new pipe's attributes, and a directory's.
const ATTRIBUTES: u32 = attribute::OWNER_READ | attribute::OWNER_WRITE;

/// The longest name a pipe is given: a containment bound on text read from
/// guest memory.
const NAME_MOST: usize = 255;

/// `*PipeCopy`'s help and syntax.
const PIPE_COPY_HELP: &str =
    "*PipeCopy copies a file, a byte at a time, to one other or two others.";
const PIPE_COPY_SYNTAX: &str = "Syntax: *PipeCopy <source> <destination> [<destination>]";

/// PipeFS.
pub static PIPE_FS: Builtin = Builtin {
    title: "PipeFS",
    help_name: None,
    help_suffix: None,
    bridge: false,
    swis: None,
    services: Some(Services {
        numbers: &[service::FS_REDECLARE],
        handler: service_call,
    }),
    commands: &[Command::new(
        "PipeCopy",
        (2, 3),
        PIPE_COPY_HELP,
        Some(PIPE_COPY_SYNTAX),
        pipe_copy,
    )],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[
        entries::open,
        entries::get_bytes,
        entries::put_bytes,
        entries::args,
        entries::close,
        entries::file,
        entries::func,
    ],
    vectors: &[],
    resources: &[Resource {
        name: "Resources.PipeFS.Messages",
        file_type: tairix_riscos_kernel::filing::filetype::TEXT,
        data: MESSAGES,
    }],
    code: None,
};

/// A pipe's bytes: a ring of `room` bytes in the RMA at `data`, `unread` of
/// them from `start`, of which no more than one less than `size` — what the
/// pipe reports it has, a multiple of [`GROWTH`] — may be held.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Ring {
    data: u32,
    room: u32,
    size: u32,
    start: u32,
    unread: u32,
}

impl Ring {
    /// The bytes it takes before it is full, which is a byte short of its
    /// size.
    const fn free(&self) -> u32 {
        self.size.saturating_sub(self.unread + 1)
    }
}

/// A pipe.
#[derive(Clone, PartialEq, Eq, Debug)]
struct Pipe {
    /// Its own number, which its streams' handles are made from.
    id: u32,
    /// Its name below `$`, as it was first given.
    name: Vec<u8>,
    /// Its poll words' block in the RMA.
    words: u32,
    ring: Ring,
    /// The bytes read and written since it was made: its reader's pointer,
    /// and its writer's pointer and extent.
    consumed: u32,
    written: u32,
    reader: bool,
    writer: bool,
    /// A writer has closed since it was made, and none has opened since:
    /// what is unread is all it will have.
    ended: bool,
    load: u32,
    exec: u32,
    attributes: u32,
}

impl Pipe {
    /// What its poll words say: whether its reader, its writer and one
    /// waiting for its writer to close may go on — a writer that has closed
    /// waiting for nothing.
    fn words(&self) -> [u32; 3] {
        [
            u32::from(self.ring.unread > 0 || self.ended),
            u32::from(!self.writer || self.ring.free() > 0),
            u32::from(!self.writer),
        ]
    }
}

/// A stream's end of its pipe.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Reader,
    Writer,
}

impl Side {
    /// The handle FileSwitch knows the stream on this side of the pipe `id`
    /// by.
    const fn handle(self, id: u32) -> u32 {
        let side = match self {
            Self::Reader => 0,
            Self::Writer => 1,
        };
        (id << 1) | side
    }

    /// The pipe and side a handle names.
    const fn of(handle: u32) -> (u32, Self) {
        let side = if handle & 1 == 0 {
            Self::Reader
        } else {
            Self::Writer
        };
        (handle >> 1, side)
    }
}

/// The most pipes' numbers run to: a handle keeps one more bit.
const ID_MOST: u32 = u32::MAX >> 1;

/// What PipeFS holds while it is active.
pub struct State {
    own: Own,
    workspace: u32,
    /// Its pipes, in the order they were made, which their numbers follow.
    pipes: Vec<Pipe>,
    /// Each pipe's number, by its name.
    named: NameIndex<NAME_MOST>,
    next_id: u32,
    /// The key the names of its directories are hashed under as a listing
    /// counts them.
    key: HashSeed,
}

impl State {
    fn place(&self, id: u32) -> Option<usize> {
        self.pipes.binary_search_by_key(&id, |pipe| pipe.id).ok()
    }

    fn pipe_mut(&mut self, id: u32) -> Option<&mut Pipe> {
        self.place(id).map(|at| &mut self.pipes[at])
    }

    fn named(&self, name: &[u8]) -> Option<&Pipe> {
        Some(&self.pipes[self.place(self.named.get(name)?)?])
    }

    /// Whether some pipe's name lies below `name`, which makes `name` a
    /// directory.
    fn holds(&self, name: &[u8]) -> bool {
        self.pipes
            .iter()
            .any(|pipe| below(&pipe.name, name).is_some())
    }
}

/// The rest of `name` below the directory `directory`, none if it is not
/// there; `directory` empty for `$`.
fn below<'a>(name: &'a [u8], directory: &[u8]) -> Option<&'a [u8]> {
    if directory.is_empty() {
        return Some(name);
    }
    let (head, rest) = name.split_at_checked(directory.len())?;
    let rest = rest.strip_prefix(b".")?;
    head.eq_ignore_ascii_case(directory).then_some(rest)
}

fn own(world: &mut World) -> Option<&mut Own> {
    world.extension::<State>().map(|state| &mut state.own)
}

/// PipeFS's error `error`.
fn fail(os: &mut Os<'_>, error: Error) -> Fail {
    os.own_error(own, error, &[])
}

/// `work` on PipeFS's state; "not present" while it has none.
fn with<T>(os: &mut Os<'_>, work: impl FnOnce(&mut State) -> T) -> Result<T, Fail> {
    match os.extension::<State>() {
        Some(state) => Ok(work(state)),
        None => Err(os.error(tairix_riscos_kernel::filing::NOT_PRESENT)),
    }
}

/// `work` on the pipe `id`; "bad operation" where there is none.
fn with_pipe<T>(os: &mut Os<'_>, id: u32, work: impl FnOnce(&mut Pipe) -> T) -> Result<T, Fail> {
    match with(os, |state| state.pipe_mut(id).map(work))? {
        Some(done) => Ok(done),
        None => Err(fail(os, BAD_OPERATION)),
    }
}

/// The pipe `id`'s poll words written as its state says.
fn post(os: &mut Os<'_>, id: u32) -> Result<(), Fail> {
    let (words, values) = with_pipe(os, id, |pipe| (pipe.words, pipe.words()))?;
    os.write_words(words, &values)
}

/// A pipe made called `name`, empty and closed: its number.
fn make(os: &mut Os<'_>, name: &[u8]) -> Result<u32, Fail> {
    let id = with(os, |state| {
        let id = state.next_id;
        (id <= ID_MOST).then(|| {
            state.next_id = id + 1;
            id
        })
    })?;
    let Some(id) = id else {
        return Err(os.no_memory());
    };
    let name = fallible::copied(name).ok_or_else(|| os.no_memory())?;
    let words = os.rma_claim(WORDS_BYTES)?;
    let data = match os.rma_claim(GROWTH) {
        Ok(data) => data,
        Err(fail) => {
            os.free_rma_quietly(words);
            return Err(fail);
        }
    };
    let pipe = Pipe {
        id,
        name,
        words,
        ring: Ring {
            data,
            room: GROWTH,
            size: GROWTH,
            start: 0,
            unread: 0,
        },
        consumed: 0,
        written: 0,
        reader: false,
        writer: false,
        ended: false,
        load: 0,
        exec: 0,
        attributes: ATTRIBUTES,
    };
    let kept = with(os, |state| {
        if state.pipes.try_reserve(1).is_err() || !state.named.insert(&pipe.name, id) {
            return false;
        }
        state.pipes.push(pipe);
        true
    })?;
    if !kept {
        os.free_rma_quietly(data);
        os.free_rma_quietly(words);
        return Err(os.no_memory());
    }
    post(os, id)?;
    Ok(id)
}

/// The pipe `id` gone, and the blocks it held.
fn remove(os: &mut Os<'_>, id: u32) -> Result<(), Fail> {
    let gone = with(os, |state| {
        let pipe = state.pipes.remove(state.place(id)?);
        state.named.remove(&pipe.name);
        Some(pipe)
    })?;
    if let Some(pipe) = gone {
        os.free_rma_quietly(pipe.ring.data);
        os.free_rma_quietly(pipe.words);
    }
    Ok(())
}

/// The next byte the pipe `id` has for its reader; none at its end, and
/// "not in a task window" where it would wait and no task switcher takes
/// the wait.
fn read_byte(os: &mut Os<'_>, id: u32) -> Result<Option<u8>, Fail> {
    loop {
        let (ring, ended, words) = with_pipe(os, id, |pipe| (pipe.ring, pipe.ended, pipe.words))?;
        if ring.unread > 0 {
            let byte = os.read_byte(ring.data + ring.start)?;
            with_pipe(os, id, |pipe| {
                pipe.ring.start = (pipe.ring.start + 1) % pipe.ring.room;
                pipe.ring.unread -= 1;
                pipe.consumed = pipe.consumed.wrapping_add(1);
            })?;
            post(os, id)?;
            return Ok(Some(byte));
        }
        if ended {
            return Ok(None);
        }
        post(os, id)?;
        if !os.sleep_on(words + READER_WORD)? {
            return Err(fail(os, NO_TASK_WINDOW));
        }
    }
}

/// `count` of `byte` written to the pipe `id`, as many at a time as there is
/// room for: with none, a task switcher waits for its reader to make some,
/// or the pipe grows.
fn write_run(os: &mut Os<'_>, id: u32, byte: u8, count: u32) -> Result<(), Fail> {
    let mut left = count;
    while left > 0 {
        let (ring, words) = with_pipe(os, id, |pipe| (pipe.ring, pipe.words))?;
        let run = ring.free().min(left);
        if run == 0 {
            post(os, id)?;
            if !os.sleep_on(words + WRITER_WORD)? {
                grow(os, id)?;
            }
            continue;
        }
        let at = (ring.start + ring.unread) % ring.room;
        let first = (ring.room - at).min(run);
        os.fill(ring.data + at, first, byte)?;
        os.fill(ring.data, run - first, byte)?;
        with_pipe(os, id, |pipe| {
            pipe.ring.unread += run;
            pipe.written = pipe.written.wrapping_add(run);
        })?;
        post(os, id)?;
        left -= run;
    }
    Ok(())
}

/// The pipe `id` made [`GROWTH`] bytes larger: its ring moved to a block of
/// twice the room where it has no more, so that growing as it fills costs
/// no more than the bytes it holds.
fn grow(os: &mut Os<'_>, id: u32) -> Result<(), Fail> {
    let ring = with_pipe(os, id, |pipe| pipe.ring)?;
    let Some(size) = ring.size.checked_add(GROWTH) else {
        return Err(os.no_memory());
    };
    if size <= ring.room {
        return with_pipe(os, id, |pipe| pipe.ring.size = size);
    }
    let room = ring.room.saturating_mul(2).max(size);
    let data = os.rma_claim(room)?;
    let first = (ring.room - ring.start).min(ring.unread);
    let moved = os
        .copy_bytes(ring.data + ring.start, data, first)
        .and_then(|()| os.copy_bytes(ring.data, data + first, ring.unread - first));
    if let Err(fail) = moved {
        os.free_rma_quietly(data);
        return Err(fail);
    }
    os.free_rma_quietly(ring.data);
    with_pipe(os, id, |pipe| {
        pipe.ring = Ring {
            data,
            room,
            size,
            start: 0,
            unread: ring.unread,
        };
    })
}

/// Waits while the pipe `id` has a writer and a task switcher takes the
/// wait: whether its writer has closed.
fn writer_closed(os: &mut Os<'_>, id: u32) -> Result<bool, Fail> {
    loop {
        let (writer, words) = with_pipe(os, id, |pipe| (pipe.writer, pipe.words))?;
        if !writer {
            return Ok(true);
        }
        if !os.sleep_on(words + NO_WRITER_WORD)? {
            return Ok(false);
        }
    }
}

/// `UpCall` 7 on the poll word at `word`: the error a task switcher objects
/// with, if it does.
fn sleep_no_more(os: &mut Os<'_>, word: u32) -> Result<Option<Fail>, Fail> {
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[upcall::SLEEP_NO_MORE, word]);
    match os.perform(X | UPCALL, &mut regs) {
        Ok(()) => Ok(None),
        Err(Fail::Error(error)) => Ok(Some(Fail::Error(error))),
        Err(unwind) => Err(unwind),
    }
}

/// Its initialisation: its workspace, and the filing system declared.
fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (workspace, messages) = DECLARED.made(os, regs.r[12])?;
    os.hold_extension(State {
        own: messages,
        workspace,
        pipes: Vec::new(),
        named: NameIndex::new(os.cache_key()),
        next_id: 1,
        key: os.cache_key(),
    })?;
    DECLARED.declare(os, workspace)
}

/// Its finalisation: the filing system removed, which closes its streams,
/// then every pipe freed; the kernel frees its workspace from its private
/// word.
fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let workspace = with(os, |state| state.workspace)?;
    DECLARED.remove(os, workspace)?;
    let pipes = with(os, |state| core::mem::take(&mut state.pipes))?;
    for pipe in pipes {
        os.free_rma_quietly(pipe.ring.data);
        os.free_rma_quietly(pipe.words);
    }
    os.close_own(own)?;
    os.release_extension::<State>();
    Ok(())
}

/// Its service handler: `Service_FSRedeclare`, the filing system declared
/// again.
fn service_call(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Unwind> {
    if regs.r[1] != service::FS_REDECLARE {
        return Ok(());
    }
    quietly(with(os, |state| state.workspace).and_then(|at| DECLARED.declare(os, at)))
}

/// `*PipeCopy`: the file the tail's first name gives read a byte at a time
/// and each byte written to the one or two after it, Escape stopping it;
/// every file it opened closed whatever it came to.
fn pipe_copy(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let mut names: Vec<Vec<u8>> = Vec::new();
    let mut at = regs.r[0];
    while names.len() < 3 {
        let (_, start) = os.skip_spaces(at)?;
        let (word, ended) = os.read_until(start, NAME_MOST, |b| b <= b' ')?;
        if !ended {
            return Err(os.syntax_failure(PIPE_COPY_SYNTAX));
        }
        if word.is_empty() {
            break;
        }
        at = start.wrapping_add(len32(word.len()));
        if !fallible::push(&mut names, word) {
            return Err(os.no_memory());
        }
    }
    if names.len() < 2 {
        return Err(os.syntax_failure(PIPE_COPY_SYNTAX));
    }
    let room = names.iter().map(|name| len32(name.len() + 1)).sum();
    os.below_supervisor_stack(room, |os, base| {
        let mut addresses = [0; 3];
        let mut place = base;
        for (name, address) in names.iter().zip(&mut addresses) {
            os.write_terminated(place, name, 0, name.len() + 1)?;
            *address = place;
            place = place.wrapping_add(len32(name.len() + 1));
        }
        let [source, outputs @ ..] = addresses;
        let wanted = names.len().saturating_sub(1);
        copy_between(os, source, &outputs[..wanted])
    })
}

/// How `*PipeCopy` opens its files: each one there to be opened, a
/// directory refused, and no path searched.
const COPY_OPENS: u32 = find::MUST_OPEN | find::NO_PATH;

/// The file at `source` copied to each file at `outputs`: each opened, every
/// byte read and written, and each closed again.
fn copy_between(os: &mut Os<'_>, source: u32, outputs: &[u32]) -> Result<(), Fail> {
    let from = open_file(os, find::INPUT | COPY_OPENS, source)?;
    let mut to = [None; 2];
    let mut opened = Ok(());
    for (name, handle) in outputs.iter().zip(&mut to) {
        match open_file(os, find::OUTPUT | COPY_OPENS, *name) {
            Ok(file) => *handle = Some(file),
            Err(fail) => {
                opened = Err(fail);
                break;
            }
        }
    }
    let copied = opened.and_then(|()| loop {
        os.escape_point()?;
        let mut get = Regs::default();
        get.r[1] = from;
        os.perform(X | BGET, &mut get)?;
        if get.flag(tairix_riscos_arm::psr::C) {
            return Ok(());
        }
        for file in to.into_iter().flatten() {
            let mut put = Regs::default();
            put.r[..2].copy_from_slice(&[get.r[0], file]);
            os.perform(X | BPUT, &mut put)?;
        }
    });
    let mut closed = close_file(os, from);
    for file in to.into_iter().flatten() {
        closed = closed.and(close_file(os, file));
    }
    copied.and(closed)
}

fn open_file(os: &mut Os<'_>, mode: u32, name: u32) -> Result<u32, Fail> {
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[mode, name]);
    os.perform(X | FIND, &mut regs)?;
    Ok(regs.r[0])
}

fn close_file(os: &mut Os<'_>, file: u32) -> Result<(), Fail> {
    let mut regs = Regs::default();
    regs.r[..2].copy_from_slice(&[find::CLOSE, file]);
    os.perform(X | FIND, &mut regs)
}
