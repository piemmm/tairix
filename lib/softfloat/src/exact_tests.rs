//! The core's rounding step and its exact operations.

use super::*;
use crate::mode::Rounding;

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
    let single = x.rounded(Rounding {
        mode: Mode::Nearest,
        bits: 24,
    });
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
