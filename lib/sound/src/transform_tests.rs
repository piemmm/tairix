//! Each transform against its definition summed directly in double
//! precision, and the MDCT pair against time-domain aliasing cancellation.

use alloc::vec::Vec;

use tairix_util::mathf::{cos, sin};

use super::{Complex, Dct2, Dct4, Fft, Mdct};

const PI: f64 = core::f64::consts::PI;

/// A reproducible sample in `-1.0..1.0`.
struct Noise(u64);

impl Noise {
    fn sample(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        let unit = f64::from(u32::try_from(self.0 >> 40).expect("24 bits")) / f64::from(1u32 << 24);
        crate::single(unit * 2.0 - 1.0)
    }

    fn samples(&mut self, count: usize) -> Vec<f32> {
        (0..count).map(|_| self.sample()).collect()
    }
}

fn index(at: usize) -> f64 {
    f64::from(u32::try_from(at).expect("small"))
}

/// The largest difference, over the largest magnitude of `want`.
fn relative_error(got: &[f32], want: &[f64]) -> f64 {
    let scale = want
        .iter()
        .fold(1e-30f64, |most, value| most.max(value.abs()));
    got.iter()
        .zip(want)
        .map(|(got, want)| (f64::from(*got) - want).abs())
        .fold(0.0, f64::max)
        / scale
}

#[test]
fn the_fft_matches_its_definition_at_every_length_the_codecs_use() {
    let mut noise = Noise(0x5eed);
    for len in [
        1usize, 2, 3, 4, 5, 6, 8, 9, 15, 16, 30, 60, 120, 240, 480, 1024, 2048,
    ] {
        let fft = Fft::new(len).expect("a plan");
        let input: Vec<Complex> = (0..len)
            .map(|_| Complex::new(noise.sample(), noise.sample()))
            .collect();
        let mut out = alloc::vec![Complex::default(); len];
        fft.process(&input, &mut out);
        let (mut got, mut want) = (Vec::new(), Vec::new());
        for (j, bin) in out.iter().enumerate() {
            let (mut re, mut im) = (0.0f64, 0.0f64);
            for (k, value) in input.iter().enumerate() {
                let angle = -2.0 * PI * index(j) * index(k) / index(len);
                re += f64::from(value.re) * cos(angle) - f64::from(value.im) * sin(angle);
                im += f64::from(value.re) * sin(angle) + f64::from(value.im) * cos(angle);
            }
            got.extend([bin.re, bin.im]);
            want.extend([re, im]);
        }
        assert!(relative_error(&got, &want) < 1e-5, "{len} points");
    }
}

#[test]
fn a_length_with_a_larger_prime_factor_has_no_plan() {
    for len in [0usize, 7, 14, 11 * 4, 49] {
        assert!(Fft::new(len).is_none(), "{len}");
    }
    assert!(Dct4::new(9).is_none(), "odd");
    assert!(Mdct::new(18).is_none(), "not a multiple of four");
    assert!(Dct2::new(7).is_none());
}

#[test]
fn the_dct4_matches_its_definition() {
    let mut noise = Noise(7);
    for len in [6usize, 18, 64, 240] {
        let mut dct = Dct4::new(len).expect("a plan");
        let input = noise.samples(len);
        let mut out = alloc::vec![0.0; len];
        dct.process(&input, &mut out);
        let want: Vec<f64> = (0..len)
            .map(|m| {
                input
                    .iter()
                    .enumerate()
                    .map(|(k, x)| {
                        f64::from(*x) * cos(PI / index(len) * (index(m) + 0.5) * (index(k) + 0.5))
                    })
                    .sum()
            })
            .collect();
        assert!(relative_error(&out, &want) < 1e-5, "{len} points");
    }
}

/// `cos(π/M (n + ½ + M/2)(k + ½))`, the kernel both MDCT directions sum.
fn kernel(m: usize, n: usize, k: usize) -> f64 {
    let m = index(m);
    cos(PI / m * (index(n) + 0.5 + m / 2.0) * (index(k) + 0.5))
}

#[test]
fn the_mdct_pair_matches_its_definition_in_both_directions() {
    let mut noise = Noise(11);
    for m in [6usize, 18, 32, 120, 960] {
        let mut mdct = Mdct::new(2 * m).expect("a plan");
        assert_eq!(mdct.coefficients(), m);
        let coefficients = noise.samples(m);
        let mut samples = alloc::vec![0.0; 2 * m];
        mdct.inverse(&coefficients, &mut samples);
        let want: Vec<f64> = (0..2 * m)
            .map(|n| {
                coefficients
                    .iter()
                    .enumerate()
                    .map(|(k, x)| f64::from(*x) * kernel(m, n, k))
                    .sum()
            })
            .collect();
        assert!(relative_error(&samples, &want) < 1e-5, "inverse, {m}");
        let block = noise.samples(2 * m);
        let mut forward = alloc::vec![0.0; m];
        mdct.forward(&block, &mut forward);
        let want: Vec<f64> = (0..m)
            .map(|k| {
                block
                    .iter()
                    .enumerate()
                    .map(|(n, x)| f64::from(*x) * kernel(m, n, k))
                    .sum()
            })
            .collect();
        assert!(relative_error(&forward, &want) < 1e-5, "forward, {m}");
    }
}

/// Overlapped blocks windowed by the sine window, transformed there and back
/// and overlap-added, give back the signal `M / 2` times over: the aliasing
/// each block leaves the next cancels.
#[test]
fn overlapped_blocks_cancel_each_other_s_aliasing() {
    let m = 64;
    let mut mdct = Mdct::new(2 * m).expect("a plan");
    let window: Vec<f32> = (0..2 * m)
        .map(|n| crate::single(sin(PI / index(2 * m) * (index(n) + 0.5))))
        .collect();
    let signal = Noise(3).samples(6 * m);
    let mut rebuilt = alloc::vec![0.0f32; 6 * m];
    let (mut block, mut coefficients, mut back) = (
        alloc::vec![0.0; 2 * m],
        alloc::vec![0.0; m],
        alloc::vec![0.0; 2 * m],
    );
    for start in (0..5 * m).step_by(m) {
        for (n, slot) in block.iter_mut().enumerate() {
            *slot = signal[start + n] * window[n];
        }
        mdct.forward(&block, &mut coefficients);
        mdct.inverse(&coefficients, &mut back);
        for (n, sample) in back.iter().enumerate() {
            rebuilt[start + n] += sample * window[n];
        }
    }
    let scale = crate::single(index(m) / 2.0);
    let worst = (m..5 * m)
        .map(|n| (rebuilt[n] - scale * signal[n]).abs())
        .fold(0.0f32, f32::max);
    assert!(worst < 1e-3 * scale, "worst {worst}");
}

#[test]
fn the_dct2_matches_its_definition() {
    let mut noise = Noise(19);
    for len in [12usize, 16, 32] {
        let mut dct = Dct2::new(len).expect("a plan");
        let input = noise.samples(len);
        let mut out = alloc::vec![0.0; len];
        dct.process(&input, &mut out);
        let want: Vec<f64> = (0..len)
            .map(|m| {
                input
                    .iter()
                    .enumerate()
                    .map(|(k, x)| {
                        f64::from(*x)
                            * cos(PI * index(m) * (2.0 * index(k) + 1.0) / (2.0 * index(len)))
                    })
                    .sum()
            })
            .collect();
        assert!(relative_error(&out, &want) < 1e-5, "{len} points");
    }
}
