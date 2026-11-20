//! The five-byte real's layout and each rule of its arithmetic, on values
//! whose results follow from the rule alone.

use super::{Five, BYTES};
use crate::Fault;

/// The value of `mantissa` (top bit set) at exponent byte `exponent`.
fn real(negative: bool, mantissa: u32, exponent: u8) -> Five {
    let sign = if negative { 1 << 31 } else { 0 };
    let [a, b, c, d] = ((mantissa & !(1 << 31)) | sign).to_le_bytes();
    Five::from_bytes([a, b, c, d, exponent])
}

fn bytes(value: Result<Five, Fault>) -> [u8; BYTES] {
    value.expect("a value").to_bytes()
}

#[test]
fn a_value_is_kept_mantissa_first_with_its_sign_in_the_top_bit() {
    assert_eq!(Five::ONE.to_bytes(), [0, 0, 0, 0, 0x81]);
    assert_eq!(Five::from_int(-1).to_bytes(), [0, 0, 0, 0x80, 0x81]);
    assert_eq!(Five::from_int(3).to_bytes(), [0, 0, 0, 0x40, 0x82]);
    assert_eq!(Five::from_int(i32::MIN).to_bytes(), [0, 0, 0, 0x80, 0xA0]);
    assert_eq!(Five::from_int(0), Five::ZERO);
    assert_eq!(Five::ZERO.to_bytes(), [0; BYTES]);
    assert!(Five::from_bytes([0; BYTES]).is_zero());
    assert!(
        !Five::from_bytes([0, 0, 0, 0x40, 0]).is_zero(),
        "an exponent of nought holds a number"
    );
}

#[test]
fn a_double_read_from_a_file_is_cut_saturated_or_flushed_as_recorded() {
    let read = |x: f64| Five::from_double_bits(x.to_bits()).to_bytes();
    let two_to = |n: i32| f64::from_bits(u64::try_from(1023 + n).expect("normal") << 52);
    assert_eq!(
        read(0.1),
        [0xCC, 0xCC, 0xCC, 0x4C, 0x7D],
        "cut, not rounded"
    );
    assert_eq!(read(1.0 / 3.0), [0xAA, 0xAA, 0xAA, 0x2A, 0x7F]);
    assert_eq!(read(-(1.0 + two_to(-33))), [0, 0, 0, 0x80, 0x81]);
    assert_eq!(read(1.0 - two_to(-34)), [0xFF, 0xFF, 0xFF, 0x7F, 0x80]);
    assert_eq!(read(two_to(-129)), [0; BYTES]);
    assert_eq!(read(1.7e38), [0x3C, 0x9E, 0xC9, 0x7F, 0xFF]);
    assert_eq!(read(1e300), [0xFF, 0xFF, 0xFF, 0x7F, 0xFF], "the largest");
    assert_eq!(read(0.0), [0; BYTES]);
}

#[test]
fn a_five_byte_real_is_a_double_exactly() {
    let double = |bytes: [u8; BYTES]| Five::from_bytes(bytes).to_double_bits();
    assert_eq!(double([0, 0, 0, 0x40, 0x81]), 1.5f64.to_bits());
    assert_eq!(double([0, 0, 0, 0xC0, 0x81]), (-1.5f64).to_bits());
    let scale = f64::from_bits(u64::try_from(1023 - 144).expect("normal") << 52);
    assert_eq!(
        double([0, 0, 0xF8, 0x3F, 0]),
        (f64::from(0xBFF8_u16) * scale).to_bits(),
        "a nought exponent's value"
    );
    assert_eq!(double([0; BYTES]), 0.0f64.to_bits());
}

#[test]
fn a_same_signed_sum_rounds_to_nearest_with_ties_to_odd() {
    let one = Five::ONE;
    // 1 + 2^-32 is a tie between 1 and 1 + 2^-31: 1 is even, so it rounds up.
    let tie = real(false, 1 << 31, 0x81 - 32);
    assert_eq!(
        bytes(one.add(tie)),
        real(false, (1 << 31) | 1, 0x81).to_bytes()
    );
    // From 1 + 2^-31, odd, the same tie stays.
    let odd = real(false, (1 << 31) | 1, 0x81);
    assert_eq!(bytes(odd.add(tie)), odd.to_bytes());
    // A quarter of the last place is lost.
    assert_eq!(
        bytes(one.add(real(false, 1 << 31, 0x81 - 33))),
        one.to_bytes()
    );
}

#[test]
fn a_difference_keeping_the_larger_exponent_is_truncated() {
    // 1.5 less a place and a quarter lies three quarters of a place above
    // the mantissa two below: truncated, not rounded up.
    let big = real(false, 3 << 30, 0x81);
    let small = real(false, 0xA000_0000, 0x62);
    assert_eq!(
        bytes(big.sub(small)),
        real(false, (3 << 30) - 2, 0x81).to_bytes()
    );
}

#[test]
fn a_difference_that_drops_a_place_rounds_to_nearest() {
    // 1 - 1.25 * 2^-32 lies three quarters of a place above 1 - 2^-31 in
    // the binade below: rounded up, where truncation would not.
    let small = real(false, 0xA000_0000, 0x61);
    assert_eq!(
        bytes(Five::ONE.sub(small)),
        real(false, u32::MAX, 0x80).to_bytes()
    );
}

#[test]
fn an_operand_more_than_32_places_down_is_ignored() {
    let big = real(false, (1 << 31) | 0x55, 0x81);
    for gap in [33u8, 40, 100] {
        let small = real(false, u32::MAX, 0x81 - gap);
        assert_eq!(bytes(big.sub(small)), big.to_bytes(), "{gap}");
        assert_eq!(bytes(big.add(small)), big.to_bytes(), "{gap}");
    }
}

#[test]
fn products_and_quotients_round_to_nearest() {
    let three = Five::from_int(3);
    let tenth = Five::ONE.div(Five::from_int(10)).expect("a value");
    assert_eq!(tenth.to_bytes(), [0xCD, 0xCC, 0xCC, 0x4C, 0x7D]);
    let third = Five::ONE.div(three).expect("a value");
    assert_eq!(third.to_bytes(), [0xAB, 0xAA, 0xAA, 0x2A, 0x7F]);
    assert_eq!(
        bytes(third.mul(three)),
        Five::ONE.to_bytes(),
        "a third rounded up, times three, is one"
    );
    assert_eq!(Five::ONE.div(Five::ZERO), Err(Fault::DivisionByZero));
}

#[test]
fn the_range_ends_in_an_error_above_and_in_zero_below() {
    let largest = real(false, u32::MAX, 0xFF);
    assert_eq!(largest.add(largest), Err(Fault::TooBig));
    let least = real(false, 0xC000_0000, 0x01);
    assert_eq!(bytes(least.mul(real(false, 1 << 31, 0x7F))), [0; BYTES]);
}

#[test]
fn square_roots_are_rounded_to_nearest() {
    let two = Five::from_int(2);
    assert_eq!(bytes(two.sqrt()), [0x34, 0xF3, 0x04, 0x35, 0x81]);
    assert_eq!(
        bytes(Five::from_int(16).sqrt()),
        Five::from_int(4).to_bytes()
    );
    let quarter = real(false, 1 << 31, 0x7F);
    assert_eq!(bytes(quarter.sqrt()), real(false, 1 << 31, 0x80).to_bytes());
    // Five's exponent is odd and it is no square: the root's search must
    // start above it.
    assert_eq!(
        bytes(Five::from_int(5).sqrt()),
        [0xDD, 0xBC, 0x1B, 0x0F, 0x82]
    );
    assert_eq!(Five::from_int(-1).sqrt(), Err(Fault::NegativeRoot));
}

#[test]
fn integers_are_truncated_toward_zero_and_floored_by_int() {
    let half = real(false, 1 << 31, 0x80);
    let one_and_half = real(false, 3 << 30, 0x81);
    assert_eq!(one_and_half.to_int(), Ok(1));
    assert_eq!(one_and_half.negated().to_int(), Ok(-1));
    assert_eq!(one_and_half.negated().floor(), Ok(-2));
    assert_eq!(half.negated().to_int(), Ok(0));
    assert_eq!(half.negated().floor(), Ok(-1));
    assert_eq!(Five::from_int(i32::MIN).to_int(), Ok(i32::MIN));
    assert_eq!(
        Five::from_int(i32::MIN).negated().to_int(),
        Err(Fault::TooBig)
    );
    assert_eq!(Five::from_i64(1 << 40).floor(), Err(Fault::TooBig));
}

#[test]
fn an_integer_product_past_an_integer_rounds_to_nearest_with_ties_to_odd() {
    // 2^33 + 3 needs 34 bits: its last two lie a place and a half below the
    // mantissa, so it rounds up.
    let value = Five::from_i64((1 << 33) + 3);
    assert_eq!(
        value.to_bytes(),
        real(false, (1 << 31) | 1, 0xA2).to_bytes()
    );
}

#[test]
fn values_compare_exactly() {
    let a = real(false, 0x8000_0001, 0x81);
    assert!(a > Five::ONE);
    assert!(a.negated() < Five::ONE.negated());
    assert!(Five::ZERO < Five::ONE && Five::ZERO > Five::ONE.negated());
    assert!(Five::from_bytes([0, 0, 0, 0x40, 0]) > Five::ZERO);
}

#[test]
fn int_refuses_two_to_the_31_of_either_sign() {
    let limit = Five::from_bytes([0, 0, 0, 0x80, 0xA0]);
    assert_eq!(limit.floor(), Err(Fault::TooBig));
    assert_eq!(limit.to_int(), Ok(i32::MIN), "an assignment takes it");
    let inside = Five::from_bytes([0xFF, 0xFF, 0xFF, 0xFF, 0x9F]);
    assert_eq!(inside.floor(), Ok(i32::MIN));
    assert_eq!(inside.to_int(), Ok(-2_147_483_647));
}
