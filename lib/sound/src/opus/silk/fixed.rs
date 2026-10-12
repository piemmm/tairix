//! The fixed-point arithmetic SILK is defined in (the reference
//! implementation's `SigProc_FIX.h`, `macros.h` and `Inlines.h`).
//!
//! SILK's decoder is integer arithmetic end to end, so a decode is
//! bit-exact with the reference's. Where the reference's products can wrap
//! two's-complement on hardware these wrap too, so no stream, however
//! malformed, can make a step panic or diverge from it.

/// `(a · b₁₆) >> 16`, `b₁₆` the low 16 bits of `b`, signed.
pub(super) const fn smulwb(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i16 as i64) >> 16) as i32
}

pub(super) const fn smlawb(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add(smulwb(b, c))
}

/// The high 16 bits of each multiplied.
pub(super) const fn smultt(a: i32, b: i32) -> i32 {
    (a >> 16) * (b >> 16)
}

/// The low 16 bits of each, signed, multiplied.
pub(super) const fn smulbb(a: i32, b: i32) -> i32 {
    (a as i16 as i32) * (b as i16 as i32)
}

pub(super) const fn smlabb(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add(smulbb(b, c))
}

/// `(a · b) >> 16` at full width.
pub(super) const fn smulww(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) >> 16) as i32
}

pub(super) const fn smlaww(a: i32, b: i32, c: i32) -> i32 {
    (a as i64 + ((b as i64 * c as i64) >> 16)) as i32
}

/// `(a · b) >> 32`.
pub(super) const fn smmul(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) >> 32) as i32
}

pub(super) const fn mla(a: i32, b: i32, c: i32) -> i32 {
    a.wrapping_add(b.wrapping_mul(c))
}

/// `a >> shift`, rounded to nearest; `shift` at least one.
pub(super) const fn rshift_round(a: i32, shift: u32) -> i32 {
    if shift == 1 {
        (a >> 1) + (a & 1)
    } else {
        ((a >> (shift - 1)) + 1) >> 1
    }
}

pub(super) const fn rshift_round64(a: i64, shift: u32) -> i64 {
    if shift == 1 {
        (a >> 1) + (a & 1)
    } else {
        ((a >> (shift - 1)) + 1) >> 1
    }
}

/// `a` clamped between the two limits, in either order.
pub(super) const fn limit(a: i32, first: i32, second: i32) -> i32 {
    let (low, high) = if first > second {
        (second, first)
    } else {
        (first, second)
    };
    if a > high {
        high
    } else if a < low {
        low
    } else {
        a
    }
}

pub(super) const fn sat16(a: i32) -> i16 {
    if a > i16::MAX as i32 {
        i16::MAX
    } else if a < i16::MIN as i32 {
        i16::MIN
    } else {
        a as i16
    }
}

/// `a << shift`, saturating rather than overflowing.
pub(super) const fn lshift_sat32(a: i32, shift: u32) -> i32 {
    limit(a, i32::MIN >> shift, i32::MAX >> shift) << shift
}

pub(super) const fn lshift(a: i32, shift: u32) -> i32 {
    ((a as u32) << shift) as i32
}

pub(super) const fn add_sat32(a: i32, b: i32) -> i32 {
    a.saturating_add(b)
}

pub(super) const fn sub_sat32(a: i32, b: i32) -> i32 {
    a.saturating_sub(b)
}

/// The next value of SILK's pseudo-random generator.
pub(super) const fn rand(seed: i32) -> i32 {
    907_633_515i32.wrapping_add(seed.wrapping_mul(196_314_165))
}

pub(super) const fn clz32(a: i32) -> u32 {
    (a as u32).leading_zeros()
}

/// The leading zeros of `a`, and the seven bits below its leading one.
pub(super) const fn clz_frac(a: i32) -> (u32, i32) {
    let lz = clz32(a);
    (lz, ror32(a, 24 - lz as i32) & 0x7F)
}

pub(super) const fn ror32(a: i32, rot: i32) -> i32 {
    let x = a as u32;
    if rot == 0 {
        a
    } else if rot < 0 {
        x.rotate_left(rot.unsigned_abs()) as i32
    } else {
        x.rotate_right(rot as u32) as i32
    }
}

/// An approximation of `√x`.
pub(super) const fn sqrt_approx(x: i32) -> i32 {
    if x <= 0 {
        return 0;
    }
    let (lz, frac) = clz_frac(x);
    let mut y = if lz & 1 != 0 { 32768 } else { 46214 };
    y >>= lz >> 1;
    smlawb(y, y, smulbb(213, frac))
}

/// `(a << q) / b`, approximately, `b` not zero.
pub(super) const fn div32_varq(a: i32, b: i32, q: i32) -> i32 {
    let a_headroom = clz32(a.wrapping_abs()) as i32 - 1;
    let mut a_normal = lshift(a, a_headroom as u32);
    let b_headroom = clz32(b.wrapping_abs()) as i32 - 1;
    let b_normal = lshift(b, b_headroom as u32);
    let divisor = b_normal >> 16;
    if divisor == 0 {
        return 0;
    }
    let inverse = (i32::MAX >> 2) / divisor;
    let mut result = smulwb(a_normal, inverse);
    a_normal = a_normal.wrapping_sub(lshift(smmul(b_normal, result), 3));
    result = smlawb(result, a_normal, inverse);
    let shift = 29 + a_headroom - b_headroom - q;
    if shift < 0 {
        lshift_sat32(result, (-shift) as u32)
    } else if shift < 32 {
        result >> shift
    } else {
        0
    }
}

/// `(1 << q) / b`, approximately, `b` not zero.
pub(super) const fn inverse32_varq(b: i32, q: i32) -> i32 {
    let headroom = clz32(b.wrapping_abs()) as i32 - 1;
    let normal = lshift(b, headroom as u32);
    let divisor = normal >> 16;
    if divisor == 0 {
        return 0;
    }
    let inverse = (i32::MAX >> 2) / divisor;
    let mut result = lshift(inverse, 16);
    let error = lshift((1 << 29) - smulwb(normal, inverse), 3);
    result = smlaww(result, error, inverse);
    let shift = 61 - headroom - q;
    if shift <= 0 {
        lshift_sat32(result, (-shift) as u32)
    } else if shift < 32 {
        result >> shift
    } else {
        0
    }
}

/// `2^(x / 128)`, approximately.
pub(super) const fn log2lin(x: i32) -> i32 {
    if x < 0 {
        return 0;
    }
    if x >= 3967 {
        return i32::MAX;
    }
    let out = 1i32 << (x >> 7);
    let frac = x & 0x7F;
    let curve = smlawb(frac, smulbb(frac, 128 - frac), -174);
    if x < 2048 {
        out + ((out * curve) >> 7)
    } else {
        mla(out, out >> 7, curve)
    }
}

#[cfg(test)]
mod tests {
    use super::{div32_varq, inverse32_varq, log2lin, sqrt_approx};

    #[test]
    fn the_approximations_stay_near_the_exact_values() {
        for x in [1, 7, 100, 4096, 65536, 1 << 20, 123_456_789, i32::MAX] {
            let root = f64::from(sqrt_approx(x));
            let exact = f64::from(x).sqrt();
            assert!((root - exact).abs() <= exact * 0.02 + 1.0, "{x}");
        }
        for q in [0, 100, 1000, 2047, 2048, 3000, 3966] {
            let value = f64::from(log2lin(q));
            let exact = 2f64.powf(f64::from(q) / 128.0);
            assert!((value - exact).abs() <= exact * 0.01 + 1.0, "{q}");
        }
        assert!((div32_varq(1 << 20, 3, 0) - (1 << 20) / 3).abs() <= 1);
        assert!((inverse32_varq(1000, 30) - (1 << 30) / 1000).abs() <= 2);
    }
}
