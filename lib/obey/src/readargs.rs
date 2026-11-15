//! `OS_ReadArgs`: a command line's arguments read against a keyword template,
//! and laid out as the call leaves them in its buffer.
//!
//! A template is entries separated by commas, each one or more names joined by
//! `=` and qualified by `/A` (required), `/K` (only by keyword), `/S` (a
//! switch), `/E` (an expression's value) or `/G` (GSTrans'd). An argument is
//! given positionally, or after `-name`; a switch's name may be abbreviated to
//! its first letter, and abbreviated switches run together, `-abc`.

use alloc::vec::Vec;

use tairix_util::fallible;

use crate::expr::{evaluate, Answer};
use crate::gstrans::{translated, Flags};
use crate::vars::Variables;
use crate::{byte, Budget, Error};

/// The word a switch that is present leaves in its slot.
const SWITCH: u32 = 0x7FFF_FFFF;

/// An argument as it was read.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Arg {
    /// Not given.
    Absent,
    /// A switch, present.
    Switch,
    /// Text, its quotes removed.
    Text(Vec<u8>),
    /// A `/G` argument, GSTrans'd.
    Translated(Vec<u8>),
    /// A `/E` argument's value.
    Evaluated(Answer),
}

/// What `OS_ReadArgs` read.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ReadArgs {
    /// Each template entry's argument, in template order.
    pub args: Vec<Arg>,
    /// The buffer as the call leaves it: a word per entry, an argument's the
    /// address of its value, then the values in the order they were given.
    pub buffer: Vec<u8>,
    /// The bytes of the buffer left unused.
    pub left: usize,
}

/// A template entry's qualifiers, a bit each.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Qualifiers(u8);

impl Qualifiers {
    /// `/A`: the argument must be given.
    const REQUIRED: u8 = 1;
    /// `/K`: only after its keyword.
    const KEYWORD: u8 = 1 << 1;
    /// `/S`: a switch, which takes no value.
    const SWITCH: u8 = 1 << 2;
    /// `/E`: an expression, evaluated; it outranks `/G`.
    const EVALUATE: u8 = 1 << 3;
    /// `/G`: GSTrans'd.
    const GSTRANS: u8 = 1 << 4;

    const fn has(self, qualifier: u8) -> bool {
        self.0 & qualifier != 0
    }

    /// The qualifier `/` and `letter` give, in either case.
    const fn of(letter: u8) -> u8 {
        match letter.to_ascii_uppercase() {
            b'A' => Self::REQUIRED,
            b'K' => Self::KEYWORD,
            b'S' => Self::SWITCH,
            b'E' => Self::EVALUATE,
            b'G' => Self::GSTRANS,
            _ => 0,
        }
    }
}

/// A template read: each entry's qualifiers, and every name with its entry
/// in template order.
struct Template<'t> {
    entries: Vec<Qualifiers>,
    names: Vec<(usize, &'t [u8])>,
}

const fn is_keyword_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

fn template(text: &[u8]) -> Result<Template<'_>, Error> {
    let end = text.iter().position(|&b| b < b' ').unwrap_or(text.len());
    let mut entries = Vec::new();
    let mut names = Vec::new();
    for (index, entry) in text[..end].split(|&b| b == b',').enumerate() {
        let mut qualifiers = Qualifiers::default();
        let mut at = 0;
        // A qualifier is the one byte after a `/`, so `a//s` has none.
        while let Some(slash) = entry[at..].iter().position(|&b| b == b'/') {
            at += slash + 1;
            qualifiers.0 |= Qualifiers::of(byte(entry, at));
            at = (at + 1).min(entry.len());
        }
        for alias in entry.split(|&b| b == b'=') {
            let start = alias.iter().take_while(|&&b| b == b' ').count();
            let len = alias[start..]
                .iter()
                .take_while(|&&b| is_keyword_byte(b))
                .count();
            if !fallible::push(&mut names, (index, &alias[start..start + len])) {
                return Err(Error::SystemHeapFull);
            }
        }
        if !fallible::push(&mut entries, qualifiers) {
            return Err(Error::SystemHeapFull);
        }
    }
    Ok(Template { entries, names })
}

/// The bytes after a `-` at `at` that name an entry: a whole name not
/// followed by a keyword character, else a name whose first letter alone they
/// begin with. The entry, where the name ends in the text, and whether it
/// was whole.
fn keyword(t: &Template<'_>, args: &[u8], at: usize) -> Option<(usize, usize, bool)> {
    let fold = |b: u8| b.to_ascii_uppercase();
    let whole = t.names.iter().find(|(_, name)| {
        args.get(at..at + name.len())
            .is_some_and(|text| text.eq_ignore_ascii_case(name))
            && !is_keyword_byte(byte(args, at + name.len()))
    });
    if let Some(&(entry, name)) = whole {
        return Some((entry, at + name.len(), true));
    }
    t.names
        .iter()
        .find(|(_, name)| {
            name.len() > 1
                && fold(name[0]) == fold(byte(args, at))
                && fold(name[1]) != fold(byte(args, at + 1))
        })
        .map(|&(entry, _)| (entry, at + 1, false))
}

/// The buffer as it fills.
struct Layout {
    base: u32,
    capacity: usize,
    slots: Vec<u32>,
    data: Vec<u8>,
    /// The data's bytes counted as used; a `/G` value's terminator is not.
    used: usize,
}

impl Layout {
    fn free(&self) -> usize {
        self.capacity - 4 * self.slots.len() - self.used
    }

    /// The address the next value is laid at.
    fn next(&self) -> u32 {
        let offset = 4 * self.slots.len() + self.used;
        self.base
            .wrapping_add(u32::try_from(offset).unwrap_or(u32::MAX))
    }

    /// `bytes` laid at the next place, `counted` of them used.
    fn put(&mut self, bytes: &[u8], counted: usize) -> Result<(), Error> {
        self.data.truncate(self.used);
        self.data
            .try_reserve(bytes.len())
            .map_err(|_| Error::SystemHeapFull)?;
        self.data.extend_from_slice(bytes);
        self.used += counted;
        Ok(())
    }
}

/// `OS_ReadArgs`: `args`, to its first control character, read against
/// `template`, laid out in a buffer of `capacity` bytes at address `base`.
pub fn read_args(
    template_text: &[u8],
    args: &[u8],
    vars: &mut dyn Variables,
    budget: &mut Budget,
    base: u32,
    capacity: usize,
) -> Result<ReadArgs, Error> {
    let t = template(template_text)?;
    let count = t.entries.len();
    if count.checked_mul(4).is_none_or(|bytes| bytes > capacity) {
        return Err(Error::BufferOverflow);
    }
    let mut layout = Layout {
        base,
        capacity,
        slots: Vec::new(),
        data: Vec::new(),
        used: 0,
    };
    if !fallible::reserve(&mut layout.slots, count) {
        return Err(Error::SystemHeapFull);
    }
    layout.slots.resize(count, 0);
    let mut read: Vec<Option<Arg>> = Vec::new();
    if !fallible::reserve(&mut read, count) {
        return Err(Error::SystemHeapFull);
    }
    read.resize_with(count, || None);
    let mut at = 0;
    // A keyword given, waiting for its value.
    let mut pending: Option<usize> = None;
    loop {
        at += args[at.min(args.len())..]
            .iter()
            .take_while(|&&b| b == b' ')
            .count();
        let b = byte(args, at);
        if b < b' ' {
            break;
        }
        if b == b'-' {
            if pending.is_some() {
                return Err(Error::BadParameters);
            }
            at = dashed(&t, args, at + 1, &mut read, &mut layout, &mut pending)?;
            continue;
        }
        let (text, after) = item(args, at)?;
        at = after;
        let entry = match pending.take() {
            Some(entry) => entry,
            None => (0..count)
                .find(|&e| {
                    read[e].is_none()
                        && !t.entries[e].has(Qualifiers::KEYWORD)
                        && !t.entries[e].has(Qualifiers::SWITCH)
                })
                .ok_or(Error::BadParameters)?,
        };
        let arg = value(t.entries[entry], text, vars, budget, &mut layout, entry)?;
        read[entry] = Some(arg);
    }
    if pending.is_some() {
        return Err(Error::BadParameters);
    }
    let mut out = Vec::new();
    if !fallible::reserve(&mut out, count) {
        return Err(Error::SystemHeapFull);
    }
    for (entry, arg) in read.into_iter().enumerate() {
        match arg {
            Some(arg) => out.push(arg),
            None if t.entries[entry].has(Qualifiers::REQUIRED) => return Err(Error::BadParameters),
            None => out.push(Arg::Absent),
        }
    }
    let mut buffer = Vec::new();
    if !fallible::reserve(&mut buffer, 4 * count + layout.data.len()) {
        return Err(Error::SystemHeapFull);
    }
    for slot in &layout.slots {
        buffer.extend_from_slice(&slot.to_le_bytes());
    }
    buffer.extend_from_slice(&layout.data);
    let left = layout.free();
    Ok(ReadArgs {
        args: out,
        buffer,
        left,
    })
}

/// The keywords and switches a `-` at `at - 1` gives; where reading goes on.
/// A name no entry has is refused, as RISC OS 5 refuses one on any command
/// line.
fn dashed(
    t: &Template<'_>,
    args: &[u8],
    mut at: usize,
    read: &mut [Option<Arg>],
    layout: &mut Layout,
    pending: &mut Option<usize>,
) -> Result<usize, Error> {
    loop {
        let (entry, after, whole) = keyword(t, args, at).ok_or(Error::BadParameters)?;
        if read[entry].is_some() {
            return Err(Error::ArgumentRepeated);
        }
        if !t.entries[entry].has(Qualifiers::SWITCH) {
            *pending = Some(entry);
            return Ok(after);
        }
        read[entry] = Some(Arg::Switch);
        layout.slots[entry] = SWITCH;
        // Abbreviated switches run together; a whole name ends the run.
        if whole || !is_keyword_byte(byte(args, after)) {
            return Ok(after);
        }
        at = after;
    }
}

/// The item at `at`: quoted, `""` within it a `"`, or to the next space or
/// control character; and where it ends.
fn item(args: &[u8], at: usize) -> Result<(Vec<u8>, usize), Error> {
    let mut text = Vec::new();
    let push = |text: &mut Vec<u8>, b: u8| {
        if fallible::push(text, b) {
            Ok(())
        } else {
            Err(Error::SystemHeapFull)
        }
    };
    if byte(args, at) != b'"' {
        let len = args[at..].iter().take_while(|&&b| b > b' ').count();
        let text = fallible::copied(&args[at..at + len]).ok_or(Error::SystemHeapFull)?;
        return Ok((text, at + len));
    }
    let mut at = at + 1;
    loop {
        let b = byte(args, at);
        if b < b' ' {
            return Err(Error::BadString);
        }
        at += 1;
        if b == b'"' {
            if byte(args, at) != b'"' {
                return Ok((text, at));
            }
            at += 1;
        }
        push(&mut text, b)?;
    }
}

/// `text` laid out as `entry`'s value: copied with a NUL after it, then for
/// `/E` its value and for `/G` its translation laid over that copy.
fn value(
    qualifiers: Qualifiers,
    text: Vec<u8>,
    vars: &mut dyn Variables,
    budget: &mut Budget,
    layout: &mut Layout,
    entry: usize,
) -> Result<Arg, Error> {
    if text.len() + 1 > layout.free() {
        return Err(Error::BufferOverflow);
    }
    layout.slots[entry] = layout.next();
    if qualifiers.has(Qualifiers::EVALUATE) {
        if layout.free() < 3 {
            return Err(Error::BufferOverflow);
        }
        let answer = evaluate(&text, vars, budget)?;
        match &answer {
            Answer::Integer(n) => {
                if layout.free() < 5 {
                    return Err(Error::BufferOverflow);
                }
                let mut block = [0u8; 5];
                block[1..].copy_from_slice(&n.to_le_bytes());
                layout.put(&block, 5)?;
            }
            Answer::String(bytes) => {
                if bytes.len() > layout.free() - 3 {
                    return Err(Error::BufferOverflow);
                }
                let len = u16::try_from(bytes.len() & 0xFFFF).unwrap_or(0);
                let [low, high] = len.to_le_bytes();
                layout.put(&[1, low, high], 3)?;
                layout.put(bytes, bytes.len())?;
            }
        }
        return Ok(Arg::Evaluated(answer));
    }
    if qualifiers.has(Qualifiers::GSTRANS) {
        if layout.free() < 2 {
            return Err(Error::BufferOverflow);
        }
        // GSTrans needs a byte past its result for its terminator, so with no
        // room at all it overflows even on nothing.
        let longest = (layout.free() - 2)
            .checked_sub(1)
            .ok_or(Error::BufferOverflow)?;
        let (bytes, _) = translated(&text, Flags::NO_QUOTES, vars, budget, longest)?;
        let len = u16::try_from(bytes.len() & 0xFFFF).unwrap_or(0);
        layout.put(&len.to_le_bytes(), 2)?;
        layout.put(&bytes, bytes.len())?;
        layout.put(&[0], 0)?;
        return Ok(Arg::Translated(bytes));
    }
    layout.put(&text, text.len())?;
    layout.put(&[0], 1)?;
    Ok(Arg::Text(text))
}

#[cfg(test)]
#[path = "readargs_tests.rs"]
mod tests;
