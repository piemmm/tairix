use alloc::vec::Vec;

use tairix_mix::{numbered, SplitMix64};
use tairix_raster::{Color, Pixel};

use super::{gaussian_radii, patterned, pixels, Distribution, Effect, Noise, Window};
use crate::canvas::{Canvas, CanvasBuilder, Kind, Sample};
use crate::shape::Bounds;

/// What `effect` makes of the whole of `canvas`, read in one window.
fn made(canvas: &Canvas, effect: Effect) -> Vec<[u8; 4]> {
    let whole = Bounds::picture(canvas.width(), canvas.height());
    let window = Window::read(canvas, whole).expect("room");
    effect.run(&window, whole).expect("room")
}

/// Pixel `(x, y)` of `canvas`, a place past the edge read as the edge.
fn clamped(canvas: &Canvas, x: i64, y: i64) -> [u8; 4] {
    let column = u32::try_from(x.clamp(0, i64::from(canvas.width()) - 1)).expect("a column");
    let row = u32::try_from(y.clamp(0, i64::from(canvas.height()) - 1)).expect("a row");
    canvas.colour_at(column, row).expect("on the picture")
}

/// A `width` × `height` picture of colour `left` before column `edge` and
/// `right` from it.
fn split(width: u32, height: u32, edge: u32, (left, right): ([u8; 4], [u8; 4])) -> Canvas {
    let mut built =
        CanvasBuilder::new(width, height, Kind::Rgba, Sample::Rgba(left)).expect("fits");
    for y in 0..height {
        for x in edge..width {
            built.set(x, y, Sample::Rgba(right));
        }
    }
    built.finish()
}

/// The blur by its definition: each pass a box of its radius over sixteen-bit
/// premultiplied pixels, across and then down, the picture's edges replicated
/// and each mean rounded to the nearest — no running sums and no reciprocal.
fn naive_blur(canvas: &Canvas, radii: [usize; 3]) -> Vec<[u8; 4]> {
    let (width, height) = (i64::from(canvas.width()), i64::from(canvas.height()));
    let index = |x: i64, y: i64| {
        let column = x.clamp(0, width - 1);
        let row = y.clamp(0, height - 1);
        usize::try_from(row * width + column).expect("a place")
    };
    let mut field: Vec<[u64; 4]> = pixels(canvas)
        .into_iter()
        .map(|[r, g, b, a]| {
            let a = u64::from(a);
            [
                u64::from(r) * a,
                u64::from(g) * a,
                u64::from(b) * a,
                a * 255,
            ]
        })
        .collect();
    for radius in radii {
        let reach = i64::try_from(radius).expect("a radius");
        let count = u64::try_from(2 * radius + 1).expect("a count");
        for across in [true, false] {
            let mut next = field.clone();
            for y in 0..height {
                for x in 0..width {
                    let mut sums = [0u64; 4];
                    for step in -reach..=reach {
                        let (column, row) = if across { (x + step, y) } else { (x, y + step) };
                        for (sum, value) in sums.iter_mut().zip(field[index(column, row)]) {
                            *sum += value;
                        }
                    }
                    next[index(x, y)] = sums.map(|sum| (sum + count / 2) / count);
                }
            }
            field = next;
        }
    }
    field
        .into_iter()
        .map(|[r, g, b, a]| {
            let level = (a + 127) / 255;
            if level == 0 {
                return [0; 4];
            }
            let channel = |value: u64| u8::try_from(((value * 255 + a / 2) / a).min(255));
            let channels = [channel(r), channel(g), channel(b), u8::try_from(level)];
            channels.map(|level| level.expect("a level"))
        })
        .collect()
}

/// The median by its definition: each premultiplied channel's middle level of
/// the square about the pixel, the picture's edges replicated, sorted afresh.
fn naive_median(canvas: &Canvas, radius: i64) -> Vec<[u8; 4]> {
    let mut out = Vec::new();
    for y in 0..i64::from(canvas.height()) {
        for x in 0..i64::from(canvas.width()) {
            let mut channels: [Vec<u8>; 4] = Default::default();
            for down in -radius..=radius {
                for across in -radius..=radius {
                    let [r, g, b, a] = clamped(canvas, x + across, y + down);
                    let pixel = Color::rgba(r, g, b, a).premultiply();
                    let levels = [pixel.r, pixel.g, pixel.b, pixel.a];
                    for (channel, level) in channels.iter_mut().zip(levels) {
                        channel.push(level);
                    }
                }
            }
            let [r, g, b, a] = channels.map(|mut levels| {
                levels.sort_unstable();
                levels[levels.len() / 2]
            });
            let colour = Pixel { r, g, b, a }.unpremultiply();
            out.push([colour.r, colour.g, colour.b, colour.a]);
        }
    }
    out
}

#[test]
fn three_boxes_match_the_gaussian_they_stand_for() {
    assert_eq!(gaussian_radii(0), [0, 0, 0]);
    assert_eq!(gaussian_radii(1), [0, 0, 1]);
    assert_eq!(gaussian_radii(2), [1, 1, 2]);
    assert_eq!(gaussian_radii(10), [9, 9, 10]);
    assert_eq!(gaussian_radii(-3), [0, 0, 0]);
    assert_eq!(
        gaussian_radii(1_000),
        gaussian_radii(50),
        "held to the most"
    );
    for sigma in 1..=50 {
        let twelve_variances: i64 = gaussian_radii(sigma)
            .iter()
            .map(|&radius| {
                let width = i64::try_from(2 * radius + 1).expect("a width");
                width * width - 1
            })
            .sum();
        let wanted = 12 * i64::from(sigma) * i64::from(sigma);
        assert!(
            (twelve_variances - wanted).abs() <= 4 * i64::from(sigma),
            "σ {sigma}: twelve variances {twelve_variances} for {wanted}"
        );
    }
}

#[test]
fn a_gaussian_blur_is_its_three_box_passes_at_sixteen_bits() {
    let canvas = patterned(37, 29);
    for sigma in [1, 3, 7] {
        let expected = naive_blur(&canvas, gaussian_radii(sigma));
        assert_eq!(
            made(&canvas, Effect::gaussian(sigma)),
            expected,
            "σ {sigma}"
        );
    }
    let flat = split(20, 20, 20, ([90, 180, 30, 200], [0; 4]));
    assert_eq!(
        made(&flat, Effect::gaussian(4)),
        pixels(&flat),
        "its own blur"
    );
    assert_eq!(made(&canvas, Effect::gaussian(0)), pixels(&canvas));
}

#[test]
fn an_unsharp_mask_pushes_each_channel_away_from_its_blur() {
    let canvas = patterned(31, 23);
    let blurred = naive_blur(&canvas, gaussian_radii(2));
    let sharpen = |colour: [u8; 4], mean: [u8; 4]| {
        if colour[3] == 0 {
            return colour;
        }
        let mut pushed = colour;
        for (level, mean) in pushed.iter_mut().zip(mean).take(3) {
            let difference = i32::from(*level) - i32::from(mean);
            if difference.abs() >= 6 {
                let moved = i32::from(*level) + (difference * 150 + 50).div_euclid(100);
                *level = u8::try_from(moved.clamp(0, 255)).expect("a level");
            }
        }
        pushed
    };
    let expected: Vec<[u8; 4]> = pixels(&canvas)
        .into_iter()
        .zip(blurred)
        .map(|(colour, mean)| sharpen(colour, mean))
        .collect();
    assert_eq!(made(&canvas, Effect::unsharp(150, 2, 6)), expected);
    assert_eq!(
        made(&canvas, Effect::unsharp(0, 2, 0)),
        pixels(&canvas),
        "no amount"
    );
}

#[test]
fn the_median_is_the_middle_of_each_channel_about_the_pixel_at_any_radius() {
    let canvas = patterned(23, 19);
    for radius in [1, 2, 5] {
        let expected = naive_median(&canvas, i64::from(radius));
        assert_eq!(
            made(&canvas, Effect::median(radius)),
            expected,
            "radius {radius}"
        );
    }
    assert_eq!(made(&canvas, Effect::median(0)), pixels(&canvas));
}

#[test]
fn find_edges_lights_each_channel_s_sobel_gradient() {
    let canvas = split(4, 3, 2, ([0, 0, 0, 255], [255, 120, 0, 255]));
    let edges = made(&canvas, Effect::Edges);
    // A step across columns 1 and 2: four times the step, held to a level.
    let lit = [255, 255, 0, 255];
    let dark = [0, 0, 0, 255];
    for row in edges.as_chunks::<4>().0 {
        assert_eq!(*row, [dark, lit, lit, dark]);
    }
}

#[test]
fn emboss_lifts_the_face_turned_to_the_light_from_mid_grey() {
    let flat = split(5, 5, 5, ([200, 10, 10, 255], [0; 4]));
    let relief = made(&flat, Effect::emboss(135, 2, 100));
    assert!(relief.iter().all(|&pixel| pixel == [128, 128, 128, 255]));
    let step = split(6, 3, 3, ([0, 0, 0, 255], [255, 255, 255, 255]));
    let from_left = made(&step, Effect::emboss(180, 1, 100));
    assert_eq!(from_left[0], [128, 128, 128, 255]);
    assert_eq!(
        from_left[2],
        [255, 255, 255, 255],
        "the rise faces the light"
    );
    let from_right = made(&step, Effect::emboss(0, 1, 100));
    assert_eq!(from_right[2], [1, 1, 1, 255], "and turns from it");
}

#[test]
fn pixelate_lays_squares_on_the_picture_s_corner_each_the_mean_of_what_it_covers() {
    let canvas = patterned(13, 10);
    let cell = 4i64;
    let mut expected = Vec::new();
    for y in 0..10i64 {
        for x in 0..13i64 {
            let (left, top) = (x / cell * cell, y / cell * cell);
            let mut sum = [0u64; 4];
            let mut count = 0u64;
            for row in top..(top + cell).min(10) {
                for column in left..(left + cell).min(13) {
                    let [r, g, b, a] = clamped(&canvas, column, row);
                    let weight = u64::from(a);
                    sum[0] += u64::from(r) * weight;
                    sum[1] += u64::from(g) * weight;
                    sum[2] += u64::from(b) * weight;
                    sum[3] += weight;
                    count += 1;
                }
            }
            let mean = |total: u64| u8::try_from((total + sum[3] / 2) / sum[3]).expect("a level");
            let alpha = u8::try_from((sum[3] + count / 2) / count).expect("a level");
            expected.push([mean(sum[0]), mean(sum[1]), mean(sum[2]), alpha]);
        }
    }
    assert_eq!(made(&canvas, Effect::pixelate(4)), expected);
    assert_eq!(made(&canvas, Effect::pixelate(1)), pixels(&canvas));
}

#[test]
fn noise_is_drawn_from_the_seed_and_the_place_alone_in_integers() {
    let noise = Noise {
        amount: 50,
        ..Noise::START
    };
    let mut draws = SplitMix64::new(numbered(noise.seed, (9 << 32) | 3));
    let expected: [i32; 3] = core::array::from_fn(|_| {
        i32::try_from(((draws.next_u64() >> 48) * 129) >> 16).expect("an offset") - 64
    });
    assert_eq!(noise.offsets(3, 9), expected, "the recipe, written out");
    let row: Vec<[i32; 3]> = (0..16).map(|x| noise.offsets(x, 0)).collect();
    assert!(
        row.iter().any(|offsets| *offsets != row[0]),
        "each place its own"
    );
    let grey = Noise {
        monochrome: true,
        ..noise
    };
    let [r, g, b] = grey.offsets(5, 5);
    assert!(r == g && g == b, "grey speckle moves the channels alike");
    let clear = [10, 20, 30, 0];
    assert_eq!(
        noise.speckled(1, 1, clear),
        clear,
        "a clear pixel keeps its colour"
    );
    let quiet = Noise { amount: 0, ..noise };
    assert_eq!(quiet.speckled(1, 1, [10, 20, 30, 255]), [10, 20, 30, 255]);
}

#[test]
fn gaussian_noise_bunches_about_none_and_uniform_noise_spreads_to_its_reach() {
    let uniform = Noise {
        amount: 100,
        ..Noise::START
    };
    let gaussian = Noise {
        distribution: Distribution::Gaussian,
        ..uniform
    };
    let (mut sum, mut squares, mut widest) = (0i64, 0i64, 0i32);
    for y in 0..64 {
        for x in 0..64 {
            for offset in uniform.offsets(x, y) {
                assert!(offset.abs() <= 128, "within its reach");
                widest = widest.max(offset.abs());
            }
            for offset in gaussian.offsets(x, y) {
                sum += i64::from(offset);
                squares += i64::from(offset) * i64::from(offset);
            }
        }
    }
    let count = 64 * 64 * 3;
    assert!(widest >= 120, "uniform offsets reach their edge: {widest}");
    assert!((sum / count).abs() <= 2, "bunched about none: {sum}");
    let spread = squares / count;
    assert!(
        (55 * 55..=73 * 73).contains(&spread),
        "a spread near 64: {spread}"
    );
}

#[test]
fn an_effect_reads_as_far_as_it_reaches() {
    assert_eq!(Effect::gaussian(10).reach(), 28);
    assert_eq!(Effect::unsharp(100, 2, 0).reach(), 4);
    assert_eq!(Effect::median(3).reach(), 3);
    assert_eq!(Effect::Noise(Noise::START).reach(), 0);
    assert_eq!(Effect::Edges.reach(), 1);
    assert_eq!(Effect::emboss(45, 3, 100).reach(), 2);
    assert_eq!(Effect::pixelate(8).reach(), 7);
    assert_eq!(Effect::pixelate(-4).reach(), 0);
}
