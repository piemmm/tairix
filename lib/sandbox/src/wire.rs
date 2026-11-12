//! Bounded, fail-closed cursor encoding for the sandbox protocol payloads.
//!
//! Every payload that crosses the sandbox boundary is encoded and decoded
//! through these two cursors. The reader treats its input as hostile — a
//! reply comes from a worker that has parsed attacker-controlled bytes and
//! may itself be compromised — so every read is bounds-checked against the
//! remaining input and every variable-length field is checked against the
//! caller's stated cap *before* any bytes are copied. A short, oversize,
//! or malformed field is the typed [`WireError`], never a panic and never
//! a partial trust of later bytes.
//!
//! Scalars are little-endian, like every TAIRiX wire format.

use alloc::vec::Vec;
use core::marker::PhantomData;

/// Typed decode failure: the input ended early, or a variable-length field
/// exceeded its fixed cap. The two cases are deliberately distinguished so
/// a test can assert *why* a hostile payload was refused.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum WireError {
    /// Fewer bytes remained than the field requires.
    Truncated,
    /// A length prefix exceeded the field's fixed cap, or a string field
    /// held invalid UTF-8.
    Malformed,
}

/// Bytes a length-prefixed byte string's length takes ahead of it.
pub const BYTES_PREFIX: usize = 4;

/// The length a byte string of `len` bytes is prefixed with.
#[must_use]
pub fn bytes_prefix(len: usize) -> [u8; BYTES_PREFIX] {
    // Payload fields are bounded well below `u32::MAX` by the frame cap; a
    // longer one cannot reach here through the public encoders, so
    // saturating keeps the encoder total without an unchecked cast.
    u32::try_from(len).unwrap_or(u32::MAX).to_le_bytes()
}

/// How a payload encoder takes the room its payload grows into.
pub trait Room: sealed::Sealed {
    /// Room in `out` for `more` bytes past its length, answering whether it
    /// was had.
    fn make(out: &mut Vec<u8>, more: usize) -> bool;
}

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::Grows {}
    impl Sealed for super::Refusable {}
}

/// Room taken as the payload needs it: a worker's reply, the worker running
/// out of memory as a process its parent contains.
#[derive(Debug, Default)]
pub struct Grows;

impl Room for Grows {
    fn make(out: &mut Vec<u8>, more: usize) -> bool {
        out.reserve(more);
        true
    }
}

/// Room asked of the heap, a payload refused whole once any of it is not
/// had: a parent's request, the parent answering a refused heap rather than
/// ending.
#[derive(Debug, Default)]
pub struct Refusable;

impl Room for Refusable {
    fn make(out: &mut Vec<u8>, more: usize) -> bool {
        out.try_reserve(more).is_ok()
    }
}

/// Append-only payload encoder over a growable byte vector: a worker's reply
/// ([`Writer::new`]) or what a parent encodes ([`Writer::refusable`]).
#[derive(Debug, Default)]
pub struct Writer<R: Room = Grows> {
    out: Vec<u8>,
    refused: bool,
    room: PhantomData<R>,
}

impl Writer {
    /// Start an empty reply.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Start an empty reply that will hold `bytes` without growing.
    #[must_use]
    pub fn with_capacity(bytes: usize) -> Self {
        Self {
            out: Vec::with_capacity(bytes),
            ..Self::default()
        }
    }

    /// [`with_capacity`](Self::with_capacity), or `None` where the allocator
    /// refuses the room — for a payload sized by what it answers.
    #[must_use]
    pub fn try_with_capacity(bytes: usize) -> Option<Self> {
        let mut out = Vec::new();
        out.try_reserve_exact(bytes).ok()?;
        Some(Self {
            out,
            ..Self::default()
        })
    }

    /// Start an empty payload in `buffer`, keeping its capacity, so a
    /// sender that encodes frame after frame allocates once.
    #[must_use]
    pub fn reusing(mut buffer: Vec<u8>) -> Self {
        buffer.clear();
        Self {
            out: buffer,
            ..Self::default()
        }
    }

    /// Finish, yielding the encoded payload.
    #[must_use]
    pub fn finish(self) -> Vec<u8> {
        self.out
    }
}

impl Writer<Refusable> {
    /// Start an empty payload whose every byte is asked of the heap.
    #[must_use]
    pub const fn refusable() -> Self {
        Self {
            out: Vec::new(),
            refused: false,
            room: PhantomData,
        }
    }

    /// [`refusable`](Self::refusable), in `buffer`, keeping its room, so a
    /// client that asks again and again allocates once.
    #[must_use]
    pub fn refusable_in(mut buffer: Vec<u8>) -> Self {
        buffer.clear();
        Self {
            out: buffer,
            refused: false,
            room: PhantomData,
        }
    }

    /// The payload, or `None` where the heap refused room for any of it.
    #[must_use]
    pub fn finish(self) -> Option<Vec<u8>> {
        (!self.refused).then_some(self.out)
    }
}

impl<R: Room> Extend<u8> for Writer<R> {
    fn extend<I: IntoIterator<Item = u8>>(&mut self, bytes: I) {
        for byte in bytes {
            self.u8(byte);
        }
    }
}

impl<R: Room> Writer<R> {
    /// Append `bytes` as they are: a fixed-width field, or a payload whose
    /// length the frame's own states.
    pub fn raw(&mut self, bytes: &[u8]) {
        if !self.refused && R::make(&mut self.out, bytes.len()) {
            self.out.extend_from_slice(bytes);
        } else {
            self.refused = true;
        }
    }

    /// Append one byte.
    pub fn u8(&mut self, value: u8) {
        self.raw(&[value]);
    }

    /// Append a little-endian `u16`.
    pub fn u16(&mut self, value: u16) {
        self.raw(&value.to_le_bytes());
    }

    /// Append a little-endian `u32`.
    pub fn u32(&mut self, value: u32) {
        self.raw(&value.to_le_bytes());
    }

    /// Append a little-endian `i32`, two's complement.
    pub fn i32(&mut self, value: i32) {
        self.u32(value.cast_unsigned());
    }

    /// Append a little-endian `u64`.
    pub fn u64(&mut self, value: u64) {
        self.raw(&value.to_le_bytes());
    }

    /// Append a length-prefixed byte string ([`bytes_prefix`], then the
    /// bytes).
    pub fn bytes(&mut self, value: &[u8]) {
        self.raw(&bytes_prefix(value.len()));
        self.raw(value);
    }

    /// Append a length-prefixed UTF-8 string.
    pub fn str(&mut self, value: &str) {
        self.bytes(value.as_bytes());
    }
}

/// Bounds-checked payload decoder over a borrowed byte slice.
#[derive(Debug)]
pub struct Reader<'a> {
    input: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    /// Start reading `input` from its first byte.
    #[must_use]
    pub fn new(input: &'a [u8]) -> Self {
        Self { input, at: 0 }
    }

    /// Whether every input byte has been consumed.
    ///
    /// Decoders check this last, so a payload with trailing bytes —
    /// a shape no honest encoder produces — is refused rather than
    /// silently accepted.
    #[must_use]
    pub fn is_exhausted(&self) -> bool {
        self.at == self.input.len()
    }

    /// Bytes not yet consumed: what bounds how many items a declared count
    /// can honestly name.
    #[must_use]
    pub fn remaining(&self) -> usize {
        self.input.len() - self.at
    }

    /// Read the rest of a payload with `fields`, refusing one with bytes left
    /// over.
    ///
    /// # Errors
    ///
    /// What `fields` refuses, or [`WireError::Malformed`] for a payload
    /// longer than its fields.
    pub fn whole<T>(
        &mut self,
        fields: impl FnOnce(&mut Self) -> Result<T, WireError>,
    ) -> Result<T, WireError> {
        let value = fields(self)?;
        if self.is_exhausted() {
            Ok(value)
        } else {
            Err(WireError::Malformed)
        }
    }

    /// Consume `len` raw bytes.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] when fewer than `len` bytes remain.
    pub fn take(&mut self, len: usize) -> Result<&'a [u8], WireError> {
        let end = self.at.checked_add(len).ok_or(WireError::Truncated)?;
        if end > self.input.len() {
            return Err(WireError::Truncated);
        }
        let slice = &self.input[self.at..end];
        self.at = end;
        Ok(slice)
    }

    /// Consume one byte.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] when the input is exhausted.
    pub fn u8(&mut self) -> Result<u8, WireError> {
        Ok(self.take(1)?[0])
    }

    /// Consume a little-endian `u16`.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] when fewer than two bytes remain.
    pub fn u16(&mut self) -> Result<u16, WireError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    /// Consume a little-endian `u32`.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] when fewer than four bytes remain.
    pub fn u32(&mut self) -> Result<u32, WireError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }

    /// Consume a little-endian `i32`, two's complement.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] when fewer than four bytes remain.
    pub fn i32(&mut self) -> Result<i32, WireError> {
        Ok(self.u32()?.cast_signed())
    }

    /// Consume a little-endian `u64`.
    ///
    /// # Errors
    ///
    /// [`WireError::Truncated`] when fewer than eight bytes remain.
    pub fn u64(&mut self) -> Result<u64, WireError> {
        let bytes = self.take(8)?;
        Ok(u64::from_le_bytes([
            bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
        ]))
    }

    /// Consume a length-prefixed byte string of at most `cap` bytes.
    ///
    /// # Errors
    ///
    /// [`WireError::Malformed`] when the length prefix exceeds `cap`;
    /// [`WireError::Truncated`] when the declared bytes are not all
    /// present. The cap is checked before any bytes are consumed, so a
    /// hostile length can never drive a large read or allocation.
    pub fn bytes(&mut self, cap: usize) -> Result<&'a [u8], WireError> {
        let len = self.u32()? as usize;
        if len > cap {
            return Err(WireError::Malformed);
        }
        self.take(len)
    }

    /// Consume a length-prefixed UTF-8 string of at most `cap` bytes, as it
    /// lies in the payload.
    ///
    /// # Errors
    ///
    /// As [`Reader::bytes`], plus [`WireError::Malformed`] for invalid
    /// UTF-8.
    pub fn str(&mut self, cap: usize) -> Result<&'a str, WireError> {
        let bytes = self.bytes(cap)?;
        core::str::from_utf8(bytes).map_err(|_| WireError::Malformed)
    }
}

#[cfg(test)]
mod tests {
    use super::{Reader, WireError, Writer};
    use alloc::vec;

    #[test]
    fn round_trips_every_scalar_and_field_shape() {
        let mut w = Writer::new();
        w.u8(0xAB);
        w.u16(0xC0DE);
        w.u32(0xDEAD_BEEF);
        w.u64(0x0123_4567_89AB_CDEF);
        w.bytes(b"raw");
        w.str("text");
        let payload = w.finish();

        let mut r = Reader::new(&payload);
        assert_eq!(r.remaining(), payload.len());
        assert_eq!(r.u8(), Ok(0xAB));
        assert_eq!(r.u16(), Ok(0xC0DE));
        assert_eq!(r.u32(), Ok(0xDEAD_BEEF));
        assert_eq!(r.u64(), Ok(0x0123_4567_89AB_CDEF));
        assert_eq!(r.bytes(16), Ok(&b"raw"[..]));
        assert_eq!(r.str(16), Ok("text"));
        assert!(r.is_exhausted());
    }

    #[test]
    fn a_refusable_payload_is_built_whole_or_answered_as_refused() {
        let build = || {
            let mut w = Writer::refusable();
            w.u8(7);
            w.str("a name");
            w.u64(9);
            w.finish()
        };
        let whole = build().expect("room to spare");
        let mut r = Reader::new(&whole);
        assert_eq!((r.u8(), r.str(16), r.u64()), (Ok(7), Ok("a name"), Ok(9)));
        for granted in 0..2 {
            let refused = tairix_fuzzseed::meter::refusing_after(granted, build);
            assert_eq!(
                refused, None,
                "never a request cut short, refused at {granted}"
            );
        }
        let mut reused = Writer::refusable_in(vec![1; 8]);
        reused.u8(2);
        assert_eq!(reused.finish(), Some(vec![2]), "kept room, nothing it held");
    }

    #[test]
    fn a_reused_writer_starts_empty_and_appends_raw_bytes_unprefixed() {
        let mut w = Writer::reusing(vec![9u8; 32]);
        w.raw(b"ab");
        w.u8(1);
        assert_eq!(w.finish(), b"ab\x01");
    }

    #[test]
    fn every_truncation_point_is_refused() {
        let mut w = Writer::new();
        w.u32(7);
        w.str("seven");
        let payload = w.finish();
        // Every proper prefix fails closed; only the full payload decodes.
        for cut in 0..payload.len() {
            let mut r = Reader::new(&payload[..cut]);
            let outcome = r.u32().and_then(|_| r.str(16).map(|_| ()));
            assert!(outcome.is_err(), "prefix {cut} must be refused");
        }
    }

    #[test]
    fn a_length_prefix_over_the_cap_is_malformed_before_any_read() {
        let mut w = Writer::new();
        w.bytes(&[0u8; 32]);
        let payload = w.finish();
        let mut r = Reader::new(&payload);
        assert_eq!(r.bytes(31), Err(WireError::Malformed));
    }

    #[test]
    fn a_hostile_length_prefix_cannot_demand_a_huge_read() {
        // A four-byte payload claiming u32::MAX following bytes: the cap
        // check refuses it before any allocation or read.
        let payload = u32::MAX.to_le_bytes();
        let mut r = Reader::new(&payload);
        assert_eq!(r.bytes(64), Err(WireError::Malformed));
    }

    #[test]
    fn invalid_utf8_in_a_string_field_is_malformed() {
        let mut w = Writer::new();
        w.bytes(&[0xFF, 0xFE]);
        let payload = w.finish();
        let mut r = Reader::new(&payload);
        assert_eq!(r.str(16), Err(WireError::Malformed));
    }

    #[test]
    fn trailing_bytes_are_visible_to_the_exhaustion_check() {
        let payload = vec![0u8; 3];
        let mut r = Reader::new(&payload);
        assert_eq!(r.u8(), Ok(0));
        assert!(!r.is_exhausted());
    }
}
