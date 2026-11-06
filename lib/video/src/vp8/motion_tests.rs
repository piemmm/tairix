//! Inter prediction tests.
//!
//! Every expected sample is worked out here from the format's own numbers,
//! written out rather than taken from the module: the taps a fraction weighs
//! with, a coordinate past an edge clamped to it, and the rounding of a chroma
//! vector.

use alloc::vec::Vec;

use super::Interpolation::{Bilinear, FullPixel, SixTap};
use super::{
    chroma_mv, luma_sad, predict_inter, Interpolation, Motion, Mv, RefPlane, EDGE_SCRATCH,
};
use crate::vp8::Plane;

/// The six-tap filters by eighth-pel phase (RFC 6386 §18.3).
const SIX_TAP: [[i32; 6]; 8] = [
    [0, 0, 128, 0, 0, 0],
    [0, -6, 123, 12, -1, 0],
    [2, -11, 108, 36, -8, 1],
    [0, -9, 93, 50, -6, 0],
    [3, -16, 77, 77, -16, 3],
    [0, -6, 50, 93, -9, 0],
    [1, -8, 36, 108, -11, 2],
    [0, -1, 12, 123, -6, 0],
];

/// The bilinear filters by phase, in the same six-sample form.
const BILINEAR: [[i32; 6]; 8] = [
    [0, 0, 128, 0, 0, 0],
    [0, 0, 112, 16, 0, 0],
    [0, 0, 96, 32, 0, 0],
    [0, 0, 80, 48, 0, 0],
    [0, 0, 64, 64, 0, 0],
    [0, 0, 48, 80, 0, 0],
    [0, 0, 32, 96, 0, 0],
    [0, 0, 16, 112, 0, 0],
];

/// Every version's interpolation.
const ALL: [Interpolation; 3] = [SixTap, Bilinear, FullPixel];

/// A vector of `x` and `y` eighth-pels.
const fn mv(x: i16, y: i16) -> Mv {
    Mv { x, y }
}

/// `value` as a sample, wrapping.
fn byte(value: usize) -> u8 {
    u8::try_from(value % 256).expect("a byte")
}

/// `value` as a coordinate.
fn signed(value: usize) -> isize {
    isize::try_from(value).expect("a coordinate")
}

/// Where macroblock `at` starts in a plane whose macroblocks are `side`
/// samples square.
fn corner(at: (usize, usize), side: usize) -> (isize, isize) {
    (signed(at.0 * side), signed(at.1 * side))
}

/// A reference frame: a luma plane and two chroma planes half its size, each
/// sample `pattern(plane, x, y)`.
struct Frame {
    planes: [Vec<u8>; 3],
    sizes: [(usize, usize); 3],
}

impl Frame {
    fn new(width: usize, height: usize, pattern: impl Fn(usize, usize, usize) -> u8) -> Self {
        let half = (width / 2, height / 2);
        let sizes = [(width, height), half, half];
        let planes = core::array::from_fn(|plane| {
            let (columns, rows) = sizes[plane];
            (0..rows * columns)
                .map(|at| pattern(plane, at % columns, at / columns))
                .collect()
        });
        Self { planes, sizes }
    }

    /// The planes as prediction reads them.
    fn views(&self) -> [RefPlane<'_>; 3] {
        core::array::from_fn(|plane| {
            let (width, height) = self.sizes[plane];
            RefPlane::new(&self.planes[plane], width, width, height).expect("a whole plane")
        })
    }

    /// Sample (`x`, `y`) of `plane`, each coordinate clamped into the plane as
    /// the format extends a reference past its edges.
    fn sample(&self, plane: usize, x: isize, y: isize) -> u8 {
        let (width, height) = self.sizes[plane];
        let clamp = |at: isize, size: usize| usize::try_from(at).map_or(0, |at| at.min(size - 1));
        self.planes[plane][clamp(y, height) * width + clamp(x, width)]
    }
}

/// One macroblock's prediction: its luma block, then its blue and red ones.
#[derive(Debug, PartialEq)]
struct Predicted {
    luma: [[u8; 16]; 16],
    chroma: [[[u8; 8]; 8]; 2],
}

/// Predict macroblock `at` of `frame` into planes holding one macroblock row,
/// and read it back.
fn predict(
    frame: &Frame,
    motion: &Motion,
    interpolation: Interpolation,
    at: (usize, usize),
) -> Predicted {
    let width = frame.sizes[0].0;
    let mut luma = Plane::new(width, 16).expect("a luma row");
    let mut blue = Plane::new(width / 2, 8).expect("a chroma row");
    let mut red = Plane::new(width / 2, 8).expect("a chroma row");
    let dst = [&mut luma, &mut blue, &mut red];
    predict_inter(&frame.views(), motion, interpolation, at, dst, 0);
    Predicted {
        luma: block(&luma, at.0 * 16, 0),
        chroma: [block(&blue, at.0 * 8, 0), block(&red, at.0 * 8, 0)],
    }
}

/// The `N`×`N` block of `plane` whose top-left sample is (`x`, `y`).
fn block<const N: usize>(plane: &Plane, x: usize, y: usize) -> [[u8; N]; N] {
    core::array::from_fn(|row| {
        core::array::from_fn(|column| plane.get(signed(x + column), signed(y + row)))
    })
}

/// The `N`×`N` block of `frame`'s `plane` from (`x`, `y`) on, each coordinate
/// clamped into the plane.
fn shifted<const N: usize>(frame: &Frame, plane: usize, (x, y): (isize, isize)) -> [[u8; N]; N] {
    core::array::from_fn(|row| {
        core::array::from_fn(|column| frame.sample(plane, x + signed(column), y + signed(row)))
    })
}

/// The `M`×`M` part of `whole` whose top-left sample is (`x`, `y`).
fn part<const N: usize, const M: usize>(
    whole: &[[u8; N]; N],
    (x, y): (usize, usize),
) -> [[u8; M]; M] {
    core::array::from_fn(|row| core::array::from_fn(|column| whole[y + row][x + column]))
}

/// Sample `at` of `plane` moved by `vector`, worked out by hand: on each of
/// six rows the samples two before to three after weighed across, rounded and
/// clamped, then those six weighed down.
fn worked(
    frame: &Frame,
    plane: usize,
    at: (isize, isize),
    vector: Mv,
    filters: &[[i32; 6]; 8],
) -> u8 {
    let x = at.0 + isize::from(vector.x >> 3);
    let y = at.1 + isize::from(vector.y >> 3);
    let phase = |component: i16| usize::try_from(component & 7).expect("a phase");
    let (across, down) = (&filters[phase(vector.x)], &filters[phase(vector.y)]);
    let weigh = |weights: &[i32; 6], samples: [i32; 6]| {
        let sum: i32 = weights
            .iter()
            .zip(samples)
            .map(|(weight, sample)| weight * sample)
            .sum();
        ((sum + 64) >> 7).clamp(0, 255)
    };
    let sample = |x: isize, y: isize| i32::from(frame.sample(plane, x, y));
    let row = |y: isize| {
        let samples = core::array::from_fn(|tap| sample(x + signed(tap) - 2, y));
        weigh(across, samples)
    };
    let column = core::array::from_fn(|tap| row(y + signed(tap) - 2));
    u8::try_from(weigh(down, column)).expect("clamped")
}

/// What macroblock `at` of `frame` predicts to, worked out sample by sample
/// with the luma and the chroma `filters`, a whole macroblock's vector being
/// every subblock's.
fn worked_macroblock(
    frame: &Frame,
    motion: &Motion,
    (luma_filters, chroma_filters): (&[[i32; 6]; 8], &[[i32; 6]; 8]),
    at: (usize, usize),
) -> Predicted {
    let vectors = match motion {
        Motion::Whole(vector) => [*vector; 16],
        Motion::Split(vectors) => *vectors,
    };
    let (x, y) = corner(at, 16);
    let luma = core::array::from_fn(|row| {
        core::array::from_fn(|column| {
            let vector = vectors[(row / 4) * 4 + column / 4];
            let place = (x + signed(column), y + signed(row));
            worked(frame, 0, place, vector, luma_filters)
        })
    });
    let chroma = core::array::from_fn(|plane| {
        core::array::from_fn(|row| {
            core::array::from_fn(|column| {
                let first = (row / 4) * 8 + (column / 4) * 2;
                let quarter = [0, 1, 4, 5].map(|step| vectors[first + step]);
                let place = (x / 2 + signed(column), y / 2 + signed(row));
                worked(frame, plane + 1, place, chroma_mv(quarter), chroma_filters)
            })
        })
    });
    Predicted { luma, chroma }
}

#[test]
fn a_flat_reference_stays_flat_under_every_phase_and_version() {
    let values = [90, 60, 200];
    let frame = Frame::new(48, 48, |plane, _, _| values[plane]);
    let expected = Predicted {
        luma: [[values[0]; 16]; 16],
        chroma: [[[values[1]; 8]; 8], [[values[2]; 8]; 8]],
    };
    let split = Motion::Split(core::array::from_fn(|block| {
        let block = i16::try_from(block).expect("a subblock");
        mv(5 * block - 40, 23 - 3 * block)
    }));
    let whole = (-9..=9).flat_map(|x| (-9..=9).map(move |y| Motion::Whole(mv(x, y))));
    for motion in whole.chain([split]) {
        for interpolation in ALL {
            for at in [(0, 0), (2, 2)] {
                let predicted = predict(&frame, &motion, interpolation, at);
                assert_eq!(predicted, expected, "{motion:?}, {interpolation:?}");
            }
        }
    }
}

#[test]
fn a_whole_pixel_vector_is_an_exact_shifted_copy_however_far_past_an_edge() {
    let frame = Frame::new(32, 32, |plane, x, y| byte(7 * x + 13 * y + 50 * plane));
    // Whole luma samples, and even, so the chroma vector is whole too.
    let shifts = [
        (0, 0),
        (2, -4),
        (-6, 10),
        (-40, 40),
        (-4096, 4094),
        (4094, -4096),
    ];
    for (dx, dy) in shifts {
        let motion = Motion::Whole(mv(dx * 8, dy * 8));
        for at in [(0, 0), (1, 1)] {
            let predicted = predict(&frame, &motion, SixTap, at);
            let (x, y) = corner(at, 16);
            let from = (x + isize::from(dx), y + isize::from(dy));
            let luma = shifted::<16>(&frame, 0, from);
            assert_eq!(predicted.luma, luma, "({dx}, {dy}) at {at:?}");
            let half = (from.0 / 2, from.1 / 2);
            let chroma = [1, 2].map(|plane| shifted::<8>(&frame, plane, half));
            assert_eq!(predicted.chroma, chroma, "({dx}, {dy}) at {at:?}");
        }
    }
}

#[test]
fn six_tap_half_and_quarter_pels_weigh_their_taps() {
    let across = Frame::new(48, 48, |_, x, _| byte(4 * x + 10));
    let predicted = predict(&across, &Motion::Whole(mv(4, 0)), SixTap, (1, 1));
    for (x, &sample) in (16..).zip(&predicted.luma[0]) {
        let p = |offset: isize| i32::from(across.sample(0, x + offset, 16));
        let half = 3 * p(-2) - 16 * p(-1) + 77 * p(0) + 77 * p(1) - 16 * p(2) + 3 * p(3);
        assert_eq!(i32::from(sample), (half + 64) >> 7);
    }
    let first = predicted.luma[0];
    assert!(predicted.luma.iter().all(|line| *line == first));
    let down = Frame::new(48, 48, |_, _, y| byte(4 * y + 10));
    let predicted = predict(&down, &Motion::Whole(mv(0, 2)), SixTap, (1, 1));
    for (y, line) in (16..).zip(&predicted.luma) {
        let p = |offset: isize| i32::from(down.sample(0, 16, y + offset));
        let quarter = 2 * p(-2) - 11 * p(-1) + 108 * p(0) + 36 * p(1) - 8 * p(2) + p(3);
        assert!(line
            .iter()
            .all(|&sample| i32::from(sample) == (quarter + 64) >> 7));
    }
}

#[test]
fn six_tap_clamps_after_each_pass() {
    // Bars two samples wide on even rows, mid-grey odd ones. Across a bar's
    // edges the half-pel taps weigh 300 and -45; the pass down weighs them
    // clamped, where unclamped they would give 214 and 42.
    let bars = Frame::new(48, 48, |_, x, y| match (y % 2, x % 4) {
        (1, _) => 128,
        (_, 0 | 1) => 250,
        _ => 5,
    });
    let across = predict(&bars, &Motion::Whole(mv(4, 0)), SixTap, (1, 1));
    for (row, line) in across.luma.iter().enumerate() {
        let expected = if row % 2 == 0 {
            [255, 128, 0, 128]
        } else {
            [128; 4]
        };
        assert!(
            line.as_chunks::<4>().0.iter().all(|run| *run == expected),
            "row {row}"
        );
    }
    let both = predict(&bars, &Motion::Whole(mv(4, 4)), SixTap, (1, 1));
    for line in &both.luma {
        assert!(line
            .as_chunks::<4>()
            .0
            .iter()
            .all(|run| *run == [192, 128, 64, 128]));
    }
}

#[test]
fn bilinear_fractions_weigh_two_samples_by_hand() {
    let ramp = Frame::new(48, 48, |_, x, y| byte(2 * x + 3 * y + 10));
    let cases = [
        (mv(2, 0), [96, 32], [128, 0]),
        (mv(0, 6), [128, 0], [32, 96]),
        (mv(4, 2), [64, 64], [96, 32]),
    ];
    let p = |x: isize, y: isize| i32::from(ramp.sample(0, x, y));
    for (vector, across, down) in cases {
        let predicted = predict(&ramp, &Motion::Whole(vector), Bilinear, (1, 1));
        for (y, line) in (16..).zip(&predicted.luma) {
            for (x, &sample) in (16..).zip(line) {
                let middle = |y: isize| (across[0] * p(x, y) + across[1] * p(x + 1, y) + 64) >> 7;
                let expected = (down[0] * middle(y) + down[1] * middle(y + 1) + 64) >> 7;
                assert_eq!(i32::from(sample), expected, "{vector:?} at ({x}, {y})");
            }
        }
    }
}

#[test]
fn chroma_vectors_average_and_round_half_away_from_zero() {
    // Four luma components and the chroma one they make, each case mirrored
    // into the other component with its signs flipped.
    let cases = [
        // A whole macroblock's vector four times: halved, exactly for the
        // even values a doubled luma vector holds, away from zero for an odd.
        ([6; 4], 3),
        ([2; 4], 1),
        ([3; 4], 2),
        // A split macroblock's four, summed and divided by eight: sums of 5,
        // 3 and 11, odd either side of zero, and a half.
        ([2, 4, 6, 8], 3),
        ([1, 1, 1, 2], 1),
        ([1, 1, 1, 0], 0),
        ([6, 2, 2, 1], 1),
        ([2, 2, 0, 0], 1),
        ([i16::MAX; 4], 16384),
    ];
    for (components, expected) in cases {
        let luma = components.map(|component| mv(component, -component));
        assert_eq!(chroma_mv(luma), mv(expected, -expected), "{components:?}");
    }
    assert_eq!(chroma_mv([mv(i16::MIN, 0); 4]), mv(-16384, 0));
}

/// Version 3 drops a chroma vector's fraction but interpolates luma as
/// versions 1 and 2 do, as the reference decoder has it.
#[test]
fn version_three_drops_only_chromas_fractions() {
    let frame = Frame::new(48, 48, |plane, x, y| byte(3 * x + 5 * y + 70 * plane));
    let motion = Motion::Whole(mv(13, -11));
    let predicted = predict(&frame, &motion, FullPixel, (1, 1));
    // The chroma vector, 7 and -6 eighths, rounded down, is none right and
    // one up.
    assert_eq!(
        predicted.luma,
        predict(&frame, &motion, Bilinear, (1, 1)).luma
    );
    assert_ne!(predicted.luma, shifted::<16>(&frame, 0, (17, 14)));
    let chroma = [1, 2].map(|plane| shifted::<8>(&frame, plane, (8, 7)));
    assert_eq!(predicted.chroma, chroma);
    assert_ne!(predict(&frame, &motion, SixTap, (1, 1)), predicted);
}

#[test]
fn every_fraction_reaching_past_an_edge_weighs_the_edge_repeated() {
    let frame = Frame::new(48, 48, |plane, x, y| {
        byte(x * x + 7 * y * y + 3 * x * y + 40 * plane)
    });
    let whole_samples = [[0, 0, 128, 0, 0, 0]; 8];
    let versions = [
        (SixTap, (&SIX_TAP, &SIX_TAP)),
        (Bilinear, (&BILINEAR, &BILINEAR)),
        (FullPixel, (&BILINEAR, &whole_samples)),
    ];
    // Whole parts carrying a block past each edge from the corners, and one
    // keeping it inside.
    let reaches = [
        ((0, 0), (-3, -2)),
        ((1, 1), (-1, 0)),
        ((2, 2), (2, 3)),
        ((2, 0), (-36, 5)),
    ];
    for (interpolation, filters) in versions {
        for phase in 0..64 {
            for (at, (dx, dy)) in reaches {
                let vector = mv(8 * dx + phase % 8, 8 * dy + phase / 8);
                let split = Motion::Split(core::array::from_fn(|block| {
                    let block = i16::try_from(block).expect("a subblock");
                    mv(vector.x + block - 7, vector.y + 2 * block - 15)
                }));
                for motion in [Motion::Whole(vector), split] {
                    let predicted = predict(&frame, &motion, interpolation, at);
                    let by_hand = worked_macroblock(&frame, &motion, filters, at);
                    assert_eq!(predicted, by_hand, "{interpolation:?} {at:?} {motion:?}");
                }
            }
        }
    }
}

#[test]
fn a_split_macroblock_moves_each_subblock_by_its_own_vector() {
    let frame = Frame::new(48, 48, |plane, x, y| byte(11 * x + 17 * y + 40 * plane));
    // Whole luma samples, each quarter's four summing to whole chroma ones.
    let shifts = [(1, 2), (3, -2), (-1, 4), (5, 4)];
    let offsets = [0, 2, -4, 6];
    let vectors = core::array::from_fn(|block| {
        let offset = offsets[(block / 8) * 2 + (block % 4) / 2];
        let (dx, dy) = shifts[(block / 4 % 2) * 2 + block % 2];
        mv(8 * (dx + offset), 8 * (dy + offset))
    });
    let predicted = predict(&frame, &Motion::Split(vectors), SixTap, (1, 1));
    for (block, vector) in vectors.iter().enumerate() {
        let place = ((block % 4) * 4, (block / 4) * 4);
        let x = signed(16 + place.0) + isize::from(vector.x / 8);
        let y = signed(16 + place.1) + isize::from(vector.y / 8);
        let expected = shifted::<4>(&frame, 0, (x, y));
        assert_eq!(part(&predicted.luma, place), expected, "subblock {block}");
    }
    for (quarter, offset) in offsets.iter().enumerate() {
        // Each way the quarter's vectors sum to 8 + 4 × `offset` samples, an
        // eighth of which the chroma block moves.
        let shift = isize::from((8 + 4 * offset) / 8);
        let place = ((quarter % 2) * 4, (quarter / 2) * 4);
        let from = (signed(8 + place.0) + shift, signed(8 + place.1) + shift);
        for (plane, chroma) in (1..).zip(&predicted.chroma) {
            let expected = shifted::<4>(&frame, plane, from);
            assert_eq!(part(chroma, place), expected, "quarter {quarter}");
        }
    }
}

#[test]
fn a_split_macroblock_whose_vectors_agree_predicts_as_its_one_vector() {
    let frame = Frame::new(48, 32, |plane, x, y| byte(x * y + 5 * x + 40 * plane));
    for interpolation in ALL {
        for vector in [mv(6, -10), mv(-3, 13), mv(-29, 4)] {
            for at in [(0, 0), (2, 1)] {
                let split = predict(&frame, &Motion::Split([vector; 16]), interpolation, at);
                let whole = predict(&frame, &Motion::Whole(vector), interpolation, at);
                assert_eq!(split, whole, "{interpolation:?} {vector:?} at {at:?}");
            }
        }
    }
}

#[test]
fn a_macroblock_lands_on_the_destination_row_named() {
    let frame = Frame::new(32, 32, |plane, x, y| byte(x + 3 * y + 40 * plane));
    let motion = Motion::Whole(mv(12, -20));
    let expected = predict(&frame, &motion, SixTap, (1, 1));
    let mut luma = Plane::new(32, 32).expect("a luma plane");
    let mut blue = Plane::new(16, 16).expect("a chroma plane");
    let mut red = Plane::new(16, 16).expect("a chroma plane");
    let dst = [&mut luma, &mut blue, &mut red];
    predict_inter(&frame.views(), &motion, SixTap, (1, 1), dst, 16);
    assert_eq!(block::<16>(&luma, 16, 16), expected.luma);
    assert_eq!(block::<16>(&luma, 16, 0), [[0; 16]; 16]);
    let chroma = [&blue, &red].map(|plane| block::<8>(plane, 8, 8));
    assert_eq!(chroma, expected.chroma);
}

#[test]
fn a_reference_view_refuses_a_geometry_its_samples_do_not_hold() {
    let samples = [0; 64];
    assert!(RefPlane::new(&samples, 8, 8, 8).is_some());
    assert!(RefPlane::new(&samples, 10, 8, 6).is_some());
    assert!(RefPlane::new(&samples, 10, 8, 7).is_none());
    assert!(RefPlane::new(&samples, 7, 8, 4).is_none());
    assert!(RefPlane::new(&samples, 8, 0, 8).is_none());
    assert!(RefPlane::new(&samples, 8, 8, 0).is_none());
    let widest = alloc::vec![0; (1 << 14) + 1];
    assert!(RefPlane::new(&widest, 1 << 14, 1 << 14, 1).is_some());
    assert!(RefPlane::new(&widest, (1 << 14) + 1, (1 << 14) + 1, 1).is_none());
}

#[test]
fn each_version_names_its_interpolation() {
    let expected = [
        Some(SixTap),
        Some(Bilinear),
        Some(Bilinear),
        Some(FullPixel),
        None,
    ];
    assert_eq!([0, 1, 2, 3, 4].map(Interpolation::of_version), expected);
}

#[test]
fn a_luma_block_misses_what_it_is_matched_against_by_its_absolute_differences() {
    let frame = Frame::new(32, 32, |plane, x, y| byte(7 * x + 13 * y + 50 * plane));
    let wanted: [u8; 256] = core::array::from_fn(|at| byte(at * 5 + 3));
    let views = frame.views();
    for at in [(0, 0), (8, 5), (16, 16), (-9, 3), (30, -20), (-40, 40)] {
        let expected: u32 = shifted::<16>(&frame, 0, at)
            .as_flattened()
            .iter()
            .zip(&wanted)
            .map(|(&made, &asked)| u32::from(made.abs_diff(asked)))
            .sum();
        let mut edge = EDGE_SCRATCH;
        assert_eq!(
            luma_sad(&views[0], at, &wanted, &mut edge),
            expected,
            "at {at:?}"
        );
    }
}
