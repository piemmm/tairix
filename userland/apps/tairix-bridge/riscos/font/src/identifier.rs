//! Font identifiers as `Font_FindFont` reads them: a plain identifier, or
//! fields each a backslash and a letter — `\F` the font, `\f` its name in a
//! territory, `\E` the encoding, `\e` its name, `\M` a matrix — the first of
//! each letter counting and any other letter passed over, as RISC OS 5.30
//! reads them. The whole ends at a control character; a field ends at the
//! next backslash, and the font's at a space too.

use alloc::vec::Vec;

/// The byte that opens a field.
pub(crate) const FIELD: u8 = b'\\';
/// The fields' letters.
pub(crate) mod letter {
    pub(crate) const FONT: u8 = b'F';
    pub(crate) const FONT_NAME: u8 = b'f';
    pub(crate) const ENCODING: u8 = b'E';
    pub(crate) const MATRIX: u8 = b'M';
}

/// The longest encoding identifier the Font Manager keeps: a longer one is
/// cut to it, as RISC OS's own errors show.
pub(crate) const ENCODING_MOST: usize = 12;

/// Whether `byte` ends an identifier.
pub(crate) const fn ends(byte: u8) -> bool {
    byte < b' '
}

/// A field's value: where it starts in the identifier, and its bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Field<'a> {
    pub(crate) at: usize,
    pub(crate) value: &'a [u8],
}

/// The value of the first field `wanted` names in `text`, which runs to its
/// terminator or its end. A text that does not open with a backslash is
/// the font's own field.
pub(crate) fn find(text: &[u8], wanted: u8) -> Option<Field<'_>> {
    let text = &text[..text.iter().position(|&b| ends(b)).unwrap_or(text.len())];
    let mut at = 0;
    if text.first() != Some(&FIELD) {
        let value = value_at(text, 0, letter::FONT);
        if wanted == letter::FONT {
            return Some(Field { at: 0, value });
        }
        at = value.len();
    }
    while at < text.len() {
        let open = text[at..].iter().position(|&b| b == FIELD)?;
        let start = at + open + 2;
        let &kind = text.get(at + open + 1)?;
        let value = value_at(text, start, kind);
        if kind == wanted {
            return Some(Field { at: start, value });
        }
        at = start + value.len();
    }
    None
}

/// A field of kind `kind`'s value from `start`: to the next backslash, and
/// for the font's to a space too.
fn value_at(text: &[u8], start: usize, kind: u8) -> &[u8] {
    let rest = text.get(start..).unwrap_or(&[]);
    let length = rest
        .iter()
        .position(|&b| b == FIELD || (kind == letter::FONT && b == b' '))
        .unwrap_or(rest.len());
    &rest[..length]
}

/// What a font identifier asks for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Asked<'a> {
    /// The font's identifier.
    pub(crate) font: Option<&'a [u8]>,
    /// Its name in a territory: the territory's number and the name.
    pub(crate) font_name: Option<(u32, &'a [u8])>,
    /// The encoding's identifier, as given.
    pub(crate) encoding: Option<&'a [u8]>,
    /// The matrix, if one is given.
    pub(crate) matrix: Option<Result<Matrix, BadMatrix>>,
}

impl<'a> Asked<'a> {
    /// What `text` asks for.
    pub(crate) fn of(text: &'a [u8]) -> Self {
        let value = |kind| find(text, kind).map(|field| field.value);
        Self {
            font: value(letter::FONT),
            font_name: value(letter::FONT_NAME).map(territory_name),
            encoding: value(letter::ENCODING),
            matrix: value(letter::MATRIX).map(Matrix::parse),
        }
    }

    /// The name an error names the font by: its name in a territory where
    /// that is the current one or no identifier is given, else the
    /// identifier.
    pub(crate) fn shown(&self, territory: u32) -> &'a [u8] {
        match (self.font, self.font_name) {
            (Some(font), Some((number, _))) if number != territory => font,
            (_, Some((_, name))) => name,
            (Some(font), None) => font,
            (None, None) => &[],
        }
    }
}

/// A `\f` field's territory number and the name after its space.
fn territory_name(value: &[u8]) -> (u32, &[u8]) {
    let digits = value.iter().take_while(|b| b.is_ascii_digit()).count();
    let number = value[..digits].iter().fold(0u32, |n, &d| {
        n.wrapping_mul(10).wrapping_add(u32::from(d - b'0'))
    });
    let name = value.get(digits + 1..).unwrap_or(&[]);
    (number, name)
}

/// An encoding identifier as the Font Manager keeps it: in lower case, cut
/// to [`ENCODING_MOST`].
pub(crate) fn encoding_key(encoding: &[u8]) -> Vec<u8> {
    encoding
        .iter()
        .take(ENCODING_MOST)
        .map(u8::to_ascii_lowercase)
        .collect()
}

/// A font's transformation: four 16.16 coefficients and two offsets in
/// thousandths of an em, as six words.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Matrix(pub(crate) [i32; 6]);

/// A `\M` field that is not six numbers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct BadMatrix;

impl Matrix {
    /// The six numbers of a `\M` field, each decimal or `&` hexadecimal, a
    /// minus before it negating it, spaces between and around them, and
    /// each fitting 32 bits, a larger unsigned one wrapping to negative.
    pub(crate) fn parse(value: &[u8]) -> Result<Self, BadMatrix> {
        let mut words = [0i32; 6];
        let mut at = 0;
        for word in &mut words {
            at += spaces(&value[at..]);
            let (number, used) = number(&value[at..]).ok_or(BadMatrix)?;
            *word = number;
            at += used;
            if value.get(at).is_some_and(|&b| b != b' ') {
                return Err(BadMatrix);
            }
        }
        at += spaces(&value[at..]);
        if at == value.len() {
            Ok(Self(words))
        } else {
            Err(BadMatrix)
        }
    }

    /// The `\M` field a full identifier gives: each number after a space.
    pub(crate) fn spelled(&self) -> Vec<u8> {
        let mut out = Vec::new();
        for word in self.0 {
            out.push(b' ');
            out.extend_from_slice(tairix_riscos_kernel::Decimal::of(word).as_bytes());
        }
        out
    }
}

fn spaces(text: &[u8]) -> usize {
    text.iter().take_while(|&&b| b == b' ').count()
}

/// A number at the start of `text` and the bytes it took.
fn number(text: &[u8]) -> Option<(i32, usize)> {
    let negative = text.first() == Some(&b'-');
    let mut at = usize::from(negative);
    let radix = if text.get(at) == Some(&b'&') {
        at += 1;
        16
    } else {
        10
    };
    let digits = text[at..]
        .iter()
        .take_while(|b| char::from(**b).is_digit(radix))
        .count();
    if digits == 0 {
        return None;
    }
    let mut value: u32 = 0;
    for &b in &text[at..at + digits] {
        let digit = char::from(b).to_digit(radix)?;
        value = value.checked_mul(radix)?.checked_add(digit)?;
    }
    let signed = value.cast_signed();
    Some((
        if negative {
            signed.wrapping_neg()
        } else {
            signed
        },
        at + digits,
    ))
}

/// `Font_ApplyFields`: `original`'s fields with each `fields` gives in its
/// place, then the fields only `fields` has after them, as RISC OS 5.30
/// merges them: a field `fields` gives empty is left out, and a plain
/// identifier stays plain.
pub(crate) fn apply(original: &[u8], fields: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let plain = original.first().is_some_and(|&b| b != FIELD && !ends(b));
    let original_kinds = kinds(original);
    for &kind in &original_kinds {
        let from = if find(fields, kind).is_some() {
            fields
        } else {
            original
        };
        if let Some(field) = find(from, kind) {
            let unmarked = kind == letter::FONT && plain && out.is_empty();
            write_field(&mut out, kind, field.value, unmarked);
        }
    }
    for kind in kinds(fields) {
        if !original_kinds.contains(&kind) {
            if let Some(field) = find(fields, kind) {
                write_field(&mut out, kind, field.value, false);
            }
        }
    }
    out
}

/// A field of kind `kind` written to `out` — its backslash and letter
/// unless `unmarked`, then its value — or nothing where its value is empty.
fn write_field(out: &mut Vec<u8>, kind: u8, value: &[u8], unmarked: bool) {
    if value.is_empty() {
        return;
    }
    if !unmarked {
        out.push(FIELD);
        out.push(kind);
    }
    out.extend_from_slice(value);
}

/// The letters of the fields `text` has, each once, in their order.
fn kinds(text: &[u8]) -> Vec<u8> {
    let text = &text[..text.iter().position(|&b| ends(b)).unwrap_or(text.len())];
    let mut found = Vec::new();
    let mut at = 0;
    if text.first() != Some(&FIELD) && !text.is_empty() {
        found.push(letter::FONT);
        at = value_at(text, 0, letter::FONT).len();
    }
    while let Some(open) = text
        .get(at..)
        .and_then(|rest| rest.iter().position(|&b| b == FIELD))
    {
        let Some(&kind) = text.get(at + open + 1) else {
            break;
        };
        if !found.contains(&kind) {
            found.push(kind);
        }
        at += open + 2 + value_at(text, at + open + 2, kind).len();
    }
    found
}

#[cfg(test)]
mod tests;
