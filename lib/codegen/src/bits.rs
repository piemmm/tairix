//! The low parts of a word, which machine code keeps on purpose.

/// The low 64 bits of `v`.
#[cfg(any(test, feature = "test-util"))]
#[allow(clippy::cast_possible_truncation)] // Keeping the low half is the point.
#[must_use]
pub const fn low64(v: u128) -> u64 {
    v as u64
}

/// The low 32 bits of `v`.
#[allow(clippy::cast_possible_truncation)] // Keeping the low half is the point.
#[must_use]
pub const fn low32(v: u64) -> u32 {
    v as u32
}

/// The low 16 bits of `v`.
#[allow(clippy::cast_possible_truncation)] // Keeping the low bits is the point.
#[must_use]
pub const fn low16(v: u64) -> u16 {
    v as u16
}

/// The low 8 bits of `v`.
#[allow(clippy::cast_possible_truncation)] // Keeping the low bits is the point.
#[must_use]
pub const fn low8(v: u64) -> u8 {
    v as u8
}

/// `v`'s low 32 bits as a signed value, sign-extended.
#[must_use]
pub fn sext32(v: u64) -> i64 {
    i64::from(low32(v).cast_signed())
}

/// `v`'s low 16 bits as a signed value, sign-extended.
#[must_use]
pub fn sext16(v: u64) -> i64 {
    i64::from(low16(v).cast_signed())
}

/// `v`'s low 8 bits as a signed value, sign-extended.
#[must_use]
pub fn sext8(v: u64) -> i64 {
    i64::from(low8(v).cast_signed())
}
