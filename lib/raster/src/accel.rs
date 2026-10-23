//! The compositor's per-pixel kernels, each a `lib/cpuops` family with a
//! portable baseline and, on targets whose vector unit is part of the target
//! itself, a packed candidate.
//!
//! A family resolves the first time it is used, against the feature set the
//! process was delivered: the candidate is taken only when its feature bit is
//! present **and** it reproduces the baseline bit for bit over the family's
//! fixed vectors, so a candidate can never change a pixel. With nothing
//! delivered — a host test, an image that never set it — the baseline runs.
//!
//! Selection is by capability, never by benchmark (`ByPriority`): a packed
//! blend that rounds as the scalar one does is faster on every core that has
//! the instructions, so there is nothing for a measurement to decide.
//!
//! Every candidate is a safe function because its extension is part of the
//! target's own baseline, which is all the build script admits a module for.
//! [`resolve`] trusts the set it is handed, so a candidate needing a feature
//! beyond that baseline (AVX2, SVE) cannot join these tables: its call would
//! be sound only once the feature was confirmed on the running core.

use tairix_cpuops::{
    Candidate, CoreKey, CpuFeatureSet, Decision, Family, FamilyId, Resolved, Selected, Selection,
    Selector,
};

use core::ops::Range;

use crate::blur::{
    add_portable, line_portable, mean_portable, slide_portable, PixelSum, Reciprocal,
};
use crate::color::{
    blend_solid_portable, blend_span_portable, cover_span_portable, leading_opaque_portable,
    leading_translucent_portable, mix_span_portable, wash_weighted_portable, Color, Pixel,
};
use crate::dither::DitherRow;

// Intrinsics carry `unsafe`; beside these kernels only the lent-memory view
// of bytes as pixels and the column bands may.
#[cfg(raster_neon)]
#[allow(unsafe_code)]
mod neon;
#[cfg(raster_sse2)]
#[allow(unsafe_code)]
mod sse2;

/// One span over another: `dst[i]` takes `src[i]` scaled by `factor`/255,
/// rounding at `tile[i % 8]`, for `i` below the shorter length.
pub(crate) type SpanBlend = fn(&mut [Pixel], &[Pixel], u8, &[u32; DitherRow::PERIOD]);

/// One colour over a span, rounding as [`SpanBlend`] does.
pub(crate) type SolidBlend = fn(&mut [Pixel], Pixel, u8, &[u32; DitherRow::PERIOD]);

/// One span dissolving into another: `dst[i]` takes `weight`/255 of the way
/// from `from[i]` to `to[i]`, rounding at `tile[i % 8]`, for `i` below the
/// shortest length.
pub(crate) type SpanMix = fn(&mut [Pixel], &[Pixel], &[Pixel], u8, &[u32; DitherRow::PERIOD]);

/// One colour over a span through a mask: [`SolidBlend`] with `coverage[i]`
/// as pixel `i`'s factor, for `i` below the shorter length.
pub(crate) type SolidCover = fn(&mut [Pixel], Pixel, &[u8], &[u32; DitherRow::PERIOD]);

/// One straight colour washed over a span: `dst[i]` moves `weights[i]`/255 of
/// the way to the colour made opaque, rounding at `tile[i % 8]`, for `i` below
/// the shorter length.
pub(crate) type SolidWash = fn(&mut [Pixel], Color, &[u8], &[u32; DitherRow::PERIOD]);

/// Wide samples two pixels apart widened onto a span, as
/// [`widen`](crate::wide::widen) describes.
pub(crate) type SampleWiden = fn(&mut [Pixel], (u32, &[u64]), &[u64; 8]);

/// The span blend family's entry points, chosen together so a solid run, a
/// covered one and the client run beside them can never round differently.
#[derive(Copy, Clone)]
pub(crate) struct BlendOps {
    pub(crate) span: SpanBlend,
    pub(crate) solid: SolidBlend,
    pub(crate) mix: SpanMix,
    pub(crate) cover: SolidCover,
    pub(crate) wash: SolidWash,
    pub(crate) widen: SampleWiden,
}

/// Lay `dst`, a tail shorter than a candidate's `N`-pixel step, through `lay`
/// as one whole step: its pixels and their `levels` padded with nought, which
/// leaves a padding pixel as it was, and copied back. A short row — a glyph's,
/// a thin shape's — then takes the packed path rather than a pixel at a time.
#[cfg(any(raster_sse2, raster_neon))]
fn padded<const N: usize>(
    dst: &mut [Pixel],
    levels: &[u8],
    lay: impl FnOnce(&mut [[Pixel; N]], &[[u8; N]]),
) {
    let len = dst.len().min(levels.len()).min(N);
    if len == 0 {
        return;
    }
    let (mut pixels, mut padded_levels) = ([Pixel::TRANSPARENT; N], [0u8; N]);
    if let (Some(into), Some(from), Some(levels_into), Some(levels_from)) = (
        pixels.get_mut(..len),
        dst.get(..len),
        padded_levels.get_mut(..len),
        levels.get(..len),
    ) {
        into.copy_from_slice(from);
        levels_into.copy_from_slice(levels_from);
    }
    lay(
        core::slice::from_mut(&mut pixels),
        core::slice::from_ref(&padded_levels),
    );
    if let (Some(into), Some(from)) = (dst.get_mut(..len), pixels.get(..len)) {
        into.copy_from_slice(from);
    }
}

/// The span blend family's stable id, which its decisions name.
pub(crate) const BLEND_FAMILY: FamilyId = FamilyId("raster-blend");

/// The portable span blend's name.
pub(crate) const BLEND_PORTABLE: &str = "raster-blend-portable";

const PORTABLE_BLEND: BlendOps = BlendOps {
    span: blend_span_portable,
    solid: blend_solid_portable,
    mix: mix_span_portable,
    cover: cover_span_portable,
    wash: wash_weighted_portable,
    widen: crate::wide::widen_portable,
};

#[cfg(raster_sse2)]
const BLEND_CANDIDATES: &[Candidate<BlendOps>] = &[Candidate {
    name: sse2::BLEND_NAME,
    requires: &[tairix_cpuops::CpuFeature::Sse2],
    impl_: BlendOps {
        span: sse2::blend_span,
        solid: sse2::blend_solid,
        mix: sse2::mix_span,
        cover: sse2::cover_span,
        wash: sse2::wash_weighted,
        widen: sse2::widen_samples,
    },
}];
#[cfg(raster_neon)]
const BLEND_CANDIDATES: &[Candidate<BlendOps>] = &[Candidate {
    name: neon::BLEND_NAME,
    requires: &[tairix_cpuops::CpuFeature::Asimd],
    impl_: BlendOps {
        span: neon::blend_span,
        solid: neon::blend_solid,
        mix: neon::mix_span,
        cover: neon::cover_span,
        wash: neon::wash_weighted,
        widen: neon::widen_samples,
    },
}];
#[cfg(not(any(raster_sse2, raster_neon)))]
const BLEND_CANDIDATES: &[Candidate<BlendOps>] = &[];

static BLEND: Resolved<BlendOps> = Resolved::new();

/// The span blend in force.
#[inline]
pub(crate) fn blend() -> BlendOps {
    in_force(&BLEND, BLEND_CANDIDATES, select_blend, PORTABLE_BLEND)
}

/// What `family` has in force: `portable` in a build with no candidates,
/// which has nothing to resolve.
#[inline]
fn in_force<T: Copy>(
    family: &Resolved<T>,
    candidates: &[Candidate<T>],
    select: fn(CpuFeatureSet) -> Selected<T>,
    portable: T,
) -> T {
    if candidates.is_empty() {
        return portable;
    }
    family.get(|features| select(features).impl_, portable)
}

/// A box blur's pass along a line, stride one: `blur::blur_span`'s contract.
pub(crate) type LineBlur = fn(&[Pixel], &mut [Pixel], usize, usize, Reciprocal, Range<usize>);

/// Add a row into a run of column sums, pairwise.
pub(crate) type RowAdd = fn(&mut [PixelSum], &[Pixel]);

/// Move a run of column sums a row on: add the entering row, remove the
/// leaving one.
pub(crate) type RowSlide = fn(&mut [PixelSum], &[Pixel], &[Pixel]);

/// Write each column sum's rounded mean.
pub(crate) type RowMean = fn(&mut [Pixel], &[PixelSum], Reciprocal);

/// The box blur family: the horizontal pass, and the vertical pass's row
/// operations, chosen together.
#[derive(Copy, Clone)]
pub(crate) struct BlurOps {
    pub(crate) line: LineBlur,
    pub(crate) add: RowAdd,
    pub(crate) slide: RowSlide,
    pub(crate) mean: RowMean,
}

/// The box blur family's stable id, which its decisions name.
pub(crate) const BLUR_FAMILY: FamilyId = FamilyId("raster-blur");

/// The portable box blur's name.
pub(crate) const BLUR_PORTABLE: &str = "raster-blur-portable";

const PORTABLE_BLUR: BlurOps = BlurOps {
    line: line_portable,
    add: add_portable,
    slide: slide_portable,
    mean: mean_portable,
};

#[cfg(raster_sse2)]
const BLUR_CANDIDATES: &[Candidate<BlurOps>] = &[Candidate {
    name: sse2::BLUR_NAME,
    requires: &[tairix_cpuops::CpuFeature::Sse2],
    impl_: BlurOps {
        line: sse2::line,
        add: sse2::add,
        slide: sse2::slide,
        mean: sse2::mean,
    },
}];
#[cfg(raster_neon)]
const BLUR_CANDIDATES: &[Candidate<BlurOps>] = &[Candidate {
    name: neon::BLUR_NAME,
    requires: &[tairix_cpuops::CpuFeature::Asimd],
    impl_: BlurOps {
        line: neon::line,
        add: neon::add,
        slide: neon::slide,
        mean: neon::mean,
    },
}];
#[cfg(not(any(raster_sse2, raster_neon)))]
const BLUR_CANDIDATES: &[Candidate<BlurOps>] = &[];

static BLUR: Resolved<BlurOps> = Resolved::new();

/// The box blur in force.
#[inline]
pub(crate) fn blur() -> BlurOps {
    in_force(&BLUR, BLUR_CANDIDATES, select_blur, PORTABLE_BLUR)
}

/// The box blur for windows whose mean `recip` takes: the one in force where
/// every sum such a window holds fits a 32-bit lane, so the candidates'
/// wrapping sums equal the reference's saturating ones, and the portable one
/// past that.
#[inline]
pub(crate) fn blur_for(recip: Reciprocal) -> BlurOps {
    within_lanes(blur(), recip)
}

fn within_lanes(ops: BlurOps, recip: Reciprocal) -> BlurOps {
    if recip.sums_fit_lanes() {
        ops
    } else {
        PORTABLE_BLUR
    }
}

/// How many pixels lead a span whose alpha is, or is not, full.
pub(crate) type LeadingRun = fn(&[Pixel]) -> usize;

/// The span scan family: where a run a compositor copies straight on begins
/// and ends, chosen together so the two can never disagree about a pixel.
#[derive(Copy, Clone)]
pub(crate) struct ScanOps {
    pub(crate) opaque: LeadingRun,
    pub(crate) translucent: LeadingRun,
}

/// The span scan family's stable id, which its decisions name.
pub(crate) const SCAN_FAMILY: FamilyId = FamilyId("raster-scan");

/// The portable span scan's name.
pub(crate) const SCAN_PORTABLE: &str = "raster-scan-portable";

const PORTABLE_SCAN: ScanOps = ScanOps {
    opaque: leading_opaque_portable,
    translucent: leading_translucent_portable,
};

#[cfg(raster_sse2)]
const SCAN_CANDIDATES: &[Candidate<ScanOps>] = &[Candidate {
    name: sse2::SCAN_NAME,
    requires: &[tairix_cpuops::CpuFeature::Sse2],
    impl_: ScanOps {
        opaque: sse2::leading_opaque,
        translucent: sse2::leading_translucent,
    },
}];
#[cfg(raster_neon)]
const SCAN_CANDIDATES: &[Candidate<ScanOps>] = &[Candidate {
    name: neon::SCAN_NAME,
    requires: &[tairix_cpuops::CpuFeature::Asimd],
    impl_: ScanOps {
        opaque: neon::leading_opaque,
        translucent: neon::leading_translucent,
    },
}];
#[cfg(not(any(raster_sse2, raster_neon)))]
const SCAN_CANDIDATES: &[Candidate<ScanOps>] = &[];

static SCAN: Resolved<ScanOps> = Resolved::new();

/// The span scan in force.
#[inline]
pub(crate) fn scan() -> ScanOps {
    in_force(&SCAN, SCAN_CANDIDATES, select_scan, PORTABLE_SCAN)
}

/// What [`resolve`] installed for each family: `None` for one that had
/// already resolved, which keeps what it had.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct Decisions {
    /// The span blend's decision.
    pub blend: Option<Decision>,
    /// The box blur's decision.
    pub blur: Option<Decision>,
    /// The span scan's decision.
    pub scan: Option<Decision>,
}

/// Resolve every family not yet resolved against `features`, and answer what
/// each was given.
///
/// A process never needs to: each family resolves itself on first use. This
/// is for a caller holding a feature set of its own — a benchmark, a test, a
/// conformance check that must see what was chosen.
#[must_use = "a decision says which implementation this call put in force"]
pub fn resolve(features: CpuFeatureSet) -> Decisions {
    Decisions {
        blend: install(&BLEND, select_blend(features)),
        blur: install(&BLUR, select_blur(features)),
        scan: install(&SCAN, select_scan(features)),
    }
}

fn install<T: Copy>(family: &Resolved<T>, selected: Selected<T>) -> Option<Decision> {
    family.install(selected.impl_).then_some(selected.decision)
}

fn select_blend(features: CpuFeatureSet) -> Selected<BlendOps> {
    by_priority(
        (BLEND_FAMILY, BLEND_CANDIDATES),
        (BLEND_PORTABLE, PORTABLE_BLEND),
        blend_reference,
        blend_run,
        &blend_cases(),
        features,
    )
}

/// The first of `candidates` `features` admits that reproduces the portable
/// kernels over `vectors`, or the portable kernels.
fn by_priority<T: Copy, In, Out: PartialEq>(
    (id, candidates): (FamilyId, &[Candidate<T>]),
    (name, portable): (&'static str, T),
    reference: fn(&In) -> Out,
    run: fn(T, &In) -> Out,
    vectors: &[In],
    features: CpuFeatureSet,
) -> Selected<T> {
    let family = Family {
        id,
        selection: Selection::ByPriority,
        candidates,
        baseline: Candidate {
            name,
            requires: &[],
            impl_: portable,
        },
        reference,
        run,
        vectors,
    };
    Selector::new().select(&family, features, CoreKey(features.bits()), None)
}

/// The longest span a self-verify case blends: past two of every candidate's
/// widest step, so each crosses its whole-step loop into its tail.
const VERIFY_SPAN: usize = 37;

/// What a verify case blends: a second span, one colour, a dissolve into a
/// third span, one colour through a mask, or one straight colour washed at a
/// weight a pixel.
#[derive(Copy, Clone)]
enum Source {
    Span,
    Solid,
    Mix,
    Cover,
    Wash,
    Widen,
}

/// One self-verify case: its source kind, how many pixels it blends of the
/// [`VERIFY_SPAN`] it writes, the factor, the dither phase, and the seed its
/// pixels are drawn from.
#[derive(Copy, Clone)]
struct BlendCase {
    source: Source,
    len: usize,
    factor: u8,
    phase: u32,
    seed: u32,
}

/// Every length across a step boundary, and two dither phases, for every
/// source; the factor's fast path and its extremes for the sources that take
/// one, and fresh pixels for those that do not.
fn blend_cases() -> [BlendCase; 6 * 7 * 4 * 2] {
    const LENS: [usize; 7] = [0, 1, 7, 8, 16, 17, VERIFY_SPAN];
    const FACTORS: [u8; 4] = [255, 200, 1, 0];
    const PHASES: [u32; 2] = [0, 5];
    let mut cases = [BlendCase {
        source: Source::Span,
        len: 0,
        factor: 0,
        phase: 0,
        seed: 0,
    }; 6 * 7 * 4 * 2];
    let mut at = 0;
    let sources = [
        Source::Span,
        Source::Solid,
        Source::Mix,
        Source::Cover,
        Source::Wash,
        Source::Widen,
    ];
    for source in sources {
        for len in LENS {
            for factor in FACTORS {
                for phase in PHASES {
                    if let Some(case) = cases.get_mut(at) {
                        *case = BlendCase {
                            source,
                            len,
                            factor,
                            phase,
                            seed: u32::try_from(at).unwrap_or(0).wrapping_mul(0x9E37_79B9),
                        };
                    }
                    at += 1;
                }
            }
        }
    }
    cases
}

/// A deterministic byte stream: every value appears, so a candidate sees
/// transparent, opaque, and out-of-range pixels alike.
fn bytes(seed: u32, index: usize) -> [u8; 4] {
    let mut x = seed ^ u32::try_from(index).unwrap_or(0).wrapping_mul(0x85EB_CA6B);
    x ^= x >> 16;
    x = x.wrapping_mul(0x7FEB_352D);
    x ^= x >> 15;
    x = x.wrapping_mul(0x846C_A68B);
    (x ^ (x >> 16)).to_le_bytes()
}

fn pixel(seed: u32, index: usize) -> Pixel {
    let [r, g, b, a] = bytes(seed, index);
    // Every fourth pixel is fully transparent or fully opaque, the two cases
    // the blend treats specially; the rest are arbitrary bytes.
    let a = match index % 4 {
        0 => 0,
        1 => 255,
        _ => a,
    };
    Pixel { r, g, b, a }
}

fn blend_run(ops: BlendOps, case: &BlendCase) -> [Pixel; VERIFY_SPAN] {
    let mut dst = [Pixel::TRANSPARENT; VERIFY_SPAN];
    for (index, slot) in dst.iter_mut().enumerate() {
        *slot = pixel(case.seed ^ 0xD57, index);
    }
    let tile = DitherRow::at(case.seed).tile_at(case.phase);
    let blended = dst.get_mut(..case.len).unwrap_or_default();
    match case.source {
        Source::Span => {
            let mut src = [Pixel::TRANSPARENT; VERIFY_SPAN];
            for (index, slot) in src.iter_mut().enumerate() {
                *slot = pixel(case.seed, index);
            }
            (ops.span)(blended, &src, case.factor, &tile);
        }
        Source::Solid => {
            let colour = pixel(case.seed, 2);
            (ops.solid)(blended, colour, case.factor, &tile);
        }
        Source::Mix => {
            let (mut from, mut to) = (
                [Pixel::TRANSPARENT; VERIFY_SPAN],
                [Pixel::TRANSPARENT; VERIFY_SPAN],
            );
            for (index, (from, to)) in from.iter_mut().zip(&mut to).enumerate() {
                *from = pixel(case.seed, index);
                *to = pixel(case.seed ^ 0x3A3, index);
            }
            (ops.mix)(blended, &from, &to, case.factor, &tile);
        }
        Source::Cover => {
            let colour = pixel(case.seed, 2);
            let mut coverage = [0u8; VERIFY_SPAN];
            for (index, level) in coverage.iter_mut().enumerate() {
                *level = coverage_level(case.seed, index);
            }
            (ops.cover)(blended, colour, &coverage, &tile);
        }
        Source::Wash => {
            let [r, g, b, a] = bytes(case.seed, 3);
            let mut weights = [0u8; VERIFY_SPAN];
            for (index, weight) in weights.iter_mut().enumerate() {
                *weight = coverage_level(case.seed ^ 0x5EED, index);
            }
            (ops.wash)(blended, Color { r, g, b, a }, &weights, &tile);
        }
        Source::Widen => {
            let (start, samples, biases) =
                widen_inputs::<{ VERIFY_SPAN / 2 + 2 }>(case.seed, case.phase);
            (ops.widen)(blended, (start, &samples), &biases);
        }
    }
    dst
}

/// A widen's inputs from `seed`: the start column `phase` names, `N` wide
/// samples, every lane and its bias below 65 536, and the dither's biases
/// spread across the lanes.
fn widen_inputs<const N: usize>(seed: u32, phase: u32) -> (u32, [u64; N], [u64; 8]) {
    let mut samples = [0u64; N];
    for (index, sample) in samples.iter_mut().enumerate() {
        let [r, g, b, fraction] = bytes(seed ^ 0x51DE, index);
        let lane = |level: u8| u64::from(level.min(254)) << 8 | u64::from(fraction);
        *sample = lane(r) | lane(g) << 16 | lane(b) << 32;
    }
    let tile = DitherRow::at(seed).tile_at(0);
    let biases = tile.map(|bias| u64::from(bias) * crate::wide::LANE_ONES);
    (phase, samples, biases)
}

/// A deterministic coverage level: every fourth uncovered and every fourth
/// full, the two a mask treats specially; the rest arbitrary.
fn coverage_level(seed: u32, index: usize) -> u8 {
    match index % 4 {
        0 => 0,
        1 => u8::MAX,
        _ => bytes(seed ^ 0xC0FE, index)[0],
    }
}

fn blend_reference(case: &BlendCase) -> [Pixel; VERIFY_SPAN] {
    blend_run(PORTABLE_BLEND, case)
}

fn select_blur(features: CpuFeatureSet) -> Selected<BlurOps> {
    by_priority(
        (BLUR_FAMILY, BLUR_CANDIDATES),
        (BLUR_PORTABLE, PORTABLE_BLUR),
        blur_reference,
        blur_run,
        &blur_cases(),
        features,
    )
}

/// One self-verify case of the box blur: a line `len` long blurred over the
/// part `out` names at `radius`, and column sums over that many pixels.
#[derive(Clone)]
struct BlurCase {
    len: usize,
    radius: usize,
    out: Range<usize>,
    seed: u32,
}

/// Every edge of a line — empty, shorter than the window, a part at either
/// end — at radii from one to past the lane divisor's reach.
fn blur_cases() -> [BlurCase; 10] {
    let case = |len, radius, out: Range<usize>, seed| BlurCase {
        len,
        radius,
        out,
        seed,
    };
    [
        case(0, 1, 0..0, 1),
        case(1, 1, 0..1, 2),
        case(VERIFY_SPAN, 1, 0..VERIFY_SPAN, 3),
        case(VERIFY_SPAN, 2, 3..30, 4),
        case(VERIFY_SPAN, 5, 0..9, 5),
        case(VERIFY_SPAN, 43, 30..VERIFY_SPAN, 6),
        case(9, 43, 0..9, 7),
        case(VERIFY_SPAN, 127, 0..VERIFY_SPAN, 8),
        case(VERIFY_SPAN, 128, 1..36, 9),
        // A window past the lane divisor's reach: each candidate hands it on.
        case(VERIFY_SPAN, 40_000, 0..VERIFY_SPAN, 10),
    ]
}

/// What a blur case produced: the line's averages, the column sums after
/// rows were added and one slid, and their means.
#[derive(PartialEq)]
struct BlurOut {
    line: [Pixel; VERIFY_SPAN],
    sums: [PixelSum; VERIFY_SPAN],
    means: [Pixel; VERIFY_SPAN],
}

fn blur_run(ops: BlurOps, case: &BlurCase) -> BlurOut {
    let count = case.radius.saturating_mul(2).saturating_add(1);
    let recip = Reciprocal::new(count);
    let row = |seed: u32| {
        let mut row = [Pixel::TRANSPARENT; VERIFY_SPAN];
        for (index, slot) in row.iter_mut().enumerate() {
            let [r, g, b, a] = bytes(seed, index);
            *slot = Pixel { r, g, b, a };
        }
        row
    };
    let line = row(case.seed);
    let mut out = BlurOut {
        line: [Pixel::TRANSPARENT; VERIFY_SPAN],
        sums: [PixelSum::default(); VERIFY_SPAN],
        means: [Pixel::TRANSPARENT; VERIFY_SPAN],
    };
    let source = line.get(..case.len).unwrap_or_default();
    let target = out.line.get_mut(..case.out.len()).unwrap_or_default();
    (ops.line)(
        source,
        target,
        case.len,
        case.radius,
        recip,
        case.out.clone(),
    );

    // A window never holds more rows than its count, which is what keeps a
    // sum inside the range its mean is exact over.
    let rows = count.min(3);
    let sums = out.sums.get_mut(..case.len).unwrap_or_default();
    for at in 0..rows {
        (ops.add)(
            sums,
            &row(case.seed ^ u32::try_from(at).unwrap_or(0).wrapping_add(11)),
        );
    }
    if rows > 0 {
        let leaving = row(case.seed ^ 0xB);
        (ops.slide)(sums, &row(case.seed ^ 0xBEEF), &leaving);
    }
    let means = out.means.get_mut(..case.len).unwrap_or_default();
    (ops.mean)(means, sums, recip);
    out
}

fn blur_reference(case: &BlurCase) -> BlurOut {
    blur_run(PORTABLE_BLUR, case)
}

fn select_scan(features: CpuFeatureSet) -> Selected<ScanOps> {
    by_priority(
        (SCAN_FAMILY, SCAN_CANDIDATES),
        (SCAN_PORTABLE, PORTABLE_SCAN),
        scan_reference,
        scan_run,
        &scan_cases(),
        features,
    )
}

/// The longest span a scan case reads: three of every candidate's widest step
/// and a tail past them.
const SCAN_SPAN: usize = 53;

/// One self-verify case of the span scan: a span `len` long whose run breaks
/// at `at` — past its end for none — on a pixel of alpha `alpha`.
#[derive(Copy, Clone)]
struct ScanCase {
    len: usize,
    at: usize,
    alpha: u8,
}

/// Lengths either side of each step, breaks at every edge of a step and
/// nowhere, and the alphas nearest full and furthest from it.
fn scan_cases() -> [ScanCase; 10 * 9 * 2] {
    const LENS: [usize; 10] = [0, 1, 3, 4, 5, 15, 16, 17, 33, SCAN_SPAN];
    const ATS: [usize; 9] = [0, 1, 3, 4, 15, 16, 17, 32, SCAN_SPAN];
    const ALPHAS: [u8; 2] = [0, 254];
    let mut cases = [ScanCase {
        len: 0,
        at: 0,
        alpha: 0,
    }; 10 * 9 * 2];
    let mut slots = cases.iter_mut();
    for len in LENS {
        for at in ATS {
            for alpha in ALPHAS {
                if let Some(slot) = slots.next() {
                    *slot = ScanCase { len, at, alpha };
                }
            }
        }
    }
    cases
}

/// A case scanned both ways: an opaque run broken by the case's alpha, and a
/// run of every other alpha broken by a full one.
fn scan_run(ops: ScanOps, case: &ScanCase) -> (usize, usize) {
    let mut opaque = [Pixel::TRANSPARENT; SCAN_SPAN];
    let mut translucent = [Pixel::TRANSPARENT; SCAN_SPAN];
    for (index, (opaque, translucent)) in opaque.iter_mut().zip(&mut translucent).enumerate() {
        let [r, g, b, a] = bytes(u32::try_from(case.len).unwrap_or(0), index);
        *opaque = Pixel {
            r,
            g,
            b,
            a: u8::MAX,
        };
        *translucent = Pixel {
            r,
            g,
            b,
            a: if a == u8::MAX { case.alpha } else { a },
        };
    }
    if let Some(pixel) = opaque.get_mut(case.at) {
        pixel.a = case.alpha;
    }
    if let Some(pixel) = translucent.get_mut(case.at) {
        pixel.a = u8::MAX;
    }
    (
        (ops.opaque)(opaque.get(..case.len).unwrap_or_default()),
        (ops.translucent)(translucent.get(..case.len).unwrap_or_default()),
    )
}

fn scan_reference(case: &ScanCase) -> (usize, usize) {
    scan_run(PORTABLE_SCAN, case)
}

/// What [`conformance_digest`] answers on every target, whichever
/// candidate its families resolved to.
#[cfg(any(test, feature = "conformance"))]
///
/// The host suite holds the portable path and every candidate it can run to
/// it, and a guest vertical on each architecture with a candidate holds that
/// candidate to it there, so a candidate the host cannot execute is still
/// proved bit-identical where it runs.
pub const CONFORMANCE_DIGEST: u64 = 5_120_574_275_861_883_754;

/// A fixed workload through the families in force — spans blended, laid,
/// dissolved, covered, washed and widened at every length, factor and phase; lines blurred
/// whole and in part; column sums
/// slid and averaged; spans scanned for their leading runs — folded into one
/// number.
#[cfg(any(test, feature = "conformance"))]
#[must_use]
pub fn conformance_digest() -> u64 {
    digest_with(blend(), blur(), scan())
}

#[cfg(any(test, feature = "conformance"))]
fn digest_with(blend: BlendOps, blur: BlurOps, scan: ScanOps) -> u64 {
    scan_digest(scan, blur_digest(blur, blend_digest(blend, 0)))
}

/// The longest span the conformance workload walks.
#[cfg(any(test, feature = "conformance"))]
const DIGEST_SPAN: usize = 160;

/// `digest` with `pixels`' bytes folded in.
#[cfg(any(test, feature = "conformance"))]
fn fold_pixels(digest: u64, pixels: &[Pixel]) -> u64 {
    let mut bytes = [0u8; 4 * DIGEST_SPAN];
    for (slot, pixel) in bytes.as_chunks_mut::<4>().0.iter_mut().zip(pixels) {
        *slot = [pixel.r, pixel.g, pixel.b, pixel.a];
    }
    tairix_hash::FastHash::hash_bytes(digest, bytes.get(..4 * pixels.len()).unwrap_or_default())
}

/// A span of the workload's pixels drawn from `seed`.
#[cfg(any(test, feature = "conformance"))]
fn digest_row(seed: u32) -> [Pixel; DIGEST_SPAN] {
    let mut row = [Pixel::TRANSPARENT; DIGEST_SPAN];
    for (index, slot) in row.iter_mut().enumerate() {
        *slot = pixel(seed, index);
    }
    row
}

/// `digest` with the blend family's workload folded in: spans blended, laid,
/// dissolved, covered, washed and widened at every length, factor and phase.
#[cfg(any(test, feature = "conformance"))]
fn blend_digest(blend: BlendOps, mut digest: u64) -> u64 {
    for round in 0u32..96 {
        let len = usize::try_from(round * 7 + 3).unwrap_or(0) % DIGEST_SPAN;
        let factor = [255, 200, 128, 1, 0][usize::try_from(round % 5).unwrap_or(0)];
        let tile = DitherRow::at(round).tile_at(round * 3);
        let source = digest_row(round);
        let mut target = digest_row(round ^ 0x5A5A);
        (blend.span)(
            target.get_mut(..len).unwrap_or_default(),
            &source,
            factor,
            &tile,
        );
        digest = fold_pixels(digest, &target);
        (blend.solid)(
            target.get_mut(..len).unwrap_or_default(),
            pixel(round, 1),
            factor,
            &tile,
        );
        digest = fold_pixels(digest, &target);
        (blend.mix)(
            target.get_mut(..len).unwrap_or_default(),
            &source,
            &digest_row(round ^ 0x6C6C),
            factor,
            &tile,
        );
        digest = fold_pixels(digest, &target);
        let mut coverage = [0u8; DIGEST_SPAN];
        for (index, level) in coverage.iter_mut().enumerate() {
            *level = coverage_level(round, index);
        }
        (blend.cover)(
            target.get_mut(..len).unwrap_or_default(),
            pixel(round, 3),
            &coverage,
            &tile,
        );
        digest = fold_pixels(digest, &target);
        let [r, g, b, a] = bytes(round, 5);
        (blend.wash)(
            target.get_mut(..len).unwrap_or_default(),
            Color { r, g, b, a },
            &coverage,
            &tile,
        );
        digest = fold_pixels(digest, &target);
        let (start, samples, biases) = widen_inputs::<{ DIGEST_SPAN / 2 + 2 }>(round, round % 9);
        (blend.widen)(
            target.get_mut(..len).unwrap_or_default(),
            (start, &samples),
            &biases,
        );
        digest = fold_pixels(digest, &target);
    }
    digest
}

/// `digest` with the blur family's workload folded in: lines blurred whole
/// and in part, column sums slid and averaged.
#[cfg(any(test, feature = "conformance"))]
fn blur_digest(blur: BlurOps, mut digest: u64) -> u64 {
    for (round, radius) in (0u32..).zip([1usize, 2, 5, 12, 43, 127, 128, 40_000]) {
        let recip = Reciprocal::new(radius * 2 + 1);
        let source = digest_row(round ^ 0xB1);
        for out in [0..DIGEST_SPAN, 7..90, 120..DIGEST_SPAN] {
            let mut target = digest_row(round ^ 0x77);
            let written = target.get_mut(..out.len()).unwrap_or_default();
            (blur.line)(&source, written, DIGEST_SPAN, radius, recip, out);
            digest = fold_pixels(digest, &target);
        }
        let mut sums = [PixelSum::default(); DIGEST_SPAN];
        let rows = (radius * 2 + 1).min(5);
        for at in 0..rows {
            (blur.add)(
                &mut sums,
                &digest_row(round ^ u32::try_from(at).unwrap_or(0) ^ 0xC3),
            );
        }
        (blur.slide)(
            &mut sums,
            &digest_row(round ^ 0xE5),
            &digest_row(round ^ 0xC3),
        );
        let mut means = [Pixel::TRANSPARENT; DIGEST_SPAN];
        (blur.mean)(&mut means, &sums, recip);
        digest = fold_pixels(digest, &means);
    }
    digest
}

/// `digest` with the scan family's workload folded in: spans scanned for their
/// leading runs, broken at every place.
#[cfg(any(test, feature = "conformance"))]
fn scan_digest(scan: ScanOps, mut digest: u64) -> u64 {
    for round in 0u32..64 {
        let breaks = usize::try_from(round * 5 + 1).unwrap_or(0) % DIGEST_SPAN;
        let mut opaque = digest_row(round ^ 0x3C);
        let mut translucent = digest_row(round ^ 0x96);
        for (index, (opaque, translucent)) in opaque.iter_mut().zip(&mut translucent).enumerate() {
            let broken = index == breaks;
            opaque.a = if broken { 0 } else { u8::MAX };
            translucent.a = match translucent.a {
                _ if broken => u8::MAX,
                u8::MAX => 1,
                a => a,
            };
        }
        for count in [(scan.opaque)(&opaque), (scan.translucent)(&translucent)] {
            let count = u64::try_from(count).unwrap_or(u64::MAX);
            digest = tairix_hash::FastHash::hash_bytes(digest, &count.to_le_bytes());
        }
    }
    digest
}

#[cfg(test)]
#[path = "accel_tests.rs"]
mod tests;
