//! Differential fuzz harness for the vector blend, blur and scan kernels.
//!
//! Every pixel a translucent window shows goes through `blend_span` or
//! `blend_solid_span`, a frosted one through `frost_region`'s line pass and
//! column sums as well, and every row of a window is split into the runs it
//! copies and blends by `leading_opaque` and `leading_translucent`, each
//! resolved once per process to the fastest kernel the CPU admits. A kernel
//! that disagreed with
//! the portable arithmetic anywhere would show as a seam or a colour drift, or
//! read past a row, so this resolves the public calls to this build's
//! candidates and holds them to an oracle spelled in the public per-pixel
//! operators: spans of every length up to several vector steps and every
//! alignment, unequal pairs, every dither phase, factors at and between the
//! extremes, and blocks a few kernels wide with radii that run past their
//! edges and buffers longer than the block.
//!
//! Runs the fixed smoke sweep under plain `cargo test`; keeps drawing from the
//! same seeded stream until `TAIRIX_FUZZ_BUDGET_SECS` elapses under
//! `cargo xtask fuzz`.

use tairix_cpuops::{CpuFeature, CpuFeatureSet};
use tairix_fuzzseed::Prng;
use tairix_raster::accel::resolve;
use tairix_raster::{
    blend_solid_span, blend_span, box_blur, leading_opaque, leading_translucent, BlurScratch,
    DitherRow, Pixel, Surface,
};

/// Rounds of each kernel per sweep of the loop below.
const ROUNDS_PER_SWEEP: u32 = 400;

/// Sweeps a plain `cargo test` runs.
const SMOKE_SWEEPS: u32 = 4;

/// The longest span drawn: several 16-pixel vector steps and every tail.
const MAX_SPAN: usize = 160;

/// The widest and tallest block drawn.
const MAX_SIDE: usize = 24;

/// Every feature a candidate in this tree needs, so whichever this build
/// compiled in is chosen; a build with none keeps the portable kernels.
fn every_kernel_feature() -> CpuFeatureSet {
    CpuFeatureSet::new()
        .with(CpuFeature::Sse2)
        .with(CpuFeature::Asimd)
}

fn pixel(rng: &mut Prng) -> Pixel {
    let [r, g, b] = [rng.next_u8(), rng.next_u8(), rng.next_u8()];
    Pixel {
        r,
        g,
        b,
        a: rng.edge_biased_u8(),
    }
}

fn row(rng: &mut Prng, len: usize) -> Vec<Pixel> {
    (0..len).map(|_| pixel(rng)).collect()
}

fn dither(rng: &mut Prng) -> DitherRow {
    if rng.below(4) == 0 {
        DitherRow::NEAREST
    } else {
        DitherRow::at(rng.next_u32())
    }
}

/// The column of the `index`th pixel of a span starting at `first_x`.
fn column(first_x: u32, index: usize) -> u32 {
    first_x.wrapping_add(u32::try_from(index).expect("a short span"))
}

/// [`blend_span`] one pixel at a time.
fn blend_oracle(dst: &mut [Pixel], src: &[Pixel], factor: u8, dither: DitherRow, first_x: u32) {
    for (index, (dst, &src)) in dst.iter_mut().zip(src).enumerate() {
        if src.a == 0 {
            continue;
        }
        let bias = dither.bias(column(first_x, index));
        *dst = src.scale_alpha_biased(factor, bias).over_biased(*dst, bias);
    }
}

/// A box blur of `radius`, edges replicated, each mean rounded to nearest.
fn blur_oracle(region: &[Pixel], width: usize, height: usize, radius: usize) -> Vec<Pixel> {
    let mean = |sums: [u64; 4]| {
        let d = 2 * radius as u64 + 1;
        let one = |sum: u64| u8::try_from(((sum + d / 2) / d).min(255)).expect("a byte");
        Pixel {
            r: one(sums[0]),
            g: one(sums[1]),
            b: one(sums[2]),
            a: one(sums[3]),
        }
    };
    let pass = |src: &[Pixel]| {
        let last = src.len() - 1;
        (0..src.len())
            .map(|i| {
                let mut sums = [0u64; 4];
                for k in 0..=2 * radius {
                    let sample = src[(i + k).saturating_sub(radius).min(last)];
                    sums[0] += u64::from(sample.r);
                    sums[1] += u64::from(sample.g);
                    sums[2] += u64::from(sample.b);
                    sums[3] += u64::from(sample.a);
                }
                mean(sums)
            })
            .collect::<Vec<_>>()
    };
    let mut across = vec![Pixel::TRANSPARENT; width * height];
    for y in 0..height {
        let line = &region[y * width..(y + 1) * width];
        across[y * width..(y + 1) * width].copy_from_slice(&pass(line));
    }
    let mut out = vec![Pixel::TRANSPARENT; width * height];
    for x in 0..width {
        let column: Vec<Pixel> = (0..height).map(|y| across[y * width + x]).collect();
        for (y, pixel) in pass(&column).into_iter().enumerate() {
            out[y * width + x] = pixel;
        }
    }
    out
}

fn fuzz_blend(rng: &mut Prng) {
    let len = rng.at_most(MAX_SPAN);
    let src_len = if rng.below(4) == 0 {
        rng.at_most(MAX_SPAN)
    } else {
        len
    };
    let src = row(rng, src_len);
    let dst = row(rng, len);
    let (factor, dither, first_x) = (rng.edge_biased_u8(), dither(rng), rng.next_u32());
    let mut kernel = dst.clone();
    blend_span(&mut kernel, &src, factor, dither, first_x);
    let mut oracle = dst;
    blend_oracle(&mut oracle, &src, factor, dither, first_x);
    assert_eq!(
        kernel, oracle,
        "blend of {len} over {src_len} at {first_x}, factor {factor}"
    );

    let solid = pixel(rng);
    let mut kernel = oracle.clone();
    blend_solid_span(&mut kernel, solid, factor, dither, first_x);
    blend_oracle(&mut oracle, &vec![solid; len], factor, dither, first_x);
    assert_eq!(
        kernel, oracle,
        "solid blend of {len} at {first_x}, factor {factor}"
    );
}

fn fuzz_blur(rng: &mut Prng) {
    let width = 1 + rng.below(MAX_SIDE);
    let height = 1 + rng.below(MAX_SIDE);
    let radius = 1 + rng.below(3 * width.max(height));
    let count = width * height;
    // Longer buffers than the block, so a kernel reading past it is caught by
    // what lies there showing in its output.
    let spare = rng.at_most(8);
    let mut region = row(rng, count + spare);
    let mut aux = row(rng, count + spare);
    let oracle = blur_oracle(&region, width, height, radius);
    let tail = region[count..].to_vec();
    box_blur(&mut region, width, height, radius, &mut aux);
    assert_eq!(
        &region[..count],
        oracle.as_slice(),
        "a {width}x{height} block blurred by {radius}"
    );
    assert_eq!(
        &region[count..],
        tail.as_slice(),
        "nothing past the block is written"
    );
}

/// A whole surface frosted at full coverage is its box blur: the frost's own
/// line pass and column sums, held to the oracle.
fn fuzz_frost(rng: &mut Prng) {
    let width = 1 + rng.below(MAX_SIDE);
    let height = 1 + rng.below(MAX_SIDE);
    let radius = 1 + rng.below(3 * width.max(height));
    let pixels = row(rng, width * height);
    let (w, h) = (
        u32::try_from(width).expect("a small side"),
        u32::try_from(height).expect("a small side"),
    );
    let mut surface = Surface::new(w, h).expect("a small surface");
    for (index, &pixel) in pixels.iter().enumerate() {
        let (x, y) = (index % width, index / width);
        surface.set(
            u32::try_from(x).expect("on the surface"),
            u32::try_from(y).expect("on the surface"),
            pixel,
        );
    }
    surface.frost_region(
        0,
        0,
        w,
        h,
        u32::try_from(radius).expect("a small radius"),
        &mut BlurScratch::new(),
        &tairix_parallel::SERIAL,
        |_, _| u8::MAX,
    );
    assert_eq!(
        surface.pixels(),
        blur_oracle(&pixels, width, height, radius).as_slice(),
        "a {width}x{height} surface frosted by {radius}"
    );
}

fn fuzz_scan(rng: &mut Prng) {
    let len = rng.at_most(MAX_SPAN);
    let breaks = rng.at_most(len + 1);
    let mut span = row(rng, len);
    for (index, pixel) in span.iter_mut().enumerate() {
        if index < breaks || rng.below(2) == 0 {
            pixel.a = u8::MAX;
        } else if pixel.a == u8::MAX {
            pixel.a = 0;
        }
    }
    let opaque = span.iter().take_while(|pixel| pixel.a == u8::MAX).count();
    assert_eq!(leading_opaque(&span), opaque, "an opaque run over {len}");
    for pixel in &mut span {
        pixel.a = if pixel.a == u8::MAX {
            rng.next_u8() % u8::MAX
        } else {
            u8::MAX
        };
    }
    let translucent = span.iter().take_while(|pixel| pixel.a != u8::MAX).count();
    assert_eq!(
        leading_translucent(&span),
        translucent,
        "a translucent run over {len}"
    );
}

#[test]
fn the_resolved_kernels_agree_with_the_portable_arithmetic() {
    let _ = resolve(every_kernel_feature());
    let mut rng = Prng::new(tairix_fuzzseed::start(
        "the_resolved_kernels_agree_with_the_portable_arithmetic",
        tairix_fuzzseed::FUZZ_SEED_ENV,
    ));
    let deadline = tairix_fuzzseed::budget_deadline(tairix_fuzzseed::FUZZ_BUDGET_ENV);
    let mut sweeps = 0;
    loop {
        for _ in 0..ROUNDS_PER_SWEEP {
            fuzz_blend(&mut rng);
            fuzz_blur(&mut rng);
            fuzz_frost(&mut rng);
            fuzz_scan(&mut rng);
        }
        sweeps += 1;
        let more = match deadline {
            Some(_) => tairix_fuzzseed::within_budget(deadline),
            None => sweeps < SMOKE_SWEEPS,
        };
        if !more {
            break;
        }
    }
}
