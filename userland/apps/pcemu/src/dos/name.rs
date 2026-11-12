//! Names as DOS reads them: a path made canonical against a drive's current
//! directory, as function `60h` reports it, and each of its components in
//! the eleven-byte form a directory entry holds — letters in upper case
//! through the country's table, a base cut to eight and an extension to
//! three, `*` filled out with `?`.

/// The longest canonical path: `C:\` and 64 more, its terminator included.
pub const PATH_MAX: usize = 67;

/// Code page 437's upper case for bytes from `80h`, as the US country table
/// gives it: accented lower-case letters become their capitals, where code
/// page 437 has them, and every other byte stays.
const UPPER_HIGH: [u8; 128] = {
    let mut table = [0u8; 128];
    let mut byte = 0x80u8;
    while byte >= 0x80 {
        table[(byte - 0x80) as usize] = byte;
        byte = byte.wrapping_add(1);
    }
    // ü é â ä à å ç ê ë è ï î ì Ä Å É æ Æ ô ö ò û ù ÿ Ö Ü
    let pairs: [(u8, u8); 18] = [
        (0x81, 0x9A),
        (0x82, 0x90),
        (0x83, 0x41),
        (0x84, 0x8E),
        (0x85, 0x41),
        (0x86, 0x8F),
        (0x87, 0x80),
        (0x88, 0x45),
        (0x89, 0x45),
        (0x8A, 0x45),
        (0x8B, 0x49),
        (0x8C, 0x49),
        (0x8D, 0x49),
        (0x91, 0x92),
        (0x93, 0x4F),
        (0x94, 0x99),
        (0x95, 0x4F),
        (0x96, 0x55),
    ];
    let mut p = 0;
    while p < pairs.len() {
        table[(pairs[p].0 - 0x80) as usize] = pairs[p].1;
        p += 1;
    }
    table[0x97 - 0x80] = 0x55;
    table[0x98 - 0x80] = 0x59;
    table[0xA0 - 0x80] = 0x41;
    table[0xA1 - 0x80] = 0x49;
    table[0xA2 - 0x80] = 0x4F;
    table[0xA3 - 0x80] = 0x55;
    table[0xA4 - 0x80] = 0xA5;
    table
};

/// `byte` in upper case, as the country's table has it.
#[must_use]
pub const fn upper(byte: u8) -> u8 {
    match byte {
        b'a'..=b'z' => byte - 0x20,
        0x80..=0xFF => UPPER_HIGH[(byte - 0x80) as usize],
        _ => byte,
    }
}

/// The table function `65h` hands out for bytes from `80h`.
#[must_use]
pub const fn upper_table() -> &'static [u8; 128] {
    &UPPER_HIGH
}

/// Bytes no file name may hold.
#[must_use]
pub const fn is_forbidden(byte: u8) -> bool {
    byte < 0x20
        || matches!(
            byte,
            b'"' | b'+'
                | b','
                | b'/'
                | b':'
                | b';'
                | b'<'
                | b'='
                | b'>'
                | b'['
                | b'\\'
                | b']'
                | b'|'
                | 0x7F
        )
}

/// Whether `byte` separates a path's components.
#[must_use]
pub const fn is_separator(byte: u8) -> bool {
    byte == b'\\' || byte == b'/'
}

/// Why a name was refused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NameError {
    /// A path that goes nowhere: an empty component, a forbidden byte, or a
    /// wildcard where none may be. DOS's `path not found`.
    Path,
    /// A file name that is none. DOS's `file not found`.
    File,
    /// A drive letter outside the drives.
    Drive,
}

/// `component` in a directory entry's form, as DOS takes it: each part cut
/// to its length, letters in upper case, a `*` filling the rest of its part
/// with `?`. Wildcards are refused unless `wild`.
pub fn field(component: &[u8], wild: bool) -> Result<[u8; 11], NameError> {
    if component == b"." || component == b".." {
        let mut out = [b' '; 11];
        out[..component.len()].copy_from_slice(component);
        return Ok(out);
    }
    let (base, ext) = match component.iter().position(|&b| b == b'.') {
        Some(dot) => (&component[..dot], &component[dot + 1..]),
        None => (component, &[][..]),
    };
    if base.is_empty() || ext.contains(&b'.') {
        return Err(NameError::File);
    }
    let mut out = [b' '; 11];
    fill(&mut out[..8], base, wild)?;
    fill(&mut out[8..], ext, wild)?;
    if out[0] == 0xE5 {
        out[0] = 0x05;
    }
    Ok(out)
}

/// The 11-byte name a volume label `text` makes: up to eight bytes, a dot,
/// up to three more, in upper case, as a name's field is made but with its
/// spaces kept; none for an empty label, a wildcard or a control
/// character.
#[must_use]
pub fn label_field(text: &[u8]) -> Option<[u8; 11]> {
    let (base, ext) = match text.iter().position(|&b| b == b'.') {
        Some(dot) => (&text[..dot], &text[dot + 1..]),
        None => (text, &[][..]),
    };
    let refused = |b: &u8| matches!(*b, b'*' | b'?' | b'.') || *b < b' ';
    if base.iter().chain(ext).any(refused) || base.iter().chain(ext).all(|&b| b == b' ') {
        return None;
    }
    let mut out = [b' '; 11];
    for (slot, &byte) in out[..8].iter_mut().zip(base) {
        *slot = upper(byte);
    }
    for (slot, &byte) in out[8..].iter_mut().zip(ext) {
        *slot = upper(byte);
    }
    Some(out)
}

fn fill(out: &mut [u8], part: &[u8], wild: bool) -> Result<(), NameError> {
    for (at, &byte) in part.iter().enumerate().take(out.len()) {
        match byte {
            b'*' if wild => {
                out[at..].fill(b'?');
                return Ok(());
            }
            b'?' if wild => out[at] = b'?',
            b'*' | b'?' | b' ' => return Err(NameError::File),
            _ if is_forbidden(byte) => return Err(NameError::File),
            _ => out[at] = upper(byte),
        }
    }
    Ok(())
}

/// Whether the entry name `name` matches the pattern `pattern`, `?`
/// matching any byte.
#[must_use]
pub fn matches(pattern: &[u8; 11], name: &[u8; 11]) -> bool {
    pattern
        .iter()
        .zip(name)
        .all(|(&p, &n)| p == b'?' || p == upper(n))
}

/// Whether `field` holds a wildcard.
#[must_use]
pub fn is_wild(field: &[u8; 11]) -> bool {
    field.contains(&b'?')
}

/// A path made canonical: `X:\A\B`, upper case, `.` and `..` resolved, no
/// trailing separator but the root's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Canonical {
    /// Its bytes, `len` of them.
    pub bytes: [u8; PATH_MAX],
    /// How many there are.
    pub len: usize,
}

impl Canonical {
    /// The path's bytes.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..self.len]
    }

    /// The drive it is on, `0` for A:.
    #[must_use]
    pub fn drive(&self) -> u8 {
        self.bytes[0] - b'A'
    }

    /// Its components after the root.
    pub fn components(&self) -> impl Iterator<Item = &[u8]> {
        self.as_bytes()[3..]
            .split(|&b| b == b'\\')
            .filter(|c| !c.is_empty())
    }

    /// Its last component, if it is not the root.
    #[must_use]
    pub fn last(&self) -> Option<&[u8]> {
        self.components().last()
    }

    fn push(&mut self, byte: u8) -> Result<(), NameError> {
        if self.len >= PATH_MAX - 1 {
            return Err(NameError::Path);
        }
        self.bytes[self.len] = byte;
        self.len += 1;
        Ok(())
    }
}

/// The directory a path names its last component in, as typed: as far as
/// its last `\` or its drive's `:`, or nothing.
#[must_use]
pub fn directory_of(path: &[u8]) -> &[u8] {
    let end = path
        .iter()
        .rposition(|&b| b == b'\\' || b == b':')
        .map_or(0, |at| at + 1);
    &path[..end]
}

/// Whether a name or path holds a wildcard.
#[must_use]
pub fn wild(text: &[u8]) -> bool {
    text.iter().any(|&b| b == b'*' || b == b'?')
}

/// The drive `path` names, else `drive`, and the rest of it.
#[must_use]
pub fn drive_of(path: &[u8], drive: u8) -> (u8, &[u8]) {
    match path {
        [letter, b':', rest @ ..] => (upper(*letter).wrapping_sub(b'A'), rest),
        _ => (drive, path),
    }
}

/// `rest`, a path on `drive` without its drive, made canonical: a relative
/// one taken from `current`, the drive's current directory as its CDS holds
/// it (`X:\...`). A wildcard is kept in the last component only.
///
/// # Errors
///
/// [`NameError`] for a path no canonical one is.
pub fn canonical(
    drive: u8,
    rest: &[u8],
    current: &[u8],
    drives: u8,
) -> Result<Canonical, NameError> {
    if drive >= drives {
        return Err(NameError::Drive);
    }
    let mut out = Canonical {
        bytes: [0; PATH_MAX],
        len: 0,
    };
    out.push(b'A' + drive)?;
    out.push(b':')?;
    out.push(b'\\')?;
    let mut components: [&[u8]; 32] = [&[]; 32];
    let mut count = 0usize;
    let relative = !rest.first().is_some_and(|&b| is_separator(b));
    let base: &[u8] = if relative && current.len() > 3 {
        &current[3..]
    } else {
        &[]
    };
    for part in base
        .split(|&b| b == b'\\')
        .chain(rest.split(|&b| is_separator(b)))
    {
        match part {
            b"" | b"." => {}
            b".." => count = count.checked_sub(1).ok_or(NameError::Path)?,
            _ => {
                if count == components.len() {
                    return Err(NameError::Path);
                }
                components[count] = part;
                count += 1;
            }
        }
    }
    for (index, part) in components[..count].iter().enumerate() {
        if index > 0 {
            out.push(b'\\')?;
        }
        let field = field(part, index + 1 == count).map_err(|_| NameError::Path)?;
        write_field(&mut out, &field)?;
    }
    Ok(out)
}

/// Appends `field` as a name: base, then `.` and extension if it has one.
fn write_field(out: &mut Canonical, field: &[u8; 11]) -> Result<(), NameError> {
    let base = trim(&field[..8]);
    let ext = trim(&field[8..]);
    for (i, &b) in base.iter().enumerate() {
        out.push(if i == 0 && b == 0x05 { 0xE5 } else { b })?;
    }
    if !ext.is_empty() {
        out.push(b'.')?;
        for &b in ext {
            out.push(b)?;
        }
    }
    Ok(())
}

fn trim(part: &[u8]) -> &[u8] {
    let len = part.iter().rposition(|&b| b != b' ').map_or(0, |at| at + 1);
    &part[..len]
}

/// The name `field` spells — `BASE.EXT` — in `out`; its length.
pub fn spell(field: &[u8; 11], out: &mut [u8; 13]) -> usize {
    let mut path = Canonical {
        bytes: [0; PATH_MAX],
        len: 0,
    };
    if write_field(&mut path, field).is_err() {
        return 0;
    }
    out[..path.len].copy_from_slice(path.as_bytes());
    path.len
}

#[cfg(test)]
#[path = "name_tests.rs"]
mod tests;
