use tairix_riscos_kernel::FRESH_FPSR;

use super::*;

/// A unit as a C program's FPA starts: invalid operation, division by zero
/// and overflow trapped.
fn unit() -> Unit {
    let mut unit = Unit::own();
    unit.set_fpsr(FRESH_FPSR);
    unit
}

#[test]
fn a_fused_multiply_add_rounds_to_extended_then_to_its_answer() {
    let one_and_a_bit = 1.0 + f64::EPSILON;
    // 1 + 2⁻⁵³ + 2⁻⁸⁰: once rounded, past the half and up; rounded first to
    // extended's 64 bits, the half exactly, and to even.
    let past_half = f64::from_bits(0x3CA0_0000_0200_0000);
    for (x, y, z, want) in [
        (past_half, 1.0, 1.0, 1.0),
        (2.0, 3.0, 4.0, 10.0),
        (0.1, 10.0, -1.0, 5.551_115_123_125_783e-17),
        (3.0, 1.0 / 3.0, -1.0, -5.551_115_123_125_783e-17),
        (
            one_and_a_bit,
            one_and_a_bit,
            -1.0,
            2.0 * f64::EPSILON + f64::EPSILON * f64::EPSILON,
        ),
        (1.0, 1.0, -1.0, 0.0),
        (1e-300, 1e-300, 1e-320, 1e-320),
    ] {
        let got = fused(&mut unit(), x, y, z, false).expect("no trap");
        assert_eq!(got.to_bits(), f64::to_bits(want), "{x:e}·{y:e} + {z:e}");
    }
}

#[test]
fn a_fused_multiply_add_s_flags_are_the_extended_rounding_s() {
    let mut u = unit();
    assert_eq!(fused(&mut u, 1e-300, 1e-300, 1e-320, false), Ok(1e-320));
    assert_eq!(
        u.fpsr() & 0x1F,
        0x10,
        "inexact, no underflow: the extended result is exactly a subnormal double"
    );
    let mut u = unit();
    assert_eq!(
        fused(&mut u, 1e300, 1e300, f64::NEG_INFINITY, false),
        Ok(f64::NEG_INFINITY)
    );
    assert_eq!(
        fused(&mut unit(), f64::INFINITY, 0.0, 1.0, false),
        Err(Exception::InvalidOperation)
    );
    assert_eq!(
        fused(&mut unit(), 1e308, 10.0, 0.0, false),
        Err(Exception::Overflow)
    );
}

#[test]
fn a_fused_multiply_add_takes_a_nan_as_the_fpa_s_product_and_sum_do() {
    let signalling = f64::from_bits(0x7FF0_0000_0000_0001);
    let mut quiet = Unit::own();
    let got = fused(&mut quiet, signalling, 1.0, 1.0, false).expect("no trap");
    assert!(
        got.is_nan() && got.to_bits() & 1 << 51 != 0,
        "quieted: {:x}",
        got.to_bits()
    );
    assert_eq!(quiet.fpsr() & 0x1F, 0x01, "an invalid operation, flagged");
    assert_eq!(
        fused(&mut unit(), signalling, 1.0, 1.0, false),
        Err(Exception::InvalidOperation),
        "trapped"
    );
    let nan = f64::from_bits(0x7FF8_0000_0000_1234);
    assert!(fused(&mut Unit::own(), 1.0, 1.0, nan, true)
        .expect("no trap")
        .is_nan());
}

#[test]
fn the_quotient_s_low_bits_are_the_nearest_whole_number_s() {
    for (x, y, want) in [
        (1.0, 1.0, 1),
        (1.0, -1.0, -1),
        (2.0, 0.5, 4),
        (7.5, 2.0, 4),
        (-7.5, 2.0, -4),
        (5.5, 3.0, 2),
        (2.5, 1.0, 2),
        (3.5, 1.0, 4),
        (10.0, 300.0, 0),
        (10.0, -2.0, -5),
        (-8.0, 1.0 / 3.0, -24),
        (1e300, 1e-300, 8_054_146),
        (1.0, 1e-320, 1_128_914_329),
    ] {
        assert_eq!(quotient_bits(x, y).cast_signed(), want, "{x:e} / {y:e}");
    }
    assert_eq!(quotient_bits(f64::NAN, 1.0), 0);
    assert_eq!(quotient_bits(1.0, f64::INFINITY), 0);
}
