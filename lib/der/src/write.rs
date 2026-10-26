//! Writing DER.

use alloc::vec::Vec;

use tairix_abi::time::{CivilTime, Time64};

use crate::read::{bit_string_is_canonical, oid_is_canonical};
use crate::{DerError, Reader, Tag};

/// Builds one DER encoding.
///
/// A failed write latches: every later write is ignored and
/// [`Writer::finish`] reports the first failure, so a message is written with
/// plain calls and checked once.
#[derive(Debug, Default)]
pub struct Writer {
    out: Vec<u8>,
    failed: Option<DerError>,
}

impl Writer {
    /// An empty writer.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            out: Vec::new(),
            failed: None,
        }
    }

    /// The encoding, or the first failure.
    ///
    /// # Errors
    ///
    /// The first [`DerError`] any write latched.
    pub fn finish(self) -> Result<Vec<u8>, DerError> {
        match self.failed {
            None => Ok(self.out),
            Some(err) => Err(err),
        }
    }

    fn fail(&mut self, err: DerError) {
        if self.failed.is_none() {
            self.failed = Some(err);
        }
    }

    fn put(&mut self, bytes: &[u8]) {
        if self.failed.is_some() {
            return;
        }
        if self.out.try_reserve(bytes.len()).is_err() {
            self.fail(DerError::OutOfMemory);
            return;
        }
        self.out.extend_from_slice(bytes);
    }

    fn identifier(&mut self, tag: Tag) {
        let form = if tag.is_constructed() { 0x20 } else { 0 };
        let lead = tag.class().bits() | form;
        let number = tag.number();
        if number < 0x1f {
            // Below 31 the number is the identifier's low five bits.
            self.put(&[lead | number.to_le_bytes()[0]]);
            return;
        }
        let mut digits = [0u8; 6];
        digits[0] = lead | 0x1f;
        let mut count = 0;
        let mut rest = number;
        while rest > 0 {
            count += 1;
            rest >>= 7;
        }
        for index in 0..count {
            let shift = 7 * (count - 1 - index);
            let more = if index + 1 < count { 0x80 } else { 0 };
            digits[1 + index] = (number >> shift).to_le_bytes()[0] & 0x7f | more;
        }
        self.put(&digits[..=count]);
    }

    fn length(&mut self, len: usize) {
        match u32::try_from(len) {
            Ok(len) => {
                let (bytes, count) = length_octets(len);
                self.put(&bytes[..count]);
            }
            Err(_) => self.fail(DerError::TooLong),
        }
    }

    /// An element of `tag` with `contents`, written as they stand.
    pub fn element(&mut self, tag: Tag, contents: &[u8]) {
        self.identifier(tag);
        self.length(contents.len());
        self.put(contents);
    }

    /// A constructed element of `tag` whose contents `write` appends.
    pub fn constructed(&mut self, tag: Tag, write: impl FnOnce(&mut Self)) {
        self.identifier(tag);
        let start = self.out.len();
        write(self);
        if self.failed.is_some() {
            return;
        }
        let Ok(len) = u32::try_from(self.out.len() - start) else {
            self.fail(DerError::TooLong);
            return;
        };
        let (bytes, count) = length_octets(len);
        if self.out.try_reserve(count).is_err() {
            self.fail(DerError::OutOfMemory);
            return;
        }
        self.out
            .splice(start..start, bytes[..count].iter().copied());
    }

    /// A `SEQUENCE` whose elements `write` appends.
    pub fn sequence(&mut self, write: impl FnOnce(&mut Self)) {
        self.constructed(Tag::SEQUENCE, write);
    }

    /// The explicitly tagged `[number]` around what `write` appends.
    pub fn explicit(&mut self, number: u32, write: impl FnOnce(&mut Self)) {
        self.constructed(Tag::context(number), write);
    }

    /// An `INTEGER`, in its minimal two's-complement form.
    pub fn integer(&mut self, value: i64) {
        self.signed(Tag::INTEGER, value);
    }

    /// An `ENUMERATED`.
    pub fn enumerated(&mut self, value: i64) {
        self.signed(Tag::ENUMERATED, value);
    }

    fn signed(&mut self, tag: Tag, value: i64) {
        let bytes = value.to_be_bytes();
        // Drop each leading byte that only repeats the sign of the next.
        let skip = bytes
            .windows(2)
            .take_while(|pair| {
                (pair[0] == 0x00 && pair[1] & 0x80 == 0) || (pair[0] == 0xff && pair[1] & 0x80 != 0)
            })
            .count();
        self.element(tag, &bytes[skip..]);
    }

    /// A `BOOLEAN`.
    pub fn boolean(&mut self, value: bool) {
        self.element(Tag::BOOLEAN, &[if value { 0xff } else { 0x00 }]);
    }

    /// A `NULL`.
    pub fn null(&mut self) {
        self.element(Tag::NULL, &[]);
    }

    /// An `OCTET STRING`.
    pub fn octet_string(&mut self, bytes: &[u8]) {
        self.element(Tag::OCTET_STRING, bytes);
    }

    /// A `GeneralString`.
    pub fn general_string(&mut self, bytes: &[u8]) {
        self.element(Tag::GENERAL_STRING, bytes);
    }

    /// An `OBJECT IDENTIFIER` from its encoded arcs; arcs that are not
    /// canonical latch [`DerError::NonCanonical`].
    pub fn oid(&mut self, encoded: &[u8]) {
        if oid_is_canonical(encoded) {
            self.element(Tag::OBJECT_IDENTIFIER, encoded);
        } else {
            self.fail(DerError::NonCanonical);
        }
    }

    /// A `BIT STRING` of `bytes` whose last `unused` bits are padding, which
    /// must be zero; anything else latches [`DerError::NonCanonical`].
    pub fn bit_string(&mut self, unused: u8, bytes: &[u8]) {
        if !bit_string_is_canonical(unused, bytes) {
            self.fail(DerError::NonCanonical);
            return;
        }
        self.identifier(Tag::BIT_STRING);
        self.length(bytes.len().saturating_add(1));
        self.put(&[unused]);
        self.put(bytes);
    }

    /// A `GeneralizedTime` naming `time`, with a fraction of a second only
    /// when it has one. An instant outside years 0 to 9999 latches
    /// [`DerError::OutOfRange`].
    pub fn generalized_time(&mut self, time: Time64) {
        let civil = CivilTime::from_time64(time);
        let Ok(year) = u32::try_from(civil.year) else {
            self.fail(DerError::OutOfRange);
            return;
        };
        if year > 9999 {
            self.fail(DerError::OutOfRange);
            return;
        }
        let mut text = [0u8; 25];
        let mut len = 0;
        for (value, width) in [
            (year, 4),
            (civil.month, 2),
            (civil.day, 2),
            (civil.hour, 2),
            (civil.minute, 2),
            (civil.second, 2),
        ] {
            len += digits(value, width, &mut text[len..]);
        }
        let nanos = time.subsec_nanos();
        if nanos != 0 {
            text[len] = b'.';
            len += 1;
            let mut fraction = [0u8; 9];
            digits(nanos, 9, &mut fraction);
            let kept = 9 - fraction
                .iter()
                .rev()
                .take_while(|&&digit| digit == b'0')
                .count();
            text[len..len + kept].copy_from_slice(&fraction[..kept]);
            len += kept;
        }
        text[len] = b'Z';
        self.element(Tag::GENERALIZED_TIME, &text[..=len]);
    }

    /// An element encoded elsewhere — a ticket carried verbatim — which must
    /// be exactly one well-formed element, or [`DerError::NonCanonical`]
    /// latches.
    pub fn raw(&mut self, encoding: &[u8]) {
        let mut reader = Reader::new(encoding);
        if reader.tlv().is_ok() && reader.is_empty() {
            self.put(encoding);
        } else {
            self.fail(DerError::NonCanonical);
        }
    }
}

/// The definite length octets for `len`, and how many there are.
fn length_octets(len: u32) -> ([u8; 5], usize) {
    let mut out = [0u8; 5];
    if len < 0x80 {
        out[0] = len.to_le_bytes()[0];
        return (out, 1);
    }
    let bytes = len.to_be_bytes();
    let skip = bytes.iter().take_while(|&&byte| byte == 0).count();
    let count = 4 - skip;
    out[0] = 0x80 | u8::try_from(count).unwrap_or(4);
    out[1..=count].copy_from_slice(&bytes[skip..]);
    (out, count + 1)
}

/// Write `value` as `width` zero-padded ASCII digits into `out`.
fn digits(value: u32, width: usize, out: &mut [u8]) -> usize {
    let mut rest = value;
    for slot in out[..width].iter_mut().rev() {
        *slot = b'0' + (rest % 10).to_le_bytes()[0];
        rest /= 10;
    }
    width
}
