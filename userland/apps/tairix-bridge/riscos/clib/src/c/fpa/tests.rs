use tairix_riscos_kernel::FRESH_FPSR;

use super::*;
use crate::c::testing::Client;

/// `sqrt` worked as the client's FPA works it.
fn root_as_client(os: &mut Os<'_>, regs: &mut Regs) -> Result<(), Fail> {
    let x = double(regs, 0);
    answer_as_client(os, |unit| unit.double_op(FpaMonadicOp::Sqt, x))
}

fn words(x: f64) -> [u32; 2] {
    fpa_words(x.to_bits())
}

#[test]
fn a_single_widens_and_narrows_exactly_and_its_nans_alike_on_every_host() {
    for single in [0x3F80_0000_u32, 0x8000_0001, 0x7F80_0000, 0xFF7F_FFFF, 0] {
        assert_eq!(
            widened(single).to_bits(),
            f64::from(f32::from_bits(single)).to_bits()
        );
        assert_eq!(single_bits(widened(single)), single, "{single:#010X}");
    }
    // A signalling NaN comes back quiet, its sign and payload kept.
    for (single, double, back) in [
        (0x7FC0_0000_u32, 0x7FF8_0000_0000_0000_u64, 0x7FC0_0000_u32),
        (0xFF80_0001, 0xFFF8_0000_2000_0000, 0xFFC0_0001),
        (0x7FA0_0003, 0x7FFC_0000_6000_0000, 0x7FE0_0003),
    ] {
        assert_eq!(widened(single).to_bits(), double, "{single:#010X}");
        assert_eq!(single_bits(f64::from_bits(double)), back, "{double:#018X}");
    }
}

#[test]
fn the_library_s_own_unit_traps_nothing_and_leaves_the_client_s_status_alone() {
    let mut client = Client::new();
    client.session.kernel.os().set_fpsr(FRESH_FPSR);
    assert!(quietly(|unit| unit.double_op(FpaMonadicOp::Sqt, -1.0)).is_nan());
    assert!(quietly(|unit| unit.div(1.0, 0.0)).is_infinite());
    assert_eq!(client.session.kernel.os().fpsr(), FRESH_FPSR);
}

#[test]
fn the_client_s_unit_leaves_its_flags_and_raises_its_traps() {
    let mut client = Client::new();
    client.session.kernel.os().set_fpsr(FRESH_FPSR);
    client.call(root_as_client, &words(2.0));
    let root = client.session.kernel.os().fpa_double(0).expect("a double");
    assert_eq!(root, core::f64::consts::SQRT_2.to_bits());
    assert_eq!(
        client.session.kernel.os().fpsr(),
        FRESH_FPSR | 0x10,
        "inexact flagged"
    );
    client.session.kernel.os().set_fpsr(FRESH_FPSR);
    assert!(
        client.outcome(root_as_client, &words(-1.0)).is_err(),
        "the invalid operation trapped"
    );
    assert_eq!(
        client.session.kernel.os().fpsr(),
        FRESH_FPSR,
        "a trapped exception flags nothing"
    );
    client.session.kernel.os().set_fpsr(0x0100_0000);
    client.call(root_as_client, &words(-1.0));
    assert_eq!(
        client.session.kernel.os().fpsr(),
        0x0100_0001,
        "held, so flagged"
    );
}

#[test]
fn a_whole_part_is_cut_exactly_toward_nought() {
    for (x, whole) in [
        (0.0, 0.0),
        (-0.5, -0.0),
        (0.999_999_999_999_999_9, 0.0),
        (1.5, 1.0),
        (-2.75, -2.0),
        (4_503_599_627_370_495.5, 4_503_599_627_370_495.0),
        (9_007_199_254_740_993.0, 9_007_199_254_740_992.0),
        (1e300, 1e300),
        (f64::INFINITY, f64::INFINITY),
        (5e-324, 0.0),
    ] {
        assert_eq!(whole_part(x).to_bits(), f64::to_bits(whole), "{x:e}");
    }
    assert!(whole_part(f64::NAN).is_nan());
}

#[test]
fn a_biased_exponent_is_the_field_as_it_stands() {
    for (x, biased) in [
        (1.0, 1023),
        (0.5, 1022),
        (5e-324, 0),
        (0.0, 0),
        (f64::INFINITY, 2047),
        (-2.0, 1024),
    ] {
        assert_eq!(biased_exponent(f64::to_bits(x)), biased, "{x:e}");
    }
}
