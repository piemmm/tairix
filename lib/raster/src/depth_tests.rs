//! The depth stage: every comparison as its operator says, every format's
//! words ordered as their depths and held to the range, each resolution and
//! the bias as stated.

use tairix_fuzzseed::Prng;

use super::{CompareOp, DepthBias, DepthFormat};

const FORMATS: [DepthFormat; 3] = [
    DepthFormat::Unorm16,
    DepthFormat::Unorm24,
    DepthFormat::Float32,
];

#[test]
fn a_comparison_passes_as_its_operator_says() {
    assert!(CompareOp::Less.passes(1, 2));
    assert!(!CompareOp::Less.passes(2, 2));
    assert!(CompareOp::LessOrEqual.passes(2, 2));
    assert!(CompareOp::Greater.passes(3, 2));
    assert!(CompareOp::GreaterOrEqual.passes(2, 2));
    assert!(CompareOp::Equal.passes(2, 2));
    assert!(CompareOp::NotEqual.passes(1, 2));
    assert!(CompareOp::Always.passes(f32::NAN, 0.0));
    assert!(!CompareOp::Never.passes(0, 0));
    assert!(
        !CompareOp::LessOrEqual.passes(f32::NAN, 1.0),
        "a NaN fails an ordered test"
    );
}

#[test]
fn every_format_stores_the_range_and_holds_what_lies_past_it() {
    for format in FORMATS {
        assert_eq!(
            format.load(format.store(0.0)).to_bits(),
            0.0f64.to_bits(),
            "{format:?}"
        );
        assert_eq!(
            format.load(format.store(1.0)).to_bits(),
            1.0f64.to_bits(),
            "{format:?}"
        );
        assert_eq!(format.store(-3.0), format.store(0.0), "{format:?}");
        assert_eq!(format.store(7.5), format.store(1.0), "{format:?}");
        assert_eq!(format.store(f64::NAN), format.store(0.0), "{format:?}");
    }
    assert_eq!(DepthFormat::Unorm16.store(1.0), 0xffff);
    assert_eq!(DepthFormat::Unorm24.store(1.0), 0x00ff_ffff);
    assert_eq!(DepthFormat::Unorm16.store(1.0 / 65535.0 * 300.0), 300);
}

#[test]
fn every_formats_words_order_as_their_depths() {
    let mut rng = Prng::new(0x7121_0030);
    for _ in 0..20_000 {
        let mut depth = || f64::from(rng.next_u32()) / f64::from(u32::MAX);
        let (a, b) = (depth(), depth());
        for format in FORMATS {
            let (sa, sb) = (format.store(a), format.store(b));
            if a < b {
                assert!(sa <= sb, "{format:?}: {a} stored past {b}");
            }
            let nearest = (format.load(sa) - a).abs();
            assert!(
                nearest <= format.resolution(1.0),
                "{format:?}: {a} stored {nearest} away"
            );
        }
    }
}

#[test]
fn a_resolution_is_a_normalised_step_or_a_float_unit_in_the_last_place() {
    assert_eq!(
        DepthFormat::Unorm16.resolution(0.3).to_bits(),
        (1.0 / 65535.0f64).to_bits()
    );
    assert_eq!(
        DepthFormat::Unorm24.resolution(0.9).to_bits(),
        (1.0 / 16_777_215.0f64).to_bits()
    );
    let ulp = |power: i32| f64::from_bits(u64::try_from(1023 + power).expect("normal") << 52);
    assert_eq!(
        DepthFormat::Float32.resolution(1.0).to_bits(),
        ulp(-23).to_bits()
    );
    assert_eq!(
        DepthFormat::Float32.resolution(0.75).to_bits(),
        ulp(-24).to_bits()
    );
    assert_eq!(
        DepthFormat::Float32.resolution(0.0).to_bits(),
        ulp(-149).to_bits()
    );
}

#[test]
fn a_bias_is_its_slope_and_constant_shares_held_to_its_clamp() {
    let bias = DepthBias {
        constant: 4.0,
        slope: 2.0,
        clamp: 0.0,
    };
    let offset = bias.offset(DepthFormat::Unorm16, 0.01, 0.5);
    assert!((offset - (0.02 + 4.0 / 65535.0)).abs() < 1e-15);
    let held = DepthBias {
        clamp: 0.005,
        ..bias
    };
    assert!((held.offset(DepthFormat::Unorm16, 0.01, 0.5) - 0.005).abs() < 1e-15);
    let below = DepthBias {
        constant: -4.0,
        slope: -2.0,
        clamp: -0.005,
    };
    assert!((below.offset(DepthFormat::Unorm16, 0.01, 0.5) + 0.005).abs() < 1e-15);
    assert_eq!(
        DepthBias::NONE
            .offset(DepthFormat::Float32, 0.5, 0.5)
            .to_bits(),
        0.0f64.to_bits()
    );
}
