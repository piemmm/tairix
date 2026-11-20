//! A fixed-capacity unsigned integer, wide enough to write any finite
//! double-extended value out whole and to scale a decimal value into that
//! range, so decimal conversion allocates nothing.

use core::cmp::Ordering;

/// Words held. The widest use is the fraction of the FPA's least denormal,
/// 16 509 bits below the point.
const WORDS: usize = 516;

/// An unsigned integer of at most [`WORDS`] 32-bit words.
#[derive(Clone)]
pub struct Big {
    /// Least significant first; every word from `len` up is zero.
    words: [u32; WORDS],
    len: usize,
}

/// The low word of `x`.
const fn low(x: u64) -> u32 {
    let b = x.to_le_bytes();
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// A count of words as bits; no count here passes [`WORDS`].
fn bits_of(words: usize) -> u32 {
    u32::try_from(words).map_or(u32::MAX, |words| words.saturating_mul(32))
}

impl Big {
    /// The value `n`.
    pub fn from_u128(n: u128) -> Self {
        let mut big = Self {
            words: [0; WORDS],
            len: 4,
        };
        let bytes = n.to_le_bytes();
        for (word, chunk) in big.words.iter_mut().zip(bytes.as_chunks::<4>().0) {
            *word = u32::from_le_bytes(*chunk);
        }
        big.trim();
        big
    }

    fn trim(&mut self) {
        while self.len > 0 && self.words[self.len - 1] == 0 {
            self.len -= 1;
        }
    }

    fn word(&self, i: usize) -> u32 {
        self.words.get(i).copied().unwrap_or(0)
    }

    /// Whether the value is zero.
    pub const fn is_zero(&self) -> bool {
        self.len == 0
    }

    /// The bits the value needs: none for zero.
    pub fn bit_len(&self) -> u32 {
        self.len.checked_sub(1).map_or(0, |top| {
            bits_of(top) + (32 - self.words[top].leading_zeros())
        })
    }

    /// The words in use multiplied by `m`, and the word carried out of them.
    fn scale(&mut self, m: u32) -> u32 {
        let mut carry = 0u64;
        for word in &mut self.words[..self.len] {
            let product = u64::from(*word) * u64::from(m) + carry;
            *word = low(product);
            carry = product >> 32;
        }
        low(carry)
    }

    /// Multiplied by `m`; false, the value then meaningless, where the
    /// product does not fit.
    #[must_use]
    pub fn mul(&mut self, m: u32) -> bool {
        let carry = self.scale(m);
        if carry != 0 {
            let Some(word) = self.words.get_mut(self.len) else {
                return false;
            };
            *word = carry;
            self.len += 1;
        }
        self.trim();
        true
    }

    /// Plus `a`; false, the value then meaningless, where the sum does not
    /// fit.
    #[must_use]
    pub fn add(&mut self, a: u32) -> bool {
        let mut carry = u64::from(a);
        let mut at = 0;
        while carry != 0 {
            let Some(word) = self.words.get_mut(at) else {
                return false;
            };
            let sum = u64::from(*word) + carry;
            *word = low(sum);
            carry = sum >> 32;
            at += 1;
        }
        self.len = self.len.max(at);
        self.trim();
        true
    }

    /// Multiplied by `m` as a number `width` words wide, `width` no less
    /// than the words in use: the product within them, and the word carried
    /// out of them.
    pub fn mul_within(&mut self, width: usize, m: u32) -> u32 {
        let carry = self.scale(m);
        if carry != 0 && self.len < width.min(WORDS) {
            self.words[self.len] = carry;
            self.len += 1;
            return 0;
        }
        self.trim();
        carry
    }

    /// Multiplied by five to the `n`; false where the product does not fit.
    #[must_use]
    pub fn mul_pow5(&mut self, n: u32) -> bool {
        // The largest power of five a word holds.
        const FIVE_13: u32 = 1_220_703_125;
        let mut left = n;
        while left >= 13 {
            if !self.mul(FIVE_13) {
                return false;
            }
            left -= 13;
        }
        self.mul(5u32.pow(left))
    }

    /// Divided by `d`, which is not zero: the remainder.
    pub fn div(&mut self, d: u32) -> u32 {
        let mut rest = 0u64;
        for word in self.words[..self.len].iter_mut().rev() {
            let at = rest << 32 | u64::from(*word);
            *word = low(at / u64::from(d));
            rest = at % u64::from(d);
        }
        self.trim();
        low(rest)
    }

    /// Word `i` of the value times two to `32 × step + bits`, `bits` below 32.
    fn shifted_word(&self, step: usize, bits: u32, i: usize) -> u32 {
        let high = i.checked_sub(step).map_or(0, |j| self.word(j) << bits);
        let below = match (bits, i.checked_sub(step + 1)) {
            (1..=31, Some(j)) => self.word(j) >> (32 - bits),
            _ => 0,
        };
        high | below
    }

    /// Shifted left `n` bits; false where the result does not fit.
    #[must_use]
    pub fn shl(&mut self, n: u32) -> bool {
        if self.is_zero() || n == 0 {
            return true;
        }
        let total = self.bit_len().saturating_add(n);
        let (Ok(step), Ok(len)) = (usize::try_from(n / 32), usize::try_from(total.div_ceil(32)))
        else {
            return false;
        };
        if len > WORDS {
            return false;
        }
        // From the top down, each word read before it is overwritten.
        for i in (0..len).rev() {
            let word = self.shifted_word(step, n % 32, i);
            self.words[i] = word;
        }
        self.len = len;
        self.trim();
        true
    }

    /// The 128 bits from bit `from` up.
    fn bits_at(&self, from: u32) -> u128 {
        let Ok(first) = usize::try_from(from / 32) else {
            return 0;
        };
        let wide = (0..4u32).fold(0u128, |acc, k| {
            acc | u128::from(self.word(first + k as usize)) << (32 * k)
        });
        match from % 32 {
            0 => wide,
            offset => wide >> offset | u128::from(self.word(first + 4)) << (128 - offset),
        }
    }

    /// The value's top `n` bits, 1 to 127 of them, its top bit at bit
    /// `n − 1` of the answer; and whether any bit below them is set.
    pub fn top(&self, n: u32) -> (u128, bool) {
        let len = self.bit_len();
        if len <= n {
            return (self.bits_at(0) << (n - len), false);
        }
        let from = len - n;
        let Ok(whole) = usize::try_from(from / 32) else {
            return (0, true);
        };
        let partial = self.word(whole) & ((1 << (from % 32)) - 1);
        let below = partial != 0 || self.words[..whole].iter().any(|&word| word != 0);
        (self.bits_at(from), below)
    }

    /// Where the value is at least `d` times two to `shift`, that subtracted
    /// and true.
    pub fn sub_shifted(&mut self, d: &Self, shift: u32) -> bool {
        if d.is_zero() {
            return true;
        }
        let (have, need) = (self.bit_len(), d.bit_len().saturating_add(shift));
        let Ok(step) = usize::try_from(shift / 32) else {
            return false;
        };
        let bits = shift % 32;
        if have < need {
            return false;
        }
        if have == need {
            let order = (0..self.len)
                .rev()
                .map(|i| self.words[i].cmp(&d.shifted_word(step, bits, i)))
                .find(|&order| order != Ordering::Equal);
            if order == Some(Ordering::Less) {
                return false;
            }
        }
        let mut borrow = false;
        for i in 0..self.len {
            let (difference, under) = self.words[i].overflowing_sub(d.shifted_word(step, bits, i));
            let (difference, under_again) = difference.overflowing_sub(u32::from(borrow));
            self.words[i] = difference;
            borrow = under || under_again;
        }
        self.trim();
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn value(big: &Big) -> u128 {
        big.bits_at(0)
    }

    #[test]
    fn small_values_survive_every_operation() {
        let n = 0x1234_5678_9ABC_DEF0;
        let mut big = Big::from_u128(n);
        assert!(big.mul(1000));
        assert_eq!(value(&big), n * 1000);
        assert_eq!(big.div(1000), 0);
        assert_eq!(value(&big), n);
        assert_eq!(big.div(7), u32::try_from(n % 7).unwrap());
        assert_eq!(value(&big), n / 7);
        let mut big = Big::from_u128(n);
        assert!(big.shl(37));
        assert_eq!(value(&big), n << 37);
        assert_eq!(big.bit_len(), 61 + 37);
        assert!(big.sub_shifted(&Big::from_u128(n), 37));
        assert!(big.is_zero());
        assert!(!big.sub_shifted(&Big::from_u128(1), 0));
    }

    #[test]
    fn a_product_carried_out_of_a_zeroed_top_word_is_kept() {
        let mut big = Big::from_u128(0x8000_0000);
        assert!(big.mul(2));
        assert_eq!(value(&big), 1 << 32);
        let mut big = Big::from_u128(0x8000_0000);
        assert_eq!(big.mul_within(1, 2), 1);
        assert!(big.is_zero());
        let mut big = Big::from_u128(0x8000_0000);
        assert_eq!(big.mul_within(2, 2), 0);
        assert_eq!(value(&big), 1 << 32);
    }

    #[test]
    fn a_subtraction_is_refused_only_where_the_value_is_less() {
        let d = Big::from_u128(0xFFFF_FFFF_FFFF);
        let mut equal = Big::from_u128(0xFFFF_FFFF_FFFF << 3);
        assert!(equal.sub_shifted(&d, 3));
        assert!(equal.is_zero());
        let mut less = Big::from_u128((0xFFFF_FFFF_FFFF << 3) - 1);
        assert!(!less.sub_shifted(&d, 3));
        assert_eq!(value(&less), (0xFFFF_FFFF_FFFF << 3) - 1);
        let mut borrowing = Big::from_u128(1 << 100);
        assert!(borrowing.sub_shifted(&Big::from_u128(1), 0));
        assert_eq!(value(&borrowing), (1 << 100) - 1);
    }

    #[test]
    fn the_top_bits_and_what_lies_below_them_are_read() {
        let mut big = Big::from_u128(0b1011);
        assert!(big.shl(200));
        assert_eq!(big.top(4), (0b1011, false));
        assert_eq!(big.top(3), (0b101, true));
        assert_eq!(big.top(127), (0b1011 << 123, false));
        let mut sticky = Big::from_u128((1 << 100) | 1);
        assert!(sticky.shl(64));
        assert_eq!(sticky.top(1), (1, true));
        assert_eq!(sticky.top(100), (1 << 99, true));
        assert_eq!(sticky.top(101), ((1 << 100) | 1, false));
        assert_eq!(Big::from_u128(5).top(127), (5 << 124, false));
    }

    #[test]
    fn a_power_of_five_fills_the_widths_decimal_conversion_needs() {
        let mut big = Big::from_u128(1);
        assert!(big.mul_pow5(4988));
        // 4988 log2(5) is 11 581.6.
        assert_eq!(big.bit_len(), 11_582);
        let mut wide = Big::from_u128(1);
        assert!(wide.shl(bits_of(WORDS) - 1));
        assert!(!wide.shl(1));
        assert!(!wide.mul(3));
    }
}
