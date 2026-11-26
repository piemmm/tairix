//! Literal values held to the specification: suffixes, ranges, the
//! significand's truncation, and hexadecimal floats exactly rounded.

use super::value;
use crate::diag::{Code, Span, Stop};
use crate::eval::Value;
use crate::lex::number;

/// The value of the literal `text`, `enable f16;` in effect.
fn read(text: &str) -> Result<Value, Stop> {
    let (length, shape) = number(text.as_bytes(), 0).expect("a literal");
    assert_eq!(length, text.len(), "{text} is one literal");
    value(text, shape, Span::default(), true)
}

fn code(text: &str) -> Code {
    match read(text) {
        Err(Stop::Error(code, ..)) => code,
        other => panic!("{text}: {other:?}"),
    }
}

#[test]
fn integers_take_their_suffix_s_type_and_its_range() {
    assert_eq!(read("123"), Ok(Value::AbstractInt(123)));
    assert_eq!(read("0x7fffffffi"), Ok(Value::I32(i32::MAX)));
    assert_eq!(read("0xffffffffu"), Ok(Value::U32(u32::MAX)));
    assert_eq!(read("4294967295u"), Ok(Value::U32(u32::MAX)));
    assert_eq!(read("0x7fffffffffffffff"), Ok(Value::AbstractInt(i64::MAX)));
    for text in [
        "2147483648i",
        "0x80000000i",
        "4294967296u",
        "9223372036854775808",
        "0x10000000000000000",
    ] {
        assert_eq!(code(text), Code::LiteralRange, "{text}");
    }
}

#[test]
fn decimal_floats_round_once_to_their_type() {
    assert_eq!(read("1.5"), Ok(Value::AbstractFloat(1.5)));
    assert_eq!(read("0.e+4f"), Ok(Value::F32(0.0)));
    assert_eq!(read(".01"), Ok(Value::AbstractFloat(0.01)));
    assert_eq!(read("1e-3"), Ok(Value::AbstractFloat(0.001)));
    assert_eq!(read("0.1f"), Ok(Value::F32(0.1)));
    assert_eq!(
        read("3.40282346638528859812e+38f"),
        Ok(Value::F32(f32::MAX))
    );
    assert_eq!(code("1e39f"), Code::LiteralRange, "past f32");
    assert_eq!(code("1e400"), Code::LiteralRange, "past binary64");
    assert_eq!(read("65504.0h"), Ok(Value::F16(0x7bff)));
    assert_eq!(code("65536.0h"), Code::LiteralRange, "past f16");
    // Truncated after twenty significant digits.
    assert_eq!(
        read("1.00000000000000000009999"),
        Ok(Value::AbstractFloat(1.0))
    );
    assert_eq!(
        read("123456789012345678901234567890.0"),
        Ok(Value::AbstractFloat(1.234_567_890_123_456_8e29))
    );
}

#[test]
fn hexadecimal_floats_are_exact_where_their_suffix_asks() {
    assert_eq!(read("0xa.fp+2"), Ok(Value::AbstractFloat(43.75)));
    assert_eq!(read("0x1P+4f"), Ok(Value::F32(16.0)));
    assert_eq!(read("0X.3"), Ok(Value::AbstractFloat(0.1875)));
    assert_eq!(read("0x3p+2h"), Ok(Value::F16(0x4a00)), "12.0");
    assert_eq!(read("0X1.fp-4"), Ok(Value::AbstractFloat(0.121_093_75)));
    assert_eq!(read("0x1.fffffep+127f"), Ok(Value::F32(f32::MAX)));
    assert_eq!(
        read("0x1p-149f"),
        Ok(Value::F32(f32::from_bits(1))),
        "least subnormal"
    );
    assert_eq!(read("0x1p-24h"), Ok(Value::F16(1)), "f16's least subnormal");
    assert_eq!(code("0x1p+128f"), Code::LiteralRange);
    assert_eq!(
        code("0x1.000001p0f"),
        Code::LiteralRange,
        "not exact in f32"
    );
    assert_eq!(
        code("0x1p-150f"),
        Code::LiteralRange,
        "below f32's subnormals"
    );
    assert_eq!(
        read("0x1.0000000001p0"),
        Ok(Value::AbstractFloat(1.0 + 1.0 / 1_099_511_627_776.0)),
        "exact in binary64"
    );
    assert_eq!(
        read("0x1p-1074"),
        Ok(Value::AbstractFloat(f64::from_bits(1)))
    );
    assert_eq!(
        read("0x1p-1076"),
        Ok(Value::AbstractFloat(0.0)),
        "below half the least subnormal"
    );
}

#[test]
fn a_decimal_half_rounds_once_from_the_decimal() {
    // 1 + 2^-11 is the tie between 1 and 1 + 2^-10; the double nearest
    // each of these decimals is that tie.
    assert_eq!(
        read("1.00048828125h"),
        Ok(Value::F16(0x3c00)),
        "the tie, to even"
    );
    assert_eq!(
        read("1.0004882812500000001h"),
        Ok(Value::F16(0x3c01)),
        "just past it"
    );
    assert_eq!(
        read("1.0004882812499999999h"),
        Ok(Value::F16(0x3c00)),
        "just short of it"
    );
    assert_eq!(
        read("1.0014648437499999999h"),
        Ok(Value::F16(0x3c01)),
        "short of a tie that rounds to even upward"
    );
    assert_eq!(
        read("65519.99999999999999h"),
        Ok(Value::F16(0x7bff)),
        "short of the overflow tie"
    );
    assert_eq!(
        code("65520.0h"),
        Code::LiteralRange,
        "the overflow tie is infinite"
    );
    assert_eq!(
        read("2.98023223876953125e-8h"),
        Ok(Value::F16(0)),
        "half the least subnormal, to even"
    );
    assert_eq!(
        read("2.9802322387695312501e-8h"),
        Ok(Value::F16(1)),
        "just past it"
    );
}

#[test]
fn an_h_literal_needs_f16_enabled() {
    let (_, shape) = number(b"1h", 0).expect("a literal");
    assert!(matches!(
        value("1h", shape, Span::default(), false),
        Err(Stop::Error(Code::Type, ..))
    ));
}
