//! Layer III against the standard's formulas evaluated directly, in `f64`
//! and through `std`'s own maths rather than the decoder's tables.

#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_precision_loss,
    reason = "the references index small tables and narrow values they keep in range"
)]

extern crate std;

use std::f64::consts::{FRAC_1_SQRT_2, PI};
use std::vec::Vec;

use super::*;
use crate::bits::BitWriter;

fn decoder<'d>(
    header: Header,
    layout: &'d Layout<'static>,
    side: &'d SideInfo,
    state: &'d Layer3,
) -> Granules<'d> {
    Granules {
        header,
        layout,
        side,
        lookups: &state.lookups,
        tables: &state.tables,
    }
}

fn header(bytes: [u8; 4]) -> Header {
    Header::parse(&bytes).expect("a header")
}

/// MPEG-1 Layer III, 128 kbit/s at 44.1 kHz, in `mode` with `extension`.
fn mpeg1(mode: u8, extension: u8) -> Header {
    header([0xFF, 0xFB, 0x90, mode << 6 | extension << 4])
}

/// MPEG-2 Layer III, 64 kbit/s at 22.05 kHz, joint stereo with `extension`.
fn mpeg2(extension: u8) -> Header {
    header([0xFF, 0xF3, 0x80, 1 << 6 | extension << 4])
}

/// MPEG-2.5 Layer III at 8 kHz, whose mixed blocks are twice as wide.
fn mpeg25_8khz() -> Header {
    header([0xFF, 0xE3, 0x18, 0xC0])
}

fn side(granule: Granule) -> SideInfo {
    SideInfo {
        main_data_begin: 0,
        scfsi: [[false; 4]; 2],
        granules: [[granule; 2]; 2],
    }
}

fn long_granule(kind: u8, mixed: bool) -> Granule {
    Granule {
        block: Block::Long { kind, mixed },
        ..EMPTY
    }
}

fn short_granule(mixed: bool) -> Granule {
    Granule {
        block: Block::Short { mixed },
        ..EMPTY
    }
}

/// Every line excited, deterministically, within ±1.
fn excitation(seed: u32) -> [f32; LINES] {
    let mut state = seed | 1;
    core::array::from_fn(|_| {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        (f64::from(state % 2001) / 1000.0 - 1.0) as f32
    })
}

/// Subbands a mixed block keeps long: two, or four at 8 kHz.
fn mixed_subbands(layout: &Layout<'_>) -> usize {
    layout.long_lines / SLOTS
}

/// Subbands the filterbank treats as long blocks.
fn long_subbands(block: Block, layout: &Layout<'_>) -> usize {
    match block {
        Block::Long { .. } => SUBBANDS,
        Block::Short { mixed: true } => mixed_subbands(layout),
        Block::Short { mixed: false } => 0,
    }
}

/// `ordered`, the filterbank's window-interleaved order, put back into the
/// order short lines are coded in: band by band, window by window.
fn coded_order(layout: &Layout<'_>, block: Block, ordered: &[f32; LINES]) -> [f32; LINES] {
    let long = long_subbands(block, layout);
    if long == SUBBANDS {
        return *ordered;
    }
    let mut coded = *ordered;
    let mut at = long * SLOTS;
    let first_short = if long > 0 { MIXED_SHORT_START } else { 0 };
    for band in first_short..13 {
        let start = layout.short_start(band);
        for window in 0..3 {
            for freq in 0..usize::from(layout.bands.short[band]) {
                coded[at] = ordered[3 * (start + freq) + window];
                at += 1;
            }
        }
    }
    coded
}

/// The standard's long window for block type `kind`, at `i` of 36.
fn long_window(kind: u8, i: usize) -> f64 {
    let normal = (PI / 36.0 * (i as f64 + 0.5)).sin();
    let narrow = |at: usize| (PI / 12.0 * (at as f64 + 0.5)).sin();
    match (kind, i) {
        (1, 18..24) | (3, 12..18) => 1.0,
        (1, 24..30) => narrow(i - 18),
        (1, 30..) | (3, 0..6) => 0.0,
        (3, 6..12) => narrow(i - 6),
        _ => normal,
    }
}

/// The hybrid filterbank as ISO/IEC 11172-3 §2.4.3.4.10 writes it, over
/// lines already in the filterbank's order: the butterflies across every
/// long boundary, each subband's IMDCT, window and overlap, then the odd
/// subbands' odd slots negated.
fn hybrid_reference(
    block: Block,
    layout: &Layout<'_>,
    lines: &[f64; LINES],
    overlap: &mut [[f64; SLOTS]; SUBBANDS],
) -> Vec<[f64; SUBBANDS]> {
    const CI: [f64; 8] = [
        -0.6, -0.535, -0.33, -0.185, -0.095, -0.041, -0.0142, -0.0037,
    ];
    let long = long_subbands(block, layout);
    let mut x = *lines;
    for sb in 0..long.saturating_sub(1) {
        for (i, &c) in CI.iter().enumerate() {
            let (cs, ca) = (1.0 / (1.0 + c * c).sqrt(), c / (1.0 + c * c).sqrt());
            let (lo, hi) = (sb * SLOTS + 17 - i, (sb + 1) * SLOTS + i);
            let (a, b) = (x[lo], x[hi]);
            x[lo] = a * cs - b * ca;
            x[hi] = b * cs + a * ca;
        }
    }
    let mut out = std::vec![[0f64; SUBBANDS]; SLOTS];
    for sb in 0..SUBBANDS {
        let band = &x[sb * SLOTS..(sb + 1) * SLOTS];
        let mut z = [0f64; 36];
        if sb < long {
            let kind = match block {
                Block::Long { kind, mixed } if !mixed || sb >= mixed_subbands(layout) => kind,
                _ => 0,
            };
            for (i, sample) in z.iter_mut().enumerate() {
                let sum: f64 = (0..18)
                    .map(|k| band[k] * (PI / 72.0 * (2 * i + 19) as f64 * (2 * k + 1) as f64).cos())
                    .sum();
                *sample = sum * long_window(kind, i);
            }
        } else {
            for window in 0..3 {
                for i in 0..12 {
                    let sum: f64 = (0..6)
                        .map(|k| {
                            band[3 * k + window]
                                * (PI / 24.0 * (2 * i + 7) as f64 * (2 * k + 1) as f64).cos()
                        })
                        .sum();
                    z[6 + 6 * window + i] += sum * (PI / 12.0 * (i as f64 + 0.5)).sin();
                }
            }
        }
        for slot in 0..SLOTS {
            let sample = z[slot] + overlap[sb][slot];
            out[slot][sb] = if sb % 2 == 1 && slot % 2 == 1 {
                -sample
            } else {
                sample
            };
            overlap[sb][slot] = z[SLOTS + slot];
        }
    }
    out
}

/// Every block shape, over two granules so the overlap carries, agrees with
/// the direct form to within single-precision rounding.
#[test]
fn the_hybrid_filterbank_matches_the_standards_direct_form() {
    let state = Layer3::new().expect("state");
    let shapes = [
        (mpeg1(3, 0), long_granule(0, false)),
        (mpeg1(3, 0), long_granule(1, false)),
        (mpeg1(3, 0), long_granule(3, false)),
        (mpeg1(3, 0), long_granule(3, true)),
        (mpeg1(3, 0), short_granule(false)),
        (mpeg1(3, 0), short_granule(true)),
        (mpeg25_8khz(), short_granule(true)),
        (mpeg25_8khz(), long_granule(1, true)),
    ];
    for (head, granule) in shapes {
        let layout = layout(head);
        let side = side(granule);
        let granules = decoder(head, &layout, &side, &state);
        let mut imdcts = Imdcts::new().expect("imdcts");
        let mut overlap = [[0f32; SLOTS]; SUBBANDS];
        let mut reference_overlap = [[0f64; SLOTS]; SUBBANDS];
        let (mut worst, mut peak) = (0f64, 0f64);
        for seed in [7, 11] {
            let ordered = excitation(seed);
            let mut lines = coded_order(&layout, granule.block, &ordered);
            let mut out = [[0f32; SUBBANDS]; SLOTS];
            granules.hybrid(
                &granule,
                &mut lines,
                LINES,
                &mut overlap,
                &mut imdcts,
                &mut out,
            );
            let expected = hybrid_reference(
                granule.block,
                &layout,
                &ordered.map(f64::from),
                &mut reference_overlap,
            );
            for (got, want) in out.iter().flatten().zip(expected.iter().flatten()) {
                worst = worst.max((f64::from(*got) - want).abs());
                peak = peak.max(want.abs());
            }
        }
        assert!(
            worst <= peak * 1e-6,
            "{:?} at {} Hz: {worst} of {peak}",
            granule.block,
            head.rate()
        );
    }
}

/// A spectrum silent above some subband is filtered exactly as a full one:
/// the butterflies across the boundary above the last non-zero subband
/// still reach the silent one, and its IMDCT still runs.
#[test]
fn a_sparse_spectrum_is_filtered_as_a_full_one() {
    let state = Layer3::new().expect("state");
    let head = mpeg1(3, 0);
    let layout = layout(head);
    let granule = long_granule(0, false);
    let side = side(granule);
    let granules = decoder(head, &layout, &side, &state);
    let mut lines = excitation(3);
    let nonzero = 5 * SLOTS;
    lines[nonzero..].fill(0.0);
    let (mut sparse, mut full) = (lines, lines);
    let mut out = [[[0f32; SUBBANDS]; SLOTS]; 2];
    let mut overlap = [[[0f32; SLOTS]; SUBBANDS]; 2];
    let [sparse_out, full_out] = &mut out;
    let [sparse_overlap, full_overlap] = &mut overlap;
    let mut imdcts = Imdcts::new().expect("imdcts");
    granules.hybrid(
        &granule,
        &mut sparse,
        nonzero,
        sparse_overlap,
        &mut imdcts,
        sparse_out,
    );
    granules.hybrid(
        &granule,
        &mut full,
        LINES,
        full_overlap,
        &mut imdcts,
        full_out,
    );
    assert_eq!(out[0], out[1]);
    assert_eq!(overlap[0], overlap[1]);
}

/// ISO/IEC 11172-3 §2.4.3.4.7.1: `|is|^(4/3)` scaled by `2^(gain/4)` and by
/// the scalefactor's shift — pre-emphasis on long bands, the subblock gain
/// on short ones.
#[test]
fn requantisation_follows_the_standards_formula() {
    let state = Layer3::new().expect("state");
    let head = mpeg1(3, 0);
    let layout = layout(head);
    let mut factors = NO_SCALEFACTORS;
    for band in 0..22 {
        factors.long[band] = (band % 5) as u8;
    }
    for band in 0..13 {
        factors.short[band] = [(band % 3) as u8, 1, (band % 4) as u8];
    }
    let quantised: [i32; LINES] = core::array::from_fn(|i| (i as i32 % 23) - 11);
    let cases = [
        (long_granule(0, false), false, true),
        (long_granule(0, false), true, false),
        (short_granule(false), true, false),
        (short_granule(true), false, false),
    ];
    for (shape, scalefac_scale, preflag) in cases {
        let granule = Granule {
            global_gain: 170,
            subblock_gain: [0, 2, 5],
            scalefac_scale,
            preflag,
            ..shape
        };
        let side = side(granule);
        let granules = decoder(head, &layout, &side, &state);
        let mut spectrum = [0f32; LINES];
        granules.requantise(&granule, &factors, &quantised, &mut spectrum);
        let multiplier = if scalefac_scale { 1.0 } else { 0.5 };
        let requantised = |line: usize, exponent: f64| {
            let value = f64::from(quantised[line]);
            value.signum() * value.abs().powf(4.0 / 3.0) * exponent.exp2()
        };
        let mut expected = [0f64; LINES];
        let mut line = 0;
        // A mixed block's long part is MPEG-1's first eight long bands.
        let long_bands = match granule.block {
            Block::Long { .. } => 22,
            Block::Short { mixed: true } => 8,
            Block::Short { mixed: false } => 0,
        };
        let long = layout.bands.long.iter().zip(&factors.long).zip(&PRETAB);
        for ((&width, &factor), &pre) in long.take(long_bands) {
            let pre = if preflag { f64::from(pre) } else { 0.0 };
            let exponent = 0.25 * (170.0 - 210.0) - multiplier * (f64::from(factor) + pre);
            for _ in 0..width {
                expected[line] = requantised(line, exponent);
                line += 1;
            }
        }
        if long_bands < 22 {
            let first_short = if long_bands > 0 { MIXED_SHORT_START } else { 0 };
            for band in first_short..13 {
                for window in 0..3 {
                    let gain = 170.0 - 210.0 - 8.0 * f64::from(granule.subblock_gain[window]);
                    let exponent =
                        0.25 * gain - multiplier * f64::from(factors.short[band][window]);
                    for _ in 0..layout.bands.short[band] {
                        expected[line] = requantised(line, exponent);
                        line += 1;
                    }
                }
            }
        }
        assert_eq!(line, LINES);
        for (at, (got, want)) in spectrum.iter().zip(&expected).enumerate() {
            let error = (f64::from(*got) - want).abs();
            assert!(
                error <= want.abs() * 1e-6,
                "line {at}: {got} against {want}"
            );
        }
    }
}

/// Mid/side below the intensity bound; above it MPEG-1's tangent ratio or
/// the lower rates' powers of their step, from the right channel's
/// positions, the top band taking the one below it.
#[test]
fn stereo_processing_follows_its_formulas() {
    let state = Layer3::new().expect("state");
    for head in [mpeg1(1, 3), mpeg2(3)] {
        let layout = layout(head);
        let granule = long_granule(0, false);
        let side = side(granule);
        let granules = decoder(head, &layout, &side, &state);
        let mut right = NO_SCALEFACTORS;
        for band in 0..22 {
            right.long[band] = (band % 6) as u8;
            right.long_limit[band] = 7;
        }
        // The right channel's last non-zero line lies in band 9.
        let bound = layout.long_start(10);
        let mut spectra = [excitation(5), excitation(9)];
        spectra[1][bound..].fill(0.0);
        let before = spectra;
        granules.stereo(0, &right, &mut spectra);
        for line in 0..LINES {
            let (m, s) = (f64::from(before[0][line]), f64::from(before[1][line]));
            let (l, r) = if line < bound {
                ((m + s) * FRAC_1_SQRT_2, (m - s) * FRAC_1_SQRT_2)
            } else {
                let band = (0..22)
                    .rfind(|&band| layout.long_start(band) <= line)
                    .expect("a band");
                let position = i32::from(right.long[band.min(20)]);
                if head.lsf() {
                    let io = 2f64.powf(-0.25);
                    match position {
                        0 => (m, m),
                        odd if odd % 2 == 1 => (m * io.powi((odd + 1) / 2), m),
                        even => (m, m * io.powi(even / 2)),
                    }
                } else {
                    let ratio = (f64::from(position) * PI / 12.0).tan();
                    (m * ratio / (1.0 + ratio), m / (1.0 + ratio))
                }
            };
            let (got_l, got_r) = (f64::from(spectra[0][line]), f64::from(spectra[1][line]));
            assert!(
                (got_l - l).abs() < 1e-6 && (got_r - r).abs() < 1e-6,
                "line {line}: ({got_l}, {got_r}) against ({l}, {r})"
            );
        }
    }
}

/// A position its band cannot take — MPEG-1's 7 and above, the lower
/// rates' largest code — leaves the band to mid/side.
#[test]
fn an_illegal_intensity_position_leaves_its_band_to_mid_side() {
    let state = Layer3::new().expect("state");
    for (head, illegal) in [(mpeg1(1, 3), 9u8), (mpeg2(3), 3)] {
        let layout = layout(head);
        let granule = long_granule(0, false);
        let side = side(granule);
        let granules = decoder(head, &layout, &side, &state);
        let mut right = NO_SCALEFACTORS;
        right.long = [illegal; 22];
        right.long_limit = [3; 22];
        let mut spectra = [excitation(1), [0f32; LINES]];
        let mid = spectra[0];
        granules.stereo(0, &right, &mut spectra);
        let halved = mid.map(|m| m * core::f32::consts::FRAC_1_SQRT_2);
        assert_eq!(spectra, [halved, halved]);
    }
}

/// Short lines are coded band by band and window by window, and the
/// filterbank reads each band's three windows interleaved.
#[test]
fn short_lines_are_interleaved_by_window() {
    let state = Layer3::new().expect("state");
    let head = mpeg1(3, 0);
    let layout = layout(head);
    let side = side(short_granule(false));
    let granules = decoder(head, &layout, &side, &state);
    let coded: [f32; LINES] = core::array::from_fn(|i| i as f32);
    let mut lines = coded;
    granules.reorder(&mut lines, 0);
    // Band 0 is four lines wide at 44.1 kHz.
    assert_eq!(
        lines[..12],
        [0., 4., 8., 1., 5., 9., 2., 6., 10., 3., 7., 11.]
    );
    // A mixed block keeps its two long subbands, then interleaves from band 3.
    let mut lines = coded;
    granules.reorder(&mut lines, 2);
    assert_eq!(lines[..36], coded[..36]);
    assert_eq!(
        lines[36..48],
        [36., 40., 44., 37., 41., 45., 38., 42., 46., 39., 43., 47.]
    );
}

/// Every rate's bands span a granule, and a mixed block's long part meets
/// its short part: two subbands, or four at 8 kHz, whose bands are twice as
/// wide.
#[test]
fn every_rates_bands_span_a_granule() {
    let width = |widths: &[u16]| -> usize { widths.iter().map(|&w| usize::from(w)).sum() };
    for (family, rates) in BANDS.iter().enumerate() {
        for (rate, bands) in rates.iter().enumerate() {
            let at = (family, rate);
            assert_eq!(width(&bands.long), LINES, "{at:?}");
            assert_eq!(width(&bands.short) * 3, LINES, "{at:?}");
            let long_part = width(&bands.long[..usize::from(bands.mixed_long)]);
            assert_eq!(
                long_part,
                3 * width(&bands.short[..MIXED_SHORT_START]),
                "{at:?}"
            );
            assert_eq!(long_part, if at == (2, 2) { 72 } else { 36 }, "{at:?}");
        }
    }
}

/// `scalefac_compress` picks the lower rates' partition and widths; the
/// right channel of intensity stereo reads a partition of its own, whose
/// widths also bound the positions it may state.
#[test]
fn lower_rate_scalefactors_follow_their_partitions() {
    let state = Layer3::new().expect("state");
    let head = mpeg2(1);
    let layout = layout(head);
    let side = side(long_granule(0, false));
    let granules = decoder(head, &layout, &side, &state);
    let read = |info: &Granule, channel: usize, bytes: &[u8]| {
        granules
            .lsf_scalefactors(&mut BitReader::new(bytes), info, channel)
            .expect("scalefactors")
    };

    // Partition 0 at 15: widths 0, 0, 3 and 3 over 6, 5, 5 and 5 bands.
    let mut writer = BitWriter::default();
    let values: Vec<u8> = (1..=10u8).map(|v| v % 8).collect();
    for &value in &values {
        writer.write(u64::from(value), 3);
    }
    writer.write(0, 32);
    let info = Granule {
        scalefac_compress: 15,
        ..long_granule(0, false)
    };
    let factors = read(&info, 0, &writer.into_bytes());
    assert_eq!(factors.long[..11], [0; 11]);
    assert_eq!(factors.long_limit[..11], [u8::MAX; 11]);
    assert_eq!(factors.long[11..21], values[..]);
    assert_eq!(factors.long_limit[11..21], [7; 10]);

    // The intensity channel's partition 3 at 2 × 40: widths 1, 0 and 4 over
    // seven bands each.
    let mut writer = BitWriter::default();
    for _ in 0..7 {
        writer.write(1, 1);
    }
    for value in 0..7 {
        writer.write(value, 4);
    }
    writer.write(0, 32);
    let info = Granule {
        scalefac_compress: 80,
        ..long_granule(0, false)
    };
    let factors = read(&info, 1, &writer.into_bytes());
    assert_eq!(factors.long[..7], [1; 7]);
    assert_eq!(factors.long_limit[..7], [1; 7]);
    assert_eq!(factors.long[7..14], [0; 7]);
    assert_eq!(factors.long_limit[7..14], [u8::MAX; 7]);
    assert_eq!(factors.long[14..21], [0, 1, 2, 3, 4, 5, 6]);
    assert_eq!(factors.long_limit[14..21], [15; 7]);
}
