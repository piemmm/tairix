extern crate std;

use alloc::vec;

use super::*;

/// The eleven opcode bits of `D8+group modrm`.
fn op(group: u8, modrm: u8) -> u16 {
    (u16::from(group) << 8) | u16::from(modrm)
}

fn real(x: f64) -> Real {
    Real::from_double(x.to_bits()).0
}

/// A register file with `values` pushed in order, so the last is ST(0).
fn with(values: &[Real]) -> Registers {
    let mut regs = Registers::default();
    for &x in values {
        regs.push(x);
    }
    regs
}

fn st(regs: &Registers, i: u8) -> Real {
    regs.data[regs.physical(i)]
}

/// Runs the register form `group modrm`.
fn run(regs: &mut Registers, group: u8, modrm: u8) -> Done {
    execute(decode(op(group, modrm), false), regs, &[], None)
}

/// Runs the memory form `group /reg` with `operand` read from 1234h.
fn run_memory(regs: &mut Registers, group: u8, reg: u8, operand: &[u8]) -> Done {
    execute(
        decode(op(group, reg << 3), true),
        regs,
        operand,
        Some(0x1234),
    )
}

fn codes(regs: &Registers) -> u16 {
    regs.status & cc::ALL
}

fn unmask(regs: &mut Registers, flags: u16) {
    regs.control &= !flags;
}

#[test]
fn each_group_decodes_to_its_instruction() {
    let memory = |group: u8, reg: u8| decode(op(group, reg << 3), true);
    let register = |group: u8, modrm: u8| decode(op(group, modrm), false);
    assert_eq!(
        memory(0, 6),
        Insn::ArithMemory(Arith::Div(false), Format::Single)
    );
    assert_eq!(
        memory(2, 5),
        Insn::ArithMemory(Arith::Sub(true), Format::Int32)
    );
    assert_eq!(
        memory(4, 3),
        Insn::ArithMemory(Arith::Compare(1), Format::Double)
    );
    assert_eq!(memory(6, 0), Insn::ArithMemory(Arith::Add, Format::Int16));
    assert_eq!(memory(1, 5), Insn::LoadControl);
    assert_eq!(memory(1, 1), Insn::Nop, "reserved");
    assert_eq!(memory(3, 5), Insn::Load(Format::Extended));
    assert_eq!(memory(3, 7), Insn::Store(Format::Extended, true));
    assert_eq!(memory(5, 6), Insn::Save);
    assert_eq!(memory(5, 7), Insn::StoreStatus);
    assert_eq!(memory(7, 4), Insn::Load(Format::Bcd));
    assert_eq!(memory(7, 7), Insn::Store(Format::Int64, true));
    // D8: into ST(0); DC: into ST(i), subtraction and division reversed.
    assert_eq!(
        register(0, 0xE9),
        Insn::ArithRegister {
            op: Arith::Sub(true),
            i: 1,
            into: false,
            pop: false
        }
    );
    assert_eq!(
        register(4, 0xE9),
        Insn::ArithRegister {
            op: Arith::Sub(false),
            i: 1,
            into: true,
            pop: false
        }
    );
    assert_eq!(
        register(6, 0xF9),
        Insn::ArithRegister {
            op: Arith::Div(false),
            i: 1,
            into: true,
            pop: true
        }
    );
    assert_eq!(register(6, 0xD9), Insn::ComparePop2);
    assert_eq!(register(6, 0xD8), Insn::Nop);
    assert_eq!(register(1, 0xEB), Insn::LoadConstant(Constant::Pi));
    assert_eq!(register(1, 0xF5), Insn::Nop, "FPREM1 is the 80387's");
    assert_eq!(register(3, 0xE1), Insn::DisableInterrupts);
    assert_eq!(register(3, 0xE4), Insn::Nop, "FSETPM is the 80287's");
    // The undocumented aliases.
    assert_eq!(register(5, 0xCB), Insn::Exchange(3));
    assert_eq!(register(7, 0xD2), Insn::StoreRegister(2, true));
    assert_eq!(register(7, 0xC1), Insn::Free(1, true));
    assert_eq!(register(5, 0xE0), Insn::Nop, "FUCOM is the 80387's");
    assert_eq!(register(7, 0xE0), Insn::Nop, "FSTSW AX is the 80287's");
}

#[test]
fn a_push_onto_a_full_stack_is_invalid() {
    let mut regs = with(&[Real::ONE; 8]);
    let done = run(&mut regs, 1, 0xE8);
    assert_eq!(done.raised, flag::INVALID);
    assert_eq!(st(&regs, 0), Real::INDEFINITE);
    // Unmasked, nothing moves.
    let mut regs = with(&[Real::ONE; 8]);
    unmask(&mut regs, flag::INVALID);
    let top = regs.top();
    run(&mut regs, 1, 0xE8);
    assert_eq!(regs.top(), top);
}

#[test]
fn an_empty_operand_is_invalid_and_answers_the_indefinite() {
    let mut regs = with(&[Real::ONE]);
    let done = run(&mut regs, 0, 0xC1);
    assert_eq!(done.raised, flag::INVALID);
    assert_eq!(st(&regs, 0), Real::INDEFINITE);
    // FXCH with an empty register: the empty one becomes the indefinite.
    let mut regs = with(&[Real::ONE]);
    run(&mut regs, 1, 0xC9);
    assert_eq!((st(&regs, 0), st(&regs, 1)), (Real::INDEFINITE, Real::ONE));
}

#[test]
fn any_nan_operand_is_invalid_the_larger_nan_answered() {
    let small = Real::new(false, 0x7FFF, INTEGER | 1);
    let large = Real::new(true, 0x7FFF, INTEGER | 0x100);
    let mut regs = with(&[large, small]);
    let done = run(&mut regs, 0, 0xC1);
    assert_eq!(done.raised, flag::INVALID);
    assert_eq!(st(&regs, 0), large);
    // Loading a NaN raises nothing.
    let mut regs = Registers::default();
    let nan = (f64::NAN.to_bits() | 1).to_le_bytes();
    assert_eq!(run_memory(&mut regs, 5, 0, &nan).raised, 0);
}

#[test]
fn infinity_is_projective_until_affine_is_asked_for() {
    let mut regs = with(&[Real::INFINITY, Real::INFINITY]);
    assert_eq!(run(&mut regs, 0, 0xC1).raised, flag::INVALID);
    let mut regs = with(&[Real::INFINITY, Real::INFINITY]);
    regs.control |= 0x1000;
    assert_eq!(run(&mut regs, 0, 0xC1).raised, 0);
    assert_eq!(st(&regs, 0), Real::INFINITY);
    // Compared projectively, infinity equals itself and nothing else.
    let mut regs = with(&[Real::INFINITY.with_sign(true), Real::INFINITY]);
    run(&mut regs, 0, 0xD1);
    assert_eq!(codes(&regs), cc::C3);
    let mut regs = with(&[Real::ONE, Real::INFINITY]);
    assert_eq!(run(&mut regs, 0, 0xD1).raised, flag::INVALID);
    assert_eq!(codes(&regs), cc::COMPARE);
    let mut regs = with(&[Real::ONE, Real::INFINITY]);
    regs.control |= 0x1000;
    run(&mut regs, 0, 0xD1);
    assert_eq!(codes(&regs), 0, "greater");
}

#[test]
fn compares_set_the_codes_and_pop_as_they_say() {
    let mut regs = with(&[real(2.0), real(1.0)]);
    run(&mut regs, 0, 0xD1);
    assert_eq!(codes(&regs), cc::C0, "less");
    let mut regs = with(&[real(2.0), real(2.0)]);
    run(&mut regs, 0, 0xD9);
    assert_eq!(
        (codes(&regs), regs.top()),
        (cc::C3, 7),
        "equal, popped once"
    );
    let mut regs = with(&[real(1.0), real(3.0)]);
    run(&mut regs, 6, 0xD9);
    assert_eq!((codes(&regs), regs.top()), (0, 0), "greater, popped twice");
    // +0 and -0 are equal; a pseudo-zero is a zero.
    let mut regs = with(&[Real::ZERO.with_sign(true)]);
    run(&mut regs, 1, 0xE4);
    assert_eq!(codes(&regs), cc::C3);
    let mut regs = with(&[Real::new(false, 0x4000, 0)]);
    run(&mut regs, 1, 0xE4);
    assert_eq!(codes(&regs), cc::C3);
    // FICOM with a word integer.
    let mut regs = with(&[real(-5.0)]);
    run_memory(&mut regs, 6, 2, &(-6i16).to_le_bytes());
    assert_eq!(codes(&regs), 0);
}

#[test]
fn fxam_tells_each_class_and_honours_the_tag() {
    let check = |x: Real, expected: u16| {
        let mut regs = with(&[x]);
        run(&mut regs, 1, 0xE5);
        assert_eq!(codes(&regs), expected, "{x:?}");
    };
    check(Real::ONE, cc::C2);
    check(real(-1.0), cc::C2 | cc::C1);
    check(Real::new(false, 0x4000, 1), 0);
    check(Real::INFINITY, cc::C2 | cc::C0);
    check(Real::INDEFINITE, cc::C1 | cc::C0);
    check(Real::ZERO, cc::C3);
    check(Real::new(false, 0, 5), cc::C3 | cc::C2);
    let mut regs = Registers::default();
    run(&mut regs, 1, 0xE5);
    assert_eq!(codes(&regs) & !cc::C1, cc::C3 | cc::C0, "empty");
    // A register tagged zero is a zero, whatever it holds.
    let mut regs = with(&[Real::ONE]);
    let physical = regs.physical(0);
    regs.set_tag(physical, tag::ZERO);
    run(&mut regs, 1, 0xE5);
    assert_eq!(codes(&regs), cc::C3);
}

#[test]
fn the_constants_are_truncated_whatever_the_rounding_mode() {
    for rc in 0..4u16 {
        let mut regs = Registers::default();
        regs.control = (regs.control & !0x0C00) | (rc << 10);
        run(&mut regs, 1, 0xEB);
        assert_eq!(
            st(&regs, 0),
            Real::new(false, 0x4000, 0xC90F_DAA2_2168_C234)
        );
        run(&mut regs, 1, 0xEA);
        assert_eq!(st(&regs, 0).sig, 0xB8AA_3B29_5C17_F0BB);
    }
}

#[test]
fn precision_control_rounds_arithmetic_only() {
    let third = |regs: &mut Registers| {
        regs.push(real(1.0));
        regs.push(real(3.0));
        run(regs, 6, 0xF9);
        st(regs, 0)
    };
    let mut regs = Registers::default();
    regs.control &= !0x0300;
    let x = third(&mut regs);
    assert_eq!(x.sig & ((1 << 40) - 1), 0, "24 bits");
    regs.control |= 0x0300;
    let x = third(&mut regs);
    assert_eq!(x.sig, 0xAAAA_AAAA_AAAA_AAAB, "64 bits");
}

#[test]
fn a_difference_of_equals_is_plus_zero_but_minus_rounding_down() {
    let mut regs = with(&[real(1.5), real(1.5)]);
    run(&mut regs, 0, 0xE1);
    assert_eq!(st(&regs, 0), Real::ZERO);
    let mut regs = with(&[real(1.5), real(1.5)]);
    regs.control |= 0x0400;
    run(&mut regs, 0, 0xE1);
    assert_eq!(st(&regs, 0), Real::ZERO.with_sign(true));
}

#[test]
fn division_by_zero_or_by_an_unnormal() {
    let mut regs = with(&[Real::ZERO, real(-2.0)]);
    run(&mut regs, 0, 0xF1);
    assert_eq!(st(&regs, 0), Real::INFINITY.with_sign(true));
    let mut regs = with(&[Real::ZERO, real(2.0)]);
    unmask(&mut regs, flag::ZERO_DIVIDE);
    assert_eq!(run(&mut regs, 0, 0xF1).raised, flag::ZERO_DIVIDE);
    assert_eq!(st(&regs, 0), real(2.0), "unmasked: left as it was");
    let mut regs = with(&[Real::ZERO, Real::ZERO]);
    assert_eq!(run(&mut regs, 0, 0xF1).raised, flag::INVALID);
    let mut regs = with(&[Real::new(false, 0x3FFF, 1 << 62), real(2.0)]);
    assert_eq!(run(&mut regs, 0, 0xF1).raised, flag::INVALID);
    // A denormal divisor is invalid, not denormal.
    let mut regs = with(&[Real::new(false, 0, 1), real(2.0)]);
    assert_eq!(run(&mut regs, 0, 0xF1).raised, flag::INVALID);
}

#[test]
fn a_denormal_operand_is_flagged_and_worked_on_as_its_unnormal() {
    let tiny = Real::new(false, 0, INTEGER >> 1);
    let mut regs = with(&[tiny, Real::ONE]);
    let done = run(&mut regs, 0, 0xC1);
    assert_eq!(done.raised, flag::DENORMAL | flag::PRECISION);
    assert_eq!(st(&regs, 0), Real::ONE);
    // Masked, it costs 33 clocks more; unmasked, the add never happens.
    assert_eq!(done.clocks, 85 + 33);
    let mut regs = with(&[tiny, real(2.0)]);
    unmask(&mut regs, flag::DENORMAL);
    assert_eq!(run(&mut regs, 0, 0xC1).raised, flag::DENORMAL);
    assert_eq!(st(&regs, 0), real(2.0));
    // A single denormal loads as the unnormal it equals.
    let mut regs = Registers::default();
    let done = run_memory(&mut regs, 1, 0, &1u32.to_le_bytes());
    assert_eq!(done.raised, flag::DENORMAL);
    assert_eq!(st(&regs, 0), Real::new(false, 0x3FFF - 126, 1 << 40));
}

#[test]
fn square_roots_refuse_what_the_8087_refuses() {
    let root = |x: Real, affine: bool| {
        let mut regs = with(&[x]);
        if affine {
            regs.control |= 0x1000;
        }
        let done = run(&mut regs, 1, 0xFA);
        (st(&regs, 0), done.raised)
    };
    assert_eq!(root(real(2.25), false), (real(1.5), 0));
    assert_eq!(
        root(Real::ZERO.with_sign(true), false),
        (Real::ZERO.with_sign(true), 0)
    );
    assert_eq!(root(real(-1.0), false), (Real::INDEFINITE, flag::INVALID));
    assert_eq!(
        root(Real::new(false, 0x4000, 1 << 62), false).1,
        flag::INVALID
    );
    assert_eq!(
        root(Real::INFINITY, false),
        (Real::INDEFINITE, flag::INVALID)
    );
    assert_eq!(root(Real::INFINITY, true), (Real::INFINITY, 0));
}

#[test]
fn binary_stores_refuse_an_unnormal_in_range_and_keep_a_nan_a_nan() {
    let mut regs = with(&[Real::new(false, 0x3FFF, 1 << 62)]);
    let done = execute(Insn::Store(Format::Double, true), &mut regs, &[], Some(0));
    assert_eq!(done.raised, flag::INVALID);
    assert_eq!(
        done.store.unwrap().data(),
        &Binary::DOUBLE.indefinite().to_le_bytes()
    );
    let mut regs = with(&[Real::new(false, 0x7FFF, INTEGER | 1)]);
    let done = execute(Insn::Store(Format::Single, false), &mut regs, &[], Some(0));
    let bits = u32::from_le_bytes(done.store.unwrap().data().try_into().unwrap());
    assert_eq!(bits & 0x7F80_0000, 0x7F80_0000);
    assert_ne!(bits & 0x007F_FFFF, 0, "still a NaN");
    // An unmasked overflow stores nothing and pops nothing.
    let mut regs = with(&[Real::new(false, 0x7FF0, INTEGER)]);
    unmask(&mut regs, flag::OVERFLOW);
    let done = execute(Insn::Store(Format::Single, true), &mut regs, &[], Some(0));
    assert_eq!(
        (done.store, done.raised, regs.top()),
        (None, flag::OVERFLOW, 7)
    );
}

#[test]
fn integer_stores_round_check_their_range_and_store_the_indefinite() {
    let store = |x: Real, format: Format| {
        let mut regs = with(&[x]);
        let done = execute(Insn::Store(format, false), &mut regs, &[], Some(0));
        (done.store.map(|s| s.data().to_vec()), done.raised)
    };
    assert_eq!(
        store(real(-2.5), Format::Int16),
        (Some((-2i16).to_le_bytes().to_vec()), flag::PRECISION)
    );
    assert_eq!(store(real(32767.0), Format::Int16).1, 0);
    assert_eq!(
        store(real(32768.0), Format::Int16),
        (Some(vec![0, 0x80]), flag::INVALID)
    );
    assert_eq!(
        store(real(-32768.0), Format::Int16),
        (Some(vec![0, 0x80]), 0)
    );
    assert_eq!(
        store(Real::ZERO.with_sign(true), Format::Int32),
        (Some(vec![0; 4]), 0)
    );
    assert_eq!(
        store(Real::new(false, 0x4000, 1 << 62), Format::Int32).1,
        flag::INVALID
    );
    assert_eq!(store(Real::INFINITY, Format::Int64).1, flag::INVALID);
}

#[test]
fn packed_decimal_stores_and_loads_back_and_refuses_nineteen_digits() {
    let mut regs = with(&[real(-123_456_789.0)]);
    let done = execute(Insn::Store(Format::Bcd, true), &mut regs, &[], Some(0));
    let bytes = done.store.unwrap().data().to_vec();
    assert_eq!(bytes, [0x89, 0x67, 0x45, 0x23, 0x01, 0, 0, 0, 0, 0x80]);
    run_memory(&mut regs, 7, 4, &bytes);
    assert_eq!(st(&regs, 0), real(-123_456_789.0));
    let mut regs = with(&[real(1e18)]);
    let done = execute(Insn::Store(Format::Bcd, true), &mut regs, &[], Some(0));
    assert_eq!(
        (done.raised, done.store.unwrap().data()[8..].to_vec()),
        (flag::INVALID, vec![0xFF, 0xFF])
    );
}

#[test]
fn fprem_gives_the_quotients_low_bits_and_reports_an_incomplete_reduction() {
    let mut regs = with(&[real(2.0), real(13.0)]);
    run(&mut regs, 1, 0xF8);
    // 13 = 6 × 2 + 1: Q = 110b, so C0 = 1, C3 = 1, C1 = 0.
    assert_eq!(st(&regs, 0), real(1.0));
    assert_eq!(codes(&regs), cc::C0 | cc::C3);
    // A quotient below two leaves C3 and C0 as they were.
    let mut regs = with(&[real(2.0), real(3.0)]);
    regs.status |= cc::C3 | cc::C0;
    run(&mut regs, 1, 0xF8);
    assert_eq!(codes(&regs), cc::C3 | cc::C1 | cc::C0);
    let mut regs = with(&[real(3.0), real(2.0f64.powi(100))]);
    run(&mut regs, 1, 0xF8);
    assert_eq!(codes(&regs) & cc::C2, cc::C2, "incomplete");
    // An unnormal modulus is invalid.
    let mut regs = with(&[Real::new(false, 0x4000, 1 << 62), real(5.0)]);
    assert_eq!(run(&mut regs, 1, 0xF8).raised, flag::INVALID);
}

#[test]
fn frndint_rounds_in_the_mode_and_takes_a_pseudo_zero_for_zero() {
    for (rc, x, expected) in [
        (0u16, 2.5, 2.0),
        (1, -1.5, -2.0),
        (2, 1.25, 2.0),
        (3, -1.75, -1.0),
    ] {
        let mut regs = with(&[real(x)]);
        regs.control = (regs.control & !0x0C00) | (rc << 10);
        assert_eq!(run(&mut regs, 1, 0xFC).raised, flag::PRECISION);
        assert_eq!(st(&regs, 0), real(expected), "{x} in mode {rc}");
    }
    let mut regs = with(&[Real::new(true, 0x5000, 0)]);
    run(&mut regs, 1, 0xFC);
    assert_eq!(st(&regs, 0), Real::ZERO.with_sign(true));
}

#[test]
fn fscale_scales_by_the_chopped_power_and_raises_no_precision() {
    let mut regs = with(&[real(-3.7), real(1.5)]);
    run(&mut regs, 1, 0xFD);
    assert_eq!(st(&regs, 0), real(1.5 / 8.0));
    let mut regs = with(&[Real::INFINITY, real(1.0)]);
    assert_eq!(run(&mut regs, 1, 0xFD).raised, flag::INVALID);
    let mut regs = with(&[real(2.0), Real::INFINITY]);
    assert_eq!(run(&mut regs, 1, 0xFD).raised, 0);
    assert_eq!(st(&regs, 0), Real::INFINITY);
    // Down into the denormals: underflow, but no precision exception.
    let mut regs = with(&[real(-16_400.0), real(1.0 + 2.0f64.powi(-52))]);
    let done = run(&mut regs, 1, 0xFD);
    assert_eq!(done.raised & flag::PRECISION, 0);
    assert_eq!(done.raised & flag::UNDERFLOW, flag::UNDERFLOW);
}

#[test]
fn fxtract_splits_a_value_and_gives_two_zeros_for_zero() {
    let mut regs = with(&[real(-12.0)]);
    run(&mut regs, 1, 0xF4);
    assert_eq!((st(&regs, 0), st(&regs, 1)), (real(-1.5), real(3.0)));
    let mut regs = with(&[Real::ZERO.with_sign(true)]);
    run(&mut regs, 1, 0xF4);
    assert_eq!(
        (st(&regs, 0), st(&regs, 1)),
        (Real::ZERO.with_sign(true), Real::ZERO.with_sign(true))
    );
    let mut regs = with(&[Real::INFINITY]);
    assert_eq!(run(&mut regs, 1, 0xF4).raised, flag::INVALID);
}

#[test]
fn the_transcendentals_answer_exactly_where_the_value_is_exact() {
    // y × log2(8) = 3y with no rounding.
    let mut regs = with(&[real(-1.25), real(8.0)]);
    assert_eq!(run(&mut regs, 1, 0xF1).raised, 0);
    assert_eq!((st(&regs, 0), regs.top()), (real(-3.75), 7));
    let mut regs = with(&[real(1.0)]);
    assert_eq!(run(&mut regs, 1, 0xF0).raised, 0, "2^1 - 1");
    assert_eq!(st(&regs, 0), real(1.0));
    let mut regs = with(&[Real::ZERO]);
    run(&mut regs, 1, 0xF2);
    assert_eq!((st(&regs, 0), st(&regs, 1)), (Real::ONE, Real::ZERO));
    // FPTAN past 2^63: C2 set, the operand left alone.
    let mut regs = with(&[real(2.0f64.powi(70))]);
    run(&mut regs, 1, 0xF2);
    assert_eq!((codes(&regs) & cc::C2, regs.top()), (cc::C2, 7));
    // FYL2X of a negative is invalid; of zero a zero divide.
    let mut regs = with(&[real(1.0), real(-2.0)]);
    assert_eq!(run(&mut regs, 1, 0xF1).raised, flag::INVALID);
    let mut regs = with(&[real(1.0), Real::ZERO]);
    assert_eq!(run(&mut regs, 1, 0xF1).raised, flag::ZERO_DIVIDE);
    assert_eq!(st(&regs, 0), Real::INFINITY.with_sign(true));
}

#[test]
fn fmul_by_a_short_operand_takes_its_faster_row() {
    let mut regs = with(&[real(1.5), real(3.0)]);
    assert_eq!(run(&mut regs, 0, 0xC9).clocks, 97);
    let mut regs = with(&[real(1.0 / 3.0), real(1.0 / 7.0)]);
    assert_eq!(run(&mut regs, 0, 0xC9).clocks, 138);
    assert_eq!(
        run_memory(&mut regs, 4, 1, &1.5f64.to_le_bytes()).clocks,
        120
    );
    assert_eq!(
        run_memory(&mut regs, 4, 1, &(1.0f64 / 3.0).to_le_bytes()).clocks,
        161
    );
}

#[test]
fn the_stack_pointer_moves_and_ffree_empties() {
    let mut regs = with(&[Real::ONE]);
    run(&mut regs, 1, 0xF6);
    assert_eq!(regs.top(), 6);
    run(&mut regs, 1, 0xF7);
    run(&mut regs, 5, 0xC0);
    assert!(regs.empty(0));
    let mut regs = with(&[real(2.0), real(-3.0)]);
    run(&mut regs, 1, 0xE1);
    assert_eq!(st(&regs, 0), real(3.0));
    run(&mut regs, 1, 0xE0);
    assert_eq!(st(&regs, 0), real(-3.0));
}
