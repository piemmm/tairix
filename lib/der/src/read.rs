//! Reading DER.

use tairix_abi::time::{CivilTime, Time64};

use crate::{Class, DerError, Tag, MAX_DEPTH};

/// One element: its identifier, its contents, and its whole encoding.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Tlv<'a> {
    /// The identifier.
    pub tag: Tag,
    /// The contents octets.
    pub contents: &'a [u8],
    /// Identifier, length and contents together, as they arrived.
    pub encoding: &'a [u8],
}

/// A `BIT STRING`'s value: the bits of `bytes` but the last `unused`.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct BitString<'a> {
    unused: u8,
    bytes: &'a [u8],
}

impl<'a> BitString<'a> {
    /// How many low bits of the last byte are padding, `0..=7`.
    #[must_use]
    pub const fn unused_bits(&self) -> u8 {
        self.unused
    }

    /// The bytes holding the bits, the first bit the high bit of the first.
    #[must_use]
    pub const fn bytes(&self) -> &'a [u8] {
        self.bytes
    }

    /// How many bits the string holds.
    #[must_use]
    pub const fn bit_len(&self) -> usize {
        self.bytes.len() * 8 - self.unused as usize
    }
}

/// Reads elements in order from a run of DER, borrowing every content.
///
/// A reader returned inside [`Reader::nested`] is one level deeper than its
/// parent; none descends past [`MAX_DEPTH`].
#[derive(Clone, Debug)]
pub struct Reader<'a> {
    rest: &'a [u8],
    depth: usize,
}

impl<'a> Reader<'a> {
    /// A reader over `input`, at the outermost level.
    #[must_use]
    pub const fn new(input: &'a [u8]) -> Self {
        Self {
            rest: input,
            depth: 0,
        }
    }

    /// Whether every element has been read.
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.rest.is_empty()
    }

    /// Succeed only if nothing is left: a structure is read whole.
    ///
    /// # Errors
    ///
    /// [`DerError::TrailingData`] if bytes remain.
    pub const fn finish(&self) -> Result<(), DerError> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(DerError::TrailingData)
        }
    }

    /// The next element's identifier, without consuming it; `None` when the
    /// reader is empty.
    ///
    /// # Errors
    ///
    /// Why the identifier is malformed.
    pub fn peek_tag(&self) -> Result<Option<Tag>, DerError> {
        if self.rest.is_empty() {
            return Ok(None);
        }
        identifier(self.rest).map(|(tag, _)| Some(tag))
    }

    /// The next element, whatever its identifier.
    ///
    /// # Errors
    ///
    /// Why the element is malformed or does not fit.
    pub fn tlv(&mut self) -> Result<Tlv<'a>, DerError> {
        let input = self.rest;
        let (tag, tag_len) = identifier(input)?;
        let (len, len_len) = length(&input[tag_len..])?;
        let header = tag_len + len_len;
        let end = header.checked_add(len).ok_or(DerError::Truncated)?;
        let encoding = input.get(..end).ok_or(DerError::Truncated)?;
        self.rest = &input[end..];
        Ok(Tlv {
            tag,
            contents: &encoding[header..],
            encoding,
        })
    }

    /// The contents of the next element, which must carry `tag`.
    ///
    /// # Errors
    ///
    /// [`DerError::UnexpectedTag`] for another identifier, or why the element
    /// is malformed.
    pub fn element(&mut self, tag: Tag) -> Result<&'a [u8], DerError> {
        let tlv = self.tlv()?;
        if tlv.tag == tag {
            Ok(tlv.contents)
        } else {
            Err(DerError::UnexpectedTag)
        }
    }

    /// The contents of the next element when it carries `tag`, else `None`
    /// with nothing consumed: an `OPTIONAL` field.
    ///
    /// # Errors
    ///
    /// Why the element is malformed.
    pub fn optional(&mut self, tag: Tag) -> Result<Option<&'a [u8]>, DerError> {
        if self.peek_tag()? == Some(tag) {
            self.element(tag).map(Some)
        } else {
            Ok(None)
        }
    }

    /// Read the constructed element carrying `tag` with `read`, which must
    /// consume its contents exactly.
    ///
    /// # Errors
    ///
    /// [`DerError::TooDeep`] past [`MAX_DEPTH`], [`DerError::TrailingData`]
    /// when `read` leaves contents unread, or whatever `read` refuses.
    pub fn nested<T>(
        &mut self,
        tag: Tag,
        read: impl FnOnce(&mut Reader<'a>) -> Result<T, DerError>,
    ) -> Result<T, DerError> {
        let contents = self.element(tag)?;
        let depth = self.depth + 1;
        if depth > MAX_DEPTH {
            return Err(DerError::TooDeep);
        }
        let mut inner = Reader {
            rest: contents,
            depth,
        };
        let value = read(&mut inner)?;
        inner.finish()?;
        Ok(value)
    }

    /// [`Reader::nested`] when the next element carries `tag`, else `None`.
    ///
    /// # Errors
    ///
    /// As [`Reader::nested`].
    pub fn nested_optional<T>(
        &mut self,
        tag: Tag,
        read: impl FnOnce(&mut Reader<'a>) -> Result<T, DerError>,
    ) -> Result<Option<T>, DerError> {
        if self.peek_tag()? == Some(tag) {
            self.nested(tag, read).map(Some)
        } else {
            Ok(None)
        }
    }

    /// A `SEQUENCE`, read by `read`.
    ///
    /// # Errors
    ///
    /// As [`Reader::nested`].
    pub fn sequence<T>(
        &mut self,
        read: impl FnOnce(&mut Reader<'a>) -> Result<T, DerError>,
    ) -> Result<T, DerError> {
        self.nested(Tag::SEQUENCE, read)
    }

    /// The explicitly tagged `[number]`, read by `read`.
    ///
    /// # Errors
    ///
    /// As [`Reader::nested`].
    pub fn explicit<T>(
        &mut self,
        number: u32,
        read: impl FnOnce(&mut Reader<'a>) -> Result<T, DerError>,
    ) -> Result<T, DerError> {
        self.nested(Tag::context(number), read)
    }

    /// The explicitly tagged `[number]` when present, read by `read`.
    ///
    /// # Errors
    ///
    /// As [`Reader::nested`].
    pub fn explicit_optional<T>(
        &mut self,
        number: u32,
        read: impl FnOnce(&mut Reader<'a>) -> Result<T, DerError>,
    ) -> Result<Option<T>, DerError> {
        self.nested_optional(Tag::context(number), read)
    }

    /// An `INTEGER` that fits an `i64`.
    ///
    /// # Errors
    ///
    /// [`DerError::NonCanonical`] for a padded or empty integer,
    /// [`DerError::OutOfRange`] for one wider than 64 bits.
    pub fn integer(&mut self) -> Result<i64, DerError> {
        integer_value(self.element(Tag::INTEGER)?)
    }

    /// An `ENUMERATED` that fits an `i64`.
    ///
    /// # Errors
    ///
    /// As [`Reader::integer`].
    pub fn enumerated(&mut self) -> Result<i64, DerError> {
        integer_value(self.element(Tag::ENUMERATED)?)
    }

    /// A `BOOLEAN`.
    ///
    /// # Errors
    ///
    /// [`DerError::NonCanonical`] unless the contents are one `00` or `FF`.
    pub fn boolean(&mut self) -> Result<bool, DerError> {
        match self.element(Tag::BOOLEAN)? {
            [0x00] => Ok(false),
            [0xff] => Ok(true),
            _ => Err(DerError::NonCanonical),
        }
    }

    /// A `NULL`.
    ///
    /// # Errors
    ///
    /// [`DerError::NonCanonical`] for any contents.
    pub fn null(&mut self) -> Result<(), DerError> {
        if self.element(Tag::NULL)?.is_empty() {
            Ok(())
        } else {
            Err(DerError::NonCanonical)
        }
    }

    /// An `OCTET STRING`'s contents. DER's strings are always primitive, so a
    /// constructed one is the wrong tag.
    ///
    /// # Errors
    ///
    /// Why the element is malformed or another.
    pub fn octet_string(&mut self) -> Result<&'a [u8], DerError> {
        self.element(Tag::OCTET_STRING)
    }

    /// A `GeneralString`'s contents, as bytes: what characters they may hold
    /// is the protocol's rule, not the encoding's.
    ///
    /// # Errors
    ///
    /// Why the element is malformed or another.
    pub fn general_string(&mut self) -> Result<&'a [u8], DerError> {
        self.element(Tag::GENERAL_STRING)
    }

    /// An `OBJECT IDENTIFIER`'s contents: the encoded arcs, compared as bytes.
    ///
    /// # Errors
    ///
    /// [`DerError::NonCanonical`] for an empty identifier, a padded arc, or
    /// one left unterminated.
    pub fn oid(&mut self) -> Result<&'a [u8], DerError> {
        let contents = self.element(Tag::OBJECT_IDENTIFIER)?;
        if oid_is_canonical(contents) {
            Ok(contents)
        } else {
            Err(DerError::NonCanonical)
        }
    }

    /// A `BIT STRING`.
    ///
    /// # Errors
    ///
    /// [`DerError::NonCanonical`] for a padding count past 7, padding in an
    /// empty string, or padding bits that are not zero.
    pub fn bit_string(&mut self) -> Result<BitString<'a>, DerError> {
        let (&unused, bytes) = self
            .element(Tag::BIT_STRING)?
            .split_first()
            .ok_or(DerError::NonCanonical)?;
        if bit_string_is_canonical(unused, bytes) {
            Ok(BitString { unused, bytes })
        } else {
            Err(DerError::NonCanonical)
        }
    }

    /// A `GeneralizedTime`, as the instant it names: `YYYYMMDDHHMMSS`, an
    /// optional fraction of a second with no trailing zero, and `Z`.
    ///
    /// # Errors
    ///
    /// [`DerError::NonCanonical`] for any other form, or a date or time the
    /// calendar cannot hold.
    pub fn generalized_time(&mut self) -> Result<Time64, DerError> {
        parse_time(self.element(Tag::GENERALIZED_TIME)?).ok_or(DerError::NonCanonical)
    }
}

/// An identifier and how many bytes it took.
fn identifier(input: &[u8]) -> Result<(Tag, usize), DerError> {
    let first = *input.first().ok_or(DerError::Truncated)?;
    let class = Class::from_bits(first);
    let constructed = first & 0x20 != 0;
    if first & 0x1f != 0x1f {
        return Ok((Tag::new(class, constructed, u32::from(first & 0x1f)), 1));
    }
    let mut number = 0u32;
    for (index, &byte) in input[1..].iter().enumerate() {
        if index == 0 && byte == 0x80 {
            return Err(DerError::NonCanonical);
        }
        number = number
            .checked_mul(0x80)
            .map(|n| n | u32::from(byte & 0x7f))
            .ok_or(DerError::OutOfRange)?;
        if byte & 0x80 == 0 {
            // A number below 31 has the one-byte form.
            if number < 0x1f {
                return Err(DerError::NonCanonical);
            }
            return Ok((Tag::new(class, constructed, number), index + 2));
        }
    }
    Err(DerError::Truncated)
}

/// A definite length and how many bytes it took.
fn length(input: &[u8]) -> Result<(usize, usize), DerError> {
    let first = *input.first().ok_or(DerError::Truncated)?;
    if first < 0x80 {
        return Ok((usize::from(first), 1));
    }
    if first == 0x80 {
        return Err(DerError::Indefinite);
    }
    let count = usize::from(first & 0x7f);
    if count > 4 {
        return Err(DerError::TooLong);
    }
    let digits = input.get(1..=count).ok_or(DerError::Truncated)?;
    if digits[0] == 0 {
        return Err(DerError::NonCanonical);
    }
    let len = digits
        .iter()
        .fold(0u32, |acc, &byte| (acc << 8) | u32::from(byte));
    if len < 0x80 {
        return Err(DerError::NonCanonical);
    }
    let len = usize::try_from(len).map_err(|_| DerError::TooLong)?;
    Ok((len, 1 + count))
}

/// The value of an `INTEGER` or `ENUMERATED`'s contents.
fn integer_value(contents: &[u8]) -> Result<i64, DerError> {
    let (&first, rest) = contents.split_first().ok_or(DerError::NonCanonical)?;
    if let Some(&second) = rest.first() {
        let padded = (first == 0x00 && second & 0x80 == 0) || (first == 0xff && second & 0x80 != 0);
        if padded {
            return Err(DerError::NonCanonical);
        }
    }
    if contents.len() > 8 {
        return Err(DerError::OutOfRange);
    }
    let seed = if first & 0x80 == 0 { 0 } else { -1 };
    Ok(contents
        .iter()
        .fold(seed, |acc: i64, &byte| (acc << 8) | i64::from(byte)))
}

/// Whether `contents` are canonically encoded arcs.
pub(crate) fn oid_is_canonical(contents: &[u8]) -> bool {
    let Some(&last) = contents.last() else {
        return false;
    };
    let mut arc_start = true;
    for &byte in contents {
        if arc_start && byte == 0x80 {
            return false;
        }
        arc_start = byte & 0x80 == 0;
    }
    last & 0x80 == 0
}

/// Whether a bit string of `bytes` with `unused` padding bits is canonical.
pub(crate) fn bit_string_is_canonical(unused: u8, bytes: &[u8]) -> bool {
    match bytes.last() {
        None => unused == 0,
        Some(&last) => unused <= 7 && last & ((1u8 << unused) - 1) == 0,
    }
}

/// The instant a `GeneralizedTime`'s contents name.
fn parse_time(contents: &[u8]) -> Option<Time64> {
    let Some((&b'Z', body)) = contents.split_last() else {
        return None;
    };
    let (whole, fraction) = match body.iter().position(|&byte| byte == b'.') {
        Some(dot) => (&body[..dot], Some(&body[dot + 1..])),
        None => (body, None),
    };
    if whole.len() != 14 {
        return None;
    }
    let field = |range: core::ops::Range<usize>| decimal(&whole[range]);
    let civil = CivilTime {
        year: i64::from(field(0..4)?),
        month: field(4..6)?,
        day: field(6..8)?,
        hour: field(8..10)?,
        minute: field(10..12)?,
        second: field(12..14)?,
    };
    let nanos = match fraction {
        None => 0,
        Some(digits) => {
            if digits.is_empty() || digits.len() > 9 || digits.last() == Some(&b'0') {
                return None;
            }
            decimal(digits)? * 10u32.pow(9 - u32::try_from(digits.len()).ok()?)
        }
    };
    Time64::new(civil.to_time64()?.secs(), nanos).ok()
}

/// The value of a run of ASCII digits short enough for a `u32`.
fn decimal(digits: &[u8]) -> Option<u32> {
    digits.iter().try_fold(0u32, |acc, &byte| {
        byte.is_ascii_digit()
            .then(|| acc * 10 + u32::from(byte - b'0'))
    })
}
