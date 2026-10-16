//! Little-endian field access, the legacy crc16, the crc32c a record takes
//! over its own checksum field, and the narrowing every on-disk number goes
//! through. An out-of-range read answers zero and an out-of-range write does
//! nothing, so a short buffer can never panic.

use alloc::vec::Vec;

use tairix_abi::DriverError;

/// A little-endian `u16` at `off`, or `0` out of bounds.
pub(crate) fn le16(buf: &[u8], off: usize) -> u16 {
    match buf.get(off..off + 2) {
        Some(b) => u16::from_le_bytes([b[0], b[1]]),
        None => 0,
    }
}

/// A little-endian `u32` at `off`, or `0` out of bounds.
pub(crate) fn le32(buf: &[u8], off: usize) -> u32 {
    match buf.get(off..off + 4) {
        Some(b) => u32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        None => 0,
    }
}

/// `value`'s low 32 bits: the half a split field's low word holds.
pub(crate) fn low32(value: u64) -> u32 {
    let [a, b, c, d, ..] = value.to_le_bytes();
    u32::from_le_bytes([a, b, c, d])
}

/// `value`'s bits 32 to 63: the half a split field's high word holds.
pub(crate) fn high32(value: u64) -> u32 {
    let [.., e, f, g, h] = value.to_le_bytes();
    u32::from_le_bytes([e, f, g, h])
}

/// `value`'s low 16 bits.
pub(crate) fn low16(value: u32) -> u16 {
    let [a, b, ..] = value.to_le_bytes();
    u16::from_le_bytes([a, b])
}

/// Store a little-endian `u16` at `off`.
pub(crate) fn put_le16(buf: &mut [u8], off: usize, value: u16) {
    if let Some(b) = buf.get_mut(off..off + 2) {
        b.copy_from_slice(&value.to_le_bytes());
    }
}

/// Store a little-endian `u32` at `off`.
pub(crate) fn put_le32(buf: &mut [u8], off: usize, value: u32) {
    if let Some(b) = buf.get_mut(off..off + 4) {
        b.copy_from_slice(&value.to_le_bytes());
    }
}

/// CRC-16 with the reversed polynomial `0xA001`, continued from `crc` — the
/// checksum the legacy `gdt_csum` feature stores in each group descriptor.
pub(crate) fn crc16(mut crc: u16, data: &[u8]) -> u16 {
    for &byte in data {
        crc ^= u16::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0xA001
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// crc32c of `bytes` continuing from `state`, the `N` bytes at `at` — the
/// record's own checksum field — counted as zero. A short buffer sums only what
/// it holds, so it fails its check rather than panicking.
pub(crate) fn crc32c_zeroed<const N: usize>(state: u32, bytes: &[u8], at: usize) -> u32 {
    let before = bytes.get(..at).unwrap_or(bytes);
    let after = at
        .checked_add(N)
        .and_then(|end| bytes.get(end..))
        .unwrap_or_default();
    let c = tairix_crc32c::update(state, before);
    let c = tairix_crc32c::update(c, &[0; N]);
    tairix_crc32c::update(c, after)
}

/// `len` zeroed bytes of heap scratch: block-sized buffers stay off the small
/// kernel stack these paths nest on.
pub(crate) fn scratch(len: usize) -> Result<Vec<u8>, DriverError> {
    tairix_util::fallible::filled(len, 0).ok_or(DriverError::NoSpace)
}

/// Narrow a `u64`, refusing overflow as damage.
pub(crate) fn u32_of(value: u64) -> Result<u32, DriverError> {
    u32::try_from(value).map_err(|_| DriverError::DeviceFault)
}

/// Narrow a `usize`, refusing overflow as damage.
pub(crate) fn u16_of(value: usize) -> Result<u16, DriverError> {
    u16::try_from(value).map_err(|_| DriverError::DeviceFault)
}

/// Narrow a `u64` to a `usize`, refusing overflow as damage.
pub(crate) fn usize_of(value: u64) -> Result<usize, DriverError> {
    usize::try_from(value).map_err(|_| DriverError::DeviceFault)
}

/// The low 32 bits of `value`, little-endian: how a group or inode number
/// enters a checksum's seed chain.
pub(crate) fn u32_le_truncate(value: u64) -> [u8; 4] {
    ((value & 0xFFFF_FFFF) as u32).to_le_bytes()
}

/// A 32-bit id an inode stores as a low and a high half.
pub(crate) fn split_id(raw: &[u8], lo: usize, hi: usize) -> u32 {
    u32::from(le16(raw, lo)) | (u32::from(le16(raw, hi)) << 16)
}

/// Store `id` as the halves [`split_id`] reads.
pub(crate) fn put_split_id(raw: &mut [u8], lo: usize, hi: usize, id: u32) {
    let [a, b, c, d] = id.to_le_bytes();
    put_le16(raw, lo, u16::from_le_bytes([a, b]));
    put_le16(raw, hi, u16::from_le_bytes([c, d]));
}

/// Round `n` up to a multiple of four, the record alignment.
pub(crate) fn align4(n: usize) -> usize {
    (n + 3) & !3
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_zeroed_field_sums_as_if_the_bytes_were_zero() {
        let record: Vec<u8> = (0u8..64).collect();
        let mut cleared = record.clone();
        cleared[20..24].fill(0);
        assert_eq!(
            crc32c_zeroed::<4>(7, &record, 20),
            tairix_crc32c::update(7, &cleared)
        );
        assert_ne!(
            crc32c_zeroed::<4>(7, &record[..10], 20),
            crc32c_zeroed::<4>(7, &record, 20),
            "a record too short for its field sums what it holds and fails its check"
        );
    }
}
