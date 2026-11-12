extern crate std;

use tairix_fuzzseed::Prng;

use super::*;
use crate::fpu::real::Class;

const MODES: [Mode; 4] = [Mode::Nearest, Mode::Down, Mode::Up, Mode::Chop];

/// A double's value as parts.
fn parts(x: f64) -> Parts {
    Parts::of(Real::from_double(x.to_bits()).0)
}

/// A rounded register result read back as a double, where it is a normal
/// one the double holds exactly.
fn double(x: Real) -> Option<f64> {
    if x.class() != Class::Normal || x.sig & ((1 << 11) - 1) != 0 {
        return None;
    }
    let exponent = i32::from(x.exponent()) - BIAS + 1023;
    let field = u64::try_from(exponent)
        .ok()
        .filter(|&e| (1..2047).contains(&e))?;
    let fraction = (x.sig << 1) >> 12;
    Some(f64::from_bits(
        (u64::from(x.negative()) << 63) | (field << 52) | fraction,
    ))
}

/// A single's bits as a double holds them exactly.
fn single(x: Real) -> Option<f32> {
    if x.class() != Class::Normal || x.sig & ((1 << 40) - 1) != 0 {
        return None;
    }
    let exponent = i32::from(x.exponent()) - BIAS + 127;
    let field = u32::try_from(exponent)
        .ok()
        .filter(|&e| (1..255).contains(&e))?;
    let fraction = u32::try_from((x.sig << 1) >> 41).ok()?;
    Some(f32::from_bits(
        (u32::from(x.negative()) << 31) | (field << 23) | fraction,
    ))
}

/// A random double of moderate exponent.
fn random_double(rng: &mut Prng, spread: i32) -> f64 {
    let sign = rng.next_u64() >> 63;
    let fraction = rng.next_u64() >> 12;
    let offset =
        i32::try_from(rng.below(usize::try_from(2 * spread + 1).unwrap())).unwrap() - spread;
    let field = u64::try_from(1023 + offset).unwrap();
    f64::from_bits((sign << 63) | (field << 52) | fraction)
}

fn rounded(x: Exact, mode: Mode, bits: u32) -> Real {
    round(x, Rounding { mode, bits }, flag::ALL).value.unwrap()
}

#[test]
fn sums_products_quotients_and_roots_round_as_ieee_doubles_do() {
    let mut rng = Prng::new(0x8087_0053);
    let mut checked = 0;
    for _ in 0..20_000 {
        let (a, b) = (random_double(&mut rng, 40), random_double(&mut rng, 40));
        let mut check = |exact: Exact, expected: f64| {
            if expected == 0.0 || !expected.is_normal() {
                return;
            }
            let got = double(rounded(exact, Mode::Nearest, 53));
            assert_eq!(
                got.map(f64::to_bits),
                Some(expected.to_bits()),
                "{a:e} {b:e}"
            );
            checked += 1;
        };
        check(add(parts(a), parts(b), false), a + b);
        check(add(parts(a), parts(b), true), a - b);
        check(multiply(parts(a), parts(b)), a * b);
        check(divide(parts(a), parts(b)), a / b);
        check(square_root(parts(a.abs())), a.abs().sqrt());
    }
    assert!(checked > 90_000, "{checked}");
}

#[test]
fn at_24_bits_they_round_as_ieee_singles_do() {
    let mut rng = Prng::new(0x8087_0024);
    for _ in 0..20_000 {
        let bits = |rng: &mut Prng| {
            let x = random_double(rng, 20);
            // A single's worth of significand.
            f32::from_bits(u32::try_from(x.to_bits() >> 32).unwrap() & 0x807F_FFFF | 0x3F00_0000)
        };
        let (a, b) = (bits(&mut rng), bits(&mut rng));
        let as_parts = |x: f32| Parts::of(Real::from_single(x.to_bits()).0);
        for (exact, expected) in [
            (add(as_parts(a), as_parts(b), false), a + b),
            (multiply(as_parts(a), as_parts(b)), a * b),
            (divide(as_parts(a), as_parts(b)), a / b),
            (square_root(as_parts(a.abs())), a.abs().sqrt()),
        ] {
            if expected == 0.0 || !expected.is_normal() {
                continue;
            }
            let got = single(rounded(exact, Mode::Nearest, 24));
            assert_eq!(
                got.map(f32::to_bits),
                Some(expected.to_bits()),
                "{a:e} {b:e}"
            );
        }
    }
}

/// The 64-bit significand and exponent `magnitude × 2^scale` rounds to at
/// `bits` in `mode`, `sticky` saying more lies below: rounded the long way,
/// apart from the code under test.
fn reference(
    negative: bool,
    magnitude: u128,
    sticky: bool,
    scale: i32,
    bits: u32,
    mode: Mode,
) -> (u64, i32) {
    let top = magnitude.ilog2();
    let cut = i32::try_from(top + 1).unwrap() - i32::try_from(bits).unwrap();
    let (mut kept, mut scale, below, half, exact) = if cut <= 0 {
        let shift = u32::try_from(-cut).unwrap();
        (
            magnitude << shift,
            scale - i32::try_from(shift).unwrap(),
            0u128,
            1u128,
            !sticky,
        )
    } else {
        let cut = u32::try_from(cut).unwrap();
        let below = magnitude & ((1 << cut) - 1);
        (
            magnitude >> cut,
            scale + i32::try_from(cut).unwrap(),
            below,
            1 << (cut - 1),
            below == 0 && !sticky,
        )
    };
    let up = match mode {
        Mode::Nearest => below > half || (below == half && (sticky || kept & 1 == 1)),
        Mode::Up => !exact && !negative,
        Mode::Down => !exact && negative,
        Mode::Chop => false,
    };
    if up {
        kept += 1;
        if kept >> bits != 0 {
            kept >>= 1;
            scale += 1;
        }
    }
    let sig = u64::try_from(kept << (64 - bits)).unwrap();
    (sig, scale + i32::try_from(bits).unwrap() - 1)
}

fn random_sig(rng: &mut Prng) -> u64 {
    rng.next_u64() | INTEGER
}

#[test]
fn sums_and_products_round_exactly_at_every_precision_and_mode() {
    let mut rng = Prng::new(0x8087_0064);
    for _ in 0..20_000 {
        let (sa, sb) = (random_sig(&mut rng), random_sig(&mut rng));
        let gap = i32::try_from(rng.below(60)).unwrap();
        let negative = rng.next_u64() & 1 == 1;
        let a = Parts {
            negative: false,
            exp: gap,
            sig: sa,
        };
        let b = Parts {
            negative,
            exp: 0,
            sig: sb,
        };
        // Exactly: sa × 2^gap ± sb, scaled by 2^-63.
        let wide = u128::from(sa) << gap;
        let (magnitude, sum_negative) = if negative {
            if wide >= u128::from(sb) {
                (wide - u128::from(sb), false)
            } else {
                (u128::from(sb) - wide, true)
            }
        } else {
            (wide + u128::from(sb), false)
        };
        let product = u128::from(sa) * u128::from(sb);
        for mode in MODES {
            for bits in [24, 53, 64] {
                if magnitude != 0 {
                    let got = rounded(add(a, b, false), mode, bits);
                    let (sig, exp) = reference(sum_negative, magnitude, false, -63, bits, mode);
                    assert_eq!(
                        (got.sig, i32::from(got.exponent()) - BIAS, got.negative()),
                        (sig, exp, sum_negative)
                    );
                }
                let got = rounded(multiply(a, b), mode, bits);
                let (sig, exp) = reference(negative, product, false, gap - 126, bits, mode);
                assert_eq!(
                    (got.sig, i32::from(got.exponent()) - BIAS),
                    (sig, exp),
                    "{mode:?} {bits}"
                );
            }
        }
    }
}

#[test]
fn quotients_round_exactly_at_every_precision_and_mode() {
    let mut rng = Prng::new(0x8087_0D17);
    for _ in 0..20_000 {
        let (sa, sb) = (random_sig(&mut rng), random_sig(&mut rng));
        // 67 quotient bits by restoring division, the integer bit first.
        let (mut rest, divisor, mut q) = (u128::from(sa), u128::from(sb), 0u128);
        for _ in 0..67 {
            q <<= 1;
            if rest >= divisor {
                rest -= divisor;
                q |= 1;
            }
            rest <<= 1;
        }
        let a = Parts {
            negative: false,
            exp: 0,
            sig: sa,
        };
        let b = Parts {
            negative: true,
            exp: 5,
            sig: sb,
        };
        for mode in MODES {
            for bits in [24, 53, 64] {
                let got = rounded(divide(a, b), mode, bits);
                let (sig, exp) = reference(true, q, rest != 0, -66 - 5, bits, mode);
                assert_eq!(
                    (got.sig, i32::from(got.exponent()) - BIAS),
                    (sig, exp),
                    "{sa:#x}/{sb:#x} {mode:?} {bits}"
                );
            }
        }
    }
}

/// `x²` in 256 bits, high half first, for `x` below 2^66.
fn square(x: u128) -> (u128, u128) {
    let mask = u128::from(u64::MAX);
    let (high, low) = (x >> 64, x & mask);
    let middle = 2 * high * low;
    let (low_part, carry) = (low * low).overflowing_add((middle & mask) << 64);
    (high * high + (middle >> 64) + u128::from(carry), low_part)
}

#[test]
fn square_roots_bracket_their_radicand_in_every_mode() {
    let mut rng = Prng::new(0x8087_5097);
    for _ in 0..20_000 {
        let sig = random_sig(&mut rng);
        let odd = rng.next_u64() & 1 == 1;
        // x = sig × 2^(e − 63) in [1, 4); its root in [1, 2) is R × 2^-63,
        // and R² against sig × 2^(63 + e).
        let e = i32::from(odd);
        let radicand = (0u128, u128::from(sig) << (63 + u32::from(odd)));
        let x = Parts {
            negative: false,
            exp: e,
            sig,
        };
        let root = |mode| {
            let r = rounded(square_root(x), mode, 64);
            assert_eq!(i32::from(r.exponent()) - BIAS, 0);
            u128::from(r.sig)
        };
        let down = root(Mode::Down);
        let up = root(Mode::Up);
        assert!(square(down) <= radicand && radicand < square(down + 1));
        assert!(square(up - 1) < radicand && radicand <= square(up));
        assert_eq!(root(Mode::Chop), down);
        // Nearest: below the midpoint's square, the root rounded down.
        let quadruple = (
            u128::from(sig) >> (63 - u32::from(odd)),
            u128::from(sig) << (65 + u32::from(odd)),
        );
        let nearest = if down == up || square(2 * down + 1) > quadruple {
            down
        } else {
            up
        };
        assert_eq!(root(Mode::Nearest), nearest);
    }
}

#[test]
fn a_tiny_result_is_denormalised_and_flagged_as_the_8087_flags_it() {
    let tiny = Exact::of(Parts {
        negative: false,
        exp: -16_383 - 3,
        sig: INTEGER | 1,
    });
    let masked = round(tiny, Rounding::extended(Mode::Nearest), flag::ALL);
    let x = masked.value.unwrap();
    assert_eq!((x.exponent(), x.sig), (0, INTEGER >> 4));
    assert_eq!(masked.flags, flag::UNDERFLOW | flag::PRECISION);
    // Exact tininess is flagged only when chopping.
    let exact = Exact::of(Parts {
        negative: false,
        exp: -16_383 - 3,
        sig: INTEGER,
    });
    assert_eq!(
        round(exact, Rounding::extended(Mode::Nearest), flag::ALL).flags,
        0
    );
    assert_eq!(
        round(exact, Rounding::extended(Mode::Chop), flag::ALL).flags,
        flag::UNDERFLOW
    );
    // Unmasked, the exponent moves up by the wrap and the significand stays.
    let unmasked = round(
        tiny,
        Rounding::extended(Mode::Nearest),
        flag::ALL & !flag::UNDERFLOW,
    );
    let x = unmasked.value.unwrap();
    assert_eq!(unmasked.flags, flag::UNDERFLOW);
    assert_eq!(i32::from(x.exponent()), -3 + WRAP);
    assert_eq!(x.sig, INTEGER | 1);
}

#[test]
fn an_overflow_answers_by_the_mode_and_infinity_when_chopping() {
    let huge = |negative| {
        Exact::of(Parts {
            negative,
            exp: 16_384,
            sig: INTEGER,
        })
    };
    let finite = |negative| Real::new(negative, EXP_MAX, u64::MAX);
    for (mode, negative, expected) in [
        (Mode::Nearest, false, Real::INFINITY),
        (Mode::Chop, true, Real::INFINITY.with_sign(true)),
        (Mode::Up, false, Real::INFINITY),
        (Mode::Up, true, finite(true)),
        (Mode::Down, false, finite(false)),
        (Mode::Down, true, Real::INFINITY.with_sign(true)),
    ] {
        let result = round(huge(negative), Rounding::extended(mode), flag::ALL);
        assert_eq!(result.value, Some(expected), "{mode:?} {negative}");
        assert_eq!(result.flags, flag::OVERFLOW | flag::PRECISION);
    }
    // At 24 bits the largest finite value has 24 ones.
    let result = round(
        huge(false),
        Rounding {
            mode: Mode::Down,
            bits: 24,
        },
        flag::ALL,
    );
    assert_eq!(
        result.value,
        Some(Real::new(false, EXP_MAX, 0xFFFF_FF00_0000_0000))
    );
    // Unmasked: wrapped down, no precision flag.
    let result = round(
        huge(false),
        Rounding::extended(Mode::Nearest),
        flag::ALL & !flag::OVERFLOW,
    );
    assert_eq!(result.flags, flag::OVERFLOW);
    assert_eq!(
        i32::from(result.value.unwrap().exponent()),
        16_384 + BIAS - WRAP
    );
}

#[test]
fn an_unnormal_operand_leaves_a_product_or_quotient_unnormal_and_a_sum_follows_the_larger() {
    let unnormal = Parts {
        negative: false,
        exp: 0,
        sig: INTEGER >> 2,
    };
    let one = Parts {
        negative: false,
        exp: 0,
        sig: INTEGER,
    };
    let x = rounded(multiply(unnormal, one), Mode::Nearest, 64);
    assert_eq!((x.exponent(), x.sig), (0x3FFF, INTEGER >> 2));
    // The divider's one place of normalisation, which a normal quotient
    // below one takes, leaves it unnormal still.
    let x = rounded(divide(unnormal, one), Mode::Nearest, 64);
    assert_eq!((x.exponent(), x.sig), (0x3FFE, INTEGER >> 1));
    // The normal one is the larger: normalized.
    let x = rounded(add(unnormal, one, false), Mode::Nearest, 64);
    assert_eq!((x.exponent(), x.sig), (0x3FFF, INTEGER | INTEGER >> 2));
    // A big unnormal plus a small normal stays unnormal at its exponent.
    let big = Parts {
        negative: false,
        exp: 10,
        sig: INTEGER >> 1,
    };
    let x = rounded(add(big, one, false), Mode::Nearest, 64);
    assert_eq!(x.exponent(), 0x3FFF + 10);
    assert_eq!(x.sig, (INTEGER >> 1) + (INTEGER >> 10));
    // A pseudo-zero of any exponent adds nothing.
    let pseudo = Parts {
        negative: false,
        exp: 5_000,
        sig: 0,
    };
    assert_eq!(
        rounded(add(pseudo, one, false), Mode::Nearest, 64),
        Real::ONE
    );
}

#[test]
fn binary_stores_round_to_the_format_and_denormalise_below_it() {
    let one = Parts {
        negative: false,
        exp: 0,
        sig: INTEGER,
    };
    assert_eq!(
        round_binary(one, Binary::SINGLE, Mode::Nearest, flag::ALL),
        (Some(0x3F80_0000), 0)
    );
    let third = parts(1.0 / 3.0);
    assert_eq!(
        round_binary(third, Binary::DOUBLE, Mode::Nearest, flag::ALL),
        (Some((1.0f64 / 3.0).to_bits()), 0)
    );
    let (bits, flags) = round_binary(third, Binary::SINGLE, Mode::Nearest, flag::ALL);
    assert_eq!(
        (bits, flags),
        (Some(u64::from((1.0f32 / 3.0).to_bits())), flag::PRECISION)
    );
    // The smallest single denormal, and below it underflow to zero.
    let small = Parts {
        negative: false,
        exp: -149,
        sig: INTEGER,
    };
    assert_eq!(
        round_binary(small, Binary::SINGLE, Mode::Nearest, flag::ALL),
        (Some(1), 0)
    );
    let smaller = Parts {
        negative: true,
        exp: -151,
        sig: INTEGER,
    };
    assert_eq!(
        round_binary(smaller, Binary::SINGLE, Mode::Nearest, flag::ALL),
        (Some(0x8000_0000), flag::UNDERFLOW | flag::PRECISION)
    );
    assert_eq!(
        round_binary(smaller, Binary::SINGLE, Mode::Down, flag::ALL),
        (Some(0x8000_0001), flag::UNDERFLOW | flag::PRECISION)
    );
    let huge = Parts {
        negative: false,
        exp: 128,
        sig: INTEGER,
    };
    assert_eq!(
        round_binary(huge, Binary::SINGLE, Mode::Nearest, flag::ALL),
        (Some(0x7F80_0000), flag::OVERFLOW | flag::PRECISION)
    );
    assert_eq!(
        round_binary(
            huge,
            Binary::SINGLE,
            Mode::Chop,
            flag::ALL & !flag::OVERFLOW
        ),
        (None, flag::OVERFLOW)
    );
    assert_eq!(
        round_binary(
            small,
            Binary::DOUBLE,
            Mode::Nearest,
            flag::ALL & !flag::UNDERFLOW
        ),
        (Some(u64::from_le_bytes([0, 0, 0, 0, 0, 0, 0xA0, 0x36])), 0),
        "a double reaches 2^-149"
    );
}

#[test]
fn integers_round_in_each_mode() {
    let value = |x: f64| parts(x).normalized();
    for (x, mode, expected) in [
        (2.5, Mode::Nearest, 2),
        (3.5, Mode::Nearest, 4),
        (2.5, Mode::Up, 3),
        (2.5, Mode::Down, 2),
        (-2.5, Mode::Down, 3),
        (-2.5, Mode::Chop, 2),
        (0.25, Mode::Nearest, 0),
        (0.5, Mode::Nearest, 0),
        (0.75, Mode::Nearest, 1),
        (1e18, Mode::Nearest, 1_000_000_000_000_000_000),
    ] {
        assert_eq!(to_integer(value(x), mode).0, Some(expected), "{x} {mode:?}");
    }
    assert_eq!(
        to_integer(value(2.0f64.powi(64)), Mode::Nearest),
        (None, false)
    );
    assert_eq!(
        to_integer(value(2.0f64.powi(63)), Mode::Nearest),
        (Some(1 << 63), false)
    );
    assert!(to_integer(value(0.1), Mode::Up).1);
}

#[test]
fn a_remainder_is_exact_and_partial_past_63_places() {
    let (rest, quotient) = remainder(parts(7.5).normalized(), parts(2.0));
    assert_eq!(quotient, Some(3));
    assert_eq!(double(rounded(rest, Mode::Nearest, 64)), Some(1.5));
    let (rest, quotient) = remainder(parts(-7.0).normalized(), parts(3.0));
    assert_eq!(quotient, Some(2));
    assert_eq!(double(rounded(rest, Mode::Nearest, 64)), Some(-1.0));
    // 2^100 by 3: the first pass takes 63 places off and is incomplete.
    let (rest, quotient) = remainder(parts(2.0f64.powi(100)).normalized(), parts(3.0));
    assert_eq!(quotient, None);
    let x = rounded(rest, Mode::Nearest, 64);
    assert!(i32::from(x.exponent()) - BIAS <= 100 - 62);
    let (rest, quotient) = remainder(parts(1.0).normalized(), parts(3.0));
    assert_eq!(
        (quotient, double(rounded(rest, Mode::Nearest, 64))),
        (Some(0), Some(1.0))
    );
}

#[test]
fn an_exponent_far_past_every_format_still_rounds() {
    for (exp, expected) in [(i32::MAX, Real::INFINITY), (i32::MIN, Real::ZERO)] {
        let x = Exact {
            negative: false,
            exp,
            sig: 1 << 126,
            sticky: true,
        };
        let masked = round(x, Rounding::extended(Mode::Nearest), flag::ALL);
        assert_eq!(masked.value, Some(expected), "{exp}");
        // Unmasked too: past even the wrap, the masked response with the
        // flag.
        let unmasked = round(x, Rounding::extended(Mode::Nearest), 0);
        assert_ne!(unmasked.flags & (flag::OVERFLOW | flag::UNDERFLOW), 0);
    }
}
