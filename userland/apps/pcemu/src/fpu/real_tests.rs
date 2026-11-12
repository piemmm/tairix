use super::*;

#[test]
fn each_class_is_told_by_its_bits() {
    let cases = [
        (Real::ZERO, Class::Zero),
        (Real::ONE, Class::Normal),
        (
            Real::new(true, 0x3FFF, 0x4000_0000_0000_0000),
            Class::Unnormal,
        ),
        (Real::new(false, 0x1234, 0), Class::PseudoZero),
        (Real::new(false, 0, 1), Class::Denormal),
        // A pseudo-denormal is a denormal: its exponent field is zero.
        (Real::new(false, 0, INTEGER), Class::Denormal),
        (Real::INFINITY, Class::Infinity),
        // The 8087 reads the special exponent's fraction, not its integer
        // bit.
        (Real::new(false, EXP_SPECIAL, 0), Class::Infinity),
        (Real::new(false, EXP_SPECIAL, 1), Class::Nan),
        (Real::INDEFINITE, Class::Nan),
    ];
    for (x, class) in cases {
        assert_eq!(x.class(), class, "{x:?}");
    }
}

#[test]
fn ten_bytes_hold_the_value_low_first() {
    let x = Real::new(true, 0x4000, 0xC90F_DAA2_2168_C234);
    let bytes = x.to_bytes();
    assert_eq!(
        bytes,
        [0x34, 0xC2, 0x68, 0x21, 0xA2, 0xDA, 0x0F, 0xC9, 0x00, 0xC0]
    );
    assert_eq!(Real::from_bytes(bytes), x);
}

#[test]
fn integers_load_exactly() {
    assert_eq!(Real::from_int(0), Real::ZERO);
    assert_eq!(Real::from_int(1), Real::ONE);
    assert_eq!(
        Real::from_int(-3),
        Real::new(true, 0x4000, 0xC000_0000_0000_0000)
    );
    assert_eq!(
        Real::from_int(i64::MIN),
        Real::new(true, 0x3FFF + 63, INTEGER)
    );
    assert_eq!(
        Real::from_int(i64::MAX),
        Real::new(false, 0x3FFF + 62, u64::MAX << 1)
    );
    assert_eq!(Real::from_magnitude(true, 0), Real::ZERO.with_sign(true));
    assert_eq!(
        Real::from_magnitude(false, u64::MAX),
        Real::new(false, 0x3FFF + 63, u64::MAX)
    );
}

#[test]
fn packed_decimal_loads_its_eighteen_digits_and_sign() {
    // -123456789012345678.
    let bytes = [0x78, 0x56, 0x34, 0x12, 0x90, 0x78, 0x56, 0x34, 0x12, 0x80];
    assert_eq!(
        Real::from_bcd(bytes),
        Real::from_int(-123_456_789_012_345_678)
    );
    let mut negative_zero = [0u8; 10];
    negative_zero[9] = 0x80;
    assert_eq!(Real::from_bcd(negative_zero), Real::ZERO.with_sign(true));
    // A nibble past 9 counts its own value; the sign byte's pad is
    // ignored.
    let mut odd = [0u8; 10];
    odd[0] = 0x0F;
    odd[9] = 0x7F;
    assert_eq!(Real::from_bcd(odd), Real::from_int(15));
}

#[test]
fn binary_reals_load_exactly_a_denormal_as_its_unnormal() {
    for &value in &[1.0f64, -2.5, 1e300, -1e-300, f64::MAX, f64::MIN_POSITIVE] {
        let (x, denormal) = Real::from_double(value.to_bits());
        assert!(!denormal);
        assert_eq!(x.class(), Class::Normal, "{value}");
        // Back to the double's own fields, which must be the same bits.
        let fraction = (x.sig << 1) >> 12;
        let exponent = u64::try_from(i32::from(x.exponent()) - BIAS + 1023).unwrap();
        let bits = (u64::from(x.negative()) << 63) | (exponent << 52) | fraction;
        assert_eq!(bits, value.to_bits(), "{value}");
    }
    let (x, denormal) = Real::from_double(1);
    assert!(denormal);
    assert_eq!(x, Real::new(false, 0x3FFF - 1022, 1 << 11));
    assert_eq!(x.class(), Class::Unnormal);
    let (x, denormal) = Real::from_single(0x0040_0000);
    assert!(denormal);
    assert_eq!(x, Real::new(false, 0x3FFF - 126, 1 << 62));
    let (x, denormal) = Real::from_single(0x8000_0000);
    assert!(!denormal);
    assert_eq!(x, Real::ZERO.with_sign(true));
    let (x, _) = Real::from_single(0x7F80_0000);
    assert_eq!(x, Real::INFINITY);
    // A NaN keeps its fraction at the top, integer bit set.
    let (x, _) = Real::from_single(0xFFC0_0001);
    assert_eq!(
        x,
        Real::new(true, EXP_SPECIAL, INTEGER | (1 << 62) | (1 << 40))
    );
}

#[test]
fn each_format_has_its_indefinite() {
    assert_eq!(Binary::SINGLE.indefinite(), 0xFFC0_0000);
    assert_eq!(Binary::DOUBLE.indefinite(), 0xFFF8_0000_0000_0000);
    assert_eq!(Real::INDEFINITE.to_bytes()[6..], [0, 0xC0, 0xFF, 0xFF]);
    assert_eq!(
        (Binary::SINGLE.min_exp(), Binary::SINGLE.max_exp()),
        (-126, 127)
    );
    assert_eq!(
        (Binary::DOUBLE.min_exp(), Binary::DOUBLE.max_exp()),
        (-1022, 1023)
    );
}
