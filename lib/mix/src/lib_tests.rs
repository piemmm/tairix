use super::{mix32, mix64, numbered, unit, SplitMix64, GOLDEN_GAMMA};

#[test]
fn splitmix64_matches_vignas_reference_outputs_from_seed_zero() {
    let mut generator = SplitMix64::new(0);
    for expected in [
        0xE220_A839_7B1D_CDAF,
        0x6E78_9E6A_A1B9_65F4,
        0x06C4_5D18_8009_454F,
        0xF88B_B8A8_724C_81EC,
        0x1B39_896A_51A8_749B,
    ] {
        assert_eq!(generator.next_u64(), expected);
    }
}

#[test]
fn a_generator_finalises_its_weyl_sequence() {
    let mut generator = SplitMix64::new(0x1234_5678);
    assert_eq!(
        generator.next_u64(),
        mix64(0x1234_5678_u64.wrapping_add(GOLDEN_GAMMA))
    );
}

/// The finaliser's fixed point at nought, and its outputs as a reference
/// implementation of Wellons's lowbias32 written apart from this one gives
/// them.
#[test]
fn lowbias32_matches_a_reference_implementation() {
    assert_eq!(mix32(0), 0);
    for (input, expected) in [
        (1, 0x6889_90C0),
        (2, 0xD113_2181),
        (0x1234_5678, 0xF5E7_1C96),
        (0xDEAD_BEEF, 0xE628_C683),
        (u32::MAX, 0x6768_824A),
    ] {
        assert_eq!(mix32(input), expected, "{input:#x}");
    }
}

#[test]
fn the_finalisers_are_bijections_over_a_run_of_counters() {
    let mut seen32: [u32; 1024] = [0; 1024];
    let mut seen64: [u64; 1024] = [0; 1024];
    for (index, (slot32, slot64)) in seen32.iter_mut().zip(seen64.iter_mut()).enumerate() {
        let counter = u32::try_from(index).unwrap_or(u32::MAX);
        *slot32 = mix32(counter);
        *slot64 = mix64(u64::from(counter));
    }
    seen32.sort_unstable();
    seen64.sort_unstable();
    assert!(seen32.windows(2).all(|pair| pair[0] != pair[1]));
    assert!(seen64.windows(2).all(|pair| pair[0] != pair[1]));
}

#[test]
fn a_flipped_bit_flips_about_half_the_word() {
    let mut flipped = 0_u32;
    let mut trials = 0_u32;
    for counter in 0..256_u64 {
        for bit in 0..64 {
            flipped += (mix64(counter) ^ mix64(counter ^ (1 << bit))).count_ones();
            trials += 1;
        }
    }
    let mean = f64::from(flipped) / f64::from(trials);
    assert!((mean - 32.0).abs() < 0.5, "{mean} bits flipped on average");
    let (mut flipped, mut trials) = (0_u32, 0_u32);
    for counter in 0..1024_u32 {
        for bit in 0..32 {
            flipped += (mix32(counter) ^ mix32(counter ^ (1 << bit))).count_ones();
            trials += 1;
        }
    }
    let mean = f64::from(flipped) / f64::from(trials);
    assert!(
        (mean - 16.0).abs() < 0.25,
        "{mean} of a short word's bits flipped on average"
    );
}

#[test]
fn numbered_streams_differ_by_index_and_by_seed() {
    let keys = [numbered(7, 0), numbered(7, 1), numbered(8, 0)];
    assert!(keys[0] != keys[1] && keys[0] != keys[2] && keys[1] != keys[2]);
    assert_eq!(numbered(7, 1), mix64(7 ^ mix64(2)));
}

#[test]
fn a_unit_fraction_spans_nought_to_short_of_one() {
    assert!(unit(0).abs() < f64::EPSILON);
    assert!(unit(u32::MAX) < 1.0);
    assert!((unit(1 << 31) - 0.5).abs() < f64::EPSILON);
}
