extern crate std;

use alloc::string::ToString;

use tairix_fuzzseed::Prng;

use super::*;

fn shown(exponent: u16, sig: u64) -> String {
    extended(Real::new(false, exponent, sig))
}

/// The values the reference worked out exactly with 6000-digit decimal
/// arithmetic, rounded to nineteen digits, ties to even.
#[test]
fn values_across_the_whole_range_are_written_exactly() {
    for (exponent, sig, expected) in [
        (0x7FFE, u64::MAX, "1.189731495357231765E+4932"),
        (0, 1, "3.645199531882474603E-4951"),
        (0, 0x7FFF_FFFF_FFFF_FFFF, "3.362103143112093506E-4932"),
        (1, 1 << 63, "3.362103143112093506E-4932"),
        (0x4000, 0xC90F_DAA2_2168_C235, "3.141592653589793239E+0000"),
        (0x3FFB, 0xCCCC_CCCC_CCCC_CCCD, "1.000000000000000000E-0001"),
        (0x3FFD, 0xAAAA_AAAA_AAAA_AAAB, "3.333333333333333333E-0001"),
        (0x3FFF, 1 << 63, "1.000000000000000000E+0000"),
        (0x403F, 1 << 63, "1.844674407370955162E+0019"),
        (0x3FFF, 3, "3.252606517456513302E-0019"),
        (0x4041, 0xAD78_EBC5_AC62_0000, "1.000000000000000000E+0020"),
    ] {
        assert_eq!(shown(exponent, sig), expected, "{exponent:04X} {sig:016X}");
    }
}

#[test]
fn a_tie_rounds_to_the_even_digit() {
    // 10000000000000000005, 10000000000000000015 and 2^64 - 1, whole numbers
    // whose twentieth digit is a final five.
    let whole = |n: u64| shown(0x403E, n);
    assert_eq!(
        whole(10_000_000_000_000_000_005),
        "1.000000000000000000E+0019"
    );
    assert_eq!(
        whole(10_000_000_000_000_000_015),
        "1.000000000000000002E+0019"
    );
    assert_eq!(
        whole(10_000_000_000_000_000_025),
        "1.000000000000000002E+0019"
    );
    assert_eq!(
        whole(10_000_000_000_000_000_026),
        "1.000000000000000003E+0019"
    );
    assert_eq!(whole(u64::MAX), "1.844674407370955162E+0019");
}

#[test]
fn rounding_up_from_all_nines_carries_into_the_next_power() {
    assert_eq!(
        shown(0x3F9E, 0xCAD2_F7F5_359A_3B3E),
        "1.000000000000000000E-0029"
    );
}

#[test]
fn signs_and_the_values_with_no_digits_are_named() {
    assert_eq!(
        extended(Real::new(true, 0x3FFF, 1 << 63)),
        "-1.000000000000000000E+0000"
    );
    assert_eq!(extended(Real::ZERO), "0");
    assert_eq!(extended(Real::new(true, 0, 0)), "-0");
    assert_eq!(extended(Real::new(false, 0x1234, 0)), "0", "a pseudo-zero");
    assert_eq!(extended(Real::INFINITY), "Infinity");
    assert_eq!(extended(Real::INFINITY.with_sign(true)), "-Infinity");
    assert_eq!(extended(Real::INDEFINITE), "-NaN");
    for value in [Real::new(true, 0x7FFE, u64::MAX), Real::new(true, 0, 1)] {
        assert!(extended(value).len() <= super::super::MOST_VALUE);
    }
}

/// Rust's own formatting of a double to nineteen digits, the exponent
/// spelled as here.
fn hosts(x: f64) -> String {
    let text = std::format!("{x:.18e}");
    let (mantissa, exponent) = text.split_once('e').unwrap();
    let exponent: i32 = exponent.parse().unwrap();
    let mark = if exponent < 0 { '-' } else { '+' };
    std::format!("{mantissa}E{mark}{:04}", exponent.unsigned_abs())
}

#[test]
fn every_double_is_written_as_the_host_writes_it() {
    let mut rng = Prng::new(0x8087_DEC1);
    for n in 0..20_000 {
        // Edge-heavy: subnormals, the ends of the range, and anywhere.
        let bits = match n % 4 {
            0 => rng.next_u64() & 0x000F_FFFF_FFFF_FFFF,
            1 => rng.next_u64() | 0x7FE0_0000_0000_0000,
            _ => rng.next_u64(),
        } & 0x7FFF_FFFF_FFFF_FFFF;
        let x = f64::from_bits(bits);
        if !x.is_finite() || x == 0.0 {
            continue;
        }
        let (value, _) = Real::from_double(bits);
        assert_eq!(extended(value), hosts(x), "{bits:016X}");
    }
    assert_eq!(hosts(0.5), "5.000000000000000000E-0001".to_string());
}
