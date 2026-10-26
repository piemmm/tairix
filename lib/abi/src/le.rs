//! Little-endian scalar read/write helpers shared across the ABI surface.
//!
//! All TAIRiX wire formats are little-endian (every Tier-1 target is
//! little-endian, and the explicit encoding lets a future big-endian port
//! participate without breaking the ABI). The same per-field index
//! arithmetic was previously open-coded in more than one module; it lives
//! here once so that the encoders and decoders cannot drift apart
//! (no duplication).
//!
//! The helpers are `pub(crate)`: they are an implementation detail of the
//! `abi` crate, not part of the frozen public surface. The indexed `read_*`
//! and `put_*` helpers serve fixed-size records whose decoder bounds-checks
//! the slice up front against its `WIRE_LEN`, so they index directly; a
//! variable-length frame is walked with [`Reader`] and built with
//! [`Writer`], which check every access themselves. None allocates.

use crate::Errno;

#[inline]
pub(crate) fn read_u16(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

#[inline]
pub(crate) fn read_u32(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

#[inline]
pub(crate) fn read_u64(bytes: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

#[inline]
pub(crate) fn read_i32(bytes: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
    ])
}

#[inline]
pub(crate) fn read_i64(bytes: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes([
        bytes[offset],
        bytes[offset + 1],
        bytes[offset + 2],
        bytes[offset + 3],
        bytes[offset + 4],
        bytes[offset + 5],
        bytes[offset + 6],
        bytes[offset + 7],
    ])
}

#[inline]
pub(crate) const fn put_u16(bytes: &mut [u8], offset: usize, value: u16) {
    let v = value.to_le_bytes();
    bytes[offset] = v[0];
    bytes[offset + 1] = v[1];
}

#[inline]
pub(crate) const fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    let v = value.to_le_bytes();
    bytes[offset] = v[0];
    bytes[offset + 1] = v[1];
    bytes[offset + 2] = v[2];
    bytes[offset + 3] = v[3];
}

#[inline]
pub(crate) fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
    let v = value.to_le_bytes();
    bytes[offset] = v[0];
    bytes[offset + 1] = v[1];
    bytes[offset + 2] = v[2];
    bytes[offset + 3] = v[3];
    bytes[offset + 4] = v[4];
    bytes[offset + 5] = v[5];
    bytes[offset + 6] = v[6];
    bytes[offset + 7] = v[7];
}

#[inline]
pub(crate) fn put_i32(bytes: &mut [u8], offset: usize, value: i32) {
    let v = value.to_le_bytes();
    bytes[offset] = v[0];
    bytes[offset + 1] = v[1];
    bytes[offset + 2] = v[2];
    bytes[offset + 3] = v[3];
}

#[inline]
pub(crate) fn put_i64(bytes: &mut [u8], offset: usize, value: i64) {
    let v = value.to_le_bytes();
    bytes[offset] = v[0];
    bytes[offset + 1] = v[1];
    bytes[offset + 2] = v[2];
    bytes[offset + 3] = v[3];
    bytes[offset + 4] = v[4];
    bytes[offset + 5] = v[5];
    bytes[offset + 6] = v[6];
    bytes[offset + 7] = v[7];
}

/// Refuse `bytes` unless it is exactly `len` long: short is truncation, and
/// long a field smuggled past the frame's end.
pub(crate) const fn exact_len(bytes: &[u8], len: usize) -> Result<(), Errno> {
    if bytes.len() < len {
        Err(Errno::BufferTooSmall)
    } else if bytes.len() > len {
        Err(Errno::LengthOutOfRange)
    } else {
        Ok(())
    }
}

/// A fail-closed reader over a variable-length frame: every accessor yields
/// a value or an [`Errno`], and nothing is read past the frame's end.
///
/// A short frame is [`Errno::LengthOutOfRange`] and a string that is not
/// UTF-8 is [`Errno::OutOfRange`].
pub(crate) struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
}

impl<'a> Reader<'a> {
    pub(crate) const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, at: 0 }
    }

    /// A reader starting `at` bytes in, past a fixed header its caller read.
    pub(crate) const fn starting_at(bytes: &'a [u8], at: usize) -> Self {
        Self { bytes, at }
    }

    pub(crate) fn take(&mut self, n: usize) -> Result<&'a [u8], Errno> {
        let end = self.at.checked_add(n).ok_or(Errno::LengthOutOfRange)?;
        let out = self
            .bytes
            .get(self.at..end)
            .ok_or(Errno::LengthOutOfRange)?;
        self.at = end;
        Ok(out)
    }

    fn array<const N: usize>(&mut self) -> Result<[u8; N], Errno> {
        let mut out = [0u8; N];
        out.copy_from_slice(self.take(N)?);
        Ok(out)
    }

    pub(crate) fn u8(&mut self) -> Result<u8, Errno> {
        Ok(self.take(1)?[0])
    }

    pub(crate) fn u16(&mut self) -> Result<u16, Errno> {
        self.array().map(u16::from_le_bytes)
    }

    pub(crate) fn u32(&mut self) -> Result<u32, Errno> {
        self.array().map(u32::from_le_bytes)
    }

    pub(crate) fn u64(&mut self) -> Result<u64, Errno> {
        self.array().map(u64::from_le_bytes)
    }

    pub(crate) fn i32(&mut self) -> Result<i32, Errno> {
        self.array().map(i32::from_le_bytes)
    }

    /// A `u16`-length-prefixed byte string.
    pub(crate) fn bytes16(&mut self) -> Result<&'a [u8], Errno> {
        let len = usize::from(self.u16()?);
        self.take(len)
    }

    /// A `u16`-length-prefixed UTF-8 string.
    pub(crate) fn str(&mut self) -> Result<&'a str, Errno> {
        core::str::from_utf8(self.bytes16()?).map_err(|_| Errno::OutOfRange)
    }

    pub(crate) const fn position(&self) -> usize {
        self.at
    }

    /// The bytes read since `from`, a position this reader reported.
    pub(crate) fn since(&self, from: usize) -> Result<&'a [u8], Errno> {
        self.bytes.get(from..self.at).ok_or(Errno::OutOfRange)
    }

    /// Whether every byte has been read: a frame is exactly one record, so
    /// trailing bytes are refused.
    pub(crate) const fn exhausted(&self) -> bool {
        self.at == self.bytes.len()
    }
}

/// A fail-closed writer over a caller's buffer: every append fits or reports
/// [`Errno::BufferTooSmall`], and nothing is written past the buffer's end.
pub(crate) struct Writer<'a> {
    out: &'a mut [u8],
    at: usize,
}

impl<'a> Writer<'a> {
    pub(crate) fn new(out: &'a mut [u8]) -> Self {
        Self { out, at: 0 }
    }

    pub(crate) fn bytes(&mut self, bytes: &[u8]) -> Result<(), Errno> {
        let end = self
            .at
            .checked_add(bytes.len())
            .ok_or(Errno::BufferTooSmall)?;
        self.out
            .get_mut(self.at..end)
            .ok_or(Errno::BufferTooSmall)?
            .copy_from_slice(bytes);
        self.at = end;
        Ok(())
    }

    pub(crate) fn u8(&mut self, v: u8) -> Result<(), Errno> {
        self.bytes(&[v])
    }

    pub(crate) fn u16(&mut self, v: u16) -> Result<(), Errno> {
        self.bytes(&v.to_le_bytes())
    }

    pub(crate) fn u32(&mut self, v: u32) -> Result<(), Errno> {
        self.bytes(&v.to_le_bytes())
    }

    pub(crate) fn u64(&mut self, v: u64) -> Result<(), Errno> {
        self.bytes(&v.to_le_bytes())
    }

    pub(crate) fn i32(&mut self, v: i32) -> Result<(), Errno> {
        self.bytes(&v.to_le_bytes())
    }

    /// A `u16`-length-prefixed byte string; one longer than a `u16` can
    /// count is [`Errno::LengthOutOfRange`].
    pub(crate) fn bytes16(&mut self, bytes: &[u8]) -> Result<(), Errno> {
        let len = u16::try_from(bytes.len()).map_err(|_| Errno::LengthOutOfRange)?;
        self.u16(len)?;
        self.bytes(bytes)
    }

    /// A `u16`-length-prefixed UTF-8 string.
    pub(crate) fn str(&mut self, s: &str) -> Result<(), Errno> {
        self.bytes16(s.as_bytes())
    }

    /// Overwrite the byte at `offset`, a field this writer already wrote
    /// whose value is known only once the frame is complete. The offset is
    /// the caller's to keep in bounds, as with [`put_u16`].
    pub(crate) fn patch_u8(&mut self, offset: usize, v: u8) {
        debug_assert!(offset < self.at);
        self.out[offset] = v;
    }

    /// The `u16` form of [`Self::patch_u8`].
    pub(crate) fn patch_u16(&mut self, offset: usize, v: u16) {
        debug_assert!(offset + 2 <= self.at);
        put_u16(self.out, offset, v);
    }

    pub(crate) const fn written(&self) -> usize {
        self.at
    }

    /// How many more bytes fit.
    pub(crate) const fn room(&self) -> usize {
        self.out.len() - self.at
    }
}

#[cfg(test)]
mod tests {
    use super::{
        exact_len, put_i32, put_i64, put_u16, put_u32, put_u64, read_i32, read_i64, read_u16,
        read_u32, read_u64, Reader, Writer,
    };
    use crate::Errno;

    #[test]
    fn a_frame_written_reads_back_field_by_field() {
        let mut buf = [0u8; 64];
        let len = {
            let mut w = Writer::new(&mut buf);
            w.u8(7).unwrap();
            w.u16(0xBEEF).unwrap();
            w.u32(0xDEAD_BEEF).unwrap();
            w.u64(u64::MAX - 1).unwrap();
            w.i32(-3).unwrap();
            w.str("name").unwrap();
            w.bytes16(&[0, 0xFF]).unwrap();
            w.written()
        };
        let mut r = Reader::new(&buf[..len]);
        assert_eq!(r.u8(), Ok(7));
        assert_eq!(r.u16(), Ok(0xBEEF));
        assert_eq!(r.u32(), Ok(0xDEAD_BEEF));
        assert_eq!(r.u64(), Ok(u64::MAX - 1));
        assert_eq!(r.i32(), Ok(-3));
        assert_eq!(r.str(), Ok("name"));
        assert_eq!(r.bytes16(), Ok(&[0, 0xFF][..]));
        assert!(r.exhausted());
    }

    #[test]
    fn a_short_frame_is_refused_and_reads_nothing() {
        let mut r = Reader::new(&[1, 2, 3]);
        assert_eq!(r.u32(), Err(Errno::LengthOutOfRange));
        assert_eq!(r.position(), 0);
        assert_eq!(
            Reader::new(&[5, 0, b'a']).bytes16(),
            Err(Errno::LengthOutOfRange)
        );
        assert_eq!(Reader::new(&[1, 0, 0xFF]).str(), Err(Errno::OutOfRange));
    }

    #[test]
    fn a_full_buffer_refuses_the_append_whole() {
        let mut buf = [0u8; 3];
        let mut w = Writer::new(&mut buf);
        assert_eq!(w.u32(1), Err(Errno::BufferTooSmall));
        assert_eq!(w.written(), 0);
        assert_eq!(w.u16(9), Ok(()));
        assert_eq!(w.bytes16(&[1]), Err(Errno::BufferTooSmall));
    }

    #[test]
    fn a_patch_rewrites_a_field_already_written() {
        let mut buf = [0u8; 8];
        let mut w = Writer::new(&mut buf);
        w.u8(0).unwrap();
        w.u16(0).unwrap();
        w.patch_u8(0, 9);
        w.patch_u16(1, 0x0506);
        assert_eq!(w.written(), 3);
        assert_eq!(&buf[..3], &[9, 6, 5]);
    }

    #[test]
    fn since_spans_the_bytes_read() {
        let bytes = [1, 2, 3, 4];
        let mut r = Reader::starting_at(&bytes, 1);
        let from = r.position();
        r.take(2).unwrap();
        assert_eq!(r.since(from), Ok(&bytes[1..3]));
    }

    #[test]
    fn a_frame_is_exactly_its_length_or_refused() {
        let frame = [0u8; 6];
        assert_eq!(exact_len(&frame, 6), Ok(()));
        assert_eq!(exact_len(&frame[..5], 6), Err(Errno::BufferTooSmall));
        assert_eq!(exact_len(&frame, 5), Err(Errno::LengthOutOfRange));
    }

    #[test]
    fn i32_round_trips_negative_at_offset() {
        let mut buf = [0u8; 8];
        put_i32(&mut buf, 2, -2);
        assert_eq!(read_i32(&buf, 2), -2);
        assert_eq!(&buf[2..6], &[0xFE, 0xFF, 0xFF, 0xFF]);
    }

    #[test]
    fn u16_round_trips_at_offset() {
        let mut buf = [0u8; 4];
        put_u16(&mut buf, 1, 0xBEEF);
        assert_eq!(buf, [0x00, 0xEF, 0xBE, 0x00]);
        assert_eq!(read_u16(&buf, 1), 0xBEEF);
    }

    #[test]
    fn u32_round_trips_at_offset() {
        let mut buf = [0u8; 8];
        put_u32(&mut buf, 2, 0xDEAD_BEEF);
        assert_eq!(read_u32(&buf, 2), 0xDEAD_BEEF);
        assert_eq!(&buf[2..6], &[0xEF, 0xBE, 0xAD, 0xDE]);
    }

    #[test]
    fn i64_round_trips_negative_at_offset() {
        let mut buf = [0u8; 16];
        put_i64(&mut buf, 4, -2);
        assert_eq!(read_i64(&buf, 4), -2);
        assert_eq!(
            &buf[4..12],
            &[0xFE, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF]
        );
    }

    #[test]
    fn u64_round_trips_at_offset() {
        let mut buf = [0u8; 16];
        put_u64(&mut buf, 4, 0x0123_4567_89AB_CDEF);
        assert_eq!(read_u64(&buf, 4), 0x0123_4567_89AB_CDEF);
        assert_eq!(
            &buf[4..12],
            &[0xEF, 0xCD, 0xAB, 0x89, 0x67, 0x45, 0x23, 0x01]
        );
    }
}
