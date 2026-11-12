use super::*;
use crate::sound::testing::{power, RATE};
use alloc::vec::Vec;

fn taken(steps: &mut Steps, until: u64) -> Vec<f32> {
    let mut out = Vec::new();
    steps.take(until, &mut |level| out.push(level));
    out
}

#[test]
fn every_impulse_row_sums_to_one() {
    let steps = Steps::new(0).unwrap();
    for row in steps.kernel.as_chunks::<TAPS>().0 {
        let sum: f32 = row.iter().sum();
        assert!((sum - 1.0).abs() < 1e-5, "{sum}");
    }
}

#[test]
fn a_step_settles_at_its_size_about_its_instant() {
    let mut steps = Steps::new(0).unwrap();
    steps.step(200, FRACTION / 2, 1.0).unwrap();
    let out = taken(&mut steps, 400);
    assert_eq!(out.len(), 400 - TAPS / 2);
    assert!(
        out[..200 - TAPS / 2].iter().all(|&x| x.abs() < 1e-6),
        "nothing before its span"
    );
    assert!(
        out[200 + TAPS / 2..]
            .iter()
            .all(|&x| (x - 1.0).abs() < 1e-5),
        "its size after"
    );
    // Centred on its instant, 200.5: the sum through 200 is half, and the
    // ringing either side mirrors.
    assert!((out[200] - 0.5).abs() < 1e-4, "{}", out[200]);
    assert!((out[199] + out[201] - 1.0).abs() < 1e-4);
}

#[test]
fn a_sampled_source_passes_as_it_is() {
    let mut steps = Steps::new(0).unwrap();
    steps.sample(10, 0.25).unwrap();
    steps.sample(10, 0.25).unwrap();
    let out = taken(&mut steps, 100);
    assert!((out[10] - 0.5).abs() < f32::EPSILON);
    assert!(out[11].abs() < f32::EPSILON);
}

#[test]
fn a_change_too_late_lands_where_it_still_can() {
    let mut steps = Steps::new(0).unwrap();
    taken(&mut steps, 1_000);
    let base = steps.base();
    steps.step(10, 0, 1.0).unwrap();
    steps.sample(10, 9.0).unwrap();
    let out = taken(&mut steps, base + 200);
    assert!(
        (out.last().unwrap() - 1.0).abs() < 1e-5,
        "the change is kept"
    );
    assert!(out.iter().all(|&x| x < 2.0), "the late sample is not");
}

#[test]
fn a_square_wave_folds_nothing_back_below_the_streams_band() {
    // 11 kHz's fifth harmonic, 55 kHz, folds to 5.284 kHz at the mix's
    // rate, a fifth of the fundamental's amplitude when drawn naively.
    let mut steps = Steps::new(0).unwrap();
    let half_period = RATE / 22_000.0;
    let mut level = 0.0f32;
    let mut at = 64.0f64;
    let mut out = Vec::new();
    while at < 40_000.0 {
        let whole = tairix_util::mathf::floor(at);
        let fraction = (at - whole) * f64::from(FRACTION);
        let sample = u64::try_from(tairix_util::mathf::round_i32(whole)).unwrap();
        let delta = if level == 0.0 { 1.0 } else { -1.0 };
        steps
            .step(
                sample,
                u32::try_from(tairix_util::mathf::round_i32(fraction))
                    .unwrap()
                    .min(FRACTION - 1),
                delta,
            )
            .unwrap();
        level += delta;
        at += half_period;
        steps.take(sample.saturating_sub(64), &mut |x| out.push(x - 0.5));
    }
    let analysed = &out[4_096..36_864];
    let fundamental = power(analysed, RATE, 11_000.0);
    let folded = power(analysed, RATE, 55_000.0 - RATE);
    let rejection =
        10.0 * tairix_util::mathf::log2(fundamental / folded) / tairix_util::mathf::log2(10.0);
    assert!(rejection > 80.0, "the alias {rejection} dB down");
}
