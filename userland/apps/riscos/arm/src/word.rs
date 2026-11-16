//! Narrower words cut from wider values: the architecture's own truncation.

/// The low halfword of a word.
pub(crate) const fn low16(value: u32) -> u16 {
    let b = value.to_le_bytes();
    u16::from_le_bytes([b[0], b[1]])
}

/// The low word of a 64-bit value.
pub(crate) const fn low(value: u64) -> u32 {
    let b = value.to_le_bytes();
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// The high word of a 64-bit value.
pub(crate) const fn high(value: u64) -> u32 {
    let b = value.to_le_bytes();
    u32::from_le_bytes([b[4], b[5], b[6], b[7]])
}

/// The low doubleword of a 128-bit value.
pub(crate) const fn low64(value: u128) -> u64 {
    let b = value.to_le_bytes();
    u64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
}
