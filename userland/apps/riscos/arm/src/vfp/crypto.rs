//! The Cryptographic Extension's instructions: the AES round steps and the
//! SHA-1 and SHA-256 hash steps, on quadwords, as Arm's pseudocode defines
//! them.
//!
//! These are a processor's instructions, emulated: the guest's cipher, never
//! one TAIRiX keeps a secret with. They run in constant time all the same, so
//! a guest's keys cannot leak through the host's caches: the S-box is the
//! inverse in GF(2^8) by exponentiation and the affine map, every byte at
//! once, with no table indexed by data.

use crate::word::low;

const ONES: u128 = 0x0101_0101_0101_0101_0101_0101_0101_0101;

/// Each byte doubled in GF(2^8), modulo AES's `x^8 + x^4 + x^3 + x + 1`.
const fn double(bytes: u128) -> u128 {
    let high = (bytes >> 7) & ONES;
    ((bytes << 1) & !ONES) ^ (high * 0x1B)
}

/// Each byte's product in GF(2^8), in constant time.
const fn multiply(mut a: u128, b: u128) -> u128 {
    let mut product = 0;
    let mut bit = 0;
    while bit < 8 {
        product ^= a & (((b >> bit) & ONES) * 0xFF);
        a = double(a);
        bit += 1;
    }
    product
}

/// Each byte's inverse in GF(2^8), zero's being zero: `x^254`.
const fn inverse(bytes: u128) -> u128 {
    let mut power = bytes;
    let mut result = ONES;
    let mut step = 1;
    while step < 8 {
        power = multiply(power, power);
        result = multiply(result, power);
        step += 1;
    }
    result
}

/// Each byte rotated left by `k`.
const fn rotate(bytes: u128, k: u32) -> u128 {
    let keep = ONES * ((0xFF >> k) as u128);
    ((bytes & keep) << k) | ((bytes >> (8 - k)) & (ONES * ((1u128 << k) - 1)))
}

/// AES's S-box on every byte.
pub const fn sub_bytes(state: u128) -> u128 {
    let b = inverse(state);
    b ^ rotate(b, 1) ^ rotate(b, 2) ^ rotate(b, 3) ^ rotate(b, 4) ^ (ONES * 0x63)
}

/// AES's inverse S-box on every byte.
pub const fn inv_sub_bytes(state: u128) -> u128 {
    inverse(rotate(state, 1) ^ rotate(state, 3) ^ rotate(state, 6) ^ (ONES * 0x05))
}

/// The bytes of `state` taken in `order`: byte `i` of the result is byte
/// `order[i]` of the state.
const fn permute(state: u128, order: &[u32; 16]) -> u128 {
    let mut result = 0;
    let mut i = 0;
    while i < 16 {
        result |= ((state >> (8 * order[i])) & 0xFF) << (8 * i);
        i += 1;
    }
    result
}

/// `ShiftRows` on the column-major state: row `r` rotated left by `r`.
const SHIFT_ROWS: [u32; 16] = [0, 5, 10, 15, 4, 9, 14, 3, 8, 13, 2, 7, 12, 1, 6, 11];
const INV_SHIFT_ROWS: [u32; 16] = [0, 13, 10, 7, 4, 1, 14, 11, 8, 5, 2, 15, 12, 9, 6, 3];

/// `AESE`: the round key added, then `ShiftRows` and `SubBytes`.
#[must_use]
pub const fn aese(state: u128, key: u128) -> u128 {
    sub_bytes(permute(state ^ key, &SHIFT_ROWS))
}

/// `AESD`: the round key added, then the inverse `ShiftRows` and
/// `SubBytes`.
#[must_use]
pub const fn aesd(state: u128, key: u128) -> u128 {
    inv_sub_bytes(permute(state ^ key, &INV_SHIFT_ROWS))
}

/// Each column's bytes rotated down by `rows` places, so row `r` of the
/// result holds row `r + rows` of the column.
const fn rotate_columns(state: u128, rows: u32) -> u128 {
    let mut result = 0;
    let mut column = 0;
    while column < 4 {
        let rotated = word(state, column).rotate_right(8 * rows);
        result |= (rotated as u128) << (32 * column);
        column += 1;
    }
    result
}

/// `AESMC`: `MixColumns`, each output byte `2a ^ 3b ^ c ^ d` of its column's
/// bytes from its own row down.
#[must_use]
pub const fn aesmc(state: u128) -> u128 {
    let (a, b, c, d) = (
        state,
        rotate_columns(state, 1),
        rotate_columns(state, 2),
        rotate_columns(state, 3),
    );
    let twice = double(a ^ b);
    twice ^ b ^ c ^ d
}

/// `AESIMC`: the inverse `MixColumns`, its coefficients 14, 11, 13 and 9.
#[must_use]
pub const fn aesimc(state: u128) -> u128 {
    let (a, b, c, d) = (
        state,
        rotate_columns(state, 1),
        rotate_columns(state, 2),
        rotate_columns(state, 3),
    );
    multiply(a, ONES * 14) ^ multiply(b, ONES * 11) ^ multiply(c, ONES * 13) ^ multiply(d, ONES * 9)
}

const fn word(value: u128, index: u32) -> u32 {
    low(crate::word::low64(value >> (32 * index)))
}

const fn words(w: [u32; 4]) -> u128 {
    (w[3] as u128) << 96 | (w[2] as u128) << 64 | (w[1] as u128) << 32 | w[0] as u128
}

const fn choose(x: u32, y: u32, z: u32) -> u32 {
    ((y ^ z) & x) ^ z
}

const fn parity(x: u32, y: u32, z: u32) -> u32 {
    x ^ y ^ z
}

const fn majority(x: u32, y: u32, z: u32) -> u32 {
    (x & y) | ((x | y) & z)
}

/// A SHA-1 hash step's function.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Sha1 {
    /// `SHA1C`.
    Choose,
    /// `SHA1P`.
    Parity,
    /// `SHA1M`.
    Majority,
}

/// `SHA1C`, `SHA1P` and `SHA1M`: four rounds on `abcd`, `e` the low word of
/// `e`, with the four words of `wk`.
#[must_use]
pub const fn sha1_hash(function: Sha1, abcd: u128, e: u128, wk: u128) -> u128 {
    let mut state = abcd;
    let mut carried = word(e, 0);
    let mut round = 0;
    while round < 4 {
        let (b, c, d) = (word(state, 1), word(state, 2), word(state, 3));
        let mixed = match function {
            Sha1::Choose => choose(b, c, d),
            Sha1::Parity => parity(b, c, d),
            Sha1::Majority => majority(b, c, d),
        };
        carried = carried
            .wrapping_add(word(state, 0).rotate_left(5))
            .wrapping_add(mixed)
            .wrapping_add(word(wk, round));
        state = (state & !(0xFFFF_FFFF_u128 << 32)) | ((b.rotate_left(30) as u128) << 32);
        let top = word(state, 3);
        state = state << 32 | carried as u128;
        carried = top;
        round += 1;
    }
    state
}

/// `SHA1H`: the low word rotated left by 30.
#[must_use]
pub const fn sha1h(value: u128) -> u128 {
    word(value, 0).rotate_left(30) as u128
}

/// `SHA1SU0`: the first message-schedule step.
#[must_use]
pub const fn sha1su0(d: u128, n: u128, m: u128) -> u128 {
    let joined = (n & u64::MAX as u128) << 64 | d >> 64;
    d ^ joined ^ m
}

/// `SHA1SU1`: the second message-schedule step.
#[must_use]
pub const fn sha1su1(d: u128, m: u128) -> u128 {
    let t = d ^ (m >> 32);
    let w0 = word(t, 0).rotate_left(1);
    let w1 = word(t, 1).rotate_left(1);
    let w2 = word(t, 2).rotate_left(1);
    let w3 = word(t, 3).rotate_left(1) ^ word(t, 0).rotate_left(2);
    words([w0, w1, w2, w3])
}

const fn sigma0(x: u32) -> u32 {
    x.rotate_right(2) ^ x.rotate_right(13) ^ x.rotate_right(22)
}

const fn sigma1(x: u32) -> u32 {
    x.rotate_right(6) ^ x.rotate_right(11) ^ x.rotate_right(25)
}

/// Four SHA-256 rounds on `x` (`abcd`) and `y` (`efgh`) with the words of
/// `wk`: the new `x` for `SHA256H`, the new `y` for `SHA256H2`.
const fn sha256_hash(mut x: u128, mut y: u128, wk: u128, part1: bool) -> u128 {
    let mut round = 0;
    while round < 4 {
        let chs = choose(word(y, 0), word(y, 1), word(y, 2));
        let maj = majority(word(x, 0), word(x, 1), word(x, 2));
        let t = word(y, 3)
            .wrapping_add(sigma1(word(y, 0)))
            .wrapping_add(chs)
            .wrapping_add(word(wk, round));
        // The new e and a, which the rotation moves into place.
        let new_e = t.wrapping_add(word(x, 3));
        let new_a = t.wrapping_add(sigma0(word(x, 0))).wrapping_add(maj);
        x = (x & !(0xFFFF_FFFF_u128 << 96)) | (new_e as u128) << 96;
        y = (y & !(0xFFFF_FFFF_u128 << 96)) | (new_a as u128) << 96;
        let (x_top, y_top) = (x >> 96, y >> 96);
        x = x << 32 | y_top;
        y = y << 32 | x_top;
        round += 1;
    }
    if part1 {
        x
    } else {
        y
    }
}

/// `SHA256H`: `abcd` after four rounds.
#[must_use]
pub const fn sha256h(abcd: u128, efgh: u128, wk: u128) -> u128 {
    sha256_hash(abcd, efgh, wk, true)
}

/// `SHA256H2`: `efgh` after the same four rounds.
#[must_use]
pub const fn sha256h2(efgh: u128, abcd: u128, wk: u128) -> u128 {
    sha256_hash(abcd, efgh, wk, false)
}

/// `SHA256SU0`: the first message-schedule step.
#[must_use]
pub const fn sha256su0(d: u128, m: u128) -> u128 {
    let t = (m & 0xFFFF_FFFF) << 96 | d >> 32;
    let mut result = [0u32; 4];
    let mut e = 0;
    while e < 4 {
        let elt = word(t, e);
        let elt = elt.rotate_right(7) ^ elt.rotate_right(18) ^ (elt >> 3);
        result[e as usize] = elt.wrapping_add(word(d, e));
        e += 1;
    }
    words(result)
}

/// `SHA256SU1`: the second message-schedule step.
#[must_use]
pub const fn sha256su1(d: u128, n: u128, m: u128) -> u128 {
    let t0 = (m & 0xFFFF_FFFF) << 96 | n >> 32;
    let mut result = [0u32; 4];
    let mut e = 0;
    while e < 4 {
        let source = if e < 2 {
            word(m, 2 + e)
        } else {
            result[(e - 2) as usize]
        };
        let elt = source.rotate_right(17) ^ source.rotate_right(19) ^ (source >> 10);
        result[e as usize] = elt.wrapping_add(word(d, e)).wrapping_add(word(t0, e));
        e += 1;
    }
    words(result)
}

#[cfg(test)]
mod tests;
