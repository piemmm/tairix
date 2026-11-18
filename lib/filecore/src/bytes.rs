//! Little-endian field access within an in-memory directory or map image.

/// Read a little-endian 24-bit value from `buf` at `at`.
pub fn get_u24(buf: &[u8], at: usize) -> u32 {
    u32::from(buf[at]) | u32::from(buf[at + 1]) << 8 | u32::from(buf[at + 2]) << 16
}

/// Read a little-endian `u32` from `buf` at `at`.
pub fn get_u32(buf: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([buf[at], buf[at + 1], buf[at + 2], buf[at + 3]])
}

/// Write a little-endian 24-bit value into `buf` at `at` (high byte of
/// `value` must be zero; enforced by the callers' 24-bit field bounds).
pub fn put_u24(buf: &mut [u8], at: usize, value: u32) {
    buf[at..at + 3].copy_from_slice(&value.to_le_bytes()[..3]);
}

/// Write a little-endian `u32` into `buf` at `at`.
pub fn put_u32(buf: &mut [u8], at: usize, value: u32) {
    buf[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
