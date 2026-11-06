//! The encoder's analysis against what it is for: the transform undoes the
//! decoder's inverse, onsets are found where sound rises, each band leaves a
//! unit shape beside its energy, and the spreading, the boosts and the tilt
//! follow the signals that call for them.

extern crate std;

use std::vec;
use std::vec::Vec;

use super::{
    audibility, boost_step, mdct, CeltEncoder, Content, BANDS, BAND_COUNT, BLOCKS, BOOST_BYTES,
    ENERGY_MEANS, EVEN_TRIM, LM, MAX_FRAME, OVERLAP, SHORT_BLOCK, SPREAD_BYTES, SPREAD_NORMAL,
    WINDOW_LEN,
};
use crate::opus::celt::{imdct, SPREAD_NONE};
use crate::transform::Dct4;

/// Samples spread over a thousand steps either side of zero.
fn samples(len: usize) -> Vec<f32> {
    let mut state = 0x9E37_79B9_7F4A_7C15u64;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 40) as f32 / 8192.0 - 1024.0
        })
        .collect()
}

/// `frames` frames of the samples at half scale: noise.
fn noise(frames: usize) -> Vec<f32> {
    samples(frames * MAX_FRAME)
        .iter()
        .map(|value| value / 2048.0)
        .collect()
}

/// Frame `frame` of a 5 kHz tone at half scale.
fn tone(frame: usize) -> Vec<f32> {
    (0..MAX_FRAME)
        .map(|index| {
            let at = (frame * MAX_FRAME + index) as f32;
            0.5 * (core::f32::consts::TAU * 5000.0 * at / 48_000.0).sin()
        })
        .collect()
}

/// A mono encoder of 24 kb/s.
fn low_rate() -> CeltEncoder {
    CeltEncoder::new(1, 59).expect("an encoder")
}

/// Code each frame of `pcm` through `encoder`.
fn code_frames(encoder: &mut CeltEncoder, pcm: &[f32]) {
    let mut frame = vec![0u8; encoder.bytes];
    for chunk in pcm.as_chunks::<MAX_FRAME>().0 {
        assert!(encoder.encode(chunk, &mut frame), "the frame fits");
    }
}

/// The energies `encoder` finds in the last frame of `pcm`, as a steady one.
fn steady(encoder: &mut CeltEncoder, pcm: &[f32]) -> [f32; 2 * BAND_COUNT] {
    for chunk in pcm.as_chunks::<MAX_FRAME>().0 {
        assert!(!encoder.analyse(chunk));
    }
    encoder.transform(false);
    encoder.normalise().0
}

#[test]
fn the_transform_is_the_decoder_s_inverse_at_both_block_sizes() {
    let transform = |len| Dct4::new(len).expect("a transform");
    let (mut long, mut short) = (transform(MAX_FRAME), transform(SHORT_BLOCK));
    let input = samples(6 * MAX_FRAME);
    let mut window = vec![0.0f32; WINDOW_LEN];
    let mut out = vec![0.0f32; MAX_FRAME + OVERLAP];
    let mut coefficients = vec![0.0f32; MAX_FRAME];
    let mut scratch = [vec![0.0f32; MAX_FRAME], vec![0.0f32; MAX_FRAME]];
    let mut work = [vec![0.0f32; MAX_FRAME], vec![0.0f32; MAX_FRAME]];
    for (frame, chunk) in input.as_chunks::<MAX_FRAME>().0.iter().enumerate() {
        window.copy_within(MAX_FRAME.., 0);
        window[OVERLAP..].copy_from_slice(chunk);
        out.copy_within(MAX_FRAME.., 0);
        let [folded, transformed] = &mut scratch;
        let [gathered, synthesised] = &mut work;
        if frame % 2 == 1 {
            for block in 0..BLOCKS {
                let at = block * SHORT_BLOCK;
                let samples = &window[at..at + SHORT_BLOCK + OVERLAP];
                mdct(&mut short, samples, folded, transformed);
                for (k, &value) in transformed[..SHORT_BLOCK].iter().enumerate() {
                    coefficients[block + BLOCKS * k] = value;
                }
            }
            for block in 0..BLOCKS {
                let at = &mut out[SHORT_BLOCK * block..];
                imdct(
                    &mut short,
                    &coefficients[block..],
                    BLOCKS,
                    at,
                    gathered,
                    synthesised,
                );
            }
        } else {
            mdct(&mut long, &window, folded, transformed);
            coefficients.copy_from_slice(&transformed[..MAX_FRAME]);
            imdct(&mut long, &coefficients, 1, &mut out, gathered, synthesised);
        }
        for (index, (&got, &want)) in out[..MAX_FRAME].iter().zip(&window).enumerate() {
            assert!(
                (got - want).abs() < 0.05,
                "frame {frame} sample {index}: {got} against {want}"
            );
        }
    }
}

#[test]
fn an_onset_is_found_where_sound_rises_and_not_where_it_holds() {
    let tone: Vec<f32> = (0..MAX_FRAME)
        .map(|index| 0.5 * (index as f32 * 0.13).sin())
        .collect();
    let mut encoder = CeltEncoder::new(1, 159).expect("an encoder");
    assert!(!encoder.analyse(&tone));
    assert!(encoder.onset(), "a tone out of silence");
    assert!(!encoder.analyse(&tone));
    assert!(!encoder.onset(), "the tone held");
    let mut rising = CeltEncoder::new(1, 159).expect("an encoder");
    assert!(rising.analyse(&vec![0.0; MAX_FRAME]), "silence");
    assert!(!rising.onset(), "silence");
    let late: Vec<f32> = (0..MAX_FRAME)
        .map(|index| if index < 480 { 0.0 } else { tone[index] })
        .collect();
    assert!(!rising.analyse(&late));
    assert!(rising.onset(), "a tone breaking in mid-frame");
}

#[test]
fn each_band_is_left_a_unit_shape_beside_its_energy() {
    let pcm = noise(2);
    let mut encoder = CeltEncoder::new(2, 199).expect("an encoder");
    assert!(!encoder.analyse(&pcm));
    encoder.transform(false);
    let before = encoder.coefficients.clone();
    let (energy, amplitudes) = encoder.normalise();
    for channel in 0..2 {
        for band in 0..BAND_COUNT {
            let base = channel * MAX_FRAME;
            let range = base + (BANDS[band] << LM)..base + (BANDS[band + 1] << LM);
            let unit: f32 = encoder.coefficients[range.clone()]
                .iter()
                .map(|value| value * value)
                .sum();
            let amplitude = before[range]
                .iter()
                .map(|value| value * value)
                .sum::<f32>()
                .sqrt();
            let at = channel * BAND_COUNT + band;
            assert!((unit - 1.0).abs() < 1e-3, "{channel} {band}");
            assert!(
                (amplitudes[at] - amplitude).abs() <= 1e-3 * amplitude,
                "{channel} {band}"
            );
            let stated = amplitude.log2() - ENERGY_MEANS[band];
            assert!((energy[at] - stated).abs() < 1e-3, "{channel} {band}");
        }
    }
}

#[test]
fn a_held_tone_is_spread_less_than_noise_and_a_frame_of_noise_moves_it_a_step() {
    let mut tonal = low_rate();
    code_frames(&mut tonal, &tone(0));
    assert_eq!(tonal.spread, SPREAD_NORMAL, "an onset");
    let held: Vec<f32> = (1..3).flat_map(tone).collect();
    code_frames(&mut tonal, &held);
    assert_eq!(tonal.spread, SPREAD_NONE, "a held tone");
    code_frames(&mut tonal, &noise(1));
    assert_eq!(tonal.spread, SPREAD_NONE + 1, "a frame of noise after it");
    let mut noisy = low_rate();
    code_frames(&mut noisy, &noise(4));
    assert!(noisy.spread >= SPREAD_NORMAL, "noise");
    let mut short = CeltEncoder::new(1, SPREAD_BYTES - 1).expect("an encoder");
    let energy = steady(&mut short, &held);
    let spread = short.spreading(Content::Steady, &energy);
    assert_eq!(spread, SPREAD_NORMAL, "a frame too short to tell");
}

#[test]
fn a_tone_is_boosted_where_it_stands_over_its_neighbours_and_noise_is_not() {
    let (below, peak) = (13, 14);
    let mut tonal = low_rate();
    let energy = steady(&mut tonal, &[tone(0), tone(1)].concat());
    let boosts = tonal.boosts(&energy, Content::Steady);
    let most = boosts.iter().copied().max().unwrap_or(0);
    assert!(boosts[peak] == most && boosts[below] > 0, "{boosts:?}");
    // The window's skirt may lift the bands beside the tone; none further off.
    let mut far = boosts
        .iter()
        .enumerate()
        .filter(|&(band, _)| band.abs_diff(peak) > 1);
    assert!(far.all(|(_, &boost)| boost == 0), "{boosts:?}");
    let mut noisy = low_rate();
    let energy = steady(&mut noisy, &noise(2));
    assert_eq!(noisy.boosts(&energy, Content::Steady), [0; BAND_COUNT]);
}

#[test]
fn boosts_need_a_long_enough_frame_and_take_two_thirds_of_it_at_most() {
    let last = BAND_COUNT - 1;
    let mut energy = [0.0f32; 2 * BAND_COUNT];
    energy[last] = 20.0;
    let encoder = low_rate();
    let boosts = encoder.boosts(&energy, Content::Onset);
    let (_, quanta) = boost_step(last, 1, LM);
    let most = (2 * encoder.bytes / 3 * 64) as i32;
    assert_eq!(boosts[last], most / quanta, "{boosts:?}");
    assert!(boosts[..last].iter().all(|&boost| boost == 0), "{boosts:?}");
    let short = CeltEncoder::new(1, BOOST_BYTES - 1).expect("an encoder");
    assert_eq!(short.boosts(&energy, Content::Onset), [0; BAND_COUNT]);
}

#[test]
fn the_spreading_weighs_a_band_by_how_far_the_louder_ones_mask_it() {
    let mut energy = [-10.0f32; 2 * BAND_COUNT];
    energy[14] = 8.0;
    let weights = audibility(&energy, 1);
    assert_eq!(
        (weights[13], weights[14], weights[15]),
        (1, 32, 1),
        "{weights:?}"
    );
    assert!(
        weights[0] > weights[13],
        "far from the loud band: {weights:?}"
    );
}

#[test]
fn the_tilt_leans_toward_the_louder_end_and_for_alike_channels_toward_the_high_bands() {
    let mono = low_rate();
    let sloped = |slope: f32| {
        let mut level = [0.0f32; 2 * BAND_COUNT];
        for (band, value) in level[..BAND_COUNT].iter_mut().enumerate() {
            *value = slope * band as f32;
        }
        level
    };
    assert_eq!(mono.trim(&sloped(0.0)), EVEN_TRIM, "level");
    assert_eq!(mono.trim(&sloped(1.0)), EVEN_TRIM - 2, "rising");
    assert_eq!(mono.trim(&sloped(-1.0)), EVEN_TRIM + 2, "falling");
    let mut stereo = CeltEncoder::new(2, 159).expect("an encoder");
    for shapes in stereo.coefficients.as_chunks_mut::<MAX_FRAME>().0 {
        for band in 0..BAND_COUNT {
            let shape = &mut shapes[BANDS[band] << LM..BANDS[band + 1] << LM];
            let value = (shape.len() as f32).sqrt().recip();
            shape.fill(value);
        }
    }
    let trim = stereo.trim(&[0.0; 2 * BAND_COUNT]);
    assert_eq!(trim, EVEN_TRIM - 4, "alike channels");
}
