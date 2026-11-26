//! Built-in modules' images: each a module laid out RISC OS's way in the
//! host pages — header, entries, service table, command table, SWI decoding
//! table, title and help string — so the guest finds a built-in as it finds
//! any module. The images make one ROM chain: each after its size word, a
//! zero word after the last.
//!
//! An entry is `LDR PC, [PC, #-4]` and the address of the host routine that
//! enters the built-in's Rust, so a copy of an image, as `*RMFaster` makes
//! one, enters the same Rust.

use alloc::vec::Vec;

use super::header::{FLAG_32BIT, WORDS};
use super::inventory::Rom;
use super::{version, Builtin, Command, CommandKind, Help};
use crate::cli::table::{CONFIGURE, FILING, HELP_IS_CODE};
use crate::memory::MemoryError;
use crate::os::{len32, Fail, Os};

/// What a built-in entry's host routine enters.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum EntryKind {
    Start,
    Init,
    Final,
    Service,
    Swi,
    Command(usize),
    Help(usize),
    Routine(usize),
}

/// A built-in entry the guest branches to: its built-in's place, what it
/// enters, and where it lies in the built-in's image.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct BuiltinEntry {
    pub(crate) place: usize,
    pub(crate) kind: EntryKind,
    pub(crate) offset: u32,
}

/// The kernel's empty environment string, after the chain: a carriage
/// return.
const EMPTY: u32 = 0x0D;

/// The ROM chain the session's built-ins make.
pub(crate) struct Chain {
    /// Its words, ready for the host pages.
    pub(crate) words: Vec<u32>,
    /// Each image's offset in the chain, its size word and where its
    /// routines and code are in it.
    pub(crate) images: Vec<(u32, u32, Placed)>,
    /// Where the empty environment string is.
    pub(crate) empty: u32,
}

/// A module image's bytes as they are laid down.
struct Image {
    bytes: Vec<u8>,
}

impl Image {
    fn at(&self) -> u32 {
        len32(self.bytes.len())
    }

    fn word(&mut self, word: u32) -> Result<(), MemoryError> {
        self.bytes.try_reserve(4)?;
        self.bytes.extend_from_slice(&word.to_le_bytes());
        Ok(())
    }

    fn text(&mut self, text: &[u8]) -> Result<u32, MemoryError> {
        let at = self.at();
        self.bytes.try_reserve(text.len() + 1)?;
        self.bytes.extend_from_slice(text);
        self.bytes.push(0);
        Ok(at)
    }

    fn align(&mut self) -> Result<(), MemoryError> {
        let pad = self.bytes.len().next_multiple_of(4) - self.bytes.len();
        self.bytes.try_reserve(pad)?;
        self.bytes.resize(self.bytes.len() + pad, 0);
        Ok(())
    }

    fn patch(&mut self, at: u32, word: u32) {
        let at = at as usize;
        if let Some(slot) = self.bytes.get_mut(at..at + 4) {
            slot.copy_from_slice(&word.to_le_bytes());
        }
    }

    /// An entry jumping to `routine`; its offset.
    fn entry(&mut self, routine: u32) -> Result<u32, MemoryError> {
        let at = self.at();
        self.word(crate::host::jump()?)?;
        self.word(routine)?;
        Ok(at)
    }
}

/// `builtin`'s help string: its help name, tabs to the sixteenth column,
/// then the version of `rom`, its row, the release's date, which every
/// built-in shares, and its suffix.
fn help_string(builtin: &Builtin, rom: &Rom) -> Result<Vec<u8>, MemoryError> {
    let name = builtin.help_name.unwrap_or(builtin.title);
    let suffix = builtin.help_suffix.unwrap_or("");
    let mut help = Vec::new();
    help.try_reserve(name.len() + suffix.len() + 24)?;
    help.extend_from_slice(name.as_bytes());
    help.extend_from_slice(if name.len() < 8 { b"\t\t" } else { b"\t" });
    help.extend_from_slice(&version::text(rom.bcd()));
    help.extend_from_slice(b" (");
    help.extend_from_slice(&crate::release::date_text());
    help.push(b')');
    help.extend_from_slice(suffix.as_bytes());
    Ok(help)
}

/// Where an image's own routines' entries begin, one after another, and
/// where its guest code begins, as offsets in it.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Placed {
    pub(crate) routines: u32,
    pub(crate) code: u32,
    /// The number of its first routine's entry, the rest following in order.
    pub(crate) first_routine: usize,
}

/// `builtin`'s image, row `rom`, whose `n`th entry is the routine
/// `routine(n)`, and where its routines' entries and its code are; each entry
/// it has is added to `entries`, with `place`, its place among the session's
/// built-ins.
fn image(
    builtin: &Builtin,
    rom: &Rom,
    place: usize,
    entries: &mut Vec<BuiltinEntry>,
    routine: &impl Fn(u32) -> u32,
) -> Result<(Vec<u8>, Placed), MemoryError> {
    let mut image = Image { bytes: Vec::new() };
    let mut header = [0; WORDS as usize];
    for _ in 0..WORDS {
        image.word(0)?;
    }
    let mut entry = |image: &mut Image, kind| -> Result<u32, MemoryError> {
        entries.try_reserve(1)?;
        let offset = image.entry(routine(len32(entries.len())))?;
        entries.push(BuiltinEntry {
            place,
            kind,
            offset,
        });
        Ok(offset)
    };
    if builtin.start.is_some() {
        header[0] = entry(&mut image, EntryKind::Start)?;
    }
    if builtin.init.is_some() {
        header[1] = entry(&mut image, EntryKind::Init)?;
    }
    if builtin.finalise.is_some() {
        header[2] = entry(&mut image, EntryKind::Final)?;
    }
    if builtin.services.is_some() {
        header[3] = entry(&mut image, EntryKind::Service)?;
    }
    if let Some(swis) = &builtin.swis {
        header[7] = swis.chunk;
        header[8] = entry(&mut image, EntryKind::Swi)?;
    }
    let mut commands = Vec::new();
    commands.try_reserve(builtin.commands.len())?;
    for (index, command) in builtin.commands.iter().enumerate() {
        let run = match command.run {
            Some(_) => entry(&mut image, EntryKind::Command(index))?,
            None => 0,
        };
        let help = match command.help {
            Help::Code(_) => entry(&mut image, EntryKind::Help(index))?,
            Help::Text(_) => 0,
        };
        commands.push((run, help));
    }
    let routines = image.at();
    for index in 0..builtin.routines.len() {
        entry(&mut image, EntryKind::Routine(index))?;
    }
    let first_routine = entries.len() - builtin.routines.len();
    let code = image.at();
    if let Some(emit) = builtin.code {
        let first = i32::try_from(routines)
            .ok()
            .zip(i32::try_from(code).ok())
            .and_then(|(routines, code)| routines.checked_sub(code))
            .ok_or(MemoryError::HostFull)?;
        for word in emit(first)? {
            image.word(word)?;
        }
    }
    header[12] = image.at();
    image.word(FLAG_32BIT)?;
    header[6] = image.at();
    let table = command_table(&mut image, builtin.commands, &commands)?;
    if let Some(swis) = &builtin.swis {
        header[9] = image.text(swis.prefix.as_bytes())?;
        for name in swis.names {
            image.text(name.as_bytes())?;
        }
        image.text(b"")?;
    }
    header[4] = image.text(builtin.title.as_bytes())?;
    header[5] = image.text(&help_string(builtin, rom)?)?;
    command_texts(&mut image, builtin.commands, &table, &commands)?;
    image.align()?;
    for (n, word) in (0..).zip(header) {
        image.patch(4 * n, word);
    }
    Ok((
        image.bytes,
        Placed {
            routines,
            code,
            first_routine,
        },
    ))
}

/// Each command's syntax and help text laid in `image`, and the command
/// table's slots `table` gives filled in with them: help that is code is its
/// entry, from `entries`.
fn command_texts(
    image: &mut Image,
    commands: &[Command],
    table: &[u32],
    entries: &[(u32, u32)],
) -> Result<(), MemoryError> {
    for ((command, &slots), &(_, code)) in commands.iter().zip(table).zip(entries) {
        let help = match command.help {
            Help::Code(_) => code,
            Help::Text(text) => {
                let at = image.at();
                image.bytes.try_reserve(text.len() + 1)?;
                image.bytes.extend_from_slice(text.as_bytes());
                match command.syntax {
                    // Help text runs on into its syntax, a return between them.
                    Some(_) => image.bytes.push(b'\r'),
                    None => image.text(b"").map(|_| ())?,
                }
                at
            }
        };
        if let Some(syntax) = command.syntax {
            let at = image.text(syntax.as_bytes())?;
            image.patch(slots, at);
        }
        image.patch(slots + 4, help);
    }
    Ok(())
}

/// The command table: each command's name, then its entry's offset, its
/// information word and its syntax and help offsets; a zero after the last.
/// Where each command's syntax and help offsets are, to be filled in.
fn command_table(
    image: &mut Image,
    commands: &[Command],
    entries: &[(u32, u32)],
) -> Result<Vec<u32>, MemoryError> {
    let mut slots = Vec::new();
    slots.try_reserve(commands.len())?;
    for (command, &(entry, _)) in commands.iter().zip(entries) {
        image.text(command.name.as_bytes())?;
        image.align()?;
        image.word(entry)?;
        let code = if matches!(command.help, Help::Code(_)) {
            HELP_IS_CODE
        } else {
            0
        };
        let kind = match command.kind {
            CommandKind::Command => 0,
            CommandKind::Filing => FILING,
            CommandKind::Configure => CONFIGURE,
        };
        image.word(
            u32::from(command.min)
                | (u32::from(command.translate) << 8)
                | (u32::from(command.max) << 16)
                | code
                | kind,
        )?;
        slots.push(image.at());
        image.word(0)?;
        image.word(0)?;
    }
    image.word(0)?;
    Ok(slots)
}

/// The ROM chain of `builtins`, each with its row, whose entries' routines
/// are at `routine(n)`; their entries are added to `entries`.
pub(crate) fn chain(
    builtins: &[(&Rom, &'static Builtin)],
    entries: &mut Vec<BuiltinEntry>,
    routine: impl Fn(u32) -> u32,
) -> Result<Chain, MemoryError> {
    let mut words = Vec::new();
    let mut images = Vec::new();
    images.try_reserve(builtins.len())?;
    for (place, &(rom, builtin)) in builtins.iter().enumerate() {
        let (bytes, placed) = image(builtin, rom, place, entries, &routine)?;
        let size = len32(bytes.len()) + 4;
        words.try_reserve(bytes.len() / 4 + 1)?;
        words.push(size);
        images.push((4 * len32(words.len()), size, placed));
        words.extend(
            bytes
                .as_chunks::<4>()
                .0
                .iter()
                .map(|word| u32::from_le_bytes(*word)),
        );
    }
    words.try_reserve(2)?;
    words.push(0);
    let empty = 4 * len32(words.len());
    words.push(EMPTY);
    Ok(Chain {
        words,
        images,
        empty,
    })
}

impl Os<'_> {
    /// A built-in entry's host routine, entered by guest code as the entry
    /// it stands for: its Rust performed with the caller's registers, which
    /// it answers in, V set with R0 the error if it fails.
    pub(crate) fn builtin_entered(&mut self, entry: u32) -> Result<(), crate::os::Unwind> {
        let modules = &self.world.modules;
        let found = modules.entries.get(entry as usize).and_then(|&entry| {
            let at = modules
                .running(entry.place)
                .map(|module| module.shape.code.wrapping_add(entry.offset));
            Some((modules.builtin(entry.place)?, entry.kind, at))
        });
        let Some((builtin, kind, at)) = found else {
            self.undefined_here();
            return Ok(());
        };
        // An abort the entry takes is reported at the entry its caller
        // branched to, in the module, rather than at its host routine.
        if let Some(at) = at {
            self.world.swi_at = at;
        }
        self.as_subroutine(|os, regs| match kind {
            EntryKind::Start => builtin.start.map_or(Ok(()), |code| code(os, regs)),
            EntryKind::Init => builtin.init.map_or(Ok(()), |code| code(os, regs)),
            EntryKind::Final => builtin.finalise.map_or(Ok(()), |code| code(os, regs)),
            EntryKind::Service => match &builtin.services {
                Some(services) => (services.handler)(os, regs).map_err(Fail::Unwind),
                None => Ok(()),
            },
            EntryKind::Swi => match &builtin.swis {
                Some(swis) => (swis.handler)(os, regs.r[11] & (crate::swi::CHUNK - 1), regs),
                None => Ok(()),
            },
            EntryKind::Command(index) => match builtin.commands.get(index).and_then(|c| c.run) {
                Some(run) => run(os, regs),
                None => Ok(()),
            },
            EntryKind::Help(index) => match builtin.commands.get(index).map(|c| &c.help) {
                Some(&Help::Code(code)) => code(os, regs),
                _ => Ok(()),
            },
            EntryKind::Routine(index) => builtin
                .routines
                .get(index)
                .map_or(Ok(()), |code| code(os, regs)),
        })
    }
}
