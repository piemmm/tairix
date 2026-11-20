use super::*;
use crate::text::Written;

fn five(text: &str) -> Five {
    Five::read(&Written::val(text.as_bytes(), crate::five::EXPONENT_DIGITS)).unwrap()
}

#[test]
fn each_function_refuses_where_basic_v_does() {
    assert_eq!(five("8388608").sin(), Err(Fault::AccuracyLost));
    assert_eq!(five("-8388608").cos(), Err(Fault::AccuracyLost));
    assert!(five("8388607.99").tan().is_ok());
    assert_eq!(five("1.0000001").asn(), Err(Fault::ArcRange));
    assert_eq!(five("-2").acs(), Err(Fault::ArcRange));
    assert_eq!(five("89.5").exp(), Err(Fault::ExponentRange));
    assert_eq!(five("89.49").exp(), Err(Fault::TooBig));
    assert_eq!(five("-1E10").exp(), Ok(Five::ZERO));
    assert_eq!(Five::ZERO.ln(), Err(Fault::LogRange));
    assert_eq!(five("-1").log(), Err(Fault::LogRange));
}

#[test]
fn a_function_s_exact_values_are_exact() {
    assert_eq!(Five::ZERO.sin(), Ok(Five::ZERO));
    assert_eq!(Five::ZERO.cos(), Ok(Five::ONE));
    assert_eq!(Five::ZERO.atn(), Ok(Five::ZERO));
    assert_eq!(Five::ONE.acs(), Ok(Five::ZERO));
    assert_eq!(Five::ONE.ln(), Ok(Five::ZERO));
    assert_eq!(Five::ZERO.exp(), Ok(Five::ONE));
    assert_eq!(five("1000").log(), Ok(Five::from_int(3)));
    // π/2 correctly rounded is PI with its exponent one lower.
    let mut half = Five::PI.to_bytes();
    half[4] -= 1;
    assert_eq!(Five::ZERO.acs(), Ok(Five::from_bytes(half)));
}

#[test]
fn radians_and_degrees_use_basic_v_s_own_constants() {
    assert_eq!(
        Five::ONE.rad(),
        Ok(Five::PI.div(Five::from_int(180)).unwrap())
    );
    assert_eq!(
        Five::ONE.deg(),
        Ok(Five::from_int(180).div(Five::PI).unwrap())
    );
}

#[test]
fn a_small_integer_power_is_taken_by_squares() {
    let x = five("1.1");
    let square = x.mul(x).unwrap();
    let fourth = square.mul(square).unwrap();
    assert_eq!(x.pow_int(5), fourth.mul(x));
    assert_eq!(x.pow_int(-2), Five::ONE.div(x).and_then(|r| r.mul(r)));
    assert_eq!(Five::ZERO.pow_int(0), Ok(Five::ONE));
    assert_eq!(Five::ZERO.pow_int(-1), Err(Fault::DivisionByZero));
    assert_eq!(five("-2").pow_int(63), Ok(five("-9223372036854775808")));
    assert_eq!(five("-2").pow_int(-64), Ok(five("5.42101086242752217E-20")));
}

#[test]
fn a_real_power_meets_the_faults_its_parts_do() {
    assert_eq!(five("-2").pow_int(64), Err(Fault::LogRange));
    assert_eq!(five("2").pow_int(128), Err(Fault::TooBig));
    assert_eq!(five("2").pow(five("130")), Err(Fault::ExponentRange));
    assert_eq!(Five::ZERO.pow(five("0.5")), Err(Fault::LogRange));
    assert_eq!(Five::ZERO.pow(five("-0.5")), Err(Fault::DivisionByZero));
    assert_eq!(Five::ZERO.pow(five("-64")), Err(Fault::LogRange));
    assert_eq!(five("-2").pow(five("-63.5")), Err(Fault::LogRange));
    // The integer part, by squares, overflows though the power does not.
    assert_eq!(five("0.25").pow(five("-63.5")), Err(Fault::TooBig));
    assert_eq!(five("4").pow(five("0.5")), Ok(five("2")));
    assert_eq!(five("2").pow(five("-200")), Ok(Five::ZERO));
}
