//! Reading a byte slice a bit at a time, most significant bit first, as FLAC
//! lays its frames out.

/// The slice ended before a read did.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Exhausted;

/// A cursor over `bytes`: the next bits are held most significant first in
/// a 64-bit cache refilled a byte at a time, so a field costs a shift rather
/// than a walk over the bytes it spans.
pub(crate) struct BitReader<'b> {
    bytes: &'b [u8],
    /// Bytes moved into the cache.
    loaded: usize,
    /// The next bits, from the top; those below `bits` are zero.
    cache: u64,
    bits: u32,
}

impl<'b> BitReader<'b> {
    pub(crate) const fn new(bytes: &'b [u8]) -> Self {
        Self {
            bytes,
            loaded: 0,
            cache: 0,
            bits: 0,
        }
    }

    /// Bytes consumed, a partial one counting whole.
    pub(crate) const fn bytes_consumed(&self) -> usize {
        (self.loaded * 8 - self.bits as usize).div_ceil(8)
    }

    fn refill(&mut self) {
        while self.bits <= 56 {
            let Some(&byte) = self.bytes.get(self.loaded) else {
                return;
            };
            self.cache |= u64::from(byte) << (56 - self.bits);
            self.bits += 8;
            self.loaded += 1;
        }
    }

    /// Drop the next `count` cached bits, `count` at most the bits held.
    const fn consume(&mut self, count: u32) {
        self.cache = if count >= 64 { 0 } else { self.cache << count };
        self.bits -= count;
    }

    /// The next `count` bits, `count` at most 32, as an unsigned value. A
    /// wider count is no field this reader serves, and fails as one past the
    /// end would.
    pub(crate) fn read(&mut self, count: u32) -> Result<u32, Exhausted> {
        if count == 0 {
            return Ok(0);
        }
        if count > 32 {
            return Err(Exhausted);
        }
        if self.bits < count {
            self.refill();
            if self.bits < count {
                return Err(Exhausted);
            }
        }
        let value = self.cache >> (64 - count);
        self.consume(count);
        Ok((value & 0xFFFF_FFFF) as u32)
    }

    /// The next `count` bits, `count` at most 32, as a two's-complement value.
    pub(crate) fn read_signed(&mut self, count: u32) -> Result<i32, Exhausted> {
        let raw = self.read(count)?;
        if count == 0 || count == 32 {
            return Ok(raw.cast_signed());
        }
        let shift = 32 - count;
        Ok((raw << shift).cast_signed() >> shift)
    }

    /// The next `count` bits, `count` at most 33, as a two's-complement value:
    /// the width a side channel of 32-bit samples needs.
    pub(crate) fn read_signed_wide(&mut self, count: u32) -> Result<i64, Exhausted> {
        if count > 33 {
            return Err(Exhausted);
        }
        if count == 0 {
            return Ok(0);
        }
        if count <= 32 {
            let high = self.read(count)?;
            let shift = 64 - count;
            return Ok((i64::from(high) << shift) >> shift);
        }
        let high = i64::from(self.read(1)?);
        let low = i64::from(self.read(32)?);
        Ok(((high << 32 | low) << 31) >> 31)
    }

    /// Zero bits up to the next one, which is consumed: a unary code.
    pub(crate) fn unary(&mut self) -> Result<u32, Exhausted> {
        let mut zeros: u32 = 0;
        loop {
            if self.bits == 0 {
                self.refill();
                if self.bits == 0 {
                    return Err(Exhausted);
                }
            }
            let lead = self.cache.leading_zeros();
            if lead < self.bits {
                self.consume(lead + 1);
                return Ok(zeros.saturating_add(lead));
            }
            zeros = zeros.saturating_add(self.bits);
            self.consume(self.bits);
        }
    }

    /// Skip to the next byte boundary.
    pub(crate) const fn align(&mut self) {
        self.consume(self.bits % 8);
    }
}

/// Bytes written a bit at a time, most significant bit first.
#[cfg(any(test, feature = "encode"))]
#[derive(Default)]
pub(crate) struct BitWriter {
    bytes: alloc::vec::Vec<u8>,
    /// Bits of the last byte in use; zero when it is whole.
    partial: u32,
}

#[cfg(any(test, feature = "encode"))]
impl BitWriter {
    /// The low `count` bits of `value`, `count` at most 64.
    pub(crate) fn write(&mut self, value: u64, count: u32) {
        let mut left = count;
        while left > 0 {
            if self.partial == 0 {
                self.bytes.push(0);
            }
            let room = 8 - self.partial;
            let take = room.min(left);
            let bits = (value >> (left - take)) & ((1u64 << take) - 1);
            if let Some(last) = self.bytes.last_mut() {
                *last |= ((bits << (room - take)) & 0xFF) as u8;
            }
            self.partial = (self.partial + take) % 8;
            left -= take;
        }
    }

    /// `value` as `count` bits of two's complement.
    pub(crate) fn write_signed(&mut self, value: i64, count: u32) {
        self.write(value.cast_unsigned(), count);
    }

    /// `zeros` zero bits and a one: a unary code.
    pub(crate) fn unary(&mut self, zeros: u64) {
        let mut left = zeros;
        while left >= 32 {
            self.write(0, 32);
            left -= 32;
        }
        self.write(1, u32::try_from(left).unwrap_or(0) + 1);
    }

    /// Zero bits to the next byte boundary.
    pub(crate) fn align(&mut self) {
        self.partial = 0;
    }

    /// Bits written.
    pub(crate) fn bits(&self) -> u64 {
        let whole = self.bytes.len() as u64 * 8;
        if self.partial == 0 {
            whole
        } else {
            whole - u64::from(8 - self.partial)
        }
    }

    /// The bytes written, the last padded with zeros.
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn into_bytes(self) -> alloc::vec::Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::{BitReader, BitWriter, Exhausted};

    #[test]
    fn what_the_writer_writes_the_reader_reads_back() {
        let mut writer = BitWriter::default();
        writer.write(0b101, 3);
        writer.write_signed(-3, 5);
        writer.unary(40);
        writer.write(0x1_2345_6789, 33);
        assert_eq!(writer.bits(), 3 + 5 + 41 + 33);
        writer.align();
        writer.write(0xAB, 8);
        let bytes = writer.into_bytes();
        let mut reader = BitReader::new(&bytes);
        assert_eq!(reader.read(3), Ok(0b101));
        assert_eq!(reader.read_signed(5), Ok(-3));
        assert_eq!(reader.unary(), Ok(40));
        assert_eq!(reader.read_signed_wide(33), Ok(0x1_2345_6789 - (1 << 33)));
        reader.align();
        assert_eq!(reader.read(8), Ok(0xAB));
    }

    #[test]
    fn fields_are_read_most_significant_bit_first_across_bytes() {
        let mut reader = BitReader::new(&[0b1011_0011, 0b0101_1100, 0xFF]);
        assert_eq!(reader.read(3), Ok(0b101));
        assert_eq!(reader.read(7), Ok(0b100_1101));
        assert_eq!(reader.read(0), Ok(0));
        assert_eq!(reader.read(6), Ok(0b01_1100));
        assert_eq!(reader.bytes_consumed(), 2);
        assert_eq!(reader.read(9), Err(Exhausted));
        assert_eq!(reader.read(8), Ok(0xFF));
    }

    #[test]
    fn signed_fields_extend_their_sign_bit() {
        let mut reader = BitReader::new(&[0b1110_0001, 0x80, 0, 0, 0]);
        assert_eq!(reader.read_signed(3), Ok(-1));
        assert_eq!(reader.read_signed(5), Ok(1));
        assert_eq!(reader.read_signed(32), Ok(i32::MIN));
        let mut wide = BitReader::new(&[0xC0, 0, 0, 0, 0]);
        assert_eq!(wide.read_signed_wide(33), Ok(-(1 << 31)));
        let mut wide = BitReader::new(&[0x7F, 0xFF, 0xFF, 0xFF, 0x80]);
        assert_eq!(wide.read_signed_wide(33), Ok((1 << 32) - 1));
    }

    #[test]
    fn a_unary_code_counts_the_zeros_before_its_one() {
        let mut reader = BitReader::new(&[0b0010_0000, 0x00, 0b0000_0001, 0b1000_0000]);
        assert_eq!(reader.unary(), Ok(2));
        assert_eq!(reader.unary(), Ok(5 + 8 + 7));
        assert_eq!(reader.unary(), Ok(0));
        assert_eq!(reader.unary(), Err(Exhausted));
    }

    #[test]
    fn aligning_skips_to_the_next_whole_byte() {
        let mut reader = BitReader::new(&[0xFF, 0x0F]);
        assert_eq!(reader.read(3), Ok(0b111));
        reader.align();
        assert_eq!(reader.bytes_consumed(), 1);
        assert_eq!(reader.read(8), Ok(0x0F));
        reader.align();
        assert_eq!(reader.bytes_consumed(), 2);
    }
}
