//! Unit tests for the per-pixel kernels' candidates.

use tairix_cpuops::{CpuFeature, CpuFeatureSet, DecisionReason};

use super::{
    blend_cases, blend_reference, blend_run, blur_cases, blur_reference, blur_run, digest_with,
    scan_cases, scan_reference, scan_run, select_blend, select_blur, select_scan, within_lanes,
    BlendOps, BlurOps, ScanOps, BLEND_CANDIDATES, BLEND_PORTABLE, BLUR_CANDIDATES, BLUR_PORTABLE,
    CONFORMANCE_DIGEST, PORTABLE_BLEND, PORTABLE_BLUR, PORTABLE_SCAN, SCAN_CANDIDATES,
    SCAN_PORTABLE,
};
use crate::blur::{slide_portable, PixelSum, Reciprocal};
use crate::color::Pixel;
use crate::dither::DitherRow;

/// The features this build's own target carries, which is what admits its
/// candidates: presenting a feature the host lacks would fault, not test.
fn target_features() -> CpuFeatureSet {
    let mut features = CpuFeatureSet::new();
    let required = BLEND_CANDIDATES
        .iter()
        .flat_map(|candidate| candidate.requires)
        .chain(
            BLUR_CANDIDATES
                .iter()
                .flat_map(|candidate| candidate.requires),
        )
        .chain(
            SCAN_CANDIDATES
                .iter()
                .flat_map(|candidate| candidate.requires),
        );
    for &feature in required {
        features.insert(feature);
    }
    features
}

fn prng(name: &str) -> tairix_fuzzseed::Prng {
    tairix_fuzzseed::Prng::new(tairix_fuzzseed::start(name, tairix_fuzzseed::FUZZ_SEED_ENV))
}

fn random_pixel(prng: &mut tairix_fuzzseed::Prng) -> Pixel {
    let [r, g, b] = [prng.next_u8(), prng.next_u8(), prng.next_u8()];
    Pixel {
        r,
        g,
        b,
        a: prng.edge_biased_u8(),
    }
}

fn random_row(prng: &mut tairix_fuzzseed::Prng, len: usize) -> alloc::vec::Vec<Pixel> {
    (0..len).map(|_| random_pixel(prng)).collect()
}

#[test]
fn every_candidate_reproduces_the_portable_blend_on_every_verify_case() {
    for candidate in BLEND_CANDIDATES {
        for (index, case) in blend_cases().iter().enumerate() {
            assert_eq!(
                blend_run(candidate.impl_, case),
                blend_reference(case),
                "{} disagrees on case {index}",
                candidate.name
            );
        }
    }
}

#[test]
fn with_no_features_the_portable_blend_is_chosen() {
    let selected = select_blend(CpuFeatureSet::EMPTY);
    assert_eq!(selected.decision.chosen, BLEND_PORTABLE);
    assert_eq!(selected.decision.reason, DecisionReason::Baseline);
}

#[test]
fn with_the_target_s_features_its_candidate_is_chosen() {
    let selected = select_blend(target_features());
    match BLEND_CANDIDATES.first() {
        Some(candidate) => {
            assert_eq!(selected.decision.chosen, candidate.name);
            assert_eq!(selected.decision.reason, DecisionReason::Priority);
        }
        None => assert_eq!(selected.decision.chosen, BLEND_PORTABLE),
    }
}

#[test]
fn a_feature_the_candidate_does_not_need_never_admits_it() {
    let unrelated = CpuFeatureSet::new()
        .with(CpuFeature::Crc32)
        .with(CpuFeature::Zbb);
    assert_eq!(select_blend(unrelated).decision.chosen, BLEND_PORTABLE);
}

/// Random spans of every length up to well past a step and at every dither
/// phase, from arbitrary bytes — transparent, opaque, and out-of-range pixels
/// alike — blended both ways.
fn assert_matches_portable(ops: BlendOps, name: &str) {
    let mut prng = prng("raster_blend_candidates");
    let rounds = if cfg!(miri) { 40 } else { 4000 };
    for _ in 0..rounds {
        let len = prng.below(70);
        let phase = prng.next_u32();
        let row = DitherRow::at(prng.next_u32());
        let factor = match prng.below(4) {
            0 => 255,
            1 => 0,
            _ => prng.next_u8(),
        };
        let extra = prng.below(3);
        let src = random_row(&mut prng, len + extra);
        let dst = random_row(&mut prng, len);
        let tile = row.tile_at(phase);
        let (mut by_candidate, mut by_portable) = (dst.clone(), dst.clone());
        (ops.span)(&mut by_candidate, &src, factor, &tile);
        (PORTABLE_BLEND.span)(&mut by_portable, &src, factor, &tile);
        assert_eq!(
            by_candidate, by_portable,
            "{name} span, len {len}, factor {factor}"
        );
        let colour = random_pixel(&mut prng);
        let (mut by_candidate, mut by_portable) = (dst.clone(), dst.clone());
        (ops.solid)(&mut by_candidate, colour, factor, &tile);
        (PORTABLE_BLEND.solid)(&mut by_portable, colour, factor, &tile);
        assert_eq!(
            by_candidate, by_portable,
            "{name} solid, len {len}, factor {factor}"
        );
        let longer = len + prng.below(3);
        let to = random_row(&mut prng, longer);
        let (mut by_candidate, mut by_portable) = (dst.clone(), dst.clone());
        (ops.mix)(&mut by_candidate, &src, &to, factor, &tile);
        (PORTABLE_BLEND.mix)(&mut by_portable, &src, &to, factor, &tile);
        assert_eq!(
            by_candidate, by_portable,
            "{name} mix, len {len}, weight {factor}"
        );
        let coverage: alloc::vec::Vec<u8> = (0..len + prng.below(3))
            .map(|_| prng.edge_biased_u8())
            .collect();
        let (mut by_candidate, mut by_portable) = (dst.clone(), dst.clone());
        (ops.cover)(&mut by_candidate, colour, &coverage, &tile);
        (PORTABLE_BLEND.cover)(&mut by_portable, colour, &coverage, &tile);
        assert_eq!(by_candidate, by_portable, "{name} cover, len {len}");
        let ink = crate::Color {
            r: prng.next_u8(),
            g: prng.next_u8(),
            b: prng.next_u8(),
            a: prng.next_u8(),
        };
        let (mut by_candidate, mut by_portable) = (dst.clone(), dst.clone());
        (ops.wash)(&mut by_candidate, ink, &coverage, &tile);
        (PORTABLE_BLEND.wash)(&mut by_portable, ink, &coverage, &tile);
        assert_eq!(by_candidate, by_portable, "{name} wash, len {len}");
        // As few samples as one, so a span reaching past the last is widened
        // from it as well as one with samples to spare.
        let samples: alloc::vec::Vec<u64> = (0..=prng.below(len / 2 + 3))
            .map(|_| {
                let mut lane =
                    || u64::from(prng.next_u8().min(254)) << 8 | u64::from(prng.next_u8());
                lane() | lane() << 16 | lane() << 32
            })
            .collect();
        let biases = tile.map(|bias| u64::from(bias) * crate::wide::LANE_ONES);
        let start = prng.next_u32() % 16;
        let (mut by_candidate, mut by_portable) = (dst.clone(), dst);
        (ops.widen)(&mut by_candidate, (start, &samples), &biases);
        (PORTABLE_BLEND.widen)(&mut by_portable, (start, &samples), &biases);
        assert_eq!(
            by_candidate,
            by_portable,
            "{name} widen, len {len}, start {start}, {} samples",
            samples.len()
        );
    }
}

#[test]
fn every_candidate_matches_the_portable_blend_on_random_spans() {
    for candidate in BLEND_CANDIDATES {
        assert_matches_portable(candidate.impl_, candidate.name);
    }
}

#[test]
fn the_shift_form_of_div255_is_exact_for_every_sum_a_blend_can_form() {
    // A product of two bytes plus a bias below one level.
    let most = 255 * 255 + 253;
    let step = if cfg!(miri) { 97 } else { 1 };
    for x in (0u32..=most).step_by(step) {
        assert_eq!((x + 1 + (x >> 8)) >> 8, x / 255, "at {x}");
    }
}

#[test]
fn every_blur_candidate_reproduces_the_portable_blur_on_every_verify_case() {
    for candidate in BLUR_CANDIDATES {
        for (index, case) in blur_cases().iter().enumerate() {
            assert!(
                blur_run(candidate.impl_, case) == blur_reference(case),
                "{} disagrees on case {index}",
                candidate.name
            );
        }
    }
}

#[test]
fn the_blur_candidate_is_chosen_only_with_its_feature() {
    assert_eq!(
        select_blur(CpuFeatureSet::EMPTY).decision.chosen,
        BLUR_PORTABLE
    );
    let chosen = select_blur(target_features()).decision.chosen;
    match BLUR_CANDIDATES.first() {
        Some(candidate) => assert_eq!(chosen, candidate.name),
        None => assert_eq!(chosen, BLUR_PORTABLE),
    }
}

/// A line blurred both ways over a random part of it, at radii from one to
/// past the lane divisor's reach.
fn assert_lines_match(ops: BlurOps, name: &str) {
    let mut prng = prng("raster_blur_line_candidates");
    let rounds = if cfg!(miri) { 24 } else { 3000 };
    for _ in 0..rounds {
        let len = prng.below(90);
        let radius = match prng.below(8) {
            0 => 40_000,
            1 => 1 + prng.below(300),
            _ => 1 + prng.below(48),
        };
        let start = prng.below(len + 1);
        let end = start + prng.below(len + 1 - start);
        let src = random_row(&mut prng, len);
        let recip = Reciprocal::new(radius * 2 + 1);
        let (mut by_candidate, mut by_portable) =
            (random_row(&mut prng, end - start), alloc::vec::Vec::new());
        by_portable.clone_from(&by_candidate);
        (ops.line)(&src, &mut by_candidate, len, radius, recip, start..end);
        (PORTABLE_BLUR.line)(&src, &mut by_portable, len, radius, recip, start..end);
        assert_eq!(
            by_candidate, by_portable,
            "{name}: len {len} radius {radius} {start}..{end}"
        );
    }
}

/// A vertical pass's column sums driven both ways: a window's rows added,
/// slid a row at a time — always removing a row that was added, as a window
/// does — and averaged.
fn assert_columns_match(ops: BlurOps, name: &str) {
    let mut prng = prng("raster_blur_column_candidates");
    let rounds = if cfg!(miri) { 12 } else { 400 };
    for _ in 0..rounds {
        let width = prng.below(70);
        let radius = 1 + prng.below(40);
        let count = radius * 2 + 1;
        let recip = Reciprocal::new(count);
        let mut window: alloc::collections::VecDeque<alloc::vec::Vec<Pixel>> =
            (0..count).map(|_| random_row(&mut prng, width)).collect();
        let (mut by_candidate, mut by_portable) = (
            alloc::vec![PixelSum::default(); width],
            alloc::vec![PixelSum::default(); width],
        );
        for row in &window {
            (ops.add)(&mut by_candidate, row);
            (PORTABLE_BLUR.add)(&mut by_portable, row);
        }
        for _ in 0..prng.below(6) {
            let entering = random_row(&mut prng, width);
            let Some(leaving) = window.pop_front() else {
                break;
            };
            (ops.slide)(&mut by_candidate, &entering, &leaving);
            (PORTABLE_BLUR.slide)(&mut by_portable, &entering, &leaving);
            window.push_back(entering);
        }
        assert_eq!(by_candidate, by_portable, "{name}: sums, width {width}");
        let (mut means, mut expected) = (random_row(&mut prng, width), alloc::vec::Vec::new());
        expected.clone_from(&means);
        (ops.mean)(&mut means, &by_candidate, recip);
        (PORTABLE_BLUR.mean)(&mut expected, &by_portable, recip);
        assert_eq!(
            means, expected,
            "{name}: means, width {width} radius {radius}"
        );
    }
}

#[test]
fn every_blur_candidate_matches_the_portable_blur_on_random_lines_and_columns() {
    for candidate in BLUR_CANDIDATES {
        assert_lines_match(candidate.impl_, candidate.name);
        assert_columns_match(candidate.impl_, candidate.name);
    }
}

#[test]
fn every_scan_candidate_reproduces_the_portable_scan_on_every_verify_case() {
    for candidate in SCAN_CANDIDATES {
        for (index, case) in scan_cases().iter().enumerate() {
            assert_eq!(
                scan_run(candidate.impl_, case),
                scan_reference(case),
                "{} disagrees on case {index}",
                candidate.name
            );
        }
    }
}

#[test]
fn the_scan_candidate_is_chosen_only_with_its_feature() {
    assert_eq!(
        select_scan(CpuFeatureSet::EMPTY).decision.chosen,
        SCAN_PORTABLE
    );
    let chosen = select_scan(target_features()).decision.chosen;
    match SCAN_CANDIDATES.first() {
        Some(candidate) => assert_eq!(chosen, candidate.name),
        None => assert_eq!(chosen, SCAN_PORTABLE),
    }
}

/// Spans of runs broken anywhere — mostly long runs, which is what a window's
/// rows are — scanned for both kinds of run, every alpha short of full drawn
/// where one is.
fn assert_scans_match(ops: ScanOps, name: &str) {
    let mut prng = prng("raster_scan_candidates");
    let rounds = if cfg!(miri) { 40 } else { 4000 };
    for _ in 0..rounds {
        let len = prng.below(90);
        let breaks = prng.below(len + 2);
        let mut opaque = random_row(&mut prng, len);
        let mut translucent = random_row(&mut prng, len);
        for (index, (opaque, translucent)) in opaque.iter_mut().zip(&mut translucent).enumerate() {
            let continues = if index < breaks {
                prng.below(16) != 0
            } else {
                prng.below(2) == 0
            };
            let short = prng.next_u8() % u8::MAX;
            (opaque.a, translucent.a) = if continues {
                (u8::MAX, short)
            } else {
                (short, u8::MAX)
            };
        }
        assert_eq!(
            (ops.opaque)(&opaque),
            (PORTABLE_SCAN.opaque)(&opaque),
            "{name}: opaque run over {len} pixels"
        );
        assert_eq!(
            (ops.translucent)(&translucent),
            (PORTABLE_SCAN.translucent)(&translucent),
            "{name}: translucent run over {len} pixels"
        );
    }
}

#[test]
fn every_scan_candidate_matches_the_portable_scan_on_random_spans() {
    for candidate in SCAN_CANDIDATES {
        assert_scans_match(candidate.impl_, candidate.name);
    }
}

/// A window too wide for a lane holds sums past 32 bits, where only the
/// reference's saturating arithmetic is defined and a candidate's wrapping
/// sums drift from it: such a window takes the portable kernels.
#[test]
fn a_window_past_the_lane_form_takes_the_portable_kernels() {
    let wide = Reciprocal::new(16_843_009);
    let white = [Pixel {
        r: 255,
        g: 255,
        b: 255,
        a: 255,
    }; 4];
    let full = PixelSum {
        r: u32::MAX,
        g: u32::MAX,
        b: u32::MAX,
        a: u32::MAX,
    };
    for candidate in BLUR_CANDIDATES {
        let mut sums = [full; 4];
        let mut reference = sums;
        (within_lanes(candidate.impl_, wide).slide)(&mut sums, &white, &white);
        slide_portable(&mut reference, &white, &white);
        assert_eq!(sums, reference, "{}", candidate.name);
    }
}

#[test]
fn the_portable_path_folds_to_the_conformance_digest() {
    assert_eq!(
        digest_with(PORTABLE_BLEND, PORTABLE_BLUR, PORTABLE_SCAN),
        CONFORMANCE_DIGEST
    );
}

#[test]
fn every_candidate_this_host_runs_folds_to_the_conformance_digest() {
    for blend in BLEND_CANDIDATES {
        for blur in BLUR_CANDIDATES {
            for scan in SCAN_CANDIDATES {
                assert_eq!(
                    digest_with(blend.impl_, blur.impl_, scan.impl_),
                    CONFORMANCE_DIGEST,
                    "{} with {} and {}",
                    blend.name,
                    blur.name,
                    scan.name
                );
            }
        }
    }
}
