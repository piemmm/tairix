//! Names: ISO 8859-1 on disk, UTF-8 above; the hash that files a name in its
//! directory's table; and soft-link targets in AmigaDOS's spelling and in
//! POSIX's.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::driver::filesystem::NameMatching;
use tairix_abi::DriverError;
use tairix_fsprobe::affs::NAME_MAX;

/// A name as a volume records it: ISO 8859-1, one to [`NAME_MAX`] bytes.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Name {
    bytes: [u8; NAME_MAX],
    len: u8,
}

impl Name {
    /// The ISO 8859-1 spelling of the UTF-8 `name`: what a lookup, a removal
    /// and a rename's source look for, which finds any name a volume records.
    ///
    /// # Errors
    ///
    /// [`DriverError::LengthOutOfRange`] for an empty name or one past
    /// [`NAME_MAX`] bytes; [`DriverError::OutOfRange`] for a name that is not
    /// UTF-8 or holds a character ISO 8859-1 lacks.
    pub(crate) fn encode(name: &[u8]) -> Result<Self, DriverError> {
        let text = core::str::from_utf8(name).map_err(|_| DriverError::OutOfRange)?;
        let mut bytes = [0u8; NAME_MAX];
        let mut len = 0usize;
        for c in text.chars() {
            let byte = u8::try_from(u32::from(c)).map_err(|_| DriverError::OutOfRange)?;
            let slot = bytes.get_mut(len).ok_or(DriverError::LengthOutOfRange)?;
            *slot = byte;
            len += 1;
        }
        if len == 0 {
            return Err(DriverError::LengthOutOfRange);
        }
        Ok(Self {
            bytes,
            len: u8::try_from(len).map_err(|_| DriverError::LengthOutOfRange)?,
        })
    }

    /// The name a volume recorded, `bytes` long, unchecked beyond its length:
    /// what an existing entry is called is what it is called.
    pub(crate) fn recorded(bytes: &[u8]) -> Result<Self, DriverError> {
        let mut name = [0u8; NAME_MAX];
        name.get_mut(..bytes.len())
            .ok_or(DriverError::BadMagic)?
            .copy_from_slice(bytes);
        Ok(Self {
            bytes: name,
            len: u8::try_from(bytes.len()).map_err(|_| DriverError::BadMagic)?,
        })
    }

    /// This name as one a change gives an entry.
    ///
    /// # Errors
    ///
    /// [`DriverError::OutOfRange`] for an empty name, or one holding a control
    /// character or the `:` and `/` AmigaDOS separates paths with.
    pub(crate) fn given(self) -> Result<Self, DriverError> {
        let bytes = self.as_bytes();
        if !bytes.is_empty() && bytes.iter().all(|&byte| storable(byte)) {
            Ok(self)
        } else {
            Err(DriverError::OutOfRange)
        }
    }

    /// The recorded bytes.
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }

    /// Whether `other` names the same entry under `matching`.
    pub(crate) fn matches(&self, other: &Self, matching: NameMatching) -> bool {
        self.len == other.len
            && self
                .as_bytes()
                .iter()
                .zip(other.as_bytes())
                .all(|(&a, &b)| fold(a, matching) == fold(b, matching))
    }

    /// The name in UTF-8, into `out`, answering its length: at most two bytes
    /// for each recorded one.
    pub(crate) fn decode(&self, out: &mut [u8; 2 * NAME_MAX]) -> usize {
        decode_into(self.as_bytes(), out)
    }

    /// The bucket of a directory's `table_len`-entry hash table the name is
    /// filed in: AmigaDOS's hash, its letters folded as `matching` folds them.
    pub(crate) fn bucket(&self, matching: NameMatching, table_len: usize) -> usize {
        let hash = self
            .as_bytes()
            .iter()
            .fold(u32::from(self.len), |hash, &byte| {
                hash.wrapping_mul(13)
                    .wrapping_add(u32::from(fold(byte, matching)))
                    & 0x7FF
            });
        usize::try_from(hash).unwrap_or(0) % table_len
    }
}

/// Whether `byte` may stand in a name: printable ISO 8859-1, neither control
/// character nor separator.
const fn storable(byte: u8) -> bool {
    !matches!(byte, 0x00..=0x1F | 0x7F..=0x9F | b':' | b'/')
}

/// The capital `byte` is compared and hashed as.
fn fold(byte: u8, matching: NameMatching) -> u8 {
    u8::try_from(u32::from(matching.fold(char::from(byte)))).unwrap_or(byte)
}

/// ISO 8859-1 `bytes` in UTF-8, into `out`, answering its length.
fn decode_into(bytes: &[u8], out: &mut [u8]) -> usize {
    let mut len = 0;
    for &byte in bytes {
        len += char::from(byte).encode_utf8(&mut out[len..]).len();
    }
    len
}

/// ISO 8859-1 `bytes` as a UTF-8 string.
fn latin1_string(bytes: &[u8]) -> Result<String, DriverError> {
    let mut text = String::new();
    text.try_reserve(2 * bytes.len())
        .map_err(|_| DriverError::OutOfMemory)?;
    text.extend(bytes.iter().map(|&byte| char::from(byte)));
    Ok(text)
}

/// A soft link's stored AmigaDOS target in POSIX spelling, for the volume
/// named `volume`.
///
/// AmigaDOS spells a parent as an empty component — a leading `/`, or `//`
/// — and a volume's root as `:` after its name, or after nothing for the
/// volume the path is on. So `/a` is `../a`, `a//b` is `a/../b`, and `:a` and
/// `Volume:a` on this volume are `/a`, an absolute target resolving within
/// the projection that shows the link. A target on another volume or an
/// assign (`SYS:C/Dir`) names nothing here and keeps its stored spelling,
/// which resolves to nothing while no entry's recorded name holds a `:`, as
/// on every volume AmigaDOS writes.
pub(crate) fn link_target_to_posix(
    stored: &[u8],
    volume: &Name,
    matching: NameMatching,
) -> Result<String, DriverError> {
    let (absolute, path) = match stored.iter().position(|&b| b == b':') {
        Some(colon) => {
            let named = &stored[..colon];
            let here = named.is_empty()
                || Name::recorded(named).is_ok_and(|named| named.matches(volume, matching));
            if !here {
                return latin1_string(stored);
            }
            (true, &stored[colon + 1..])
        }
        None => (false, stored),
    };
    let mut out = String::new();
    // A stored byte spells at most three: an empty component's `..` and the
    // separator after it, or a Latin-1 letter's two UTF-8 bytes.
    out.try_reserve(3 * path.len() + 1)
        .map_err(|_| DriverError::OutOfMemory)?;
    if absolute {
        out.push('/');
    }
    let mut parts = path.split(|&byte| byte == b'/').peekable();
    let mut first = true;
    while let Some(part) = parts.next() {
        if part.is_empty() && parts.peek().is_none() {
            break;
        }
        if !first {
            out.push('/');
        }
        first = false;
        if part.is_empty() {
            out.push_str("..");
        } else {
            out.extend(part.iter().map(|&byte| char::from(byte)));
        }
    }
    if out.is_empty() {
        return Err(DriverError::BadMagic);
    }
    Ok(out)
}

/// A POSIX `target` in AmigaDOS's spelling, ISO 8859-1, to be stored in a
/// soft link: the inverse of [`link_target_to_posix`] for a target on this
/// volume.
///
/// # Errors
///
/// [`DriverError::OutOfRange`] for a target that is not UTF-8, holds a
/// character ISO 8859-1 lacks or a `:`, or is empty;
/// [`DriverError::LengthOutOfRange`] for one past `room` bytes.
pub(crate) fn link_target_to_amiga(target: &[u8], room: usize) -> Result<Vec<u8>, DriverError> {
    let text = core::str::from_utf8(target).map_err(|_| DriverError::OutOfRange)?;
    let (absolute, rest) = match text.strip_prefix('/') {
        Some(rest) => (true, rest),
        None => (false, text),
    };
    let mut out = Vec::new();
    let put = |out: &mut Vec<u8>, byte: u8| -> Result<(), DriverError> {
        if out.len() == room {
            return Err(DriverError::LengthOutOfRange);
        }
        if tairix_util::fallible::push(out, byte) {
            Ok(())
        } else {
            Err(DriverError::OutOfMemory)
        }
    };
    if absolute {
        put(&mut out, b':')?;
    }
    // What the last step put down: nothing yet, a name, or a parent's `/`.
    let mut after_name = false;
    for part in rest
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
    {
        if part == ".." {
            if after_name {
                put(&mut out, b'/')?;
            }
            put(&mut out, b'/')?;
            after_name = false;
            continue;
        }
        if after_name {
            put(&mut out, b'/')?;
        }
        for c in part.chars() {
            let byte = u8::try_from(u32::from(c)).map_err(|_| DriverError::OutOfRange)?;
            if byte == b':' || byte == 0 {
                return Err(DriverError::OutOfRange);
            }
            put(&mut out, byte)?;
        }
        after_name = true;
    }
    if out.is_empty() {
        return Err(DriverError::OutOfRange);
    }
    Ok(out)
}
