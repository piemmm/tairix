//! The core's rounding step and its exact operations.

use tairix_fuzzseed::Prng;

use super::*;
use crate::mode::Rounding;

const MODES: [Mode; 4] = [Mode::Nearest, Mode::Down, Mode::Up, Mode::Zero];

/// An exact value whose significand is `sig` at the integer bit's place, with
/// `low` the 63 bits below a 64-bit significand.
fn value(sig: u64, low: u64, sticky: bool) -> Exact {
    Exact {
        negative: false,
        exp: 0,
        sig: u128::from(sig) << 63 | u128::from(low),
        sticky,
    }
}

#[test]
fn a_tie_rounds_to_even_away_or_by_direction_and_sticky_breaks_it() {
    let half = 1 << 62;
    for (sig, low, sticky, mode, want) in [
        (
            0x8000_0000_0000_0002,
            half,
            false,
            Mode::Nearest,
            0x8000_0000_0000_0002,
        ),
        (
            0x8000_0000_0000_0003,
            half,
            false,
            Mode::Nearest,
            0x8000_0000_0000_0004,
        ),
        (
            0x8000_0000_0000_0002,
            half,
            true,
            Mode::Nearest,
            0x8000_0000_0000_0003,
        ),
        (
            0x8000_0000_0000_0002,
            half,
            false,
            Mode::NearestAway,
            0x8000_0000_0000_0003,
        ),
        (
            0x8000_0000_0000_0002,
            half - 1,
            true,
            Mode::NearestAway,
            0x8000_0000_0000_0002,
        ),
        (
            0x8000_0000_0000_0002,
            0,
            true,
            Mode::Up,
            0x8000_0000_0000_0003,
        ),
        (
            0x8000_0000_0000_0002,
            half,
            false,
            Mode::Down,
            0x8000_0000_0000_0002,
        ),
        (
            0x8000_0000_0000_0002,
            half,
            false,
            Mode::Zero,
            0x8000_0000_0000_0002,
        ),
    ] {
        let rounded = value(sig, low, sticky).rounded(Rounding::extended(mode));
        assert_eq!(rounded.sig, want, "{sig:#x} {low:#x} {sticky} {mode:?}");
        assert!(rounded.inexact);
        assert!(!rounded.carried);
    }
    let exact = value(0x8000_0000_0000_0002, 0, false).rounded(Rounding::extended(Mode::Up));
    assert!(!exact.inexact, "nothing below is nothing to round");
}

#[test]
fn rounding_up_from_all_ones_carries_into_the_next_exponent() {
    let rounded = value(u64::MAX, 1 << 62, false).rounded(Rounding::extended(Mode::Nearest));
    assert_eq!(rounded.sig, INTEGER);
    assert!(rounded.carried && rounded.inexact);
}

#[test]
fn a_narrower_precision_rounds_at_its_own_last_bit() {
    // 24 bits kept, the last at bit 40, so the half below it is bit 39.
    let below = 1u64 << 39;
    let x = value(0x8000_0100_0000_0000 | below, 0, false);
    let single = x.rounded(Rounding::new(Mode::Nearest, 24).unwrap());
    assert_eq!(
        single.sig, 0x8000_0200_0000_0000,
        "a tie to the even last bit"
    );
    assert!(single.inexact);
}

#[test]
fn integers_round_in_every_mode() {
    let parts = |negative, exp, sig| Parts { negative, exp, sig };
    let two_and_a_half = parts(false, 1, 0xA000_0000_0000_0000);
    let wants = [
        (Mode::Nearest, 2),
        (Mode::Down, 2),
        (Mode::Up, 3),
        (Mode::Zero, 2),
        (Mode::NearestAway, 3),
    ];
    for (mode, want) in wants {
        assert_eq!(
            to_integer(two_and_a_half, mode),
            (Some(want), true),
            "{mode:?}"
        );
    }
    let minus_half = parts(true, -1, INTEGER);
    assert_eq!(to_integer(minus_half, Mode::Nearest), (Some(0), true));
    assert_eq!(to_integer(minus_half, Mode::NearestAway), (Some(1), true));
    assert_eq!(to_integer(minus_half, Mode::Down), (Some(1), true));
    let tiny = parts(false, -100, INTEGER);
    assert_eq!(to_integer(tiny, Mode::Up), (Some(1), true));
    assert_eq!(to_integer(tiny, Mode::NearestAway), (Some(0), true));
    assert_eq!(
        to_integer(parts(false, 64, INTEGER), Mode::Zero),
        (None, false)
    );
}

#[test]
fn a_perfect_square_has_an_exact_root() {
    for n in [1u64, 4, 9, 1 << 40, 0xFFFF_FFFF * 0xFFFF_FFFF] {
        let p = Parts {
            negative: false,
            exp: 63,
            sig: n,
        }
        .normalized();
        let root = square_root(p);
        assert!(!root.sticky, "{n}");
        let r = root.rounded(Rounding::extended(Mode::Zero));
        assert!(!r.inexact, "{n}");
    }
}

#[test]
fn a_rounding_keeps_one_to_sixty_four_bits_and_a_zero_divisor_divides_nothing() {
    assert_eq!(Rounding::new(Mode::Zero, 0), None);
    assert_eq!(Rounding::new(Mode::Zero, 65), None);
    assert_eq!(
        Rounding::new(Mode::Zero, 64),
        Some(Rounding::extended(Mode::Zero))
    );
    let one = Parts {
        negative: false,
        exp: 0,
        sig: 1 << 63,
    };
    assert_eq!(divide(one, Parts { sig: 0, ..one }), None);
    assert!(divide(one, one).is_some());
}

/// The root two bits of the radicand at a time, as long division takes it.
fn root_by_digits(m: u128) -> (u128, bool) {
    let (mut rest, mut root) = (0u128, 0u128);
    for n in (0..67).rev() {
        let pair = if n >= 34 {
            (m >> (2 * (n - 34))) & 3
        } else {
            0
        };
        rest = (rest << 2) | pair;
        let trial = (root << 2) | 1;
        root <<= 1;
        if rest >= trial {
            rest -= trial;
            root |= 1;
        }
    }
    (root, rest != 0)
}

#[test]
fn the_scaled_root_is_the_digit_by_digit_one() {
    let mut x = 0x2545_F491_4F6C_DD1Du128;
    let mut next = || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        x
    };
    let random = (0..20_000).map(|n| (next() >> (62 + n % 64)) & ((1 << 66) - 1));
    let squares = [1u128, 2, 3, 0xFFFF_FFFF, (1 << 33) - 1]
        .into_iter()
        .flat_map(|root| [root * root, root * root - 1, root * root + 1]);
    let edges = [0, 1, (1 << 66) - 1, 1 << 65, (1 << 64) - 1, 1 << 63];
    for m in random.chain(squares).chain(edges) {
        assert_eq!(root_scaled(m), root_by_digits(m), "{m:#x}");
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
        Mode::Zero => false,
        Mode::NearestAway => below >= half,
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

/// `x`'s significand and the exponent of its integer bit, rounded to `bits`
/// in `mode`.
fn rounded(x: Exact, mode: Mode, bits: u32) -> (u64, i32) {
    let x = x.normalized(true);
    let r = x.rounded(Rounding::new(mode, bits).unwrap());
    (r.sig, x.exp + i32::from(r.carried))
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
                    let sum = add(a, b, false);
                    let reference = reference(sum_negative, magnitude, false, -63, bits, mode);
                    assert_eq!(
                        (rounded(sum, mode, bits), sum.negative),
                        (reference, sum_negative)
                    );
                }
                let got = rounded(multiply(a, b), mode, bits);
                let reference = reference(negative, product, false, gap - 126, bits, mode);
                assert_eq!(got, reference, "{mode:?} {bits}");
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
                let got = rounded(divide(a, b).unwrap(), mode, bits);
                let reference = reference(true, q, rest != 0, -66 - 5, bits, mode);
                assert_eq!(got, reference, "{sa:#x}/{sb:#x} {mode:?} {bits}");
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
            let (sig, exp) = rounded(square_root(x), mode, 64);
            assert_eq!(exp, 0);
            u128::from(sig)
        };
        let down = root(Mode::Down);
        let up = root(Mode::Up);
        assert!(square(down) <= radicand && radicand < square(down + 1));
        assert!(square(up - 1) < radicand && radicand <= square(up));
        assert_eq!(root(Mode::Zero), down);
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
