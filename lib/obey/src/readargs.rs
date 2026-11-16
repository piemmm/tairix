//! `OS_ReadArgs`: a command line's arguments read against a keyword template,
//! and laid out as the call leaves them in its buffer.
//!
//! A template is entries separated by commas, each one or more names joined by
//! `=` and qualified by `/A` (required), `/K` (only by keyword), `/S` (a
//! switch), `/E` (an expression's value) or `/G` (GSTrans'd). An argument is
//! given positionally, or after `-name`; a name may be abbreviated to its
//! first letter, and abbreviated switches run together, `-abc`.

use alloc::borrow::Cow;
use alloc::vec::Vec;
use core::cmp::Ordering;
use core::ops::Range;

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
    /// Text, its quotes removed: where the buffer holds it.
    Text(Range<usize>),
    /// A `/G` argument, GSTrans'd: where the buffer holds it.
    Translated(Range<usize>),
    /// A `/E` argument's value.
    Evaluated(Answer),
}

/// What `OS_ReadArgs` read.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ReadArgs {
    /// Each template entry's argument, in template order.
    pub args: Vec<Arg>,
    /// The buffer as the call leaves it: a word per entry, an argument's the
    /// address of its value, then the values in the order they were given,
    /// each laid where the one before ended, with what is left of the text
    /// an `/E` or `/G` value was copied from.
    pub buffer: Vec<u8>,
    /// The room left as RISC OS 5 counts it: each `/E` integer takes eight
    /// bytes, three it never writes, and a `/G` value's terminator none.
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

/// A template read: each entry's qualifiers; every name with its entry,
/// sorted by name without regard to case and then by entry; and the first
/// entry each name's first two letters abbreviate.
struct Template<'t> {
    entries: Vec<Qualifiers>,
    names: Vec<(&'t [u8], usize)>,
    abbreviated: Vec<usize>,
}

const fn is_keyword_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_'
}

/// The keyword characters, one class each without regard to case.
const CLASSES: usize = 26 + 10 + 1;

/// `b`'s class, where it is a keyword character.
const fn class(b: u8) -> Option<usize> {
    match b.to_ascii_uppercase() {
        letter @ b'A'..=b'Z' => Some((letter - b'A') as usize),
        digit @ b'0'..=b'9' => Some(26 + (digit - b'0') as usize),
        b'_' => Some(36),
        _ => None,
    }
}

/// `a` against `b`, without regard to case.
fn folded(a: &[u8], b: &[u8]) -> Ordering {
    a.iter()
        .map(u8::to_ascii_uppercase)
        .cmp(b.iter().map(u8::to_ascii_uppercase))
}

fn template(text: &[u8]) -> Result<Template<'_>, Error> {
    let end = text.iter().position(|&b| b < b' ').unwrap_or(text.len());
    let mut entries = Vec::new();
    let mut names: Vec<(&[u8], usize)> = Vec::new();
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
            if !fallible::push(&mut names, (&alias[start..start + len], index)) {
                return Err(Error::SystemHeapFull);
            }
        }
        if !fallible::push(&mut entries, qualifiers) {
            return Err(Error::SystemHeapFull);
        }
    }
    let mut abbreviated = Vec::new();
    if !fallible::reserve(&mut abbreviated, CLASSES * CLASSES) {
        return Err(Error::SystemHeapFull);
    }
    abbreviated.resize(CLASSES * CLASSES, usize::MAX);
    for &(name, entry) in &names {
        if let [first, second, ..] = *name {
            if let (Some(first), Some(second)) = (class(first), class(second)) {
                let slot = &mut abbreviated[first * CLASSES + second];
                *slot = (*slot).min(entry);
            }
        }
    }
    names.sort_unstable_by(|(a, x), (b, y)| folded(a, b).then(x.cmp(y)));
    Ok(Template {
        entries,
        names,
        abbreviated,
    })
}

/// The bytes after a `-` at `at` that name an entry: a whole name not
/// followed by a keyword character, else the first entry with a name of two
/// letters or more whose first letter alone they begin with. The entry, where
/// the name ends in the text, and whether it was whole.
fn keyword(t: &Template<'_>, args: &[u8], at: usize) -> Option<(usize, usize, bool)> {
    let word = &args[at.min(args.len())..];
    let word = &word[..word.iter().take_while(|&&b| is_keyword_byte(b)).count()];
    let first = t
        .names
        .partition_point(|&(name, _)| folded(name, word) == Ordering::Less);
    if let Some(&(name, entry)) = t.names.get(first) {
        if folded(name, word) == Ordering::Equal {
            return Some((entry, at + word.len(), true));
        }
    }
    let first = class(byte(args, at))?;
    let unlike = class(byte(args, at + 1));
    (0..CLASSES)
        .filter(|&second| Some(second) != unlike)
        .map(|second| t.abbreviated[first * CLASSES + second])
        .min()
        .filter(|&entry| entry != usize::MAX)
        .map(|entry| (entry, at + 1, false))
}

/// The room an `/E` integer is counted as taking: its five bytes, and three
/// RISC OS 5 counts and leaves unwritten, the next value laid after the five.
const INTEGER_ROOM: usize = 8;

/// The buffer as it fills: a word per entry, then the values, each laid where
/// the one before ended.
struct Layout {
    base: u32,
    capacity: usize,
    /// Every byte written: each value, and the text an `/E` or `/G` value was
    /// copied from first, which a shorter value leaves the rest of.
    buffer: Vec<u8>,
    /// Where the next value is laid.
    end: usize,
    /// The room taken: the bytes laid, but for a `/G` value's terminator, and
    /// each `/E` integer as [`INTEGER_ROOM`].
    taken: usize,
}

impl Layout {
    fn free(&self) -> usize {
        self.capacity.saturating_sub(self.taken)
    }

    /// The address the next value is laid at.
    fn next(&self) -> u32 {
        self.base
            .wrapping_add(u32::try_from(self.end).unwrap_or(u32::MAX))
    }

    fn set_slot(&mut self, entry: usize, word: u32) {
        self.buffer[4 * entry..4 * entry + 4].copy_from_slice(&word.to_le_bytes());
    }

    /// `bytes` written at `at`, beyond the buffer's end as it stands where
    /// they reach it.
    fn write(&mut self, at: usize, bytes: &[u8]) -> Result<(), Error> {
        let end = at + bytes.len();
        if let Some(grow) = end.checked_sub(self.buffer.len()) {
            if !fallible::reserve(&mut self.buffer, grow) {
                return Err(Error::SystemHeapFull);
            }
            self.buffer.resize(end, 0);
        }
        self.buffer[at..end].copy_from_slice(bytes);
        Ok(())
    }

    /// `bytes` laid at the next place, taking `room` of the buffer's room;
    /// where they lie.
    fn put(&mut self, bytes: &[u8], room: usize) -> Result<Range<usize>, Error> {
        let at = self.end;
        self.write(at, bytes)?;
        self.end += bytes.len();
        self.taken += room;
        Ok(at..self.end)
    }

    /// `text` copied to the next place with a NUL after it, as RISC OS copies
    /// a value's text before reading it, taking no room.
    fn copy(&mut self, text: &[u8]) -> Result<(), Error> {
        self.write(self.end, text)?;
        self.write(self.end + text.len(), &[0])
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
        buffer: Vec::new(),
        end: 4 * count,
        taken: 4 * count,
    };
    if !fallible::reserve(&mut layout.buffer, 4 * count) {
        return Err(Error::SystemHeapFull);
    }
    layout.buffer.resize(4 * count, 0);
    let mut read: Vec<Option<Arg>> = Vec::new();
    if !fallible::reserve(&mut read, count) {
        return Err(Error::SystemHeapFull);
    }
    read.resize_with(count, || None);
    let mut at = 0;
    // A keyword given, waiting for its value.
    let mut pending: Option<usize> = None;
    // Entries are only ever filled, so the first a positional item can take
    // never moves back.
    let mut positional = 0;
    let mut first = true;
    loop {
        at += args[at.min(args.len())..]
            .iter()
            .take_while(|&&b| b == b' ')
            .count();
        let b = byte(args, at);
        if b < b' ' {
            break;
        }
        let opening = core::mem::replace(&mut first, false);
        if b == b'-' {
            if pending.is_some() {
                return Err(Error::BadParameters);
            }
            match dashed(&t, args, at + 1, &mut read, &mut layout, &mut pending)? {
                Some(after) => at = after,
                // An unknown name first on the line gives the first entry
                // a positional item could take an empty value, and ends
                // the reading; anywhere else it is refused.
                None if opening => {
                    let entry = first_positional(&t, &read, 0).ok_or(Error::BadParameters)?;
                    read[entry] = Some(value(
                        t.entries[entry],
                        b"",
                        vars,
                        budget,
                        &mut layout,
                        entry,
                    )?);
                    break;
                }
                None => return Err(Error::BadParameters),
            }
            continue;
        }
        let (text, after) = item(args, at)?;
        at = after;
        let entry = if let Some(entry) = pending.take() {
            entry
        } else {
            positional = first_positional(&t, &read, positional).ok_or(Error::BadParameters)?;
            positional
        };
        let arg = value(t.entries[entry], &text, vars, budget, &mut layout, entry)?;
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
    let left = layout.free();
    Ok(ReadArgs {
        args: out,
        buffer: layout.buffer,
        left,
    })
}

/// The first entry from `from` that a positional item may take: unfilled,
/// neither a keyword's alone nor a switch.
fn first_positional(t: &Template<'_>, read: &[Option<Arg>], from: usize) -> Option<usize> {
    (from..t.entries.len()).find(|&entry| {
        read[entry].is_none()
            && !t.entries[entry].has(Qualifiers::KEYWORD)
            && !t.entries[entry].has(Qualifiers::SWITCH)
    })
}

/// The keywords and switches a `-` at `at - 1` gives; where reading goes on,
/// or `None` where the first names no entry. A later name in a run of
/// switches that names none is refused.
fn dashed(
    t: &Template<'_>,
    args: &[u8],
    mut at: usize,
    read: &mut [Option<Arg>],
    layout: &mut Layout,
    pending: &mut Option<usize>,
) -> Result<Option<usize>, Error> {
    let mut run = false;
    loop {
        let Some((entry, after, whole)) = keyword(t, args, at) else {
            return if run {
                Err(Error::BadParameters)
            } else {
                Ok(None)
            };
        };
        run = true;
        if read[entry].is_some() {
            return Err(Error::ArgumentRepeated);
        }
        if !t.entries[entry].has(Qualifiers::SWITCH) {
            *pending = Some(entry);
            return Ok(Some(after));
        }
        read[entry] = Some(Arg::Switch);
        layout.set_slot(entry, SWITCH);
        // Abbreviated switches run together; a whole name ends the run.
        if whole || !is_keyword_byte(byte(args, after)) {
            return Ok(Some(after));
        }
        at = after;
    }
}

/// The item at `at`: quoted, `""` within it a `"`, or to the next space or
/// control character; and where it ends. Only a quoted item holding a `""`
/// is copied.
fn item(args: &[u8], at: usize) -> Result<(Cow<'_, [u8]>, usize), Error> {
    if byte(args, at) != b'"' {
        let len = args[at..].iter().take_while(|&&b| b > b' ').count();
        return Ok((Cow::Borrowed(&args[at..at + len]), at + len));
    }
    let start = at + 1;
    let mut at = start;
    let mut doubled = false;
    loop {
        let b = byte(args, at);
        if b < b' ' {
            return Err(Error::BadString);
        }
        if b == b'"' {
            if byte(args, at + 1) != b'"' {
                break;
            }
            doubled = true;
            at += 1;
        }
        at += 1;
    }
    let quoted = &args[start..at];
    if !doubled {
        return Ok((Cow::Borrowed(quoted), at + 1));
    }
    let mut text = Vec::new();
    if !fallible::reserve(&mut text, quoted.len()) {
        return Err(Error::SystemHeapFull);
    }
    let mut rest = quoted;
    while let Some(quote) = rest.iter().position(|&b| b == b'"') {
        text.extend_from_slice(&rest[..=quote]);
        rest = &rest[quote + 2..];
    }
    text.extend_from_slice(rest);
    Ok((Cow::Owned(text), at + 1))
}

/// `text` laid out as `entry`'s value: copied with a NUL after it, then for
/// `/E` its value and for `/G` its translation laid over that copy.
fn value(
    qualifiers: Qualifiers,
    text: &[u8],
    vars: &mut dyn Variables,
    budget: &mut Budget,
    layout: &mut Layout,
    entry: usize,
) -> Result<Arg, Error> {
    if text.len() + 1 > layout.free() {
        return Err(Error::BufferOverflow);
    }
    layout.set_slot(entry, layout.next());
    if qualifiers.has(Qualifiers::EVALUATE) {
        layout.copy(text)?;
        let answer = evaluate(text, vars, budget)?;
        match &answer {
            Answer::Integer(n) => {
                if layout.free() < INTEGER_ROOM {
                    return Err(Error::BufferOverflow);
                }
                let mut block = [0u8; 5];
                block[1..].copy_from_slice(&n.to_le_bytes());
                layout.put(&block, INTEGER_ROOM)?;
            }
            Answer::String(bytes) => {
                if bytes.len() + 3 > layout.free() {
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
        layout.copy(text)?;
        // GSTrans needs a byte past its result for its terminator, and the
        // length word holds sixteen bits.
        let longest = layout
            .free()
            .checked_sub(3)
            .ok_or(Error::BufferOverflow)?
            .min(usize::from(u16::MAX));
        let (bytes, _) = translated(text, Flags::NO_QUOTES, vars, budget, longest)?;
        let len = u16::try_from(bytes.len()).map_err(|_| Error::BufferOverflow)?;
        layout.put(&len.to_le_bytes(), 2)?;
        let at = layout.put(&bytes, bytes.len())?;
        // Written, but neither counted nor kept from the next value.
        layout.copy(&[])?;
        return Ok(Arg::Translated(at));
    }
    let at = layout.put(text, text.len())?;
    layout.put(&[0], 1)?;
    Ok(Arg::Text(at))
}

#[cfg(test)]
#[path = "readargs_tests.rs"]
mod tests;
