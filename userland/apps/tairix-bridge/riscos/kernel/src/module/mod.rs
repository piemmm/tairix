//! Modules (`plans/RISCOS-EMULATOR.md` §4.3): the seam every built-in module
//! joins the kernel through, and the module chain — each module's code, its
//! incarnations, the SWIs and services it registered — that `OS_Module`
//! keeps as RISC OS 5's module handler keeps it.
//!
//! A built-in module is Rust behind a real module image in the host pages,
//! so a reader of a module finds a built-in where it finds any. The kernel
//! calls its Rust directly; guest code that calls one of its entries reaches
//! the same Rust through the entry's host routine. A guest module, in either
//! profile, is code in the RMA the kernel calls by its entries.
//!
//! - [`inventory`]: every module of RISC OS 5.30's ROM, and what the
//!   emulator holds of each.
//! - [`squeeze`]: squeezed modules, expanded as they are initialised.

mod commands;
mod header;
mod image;
pub mod inventory;
mod lifecycle;
mod name;
mod reasons;
mod rom;
mod service;
pub mod squeeze;
mod swis;
pub(crate) mod utility;
mod version;

use alloc::vec::Vec;

use tairix_collections::{Key, OrderedMap, SlotKey, SlotMap};

use crate::memory::Bits;
use crate::os::{Fail, Os, Regs, Unwind};

pub(crate) use header::{Field, Header};
pub(crate) use image::EntryKind;
pub(crate) use rom::{arranged, entry_count, install, RomModule};
pub(crate) use swis::ChunkNames;

/// A built-in module's Rust for an entry, given the registers the entry's
/// protocol gives and answering in them.
pub type Code = for<'a, 'b, 'c> fn(&'a mut Os<'b>, &'c mut Regs) -> Result<(), Fail>;

/// A built-in module's SWI handler: the SWI's offset in the chunk, and the
/// caller's registers in and out.
pub type SwiCode = for<'a, 'b, 'c> fn(&'a mut Os<'b>, u32, &'c mut Regs) -> Result<(), Fail>;

/// A built-in module's service handler: R1 the service, which it zeroes to
/// claim it.
pub type ServiceCode = for<'a, 'b, 'c> fn(&'a mut Os<'b>, &'c mut Regs) -> Result<(), Unwind>;

/// A built-in module: the emulator's Rust for one of the inventory's
/// provided modules.
#[derive(Debug)]
pub struct Builtin {
    /// Its title, which names its row in the inventory.
    pub title: &'static str,
    /// The name its help string gives where its title is too long to line
    /// up the help strings at the sixteenth column.
    pub help_name: Option<&'static str>,
    /// Whether no guest module may take its place: without it the guest would
    /// have no way to reach the desktop.
    pub bridge: bool,
    /// Its SWIs.
    pub swis: Option<Swis>,
    /// The services it takes.
    pub services: Option<Services>,
    /// Its `*` commands, in its command table's order.
    pub commands: &'static [Command],
    /// Its initialisation: R10 its environment string, R11 the incarnations
    /// it already has, R12 its private word's address.
    pub init: Option<Code>,
    /// Its finalisation: R10 one, R11 the incarnation's place, R12 its
    /// private word's address.
    pub finalise: Option<Code>,
    /// Its start, entered in User mode as the application: R0 its
    /// parameters, R12 its private word's address.
    pub start: Option<Code>,
    /// Its other code, handed out by address — a callback, a table's entries
    /// — each entered as the protocol of whoever calls it says.
    pub routines: &'static [Code],
    /// The vectors whose default it is while it is active, in the kernel's
    /// place.
    pub vectors: &'static [VectorDefault],
    /// The files it keeps in ResourceFS, which the Messages module registers.
    pub resources: &'static [Resource],
    /// The code the guest runs or reads in its image, after its routines'
    /// entries: a table the guest branches through, a stub it returns by.
    pub code: Option<Emit>,
}

/// A built-in's guest code: its words, given its first routine's entry as an
/// offset from the code's start, each entry two words after the last. The
/// words are relative to the image, so a copy of it works as the original.
pub type Emit = fn(i32) -> Result<Vec<u32>, crate::memory::MemoryError>;

/// A vector whose default a built-in module is.
#[derive(Debug)]
pub struct VectorDefault {
    /// The vector.
    pub vector: crate::vectors::Vector,
    /// Its Rust, given the vector's registers and answering in them.
    pub code: Code,
}

/// A file a built-in module keeps in ResourceFS.
#[derive(Debug)]
pub struct Resource {
    /// Its name below `Resources:$`, its parts separated by full stops.
    pub name: &'static str,
    /// Its file type.
    pub file_type: u16,
    /// Its contents.
    pub data: &'static [u8],
}

/// A built-in module's SWIs.
#[derive(Debug)]
pub struct Swis {
    /// Its chunk.
    pub chunk: u32,
    /// Its decoding table's prefix.
    pub prefix: &'static str,
    /// Its SWIs' names, from offset 0.
    pub names: &'static [&'static str],
    /// Its handler.
    pub handler: SwiCode,
}

/// The services a built-in module takes, as its service table lists them.
#[derive(Debug)]
pub struct Services {
    /// The services' numbers.
    pub numbers: &'static [u32],
    /// Its handler.
    pub handler: ServiceCode,
}

/// What a command table's entry is.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CommandKind {
    /// A command, or a keyword with help alone.
    Command,
    /// A filing system's command, found only while it is current.
    Filing,
    /// A `*Configure` and `*Status` keyword.
    Configure,
}

/// A built-in module's `*` command, or a keyword it has help on.
#[derive(Debug)]
pub struct Command {
    /// Its name.
    pub name: &'static str,
    /// What it is.
    pub kind: CommandKind,
    /// The fewest parameters it takes.
    pub min: u8,
    /// The most.
    pub max: u8,
    /// The parameters the kernel passes through `GSTrans` before the command
    /// sees them, bit n for the nth of the first eight.
    pub translate: u8,
    /// What `*Help` says of it.
    pub help: Help,
    /// Its syntax, which `*Help` gives after text help and a syntax error
    /// gives alone.
    pub syntax: Option<&'static str>,
    /// What it does: R0 its parameters, R1 how many there are. None for a
    /// keyword that has help only.
    pub run: Option<Code>,
}

/// What `*Help` says of a command.
#[derive(Debug)]
pub enum Help {
    /// Text.
    Text(&'static str),
    /// Code that writes it: R0 a buffer of R1 bytes, R2 the module's base;
    /// R0 text to print as well, or zero.
    Code(Code),
}

/// A module's key: its identity while it is in the chain.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct ModuleKey(SlotKey);

impl Key for ModuleKey {
    fn from_slot(slot: SlotKey) -> Self {
        Self(slot)
    }

    fn slot(self) -> SlotKey {
        self.0
    }
}

/// What a module is: its code, checked header, profile and origin.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Shape {
    /// Its header's address.
    pub(crate) code: u32,
    /// Its header, as checked.
    pub(crate) header: Header,
    /// The profile its entries run in.
    pub(crate) bits: Bits,
    /// The ROM module it was started from, by its place among the
    /// session's built-ins.
    pub(crate) rom: Option<usize>,
    /// R11 for its first incarnation's initialisation.
    pub(crate) hardware: u32,
}

/// A module of the chain.
#[derive(Debug)]
pub(crate) struct Module {
    /// What it is.
    pub(crate) shape: Shape,
    /// Its incarnations, the preferred first.
    pub(crate) incarnations: Vec<Incarnation>,
    /// How its incarnations are offered services, once one registered.
    pub(crate) services: Option<service::Handler>,
}

/// An incarnation: its node in the system heap — a word, its private word,
/// then its postfix, so R12 + 4 is its postfix.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Incarnation {
    /// Its node.
    pub(crate) node: u32,
}

impl Incarnation {
    /// Its private word's address.
    pub(crate) const fn private(self) -> u32 {
        self.node + 4
    }

    /// Its postfix's address.
    pub(crate) const fn postfix(self) -> u32 {
        self.node + 8
    }
}

/// The session's modules: the chain, the SWI chunks and services they
/// registered, and the built-ins.
pub(crate) struct Modules {
    all: SlotMap<ModuleKey, Module>,
    /// Every module in the chain, in order.
    chain: Vec<ModuleKey>,
    /// The modules registered for each SWI chunk, the newest last: the one
    /// its SWIs reach.
    chunks: OrderedMap<u32, Vec<ModuleKey>>,
    /// The kernel owns FIQ, as it does at reset.
    pub(crate) kernel_has_fiq: bool,
    /// The session's built-ins, in the ROM's order.
    pub(crate) roms: Vec<RomModule>,
    /// What each built-in entry's host routine enters, by its number.
    pub(crate) entries: Vec<(usize, EntryKind)>,
    /// The word after the ROM chain, which holds the kernel's empty
    /// environment string.
    pub(crate) empty: u32,
}

/// Where a module is linked into the chain.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Place {
    /// Last.
    End,
    /// Just after this module, or last if it has gone; first if none.
    After(Option<ModuleKey>),
}

impl Modules {
    pub(crate) const fn new() -> Self {
        Self {
            all: SlotMap::new(),
            chain: Vec::new(),
            chunks: OrderedMap::new(),
            kernel_has_fiq: true,
            roms: Vec::new(),
            entries: Vec::new(),
            empty: 0,
        }
    }

    /// The module `key` names, if it is still held.
    pub(crate) fn get(&self, key: ModuleKey) -> Option<&Module> {
        self.all.get(key)
    }

    pub(crate) fn get_mut(&mut self, key: ModuleKey) -> Option<&mut Module> {
        self.all.get_mut(key)
    }

    /// The chain, in order.
    pub(crate) fn chain(&self) -> &[ModuleKey] {
        &self.chain
    }

    /// Every module held, in the chain or still being initialised.
    fn held(&self) -> impl Iterator<Item = &Module> {
        self.all.values()
    }

    /// The `index`th module of the chain.
    pub(crate) fn nth(&self, index: usize) -> Option<ModuleKey> {
        self.chain.get(index).copied()
    }

    /// Where `key` is in the chain.
    pub(crate) fn place_of(&self, key: ModuleKey) -> Option<usize> {
        self.chain.iter().position(|&k| k == key)
    }

    /// The module before `key` in the chain, none for the first.
    pub(crate) fn before(&self, key: ModuleKey) -> Option<ModuleKey> {
        let at = self.place_of(key)?;
        at.checked_sub(1).and_then(|before| self.nth(before))
    }

    /// Holds a module of `shape`, with no incarnation and not yet in the
    /// chain.
    fn hold(&mut self, shape: Shape) -> Option<ModuleKey> {
        self.all
            .try_insert(Module {
                shape,
                incarnations: Vec::new(),
                services: None,
            })
            .ok()
    }

    /// Room for one more incarnation of `key` in its list, so that one
    /// initialised can be recorded; whether there is.
    fn make_room(&mut self, key: ModuleKey) -> bool {
        self.all
            .get_mut(key)
            .is_some_and(|module| module.incarnations.try_reserve(1).is_ok())
    }

    /// Puts `incarnation` at `at` in `key`'s list; whether it could be,
    /// nothing changed if not.
    fn attach(&mut self, key: ModuleKey, at: usize, incarnation: Incarnation) -> bool {
        let Some(module) = self.all.get_mut(key) else {
            return false;
        };
        if module.incarnations.try_reserve(1).is_err() {
            return false;
        }
        let at = at.min(module.incarnations.len());
        module.incarnations.insert(at, incarnation);
        true
    }

    /// Takes incarnation `index` of `key` out of its list, keeping room for
    /// it there.
    fn detach(&mut self, key: ModuleKey, index: usize) -> Option<Incarnation> {
        let module = self.all.get_mut(key)?;
        if index >= module.incarnations.len() {
            return None;
        }
        Some(module.incarnations.remove(index))
    }

    /// Incarnation `index` of `key` is `moved` now, its node new.
    fn moved(&mut self, key: ModuleKey, index: usize, moved: Incarnation) {
        if let Some(slot) = self
            .all
            .get_mut(key)
            .and_then(|module| module.incarnations.get_mut(index))
        {
            *slot = moved;
        }
    }

    /// Whether `key` is held with no incarnation.
    fn empty(&self, key: ModuleKey) -> bool {
        self.get(key).is_some_and(|m| m.incarnations.is_empty())
    }

    /// Links `key` into the chain at `place`.
    fn link(&mut self, key: ModuleKey, place: Place) -> bool {
        if self.chain.try_reserve(1).is_err() {
            return false;
        }
        let at = match place {
            Place::End => self.chain.len(),
            Place::After(None) => 0,
            Place::After(Some(before)) => {
                self.place_of(before).map_or(self.chain.len(), |at| at + 1)
            }
        };
        self.chain.insert(at, key);
        true
    }

    /// Takes `key` out of the chain and lets go of it.
    fn drop_module(&mut self, key: ModuleKey) -> Option<Module> {
        self.chain.retain(|&k| k != key);
        self.all.remove(key)
    }

    /// Registers `key` for `chunk`, ahead of any module registered for it.
    fn register_chunk(&mut self, chunk: u32, key: ModuleKey) -> bool {
        if let Some(keys) = self.chunks.get_mut(&chunk) {
            if keys.try_reserve(1).is_err() {
                return false;
            }
            keys.push(key);
            return true;
        }
        let mut keys = Vec::new();
        if keys.try_reserve(1).is_err() {
            return false;
        }
        keys.push(key);
        self.chunks.try_insert(chunk, keys).is_ok()
    }

    /// Withdraws `key`'s registration for `chunk`.
    fn release_chunk(&mut self, chunk: u32, key: ModuleKey) {
        if let Some(keys) = self.chunks.get_mut(&chunk) {
            if let Some(at) = keys.iter().rposition(|&k| k == key) {
                keys.remove(at);
            }
            if keys.is_empty() {
                self.chunks.remove(&chunk);
            }
        }
    }

    /// The module `chunk`'s SWIs reach.
    pub(crate) fn chunk_owner(&self, chunk: u32) -> Option<ModuleKey> {
        self.chunks.get(&chunk)?.last().copied()
    }

    /// The Rust of the session's `rom`th built-in.
    pub(crate) fn builtin(&self, rom: usize) -> Option<&'static Builtin> {
        self.roms.get(rom).map(|rom| rom.builtin)
    }
}

/// How a name given to `OS_Module` failed to find a module.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Missing {
    /// No module has the title.
    Module,
    /// The module has no incarnation of the postfix, or none at all.
    Incarnation(ModuleKey),
}

/// "Module '…' not found".
const NOT_FOUND: crate::error::Error = crate::error::Error {
    number: 0x102,
    token: "ModuleNotFound",
};

/// "No instance of that name".
const NO_INCARNATION: crate::error::Error = crate::error::Error {
    number: 0x10C,
    token: "NoSuchInstance",
};

/// A SWI of a module's chunk past its table, as RISC OS 5's own modules
/// refuse one, the module's title its parameter.
pub(crate) const SWI_OUT_OF_RANGE: crate::error::Error = crate::error::Error {
    number: 0x110,
    token: "SwiOutOfRangeForModule",
};

impl Os<'_> {
    /// Where `builtin`'s module is — in the chain, or being initialised —
    /// and its place among the session's built-ins: its code may be a copy
    /// of its image.
    fn builtin_active(&self, builtin: &'static Builtin) -> Option<(u32, usize)> {
        let modules = &self.world.modules;
        let place = modules
            .roms
            .iter()
            .position(|rom| core::ptr::eq(rom.builtin, builtin))?;
        let code = modules
            .held()
            .find(|module| module.shape.rom == Some(place))?
            .shape
            .code;
        Some((code, place))
    }

    /// The Rust of the built-in routine whose entry is at `address`, where the
    /// entry is still the jump its image laid there: a call to it needs no
    /// trip through the processor.
    pub(crate) fn builtin_routine_at(&mut self, address: u32) -> Option<Code> {
        let jump = crate::host::jump().ok()?;
        let (place, index) = {
            let modules = &self.world.modules;
            modules.held().find_map(|module| {
                let place = module.shape.rom?;
                let rom = modules.roms.get(place)?;
                let offset = address.checked_sub(module.shape.code.wrapping_add(rom.routines))?;
                let index = (offset % 8 == 0).then_some((offset / 8) as usize)?;
                (index < rom.builtin.routines.len()).then_some((place, index))
            })?
        };
        let number = self
            .world
            .modules
            .entries
            .iter()
            .position(|&entry| entry == (place, EntryKind::Routine(index)))?;
        let routine = self
            .world
            .routine_address(crate::host::Routine::Builtin(u32::try_from(number).ok()?));
        let laid = self.peek_word(address) == Some(jump)
            && self.peek_word(address.wrapping_add(4)) == Some(routine);
        laid.then(|| self.world.modules.roms[place].builtin.routines[index])
    }

    /// The address of `builtin`'s module, while it is active.
    #[must_use]
    pub fn module_of(&self, builtin: &'static Builtin) -> Option<u32> {
        Some(self.builtin_active(builtin)?.0)
    }

    /// The address of `builtin`'s `index`th routine in its active module's
    /// code; none while it is not active.
    #[must_use]
    pub fn routine_of(&self, builtin: &'static Builtin, index: usize) -> Option<u32> {
        let (code, place) = self.builtin_active(builtin)?;
        let offset = self.world.modules.roms.get(place)?.routines;
        let index = u32::try_from(index)
            .ok()
            .filter(|&i| (i as usize) < builtin.routines.len())?;
        Some(code.wrapping_add(offset).wrapping_add(8 * index))
    }

    /// The address of `builtin`'s guest code in its active module; none
    /// while it is not active, or where it has none.
    #[must_use]
    pub fn guest_code_of(&self, builtin: &'static Builtin) -> Option<u32> {
        builtin.code?;
        let (code, place) = self.builtin_active(builtin)?;
        Some(code.wrapping_add(self.world.modules.roms.get(place)?.code))
    }

    /// The title of the module at `code`, to its first control character
    /// or space.
    pub(crate) fn title_at(&mut self, code: u32, header: &Header) -> Result<Vec<u8>, Fail> {
        let title = code.wrapping_add(header.get(Field::Title));
        Ok(self.read_until(title, header::TITLE_MOST, |b| b <= b' ')?.0)
    }

    /// `key`'s title.
    pub(crate) fn title_of(&mut self, key: ModuleKey) -> Result<Vec<u8>, Fail> {
        let (code, header) = self.code_of(key)?;
        self.title_at(code, &header)
    }

    /// `key`'s code and header.
    fn code_of(&mut self, key: ModuleKey) -> Result<(u32, Header), Fail> {
        match self.world.modules.get(key) {
            Some(module) => Ok((module.shape.code, module.shape.header)),
            None => Err(self.error(NOT_FOUND)),
        }
    }

    /// The first module of the chain whose title `title` names.
    pub(crate) fn find_module(&mut self, title: &[u8]) -> Result<Option<ModuleKey>, Fail> {
        let mut index = 0;
        while let Some(key) = self.world.modules.nth(index) {
            index += 1;
            if name::names(title, &self.title_of(key)?) {
                return Ok(Some(key));
            }
            self.yield_point()?;
        }
        Ok(None)
    }

    /// The incarnation of `key` whose postfix `postfix` names.
    fn find_incarnation(&mut self, key: ModuleKey, postfix: &[u8]) -> Result<Option<usize>, Fail> {
        let count = self
            .world
            .modules
            .get(key)
            .map_or(0, |m| m.incarnations.len());
        for index in 0..count {
            let Some(at) = self
                .world
                .modules
                .get(key)
                .and_then(|m| m.incarnations.get(index).copied())
                .map(Incarnation::postfix)
            else {
                break;
            };
            let (text, _) = self.read_until(at, header::TITLE_MOST, |b| b <= b' ')?;
            if name::names(postfix, &text) {
                return Ok(Some(index));
            }
        }
        Ok(None)
    }

    /// The module and incarnation `text` names: the postfix's, or the
    /// preferred one.
    pub(crate) fn look_up(
        &mut self,
        text: &[u8],
    ) -> Result<Result<(ModuleKey, usize), Missing>, Fail> {
        let given = name::Given::parse(text);
        let Some(key) = self.find_module(given.title)? else {
            return Ok(Err(Missing::Module));
        };
        let index = match given.postfix {
            Some([]) => return Ok(Err(Missing::Module)),
            Some(postfix) => self.find_incarnation(key, postfix)?,
            None => self
                .world
                .modules
                .get(key)
                .filter(|m| !m.incarnations.is_empty())
                .map(|_| 0),
        };
        Ok(index
            .map(|index| (key, index))
            .ok_or(Missing::Incarnation(key)))
    }

    /// The module and incarnation the name at `address` gives.
    pub(crate) fn look_up_at(
        &mut self,
        address: u32,
    ) -> Result<Result<(ModuleKey, usize), Missing>, Fail> {
        let (text, _) = self.read_until(address, header::TITLE_MOST, |b| b <= b' ')?;
        self.look_up(&text)
    }

    /// The module and incarnation the name at `address` gives, or the error
    /// a failed look-up gives.
    pub(crate) fn named(&mut self, address: u32) -> Result<(ModuleKey, usize), Fail> {
        match self.look_up_at(address)? {
            Ok(found) => Ok(found),
            Err(missing) => Err(self.missing(missing, address)),
        }
    }

    /// The error a failed look-up of the name at `address` gives: "Module …
    /// not found" naming the rest of the string.
    pub(crate) fn missing(&mut self, missing: Missing, address: u32) -> Fail {
        match missing {
            Missing::Module => match self.read_until(address, crate::error::TEXT, |b| b < b' ') {
                Ok((text, _)) => self.error_with(NOT_FOUND, &[&text]),
                Err(fail) => fail,
            },
            Missing::Incarnation(_) => self.error(NO_INCARNATION),
        }
    }

    /// Where the parameters after the name at `address` begin: past it and
    /// the spaces that follow.
    pub(crate) fn past_name(&mut self, address: u32) -> Result<u32, Fail> {
        let mut at = address;
        while self.read_byte(at)? > b' ' {
            at = at.wrapping_add(1);
            self.yield_point()?;
        }
        Ok(self.skip_spaces(at)?.1)
    }
}

#[cfg(test)]
mod code_tests;
#[cfg(test)]
pub(crate) mod tests;
