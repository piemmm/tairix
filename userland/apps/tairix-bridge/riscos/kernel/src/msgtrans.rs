//! MessageTrans (`plans/RISCOS-EMULATOR.md` §4.6): message files opened
//! through descriptors in the caller's memory, their tokens' values looked
//! up, parameters substituted and dictionary words expanded, error blocks
//! made in a ring of buffers that keep still, and menus built from tokens.
//!
//! A file in ResourceFS is used in place; another is loaded into the RMA or
//! the caller's buffer. A file named `Resources:` is found in ResourceFS's
//! store directly, as FileSwitch would find it; any other name is opened
//! through FileSwitch's SWIs. A file held for the caller in place is
//! re-found when ResourceFS's blocks change, its holders told by
//! `Service_MessageFileClosed`.
//!
//! The descriptors and the data are guest memory, read bounded on every use;
//! a file larger than `FILE_MOST` is refused.

mod menus;
pub(crate) mod scan;

use alloc::vec::Vec;

use crate::error::Error;
use crate::filing::{IS_A_DIRECTORY, NOT_FOUND as FILE_NOT_FOUND};
use crate::module::{Builtin, Resource, Services, Swis};
use crate::os::{len32, Fail, Os, Regs, Unwind, World};
use crate::resourcefs::{Object, FS_NUMBER};
use crate::service;
use crate::swi::{os, X};

/// The SWI chunk and each SWI.
pub(crate) use crate::swi::chunk::MESSAGE_TRANS as CHUNK;
pub(crate) const FILE_INFO: u32 = CHUNK;
pub(crate) const OPEN_FILE: u32 = CHUNK + 1;
pub(crate) const LOOKUP: u32 = CHUNK + 2;
pub(crate) const MAKE_MENUS: u32 = CHUNK + 3;
pub(crate) const CLOSE_FILE: u32 = CHUNK + 4;
pub(crate) const ENUMERATE_TOKENS: u32 = CHUNK + 5;
pub(crate) const ERROR_LOOKUP: u32 = CHUNK + 6;
pub(crate) const GS_LOOKUP: u32 = CHUNK + 7;
pub(crate) const COPY_ERROR: u32 = CHUNK + 8;
pub(crate) const DICTIONARY: u32 = CHUNK + 9;

/// Its routines: the callback that tells clients, once it is up, that every
/// file they had open is closed.
const CLOSED_CALLBACK: usize = 0;

/// A descriptor's magic words.
const FAST: u32 = u32::from_le_bytes(*b"FAST");
const SLOW: u32 = u32::from_le_bytes(*b"SLOW");

/// A descriptor's flags.
mod flag {
    /// The data is used in place in ResourceFS.
    pub(super) const IN_PLACE: u32 = 1;
    /// MessageTrans claimed the data and frees it at the close.
    pub(super) const CLAIMED: u32 = 1 << 1;
    /// Opened again at its next use: its MessageTrans died.
    pub(super) const REOPEN: u32 = 1 << 2;
    /// Opened with `FAST`.
    pub(super) const FAST: u32 = 1 << 26;
    /// Worth indexing.
    pub(super) const INDEX: u32 = 1 << 27;
    /// MessageTrans made this descriptor.
    pub(super) const ALLOCATED: u32 = 1 << 29;
    /// The caller's block fronts the descriptor its +8 points at.
    pub(super) const PROXY: u32 = 1 << 30;
    /// A squashed text file in ResourceFS.
    pub(super) const SQUASHED: u32 = 1 << 31;
    /// The flags `MessageTrans_FileInfo` gives.
    pub(super) const PUBLIC: u32 = (1 << 25) - 1;
}

/// A descriptor's words: its magic or link, its flags, its data block and
/// the file's name; then, in MessageTrans's own, the caller's block.
const MAGIC: u32 = 0;
const FLAGS: u32 = 4;
const DATA: u32 = 8;
const NAME: u32 = 12;
const BACK: u32 = 20;
const COUNTER: u32 = 24;
/// MessageTrans's own descriptor's size.
const DESCRIPTOR: u32 = 32;

/// The largest file a lookup reads: a containment bound on the host's copy
/// of a file the guest describes, not a capacity.
pub(crate) const FILE_MOST: u32 = 16 << 20;
/// A file smaller than this is not worth indexing, nor one of fewer tokens.
const INDEX_LEAST_BYTES: u32 = 80;
const INDEX_LEAST_TOKENS: usize = 8;
/// The deepest the files can be found within one another: an open that
/// needs a lookup, a lookup an open.
const NESTING_MOST: u32 = 2;

/// The error buffers the PRM gives it, ten for the foreground and two for
/// interrupts, each a number and the longest text.
const FOREGROUND: u32 = 10;
const INTERRUPT: u32 = 2;
const BUFFER: u32 = 256;
/// The longest text an error block it makes holds.
const TEXT_MOST: u32 = BUFFER - 4 - 1;

/// Its errors.
const SYNTAX: Error = Error {
    number: 0xAC0,
    token: "BadTokenPattern",
};
const TOKEN_NOT_FOUND: Error = Error {
    number: 0xAC2,
    token: "MessageTokenNotFound",
};
const BAD_DESCRIPTOR: Error = Error {
    number: 0xAC1,
    token: "BadMessageFileDescriptor",
};
/// Its refusal of a file asked for while it finds one: too deep to look its
/// own text up.
const RECURSION: (u32, &[u8]) = (0xAC3, b"MessageTrans re-entered too deeply");
/// "Buffer overflow", the global file's text.
pub(crate) const BUFFER_OVERFLOW: Error = Error {
    number: 0x2C1,
    token: "BufferOverflow",
};

/// Its own messages, and the global file's, which the Messages module keeps.
const MESSAGES: &[u8] = include_bytes!("../resources/MsgTrans/Messages");
pub(crate) const GLOBAL: &[u8] = include_bytes!("../resources/Global/Messages");
const OWN_NAME: &[u8] = b"Resources:$.Resources.MsgTrans.Messages\0";
const GLOBAL_NAME: &[u8] = b"Resources:$.Resources.Global.Messages\0";

/// Its workspace: the descriptors of its own file and the global one, their
/// names, then the error buffers, sixteen-aligned.
const OWN_AT: u32 = 0;
const GLOBAL_AT: u32 = 16;
const OWN_NAME_AT: u32 = 32;
const GLOBAL_NAME_AT: u32 = OWN_NAME_AT + len32(OWN_NAME.len());
const BUFFERS_AT: u32 = (GLOBAL_NAME_AT + len32(GLOBAL_NAME.len())).next_multiple_of(16);
const WORKSPACE: u32 = BUFFERS_AT + (FOREGROUND + INTERRUPT) * BUFFER;

/// MessageTrans.
pub(crate) static MESSAGE_TRANS: Builtin = Builtin {
    title: "MessageTrans",
    help_name: None,
    bridge: false,
    swis: Some(Swis {
        chunk: CHUNK,
        prefix: "MessageTrans",
        names: &[
            "FileInfo",
            "OpenFile",
            "Lookup",
            "MakeMenus",
            "CloseFile",
            "EnumerateTokens",
            "ErrorLookup",
            "GSLookup",
            "CopyError",
            "Dictionary",
        ],
        handler: swi,
    }),
    services: Some(Services {
        numbers: &[
            service::RESOURCE_FS_STARTED,
            service::RESOURCE_FS_DYING,
            service::TERRITORY_STARTED,
        ],
        handler: service,
    }),
    commands: &[],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[announce_closed],
    vectors: &[],
    resources: &[Resource {
        name: "Resources.MsgTrans.Messages",
        file_type: 0xFFF,
        data: MESSAGES,
    }],
    code: None,
};

/// The tokens of a file opened to be indexed, so a token without a `?` is
/// found without a walk: each first in the file, then those with a `?` in
/// the file's order, which the first only follow.
struct TokenIndex {
    names: Vec<u8>,
    exact: Vec<(u32, u32, u32)>,
    wild: Vec<(u32, u32, u32)>,
}

impl TokenIndex {
    fn name(&self, &(start, length, _): &(u32, u32, u32)) -> &[u8] {
        let start = start as usize;
        self.names
            .get(start..start + length as usize)
            .unwrap_or(&[])
    }

    /// Where `token`'s value is, as the index says.
    fn find(&self, token: &[u8]) -> Option<u32> {
        let at = self.exact.partition_point(|entry| self.name(entry) < token);
        if let Some(entry) = self.exact.get(at).filter(|entry| self.name(entry) == token) {
            return Some(entry.2);
        }
        self.wild
            .iter()
            .find(|entry| {
                let pattern = self.name(entry);
                pattern.len() == token.len()
                    && pattern
                        .iter()
                        .zip(token)
                        .all(|(&p, &t)| p == t || p == b'?')
            })
            .map(|entry| entry.2)
    }

    /// The index of `data`, if it has the tokens to be worth one.
    fn of(data: &[u8]) -> Option<Self> {
        let mut index = Self {
            names: Vec::new(),
            exact: Vec::new(),
            wild: Vec::new(),
        };
        let mut at = 0;
        let mut count = 0;
        while let Some((range, next)) = scan::next_token(data, b"*", at) {
            at = next;
            let Some(value) = scan::value_of(data, &range) else {
                continue;
            };
            count += 1;
            let token = &data[range];
            index.names.try_reserve(token.len()).ok()?;
            let entry = (len32(index.names.len()), len32(token.len()), len32(value));
            index.names.extend_from_slice(token);
            let list = if token.contains(&b'?') {
                &mut index.wild
            } else {
                &mut index.exact
            };
            list.try_reserve(1).ok()?;
            list.push(entry);
        }
        if count < INDEX_LEAST_TOKENS {
            return None;
        }
        let names = core::mem::take(&mut index.names);
        index
            .exact
            .sort_by(|a, b| slice(&names, a).cmp(slice(&names, b)));
        index
            .exact
            .dedup_by(|later, earlier| slice(&names, later) == slice(&names, earlier));
        index.names = names;
        Some(index)
    }
}

fn slice<'a>(names: &'a [u8], &(start, length, _): &(u32, u32, u32)) -> &'a [u8] {
    names
        .get(start as usize..(start + length) as usize)
        .unwrap_or(&[])
}

/// MessageTrans's state.
pub(crate) struct State {
    /// The descriptors it made, the newest first.
    open: Vec<u32>,
    /// The indexes of those indexed, by descriptor.
    indexes: Vec<(u32, TokenIndex)>,
    /// Files being found within one another.
    nesting: u32,
    workspace: Option<u32>,
    /// Its own file and the global one are open.
    own_open: bool,
    global_open: bool,
    /// The next foreground and interrupt buffer.
    next_foreground: u32,
    next_interrupt: u32,
}

impl State {
    pub(crate) const fn new() -> Self {
        Self {
            open: Vec::new(),
            indexes: Vec::new(),
            nesting: 0,
            workspace: None,
            own_open: false,
            global_open: false,
            next_foreground: 0,
            next_interrupt: 0,
        }
    }

    fn index(&self, descriptor: u32) -> Option<&TokenIndex> {
        self.indexes
            .iter()
            .find(|(d, _)| *d == descriptor)
            .map(|(_, index)| index)
    }

    fn drop_index(&mut self, descriptor: u32) {
        self.indexes.retain(|(d, _)| *d != descriptor);
    }
}

/// Where a file was found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Located {
    /// In ResourceFS, to be used in place: its block, the length word
    /// before its data.
    InPlace { block: u32 },
    /// Squashed in ResourceFS: its block, and the size its data expands to.
    Squashed { block: u32, size: u32 },
    /// Elsewhere, to be loaded: its length.
    Elsewhere { length: u32 },
}

impl Located {
    /// The flags it gives a descriptor.
    const fn flags(self) -> u32 {
        match self {
            Self::InPlace { .. } => flag::IN_PLACE,
            Self::Squashed { .. } => flag::SQUASHED,
            Self::Elsewhere { .. } => 0,
        }
    }
}

/// A file as a lookup reads it: its descriptor, and where its data is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Opened {
    descriptor: u32,
    data: u32,
    length: u32,
}

/// What a lookup found.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
struct Found {
    /// The value's address and length, to its terminator.
    value: u32,
    length: u32,
    /// Where the caller's string's token, or its default, ends.
    end: u32,
}

/// The four parameters, read from where R4 to R7 point.
type Parameters = [Option<Vec<u8>>; 4];

/// The longest parameter read.
const PARAMETER_MOST: usize = 256;

/// The name `name` gives below `Resources:`, if it names ResourceFS.
fn resources_path(name: &[u8]) -> Option<&[u8]> {
    const PREFIX: &[u8] = b"Resources:";
    (name.len() >= PREFIX.len() && name[..PREFIX.len()].eq_ignore_ascii_case(PREFIX))
        .then(|| &name[PREFIX.len()..])
}

impl Os<'_> {
    /// The next error buffer: an interrupt's while one is being taken.
    fn next_buffer(&mut self) -> Option<u32> {
        let workspace = self.world.message_trans.workspace?;
        let state = &mut self.world.message_trans;
        let slot = if self.world.in_interrupt {
            let n = state.next_interrupt;
            state.next_interrupt = (n + 1) % INTERRUPT;
            FOREGROUND + n
        } else {
            let n = state.next_foreground;
            state.next_foreground = (n + 1) % FOREGROUND;
            n
        };
        Some(workspace + BUFFERS_AT + slot * BUFFER)
    }

    /// An error block of `number` and `text`, cut to fit, in the next
    /// buffer.
    fn made_error(&mut self, number: u32, text: &[u8]) -> Fail {
        let Some(buffer) = self.next_buffer() else {
            return self.raw_error(number, text);
        };
        let text = &text[..text.len().min(TEXT_MOST as usize)];
        let written = self
            .write_word(buffer, number)
            .and_then(|()| self.write_bytes(buffer + 4, text))
            .and_then(|()| self.write_byte(buffer + 4 + len32(text.len()), 0));
        match written {
            Ok(()) => Fail::Error(buffer),
            Err(fail) => fail,
        }
    }

    /// One of MessageTrans's own errors: its text from its own file, else the
    /// global one, else its token.
    fn message_trans_error(&mut self, error: Error, parameter: &[u8]) -> Fail {
        let own = self.world.message_trans.own_open;
        self.made_from(error, parameter, own)
    }

    /// An error whose text only the global file has.
    fn message_trans_global_error(&mut self, error: Error, parameter: &[u8]) -> Fail {
        self.made_from(error, parameter, false)
    }

    fn made_from(&mut self, error: Error, parameter: &[u8], own: bool) -> Fail {
        let token = error.token.as_bytes();
        let mut text = Vec::new();
        if text.try_reserve_exact(parameter.len()).is_err() {
            return self.no_memory();
        }
        text.extend_from_slice(parameter);
        let parameters: Parameters = [Some(text), None, None, None];
        let workspace = self.world.message_trans.workspace;
        let files = [
            (own, OWN_AT),
            (self.world.message_trans.global_open, GLOBAL_AT),
        ];
        let mut text = None;
        if self.world.message_trans.nesting < NESTING_MOST {
            for (open, at) in files {
                let Some(workspace) = workspace.filter(|_| open) else {
                    continue;
                };
                self.world.message_trans.nesting += 1;
                let found = self.expanded(workspace + at, token, &parameters, TEXT_MOST);
                self.world.message_trans.nesting -= 1;
                match found {
                    Ok(Some(found)) => {
                        text = Some(found);
                        break;
                    }
                    Ok(None) | Err(Fail::Error(_)) => {}
                    Err(unwind @ Fail::Unwind(_)) => return unwind,
                }
            }
        }
        let text = text.unwrap_or_else(|| token.to_vec());
        self.made_error(error.number, &text)
    }

    /// Where the file `name` names is, as FileSwitch finds it: the error
    /// FileSwitch gives if it cannot be opened.
    fn locate(&mut self, name: u32) -> Result<Located, Fail> {
        if self.world.message_trans.nesting >= NESTING_MOST {
            return Err(self.made_error(RECURSION.0, RECURSION.1));
        }
        self.world.message_trans.nesting += 1;
        let located = self.locate_once(name);
        self.world.message_trans.nesting -= 1;
        located
    }

    fn locate_once(&mut self, name: u32) -> Result<Located, Fail> {
        let text = self.read_text(name, crate::error::TEXT)?;
        if let Some(found) = self.locate_in_resources(&text)? {
            return found;
        }
        let mut open = Regs::default();
        open.r[0] = os::find::INPUT | os::find::MUST_OPEN | NO_PATH;
        open.r[1] = name;
        self.perform(X | os::FIND, &mut open)?;
        let handle = open.r[0];
        let located = self.handle_located(handle);
        let mut close = Regs::default();
        close.r[1] = handle;
        let closed = self.perform(X | os::FIND, &mut close);
        let located = located?;
        closed?;
        Ok(located)
    }

    /// Where `name` is if it names ResourceFS, itself or through a path
    /// variable whose every element does, each tried in turn as FileSwitch
    /// tries them: the file, or FileSwitch's error; none where FileSwitch
    /// must find it.
    fn locate_in_resources(&mut self, name: &[u8]) -> Result<Option<Result<Located, Fail>>, Fail> {
        let mut candidates = Vec::new();
        if let Some(path) = resources_path(name) {
            candidates.try_reserve(1).map_err(|_| self.no_memory())?;
            candidates.push(path.to_vec());
        } else if let Some(colon) = name.iter().position(|&b| b == b':').filter(|&at| at > 0) {
            let (prefix, rest) = (&name[..colon], &name[colon + 1..]);
            let mut variable = Vec::new();
            variable
                .try_reserve_exact(prefix.len() + 5)
                .map_err(|_| self.no_memory())?;
            variable.extend_from_slice(prefix);
            variable.extend_from_slice(b"$Path");
            let Some(value) = self.variable_text(&variable)? else {
                return Ok(None);
            };
            for element in value.split(|&b| b == b',') {
                let element = element.trim_ascii();
                let mut whole = Vec::new();
                whole
                    .try_reserve_exact(element.len() + rest.len())
                    .map_err(|_| self.no_memory())?;
                whole.extend_from_slice(element);
                whole.extend_from_slice(rest);
                let Some(path) = resources_path(&whole) else {
                    return Ok(None);
                };
                let path = path.to_vec();
                candidates.try_reserve(1).map_err(|_| self.no_memory())?;
                candidates.push(path);
            }
        } else {
            return Ok(None);
        }
        for path in candidates {
            match self.resource(&path)? {
                Some(Object::File(entry)) => {
                    return Ok(Some(self.in_resources(entry.data, entry.length)))
                }
                Some(Object::Directory { .. }) => {
                    return Ok(Some(Err(self.error_with(IS_A_DIRECTORY, &[name]))))
                }
                None => {}
            }
        }
        Ok(Some(Err(self.error_with(FILE_NOT_FOUND, &[name]))))
    }

    /// The open file `handle`'s place: in place if its filing system is
    /// ResourceFS.
    fn handle_located(&mut self, handle: u32) -> Result<Located, Fail> {
        let mut info = Regs::default();
        info.r[0] = READ_FS_HANDLE;
        info.r[1] = handle;
        self.perform(X | os::FS_CONTROL, &mut info)?;
        let mut extent = Regs::default();
        extent.r[0] = os::args::READ_EXTENT;
        extent.r[1] = handle;
        self.perform(X | os::ARGS, &mut extent)?;
        if info.r[2] & 0xFF == FS_NUMBER {
            return self.in_resources(info.r[1], extent.r[2]);
        }
        Ok(Located::Elsewhere {
            length: extent.r[2],
        })
    }

    /// A file in ResourceFS whose data is at `data`: squashed text if its
    /// header says so.
    fn in_resources(&mut self, data: u32, length: u32) -> Result<Located, Fail> {
        let block = data.wrapping_sub(4);
        if length >= SQUASH_HEADER && self.read_word(data)? == SQSH {
            let load = self.read_word(data.wrapping_add(8))?;
            if load & 0xFFFF_FF00 == 0xFFFF_FF00 {
                let size = self.read_word(data.wrapping_add(4))?;
                return Ok(Located::Squashed { block, size });
            }
        }
        Ok(Located::InPlace { block })
    }

    /// The bytes a file `located` needs in a buffer: its length and the
    /// length word.
    fn needed(&mut self, located: Located) -> Result<u32, Fail> {
        let length = match located {
            Located::InPlace { block } => self.read_word(block)?.wrapping_sub(4),
            Located::Squashed { size, .. } => size,
            Located::Elsewhere { length } => length,
        };
        Ok(length.wrapping_add(4))
    }

    /// Loads the file `name`, found `located`, into the block at `to`, which
    /// holds `size` bytes and its length word.
    fn load_into(&mut self, name: u32, located: Located, to: u32, size: u32) -> Result<(), Fail> {
        self.write_word(to, size)?;
        let data = to.wrapping_add(4);
        match located {
            Located::InPlace { block } => {
                let length = size.wrapping_sub(4);
                self.copy_bytes(block.wrapping_add(4), data, length)
            }
            Located::Squashed { block, size: out } => {
                let mut ask = Regs::default();
                ask.r[0] = SQUASH_WORKSPACE;
                self.perform(X | SQUASH_DECOMPRESS, &mut ask)?;
                let workspace = self.rma_claim(ask.r[0])?;
                let inbytes = self.read_word(block)?.wrapping_sub(SQUASH_HEADER + 4);
                let mut go = Regs::default();
                go.r[..6].copy_from_slice(&[
                    0,
                    workspace,
                    block.wrapping_add(4 + SQUASH_HEADER),
                    inbytes,
                    data,
                    out,
                ]);
                let result = self.perform(X | SQUASH_DECOMPRESS, &mut go);
                self.free_rma_quietly(workspace);
                result
            }
            Located::Elsewhere { .. } => {
                let mut load = Regs::default();
                load.r[..4].copy_from_slice(&[os::file::LOAD, name, data, 0]);
                self.perform(X | os::FILE, &mut load)
            }
        }
    }

    /// The descriptor a caller's block stands for: itself, or the one it
    /// fronts.
    fn real_descriptor(&mut self, block: u32) -> Result<u32, Fail> {
        let flags = self.read_word(block.wrapping_add(FLAGS))?;
        if flags & flag::PROXY != 0 {
            self.read_word(block.wrapping_add(DATA))
        } else {
            Ok(block)
        }
    }

    /// The caller's block `block`'s file, ready to read: re-found or
    /// re-opened if it must be.
    fn opened(&mut self, block: u32) -> Result<Opened, Fail> {
        let mut descriptor = self.real_descriptor(block)?;
        let mut data = self.read_word(descriptor.wrapping_add(DATA))?;
        if data == 0 {
            let flags = self.read_word(descriptor.wrapping_add(FLAGS))?;
            if flags & flag::REOPEN != 0 {
                let magic = if flags & flag::FAST != 0 {
                    FAST
                } else if flags & flag::INDEX == 0 {
                    SLOW
                } else {
                    flags
                };
                let name = self.read_word(descriptor.wrapping_add(NAME))?;
                self.open_message_file(block, name, 0, magic)?;
                descriptor = self.real_descriptor(block)?;
                data = self.read_word(descriptor.wrapping_add(DATA))?;
            } else if flags & flag::IN_PLACE != 0 {
                let name = self.read_word(descriptor.wrapping_add(NAME))?;
                let Located::InPlace { block: found } = self.locate(name)? else {
                    return Err(self.message_trans_error(BAD_DESCRIPTOR, b""));
                };
                self.write_word(descriptor.wrapping_add(DATA), found)?;
                data = found;
            } else {
                return Err(self.message_trans_error(BAD_DESCRIPTOR, b""));
            }
        }
        let length = self.read_word(data)?.wrapping_sub(4);
        if length > FILE_MOST {
            return Err(self.message_trans_error(BAD_DESCRIPTOR, b""));
        }
        Ok(Opened {
            descriptor,
            data: data.wrapping_add(4),
            length,
        })
    }

    /// The file's bytes, copied for a walk.
    fn file_bytes(&mut self, file: Opened) -> Result<Vec<u8>, Fail> {
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(file.length as usize)
            .map_err(|_| self.no_memory())?;
        bytes.resize(file.length as usize, 0);
        self.read_bytes(file.data, &mut bytes)?;
        Ok(bytes)
    }

    /// Where `token`'s value is in `file`, and its length; none if it is not
    /// there.
    fn find_value(&mut self, file: Opened, token: &[u8]) -> Result<Option<(u32, u32)>, Fail> {
        let indexed = if token.contains(&b'?') {
            None
        } else {
            self.world
                .message_trans
                .index(file.descriptor)
                .map(|index| index.find(token))
        };
        match indexed {
            Some(None) => Ok(None),
            Some(Some(at)) => {
                let length = self.value_length_at(file, at)?;
                Ok(length.map(|length| (file.data.wrapping_add(at), length)))
            }
            None => {
                let bytes = self.file_bytes(file)?;
                Ok(scan::find(&bytes, token).and_then(|at| {
                    let length = scan::value_length(&bytes, at)?;
                    Some((file.data.wrapping_add(len32(at)), len32(length)))
                }))
            }
        }
    }

    /// The length of the value at `at` in `file`, read a piece at a time
    /// until its terminator; none if the file ends first.
    fn value_length_at(&mut self, file: Opened, at: u32) -> Result<Option<u32>, Fail> {
        const PIECE: u32 = 256;
        let rest = file.length.saturating_sub(at);
        let mut read = 0;
        let mut bytes = Vec::new();
        while read < rest {
            let more = PIECE.min(rest - read);
            bytes
                .try_reserve(more as usize)
                .map_err(|_| self.no_memory())?;
            let start = bytes.len();
            bytes.resize(start + more as usize, 0);
            self.read_bytes(file.data.wrapping_add(at + read), &mut bytes[start..])?;
            read += more;
            if let Some(length) = scan::value_length(&bytes, 0) {
                return Ok(Some(len32(length)));
            }
        }
        Ok(None)
    }

    /// Looks the token at `token` up in the caller's file `descriptor`, or
    /// only the global file if none: its default used where the file has
    /// none, the global file asked where it has neither.
    fn look_up_token(&mut self, descriptor: u32, token: u32) -> Result<Found, Fail> {
        let (text, _) = self.read_until(token, crate::error::TEXT, |b| b < b' ')?;
        let name = &text[..scan::token_end(&text)];
        let default = scan::default_of(&text);
        let mut files = Vec::new();
        files.try_reserve(2).map_err(|_| self.no_memory())?;
        if descriptor != 0 {
            files.push(Some(descriptor));
        }
        files.push(None);
        for file in files {
            let opened = match file {
                Some(block) => self.opened(block),
                None => match self.world.message_trans.workspace {
                    Some(workspace) if self.world.message_trans.global_open => {
                        self.opened(workspace + GLOBAL_AT)
                    }
                    _ => {
                        return Err(self
                            .made_error(TOKEN_NOT_FOUND.number, TOKEN_NOT_FOUND.token.as_bytes()))
                    }
                },
            };
            let found = match opened {
                Ok(opened) => self.find_value(opened, name)?,
                Err(Fail::Error(_)) if file.is_some() => None,
                Err(fail) => return Err(fail),
            };
            if let Some((value, length)) = found {
                return Ok(Found {
                    value,
                    length,
                    end: token.wrapping_add(len32(name.len())),
                });
            }
            if let Some(range) = &default {
                return Ok(Found {
                    value: token.wrapping_add(len32(range.start)),
                    length: len32(range.len()),
                    end: token.wrapping_add(len32(range.end)),
                });
            }
        }
        Err(self.message_trans_error(TOKEN_NOT_FOUND, name))
    }

    /// The four parameters R4 to R7 point at, each to its terminator.
    fn parameters(&mut self, regs: &Regs) -> Result<Parameters, Fail> {
        let mut parameters: Parameters = [None, None, None, None];
        for (slot, &address) in parameters.iter_mut().zip(&regs.r[4..8]) {
            if address != 0 {
                let (text, _) =
                    self.read_until(address, PARAMETER_MOST, |b| matches!(b, 0 | b'\n' | b'\r'))?;
                *slot = Some(text);
            }
        }
        Ok(parameters)
    }

    /// MessageTrans's dictionary, read entry by entry to its end.
    fn dictionary_bytes(&mut self) -> Result<Vec<u8>, Fail> {
        let mut bytes = Vec::new();
        let mut at = self.world.dictionaries.messages;
        loop {
            let length = self.read_byte(at)?;
            if length == 0 || bytes.len() + usize::from(length) > FILE_MOST as usize {
                bytes.try_reserve(1).map_err(|_| self.no_memory())?;
                bytes.push(0);
                return Ok(bytes);
            }
            let start = bytes.len();
            bytes
                .try_reserve(usize::from(length))
                .map_err(|_| self.no_memory())?;
            bytes.resize(start + usize::from(length), 0);
            self.read_bytes(at, &mut bytes[start..])?;
            at = at.wrapping_add(u32::from(length));
        }
    }

    /// The value at `found` expanded with `parameters` into the buffer at
    /// `buffer` of `size` bytes, its terminator after: its length.
    fn expand_into(
        &mut self,
        found: Found,
        parameters: &Parameters,
        buffer: u32,
        size: u32,
    ) -> Result<u32, Fail> {
        let mut value = Vec::new();
        value
            .try_reserve_exact(found.length as usize)
            .map_err(|_| self.no_memory())?;
        value.resize(found.length as usize, 0);
        self.read_bytes(found.value, &mut value)?;
        let dictionary = if value.contains(&crate::dictionary::ESCAPE) {
            self.dictionary_bytes()?
        } else {
            Vec::new()
        };
        let expansion = scan::Expansion {
            parameters: [0, 1, 2, 3].map(|n| parameters[n].as_deref()),
            dictionary: &dictionary,
        };
        // A buffer with no room even for the terminator is left alone.
        let Some(room) = size.checked_sub(1) else {
            return Ok(0);
        };
        let mut out = Vec::new();
        out.try_reserve_exact(room as usize)
            .map_err(|_| self.no_memory())?;
        out.resize(room as usize, 0);
        let length = expansion.expand(&value, &mut out);
        self.write_bytes(buffer, &out[..length])?;
        self.write_byte(buffer.wrapping_add(len32(length)), 0)?;
        Ok(len32(length))
    }

    /// The token `token` looked up in the descriptor at `descriptor` and
    /// expanded, at most `most` bytes: none if it is not there.
    fn expanded(
        &mut self,
        descriptor: u32,
        token: &[u8],
        parameters: &Parameters,
        most: u32,
    ) -> Result<Option<Vec<u8>>, Fail> {
        let file = self.opened(descriptor)?;
        let Some((at, length)) = self.find_value(file, token)? else {
            return Ok(None);
        };
        let mut value = Vec::new();
        value
            .try_reserve_exact(length as usize)
            .map_err(|_| self.no_memory())?;
        value.resize(length as usize, 0);
        self.read_bytes(at, &mut value)?;
        let expansion = scan::Expansion {
            parameters: [0, 1, 2, 3].map(|n| parameters[n].as_deref()),
            dictionary: &[],
        };
        let mut out = Vec::new();
        out.try_reserve_exact(most as usize)
            .map_err(|_| self.no_memory())?;
        out.resize(most as usize, 0);
        let length = expansion.expand(&value, &mut out);
        out.truncate(length);
        Ok(Some(out))
    }

    /// `MessageTrans_OpenFile`: the file `name` names opened through the
    /// caller's block `block`, into `buffer` or held by MessageTrans.
    fn open_message_file(
        &mut self,
        block: u32,
        name: u32,
        buffer: u32,
        magic: u32,
    ) -> Result<(), Fail> {
        if self.read_word(block.wrapping_add(MAGIC))? == FAST {
            let flags = self.read_word(block.wrapping_add(FLAGS))?;
            if flags & flag::PROXY != 0 {
                let held = self.read_word(block.wrapping_add(DATA))?;
                if self.world.message_trans.open.contains(&held) {
                    let back = self.read_word(held.wrapping_add(BACK))?;
                    let held_flags = self.read_word(held.wrapping_add(FLAGS))?;
                    let held_name = self.read_word(held.wrapping_add(NAME))?;
                    if back == block && held_flags & flag::IN_PLACE != 0 && held_name == name {
                        if self.read_word(held.wrapping_add(DATA))? == 0 {
                            if let Located::InPlace { block: found } = self.locate(name)? {
                                self.write_word(held.wrapping_add(DATA), found)?;
                            }
                        }
                        return Ok(());
                    }
                    self.close_message_file(block)?;
                }
            } else {
                self.close_message_file(block)?;
            }
        }
        let (located, size) = if name == 0 {
            (Located::Elsewhere { length: 0 }, self.read_word(buffer)?)
        } else {
            let located = self.locate(name)?;
            (located, self.needed(located)?)
        };
        let mut flags = if name == 0 { 0 } else { located.flags() };
        if magic == FAST {
            flags |= flag::FAST | flag::INDEX;
            self.write_word(block.wrapping_add(BACK), 0)?;
            self.write_word(block.wrapping_add(COUNTER), 0)?;
        } else if magic == SLOW {
            flags &= !flag::INDEX;
        } else if buffer == 0 {
            flags |= flag::INDEX;
        }
        if size < INDEX_LEAST_BYTES {
            flags &= !flag::INDEX;
        }
        self.write_word(block.wrapping_add(FLAGS), flags)?;
        self.write_word(block.wrapping_add(NAME), name)?;
        self.write_word(block.wrapping_add(MAGIC), magic)?;
        let descriptor = if buffer == 0 {
            self.hold_message_file(block, name, located, size, flags)?
        } else {
            self.write_word(block.wrapping_add(FLAGS), flags & !flag::IN_PLACE)?;
            self.write_word(block.wrapping_add(DATA), buffer)?;
            if name != 0 {
                self.load_into(name, located, buffer, size)?;
            }
            block
        };
        if flags & flag::INDEX != 0 {
            let file = self.opened(block)?;
            let bytes = self.file_bytes(file)?;
            if let Some(index) = TokenIndex::of(&bytes) {
                let state = &mut self.world.message_trans;
                state.drop_index(descriptor);
                if state.indexes.try_reserve(1).is_ok() {
                    state.indexes.push((descriptor, index));
                }
            }
        }
        Ok(())
    }

    /// A descriptor MessageTrans makes for the caller's `block`, holding the
    /// file in place or in the RMA: on the open list, newest.
    fn hold_message_file(
        &mut self,
        block: u32,
        name: u32,
        located: Located,
        size: u32,
        flags: u32,
    ) -> Result<u32, Fail> {
        if self.world.message_trans.open.try_reserve(1).is_err() {
            return Err(self.no_memory());
        }
        let descriptor = self.rma_claim(DESCRIPTOR)?;
        let mut held = flags | flag::ALLOCATED;
        let data = if let Located::InPlace { block } = located {
            block
        } else {
            let claimed = match self.rma_claim(size) {
                Ok(claimed) => claimed,
                Err(fail) => {
                    self.free_rma_quietly(descriptor);
                    return Err(fail);
                }
            };
            if let Err(fail) = self.load_into(name, located, claimed, size) {
                self.free_rma_quietly(claimed);
                self.free_rma_quietly(descriptor);
                return Err(fail);
            }
            held |= flag::CLAIMED;
            claimed
        };
        let words = [0, held, data, name, 0, block, 0, 0];
        for (n, word) in (0..).zip(words) {
            self.write_word(descriptor + 4 * n, word)?;
        }
        self.write_word(block.wrapping_add(MAGIC), FAST)?;
        self.write_word(block.wrapping_add(FLAGS), flags | flag::PROXY)?;
        self.write_word(block.wrapping_add(DATA), descriptor)?;
        self.world.message_trans.open.insert(0, descriptor);
        Ok(descriptor)
    }

    /// `MessageTrans_CloseFile`: the caller's `block` closed, what
    /// MessageTrans holds for it freed.
    fn close_message_file(&mut self, block: u32) -> Result<(), Fail> {
        if block == 0 {
            return Err(self.message_trans_error(BAD_DESCRIPTOR, b""));
        }
        let descriptor = self.real_descriptor(block)?;
        self.world.message_trans.drop_index(descriptor);
        self.release_descriptor(descriptor)?;
        let magic = self.read_word(block.wrapping_add(MAGIC))?;
        // The magic word decides, not the flags a program may since have
        // changed.
        if magic & (flag::FAST | flag::PROXY) != 0 {
            for offset in [MAGIC, FLAGS, DATA] {
                self.write_word(block.wrapping_add(offset), 0)?;
            }
        }
        Ok(())
    }

    /// A descriptor of the open list taken off it and freed with what it
    /// holds; nothing for one not on it.
    fn release_descriptor(&mut self, descriptor: u32) -> Result<(), Fail> {
        let Some(at) = self
            .world
            .message_trans
            .open
            .iter()
            .position(|&d| d == descriptor)
        else {
            return Ok(());
        };
        self.world.message_trans.open.remove(at);
        let flags = self.read_word(descriptor.wrapping_add(FLAGS))?;
        if flags & flag::CLAIMED != 0 {
            let data = self.read_word(descriptor.wrapping_add(DATA))?;
            self.free_rma_quietly(data);
        }
        if flags & flag::ALLOCATED != 0 {
            self.free_rma_quietly(descriptor);
        }
        Ok(())
    }

    /// Every file held in place found again: those that moved or went are
    /// re-found at their next use, and their holders told, unless
    /// ResourceFS is dying.
    fn refind_in_place(&mut self, dying: bool) -> Result<(), Fail> {
        let mut changed = false;
        let open = self.world.message_trans.open.clone();
        for descriptor in open {
            let flags = self.read_word(descriptor.wrapping_add(FLAGS))?;
            if flags & flag::IN_PLACE == 0 {
                continue;
            }
            let data = self.read_word(descriptor.wrapping_add(DATA))?;
            let name = self.read_word(descriptor.wrapping_add(NAME))?;
            let same = match self.locate(name) {
                Ok(Located::InPlace { block }) => block == data,
                Ok(_) | Err(Fail::Error(_)) => false,
                Err(unwind) => return Err(unwind),
            };
            if !same {
                self.write_word(descriptor.wrapping_add(DATA), 0)?;
                self.world.message_trans.drop_index(descriptor);
                changed = true;
            }
        }
        if changed && !dying {
            let mut regs = Regs::default();
            regs.r[1] = service::MESSAGE_FILE_CLOSED;
            self.service(&mut regs)?;
        }
        Ok(())
    }

    /// Opens MessageTrans's own file and the global one, those not open yet.
    fn open_own_files(&mut self) -> Result<(), Fail> {
        let Some(workspace) = self.world.message_trans.workspace else {
            return Ok(());
        };
        for (at, name, global) in [
            (OWN_AT, OWN_NAME_AT, false),
            (GLOBAL_AT, GLOBAL_NAME_AT, true),
        ] {
            let open = if global {
                self.world.message_trans.global_open
            } else {
                self.world.message_trans.own_open
            };
            if open {
                continue;
            }
            match self.open_message_file(workspace + at, workspace + name, 0, 0) {
                Ok(()) => {
                    if global {
                        self.world.message_trans.global_open = true;
                    } else {
                        self.world.message_trans.own_open = true;
                    }
                }
                Err(Fail::Error(_)) => {}
                Err(unwind) => return Err(unwind),
            }
        }
        Ok(())
    }
}

/// `OS_Find`'s flag for no path, `OS_FSControl` 21 and `Squash_Decompress`.
const NO_PATH: u32 = 3;
const READ_FS_HANDLE: u32 = 21;
const SQUASH_DECOMPRESS: u32 = 0x4_2701;
const SQUASH_WORKSPACE: u32 = 8;
/// A squashed file's header: its magic and the words after it.
const SQSH: u32 = u32::from_le_bytes(*b"SQSH");
const SQUASH_HEADER: u32 = 20;

fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let workspace = os.rma_claim(WORKSPACE)?;
    os.write_word(regs.r[12], workspace)?;
    for offset in (0..BUFFERS_AT).step_by(4) {
        os.write_word(workspace + offset, 0)?;
    }
    os.write_bytes(workspace + OWN_NAME_AT, OWN_NAME)?;
    os.write_bytes(workspace + GLOBAL_NAME_AT, GLOBAL_NAME)?;
    os.world.message_trans = State::new();
    os.world.message_trans.workspace = Some(workspace);
    os.open_own_files()?;
    if let Some(callback) = os.routine_of(&MESSAGE_TRANS, CLOSED_CALLBACK) {
        let mut add = Regs::default();
        add.r[..2].copy_from_slice(&[callback, workspace]);
        os.perform(crate::swi::X | crate::swi::os::ADD_CALLBACK, &mut add)?;
    }
    Ok(())
}

/// `Service_MessageFileClosed`, from a callback once it is up: a client's
/// descriptor from before it started opens its file again.
fn announce_closed(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let mut closed = Regs::default();
    closed.r[1] = service::MESSAGE_FILE_CLOSED;
    os.service(&mut closed)?;
    Ok(())
}

/// Its death: every file it holds let go, the callers' blocks marked to open
/// again at their next use.
fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    while let Some(&descriptor) = os.world.message_trans.open.first() {
        let block = os.read_word(descriptor.wrapping_add(BACK)).unwrap_or(0);
        os.world.message_trans.drop_index(descriptor);
        os.release_descriptor(descriptor)?;
        if block != 0 && os.read_word(block.wrapping_add(MAGIC)) == Ok(FAST) {
            let flags = os.read_word(block.wrapping_add(FLAGS))?;
            os.write_word(block.wrapping_add(DATA), 0)?;
            os.write_word(block.wrapping_add(MAGIC), 0)?;
            os.write_word(
                block.wrapping_add(FLAGS),
                (flags & (flag::FAST | flag::INDEX)) | flag::REOPEN,
            )?;
        }
    }
    os.world.message_trans = State::new();
    Ok(())
}

fn service(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Unwind> {
    let result = match regs.r[1] {
        service::RESOURCE_FS_STARTED => {
            os.refind_in_place(false).and_then(|()| os.open_own_files())
        }
        service::RESOURCE_FS_DYING => os.refind_in_place(true),
        service::TERRITORY_STARTED => os.refind_in_place(false),
        _ => Ok(()),
    };
    match result {
        Err(Fail::Unwind(unwind)) => Err(unwind),
        _ => Ok(()),
    }
}

fn swi(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    match CHUNK + offset {
        FILE_INFO => {
            let located = os.locate(regs.r[1])?;
            let size = os.needed(located)?;
            regs.r[0] = located.flags() & flag::PUBLIC & flag::IN_PLACE;
            regs.r[2] = size;
            Ok(())
        }
        OPEN_FILE => os.open_message_file(regs.r[0], regs.r[1], regs.r[2], regs.r[3]),
        LOOKUP => lookup(os, regs),
        MAKE_MENUS => menus::make_menus(os, regs),
        CLOSE_FILE => os.close_message_file(regs.r[0]),
        ENUMERATE_TOKENS => enumerate_tokens(os, regs),
        ERROR_LOOKUP => error_lookup(os, regs),
        GS_LOOKUP => gs_lookup(os, regs),
        COPY_ERROR => {
            let (number, text) = os.read_error(regs.r[0])?;
            Err(os.made_error(number, &text))
        }
        DICTIONARY => dictionary(os, regs),
        _ => Err(os.message_trans_global_error(
            crate::module::SWI_OUT_OF_RANGE,
            MESSAGE_TRANS.title.as_bytes(),
        )),
    }
}

/// `MessageTrans_Lookup`: R2 the value in place, or expanded into R2's
/// buffer of R3 bytes; R3 its length, R1 past the token.
fn lookup(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let found = os.look_up_token(regs.r[0], regs.r[1])?;
    regs.r[1] = found.end;
    if regs.r[2] == 0 {
        regs.r[2] = found.value;
        regs.r[3] = found.length;
        return Ok(());
    }
    let parameters = os.parameters(regs)?;
    regs.r[3] = os.expand_into(found, &parameters, regs.r[2], regs.r[3])?;
    Ok(())
}

/// The smallest buffer an error block fits in: its number and a terminator.
const ERROR_LEAST: u32 = 5;

/// `MessageTrans_ErrorLookup`: the error block at R0's token looked up into
/// R2's buffer, or the next of MessageTrans's; always an error.
fn error_lookup(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let block = regs.r[0];
    let number = os.read_word(block)?;
    let (buffer, size) = match regs.r[2] {
        0 => match os.next_buffer() {
            Some(buffer) => (buffer, BUFFER),
            None => return Err(os.no_memory()),
        },
        // An error block needs its number and a terminated text.
        _ if regs.r[3] < ERROR_LEAST => {
            return Err(os.message_trans_global_error(BUFFER_OVERFLOW, b""))
        }
        buffer => (buffer, regs.r[3]),
    };
    let found = os.look_up_token(regs.r[1], block.wrapping_add(4))?;
    let parameters = os.parameters(regs)?;
    os.expand_into(
        found,
        &parameters,
        buffer.wrapping_add(4),
        size.saturating_sub(4),
    )?;
    os.write_word(buffer, number)?;
    Err(Fail::Error(buffer))
}

/// `MessageTrans_GSLookup`: a lookup, then `OS_GSTrans` into the caller's
/// buffer.
fn gs_lookup(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    if regs.r[2] == 0 {
        return lookup(os, regs);
    }
    let found = os.look_up_token(regs.r[0], regs.r[1])?;
    let parameters = os.parameters(regs)?;
    let temporary = os.rma_claim(regs.r[3].max(1))?;
    let expanded = os.expand_into(found, &parameters, temporary, regs.r[3]);
    let result = expanded.and_then(|_| {
        let mut trans = Regs::default();
        trans.r[..3].copy_from_slice(&[temporary, regs.r[2], regs.r[3]]);
        os.perform(X | os::GS_TRANS, &mut trans)
            .map(|()| trans.r[2])
    });
    os.free_rma_quietly(temporary);
    regs.r[3] = result?;
    Ok(())
}

/// `MessageTrans_EnumerateTokens`: the next token R1's pattern matches from
/// R4 on, copied into R2's buffer with the pattern's terminator; R2 zero when
/// there are no more.
fn enumerate_tokens(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let (pattern, _) = os.read_until(regs.r[1], crate::error::TEXT, |b| {
        b <= b' ' || b == b',' || b == b')' || b == b':'
    })?;
    let terminator = os.read_byte(regs.r[1].wrapping_add(len32(pattern.len())))?;
    if scan::pattern_misplaced_star(&pattern) {
        return Err(os.message_trans_error(SYNTAX, &pattern));
    }
    let file = os.opened(regs.r[0])?;
    let bytes = os.file_bytes(file)?;
    let Some((range, next)) = scan::next_token(&bytes, &pattern, regs.r[4] as usize) else {
        regs.r[2] = 0;
        return Ok(());
    };
    let token = &bytes[range];
    if regs.r[3] <= len32(token.len()) {
        return Err(os.message_trans_global_error(BUFFER_OVERFLOW, b""));
    }
    os.write_bytes(regs.r[2], token)?;
    os.write_byte(regs.r[2].wrapping_add(len32(token.len())), terminator)?;
    regs.r[3] = len32(token.len());
    regs.r[4] = len32(next);
    Ok(())
}

/// `MessageTrans_Dictionary`: R0 MessageTrans's dictionary, which genuine
/// RISC OS 5 keeps apart from the kernel's.
#[expect(clippy::unnecessary_wraps, reason = "a SWI handler's signature")]
fn dictionary(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    regs.r[0] = os.world.dictionaries.messages;
    Ok(())
}

/// A module's own message file, opened when its first message is wanted:
/// where its descriptor and its name are in the module's workspace.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Own {
    descriptor: u32,
    name: u32,
    /// Where an error's request is made: its number, token and parameters.
    request: u32,
    open: bool,
}

/// The room a module's workspace gives an own file's request.
pub const REQUEST: u32 = 256;
/// An own file's descriptor.
pub const OWN_DESCRIPTOR: u32 = 16;

impl Default for Own {
    fn default() -> Self {
        Self::new()
    }
}

impl Own {
    /// No file.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            descriptor: 0,
            name: 0,
            request: 0,
            open: false,
        }
    }

    /// The file named at `name`, its descriptor at `descriptor` and its
    /// requests made at `request`, all in the module's workspace.
    #[must_use]
    pub const fn at(descriptor: u32, name: u32, request: u32) -> Self {
        Self {
            descriptor,
            name,
            request,
            open: false,
        }
    }

    pub(crate) const fn is_open(self) -> bool {
        self.open
    }

    pub(crate) const fn descriptor(self) -> u32 {
        self.descriptor
    }

    pub(crate) const fn request(self) -> u32 {
        self.request
    }
}

/// Which own file in the world: none while its module keeps none.
pub type OwnOf = fn(&mut World) -> Option<&mut Own>;

/// The own file `own` names as it stands, no file while there is none.
fn own_now(world: &mut World, own: OwnOf) -> Own {
    own(world).map_or(Own::new(), |file| *file)
}

impl Os<'_> {
    /// The module's own file `own`, opened if it is not.
    pub(crate) fn own_open(&mut self, own: OwnOf) -> Result<Own, Fail> {
        let file = own_now(self.world, own);
        if !file.open {
            let mut open = Regs::default();
            open.r[..3].copy_from_slice(&[file.descriptor, file.name, 0]);
            self.perform(X | OPEN_FILE, &mut open)?;
            if let Some(file) = own(self.world) {
                file.open = true;
            }
        }
        Ok(own_now(self.world, own))
    }

    /// `error` from the module's own file `own`, `%0` onwards `parameters`:
    /// MessageTrans's answer, or its own error where it cannot give one.
    pub fn own_error(&mut self, own: OwnOf, error: Error, parameters: &[&[u8]]) -> Fail {
        if own_now(self.world, own).descriptor == 0 {
            return self.error_with(error, parameters);
        }
        match self.own_open(own) {
            Ok(file) => self.request_error(file.request, file.descriptor, error, parameters),
            Err(fail) => fail,
        }
    }

    /// The text `token` stands for in the module's own file `own`, `%0`
    /// onwards `parameters`, as `MessageTrans_Lookup` gives it.
    ///
    /// # Errors
    ///
    /// MessageTrans's, where the file cannot be opened or holds no such
    /// token; "not enough memory" where the host has no room for the text.
    pub fn own_text(
        &mut self,
        own: OwnOf,
        token: &str,
        parameters: &[&[u8]],
    ) -> Result<alloc::vec::Vec<u8>, Fail> {
        let file = self.own_open(own)?;
        let token = token.as_bytes();
        let parameter_room: u32 = parameters
            .iter()
            .take(4)
            .map(|parameter| len32(parameter.len()) + 1)
            .sum();
        let room = (len32(token.len()) + 1 + parameter_room).next_multiple_of(4) + BUFFER;
        self.below_supervisor_stack(room, |os, frame| {
            os.write_terminated(frame, token, 0, token.len() + 1)?;
            let mut regs = Regs::default();
            let mut at = frame + len32(token.len()) + 1;
            for (slot, parameter) in regs.r[4..8].iter_mut().zip(parameters) {
                os.write_terminated(at, parameter, 0, parameter.len() + 1)?;
                *slot = at;
                at += len32(parameter.len()) + 1;
            }
            let buffer = at.next_multiple_of(4);
            regs.r[..4].copy_from_slice(&[file.descriptor, frame, buffer, BUFFER]);
            os.perform(X | LOOKUP, &mut regs)?;
            let mut text = alloc::vec::Vec::new();
            let length = regs.r[3].min(BUFFER) as usize;
            if text.try_reserve_exact(length).is_err() {
                return Err(os.no_memory());
            }
            text.resize(length, 0);
            os.read_bytes(regs.r[2], &mut text)?;
            Ok(text)
        })
    }

    /// `error` from the global file, `%0` onwards `parameters`, the request
    /// made in the module's own file's room.
    pub(crate) fn global_error(&mut self, own: OwnOf, error: Error, parameters: &[&[u8]]) -> Fail {
        let file = own_now(self.world, own);
        if file.request == 0 {
            return self.error_with(error, parameters);
        }
        self.request_error(file.request, 0, error, parameters)
    }

    /// `MessageTrans_ErrorLookup` of `error` in `descriptor`, the request
    /// made at `request`.
    pub(crate) fn request_error(
        &mut self,
        request: u32,
        descriptor: u32,
        error: Error,
        parameters: &[&[u8]],
    ) -> Fail {
        let token = error.token.as_bytes();
        let mut at = request.wrapping_add(4 + len32(token.len()) + 1);
        let mut regs = Regs::default();
        let written = self
            .write_word(request, error.number)
            .and_then(|()| self.write_bytes(request.wrapping_add(4), token))
            .and_then(|()| self.write_byte(request.wrapping_add(4 + len32(token.len())), 0));
        if let Err(fail) = written {
            return fail;
        }
        let end = request.wrapping_add(REQUEST);
        for (slot, parameter) in regs.r[4..8].iter_mut().zip(parameters) {
            let room = end.wrapping_sub(at).saturating_sub(1) as usize;
            let parameter = &parameter[..parameter.len().min(room)];
            if let Err(fail) = self
                .write_bytes(at, parameter)
                .and_then(|()| self.write_byte(at.wrapping_add(len32(parameter.len())), 0))
            {
                return fail;
            }
            *slot = at;
            at = at.wrapping_add(len32(parameter.len()) + 1);
        }
        regs.r[..4].copy_from_slice(&[request, descriptor, 0, 0]);
        match self.perform(X | ERROR_LOOKUP, &mut regs) {
            Err(fail) => fail,
            Ok(()) => Fail::Error(regs.r[0]),
        }
    }

    /// Opens the kernel's own file through MessageTrans, once ResourceFS
    /// holds it; its errors are looked up there from then on.
    pub(crate) fn open_kernel_messages(&mut self) -> Result<(), Unwind> {
        let file = self.world.kernel_messages;
        if file.descriptor == 0 || file.open {
            return Ok(());
        }
        let mut open = Regs::default();
        open.r[..3].copy_from_slice(&[file.descriptor, file.name, 0]);
        if self.perform_quietly(X | OPEN_FILE, &mut open)? {
            self.world.kernel_messages.open = true;
        }
        Ok(())
    }

    /// The module's own file closed, if open.
    pub fn close_own(&mut self, own: OwnOf) -> Result<(), Fail> {
        let file = own_now(self.world, own);
        if file.open {
            if let Some(file) = own(self.world) {
                file.open = false;
            }
            let mut close = Regs::default();
            close.r[0] = file.descriptor;
            self.perform_quietly(X | CLOSE_FILE, &mut close)?;
        }
        Ok(())
    }

    /// The error block at `block`: its number and text.
    ///
    /// # Errors
    ///
    /// The abort that reading its number meets.
    pub fn read_error(&mut self, block: u32) -> Result<(u32, Vec<u8>), Fail> {
        let number = self.read_word(block)?;
        let text = self
            .read_zstring(block.wrapping_add(4), TEXT_MOST as usize)
            .or_else(|fail| match fail {
                Fail::Error(_) => Ok(Vec::new()),
                unwind @ Fail::Unwind(_) => Err(unwind),
            })?;
        Ok((number, text))
    }
}

#[cfg(test)]
pub(crate) mod tests;
