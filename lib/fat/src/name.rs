//! Short names: the 8.3 field every entry carries, the names a field spells,
//! the aliases a long name is given (`BASE~N.EXT`, the lowest tail its
//! directory leaves free), and volume labels.
//!
//! A short name spells upper-case letters, digits, the punctuation below and
//! bytes of the code page above `7Fh`. FAT records no code page, so an alias
//! spells none of the last: a long name's other characters become `_`.

use crate::dir::{DELETED, DOT, DOTDOT, KANJI};

/// The punctuation a short name may hold beside its letters and digits.
pub const PUNCTUATION: &[u8] = b"$%'-_@~`!(){}^#&";
/// What formatters write for a volume given no label: a placeholder, not a
/// label.
pub const NO_LABEL: &[u8] = b"NO NAME";
/// The bytes of a label, as of a short name.
pub const LABEL_LEN: usize = 11;
/// How many consecutive tails an alias search looks across at once.
pub const TAIL_WINDOW: u32 = 1024;

/// `field` without the spaces that pad it.
#[must_use]
pub fn trimmed(field: &[u8]) -> &[u8] {
    let len = field
        .iter()
        .rposition(|&byte| byte != b' ')
        .map_or(0, |at| at + 1);
    &field[..len]
}

/// The name `field` spells — `BASE.EXT`, or `BASE` without an extension —
/// in `out`; its length.
pub fn display(field: &[u8; 11], out: &mut [u8; 12]) -> usize {
    let base = trimmed(&field[..8]);
    let ext = trimmed(&field[8..]);
    out[..base.len()].copy_from_slice(base);
    if out[0] == KANJI && !base.is_empty() {
        out[0] = DELETED;
    }
    let mut len = base.len();
    if !ext.is_empty() {
        out[len] = b'.';
        out[len + 1..len + 1 + ext.len()].copy_from_slice(ext);
        len += 1 + ext.len();
    }
    len
}

/// Whether `byte` may stand in a short name as it is.
const fn spelt(byte: u8) -> bool {
    byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte >= 0x80 || is_punctuation(byte)
}

const fn is_punctuation(byte: u8) -> bool {
    let mut i = 0;
    while i < PUNCTUATION.len() {
        if PUNCTUATION[i] == byte {
            return true;
        }
        i += 1;
    }
    false
}

/// The field of `name` when it is an 8.3 name, its letters taken in upper
/// case as DOS takes them: a base of one to eight bytes and an extension of
/// at most three, or `.` and `..`. `None` for any other name.
#[must_use]
pub fn field(name: &[u8]) -> Option<[u8; 11]> {
    match name {
        b"." => return Some(DOT),
        b".." => return Some(DOTDOT),
        _ => {}
    }
    let mut field = [b' '; 11];
    let (base, ext) = match name.iter().position(|&byte| byte == b'.') {
        Some(dot) => (&name[..dot], &name[dot + 1..]),
        None => (name, &[][..]),
    };
    if base.is_empty() || base.len() > 8 || ext.len() > 3 {
        return None;
    }
    let (base_field, ext_field) = field.split_at_mut(8);
    for (slot, &byte) in base_field
        .iter_mut()
        .zip(base)
        .chain(ext_field.iter_mut().zip(ext))
    {
        let byte = byte.to_ascii_uppercase();
        if !spelt(byte) {
            return None;
        }
        *slot = byte;
    }
    if field[0] == DELETED {
        field[0] = KANJI;
    }
    Some(field)
}

/// A long name's byte as an alias spells it: letters in upper case, digits
/// and [`PUNCTUATION`] as they are, anything else `_`.
#[must_use]
pub const fn alias_char(byte: u8) -> u8 {
    if byte.is_ascii_alphanumeric() {
        byte.to_ascii_uppercase()
    } else if is_punctuation(byte) {
        byte
    } else {
        b'_'
    }
}

/// `name` split at its last dot that does not lead it: base, then
/// extension.
#[must_use]
pub fn split(name: &[u8]) -> (&[u8], &[u8]) {
    match name
        .iter()
        .rposition(|&byte| byte == b'.')
        .filter(|&dot| dot != 0)
    {
        Some(dot) => (&name[..dot], &name[dot + 1..]),
        None => (name, &[]),
    }
}

/// The part of a long name's alias its `~N` tail leaves alone: the base and
/// extension as an alias spells them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AliasStem {
    base: [u8; 8],
    base_len: usize,
    ext: [u8; 3],
}

impl AliasStem {
    /// The stem of `name`'s aliases: its spaces and dots dropped, the rest
    /// spelt as [`alias_char`] spells it, the base kept to eight bytes and
    /// the extension to three.
    #[must_use]
    pub fn of(name: &[u8]) -> Self {
        let kept = |byte: &&u8| **byte != b' ' && **byte != b'.';
        let (base_src, ext_src) = split(name);
        let mut ext = [b' '; 3];
        for (slot, &byte) in ext.iter_mut().zip(ext_src.iter().filter(kept)) {
            *slot = alias_char(byte);
        }
        let mut base = [b'_'; 8];
        let mut base_len = 0;
        for (slot, &byte) in base.iter_mut().zip(base_src.iter().filter(kept)) {
            *slot = alias_char(byte);
            base_len += 1;
        }
        Self {
            base,
            base_len: base_len.max(1),
            ext,
        }
    }

    /// The alias carrying `tail`, its base cut short to make room; `None`
    /// for a tail of more than six digits.
    #[must_use]
    pub fn alias(&self, tail: u32) -> Option<[u8; 11]> {
        let mut digits = [0u8; 10];
        let mut len = 0;
        let mut rest = tail;
        loop {
            digits[len] = b'0' + u8::try_from(rest % 10).unwrap_or(0);
            len += 1;
            rest /= 10;
            if rest == 0 {
                break;
            }
        }
        if len > 6 {
            return None;
        }
        digits[..len].reverse();
        let keep = self.base_len.min(7 - len);
        let mut field = [b' '; 11];
        field[..keep].copy_from_slice(&self.base[..keep]);
        field[keep] = b'~';
        field[keep + 1..=keep + len].copy_from_slice(&digits[..len]);
        field[8..].copy_from_slice(&self.ext);
        Some(field)
    }

    /// The tail `name` carries, when it is this stem's alias for one.
    #[must_use]
    pub fn tail_of(&self, name: &[u8; 11]) -> Option<u32> {
        let tilde = name[..8].iter().rposition(|&byte| byte == b'~')?;
        let tail = name[tilde + 1..8]
            .iter()
            .take_while(|byte| byte.is_ascii_digit())
            .try_fold(0u32, |tail, &digit| {
                tail.checked_mul(10)?.checked_add(u32::from(digit - b'0'))
            })?;
        (self.alias(tail)? == *name).then_some(tail)
    }
}

/// Which of [`TAIL_WINDOW`] consecutive alias tails a directory's entries
/// hold, from `first`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TailWindow {
    first: u32,
    held: [u64; TAIL_WINDOW as usize / 64],
}

impl TailWindow {
    /// No tail held, the window starting at `first`.
    #[must_use]
    pub const fn starting_at(first: u32) -> Self {
        Self {
            first,
            held: [0; TAIL_WINDOW as usize / 64],
        }
    }

    /// Marks `tail` held, if the window covers it.
    pub fn hold(&mut self, tail: u32) {
        if let Some(index) = tail
            .checked_sub(self.first)
            .filter(|&index| index < TAIL_WINDOW)
        {
            if let Some(word) = self.held.get_mut(index as usize / 64) {
                *word |= 1 << (index % 64);
            }
        }
    }

    /// The lowest tail the window leaves free.
    #[must_use]
    pub fn lowest_free(&self) -> Option<u32> {
        self.held
            .iter()
            .zip((self.first..).step_by(64))
            .find_map(|(word, base)| (*word != u64::MAX).then(|| base + word.trailing_ones()))
    }
}

/// `text`, at most [`LABEL_LEN`] bytes of it, as a label field: space
/// padded.
#[must_use]
pub fn label_field(text: &[u8]) -> [u8; LABEL_LEN] {
    let mut field = [b' '; LABEL_LEN];
    for (slot, &byte) in field.iter_mut().zip(text) {
        *slot = byte;
    }
    field
}

/// Why a label was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LabelError {
    /// It spells a byte no label may, opens or closes with a space, or is
    /// the no-label placeholder.
    Spelling,
    /// It is longer than a label.
    Length,
}

/// `text` as a label field, or `None` for the empty text, which is no
/// label. A label spells upper-case letters, digits, spaces and
/// [`PUNCTUATION`] — no code-page byte, as FAT records no code page to
/// spell one in — neither opens nor closes with the space that pads it, and
/// is not the placeholder, which reads back as none.
pub fn label(text: &[u8]) -> Result<Option<[u8; LABEL_LEN]>, LabelError> {
    let spells = |&byte: &u8| {
        byte.is_ascii_uppercase() || byte.is_ascii_digit() || byte == b' ' || is_punctuation(byte)
    };
    if !text.iter().all(spells)
        || text.starts_with(b" ")
        || text.ends_with(b" ")
        || text == NO_LABEL
    {
        return Err(LabelError::Spelling);
    }
    if text.len() > LABEL_LEN {
        return Err(LabelError::Length);
    }
    Ok((!text.is_empty()).then(|| label_field(text)))
}

#[cfg(test)]
#[path = "name_tests.rs"]
mod tests;
