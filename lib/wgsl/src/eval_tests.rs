//! Constant evaluation held to the specification's arithmetic: wrapping
//! concrete integers, abstract overflow and division errors, shifts,
//! finite floats, conversions, and linear algebra.

use super::{binary_scalar, construct_scalar, unary_scalar, Value, Values};
use crate::diag::{Code, Span, Stop};
use crate::syntax::{BinaryOp, UnaryOp};
use crate::types::{Scalar, Ty, Types};

fn refused(outcome: Result<Value, Stop>) -> bool {
    outcome.is_err_and(|stop| matches!(stop, Stop::Error(Code::Evaluation, ..)))
}

fn op(op: BinaryOp, a: Value, b: Value) -> Result<Value, Stop> {
    binary_scalar(op, a, b, Span::default())
}

#[test]
fn concrete_integers_wrap_and_abstract_ones_never_overflow() {
    use BinaryOp::*;
    assert_eq!(
        op(Add, Value::I32(i32::MAX), Value::I32(1)),
        Ok(Value::I32(i32::MIN))
    );
    assert_eq!(
        op(Subtract, Value::U32(0), Value::U32(1)),
        Ok(Value::U32(u32::MAX))
    );
    assert_eq!(
        op(Multiply, Value::I32(-1), Value::I32(i32::MIN)),
        Ok(Value::I32(i32::MIN)),
        "-1 * i32(-2147483648) wraps"
    );
    assert!(refused(op(
        Add,
        Value::AbstractInt(i64::MAX),
        Value::AbstractInt(1)
    )));
    assert!(refused(op(
        Multiply,
        Value::AbstractInt(1 << 32),
        Value::AbstractInt(1 << 32)
    )));
    assert!(refused(unary_scalar(
        UnaryOp::Negate,
        Value::AbstractInt(i64::MIN),
        Span::default()
    )));
    assert_eq!(
        unary_scalar(UnaryOp::Negate, Value::I32(i32::MIN), Span::default()),
        Ok(Value::I32(i32::MIN)),
        "the most negative i32 negates to itself"
    );
}

#[test]
fn division_and_remainder_refuse_zero_and_the_overflowing_quotient() {
    use BinaryOp::*;
    for divide in [Divide, Remainder] {
        assert!(refused(op(divide, Value::I32(7), Value::I32(0))));
        assert!(refused(op(divide, Value::U32(7), Value::U32(0))));
        assert!(refused(op(divide, Value::I32(i32::MIN), Value::I32(-1))));
        assert!(refused(op(
            divide,
            Value::AbstractInt(i64::MIN),
            Value::AbstractInt(-1)
        )));
    }
    assert_eq!(
        op(Divide, Value::I32(-7), Value::I32(2)),
        Ok(Value::I32(-3))
    );
    assert_eq!(
        op(Remainder, Value::I32(-7), Value::I32(2)),
        Ok(Value::I32(-1))
    );
    assert_eq!(
        op(Remainder, Value::I32(7), Value::I32(-2)),
        Ok(Value::I32(1))
    );
    assert_eq!(
        op(
            Divide,
            Value::AbstractInt(0x1_ffff_ffff),
            Value::AbstractInt(8)
        ),
        Ok(Value::AbstractInt(0x3fff_ffff))
    );
}

#[test]
fn shifts_hold_to_their_width_and_keep_every_significant_bit() {
    use BinaryOp::*;
    assert_eq!(
        op(ShiftLeft, Value::U32(1), Value::U32(31)),
        Ok(Value::U32(1 << 31))
    );
    assert!(refused(op(ShiftLeft, Value::U32(1), Value::U32(32))));
    assert!(
        refused(op(ShiftLeft, Value::U32(3), Value::U32(31))),
        "a set bit lost"
    );
    assert_eq!(
        op(ShiftLeft, Value::I32(-1), Value::I32(31)),
        Ok(Value::I32(i32::MIN))
    );
    assert!(
        refused(op(ShiftLeft, Value::I32(1), Value::I32(31))),
        "the sign changes"
    );
    assert_eq!(
        op(ShiftRight, Value::I32(-8), Value::I32(1)),
        Ok(Value::I32(-4))
    );
    assert_eq!(
        op(ShiftRight, Value::U32(u32::MAX), Value::U32(31)),
        Ok(Value::U32(1))
    );
    assert_eq!(
        op(ShiftLeft, Value::AbstractInt(1), Value::AbstractInt(62)),
        Ok(Value::AbstractInt(1 << 62))
    );
    assert!(refused(op(
        ShiftLeft,
        Value::AbstractInt(1),
        Value::AbstractInt(63)
    )));
    assert!(refused(op(
        ShiftLeft,
        Value::AbstractInt(1),
        Value::AbstractInt(64)
    )));
    assert_eq!(
        op(ShiftLeft, Value::AbstractInt(0), Value::AbstractInt(70)),
        Ok(Value::AbstractInt(0))
    );
    assert_eq!(
        op(ShiftRight, Value::AbstractInt(-5), Value::AbstractInt(70)),
        Ok(Value::AbstractInt(-1))
    );
}

#[test]
fn a_float_constant_must_stay_finite() {
    use BinaryOp::*;
    assert!(refused(op(Divide, Value::F32(1.0), Value::F32(0.0))));
    assert!(refused(op(
        Divide,
        Value::AbstractFloat(0.0),
        Value::AbstractFloat(0.0)
    )));
    assert!(refused(op(Multiply, Value::F32(f32::MAX), Value::F32(2.0))));
    assert_eq!(
        op(Add, Value::F32(0.5), Value::F32(0.25)),
        Ok(Value::F32(0.75))
    );
    let half = |x: f32| Value::F16(tairix_util::mathf::to_half(x));
    assert_eq!(op(Add, half(1.0), half(2.0)), Ok(half(3.0)));
    assert!(
        refused(op(Multiply, half(60000.0), half(2.0))),
        "past f16's range"
    );
    assert_eq!(
        op(Less, Value::F32(-0.0), Value::F32(0.0)),
        Ok(Value::Bool(false)),
        "the zeros compare equal"
    );
    assert_eq!(
        op(Equal, Value::F32(-0.0), Value::F32(0.0)),
        Ok(Value::Bool(true))
    );
}

#[test]
fn constructors_convert_as_the_specification_has_them() {
    let s = Span::default();
    assert_eq!(
        construct_scalar(Value::F32(1e20), Scalar::U32, s),
        Ok(Value::U32(4_294_967_040))
    );
    assert_eq!(
        construct_scalar(Value::F32(1e20), Scalar::I32, s),
        Ok(Value::I32(2_147_483_520))
    );
    assert_eq!(
        construct_scalar(Value::F32(-1e20), Scalar::I32, s),
        Ok(Value::I32(i32::MIN))
    );
    assert_eq!(
        construct_scalar(Value::F32(-1.0), Scalar::U32, s),
        Ok(Value::U32(0))
    );
    assert_eq!(
        construct_scalar(Value::F32(-3.9), Scalar::I32, s),
        Ok(Value::I32(-3))
    );
    assert_eq!(
        construct_scalar(Value::I32(-1), Scalar::U32, s),
        Ok(Value::U32(u32::MAX))
    );
    assert_eq!(
        construct_scalar(Value::U32(u32::MAX), Scalar::I32, s),
        Ok(Value::I32(-1))
    );
    assert_eq!(
        construct_scalar(Value::F32(-0.0), Scalar::Bool, s),
        Ok(Value::Bool(false))
    );
    assert_eq!(
        construct_scalar(Value::Bool(true), Scalar::F32, s),
        Ok(Value::F32(1.0))
    );
    assert_eq!(
        construct_scalar(Value::AbstractInt(4_000_000_000), Scalar::U32, s),
        Ok(Value::U32(4_000_000_000))
    );
    assert!(refused(construct_scalar(
        Value::AbstractInt(1 << 40),
        Scalar::I32,
        s
    )));
}

#[test]
fn a_matrix_product_is_linear_algebra() {
    let mut types = Types::default();
    let mut values = Values::default();
    let f32_ty = types.scalar(Scalar::F32).expect("f32");
    let column = types.intern(Ty::Vector(2, Scalar::F32)).expect("vec2");
    let matrix = types
        .intern(Ty::Matrix {
            columns: 2,
            rows: 2,
            scalar: Scalar::F32,
        })
        .expect("mat2x2");
    let mut float = |x: f32| values.add(Value::F32(x)).expect("a value");
    let (one, two, three, four) = (float(1.0), float(2.0), float(3.0), float(4.0));
    let (five, six) = (float(5.0), float(6.0));
    // Columns (1, 2) and (3, 4): the matrix [[1, 3], [2, 4]].
    let first = values.composite(column, &[one, two]).expect("column");
    let second = values.composite(column, &[three, four]).expect("column");
    let m = values.composite(matrix, &[first, second]).expect("matrix");
    let v = values.composite(column, &[five, six]).expect("vector");
    let span = Span::default();
    let product = values
        .binary(
            &mut types,
            BinaryOp::Multiply,
            (m, matrix),
            (v, column),
            column,
            span,
        )
        .expect("m * v");
    let read = |values: &mut Values, types: &mut Types, id| {
        let mut parts = alloc::vec::Vec::new();
        values
            .components(types, (id, column), &mut parts)
            .expect("parts");
        parts
            .iter()
            .map(|&p| values.get(p))
            .collect::<alloc::vec::Vec<_>>()
    };
    assert_eq!(
        read(&mut values, &mut types, product),
        [Value::F32(23.0), Value::F32(34.0)],
        "(1*5 + 3*6, 2*5 + 4*6)"
    );
    let product = values
        .binary(
            &mut types,
            BinaryOp::Multiply,
            (v, column),
            (m, matrix),
            column,
            span,
        )
        .expect("v * m");
    assert_eq!(
        read(&mut values, &mut types, product),
        [Value::F32(17.0), Value::F32(39.0)],
        "(5*1 + 6*2, 5*3 + 6*4)"
    );
    let scaled = values
        .binary(
            &mut types,
            BinaryOp::Multiply,
            (v, column),
            (one, f32_ty),
            column,
            span,
        )
        .expect("v * 1");
    assert_eq!(
        read(&mut values, &mut types, scaled),
        [Value::F32(5.0), Value::F32(6.0)]
    );
}

#[test]
fn a_conversion_to_a_float_rounds_once() {
    let span = Span::default();
    // 1 + 2^-11 + 2^-40: past the half tie by less than a single's unit.
    let past_tie = f64::from_bits(0x3ff0_0200_0000_1000);
    assert_eq!(
        construct_scalar(Value::AbstractFloat(past_tie), Scalar::F16, span),
        Ok(Value::F16(0x3c01))
    );
    // 2^60 + 2^36 + 1: past the single tie by less than a double's unit.
    let past_single_tie = (1i64 << 60) + (1 << 36) + 1;
    assert_eq!(
        construct_scalar(Value::AbstractInt(past_single_tie), Scalar::F32, span),
        Ok(Value::F32(f32::from_bits(0x5d80_0001)))
    );
}
