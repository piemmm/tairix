//! The compositor's chrome passes against their software definitions, bit
//! for bit: the body against `Body` laid through `Pixel::laid_over`, the
//! shadow against `shadow_row`'s blend, and the resample against
//! `Surface::resample_window_into`.

use tairix_fuzzseed::Prng;
use tairix_raster::{
    shadow_row, Body, Caster, Color, DitherRow, DropShadow, Pixel, Region, ResamplePlan,
    ResampleScratch, RoundRect, Surface, ROUND_NEAREST,
};
use tairix_shaders::chrome::body::{Layout, Strip};
use tairix_shaders::{chrome, compose, groups, MAX_PASS_RADIUS};

use super::device::{
    between, gpu, picture, pixel, premultiplied, presented, rng, run, word, words, Gpu,
};

/// `count` words of pixels from `draw`.
fn drawn(rng: &mut Prng, count: u32, draw: fn(&mut Prng) -> Pixel) -> Vec<u32> {
    (0..count).map(|_| word(draw(rng))).collect()
}

/// A window to lay: its layout, its client's pixels and its furniture's,
/// decorated or plain, cut or square, presented short or whole, somewhere
/// on or partly off a `frame`-sized frame.
fn window(rng: &mut Prng, frame: (u32, u32)) -> (Layout, Vec<u32>, Vec<u32>) {
    let (outer_width, outer_height) = (between(rng, 4, 36), between(rng, 4, 30));
    let signed = |value: u32| i32::try_from(value).expect("small");
    let origin = (
        signed(between(rng, 0, frame.0 + 16)) - 12,
        signed(between(rng, 0, frame.1 + 16)) - 12,
    );
    let radius = between(rng, 0, 9);
    let (client, cut, furniture, strips) = if rng.below(2) == 0 {
        let insets = [(); 4].map(|()| between(rng, 1, 3));
        let [left, top, right, bottom] = insets;
        let rim = between(rng, 0, *insets.iter().min().expect("four insets"));
        let client = Strip {
            x: left,
            y: top,
            width: outer_width.saturating_sub(left + right).max(1),
            height: outer_height.saturating_sub(top + bottom).max(1),
            base: 0,
            stride: 0,
        };
        let cut = RoundRect {
            width: outer_width.saturating_sub(2 * rim),
            height: outer_height.saturating_sub(2 * rim),
            radius: radius.saturating_sub(rim),
        };
        let deep = |inset: u32| inset.max(radius).min(outer_height / 2);
        let (top_depth, bottom_depth) = (deep(top), deep(bottom));
        let sides = outer_height.saturating_sub(top_depth + bottom_depth);
        let right_edge = client.x + client.width;
        let mut base = 0;
        let mut strip = |x, y, width, height| {
            let laid = Strip {
                x,
                y,
                width,
                height,
                base,
                stride: width,
            };
            base += width * height;
            laid
        };
        let strips = [
            strip(0, 0, outer_width, top_depth),
            strip(0, outer_height - bottom_depth, outer_width, bottom_depth),
            strip(0, top_depth, left, sides),
            strip(right_edge, top_depth, right, sides),
        ];
        let pixels = drawn(rng, base, premultiplied);
        (
            client,
            Some((cut, (left.saturating_sub(rim), top.saturating_sub(rim)))),
            pixels,
            strips,
        )
    } else {
        let client = Strip {
            x: 0,
            y: 0,
            width: outer_width,
            height: outer_height,
            base: 0,
            stride: 0,
        };
        let cut = RoundRect {
            width: outer_width,
            height: outer_height,
            radius,
        };
        let cut = (rng.below(3) > 0).then_some((cut, (0, 0)));
        (client, cut, Vec::new(), [Strip::default(); 4])
    };
    let stride = client.width + between(rng, 0, 2);
    let client = Strip {
        base: between(rng, 0, 3),
        stride,
        ..client
    };
    let drawn_extent = if rng.below(2) == 0 {
        (client.width, client.height)
    } else {
        (
            between(rng, 0, client.width),
            between(rng, 0, client.height),
        )
    };
    let pixels = drawn(rng, client.base + stride * client.height, presented);
    let layout = Layout {
        origin,
        opacity: rng.edge_biased_u8(),
        client,
        drawn: drawn_extent,
        backdrop: (rng.below(2) == 0).then(|| premultiplied(rng)),
        cut,
        furniture: strips,
    };
    (layout, pixels, furniture)
}

/// What `layout` holds at frame pixel (`x`, `y`), resolved as the body pass
/// states its layout.
fn held(layout: &Layout, (client, furniture): (&[u32], &[u32]), (x, y): (u32, u32)) -> Body {
    let lx = x.wrapping_sub(layout.origin.0.cast_unsigned());
    let ly = y.wrapping_sub(layout.origin.1.cast_unsigned());
    let at = |words: &[u32], strip: &Strip, (sx, sy): (u32, u32)| {
        pixel(words[usize::try_from(strip.base + sy * strip.stride + sx).expect("small")])
    };
    let owned = &layout.client;
    let (cx, cy) = (lx.wrapping_sub(owned.x), ly.wrapping_sub(owned.y));
    let in_client = cx < owned.width && cy < owned.height;
    let client = in_client.then(|| {
        let reached = cx < layout.drawn.0 && cy < layout.drawn.1;
        let own = reached.then(|| at(client, owned, (cx, cy)));
        let coverage = layout
            .cut
            .map_or(255, |(shape, (ox, oy))| shape.coverage(cx + ox, cy + oy));
        (own.or(layout.backdrop), coverage)
    });
    let furniture = layout.furniture.iter().find_map(|strip| {
        let (fx, fy) = (lx.wrapping_sub(strip.x), ly.wrapping_sub(strip.y));
        (fx < strip.width && fy < strip.height).then(|| at(furniture, strip, (fx, fy)))
    });
    Body { client, furniture }
}

/// The rounding bias of frame pixel (`x`, `y`).
fn bias(dithered: bool, (x, y): (u32, u32)) -> u32 {
    if dithered {
        DitherRow::at(y).bias(x)
    } else {
        ROUND_NEAREST
    }
}

/// `layout` laid over the whole of `frame` by the pass, and by its
/// definition.
fn laid_both_ways(
    gpu: &Gpu,
    frame: &Surface,
    (layout, client, furniture): (&Layout, &[u32], &[u32]),
    dithered: bool,
) -> (Vec<u32>, Vec<u32>) {
    let (width, height) = (frame.width(), frame.height());
    let push = chrome::Push {
        stride: width,
        left: 0,
        top: 0,
        width,
        height,
        dithered,
    };
    let described = layout.words().expect("a bounded layout");
    let target = words(frame);
    let ran = run(
        gpu,
        chrome::body::SPIRV,
        &push.words(),
        &[&target, client, furniture, &described],
        groups(width, height),
    );
    let mut expected = target;
    for y in 0..height {
        for x in 0..width {
            let at = usize::try_from(y * width + x).expect("small");
            if let Some((source, factor)) =
                held(layout, (client, furniture), (x, y)).laid(layout.opacity)
            {
                let under = pixel(expected[at]);
                expected[at] = word(source.laid_over(under, factor, bias(dithered, (x, y))));
            }
        }
    }
    (ran.into_iter().next().expect("the frame"), expected)
}

#[test]
fn the_body_pass_lays_what_each_window_holds_to_the_bit() {
    let gpu = gpu();
    let mut rng = rng("the_body_pass_lays_what_each_window_holds_to_the_bit");
    let frame = (40, 32);
    for case in 0..24 {
        let under = picture(&mut rng, frame.0, frame.1);
        let (layout, client, furniture) = window(&mut rng, frame);
        let dithered = rng.below(2) == 0;
        let (ran, expected) =
            laid_both_ways(&gpu, &under, (&layout, &client, &furniture), dithered);
        for (at, (ran, expected)) in ran.iter().zip(&expected).enumerate() {
            let (x, y) = (at % 40, at / 40);
            assert_eq!(
                pixel(*ran),
                pixel(*expected),
                "case {case} at ({x}, {y}), dithered {dithered}: {layout:?}"
            );
        }
    }
}

/// Exact at the largest radius a pass takes, where the scaled radius' square
/// and the sum of two near it fill most of a word: clients laid across the
/// arc of an 8190-pixel cut and at its corners, and two laid far past its
/// edges, whose distances would leave a word were they not held.
#[test]
fn a_cut_at_the_largest_radius_is_exact() {
    let gpu = gpu();
    let mut rng = rng("a_cut_at_the_largest_radius_is_exact");
    let side = 2 * MAX_PASS_RADIUS;
    for at in [
        (1190, 1190),
        (0, 0),
        (4, 4090),
        (side - 24, 8),
        (12_000, 8),
        (8, 12_000),
    ] {
        let under = picture(&mut rng, 24, 24);
        let client = Strip {
            x: 0,
            y: 0,
            width: 24,
            height: 24,
            base: 0,
            stride: 24,
        };
        let pixels = drawn(&mut rng, 24 * 24, premultiplied);
        let layout = Layout {
            origin: (0, 0),
            opacity: 255,
            client,
            drawn: (24, 24),
            backdrop: None,
            cut: Some((
                RoundRect {
                    width: side,
                    height: side,
                    radius: MAX_PASS_RADIUS,
                },
                at,
            )),
            furniture: [Strip::default(); 4],
        };
        let (ran, expected) = laid_both_ways(&gpu, &under, (&layout, &pixels, &[]), true);
        assert_eq!(ran, expected, "a client at {at:?} of the cut");
    }
}

/// A shadow of `reach` in a dark colour of `alpha`, holding a tile for
/// `radius` unless `tiled` is false.
fn level(reach: u32, alpha: u8, radius: u32, tiled: bool) -> DropShadow {
    let mut shadow = DropShadow::new(reach, Color::rgba(8, 10, 14, alpha));
    if tiled {
        shadow.ensure_tile(radius);
    }
    shadow
}

#[test]
fn the_shadow_pass_lays_what_the_shadow_row_lays_to_the_bit() {
    let gpu = gpu();
    let mut rng = rng("the_shadow_pass_lays_what_the_shadow_row_lays_to_the_bit");
    let (width, height) = (48u32, 44u32);
    let mut cast = 0;
    for case in 0..24 {
        let signed = |value: u32| i32::try_from(value).expect("small");
        let size = (between(&mut rng, 1, 30), between(&mut rng, 1, 24));
        let from = signed(between(&mut rng, 0, width + 10)) - 16;
        let top = signed(between(&mut rng, 0, height)) - 10;
        let radius = between(&mut rng, 0, 9);
        let silhouette = (rng.below(3) > 0).then_some(RoundRect {
            width: size.0,
            height: size.1,
            radius,
        });
        let caster = Caster {
            left: from,
            top,
            right: from + signed(size.0),
            bottom: top + signed(size.1),
            silhouette,
            opacity: rng.edge_biased_u8(),
        };
        let corners = silhouette.map_or(0, RoundRect::corner_reach);
        let tiled = rng.below(5) > 0;
        let lifted = level(between(&mut rng, 0, 12), rng.next_u8(), corners, tiled);
        let resting = level(between(&mut rng, 0, 7), rng.next_u8(), corners, tiled);
        let between_levels = rng.next_u8();
        let lift = *rng.pick(&[0, 255, between_levels]);
        let dithered = rng.below(2) == 0;
        let under = picture(&mut rng, width, height);
        let tables = chrome::shadow::Tables::new(&lifted, &resting).expect("tables");
        let frame = chrome::Push {
            stride: width,
            left: 0,
            top: 0,
            width,
            height,
            dithered,
        };
        let push = tables
            .push(&frame, &caster, lift)
            .expect("a bounded caster");
        let target = words(&under);
        let ran = run(
            &gpu,
            chrome::shadow::SPIRV,
            &push,
            &[&target, tables.words()],
            groups(width, height),
        );
        let mut expected = under.clone();
        for y in 0..height {
            let Some(row) = shadow_row(&lifted, &resting, &caster, lift, signed(y)) else {
                continue;
            };
            let dither = if dithered {
                DitherRow::at(y)
            } else {
                DitherRow::NEAREST
            };
            let start = usize::try_from(y * width).expect("small");
            let pixels = &mut expected.pixels_mut()[start..start + width as usize];
            cast += row.blend_into(pixels, 0, dither);
        }
        for (at, (ran, expected)) in ran[0].iter().zip(words(&expected)).enumerate() {
            let (x, y) = (at % 48, at / 48);
            assert_eq!(
                pixel(*ran),
                pixel(expected),
                "case {case} at ({x}, {y}), lift {lift}, dithered {dithered}: {caster:?}"
            );
        }
    }
    assert!(cast > 0, "no case cast a shadow on the frame");
}

#[test]
fn the_resample_pass_filters_each_window_as_the_resample_does_to_the_bit() {
    let gpu = gpu();
    let mut rng = rng("the_resample_pass_filters_each_window_as_the_resample_does_to_the_bit");
    let marker = Pixel {
        r: 1,
        g: 2,
        b: 3,
        a: 255,
    };
    for ((sw, sh), (dw, dh)) in [
        ((16, 12), (40, 30)),
        ((40, 30), (16, 12)),
        ((40, 30), (17, 45)),
        ((23, 19), (23, 19)),
        ((23, 19), (23, 31)),
        ((5, 3), (64, 2)),
    ] {
        let mut source = Surface::new(sw, sh).expect("a surface");
        for slot in source.pixels_mut() {
            *slot = presented(&mut rng);
        }
        for window in [
            Region {
                x: 0,
                y: 0,
                width: dw,
                height: dh,
            },
            Region {
                x: between(&mut rng, 0, dw / 2),
                y: between(&mut rng, 0, dh / 2),
                width: between(&mut rng, 1, dw / 2),
                height: between(&mut rng, 1, dh / 2),
            },
        ] {
            let mut expected = Surface::filled(dw, dh, marker).expect("a surface");
            let mut scratch = [ResampleScratch::new()];
            source
                .resample_window_into(
                    &mut expected,
                    window,
                    &mut scratch,
                    &tairix_parallel::SERIAL,
                )
                .expect("resampled");
            let plan = ResamplePlan::new((sw, sh), (dw, dh), window).expect("a plan");
            let push = compose::resample::Push::new(&plan, window, (dw, sw)).expect("countable");
            let taps = compose::resample::taps(&plan).expect("taps");
            let target = vec![word(marker); usize::try_from(dw * dh).expect("small")];
            let ran = run(
                &gpu,
                compose::resample::SPIRV,
                &push.words(),
                &[&target, &words(&source), &taps],
                groups(window.width, window.height),
            );
            assert_eq!(
                ran[0],
                words(&expected),
                "{sw}x{sh} onto {dw}x{dh}, {window:?}"
            );
        }
    }
}
