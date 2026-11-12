use super::testing::{cycles, power, RATE};
use super::*;
use alloc::vec;
use tairix_util::mathf;

/// One second of a 1 kHz sine at the mix's rate, as the YM3812 would hand
/// it over, rendered to the stream.
fn sine_second(mixer: &mut Mixer) -> Vec<f32> {
    let samples = u64::try_from(mathf::round_i32(RATE)).unwrap();
    let start = mixer.steps.base() + u64::try_from(steps::TAPS).unwrap();
    let mut heard = Vec::new();
    let mut drain = |mixer: &mut Mixer| {
        let mut out = vec![0.0f32; 2 * 2_048];
        let frames = mixer.take(&mut out);
        heard.extend(
            out[..2 * frames]
                .as_chunks::<2>()
                .0
                .iter()
                .map(|frame| frame[0]),
        );
    };
    for n in start..start + samples {
        let phase =
            2.0 * core::f64::consts::PI * 1_000.0 * mathf::real(usize::try_from(n).unwrap()) / RATE;
        mixer.sample(Source::Fm, n, mathf::single(0.5 * mathf::sin(phase)));
        if n % 1_000 == 999 {
            mixer.render((n + 1) * TICKS_PER_SAMPLE);
            drain(mixer);
        }
    }
    mixer.render((start + samples) * TICKS_PER_SAMPLE);
    drain(mixer);
    heard
}

#[test]
fn the_standard_rates_reduce_to_their_exact_ratios() {
    assert_eq!(ratio(48_000, 0), Ratio::new(4_375, 4_224));
    assert_eq!(ratio(44_100, 0), Ratio::new(3_125, 2_772));
    assert_eq!(ratio(48_000, 10), Ratio::new(4_375 * 2_010, 4_224 * 2_000));
    assert!(
        ratio(44_101, -10).is_some(),
        "a rate with no common factor still steps"
    );
    assert_eq!(Mixer::new(0).err(), Some(MixError::Rate));
}

#[test]
fn a_tone_reaches_the_stream_at_its_pitch_on_both_channels() {
    let mut mixer = Mixer::new(48_000).unwrap();
    let out = sine_second(&mut mixer);
    assert!(
        (47_800..=48_000).contains(&out.len()),
        "{} frames",
        out.len()
    );
    let settled = &out[4_800..];
    let seconds = mathf::real(settled.len()) / 48_000.0;
    let rate = mathf::real(cycles(settled)) / seconds;
    assert!((rate - 1_000.0).abs() < 2.0, "{rate} Hz");
    let mut stereo = vec![0.0f32; 4];
    let mut mixer = Mixer::new(48_000).unwrap();
    sine_second(&mut mixer);
    mixer.sample(Source::Fm, 60_000, 0.25);
    mixer.render(61_000 * TICKS_PER_SAMPLE);
    mixer.take(&mut stereo);
    assert_eq!(
        stereo[0].to_bits(),
        stereo[1].to_bits(),
        "the left and right are one"
    );
}

#[test]
fn the_mix_is_ac_coupled() {
    let mut mixer = Mixer::new(48_000).unwrap();
    mixer.step(Source::Dsp, 1_000 * TICKS_PER_SAMPLE, 1.0);
    mixer.render(50_000 * TICKS_PER_SAMPLE);
    let mut out = vec![0.0f32; 2 * 60_000];
    let frames = mixer.take(&mut out);
    let levels: Vec<f32> = out[..2 * frames]
        .as_chunks::<2>()
        .0
        .iter()
        .map(|f| f[0])
        .collect();
    let peak = levels.iter().fold(0.0f32, |m, &x| m.max(x));
    assert!(peak > 0.9, "the step passes: {peak}");
    assert!(levels.last().unwrap().abs() < 1e-3, "and its level decays");
}

#[test]
fn a_full_ring_plays_the_mix_faster_and_an_empty_one_slower() {
    let mut steady = Mixer::new(48_000).unwrap();
    let base = sine_second(&mut steady).len();
    let mut fast = Mixer::new(48_000).unwrap();
    for _ in 0..64 {
        fast.steer(9_600, 4_800);
    }
    let fewer = sine_second(&mut fast).len();
    let mut slow = Mixer::new(48_000).unwrap();
    for _ in 0..64 {
        slow.steer(0, 4_800);
    }
    let more = sine_second(&mut slow).len();
    assert!(
        fewer < base - 200 && fewer > base - 260,
        "0.5 % fewer: {fewer} of {base}"
    );
    assert!(
        more > base + 200 && more < base + 260,
        "0.5 % more: {more} of {base}"
    );
    let tone = sine_second(&mut fast);
    let peak = power(&tone, 48_000.0, 1_005.0);
    assert!(
        peak > power(&tone, 48_000.0, 1_100.0) * 100.0,
        "the tone stays clean"
    );
}

#[test]
fn nothing_grows_without_bound_when_nothing_renders_or_drains() {
    let mut mixer = Mixer::new(48_000).unwrap();
    for n in 0..300_000u64 {
        mixer.sample(Source::Fm, n, 0.1);
    }
    assert!(mixer.steps.pending() <= usize::try_from(MAX_PENDING).unwrap() + 1);
    assert!(mixer.dropped() > 0, "what lands past its bound is counted");
    for second in 1..4u64 {
        mixer.render(second * 49_716 * TICKS_PER_SAMPLE);
    }
    assert!(
        mixer.buffered() <= 48_000,
        "a second at most waits to be drained"
    );
}

#[test]
fn a_source_running_far_ahead_cannot_make_anothers_changes_late() {
    let mut mixer = Mixer::new(48_000).unwrap();
    mixer.set_gain(Source::Speaker, 1.0);
    mixer.sample(Source::Fm, 200_000, 0.5);
    assert_eq!(
        mixer.dropped(),
        1,
        "the far sample is refused, not rendered toward"
    );
    mixer.step(Source::Speaker, 1_000 * TICKS_PER_SAMPLE, 1.0);
    mixer.render(1_200 * TICKS_PER_SAMPLE);
    let mut out = vec![0.0f32; 2 * 2_000];
    let frames = mixer.take(&mut out);
    let peak = out[..2 * frames].iter().fold(0.0f32, |m, &x| m.max(x));
    assert!(peak > 0.9, "the step arrives where it was made: {peak}");
}

#[test]
fn a_jittering_ring_barely_moves_the_ratio() {
    let mut mixer = Mixer::new(48_000).unwrap();
    let mut retunes = 0;
    let mut last = mixer.steering;
    for n in 0..1_000u32 {
        // A fill swinging a fifth either side of its target.
        let fill = if n % 2 == 0 { 3_840 } else { 5_760 };
        mixer.steer(fill, 4_800);
        if mixer.steering != last {
            retunes += 1;
            last = mixer.steering;
        }
    }
    assert!(retunes <= 2, "{retunes} retunes");
}
