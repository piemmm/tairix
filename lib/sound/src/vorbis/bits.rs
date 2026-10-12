//! A Vorbis packet read a bit at a time: each byte's least significant bit
//! first, a field's bits from its least significant up (Vorbis I, section 2).

/// A read ran past the packet's end. In the floor and residue decode this is
/// nominal: the packet ends there, and what was read stands.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct EndOfPacket;

/// A cursor over a packet: the next bits held from bit 0 up in a 64-bit
/// cache, so a field costs a mask and a shift.
pub(super) struct Bits<'p> {
    bytes: &'p [u8],
    /// Bytes moved into the cache.
    loaded: usize,
    cache: u64,
    /// Bits held; those above are zero.
    count: u32,
}

impl<'p> Bits<'p> {
    pub(super) const fn new(bytes: &'p [u8]) -> Self {
        Self {
            bytes,
            loaded: 0,
            cache: 0,
            count: 0,
        }
    }

    fn refill(&mut self) {
        if let Some(word) = self
            .bytes
            .get(self.loaded..self.loaded + 8)
            .and_then(|bytes| bytes.first_chunk::<8>())
        {
            let take = (63 - self.count) / 8;
            self.cache |= u64::from_le_bytes(*word) << self.count;
            self.loaded += take as usize;
            self.count += take * 8;
            self.cache &= low_mask(self.count);
            return;
        }
        while self.count <= 56 {
            let Some(&byte) = self.bytes.get(self.loaded) else {
                return;
            };
            self.cache |= u64::from(byte) << self.count;
            self.count += 8;
            self.loaded += 1;
        }
    }

    /// The next `count` bits, `count` at most 32.
    pub(super) fn read(&mut self, count: u32) -> Result<u32, EndOfPacket> {
        if self.count < count {
            self.refill();
            if self.count < count {
                self.count = 0;
                self.cache = 0;
                self.loaded = self.bytes.len();
                return Err(EndOfPacket);
            }
        }
        let value = self.cache & low_mask(count);
        self.cache >>= count;
        self.count -= count;
        Ok(u32::try_from(value).unwrap_or(u32::MAX))
    }

    pub(super) fn flag(&mut self) -> Result<bool, EndOfPacket> {
        self.read(1).map(|bit| bit == 1)
    }

    /// The next `count` bits, `count` at most 32, without consuming them:
    /// zeros past the packet's end.
    pub(super) fn peek(&mut self, count: u32) -> u32 {
        if self.count < count {
            self.refill();
        }
        u32::try_from(self.cache & low_mask(count)).unwrap_or(u32::MAX)
    }

    /// Consume `count` bits a peek showed, or end the packet where it holds
    /// fewer.
    pub(super) fn consume(&mut self, count: u32) -> Result<(), EndOfPacket> {
        self.read(count).map(drop)
    }
}

/// The low `count` bits set, `count` at most 64.
const fn low_mask(count: u32) -> u64 {
    if count >= 64 {
        u64::MAX
    } else {
        (1 << count) - 1
    }
}

/// The bits `value` needs: its highest set bit's position, from one; zero
/// for zero (Vorbis I, section 9.2.1).
pub(super) const fn ilog(value: u32) -> u32 {
    u32::BITS - value.leading_zeros()
}

/// Bytes written a bit at a time, least significant first: what the tests'
/// stream writer emits packets with.
#[cfg(test)]
#[derive(Default)]
pub(super) struct BitWriter {
    bytes: alloc::vec::Vec<u8>,
    /// Bits of the last byte in use; zero when it is whole.
    partial: u32,
}

#[cfg(test)]
impl BitWriter {
    /// The low `count` bits of `value`, `count` at most 32.
    pub(super) fn write(&mut self, value: u32, count: u32) {
        for bit in 0..count {
            if self.partial == 0 {
                self.bytes.push(0);
            }
            if value >> bit & 1 == 1 {
                if let Some(last) = self.bytes.last_mut() {
                    *last |= 1 << self.partial;
                }
            }
            self.partial = (self.partial + 1) % 8;
        }
    }

    pub(super) fn flag(&mut self, set: bool) {
        self.write(u32::from(set), 1);
    }

    pub(super) fn into_bytes(self) -> alloc::vec::Vec<u8> {
        self.bytes
    }
}

#[cfg(test)]
mod tests {
    use super::{ilog, BitWriter, Bits, EndOfPacket};

    #[test]
    fn fields_are_read_least_significant_bit_first_across_bytes() {
        let bytes = [0b1011_0011, 0b0101_1100, 0xFF];
        let mut bits = Bits::new(&bytes);
        assert_eq!(bits.read(3), Ok(0b011));
        assert_eq!(bits.read(7), Ok(0b00_10110));
        assert_eq!(bits.read(0), Ok(0));
        assert_eq!(bits.peek(6), 0b01_0111);
        assert_eq!(bits.read(6), Ok(0b01_0111));
        assert_eq!(bits.read(9), Err(EndOfPacket));
        assert_eq!(bits.read(1), Err(EndOfPacket));
    }

    #[test]
    fn what_the_writer_writes_the_reader_reads_back_through_the_wide_refill() {
        let mut writer = BitWriter::default();
        let fields: [(u32, u32); 6] = [
            (0x1F, 5),
            (0xDEAD_BEEF, 32),
            (1, 1),
            (0x3FF, 10),
            (0, 7),
            (0x1234_5678, 31),
        ];
        for _ in 0..4 {
            for (value, count) in fields {
                writer.write(value, count);
            }
        }
        let bytes = writer.into_bytes();
        let mut bits = Bits::new(&bytes);
        for _ in 0..4 {
            for (value, count) in fields {
                assert_eq!(
                    bits.read(count),
                    Ok(value & (u32::MAX >> (32 - count.max(1))))
                );
            }
        }
    }

    #[test]
    fn a_peek_past_the_end_reads_zeros_and_a_consume_past_it_ends_the_packet() {
        let bytes = [0b1010_0101];
        let mut bits = Bits::new(&bytes);
        assert_eq!(bits.peek(12), 0b1010_0101);
        assert_eq!(bits.consume(9), Err(EndOfPacket));
    }

    #[test]
    fn ilog_counts_the_bits_a_value_needs() {
        let cases = [
            (0, 0),
            (1, 1),
            (2, 2),
            (3, 2),
            (4, 3),
            (7, 3),
            (u32::MAX, 32),
        ];
        for (value, bits) in cases {
            assert_eq!(ilog(value), bits, "{value}");
        }
    }
}
