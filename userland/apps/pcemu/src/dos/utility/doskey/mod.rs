//! `DOSKEY [/REINSTALL] [/BUFSIZE=size] [/MACROS] [/HISTORY]
//! [/INSERT | /OVERSTRIKE] [name=[text]]`: the commands typed kept and
//! recalled, lines edited, and macros, as MS-DOS 5's `DOSKEY` gives them.
//! The first run stays resident; later runs change the resident copy's
//! macros and mode and list what it keeps.
//!
//! The resident copy takes function `0Ah` of INT 21h when the standard
//! input is the console, and INT 2Fh's functions `48h`: `4800h`, the
//! installation check, and `4810h`, a command line read with its macros
//! expanded, which `COMMAND.COM` asks for. Its hooks pass every other call
//! straight on in the guest's own code. A Ctrl+C typed is put back for
//! DOS's own line input to meet, so it breaks as it would with no
//! `DOSKEY`.

mod edit;
mod hooks;
pub(crate) mod store;

pub use edit::{DosKeyEdit, DosKeyWrite};
pub use hooks::{DosKey21, DosKey2F};

use super::{refuse, switch_parts, value_in, Refusal};
use crate::asm::{Asm, AsmError, Cond, Reg8};
use crate::dos::memory::psp;
use crate::dos::program::{
    self, handler_entry, say, Flow, Frame, Id, Int, Program, Returned, Routine,
};
use crate::dos::shell::parse;
use crate::frames::{locals, take, Far, Outcome};
use alloc::vec::Vec;
use store::{NoRoom, Store};

/// Where `DOSKEY`'s state lies in its memory; its buffer follows it,
/// [`SIZE`](at::SIZE) bytes long.
pub mod at {
    use crate::dos::program::at::STATE;

    /// [`super::flag`].
    pub const FLAGS: u16 = STATE;
    /// The handlers before this copy's.
    pub const OLD_21: u16 = STATE + 0x02;
    pub const OLD_2F: u16 = STATE + 0x06;
    /// The buffer's size, and the bytes of it the macros and the commands
    /// take.
    pub const SIZE: u16 = STATE + 0x0A;
    pub const MACROS: u16 = STATE + 0x0C;
    pub const HISTORY: u16 = STATE + 0x0E;
    /// The bytes of the commands a macro's `$T` left to run, and the
    /// template's.
    pub const QUEUED_LEN: u16 = STATE + 0x10;
    pub const TEMPLATE_LEN: u16 = STATE + 0x12;
    /// The run's own: the copy it changes, what it lists, and where the
    /// listing or the closing of the standard handles has reached.
    pub const TARGET: u16 = STATE + 0x14;
    pub const ASKED: u16 = STATE + 0x16;
    pub const STEP: u16 = STATE + 0x18;
    /// The code passing INT 21h's function `0Ah`, and INT 2Fh's functions
    /// `48h`, to the handlers, and every other call to the handler before.
    pub const FILTER_21: u16 = STATE + 0x20;
    pub const FILTER_2F: u16 = STATE + 0x30;
    pub const FILTER_LEN: u16 = 0x10;
    /// The line being edited, and the template.
    pub const LINE: u16 = STATE + 0x40;
    pub const LINE_ROOM: u16 = 0x80;
    pub const TEMPLATE: u16 = LINE + LINE_ROOM;
    /// Text written to the screen.
    pub const TEXT: u16 = TEMPLATE + LINE_ROOM;
    pub const TEXT_ROOM: u16 = 0x100;
    /// The commands a macro's `$T` left to run, NUL-ended in turn.
    pub const QUEUED: u16 = TEXT + TEXT_ROOM;
    pub const QUEUED_ROOM: u16 = 0x200;
    /// The buffer.
    pub const BUFFER: u16 = QUEUED + QUEUED_ROOM;
    /// Bytes of `DOSKEY`'s state before its buffer.
    pub const LEN: u16 = BUFFER - STATE;

    const _: () = assert!(FILTER_21 + FILTER_LEN <= FILTER_2F && FILTER_2F + FILTER_LEN <= LINE);
}

/// What the state's flags record.
pub mod flag {
    /// New text is put in, not over what is there.
    pub const INSERT: u8 = 0x01;
}

/// The handlers `DOSKEY` installs.
pub mod handler {
    pub const EDIT: u16 = 0;
    pub const MULTIPLEX: u16 = 1;
}

/// INT 2Fh's `DOSKEY` functions.
pub mod multiplex {
    pub const AH: u8 = 0x48;
    pub const CHECK: u16 = 0x4800;
    pub const READ: u8 = 0x10;
    /// What the installation check answers in AL.
    pub const INSTALLED: u8 = 0xFF;
}

/// What `DOSKEY` says.
pub mod text {
    pub use crate::dos::message::{MEMORY, NEW_LINE};
    pub const INSTALLED: &[u8] = b"DOSKey installed.\r\n";
    pub const NO_ROOM: &[u8] =
        b"Insufficient memory to store macro.  Use the DOSKEY command with the /BUFSIZE switch to increase available memory.\r\n";
    pub const FIXED_SIZE: &[u8] = b"Cannot change BUFSIZE\r\n";
    pub const BAD_MACRO: &[u8] = b"Invalid macro definition\r\n";
}

/// The buffer's size when `/BUFSIZE` gives none, and the sizes it takes.
pub const DEFAULT_SIZE: u16 = 512;
const SIZES: core::ops::RangeInclusive<u16> = 256..=0x8000;

/// What `DOSKEY`'s command line asks for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Request<'a> {
    pub size: Option<u16>,
    pub reinstall: bool,
    pub macros: bool,
    pub history: bool,
    /// `/INSERT` or `/OVERSTRIKE`.
    pub insert: Option<bool>,
    /// A macro's name and its text, none to delete it.
    pub definition: Option<(&'a [u8], &'a [u8])>,
}

/// Why a command line is refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refused<'a> {
    Parse(Refusal<'a>),
    /// What follows the switches names no macro.
    Macro,
}

/// `DOSKEY`'s command line: its switches, then a macro's definition, which
/// runs to the line's end, switches and blanks in it its own.
///
/// # Errors
///
/// What it says of a line it cannot take.
pub fn request(tail: &[u8]) -> Result<Request<'_>, Refused<'_>> {
    let mut request = Request::default();
    let mut rest = tail;
    loop {
        let start = rest
            .iter()
            .position(|&byte| !matches!(byte, b' ' | b'\t'))
            .unwrap_or(rest.len());
        rest = &rest[start..];
        let Some(switch) = rest.strip_prefix(b"/") else {
            break;
        };
        let end = switch
            .iter()
            .position(|&byte| matches!(byte, b' ' | b'\t' | b'/'))
            .unwrap_or(switch.len());
        let word = &rest[..=end];
        let (name, value) = match switch[..end].iter().position(|&byte| byte == b'=') {
            Some(at) => (&switch[..at], Some(&switch[at + 1..end])),
            None => switch_parts(&switch[..end]),
        };
        let name = name.to_ascii_uppercase();
        match (name.as_slice(), value) {
            (b"BUFSIZE", Some(size)) => {
                request.size = Some(value_in(size, SIZES, word).map_err(Refused::Parse)?);
            }
            (b"REINSTALL", None) => request.reinstall = true,
            (b"M" | b"MACROS", None) => request.macros = true,
            (b"H" | b"HISTORY", None) => request.history = true,
            (b"INSERT", None) => request.insert = Some(true),
            (b"OVERSTRIKE", None) => request.insert = Some(false),
            _ => return Err(Refused::Parse(Refusal::Switch(word))),
        }
        rest = &rest[word.len()..];
    }
    if !rest.is_empty() {
        let at = rest
            .iter()
            .position(|&byte| byte == b'=')
            .ok_or(Refused::Macro)?;
        let name = parse::trim(&rest[..at]);
        let text = &rest[at + 1..];
        let text = &text[text
            .iter()
            .position(|&byte| !matches!(byte, b' ' | b'\t'))
            .unwrap_or(text.len())..];
        if name.is_empty() || name.iter().any(|&byte| matches!(byte, b' ' | b'\t')) {
            return Err(Refused::Macro);
        }
        request.definition = Some((name, text));
    }
    Ok(request)
}

/// The store kept in the memory of the copy at `segment`.
pub(crate) fn load(p: &mut Program<'_, '_>, segment: u16) -> Store {
    let size = p.g.read16(segment, at::SIZE);
    let used =
        p.g.read16(segment, at::MACROS)
            .saturating_add(p.g.read16(segment, at::HISTORY))
            .min(size);
    let mut bytes = alloc::vec![0u8; usize::from(used)];
    p.g.read_bytes(segment, at::BUFFER, &mut bytes);
    Store::read(
        usize::from(size),
        &bytes,
        usize::from(p.g.read16(segment, at::MACROS)),
    )
}

/// `store` kept in the memory of the copy at `segment`.
pub(crate) fn keep(p: &mut Program<'_, '_>, segment: u16, store: &Store) {
    let (bytes, macros) = store.bytes();
    p.g.write_bytes(segment, at::BUFFER, &bytes);
    let len = |n: usize| u16::try_from(n).unwrap_or(0);
    p.g.write16(segment, at::MACROS, len(macros));
    p.g.write16(segment, at::HISTORY, len(bytes.len() - macros));
}

/// The resident copy the installation check answered with: its segment,
/// once its memory holds a `DOSKEY` of its own.
fn resident(p: &mut Program<'_, '_>, child: Outcome) -> Option<u16> {
    let answered = child.is_ok_and(|ax| ax.to_le_bytes()[0] == multiplex::INSTALLED);
    let segment = p.result(Returned::Es);
    let ours = answered
        && segment != p.own
        && u16::from(p.g.read8(segment, program::at::NUMBER)) == super::number::DOSKEY
        && p.g
            .read16(segment.wrapping_sub(1), crate::dos::memory::mcb::OWNER)
            == segment;
    ours.then_some(segment)
}

/// The filter at `at`: calls of `ah` to handler `n`, every other to the
/// handler whose address lies at `old`.
fn filter(at: u16, ah: u8, n: u16, old: u16) -> Result<Vec<u8>, AsmError> {
    let mut asm = Asm::new(at);
    let chain = asm.label();
    asm.cmp_imm8(Reg8::Ah, ah);
    asm.jcc(Cond::Ne, &chain);
    asm.jmp_to(handler_entry(n));
    asm.bind(&chain);
    asm.jmp_far_through(old);
    asm.finish()
}

/// This copy made the resident one: its filters laid, INT 21h and INT 2Fh
/// led to them and the handlers before kept to pass calls on to; written
/// straight into the vector table with no call before the end, so a
/// Ctrl+C can never end `DOSKEY` with a vector leading into memory it gave
/// back.
fn hook(p: &mut Program<'_, '_>) -> bool {
    let filters = [
        (0x21u16, at::FILTER_21, 0x0A, handler::EDIT, at::OLD_21),
        (
            0x2F,
            at::FILTER_2F,
            multiplex::AH,
            handler::MULTIPLEX,
            at::OLD_2F,
        ),
    ];
    let mut codes = Vec::with_capacity(filters.len());
    for &(_, at, ah, n, old) in &filters {
        match filter(at, ah, n, old) {
            Ok(code) if code.len() <= usize::from(at::FILTER_LEN) => codes.push(code),
            _ => return false,
        }
    }
    for (&(vector, at, _, _, old), code) in filters.iter().zip(codes) {
        p.g.write_bytes(p.own, at, &code);
        let entry = vector * 4;
        let before = Far::new(p.g.read16(0, entry + 2), p.g.read16(0, entry));
        p.set_far(old, before);
        p.g.write16(0, entry, at);
        p.g.write16(0, entry + 2, p.own);
    }
    true
}

locals! {
    /// `DOSKEY`'s main routine.
    pub struct DosKey {}
}

mod phase {
    pub const SAID: u8 = 1;
    pub const CHECKED: u8 = 2;
    pub const APPLY: u8 = 3;
    pub const LISTED: u8 = 4;
    pub const CLOSING: u8 = 5;
}

/// What a run lists: the macros, the commands.
mod asked {
    pub const MACROS: u8 = 0x01;
    pub const HISTORY: u8 = 0x02;
}

impl DosKey {
    /// The command line, which the first step found a request in.
    fn request(tail: &[u8]) -> Request<'_> {
        request(tail).unwrap_or_default()
    }

    /// The mode and the macro the request names set in the copy the run
    /// changes, then its listings.
    fn apply(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let tail = p.tail();
        let request = Self::request(&tail);
        let target = p.get16(at::TARGET);
        if let Some(insert) = request.insert {
            let flags = p.g.read8(target, at::FLAGS) & !flag::INSERT;
            p.g.write8(
                target,
                at::FLAGS,
                flags | if insert { flag::INSERT } else { 0 },
            );
        }
        p.set8(
            at::ASKED,
            (u8::from(request.macros) * asked::MACROS)
                | (u8::from(request.history) * asked::HISTORY),
        );
        p.set16(at::STEP, 0);
        if let Some((name, macro_text)) = request.definition {
            let mut store = load(p, target);
            let defined = store.define(name, macro_text);
            keep(p, target, &store);
            if defined == Err(NoRoom) {
                *phase = phase::LISTED;
                return Flow::Call(say(p, text::NO_ROOM));
            }
        }
        Self::list(p, phase)
    }

    /// The next entry a listing asks for said — the macros, then the
    /// commands — or, with none left, the end: this copy staying resident
    /// once its handles are closed, or the run done.
    fn list(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let target = p.get16(at::TARGET);
        let store = load(p, target);
        let wanted = p.get8(at::ASKED);
        let macros = store
            .macros()
            .iter()
            .filter(|_| wanted & asked::MACROS != 0)
            .map(|each| [&each.name[..], b"=", &each.text].concat());
        let history = store
            .history()
            .iter()
            .filter(|_| wanted & asked::HISTORY != 0)
            .cloned();
        let step = p.get16(at::STEP);
        if let Some(line) = macros.chain(history).nth(usize::from(step)) {
            p.set16(at::STEP, step + 1);
            *phase = phase::LISTED;
            return Flow::Call(say(p, &[&line[..], text::NEW_LINE].concat()));
        }
        if target != p.own {
            return Flow::Return(Ok(0));
        }
        p.set16(at::STEP, 0);
        Self::close(p, phase)
    }

    /// The standard handles closed, then this copy hooked in as it stays.
    fn close(p: &mut Program<'_, '_>, phase: &mut u8) -> Flow {
        let handle = p.get16(at::STEP);
        if handle < 5 {
            p.set16(at::STEP, handle + 1);
            *phase = phase::CLOSING;
            return Flow::Call(Frame::of(&Int {
                bx: handle,
                ..Int::dos(p.own, 0x3E00)
            }));
        }
        if !hook(p) {
            return Flow::Return(Ok(super::FAILED));
        }
        let size = p.get16(at::SIZE);
        let paragraphs = p.paragraphs().saturating_add(size.div_ceil(16));
        p.stay_resident(0, paragraphs)
    }

    /// This copy readied to stay: its buffer empty, its mode the one asked
    /// for; none where its memory cannot hold the buffer.
    fn ready(p: &mut Program<'_, '_>, phase: &mut u8, request: &Request<'_>) -> Flow {
        let size = request.size.unwrap_or(DEFAULT_SIZE);
        let needed = u32::from(p.paragraphs()) + u32::from(size.div_ceil(16));
        if u32::from(p.get16(psp::TOP).wrapping_sub(p.own)) < needed {
            *phase = phase::SAID;
            return Flow::Call(say(p, text::MEMORY));
        }
        p.set16(at::TARGET, p.own);
        p.set8(at::FLAGS, 0);
        for field in [at::MACROS, at::HISTORY, at::QUEUED_LEN, at::TEMPLATE_LEN] {
            p.set16(field, 0);
        }
        p.set16(at::SIZE, size);
        *phase = phase::APPLY;
        Flow::Call(say(p, text::INSTALLED))
    }
}

impl Routine for DosKey {
    const ID: Id = Id::DosKey;

    fn step(&mut self, p: &mut Program<'_, '_>, phase: &mut u8, child: Outcome) -> Flow {
        match *phase {
            phase::SAID => {
                take!(child);
                Flow::Return(Ok(super::FAILED))
            }
            phase::CHECKED => {
                let tail = p.tail();
                let request = Self::request(&tail);
                let Some(target) = resident(p, child).filter(|_| !request.reinstall) else {
                    return Self::ready(p, phase, &request);
                };
                p.set16(at::TARGET, target);
                if request
                    .size
                    .is_some_and(|size| size != p.g.read16(target, at::SIZE))
                {
                    *phase = phase::APPLY;
                    return Flow::Call(say(p, text::FIXED_SIZE));
                }
                Self::apply(p, phase)
            }
            phase::APPLY => {
                take!(child);
                Self::apply(p, phase)
            }
            phase::LISTED => {
                take!(child);
                Self::list(p, phase)
            }
            phase::CLOSING => {
                take!(child);
                Self::close(p, phase)
            }
            _ => {
                let tail = p.tail();
                if let Err(refused) = request(&tail) {
                    *phase = phase::SAID;
                    return Flow::Call(match refused {
                        Refused::Parse(refusal) => refuse(p, b"", refusal),
                        Refused::Macro => say(p, text::BAD_MACRO),
                    });
                }
                *phase = phase::CHECKED;
                Flow::Call(Frame::of(&Int::bios(p.own, 0x2F, multiplex::CHECK)))
            }
        }
    }
}

#[cfg(test)]
#[path = "doskey_tests.rs"]
mod tests;
