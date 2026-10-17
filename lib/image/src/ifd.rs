//! The directory structure a TIFF file and an EXIF block share: the byte
//! order, the twelve-byte entry, and where an entry's values lie.
//!
//! One reading of the layout, so the TIFF decoder, the EXIF orientation
//! reader and the structure report cannot disagree about where a value is.
//! The decoder reads a directory strictly ([`Ifd`]); a reader of advisory
//! metadata walks what a truncated block still holds, entry by entry
//! ([`entry`]).

use crate::DecodeError;

/// The version a classic TIFF header carries after its byte-order mark.
pub(crate) const MAGIC: u16 = 42;

/// A directory entry's fixed length: tag, type, count, and the value or its
/// offset.
pub(crate) const ENTRY_LEN: usize = 12;

/// The field types an entry's values are held in.
pub(crate) const BYTE: u16 = 1;
pub(crate) const ASCII: u16 = 2;
pub(crate) const SHORT: u16 = 3;
pub(crate) const LONG: u16 = 4;
pub(crate) const RATIONAL: u16 = 5;
pub(crate) const SBYTE: u16 = 6;
pub(crate) const UNDEFINED: u16 = 7;
pub(crate) const SSHORT: u16 = 8;
pub(crate) const SLONG: u16 = 9;
pub(crate) const SRATIONAL: u16 = 10;
pub(crate) const FLOAT: u16 = 11;
pub(crate) const DOUBLE: u16 = 12;

/// A file's byte order, which every multi-byte field in it is read in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Endian {
    Little,
    Big,
}

impl Endian {
    /// The byte order the mark opening `header` states, or `None` for
    /// neither `II` nor `MM`.
    pub(crate) fn of(header: &[u8]) -> Option<Self> {
        match header.first_chunk::<2>() {
            Some(b"II") => Some(Self::Little),
            Some(b"MM") => Some(Self::Big),
            _ => None,
        }
    }

    pub(crate) fn u16(self, bytes: &[u8], at: usize) -> Option<u16> {
        match self {
            Self::Little => crate::le_u16(bytes, at),
            Self::Big => crate::be_u16(bytes, at),
        }
    }

    pub(crate) fn u32(self, bytes: &[u8], at: usize) -> Option<u32> {
        match self {
            Self::Little => crate::le_u32(bytes, at),
            Self::Big => crate::be_u32(bytes, at),
        }
    }

    /// The eight bytes at `at`, read as one number in this order.
    pub(crate) fn u64(self, bytes: &[u8], at: usize) -> Option<u64> {
        let raw = *bytes.get(at..)?.first_chunk::<8>()?;
        Some(match self {
            Self::Little => u64::from_le_bytes(raw),
            Self::Big => u64::from_be_bytes(raw),
        })
    }
}

/// Bytes one element of a directory entry's field type occupies, or `None`
/// for a type this decoder has no reading of.
pub(crate) const fn type_size(kind: u16) -> Option<u32> {
    Some(match kind {
        BYTE | ASCII | SBYTE | UNDEFINED => 1,
        SHORT | SSHORT => 2,
        LONG | SLONG | FLOAT => 4,
        RATIONAL | SRATIONAL | DOUBLE => 8,
        _ => return None,
    })
}

/// One directory entry's values, wherever the entry put them.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Field {
    pub(crate) kind: u16,
    pub(crate) count: u32,
    pub(crate) at: usize,
}

/// One directory entry as it is stored: its tag, and its values when they
/// lie wholly within the file.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Entry {
    pub(crate) tag: u16,
    pub(crate) kind: u16,
    pub(crate) count: u32,
    /// Where the values are, or `None` for a type with no reading or values
    /// that run past the end of the file.
    pub(crate) values: Option<Field>,
}

/// The entry at `at` in `file`, or `None` when the entry itself runs past
/// the end.
pub(crate) fn entry(file: &[u8], endian: Endian, at: usize) -> Option<Entry> {
    let tag = endian.u16(file, at)?;
    let kind = endian.u16(file, at.checked_add(2)?)?;
    let count = endian.u32(file, at.checked_add(4)?)?;
    let inline = at.checked_add(8)?;
    endian.u32(file, inline)?;
    let values = type_size(kind).and_then(|size| {
        let bytes = u64::from(count) * u64::from(size);
        let start = if bytes <= 4 {
            inline
        } else {
            usize::try_from(endian.u32(file, inline)?).ok()?
        };
        let end = u64::try_from(start).ok()?.checked_add(bytes)?;
        (end <= file.len() as u64).then_some(Field {
            kind,
            count,
            at: start,
        })
    });
    Some(Entry {
        tag,
        kind,
        count,
        values,
    })
}

/// One image file directory, and the file it indexes into.
#[derive(Copy, Clone)]
pub(crate) struct Ifd<'a> {
    pub(crate) file: &'a [u8],
    pub(crate) endian: Endian,
    pub(crate) entries: usize,
    pub(crate) count: u16,
}

impl<'a> Ifd<'a> {
    /// Read the directory at `at`, answering it and the offset of the next.
    pub(crate) fn read(
        file: &'a [u8],
        endian: Endian,
        at: usize,
    ) -> Result<(Self, usize), DecodeError> {
        let count = endian.u16(file, at).ok_or(DecodeError::TiffTruncated)?;
        let entries = at.checked_add(2).ok_or(DecodeError::TiffTruncated)?;
        let span = usize::from(count)
            .checked_mul(ENTRY_LEN)
            .ok_or(DecodeError::TiffTruncated)?;
        let after = entries
            .checked_add(span)
            .ok_or(DecodeError::TiffTruncated)?;
        let next = endian.u32(file, after).ok_or(DecodeError::TiffTruncated)?;
        let next = usize::try_from(next).map_err(|_| DecodeError::TiffTruncated)?;
        Ok((
            Self {
                file,
                endian,
                entries,
                count,
            },
            next,
        ))
    }

    /// The entry for `tag`, or `None` where the directory carries none — or
    /// carries one whose type this decoder has no reading of, which the
    /// format asks a reader to pass over rather than refuse.
    pub(crate) fn field(&self, tag: u16) -> Result<Option<Field>, DecodeError> {
        Ok(self.fields([tag])?[0])
    }

    /// The entries for several tags, found in one pass.
    ///
    /// The chain walk reads a handful of tags from every page, so scanning
    /// once for all of them is what keeps that walk linear in the file
    /// rather than multiplying it by the tags wanted.
    pub(crate) fn fields<const N: usize>(
        &self,
        tags: [u16; N],
    ) -> Result<[Option<Field>; N], DecodeError> {
        let mut found = [None; N];
        for index in 0..usize::from(self.count) {
            let at = self.entries + index * ENTRY_LEN;
            let entry = self
                .endian
                .u16(self.file, at)
                .ok_or(DecodeError::TiffTruncated)?;
            let Some(slot) = tags
                .iter()
                .position(|tag| *tag == entry)
                .and_then(|slot| found.get_mut(slot))
            else {
                continue;
            };
            if slot.is_some() {
                continue;
            }
            *slot = self.read_field(at)?;
        }
        Ok(found)
    }

    /// The value span of the entry at `at`, or `None` for a field type this
    /// decoder has no reading of.
    fn read_field(&self, at: usize) -> Result<Option<Field>, DecodeError> {
        let read = entry(self.file, self.endian, at).ok_or(DecodeError::TiffTruncated)?;
        match (type_size(read.kind), read.values) {
            (None, _) => Ok(None),
            (Some(_), Some(values)) => Ok(Some(values)),
            (Some(_), None) => Err(DecodeError::TiffTruncated),
        }
    }

    /// Element `index` of `field`, read as an unsigned integer.
    pub(crate) fn integer(&self, field: &Field, index: u32) -> Result<u32, DecodeError> {
        if index >= field.count {
            return Err(DecodeError::TiffInvalidTagValue);
        }
        let index = usize::try_from(index).map_err(|_| DecodeError::TiffInvalidTagValue)?;
        match field.kind {
            BYTE | UNDEFINED => self
                .file
                .get(field.at + index)
                .map(|byte| u32::from(*byte))
                .ok_or(DecodeError::TiffTruncated),
            SHORT => self
                .endian
                .u16(self.file, field.at + index * 2)
                .map(u32::from)
                .ok_or(DecodeError::TiffTruncated),
            LONG => self
                .endian
                .u32(self.file, field.at + index * 4)
                .ok_or(DecodeError::TiffTruncated),
            _ => Err(DecodeError::TiffInvalidTagValue),
        }
    }

    /// Element `index` of a RATIONAL field, as its numerator and
    /// denominator.
    pub(crate) fn rational(&self, field: &Field, index: u32) -> Result<(u32, u32), DecodeError> {
        if field.kind != RATIONAL || index >= field.count {
            return Err(DecodeError::TiffInvalidTagValue);
        }
        let at = field.at + usize::try_from(index).unwrap_or(usize::MAX) * 8;
        let numerator = self
            .endian
            .u32(self.file, at)
            .ok_or(DecodeError::TiffTruncated)?;
        let denominator = self
            .endian
            .u32(self.file, at + 4)
            .ok_or(DecodeError::TiffTruncated)?;
        if denominator == 0 {
            return Err(DecodeError::TiffInvalidTagValue);
        }
        Ok((numerator, denominator))
    }

    /// A field's first value, or `fallback` where the directory carried no
    /// such field — refusing when there is no fallback and so no page.
    pub(crate) fn first(
        &self,
        field: Option<&Field>,
        fallback: Option<u32>,
    ) -> Result<u32, DecodeError> {
        match field {
            Some(field) if field.count > 0 => self.integer(field, 0),
            _ => fallback.ok_or(DecodeError::TiffMissingTag),
        }
    }

    /// `tag`'s first value, or `fallback` where the directory omits it.
    pub(crate) fn value(&self, tag: u16, fallback: u32) -> Result<u32, DecodeError> {
        match self.field(tag)? {
            Some(field) if field.count > 0 => self.integer(&field, 0),
            _ => Ok(fallback),
        }
    }

    /// `tag`'s first value, refusing a directory that omits a tag its page
    /// cannot be read without.
    pub(crate) fn required(&self, tag: u16) -> Result<u32, DecodeError> {
        let field = self.field(tag)?.ok_or(DecodeError::TiffMissingTag)?;
        if field.count == 0 {
            return Err(DecodeError::TiffMissingTag);
        }
        self.integer(&field, 0)
    }
}

#[cfg(test)]
#[path = "ifd_tests.rs"]
mod tests;
