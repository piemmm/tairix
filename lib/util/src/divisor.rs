//! The greatest common divisor, by Euclid's algorithm.

use core::ops::Rem;

/// The unsigned integer types [`gcd`] takes: a signed value's divisor has no
/// one sign, so a caller holding one takes its magnitude first.
pub trait Unsigned: Copy + PartialEq + Rem<Output = Self> {
    /// Nought.
    const ZERO: Self;
}

macro_rules! unsigned {
    ($($t:ty),*) => {$(
        impl Unsigned for $t {
            const ZERO: Self = 0;
        }
    )*};
}

unsigned!(u8, u16, u32, u64, u128, usize);

/// The greatest common divisor of `a` and `b`; nought only where both are.
#[must_use]
pub fn gcd<T: Unsigned>(mut a: T, mut b: T) -> T {
    while b != T::ZERO {
        (a, b) = (b, a % b);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::gcd;

    #[test]
    fn the_greatest_common_divisor_is_euclid_s_in_every_width() {
        assert_eq!(gcd(72u32, 144), 72);
        assert_eq!(gcd(0u32, 5), 5);
        assert_eq!(gcd(5u64, 0), 5);
        assert_eq!(gcd(0u8, 0), 0);
        assert_eq!(gcd(48_000u32, 44_100), 300);
        assert_eq!(gcd(u128::MAX, u128::MAX - 1), 1);
        assert_eq!(gcd(1usize << 40, 3 << 20), 1 << 20);
        assert_eq!(gcd(65_535u16, 4_369), 4_369);
    }
}
