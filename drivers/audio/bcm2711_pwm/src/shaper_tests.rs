//! The shaper measured: what it leaves in the audible band, at the jack's
//! rate and levels, by a windowed spectrum of its output against the exact
//! duty the input asked for.

#![allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a measurement over two to the sixteenth samples of a few hundred \
              levels, where every index, bin and level is exact in a double"
)]

use std::f64::consts::PI;
use std::vec;
use std::vec::Vec;

use super::{NoiseShaper, MIN_LEVELS};

/// The jack's rate and levels, which the documented figures are for.
const RATE: f64 = 375_000.0;
const LEVELS: u32 = 250;
const SAMPLES: usize = 1 << 16;
/// Samples run through before the measurement, so the loop has settled.
const SETTLE: usize = 4096;

/// The bound the crate's documentation states for the in-band noise.
const DOCUMENTED_NOISE_DBFS: f64 = -90.0;

/// In-place radix-2 FFT.
fn fft(re: &mut [f64], im: &mut [f64]) {
    let n = re.len();
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let mut len = 2;
    while len <= n {
        let step = -2.0 * PI / len as f64;
        for start in (0..n).step_by(len) {
            for k in 0..len / 2 {
                let (wr, wi) = ((step * k as f64).cos(), (step * k as f64).sin());
                let (a, b) = (start + k, start + k + len / 2);
                let tr = re[b] * wr - im[b] * wi;
                let ti = re[b] * wi + im[b] * wr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
            }
        }
        len <<= 1;
    }
}

/// Each bin's power of `signal` through a four-term Blackman-Harris window,
/// whose sidelobes sit far below the shaped noise's own rise.
fn spectrum(signal: &[f64]) -> Vec<f64> {
    let n = signal.len();
    let (a0, a1, a2, a3) = (0.358_75, 0.488_29, 0.141_28, 0.011_68);
    let mut re = vec![0.0; n];
    let mut im = vec![0.0; n];
    let mut window_power = 0.0;
    for (i, &value) in signal.iter().enumerate() {
        let x = 2.0 * PI * i as f64 / n as f64;
        let w = a0 - a1 * x.cos() + a2 * (2.0 * x).cos() - a3 * (3.0 * x).cos();
        re[i] = value * w;
        window_power += w * w;
    }
    fft(&mut re, &mut im);
    (0..=n / 2)
        .map(|k| {
            let power = (re[k] * re[k] + im[k] * im[k]) / (window_power * n as f64);
            if k == 0 || k == n / 2 {
                power
            } else {
                2.0 * power
            }
        })
        .collect()
}

/// The bins from 20 Hz to 20 kHz.
fn audible(bins: &[f64]) -> &[f64] {
    let per_bin = RATE / SAMPLES as f64;
    let first = (20.0 / per_bin).ceil() as usize;
    let last = (20_000.0 / per_bin).floor() as usize;
    &bins[first..=last]
}

/// The shaper's error against the exact duty for `input`, a sequence of
/// samples in full scale.
fn error_of(input: impl Fn(usize) -> f64) -> (Vec<f64>, f64) {
    let mut shaper = NoiseShaper::new(LEVELS, 0x5EED).expect("enough levels");
    let mid = f64::from(LEVELS / 2);
    let swing = f64::from(shaper.swing());
    let mut error = Vec::with_capacity(SAMPLES);
    for n in 0..SETTLE + SAMPLES {
        let x = input(n);
        let sample = (x * f64::from(i32::MAX)) as i32;
        let duty = shaper.duty(sample);
        if n >= SETTLE {
            let exact = mid + f64::from(sample) / 2_147_483_648.0 * swing;
            error.push(f64::from(duty) - exact);
        }
    }
    (error, swing)
}

/// The in-band noise of `error` relative to the largest sine the jack makes.
fn in_band_dbfs(error: &[f64], swing: f64) -> f64 {
    let noise: f64 = audible(&spectrum(error)).iter().sum();
    10.0 * (noise / (swing * swing / 2.0)).log10()
}

fn sine(dbfs: f64) -> impl Fn(usize) -> f64 {
    let amplitude = 10f64.powf(dbfs / 20.0);
    move |n| amplitude * (2.0 * PI * 997.0 * n as f64 / RATE).sin()
}

#[test]
fn the_audible_band_holds_less_noise_than_the_documentation_states() {
    for dbfs in [-6.0, -60.0] {
        let (error, swing) = error_of(sine(dbfs));
        let noise = in_band_dbfs(&error, swing);
        assert!(
            noise < DOCUMENTED_NOISE_DBFS,
            "{noise:.1} dBFS of noise under a {dbfs} dBFS tone"
        );
    }
}

#[test]
fn the_shaping_is_what_buys_the_quiet() {
    // Rounding straight onto the same levels at the same rate, the baseline
    // the shaper is measured against.
    let mid = f64::from(LEVELS / 2);
    let swing = f64::from(NoiseShaper::new(LEVELS, 1).expect("enough levels").swing());
    let tone = sine(-6.0);
    let rounded: Vec<f64> = (0..SAMPLES)
        .map(|n| {
            let exact = mid + tone(n) * swing;
            exact.round() - exact
        })
        .collect();
    let (shaped, _) = error_of(sine(-6.0));
    let gain = in_band_dbfs(&rounded, swing) - in_band_dbfs(&shaped, swing);
    assert!(gain > 25.0, "only {gain:.1} dB quieter than rounding");
}

#[test]
fn a_quiet_tone_leaves_no_harmonic_above_the_noise() {
    // A tone on a bin, so each harmonic lands on one; each is judged against
    // the median of the bins around it, since shaped noise rises steeply
    // across the band.
    let bin = 175;
    let hz = bin as f64 * RATE / SAMPLES as f64;
    let amplitude = 10f64.powf(-80.0 / 20.0);
    let (error, _) = error_of(|n| amplitude * (2.0 * PI * hz * n as f64 / RATE).sin());
    let bins = spectrum(&error);
    let (reach, lobe) = (64, 4);
    let mut worst = 0.0f64;
    for harmonic in 2..=20 {
        let k = bin * harmonic;
        let mut around: Vec<f64> = bins[k - reach..k - lobe]
            .iter()
            .chain(&bins[k + lobe + 1..=k + reach])
            .copied()
            .collect();
        around.sort_by(f64::total_cmp);
        worst = worst.max(bins[k] / around[around.len() / 2]);
    }
    let ratio = 10.0 * worst.log10();
    assert!(
        ratio < 15.0,
        "a harmonic {ratio:.1} dB above the noise around it"
    );
}

#[test]
fn full_scale_input_never_reaches_a_rail() {
    let mut shaper = NoiseShaper::new(LEVELS, 3).expect("enough levels");
    for n in 0..200_000 {
        let sample = match n % 3 {
            0 => i32::MAX,
            1 => i32::MIN,
            _ => {
                if n % 2 == 0 {
                    i32::MAX
                } else {
                    i32::MIN
                }
            }
        };
        let duty = shaper.duty(sample);
        assert!(duty > 0 && duty < LEVELS, "clipped at {duty}");
    }
}

#[test]
fn silence_sits_at_the_middle_level() {
    let mut shaper = NoiseShaper::new(LEVELS, 4).expect("enough levels");
    let count = 100_000u32;
    let total: u64 = (0..count).map(|_| u64::from(shaper.duty(0))).sum();
    let mean = total as f64 / f64::from(count);
    assert!((mean - f64::from(shaper.silence())).abs() < 0.01, "{mean}");
}

#[test]
fn a_seed_fixes_the_output_and_too_few_levels_are_refused() {
    let run = |seed| {
        let mut shaper = NoiseShaper::new(LEVELS, seed).expect("enough levels");
        (0..64)
            .map(|n| shaper.duty(n * 1_000_003))
            .collect::<Vec<_>>()
    };
    assert_eq!(run(9), run(9));
    assert!(NoiseShaper::new(MIN_LEVELS - 1, 0).is_none());
    assert!(NoiseShaper::new(MIN_LEVELS, 0).is_some());
}
