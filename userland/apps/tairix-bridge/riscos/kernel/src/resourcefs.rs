//! ResourceFS (`plans/RISCOS-EMULATOR.md` §4.6): files a module keeps in its
//! own memory, in blocks it registers and keeps in place, found by name —
//! newest registration first, as the PRM has it, case folded through the
//! territory's upper-case table, a directory being any leading part of a
//! name that ends at a full stop. Its face as a filing system,
//! through which FileSwitch reads the files, comes with FileSwitch.
//!
//! A block is guest memory: every entry is read bounded and a malformed one
//! ends its block's walk.

use alloc::boxed::Box;
use alloc::vec::Vec;

use crate::error::Error;
use crate::filing::fs::{self as interface, block};
use crate::module::{Builtin, Command, CommandKind, Help, Resource, Services, Swis};
use crate::msgtrans::Own;
use crate::os::{len32, Fail, Os, Regs, Unwind, World};
use crate::swi::{os, X};

use crate::swi::chunk::RESOURCE_FS as CHUNK;
pub(crate) use crate::swi::resource_fs::{DEREGISTER_FILES, REGISTER_FILES};

/// The services ResourceFS issues.
const STARTED: u32 = 0x59;
const DYING: u32 = 0x5A;
const STARTING: u32 = 0x60;
/// `Service_TerritoryStarted`: the upper-case table may have changed.
pub(crate) const TERRITORY_STARTED: u32 = 0x75;

/// ResourceFS's filing system number, and its information word: read-only.
pub(crate) const FS_NUMBER: u32 = 46;
const FS_INFO: u32 = FS_NUMBER | interface::info::READ_ONLY;

/// The longest name an entry may have: a containment bound on the walk of a
/// block the guest wrote, not a capacity.
const NAME_MOST: u32 = 1024;
/// An entry's words before its name.
const HEADER: u32 = 20;
/// The least an entry may be: its words, a name of one character and its
/// terminator padded, and the data's length word.
const LEAST_ENTRY: u32 = HEADER + 4 + 4;

/// "%0 has these files registered already" and "%0 has no such files registered".
const ALREADY_REGISTERED: Error = Error {
    number: 0x1_2E00,
    token: "FilesAlreadyRegistered",
};
const NOT_REGISTERED: Error = Error {
    number: 0x1_2E01,
    token: "FilesNotRegistered",
};

/// The module's own messages.
const MESSAGES: &[u8] = include_bytes!("../resources/ResourceFS/Messages");
/// Its file's name, which its descriptor keeps.
const MESSAGES_NAME: &[u8] = b"Resources:$.Resources.ResourceFS.Messages\0";

/// The filing system's name.
const FS_NAME: &[u8] = b"Resources\0";

/// Where the workspace keeps the messages descriptor, an empty name, the
/// file's name, an error's request, and the filing system's information
/// block and name.
const OWN_AT: u32 = 0;
const EMPTY_AT: u32 = crate::msgtrans::OWN_DESCRIPTOR;
const NAME_AT: u32 = EMPTY_AT + 4;
const REQUEST_AT: u32 = (NAME_AT + len32(MESSAGES_NAME.len())).next_multiple_of(4);
const FS_BLOCK_AT: u32 = REQUEST_AT + crate::msgtrans::REQUEST;
const FS_NAME_AT: u32 = FS_BLOCK_AT + 4 * len32(block::WORDS);
const WORKSPACE: u32 = (FS_NAME_AT + len32(FS_NAME.len())).next_multiple_of(4);

/// The routines it hands out: the registration a module calls during
/// `Service_ResourceFSStarting`, the callback that issues
/// `Service_ResourceFSStarted`, and the filing system's entries, by their
/// words in its information block.
const REGISTER_STARTING: usize = 0;
const STARTED_CALLBACK: usize = 1;
const FS_ENTRIES: [(usize, usize); 7] = [
    (block::OPEN, 2),
    (block::GET_BYTES, 3),
    (block::PUT_BYTES, 4),
    (block::ARGS, 5),
    (block::CLOSE, 6),
    (block::FILE, 7),
    (block::FUNC, 8),
];

/// ResourceFS.
pub(crate) static RESOURCE_FS: Builtin = Builtin {
    title: "ResourceFS",
    help_name: None,
    bridge: false,
    swis: Some(Swis {
        chunk: CHUNK,
        prefix: "ResourceFS",
        names: &["RegisterFiles", "DeregisterFiles"],
        handler: swi,
    }),
    services: Some(Services {
        numbers: &[interface::REDECLARE, TERRITORY_STARTED],
        handler: service,
    }),
    commands: &[Command {
        name: "ResourceFS",
        kind: CommandKind::Command,
        min: 0,
        max: 0,
        translate: 0,
        help: Help::Text("*ResourceFS makes ResourceFS the current filing system."),
        syntax: Some("Syntax: *ResourceFS"),
        run: Some(select),
    }],
    init: Some(init),
    finalise: Some(finalise),
    start: None,
    routines: &[
        register_starting,
        started,
        fs::open,
        fs::get_bytes,
        fs::put_bytes,
        fs::args,
        fs::close,
        fs::file,
        fs::func,
    ],
    vectors: &[],
    resources: &[Resource {
        name: "Resources.ResourceFS.Messages",
        file_type: 0xFFF,
        data: MESSAGES,
    }],
};

/// An entry of a registered block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Entry {
    /// Its address.
    pub(crate) at: u32,
    /// The next entry's address, or none after the last.
    next: Option<u32>,
    pub(crate) load: u32,
    pub(crate) exec: u32,
    pub(crate) length: u32,
    pub(crate) attributes: u32,
    name: u32,
    name_length: u32,
    /// Its data's address: after the name, padded, and the length word.
    pub(crate) data: u32,
}

/// What a name finds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Object {
    /// A file: its entry.
    File(Entry),
    /// A directory: the root, or the leading part of the entry's name.
    Directory {
        /// The load and execution addresses the entry gives it.
        load: u32,
        exec: u32,
    },
}

/// An index entry: its folded name's place in the index's bytes, its place
/// in registration order, and the entry.
#[derive(Clone, Copy, Debug)]
struct Indexed {
    start: u32,
    length: u32,
    ordinal: u32,
    entry: Entry,
}

/// Every registered name, folded and sorted, so a name without wildcards is
/// found by a binary search.
struct Index {
    names: Vec<u8>,
    entries: Vec<Indexed>,
}

impl Index {
    fn name(&self, indexed: &Indexed) -> &[u8] {
        let start = indexed.start as usize;
        self.names
            .get(start..start + indexed.length as usize)
            .unwrap_or(&[])
    }
}

/// ResourceFS's state.
pub(crate) struct State {
    /// The registered blocks, the newest first.
    blocks: Vec<u32>,
    /// The table names are folded through; none compares them as they are.
    upper: Option<Box<[u8; 256]>>,
    index: Option<Index>,
    /// `Service_ResourceFSStarted` is waiting for its callback.
    started_pending: bool,
    /// The workspace, while it is initialised.
    workspace: Option<u32>,
    /// What FileSwitch has open, by handle less one.
    open: Vec<Option<fs::Opened>>,
    pub(crate) own: Own,
}

impl State {
    pub(crate) const fn new() -> Self {
        Self {
            blocks: Vec::new(),
            upper: None,
            index: None,
            started_pending: false,
            workspace: None,
            open: Vec::new(),
            own: Own::new(),
        }
    }

    fn fold(&self, byte: u8) -> u8 {
        self.upper
            .as_ref()
            .map_or(byte, |table| table[usize::from(byte)])
    }
}

/// Where a name begins below the root: past one `$.`, `@.`, `%.` or `\.`;
/// none for the root itself.
fn below_root(name: &[u8]) -> Option<&[u8]> {
    match name {
        [] | [b'$' | b'@' | b'%' | b'\\'] => None,
        [b'$' | b'@' | b'%' | b'\\', b'.', rest @ ..] => Some(rest),
        _ => Some(name),
    }
}

/// Whether `pattern`, `*` matching any run and `#` any one character within a
/// part, names the folded `name` whole, or a leading part of it that ends at
/// a full stop: a file, or a directory.
fn wild_match(pattern: &[u8], name: &[u8]) -> Option<bool> {
    fn part(pattern: &[u8], name: &[u8]) -> bool {
        match (pattern.split_first(), name.split_first()) {
            (None, None) => true,
            (Some((b'*', rest)), _) => {
                part(rest, name)
                    || name
                        .split_first()
                        .is_some_and(|(_, tail)| part(pattern, tail))
            }
            (Some((b'#', rest)), Some((_, tail))) => part(rest, tail),
            (Some((p, rest)), Some((n, tail))) => p == n && part(rest, tail),
            _ => false,
        }
    }
    let mut patterns = pattern.split(|&b| b == b'.');
    let mut names = name.split(|&b| b == b'.');
    loop {
        match (patterns.next(), names.next()) {
            (None, None) => return Some(true),
            (None, Some(_)) => return Some(false),
            (Some(_), None) => return None,
            (Some(p), Some(n)) => {
                if !part(p, n) {
                    return None;
                }
            }
        }
    }
}

impl Os<'_> {
    /// The entry at `at`, or none where its block ends there or the entry
    /// cannot be one.
    fn entry_at(&mut self, at: u32) -> Result<Option<Entry>, Fail> {
        let offset = self.read_word(at)?;
        if offset < LEAST_ENTRY {
            return Ok(None);
        }
        let mut words = [0; 4];
        for (n, word) in (1..).zip(words.iter_mut()) {
            *word = self.read_word(at.wrapping_add(4 * n))?;
        }
        let [load, exec, length, attributes] = words;
        let name = at.wrapping_add(HEADER);
        let room = (offset - HEADER).min(NAME_MOST);
        let (text, ended) = self.read_until(name, room as usize, |b| b == 0)?;
        if !ended || text.is_empty() {
            return Ok(None);
        }
        let name_length = len32(text.len());
        let data = name
            .wrapping_add(name_length + 1)
            .next_multiple_of(4)
            .wrapping_add(4);
        let next = at.checked_add(offset);
        Ok(Some(Entry {
            at,
            next,
            load,
            exec,
            length,
            attributes,
            name,
            name_length,
            data,
        }))
    }

    /// Each entry of every registered block, the newest block first and each
    /// block in order, given to `each` with its folded name, until it answers.
    fn walk_entries<T>(
        &mut self,
        mut each: impl FnMut(&mut Self, Entry, &[u8]) -> Result<Option<T>, Fail>,
    ) -> Result<Option<T>, Fail> {
        let mut block = 0;
        while let Some(&start) = self.world.resource_fs.blocks.get(block) {
            block += 1;
            let mut at = Some(start);
            while let Some(here) = at {
                self.yield_point()?;
                let Some(entry) = self.entry_at(here)? else {
                    break;
                };
                let mut name = Vec::new();
                name.try_reserve_exact(entry.name_length as usize)
                    .map_err(|_| self.no_memory())?;
                name.resize(entry.name_length as usize, 0);
                self.read_bytes(entry.name, &mut name)?;
                for byte in &mut name {
                    *byte = self.world.resource_fs.fold(*byte);
                }
                if let Some(found) = each(self, entry, &name)? {
                    return Ok(Some(found));
                }
                at = entry.next;
            }
        }
        Ok(None)
    }

    /// The index of every registered name, built if it is not held.
    fn build_index(&mut self) -> Result<(), Fail> {
        if self.world.resource_fs.index.is_some() {
            return Ok(());
        }
        let mut index = Index {
            names: Vec::new(),
            entries: Vec::new(),
        };
        let built = self.walk_entries(|os, entry, name| {
            if index.names.try_reserve(name.len()).is_err() || index.entries.try_reserve(1).is_err()
            {
                return Err(os.no_memory());
            }
            index.entries.push(Indexed {
                start: len32(index.names.len()),
                length: len32(name.len()),
                ordinal: len32(index.entries.len()),
                entry,
            });
            index.names.extend_from_slice(name);
            Ok(None::<()>)
        });
        built?;
        let Index { names, mut entries } = index;
        // Stable, so of two entries of one name the newest block's, then the
        // earlier in its block, comes first.
        entries.sort_by(|a, b| {
            let name = |i: &Indexed| &names[i.start as usize..(i.start + i.length) as usize];
            name(a).cmp(name(b))
        });
        self.world.resource_fs.index = Some(Index { names, entries });
        Ok(())
    }

    /// The object `name`, the path after `Resources:`, finds.
    pub(crate) fn resource(&mut self, name: &[u8]) -> Result<Option<Object>, Fail> {
        let Some(path) = below_root(name) else {
            return Ok(Some(Object::Directory { load: 0, exec: 0 }));
        };
        let mut folded = Vec::new();
        folded
            .try_reserve_exact(path.len() + 1)
            .map_err(|_| self.no_memory())?;
        folded.extend(path.iter().map(|&b| self.world.resource_fs.fold(b)));
        if folded.iter().any(|&b| b == b'*' || b == b'#') {
            return self.walk_entries(|_, entry, stored| {
                Ok(wild_match(&folded, stored).map(|whole| {
                    if whole {
                        Object::File(entry)
                    } else {
                        Object::Directory {
                            load: entry.load,
                            exec: entry.exec,
                        }
                    }
                }))
            });
        }
        if !self.index_held()? {
            return self.walk_entries(|_, entry, stored| {
                Ok(if stored == folded.as_slice() {
                    Some(Object::File(entry))
                } else if stored.starts_with(&folded) && stored.get(folded.len()) == Some(&b'.') {
                    Some(Object::Directory {
                        load: entry.load,
                        exec: entry.exec,
                    })
                } else {
                    None
                })
            });
        }
        let Some(index) = self.world.resource_fs.index.as_ref() else {
            return Ok(None);
        };
        let at = index
            .entries
            .partition_point(|i| index.name(i) < folded.as_slice());
        if let Some(found) = index
            .entries
            .get(at)
            .filter(|i| index.name(i) == folded.as_slice())
        {
            return Ok(Some(Object::File(found.entry)));
        }
        folded.push(b'.');
        let at = index
            .entries
            .partition_point(|i| index.name(i) < folded.as_slice());
        Ok(index
            .entries
            .get(at)
            .filter(|i| index.name(i).starts_with(&folded))
            .map(|i| Object::Directory {
                load: i.entry.load,
                exec: i.entry.exec,
            }))
    }

    /// Whether the index is held, built if it can be: a walk of the blocks
    /// finds what it would where there is no room for it.
    fn index_held(&mut self) -> Result<bool, Fail> {
        match self.build_index() {
            Ok(()) => Ok(true),
            Err(Fail::Error(_)) => Ok(false),
            Err(unwind) => Err(unwind),
        }
    }

    /// Every entry whose folded name starts with `prefix`: its folded name,
    /// its place in registration order, and the entry.
    fn entries_under(&mut self, prefix: &[u8]) -> Result<Vec<(Vec<u8>, u32, Entry)>, Fail> {
        let mut under = Vec::new();
        if self.index_held()? {
            let Some(index) = self.world.resource_fs.index.as_ref() else {
                return Ok(under);
            };
            let start = index.entries.partition_point(|i| index.name(i) < prefix);
            let mut room = true;
            for indexed in &index.entries[start..] {
                let name = index.name(indexed);
                if !name.starts_with(prefix) {
                    break;
                }
                let Some(name) = tairix_util::fallible::copied(name) else {
                    room = false;
                    break;
                };
                if under.try_reserve(1).is_err() {
                    room = false;
                    break;
                }
                under.push((name, indexed.ordinal, indexed.entry));
            }
            return if room {
                Ok(under)
            } else {
                Err(self.no_memory())
            };
        }
        let mut ordinal = 0;
        self.walk_entries(|os, entry, name| {
            if name.starts_with(prefix) {
                let copy = tairix_util::fallible::copied(name).ok_or_else(|| os.no_memory())?;
                under.try_reserve(1).map_err(|_| os.no_memory())?;
                under.push((copy, ordinal, entry));
            }
            ordinal += 1;
            Ok(None::<()>)
        })?;
        Ok(under)
    }

    /// The filing system registered with FileSwitch, if FileSwitch is there
    /// to take it.
    fn declare(&mut self) -> Result<(), Fail> {
        let (Some(base), Some(workspace)) = (
            self.module_of(&RESOURCE_FS),
            self.world.resource_fs.workspace,
        ) else {
            return Ok(());
        };
        let mut words = [0; block::WORDS];
        words[block::NAME] = (workspace + FS_NAME_AT).wrapping_sub(base);
        words[block::STARTUP] = (workspace + EMPTY_AT).wrapping_sub(base);
        words[block::INFO] = FS_INFO;
        for (word, routine) in FS_ENTRIES {
            let Some(at) = self.routine_of(&RESOURCE_FS, routine) else {
                return Ok(());
            };
            words[word] = at.wrapping_sub(base);
        }
        for (n, word) in (0..).zip(words) {
            self.write_word(workspace + FS_BLOCK_AT + 4 * n, word)?;
        }
        let mut regs = Regs::default();
        regs.r[..4].copy_from_slice(&[
            os::fs_control::ADD,
            base,
            (workspace + FS_BLOCK_AT).wrapping_sub(base),
            workspace,
        ]);
        self.perform_quietly(X | os::FS_CONTROL, &mut regs)?;
        Ok(())
    }

    /// Registers the block at `block`, newest: refused if it is registered.
    fn register(&mut self, block: u32) -> Result<(), Fail> {
        if self.world.resource_fs.blocks.contains(&block) {
            return Err(self.own_error(own, ALREADY_REGISTERED, &[RESOURCE_FS.title.as_bytes()]));
        }
        if self.world.resource_fs.blocks.try_reserve(1).is_err() {
            return Err(self.no_memory());
        }
        self.world.resource_fs.blocks.insert(0, block);
        self.world.resource_fs.index = None;
        self.modifying_files(interface::file::CREATE)
    }

    /// Deregisters the block at `block`: refused if it is not registered.
    fn deregister(&mut self, block: u32) -> Result<(), Fail> {
        let Some(place) = self
            .world
            .resource_fs
            .blocks
            .iter()
            .position(|&b| b == block)
        else {
            return Err(self.own_error(own, NOT_REGISTERED, &[RESOURCE_FS.title.as_bytes()]));
        };
        self.world.resource_fs.index = None;
        self.modifying_files(interface::file::DELETE)?;
        self.world.resource_fs.blocks.remove(place);
        Ok(())
    }

    /// `UpCall_ModifyingFile` for every directory: it cannot say which
    /// change. Its errors are passed over.
    fn modifying_files(&mut self, reason: u32) -> Result<(), Fail> {
        let empty = self
            .world
            .resource_fs
            .workspace
            .map_or(0, |workspace| workspace + EMPTY_AT);
        let mut regs = Regs::default();
        regs.r[0] = interface::modifying::UPCALL;
        regs.r[1] = empty;
        regs.r[8] = FS_INFO;
        regs.r[9] = reason;
        self.perform_quietly(X | os::UPCALL, &mut regs)?;
        Ok(())
    }

    /// `Service_ResourceFSStarted` from a callback, once for any number of
    /// changes before it runs.
    fn schedule_started(&mut self) -> Result<(), Fail> {
        if self.world.resource_fs.started_pending {
            return Ok(());
        }
        let Some(callback) = self.routine_of(&RESOURCE_FS, STARTED_CALLBACK) else {
            return Ok(());
        };
        let mut regs = Regs::default();
        regs.r[0] = callback;
        regs.r[1] = self.world.resource_fs.workspace.unwrap_or(0);
        self.perform(X | os::ADD_CALLBACK, &mut regs)?;
        self.world.resource_fs.started_pending = true;
        Ok(())
    }

    /// The territory's upper-case table, which names are folded through
    /// from now; none while no territory answers.
    fn read_upper_case(&mut self) -> Result<(), Unwind> {
        let mut regs = Regs::default();
        regs.r[0] = u32::MAX;
        let table = if self.perform_quietly(X | crate::territory::UPPER_CASE_TABLE, &mut regs)? {
            let mut table = Box::new([0; 256]);
            match self.read_bytes(regs.r[0], &mut table[..]) {
                Ok(()) => Some(table),
                Err(Fail::Unwind(unwind)) => return Err(unwind),
                Err(Fail::Error(_)) => None,
            }
        } else {
            None
        };
        self.world.resource_fs.upper = table;
        self.world.resource_fs.index = None;
        Ok(())
    }
}

/// ResourceFS's own state in the world.
#[expect(clippy::unnecessary_wraps, reason = "an own-file accessor's signature")]
fn own(world: &mut World) -> Option<&mut Own> {
    Some(&mut world.resource_fs.own)
}

/// Its initialisation: its workspace, the case table, then
/// `Service_ResourceFSStarting` for the modules that keep files, and
/// `Service_ResourceFSStarted` when the callback runs.
fn init(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let workspace = os.rma_claim(WORKSPACE)?;
    os.write_word(regs.r[12], workspace)?;
    os.write_word(workspace + EMPTY_AT, 0)?;
    os.write_bytes(workspace + NAME_AT, MESSAGES_NAME)?;
    os.world.resource_fs = State::new();
    os.world.resource_fs.workspace = Some(workspace);
    os.world.resource_fs.own = Own::at(
        workspace + OWN_AT,
        workspace + NAME_AT,
        workspace + REQUEST_AT,
    );
    os.write_bytes(workspace + FS_NAME_AT, FS_NAME)?;
    os.read_upper_case()?;
    os.declare()?;
    if let Some(routine) = os.routine_of(&RESOURCE_FS, REGISTER_STARTING) {
        let mut starting = Regs::default();
        starting.r[1] = STARTING;
        starting.r[2] = routine;
        starting.r[3] = workspace;
        os.service(&mut starting)?;
    }
    os.schedule_started()
}

/// Its finalisation: the callback cancelled, its messages closed, every
/// block let go with its `UpCall`, then `Service_ResourceFSDying`.
fn finalise(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.world.resource_fs.index = None;
    if os.world.resource_fs.started_pending {
        if let Some(callback) = os.routine_of(&RESOURCE_FS, STARTED_CALLBACK) {
            let mut regs = Regs::default();
            regs.r[0] = callback;
            regs.r[1] = os.world.resource_fs.workspace.unwrap_or(0);
            os.perform_quietly(X | os::REMOVE_CALLBACK, &mut regs)?;
        }
        os.world.resource_fs.started_pending = false;
    }
    os.close_own(own)?;
    if let Some(workspace) = os.world.resource_fs.workspace {
        let mut remove = Regs::default();
        remove.r[..2].copy_from_slice(&[os::fs_control::REMOVE, workspace + FS_NAME_AT]);
        os.perform_quietly(X | os::FS_CONTROL, &mut remove)?;
    }
    os.world.resource_fs.open.clear();
    while !os.world.resource_fs.blocks.is_empty() {
        os.modifying_files(interface::file::DELETE)?;
        os.world.resource_fs.blocks.remove(0);
    }
    let mut dying = Regs::default();
    dying.r[1] = DYING;
    os.service(&mut dying)?;
    os.world.resource_fs.workspace = None;
    Ok(())
}

fn swi(os: &mut Os<'_>, offset: u32, regs: &mut Regs) -> Result<(), Fail> {
    match offset {
        0 => {
            os.register(regs.r[0])?;
            os.schedule_started()
        }
        1 => {
            os.deregister(regs.r[0])?;
            os.schedule_started()
        }
        _ => Err(os.global_error(
            own,
            crate::module::SWI_OUT_OF_RANGE,
            &[RESOURCE_FS.title.as_bytes()],
        )),
    }
}

fn service(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Unwind> {
    match regs.r[1] {
        TERRITORY_STARTED => os.read_upper_case(),
        interface::REDECLARE => match os.declare() {
            Ok(()) | Err(Fail::Error(_)) => Ok(()),
            Err(Fail::Unwind(unwind)) => Err(unwind),
        },
        _ => Ok(()),
    }
}

/// The registration a module calls from `Service_ResourceFSStarting`: R0 its
/// block. `Service_ResourceFSStarted` follows the service anyway.
fn register_starting(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    os.register(regs.r[0])
}

/// The callback: `Service_ResourceFSStarted`.
fn started(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    os.world.resource_fs.started_pending = false;
    let mut regs = Regs::default();
    regs.r[1] = STARTED;
    os.service(&mut regs)?;
    Ok(())
}

/// `*ResourceFS`: the filing system selected.
fn select(os: &mut Os<'_>, _regs: &mut Regs) -> Result<(), Fail> {
    let name = os.scratch_text(FS_NAME)?;
    let mut regs = Regs::default();
    regs.r[0] = os::fs_control::SELECT;
    regs.r[1] = name;
    os.perform(X | os::FS_CONTROL, &mut regs)
}

mod fs;

#[cfg(test)]
mod tests;
