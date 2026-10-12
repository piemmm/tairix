//! Codebooks (Vorbis I, section 3): a Huffman code over entry numbers, and
//! for a book read in VQ context the vector each entry stands for.
//!
//! A codeword up to [`FAST_BITS`] long is decoded by one lookup on the next
//! bits; a longer one by a binary search over the book's longer codewords,
//! sorted as left-aligned values, which a complete prefix code makes exact.

use alloc::vec::Vec;

use super::bits::{ilog, Bits, EndOfPacket};
use super::Budget;
use crate::DecodeError;

/// Bits of the lookup a codeword is first decoded through.
const FAST_BITS: u32 = 10;

/// The pattern each codebook opens with: `BCV`.
const SYNC: u32 = 0x56_4342;

/// The longest codeword a length field states.
const MAX_CODEWORD: u32 = 32;

/// A codeword longer than the fast lookup's bits, left-aligned in 32 bits,
/// and its entry with its length above bit 24, as the fast lookup packs them.
#[derive(Copy, Clone)]
struct LongCode {
    code: u32,
    slot: u32,
}

/// One codebook.
pub(super) struct Codebook {
    dimensions: usize,
    entries: u32,
    /// Indexed by the next [`Self::fast_bits`] bits: an entry and its length
    /// above bit 24, or zero where the codeword is longer.
    fast: Vec<u32>,
    fast_bits: u32,
    long: Vec<LongCode>,
    /// The entry of a book with one used entry, whose codeword is one bit
    /// of either value.
    single: Option<u32>,
    /// Every entry's vector, `dimensions` values each; empty for a book read
    /// for entry numbers only.
    values: Vec<f32>,
}

impl Codebook {
    pub(super) const fn dimensions(&self) -> usize {
        self.dimensions
    }

    pub(super) const fn entries(&self) -> u32 {
        self.entries
    }

    pub(super) fn has_values(&self) -> bool {
        !self.values.is_empty()
    }

    /// The next entry number. A book with no used entry ends the packet.
    pub(super) fn decode(&self, bits: &mut Bits<'_>) -> Result<u32, EndOfPacket> {
        if let Some(entry) = self.single {
            bits.consume(1)?;
            return Ok(entry);
        }
        let slot = self.fast[bits.peek(self.fast_bits) as usize];
        if slot != 0 {
            bits.consume(slot >> 24)?;
            return Ok(slot & 0xFF_FFFF);
        }
        let code = bits.peek(MAX_CODEWORD).reverse_bits();
        let at = self.long.partition_point(|long| long.code <= code);
        let long = at
            .checked_sub(1)
            .and_then(|at| self.long.get(at))
            .ok_or(EndOfPacket)?;
        bits.consume(long.slot >> 24)?;
        Ok(long.slot & 0xFF_FFFF)
    }

    /// The vector the next entry stands for.
    pub(super) fn vector(&self, bits: &mut Bits<'_>) -> Result<&[f32], EndOfPacket> {
        let start = self.decode(bits)? as usize * self.dimensions;
        self.values
            .get(start..start + self.dimensions)
            .ok_or(EndOfPacket)
    }
}

/// `mantissa * 2^(exponent - 788)` as the 32 bits state it, the exponent
/// held within the 63 binary orders either way libvorbis allows, so the value
/// stays finite.
fn float32_unpack(raw: u32) -> f32 {
    let mantissa = f64::from(raw & 0x1F_FFFF);
    let exponent = i32::try_from((raw & 0x7FE0_0000) >> 21).unwrap_or(0) - 788;
    let magnitude = mantissa * pow2(exponent.clamp(-63, 63));
    crate::single(if raw & 0x8000_0000 != 0 {
        -magnitude
    } else {
        magnitude
    })
}

/// `2^exponent`, exactly, for an exponent within 63 either way.
fn pow2(exponent: i32) -> f64 {
    let whole = f64::from(1u32 << (exponent.unsigned_abs() % 32));
    let rest = f64::from(1u32 << 31) * 2.0;
    let mut value = whole;
    for _ in 0..exponent.unsigned_abs() / 32 {
        value *= rest;
    }
    if exponent < 0 {
        1.0 / value
    } else {
        value
    }
}

/// The greatest `r` with `r^dimensions` at most `entries` (Vorbis I, section
/// 9.2.3).
fn lookup1_values(entries: u32, dimensions: usize) -> u32 {
    let fits = |r: u64| {
        let mut product = 1u64;
        for _ in 0..dimensions {
            product = product.saturating_mul(r);
            if product > u64::from(entries) {
                return false;
            }
        }
        true
    };
    let (mut lo, mut hi) = (1u64, u64::from(entries) + 1);
    while hi - lo > 1 {
        let mid = lo + (hi - lo) / 2;
        if fits(mid) {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    u32::try_from(lo).unwrap_or(1)
}

/// Read one codebook from the setup, charging its entries and expanded
/// values to `budget` before anything is allocated for them.
pub(super) fn read(bits: &mut Bits<'_>, budget: &mut Budget) -> Result<Codebook, DecodeError> {
    let bad = |_| DecodeError::VorbisBadSetup;
    if bits.read(24).map_err(bad)? != SYNC {
        return Err(DecodeError::VorbisBadSetup);
    }
    let dimensions = bits.read(16).map_err(bad)?;
    let entries = bits.read(24).map_err(bad)?;
    // libvorbis refuses a book whose entries and dimensions together take
    // more than 24 bits, so this refuses no stream it plays.
    if dimensions == 0 || entries == 0 || ilog(dimensions) + ilog(entries) > 24 {
        return Err(DecodeError::VorbisBadSetup);
    }
    budget.charge_entries(entries)?;
    let lengths = lengths(bits, entries)?;
    let dimensions = dimensions as usize;
    let values = match bits.read(4).map_err(bad)? {
        0 => Vec::new(),
        lookup @ (1 | 2) => vectors(bits, lookup, entries, dimensions, budget)?,
        _ => return Err(DecodeError::VorbisBadSetup),
    };
    let mut book = Codebook {
        dimensions,
        entries,
        fast: Vec::new(),
        fast_bits: 0,
        long: Vec::new(),
        single: None,
        values,
    };
    huffman(&mut book, &lengths)?;
    Ok(book)
}

/// Each entry's codeword length, zero for an unused entry.
fn lengths(bits: &mut Bits<'_>, entries: u32) -> Result<Vec<u8>, DecodeError> {
    let bad = |_| DecodeError::VorbisBadSetup;
    let count = entries as usize;
    let mut lengths = Vec::new();
    if lengths.try_reserve_exact(count).is_err() {
        return Err(DecodeError::OutOfMemory);
    }
    if bits.flag().map_err(bad)? {
        let mut length = bits.read(5).map_err(bad)? + 1;
        while lengths.len() < count {
            if length > MAX_CODEWORD {
                return Err(DecodeError::VorbisBadSetup);
            }
            let left =
                entries - u32::try_from(lengths.len()).map_err(|_| DecodeError::VorbisBadSetup)?;
            let run = bits.read(ilog(left)).map_err(bad)?;
            if run > left {
                return Err(DecodeError::VorbisBadSetup);
            }
            let byte = u8::try_from(length).map_err(|_| DecodeError::VorbisBadSetup)?;
            lengths.resize(lengths.len() + run as usize, byte);
            length += 1;
        }
    } else {
        let sparse = bits.flag().map_err(bad)?;
        for _ in 0..count {
            let used = !sparse || bits.flag().map_err(bad)?;
            let length = if used {
                bits.read(5).map_err(bad)? + 1
            } else {
                0
            };
            lengths.push(u8::try_from(length).map_err(|_| DecodeError::VorbisBadSetup)?);
        }
    }
    Ok(lengths)
}

/// Every entry's vector, from the lookup the setup states (Vorbis I, section
/// 3.2.1): lookup 1 builds each from a lattice of shared values, lookup 2
/// lists each in full.
fn vectors(
    bits: &mut Bits<'_>,
    lookup: u32,
    entries: u32,
    dimensions: usize,
    budget: &mut Budget,
) -> Result<Vec<f32>, DecodeError> {
    let bad = |_| DecodeError::VorbisBadSetup;
    let minimum = float32_unpack(bits.read(32).map_err(bad)?);
    let delta = float32_unpack(bits.read(32).map_err(bad)?);
    let value_bits = bits.read(4).map_err(bad)? + 1;
    let sequence = bits.flag().map_err(bad)?;
    let total = entries as usize * dimensions;
    budget.charge_values(total)?;
    let stored = if lookup == 1 {
        lookup1_values(entries, dimensions) as usize
    } else {
        total
    };
    let mut multiplicands = Vec::new();
    let mut values = Vec::new();
    if multiplicands.try_reserve_exact(stored).is_err() || values.try_reserve_exact(total).is_err()
    {
        return Err(DecodeError::OutOfMemory);
    }
    for _ in 0..stored {
        multiplicands.push(bits.read(value_bits).map_err(bad)?);
    }
    let scaled = |multiplicand: u32, last: f32| {
        crate::single(f64::from(multiplicand)) * delta + minimum + last
    };
    for entry in 0..entries as usize {
        let mut last = 0.0;
        let mut divisor = 1usize;
        for dimension in 0..dimensions {
            let offset = if lookup == 1 {
                let offset = entry / divisor % stored;
                divisor = divisor.saturating_mul(stored);
                offset
            } else {
                entry * dimensions + dimension
            };
            let value = scaled(multiplicands[offset], last);
            if sequence {
                last = value;
            }
            values.push(value);
        }
    }
    if values.iter().any(|value| !value.is_finite()) {
        return Err(DecodeError::VorbisBadSetup);
    }
    Ok(values)
}

/// Assign each used entry, in order, the lowest-valued codeword of its
/// length still free (Vorbis I, section 3.2.1), refusing a code that is
/// over- or underspecified; build the lookups decoding reads through.
fn huffman(book: &mut Codebook, lengths: &[u8]) -> Result<(), DecodeError> {
    let used = lengths.iter().filter(|&&length| length > 0).count();
    let longest = u32::from(lengths.iter().copied().max().unwrap_or(0));
    book.fast_bits = longest.min(FAST_BITS);
    let mut fast = Vec::new();
    if fast.try_reserve_exact(1 << book.fast_bits).is_err() {
        return Err(DecodeError::OutOfMemory);
    }
    fast.resize(1 << book.fast_bits, 0u32);
    // The next free codeword of each length, as a value of that many bits;
    // held wide, as a length-32 marker reaches 2^32 once that depth fills.
    let mut marker = [0u64; MAX_CODEWORD as usize + 1];
    for (entry, &length) in lengths.iter().enumerate() {
        if length == 0 {
            continue;
        }
        let length = usize::from(length);
        let code = marker[length];
        if code >> length != 0 {
            return Err(DecodeError::VorbisBadSetup);
        }
        let entry = u32::try_from(entry).map_err(|_| DecodeError::VorbisBadSetup)?;
        place(book, &mut fast, code, length, entry)?;
        for depth in (1..=length).rev() {
            if marker[depth] & 1 == 1 {
                marker[depth] = if depth == 1 {
                    marker[1] + 1
                } else {
                    marker[depth - 1] << 1
                };
                break;
            }
            marker[depth] += 1;
        }
        let mut taken = code;
        for depth in length + 1..=MAX_CODEWORD as usize {
            if marker[depth] >> 1 != taken {
                break;
            }
            taken = marker[depth];
            marker[depth] = marker[depth - 1] << 1;
        }
    }
    // A book of one used entry has the one codeword `0`, which leaves the
    // tree underpopulated; the errata admit it only at length one.
    let single = used == 1 && marker[2] == 2;
    if !single
        && (1..=MAX_CODEWORD as usize).any(|depth| marker[depth] & ((1u64 << depth) - 1) != 0)
    {
        return Err(DecodeError::VorbisBadSetup);
    }
    if single {
        book.single = lengths
            .iter()
            .position(|&length| length > 0)
            .and_then(|entry| u32::try_from(entry).ok());
    }
    book.long.sort_unstable_by_key(|long| long.code);
    book.fast = fast;
    Ok(())
}

/// Enter `entry`'s codeword `code`, `length` bits, into the lookup it is
/// decoded through.
fn place(
    book: &mut Codebook,
    fast: &mut [u32],
    code: u64,
    length: usize,
    entry: u32,
) -> Result<(), DecodeError> {
    let length_bits = u32::try_from(length).map_err(|_| DecodeError::VorbisBadSetup)?;
    let code = u32::try_from(code).map_err(|_| DecodeError::VorbisBadSetup)?;
    let slot = length_bits << 24 | entry;
    if length_bits <= book.fast_bits {
        // The stream presents a codeword's first bit lowest, so the lookup is
        // indexed by the codeword reversed.
        let reversed = code.reverse_bits() >> (32 - length_bits);
        for index in (reversed as usize..fast.len()).step_by(1 << length) {
            fast[index] = slot;
        }
        return Ok(());
    }
    if book.long.try_reserve(1).is_err() {
        return Err(DecodeError::OutOfMemory);
    }
    book.long.push(LongCode {
        code: code << (32 - length_bits),
        slot,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use alloc::vec;
    use alloc::vec::Vec;

    use super::super::bits::{BitWriter, Bits, EndOfPacket};
    use super::super::Budget;
    use super::{float32_unpack, huffman, lookup1_values, Codebook};
    use crate::DecodeError;

    fn book(lengths: &[u8]) -> Result<Codebook, DecodeError> {
        let mut book = Codebook {
            dimensions: 1,
            entries: u32::try_from(lengths.len()).expect("small"),
            fast: Vec::new(),
            fast_bits: 0,
            long: Vec::new(),
            single: None,
            values: Vec::new(),
        };
        huffman(&mut book, lengths).map(|()| book)
    }

    /// The codeword `bits`, first bit leftmost, as the stream presents it.
    fn write_code(writer: &mut BitWriter, code: &str) {
        for bit in code.bytes() {
            writer.flag(bit == b'1');
        }
    }

    /// The specification's own example: lengths 2,4,4,4,4,2,3,3 give codewords
    /// 00, 0100, 0101, 0110, 0111, 10, 110, 111.
    #[test]
    fn codewords_are_assigned_as_the_specification_s_example_lays_them_out() {
        let codes = ["00", "0100", "0101", "0110", "0111", "10", "110", "111"];
        let book = book(&[2, 4, 4, 4, 4, 2, 3, 3]).expect("a complete code");
        let mut writer = BitWriter::default();
        for code in codes.iter().rev().chain(codes.iter()) {
            write_code(&mut writer, code);
        }
        let bytes = writer.into_bytes();
        let mut bits = Bits::new(&bytes);
        let decoded: Vec<u32> = (0..16)
            .map(|_| book.decode(&mut bits).expect("an entry"))
            .collect();
        let expected: Vec<u32> = (0..8).rev().chain(0..8).collect();
        assert_eq!(decoded, expected);
    }

    /// Codewords longer than the fast lookup decode through the sorted ones.
    #[test]
    fn long_codewords_decode_through_the_search() {
        let lengths: Vec<u8> = (1..=20).chain([20]).collect();
        let book = book(&lengths).expect("a complete code");
        let mut writer = BitWriter::default();
        for entry in [20usize, 3, 13, 19, 0, 12] {
            let code: alloc::string::String = if entry == 20 {
                "1".repeat(20)
            } else {
                "1".repeat(entry) + "0"
            };
            write_code(&mut writer, &code);
        }
        let bytes = writer.into_bytes();
        let mut bits = Bits::new(&bytes);
        for entry in [20, 3, 13, 19, 0, 12] {
            assert_eq!(book.decode(&mut bits), Ok(entry));
        }
        assert_eq!(book.decode(&mut bits), Err(EndOfPacket));
    }

    #[test]
    fn over_and_underspecified_codes_are_refused() {
        assert_eq!(book(&[1, 1, 1]).err(), Some(DecodeError::VorbisBadSetup));
        assert_eq!(book(&[1, 2]).err(), Some(DecodeError::VorbisBadSetup));
        assert_eq!(book(&[2, 0, 0]).err(), Some(DecodeError::VorbisBadSetup));
        assert!(book(&[1, 0, 2, 2]).is_ok());
    }

    /// One used entry is admitted only at length one, and reads as one bit of
    /// either value; a book with none ends the packet when read.
    #[test]
    fn a_single_entry_book_reads_one_bit_of_either_value() {
        let single = book(&[0, 1, 0]).expect("the errata's single entry");
        let bytes = [0b10];
        let mut bits = Bits::new(&bytes);
        assert_eq!(single.decode(&mut bits), Ok(1));
        assert_eq!(single.decode(&mut bits), Ok(1));
        let empty = book(&[0, 0]).expect("libvorbis admits an empty book");
        assert_eq!(empty.decode(&mut Bits::new(&[0xFF])), Err(EndOfPacket));
    }

    #[test]
    fn float32_unpack_reads_the_vq_float_format() {
        assert_eq!(float32_unpack(788 << 21 | 3).to_bits(), 3.0f32.to_bits());
        assert_eq!(
            float32_unpack(0x8000_0000 | 787 << 21 | 1).to_bits(),
            (-0.5f32).to_bits()
        );
        assert_eq!(float32_unpack(0).to_bits(), 0.0f32.to_bits());
        assert!(float32_unpack(0x7FFF_FFFF).is_finite());
    }

    #[test]
    fn lookup1_values_is_the_greatest_lattice_side_the_entries_hold() {
        assert_eq!(lookup1_values(81, 4), 3);
        assert_eq!(lookup1_values(80, 4), 2);
        assert_eq!(lookup1_values(1, 8), 1);
        assert_eq!(lookup1_values(625, 2), 25);
        assert_eq!(lookup1_values(8_388_607, 1), 8_388_607);
    }

    /// A lookup-1 book builds each entry's vector from the lattice, the
    /// sequence flag accumulating along it.
    #[test]
    fn lattice_vectors_follow_the_entry_digits_and_accumulate_in_sequence() {
        let mut writer = BitWriter::default();
        writer.write(0x56_4342, 24);
        writer.write(2, 16);
        writer.write(9, 24);
        writer.flag(false);
        writer.flag(false);
        for _ in 0..9 {
            writer.write(3, 5);
        }
        writer.write(1, 4);
        writer.write(788 << 21 | 1, 32);
        writer.write(788 << 21 | 2, 32);
        writer.write(1, 4);
        writer.flag(true);
        for multiplicand in [0, 1, 3] {
            writer.write(multiplicand, 2);
        }
        let bytes = writer.into_bytes();
        let mut budget = Budget::default();
        let book = super::read(&mut Bits::new(&bytes), &mut budget).err();
        assert_eq!(
            book,
            Some(DecodeError::VorbisBadSetup),
            "nine codewords of four bits leave seven free"
        );
        let mut writer = BitWriter::default();
        writer.write(0x56_4342, 24);
        writer.write(2, 16);
        writer.write(9, 24);
        writer.flag(false);
        writer.flag(false);
        for length in [1u32, 2, 3, 4, 5, 6, 7, 8, 8] {
            writer.write(length - 1, 5);
        }
        writer.write(1, 4);
        writer.write(788 << 21 | 1, 32);
        writer.write(788 << 21 | 2, 32);
        writer.write(1, 4);
        writer.flag(true);
        for multiplicand in [0, 1, 3] {
            writer.write(multiplicand, 2);
        }
        let bytes = writer.into_bytes();
        let book = super::read(&mut Bits::new(&bytes), &mut budget).expect("a book");
        assert_eq!(book.dimensions(), 2);
        // Entry 7 is digits (1, 2) in base three: values 1 + 2*1 = 3, then
        // 1 + 2*3 + 3 = 10 with the first carried.
        assert_eq!(book.values[14..16], [3.0, 10.0]);
        assert_eq!(book.values[..2], [1.0, 2.0]);
        let codes = vec![(7, "11111110"), (0, "0")];
        let mut writer = BitWriter::default();
        for (_, code) in &codes {
            write_code(&mut writer, code);
        }
        let bytes = writer.into_bytes();
        let mut bits = Bits::new(&bytes);
        assert_eq!(book.vector(&mut bits).expect("a vector"), &[3.0, 10.0]);
        assert_eq!(book.vector(&mut bits).expect("a vector"), &[1.0, 2.0]);
    }
}
