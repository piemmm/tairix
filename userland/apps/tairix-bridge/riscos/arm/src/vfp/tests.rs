use tairix_softfloat::{Binary, Mode};

use super::float::{unsigned_recip_estimate, unsigned_rsqrt_estimate, Comparison};
use super::{fpexc, fpscr, System, Unit, Vfp};
use crate::isa::Isa;
use crate::profile::BIT32;

const S: Binary = Binary::SINGLE;
const D: Binary = Binary::DOUBLE;
const H: Binary = Binary::HALF;

fn s(value: f32) -> u64 {
    u64::from(value.to_bits())
}

fn d(value: f64) -> u64 {
    value.to_bits()
}

/// A unit under the FPSCR `controls`.
fn unit(controls: u32) -> Unit {
    Unit::new(controls)
}

#[test]
fn the_registers_alias_as_singles_doubles_and_quads() {
    let mut vfp = Vfp::new();
    vfp.set_s(0, 0x1111_1111);
    vfp.set_s(1, 0x2222_2222);
    assert_eq!(vfp.d(0), 0x2222_2222_1111_1111);
    vfp.set_d(1, 0x4444_4444_3333_3333);
    assert_eq!(vfp.q(0), 0x4444_4444_3333_3333_2222_2222_1111_1111);
    assert_eq!(vfp.s(3), 0x4444_4444);
    vfp.set_q(15, u128::MAX);
    assert_eq!((vfp.d(30), vfp.d(31)), (u64::MAX, u64::MAX));
    vfp.set_s(31, 7);
    assert_eq!(vfp.d(15), 7 << 32);
    // Past the file, a number wraps rather than reaching outside it.
    assert_eq!(vfp.s(32), vfp.s(0));
}

#[test]
fn vmsr_writes_neither_trap_enables_nor_reserved_bits() {
    let mut vfp = Vfp::new();
    vfp.set_fpscr(u32::MAX);
    assert_eq!(vfp.fpscr(), fpscr::WRITABLE);
    assert_eq!(vfp.fpscr() & 0x0008_FF60, 0);
    vfp.set_fpexc(u32::MAX);
    assert_eq!(vfp.fpexc(), fpexc::EN);
    assert!(vfp.enabled());
    vfp.set_fpscr(0);
    vfp.raise(u32::MAX);
    assert_eq!(vfp.fpscr(), fpscr::FLAGS | fpscr::QC);
    vfp.set_nzcv(0x6 << 28 | 0xFFF);
    assert_eq!(vfp.fpscr() >> 28, 0x6);
}

#[test]
fn the_short_vector_controls_name_a_length_and_a_stride() {
    let mut vfp = Vfp::new();
    assert_eq!(vfp.vector(), Some((1, 1)));
    vfp.set_fpscr(7 << fpscr::LEN_SHIFT);
    assert_eq!(vfp.vector(), Some((8, 1)));
    vfp.set_fpscr(3 << fpscr::LEN_SHIFT | 3 << fpscr::STRIDE_SHIFT);
    assert_eq!(vfp.vector(), Some((4, 2)));
    for stride in [1, 2] {
        vfp.set_fpscr(stride << fpscr::STRIDE_SHIFT);
        assert_eq!(vfp.vector(), None);
    }
}

#[test]
fn the_identification_registers_report_the_profile_s_units() {
    for number in 0..16 {
        if let Some(register) = System::of(number) {
            assert_eq!(register.number(), number);
        }
    }
    assert_eq!(System::of(2), None);
    let isa = BIT32.isa;
    assert_eq!(System::Mvfr0.identity(isa), 0x1111_0222);
    assert_eq!(System::Mvfr1.identity(isa), 0x1311_1111);
    assert_eq!(System::Mvfr2.identity(isa), 0x43);
    assert_eq!(System::Mvfr2.identity(Isa::of(&[])), 0);
    assert_eq!(System::Fpsid.name(), "FPSID");
}

#[test]
fn arithmetic_is_ieee_s_rounded_as_the_fpscr_says() {
    let mut nearest = unit(0);
    assert_eq!(nearest.add(S, s(1.0), s(2.0)), s(3.0));
    assert_eq!(nearest.flags, 0);
    assert_eq!(nearest.div(S, s(1.0), s(3.0)), s(1.0 / 3.0));
    assert_eq!(nearest.flags, fpscr::IXC);
    // A third rounds up to nearest in single precision, down in double.
    let mut up = unit(1 << fpscr::RMODE_SHIFT);
    assert_eq!(up.div(D, d(1.0), d(3.0)), d(1.0 / 3.0) + 1);
    let mut zero = unit(3 << fpscr::RMODE_SHIFT);
    assert_eq!(zero.div(S, s(-1.0), s(3.0)), s(-1.0 / 3.0) - 1);
    let mut divide = unit(0);
    assert_eq!(divide.div(S, s(1.0), s(0.0)), s(f32::INFINITY));
    assert_eq!(divide.flags, fpscr::DZC);
}

#[test]
fn flush_to_zero_flushes_denormal_operands_and_tiny_results() {
    let denormal = 1u64;
    let mut flushing = unit(fpscr::FZ);
    assert_eq!(flushing.add(S, denormal, 0), 0);
    assert_eq!(flushing.flags, fpscr::IDC);
    // A negative denormal flushes to a negative zero.
    assert_eq!(flushing.mul(S, 0x8000_0001, s(2.0)), 0x8000_0000);
    let mut tiny = unit(fpscr::FZ);
    assert_eq!(tiny.mul(S, s(2f32.powi(-100)), s(2f32.powi(-30))), 0);
    assert_eq!(tiny.flags, fpscr::UFC);
    let mut gradual = unit(0);
    assert_eq!(
        gradual.mul(S, s(2f32.powi(-100)), s(2f32.powi(-30))),
        1 << 19
    );
    assert_eq!(gradual.flags, 0);
    assert_eq!(
        gradual.mul(S, s(1.25 * 2f32.powi(-100)), s(2f32.powi(-49))),
        1
    );
    assert_eq!(gradual.flags, fpscr::UFC | fpscr::IXC);
}

#[test]
fn a_nan_result_is_the_first_signalling_operand_quieted_or_the_default() {
    let signalling = 0x7F80_0001;
    let quiet = 0x7FC0_0002;
    let mut propagate = unit(0);
    assert_eq!(propagate.add(S, quiet, signalling), 0x7FC0_0001);
    assert_eq!(propagate.flags, fpscr::IOC);
    let mut first = unit(0);
    assert_eq!(first.add(S, quiet, s(1.0)), quiet);
    assert_eq!(first.flags, 0);
    let mut default = unit(fpscr::DN);
    assert_eq!(default.add(S, quiet, s(1.0)), 0x7FC0_0000);
    let mut invalid = unit(0);
    assert_eq!(
        invalid.sub(S, s(f32::INFINITY), s(f32::INFINITY)),
        0x7FC0_0000
    );
    assert_eq!(invalid.flags, fpscr::IOC);
}

#[test]
fn a_fused_multiply_add_rounds_once() {
    let mut fused = unit(0);
    let a = s(1.0 + 2f32.powi(-12));
    let product = fused.mul_add(S, s(-1.0), a, a);
    assert_eq!(product, s(2f32.powi(-11) + 2f32.powi(-24)));
    assert_eq!(fused.flags, 0);
}

#[test]
fn compares_set_nzcv_and_signal_as_their_kind_says() {
    let quiet = 0x7FC0_0000;
    let mut cmp = unit(0);
    assert_eq!(cmp.compare(S, s(1.0), s(2.0), false), 0x8 << 28);
    assert_eq!(cmp.compare(S, s(2.0), s(2.0), false), 0x6 << 28);
    assert_eq!(cmp.compare(S, s(3.0), s(2.0), false), 0x2 << 28);
    assert_eq!(cmp.compare(S, quiet, s(2.0), false), 0x3 << 28);
    assert_eq!(cmp.flags, 0);
    assert_eq!(cmp.compare(S, quiet, s(2.0), true), 0x3 << 28);
    assert_eq!(cmp.flags, fpscr::IOC);
    let mut equal = unit(0);
    assert!(!equal.test(S, quiet, quiet, Comparison::Equal));
    assert_eq!(equal.flags, 0);
    assert!(equal.test(S, s(-0.0), s(0.0), Comparison::Equal));
    assert!(!equal.test(S, quiet, s(0.0), Comparison::GreaterOrEqual));
    assert_eq!(equal.flags, fpscr::IOC);
}

#[test]
fn maximum_and_minimum_order_zeros_and_lose_or_keep_nans() {
    let quiet = 0x7FC0_0000;
    let mut unit = unit(0);
    assert_eq!(unit.max(S, s(0.0), s(-0.0)), s(0.0));
    assert_eq!(unit.min(S, s(0.0), s(-0.0)), s(-0.0));
    assert_eq!(unit.max(S, s(-1.0), s(-0.0)), s(-0.0));
    assert_eq!(unit.max(S, s(1.0), quiet), quiet);
    assert_eq!(unit.max_number(S, s(1.0), quiet), s(1.0));
    assert_eq!(unit.min_number(S, quiet, s(-1.0)), s(-1.0));
    assert_eq!(unit.flags, 0);
    assert_eq!(unit.max_number(S, quiet, 0x7F80_0001), 0x7FC0_0001);
    assert_eq!(unit.flags, fpscr::IOC);
}

#[test]
fn conversions_to_integers_saturate_and_signal() {
    let mut unit = unit(0);
    assert_eq!(unit.to_fixed(S, s(2.5), 0, false, 32, Mode::Zero), 2);
    assert_eq!(unit.flags, fpscr::IXC);
    unit.flags = 0;
    assert_eq!(unit.to_fixed(S, s(2.5), 0, false, 32, Mode::Nearest), 2);
    assert_eq!(unit.to_fixed(S, s(2.5), 0, false, 32, Mode::NearestAway), 3);
    assert_eq!(
        unit.to_fixed(S, s(-2.5), 0, false, 32, Mode::Down),
        (-3i32).cast_unsigned()
    );
    unit.flags = 0;
    assert_eq!(
        unit.to_fixed(D, d(1e10), 0, false, 32, Mode::Zero),
        0x7FFF_FFFF
    );
    assert_eq!(unit.flags, fpscr::IOC);
    assert_eq!(
        unit.to_fixed(D, d(-1e10), 0, false, 32, Mode::Zero),
        0x8000_0000
    );
    assert_eq!(unit.to_fixed(S, s(-1.0), 0, true, 32, Mode::Zero), 0);
    assert_eq!(unit.to_fixed(S, 0x7FC0_0000, 0, false, 32, Mode::Zero), 0);
    unit.flags = 0;
    assert_eq!(unit.to_fixed(S, s(-0.5), 0, true, 32, Mode::Zero), 0);
    assert_eq!(unit.flags, fpscr::IXC);
    assert_eq!(unit.to_fixed(S, s(1.5), 8, false, 16, Mode::Zero), 0x180);
    assert_eq!(
        unit.to_fixed(S, s(-1.5), 8, false, 16, Mode::Zero),
        0xFFFF_FE80
    );
    assert_eq!(unit.to_fixed(S, s(300.0), 8, false, 16, Mode::Zero), 0x7FFF);
    assert_eq!(unit.to_fixed(S, s(300.0), 8, true, 16, Mode::Zero), 0xFFFF);
}

#[test]
fn conversions_from_integers_read_their_width_and_scale() {
    let mut unit = unit(0);
    assert_eq!(
        unit.from_fixed(S, 0xFFFF, 16, 1, false, Mode::Nearest),
        s(-0.5)
    );
    assert_eq!(
        unit.from_fixed(S, 0xFFFF, 16, 0, true, Mode::Nearest),
        s(65535.0)
    );
    assert_eq!(
        unit.from_fixed(S, 0x1234_FFFF, 16, 0, true, Mode::Nearest),
        s(65535.0)
    );
    assert_eq!(
        unit.from_fixed(D, u32::MAX, 32, 0, false, Mode::Nearest),
        d(-1.0)
    );
    assert_eq!(
        unit.from_fixed(S, u32::MAX, 32, 0, true, Mode::Nearest),
        s(4_294_967_296.0)
    );
    assert_eq!(unit.flags, fpscr::IXC);
    assert_eq!(unit.from_fixed(S, 0, 32, 0, false, Mode::Nearest), 0);
}

#[test]
fn half_precision_converts_as_ieee_or_the_alternative_format() {
    let mut ieee = unit(fpscr::FZ);
    assert_eq!(ieee.convert(S, H, s(1.0)), 0x3C00);
    assert_eq!(ieee.convert(H, S, 0x3C00), s(1.0));
    assert_eq!(ieee.convert(H, D, 0xC000), d(-2.0));
    // A half result is never flushed, nor a half operand.
    assert_eq!(ieee.convert(S, H, s(2f32.powi(-20))), 0x0010);
    assert_eq!(ieee.convert(H, S, 0x0001), s(2f32.powi(-24)));
    assert_eq!(ieee.flags, 0);
    assert_eq!(ieee.convert(S, H, s(65520.0)), 0x7C00);
    assert_eq!(ieee.flags, fpscr::OFC | fpscr::IXC);
    let mut alternative = unit(fpscr::AHP);
    assert_eq!(alternative.convert(H, S, 0x7C00), s(65536.0));
    assert_eq!(alternative.convert(H, S, 0xFFFF), s(-131_008.0));
    assert_eq!(alternative.convert(S, H, s(65536.0)), 0x7C00);
    assert_eq!(alternative.convert(S, H, s(131_008.0)), 0x7FFF);
    assert_eq!(alternative.convert(S, H, s(65_521.0)), 0x7BFF + 1);
    assert_eq!(alternative.flags, fpscr::IXC);
    alternative.flags = 0;
    assert_eq!(alternative.convert(S, H, s(131_072.0)), 0x7FFF);
    assert_eq!(alternative.flags, fpscr::IOC);
    assert_eq!(alternative.convert(S, H, s(f32::NEG_INFINITY)), 0xFFFF);
    assert_eq!(alternative.convert(S, H, 0xFFC0_0000), 0x8000);
}

#[test]
fn a_narrowing_conversion_flushes_its_single_result() {
    let mut flushing = unit(fpscr::FZ);
    assert_eq!(flushing.convert(D, S, d(1e-40)), 0);
    assert_eq!(flushing.flags, fpscr::UFC);
    let mut gradual = unit(0);
    assert_ne!(gradual.convert(D, S, d(1e-40)), 0);
}

#[test]
fn rounding_to_an_integral_value_keeps_its_sign_and_flags_only_when_exact() {
    let mut unit = unit(0);
    assert_eq!(unit.round_integral(S, s(2.5), Mode::Nearest, false), s(2.0));
    assert_eq!(
        unit.round_integral(S, s(2.5), Mode::NearestAway, false),
        s(3.0)
    );
    assert_eq!(unit.round_integral(S, s(-0.5), Mode::Zero, false), s(-0.0));
    assert_eq!(unit.flags, 0);
    assert_eq!(unit.round_integral(D, d(2.5), Mode::Up, true), d(3.0));
    assert_eq!(unit.flags, fpscr::IXC);
}

#[test]
fn the_reciprocal_estimates_are_the_architecture_s_tables() {
    let mut unit = Unit::standard(0);
    assert_eq!(unit.recip_estimate(0x3F80_0000), 0x3F7F_8000);
    assert_eq!(unit.recip_estimate(0x4000_0000), 0x3EFF_8000);
    assert_eq!(unit.recip_estimate(0x7F80_0000), 0);
    assert_eq!(unit.flags, 0);
    assert_eq!(unit.recip_estimate(0x8000_0000), 0xFF80_0000);
    assert_eq!(unit.flags, fpscr::DZC);
    let mut huge = Unit::standard(0);
    assert_eq!(huge.recip_estimate(0x7F00_0000), 0);
    assert_eq!(huge.flags, fpscr::UFC);
    let mut root = Unit::standard(0);
    assert_eq!(root.rsqrt_estimate(0x3F80_0000), 0x3F7F_8000);
    assert_eq!(root.rsqrt_estimate(0x4080_0000), 0x3EFF_8000);
    assert_eq!(root.rsqrt_estimate(0x4000_0000), 0x3F34_8000);
    assert_eq!(root.flags, 0);
    assert_eq!(root.rsqrt_estimate(0xBF80_0000), 0x7FC0_0000);
    assert_eq!(root.flags, fpscr::IOC);
    assert_eq!(unsigned_recip_estimate(0x8000_0000), 0xFF80_0000);
    assert_eq!(unsigned_recip_estimate(0x7FFF_FFFF), u32::MAX);
    assert_eq!(unsigned_rsqrt_estimate(0x4000_0000), 0xFF80_0000);
    assert_eq!(unsigned_rsqrt_estimate(0x3FFF_FFFF), u32::MAX);
}

#[test]
fn the_reciprocal_steps_take_infinity_times_zero_as_zero() {
    let mut unit = Unit::standard(0);
    assert_eq!(unit.recip_step(0x4000_0000, 0x3F00_0000), 0x3F80_0000);
    assert_eq!(unit.recip_step(0x7F80_0000, 0), 0x4000_0000);
    assert_eq!(unit.rsqrt_step(0x3F80_0000, 0x3F80_0000), 0x3F80_0000);
    assert_eq!(unit.rsqrt_step(0, 0xFF80_0000), 0x3FC0_0000);
    assert_eq!(unit.rsqrt_step(0x7F80_0000, 0x3F80_0000), 0xFF80_0000);
    assert_eq!(unit.flags, 0);
    assert_eq!(unit.recip_step(0x7F80_0001, 0), 0x7FC0_0000);
    assert_eq!(unit.flags, fpscr::IOC);
}
