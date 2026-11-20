extern crate alloc;

use alloc::string::String;

use super::*;
use crate::text::{write, Format};
use tairix_riscos_arm::fpa::Exception;

/// RISC OS's floating-point status for an application.
const TRAPPING: u32 = tairix_riscos_kernel::os::APPLICATION_FPSR;

fn bits(x: f64) -> Six {
    Six::from_bits(x.to_bits())
}

fn read(text: &str) -> Result<Six, Fault> {
    Arithmetic::new(TRAPPING).read(&Written::val(text.as_bytes(), EXPONENT_DIGITS))
}

fn written(x: Six, at: u32) -> String {
    let digits = Arithmetic::new(TRAPPING).digits(x).unwrap();
    let mut out = String::new();
    write(&digits, Format::of_str(at), &mut out).unwrap();
    out
}

#[test]
fn a_double_is_kept_high_word_first() {
    assert_eq!(Six::ONE.to_bytes(), [0, 0, 0xF0, 0x3F, 0, 0, 0, 0]);
    let x = bits(-1234.5678e-9);
    assert_eq!(Six::from_bytes(x.to_bytes()), x);
}

#[test]
fn arithmetic_is_the_fpa_s_and_traps_as_the_program_has_it() {
    let mut a = Arithmetic::new(TRAPPING);
    assert_eq!(a.add(bits(0.1), bits(0.2)), Ok(bits(0.1 + 0.2)));
    assert_eq!(a.div(Six::ONE, Six::ZERO), Err(Fault::DivisionByZero));
    assert_eq!(
        a.mul(bits(1e308), bits(10.0)),
        Err(Fault::Floating(Exception::Overflow))
    );
    assert_eq!(
        a.sqrt(bits(-1.0)),
        Err(Fault::Floating(Exception::InvalidOperation))
    );
    assert_eq!(
        a.ln(Six::ZERO),
        Err(Fault::Floating(Exception::DivisionByZero))
    );
    assert_eq!(
        a.mul(bits(5e-324), bits(0.5)),
        Ok(Six::ZERO),
        "underflow is not trapped"
    );
    let mut quiet = Arithmetic::new(0);
    assert!(quiet.mul(bits(1e308), bits(10.0)).is_ok());
}

#[test]
fn negation_is_zero_less_the_value() {
    let mut a = Arithmetic::new(TRAPPING);
    assert_eq!(a.negate(Six::ZERO), Ok(Six::ZERO));
    assert_eq!(a.negate(bits(-0.0)), Ok(Six::ZERO));
    assert_eq!(a.negate(bits(2.5)), Ok(bits(-2.5)));
}

#[test]
fn conversions_to_integers_truncate_or_floor() {
    let mut a = Arithmetic::new(TRAPPING);
    assert_eq!(a.to_int(bits(-2_147_483_648.9)), Ok(i32::MIN));
    assert_eq!(
        a.to_int(bits(2_147_483_648.0)),
        Err(Fault::Floating(Exception::InvalidOperation))
    );
    assert_eq!(a.floor(bits(-0.5)), Ok(-1));
    assert_eq!(a.from_int(i32::MIN), Ok(bits(-2_147_483_648.0)));
}

#[test]
fn rad_and_deg_multiply_by_rounded_constants() {
    let mut a = Arithmetic::new(TRAPPING);
    assert_eq!(a.rad(Six::ONE), Ok(bits(core::f64::consts::PI / 180.0)));
    assert_eq!(a.deg(Six::ONE), Ok(bits(180.0 / core::f64::consts::PI)));
    assert_eq!(Six::PI, bits(core::f64::consts::PI));
}

#[test]
fn a_number_is_read_from_its_first_nineteen_digits() {
    assert_eq!(read("1E300"), Ok(bits(1e300)));
    assert_eq!(read("1E0001"), Ok(Six::ONE), "three exponent digits");
    assert_eq!(read("2.5E-324"), Ok(Six::from_bits(1)));
    assert_eq!(read("-0"), Ok(Six::ZERO));
    assert_eq!(read("1E309"), Err(Fault::Floating(Exception::Overflow)));
    // Twenty nines: the last is beyond what is read, so the value stays
    // short of one.
    assert_eq!(
        read("0.99999999999999999999"),
        read("0.9999999999999999999")
    );
}

#[test]
fn text_is_written_from_the_packed_digits() {
    assert_eq!(written(bits(1.0 / 3.0), 0x90A), "0.33333333333333331");
    assert_eq!(written(bits(-0.0), 0x90A), "-0");
    assert_eq!(written(bits(-0.0), 0x0101_0A0A), "-0.000000000E0   ");
    assert_eq!(written(bits(1e100), 0x0101_0A0A), "1.000000000E100 ");
    assert_eq!(written(bits(5e-324), 0x90A), "4.9406564584124654E-324");
    assert_eq!(written(bits(99_999.999_999_999_99), 0x0100_0A0A), "100000");
    assert_eq!(
        written(bits(6.455_197_844_915_191e17), 0x90A),
        "645519784491519100"
    );
    assert_eq!(written(bits(1e18), 0x90A), "1E18");
}
