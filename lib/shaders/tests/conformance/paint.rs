//! The furniture painter's passes against the `Surface` calls they are, bit
//! for bit: plates, washes, rings, shapes covered and laid, glyph masks, and
//! pictures rounded and toned.

use tairix_fuzzseed::Prng;
use tairix_raster::{
    cover_span, stroke_quads, Color, Coverage, DitherRow, FillRule, Pixel, Ring, RingInk,
    ScanScratch, Surface, SUBPIXEL,
};
use tairix_shaders::groups;
use tairix_shaders::paint::{self, coverage, mask, picture, plate, ring, wash, Push};

use super::device::{between, gpu, picture as drawn_picture, pixel, rng, run, words, Gpu};

/// The width and height every case paints a surface of.
const SIDE: (u32, u32) = (48, 40);

/// A colour whose alpha is often at an extreme.
fn colour(rng: &mut Prng) -> Color {
    Color::rgba(
        rng.next_u8(),
        rng.next_u8(),
        rng.next_u8(),
        rng.edge_biased_u8(),
    )
}

/// The push over the rectangle `rect` of a `SIDE` surface.
fn over((left, top, width, height): (u32, u32, u32, u32), dithered: bool) -> Push {
    Push {
        stride: SIDE.0,
        left,
        top,
        width,
        height,
        dithered,
    }
}

/// `rect` cut to the `SIDE` surface, or `None` where nothing of it is left.
fn cut((x, y, w, h): (u32, u32, u32, u32)) -> Option<(u32, u32, u32, u32)> {
    let right = (x + w).min(SIDE.0);
    let bottom = (y + h).min(SIDE.1);
    (x < right && y < bottom).then(|| (x, y, right - x, bottom - y))
}

/// A rectangle partly off the `SIDE` surface now and then.
fn rect(rng: &mut Prng) -> (u32, u32, u32, u32) {
    (
        between(rng, 0, SIDE.0 - 2),
        between(rng, 0, SIDE.1 - 2),
        between(rng, 1, 40),
        between(rng, 1, 34),
    )
}

/// Run `spirv` with `push` over `buffers`, the surface the first, across
/// `rect`: the surface it leaves.
fn painted(
    gpu: &Gpu,
    spirv: &[u8],
    push: &[u32],
    buffers: &[&[u32]],
    (width, height): (u32, u32),
) -> Vec<u32> {
    run(gpu, spirv, push, buffers, groups(width, height)).swap_remove(0)
}

/// Assert `ran` and `expected` are the same pixels, naming `what` and the
/// first that differs.
fn same(ran: &[u32], expected: &Surface, what: &str) {
    for (at, (ran, expected)) in ran.iter().zip(words(expected)).enumerate() {
        let (x, y) = (at % SIDE.0 as usize, at / SIDE.0 as usize);
        assert_eq!(pixel(*ran), pixel(expected), "{what} at ({x}, {y})");
    }
}

#[test]
fn the_plate_pass_fills_and_lays_rounded_rectangles_to_the_bit() {
    let gpu = gpu();
    let mut rng = rng("the_plate_pass_fills_and_lays_rounded_rectangles_to_the_bit");
    for case in 0..32 {
        let under = drawn_picture(&mut rng, SIDE.0, SIDE.1);
        let (x, y, width, height) = rect(&mut rng);
        let laid = plate::Plate {
            x,
            y,
            width,
            height,
            radius: between(&mut rng, 0, 12),
            color: colour(&mut rng),
            replace: rng.below(2) == 0,
        };
        let Some(area) = cut((x, y, width, height)) else {
            continue;
        };
        let mut expected = under.clone();
        if laid.replace {
            expected.set_round_rect(x, y, width, height, laid.radius, laid.color);
        } else {
            expected.fill_round_rect(x, y, width, height, laid.radius, laid.color);
        }
        let push = laid.push(&over(area, true)).expect("a bounded plate");
        let ran = painted(
            &gpu,
            plate::SPIRV,
            &push,
            &[&words(&under)],
            (area.2, area.3),
        );
        same(&ran, &expected, &format!("case {case}: {laid:?}"));
    }
}

/// A plane of `count` random weights, often at an extreme.
fn plane(rng: &mut Prng, count: u32) -> Vec<u32> {
    (0..count)
        .map(|_| u32::from(rng.edge_biased_u8()))
        .collect()
}

#[test]
fn the_wash_pass_washes_gradients_and_masks_to_the_bit() {
    let gpu = gpu();
    let mut rng = rng("the_wash_pass_washes_gradients_and_masks_to_the_bit");
    for case in 0..32 {
        let under = drawn_picture(&mut rng, SIDE.0, SIDE.1);
        let (x, y, width, height) = rect(&mut rng);
        let Some(area) = cut((x, y, width, height)) else {
            continue;
        };
        let masked = rng.below(2) == 0;
        let (top, bottom) = if masked {
            let flat = colour(&mut rng);
            (flat, flat)
        } else {
            (colour(&mut rng), colour(&mut rng))
        };
        let weights = plane(&mut rng, width * height);
        let mut expected = under.clone();
        if masked {
            expected.wash_region(x, y, width, height, top, |row, column, out| {
                for (slot, at) in out.iter_mut().zip(column..) {
                    *slot = u8::try_from(weights[(row * width + at) as usize]).expect("a byte");
                }
            });
        } else {
            expected.fill_vertical_gradient(x, y, width, height, top, bottom);
        }
        let laid = wash::Wash {
            x,
            y,
            width,
            height,
            top,
            bottom,
            planes: [
                masked.then_some(wash::Plane {
                    base: 0,
                    stride: width,
                }),
                None,
            ],
        };
        let push = laid.push(&over(area, true)).expect("a bounded wash");
        let ran = painted(
            &gpu,
            wash::SPIRV,
            &push,
            &[&words(&under), &weights],
            (area.2, area.3),
        );
        same(&ran, &expected, &format!("case {case}, masked {masked}"));
    }
}

/// A closed polygon in `SUBPIXEL` units: a star or a blob of `points`
/// vertices about a centre that may lie near or past the surface's edges.
fn polygon(rng: &mut Prng) -> Vec<(i32, i32)> {
    let unit = SUBPIXEL;
    let signed = |value: u32| i32::try_from(value).expect("small");
    let centre = (
        signed(between(rng, 0, SIDE.0 + 8)) * unit - 4 * unit,
        signed(between(rng, 0, SIDE.1 + 8)) * unit - 4 * unit,
    );
    let points = between(rng, 3, 9);
    (0..points)
        .map(|_| {
            (
                centre.0 + signed(between(rng, 0, 40 * 8)) - 20 * unit,
                centre.1 + signed(between(rng, 0, 36 * 8)) - 18 * unit,
            )
        })
        .collect()
}

/// A window of the surface, `(x, y, width, height)`, and the plane covering
/// it.
type Covered = ((u32, u32, u32, u32), Vec<u32>);

/// The shape `rows` covers, by the coverage pass, over the window the
/// converter bounds it to on the `SIDE` surface.
fn covered(gpu: &Gpu, rows: &tairix_raster::CoverageRows<'_>) -> Option<Covered> {
    let bounds = rows.bounds((0, 0, SIDE.0, SIDE.1))?;
    let shape = coverage::Shape::new(rows, bounds).expect("a bounded shape");
    let (x, y, width, height) = shape.window();
    let plane = vec![0; (width * height) as usize];
    let ran = run(
        gpu,
        coverage::SPIRV,
        &shape.push(),
        &[&plane, shape.words()],
        groups(width, height),
    );
    Some((
        (x, y, width, height),
        ran.into_iter().next().expect("the plane"),
    ))
}

#[test]
fn the_coverage_pass_covers_each_pixel_as_the_converter_does() {
    let gpu = gpu();
    let mut rng = rng("the_coverage_pass_covers_each_pixel_as_the_converter_does");
    let mut scratch = ScanScratch::new();
    for case in 0..48 {
        let contours = [polygon(&mut rng), polygon(&mut rng)];
        let contours = &contours[..=rng.below(2)];
        let rule = if rng.below(2) == 0 {
            FillRule::EvenOdd
        } else {
            FillRule::NonZero
        };
        let Some(mut rows) =
            scratch.coverage_rows(contours, SUBPIXEL.unsigned_abs(), rule, Coverage::Area)
        else {
            continue;
        };
        let Some(((x, y, width, height), plane)) = covered(&gpu, &rows) else {
            continue;
        };
        let mut row = vec![0u8; width as usize];
        for line in 0..height {
            rows.row(y + line, x, &mut row);
            let ran: Vec<u8> = plane[(line * width) as usize..((line + 1) * width) as usize]
                .iter()
                .map(|&word| u8::try_from(word).expect("a byte"))
                .collect();
            assert_eq!(ran, row, "case {case}, {rule:?}, row {}", y + line);
        }
    }
}

/// Lay `ink` through the plane covering `window` over `surface`, as the mask
/// pass does: what it leaves.
fn laid_through(
    gpu: &Gpu,
    surface: &[u32],
    (window, plane): ((u32, u32, u32, u32), &[u32]),
    ink: Color,
) -> Vec<u32> {
    let (x, y, width, height) = window;
    let laid = mask::Mask {
        x: i32::try_from(x).expect("small"),
        y: i32::try_from(y).expect("small"),
        width,
        height,
        base: 0,
        stride: width,
        ink,
    };
    let push = laid
        .push(&over((x, y, width, height), false))
        .expect("a bounded mask");
    painted(gpu, mask::SPIRV, &push, &[surface, plane], (width, height))
}

#[test]
fn a_covered_shape_laid_through_its_plane_is_the_fill_and_the_stroke() {
    let gpu = gpu();
    let mut rng = rng("a_covered_shape_laid_through_its_plane_is_the_fill_and_the_stroke");
    let mut scratch = ScanScratch::new();
    for case in 0..24 {
        let under = drawn_picture(&mut rng, SIDE.0, SIDE.1);
        let ink = colour(&mut rng);
        let shape = polygon(&mut rng);
        // The fill: one shape, even-odd, laid where it falls.
        let mut expected = under.clone();
        expected.fill_polygon_subpixel(&shape, ink);
        let mut ran = words(&under);
        if let Some(rows) = scratch.coverage_rows(
            &[&shape],
            SUBPIXEL.unsigned_abs(),
            FillRule::EvenOdd,
            Coverage::Area,
        ) {
            if let Some(covered) = covered(&gpu, &rows) {
                ran = laid_through(&gpu, &ran, (covered.0, &covered.1), ink);
            }
        }
        same(&ran, &expected, &format!("case {case}: the fill"));
        // The stroke: each of its quads covered and laid in turn.
        let weight = i32::try_from(between(&mut rng, 1, 24)).expect("small");
        let mut expected = under.clone();
        expected.stroke_polyline(&shape, weight, ink);
        let mut ran = words(&under);
        for quad in stroke_quads(&shape, weight) {
            let Some(rows) = scratch.coverage_rows(
                &[quad],
                SUBPIXEL.unsigned_abs(),
                FillRule::EvenOdd,
                Coverage::Area,
            ) else {
                continue;
            };
            if let Some(covered) = covered(&gpu, &rows) {
                ran = laid_through(&gpu, &ran, (covered.0, &covered.1), ink);
            }
        }
        same(
            &ran,
            &expected,
            &format!("case {case}: the stroke, {weight} wide"),
        );
        // The design-space fill, stretched across the whole surface.
        let design = between(&mut rng, 64, 512);
        let drawn: Vec<(i32, i32)> = shape
            .iter()
            .map(|&(x, y)| (x.rem_euclid(512), y.rem_euclid(512)))
            .collect();
        let mut expected = under.clone();
        expected.fill_polygon(&drawn, design, ink);
        let mut ran = words(&under);
        if let Some(rows) =
            scratch.design_rows(&[&drawn], design, (0, 0, SIDE.0, SIDE.1), FillRule::EvenOdd)
        {
            if let Some(covered) = covered(&gpu, &rows) {
                ran = laid_through(&gpu, &ran, (covered.0, &covered.1), ink);
            }
        }
        same(
            &ran,
            &expected,
            &format!("case {case}: the design fill at {design}"),
        );
    }
}

#[test]
fn a_washed_shape_is_its_coverage_and_its_mask_washed_together() {
    let gpu = gpu();
    let mut rng = rng("a_washed_shape_is_its_coverage_and_its_mask_washed_together");
    let mut scratch = ScanScratch::new();
    for case in 0..24 {
        let under = drawn_picture(&mut rng, SIDE.0, SIDE.1);
        let ink = colour(&mut rng);
        let shape = polygon(&mut rng);
        let field: Vec<u8> = (0..SIDE.0 * SIDE.1).map(|_| rng.edge_biased_u8()).collect();
        let strength = |x: u32, y: u32| field[(y * SIDE.0 + x) as usize];
        let mut expected = under.clone();
        expected.wash_polygon_subpixel(&shape, ink, |row, column, out| {
            for (slot, at) in out.iter_mut().zip(column..) {
                *slot = strength(at, row);
            }
        });
        let mut ran = words(&under);
        if let Some(rows) = scratch.coverage_rows(
            &[&shape],
            SUBPIXEL.unsigned_abs(),
            FillRule::EvenOdd,
            Coverage::Area,
        ) {
            if let Some(((x, y, width, height), plane)) = covered(&gpu, &rows) {
                let mut planes = plane;
                for row in 0..height {
                    for column in 0..width {
                        planes.push(u32::from(strength(x + column, y + row)));
                    }
                }
                let laid = wash::Wash {
                    x,
                    y,
                    width,
                    height,
                    top: ink,
                    bottom: ink,
                    planes: [
                        Some(wash::Plane {
                            base: 0,
                            stride: width,
                        }),
                        Some(wash::Plane {
                            base: width * height,
                            stride: width,
                        }),
                    ],
                };
                let push = laid
                    .push(&over((x, y, width, height), true))
                    .expect("a bounded wash");
                ran = painted(&gpu, wash::SPIRV, &push, &[&ran, &planes], (width, height));
            }
        }
        same(&ran, &expected, &format!("case {case}"));
    }
}

#[test]
fn the_mask_pass_lays_a_tinted_mask_as_the_tinted_blit_does() {
    let gpu = gpu();
    let mut rng = rng("the_mask_pass_lays_a_tinted_mask_as_the_tinted_blit_does");
    for case in 0..32 {
        let under = drawn_picture(&mut rng, SIDE.0, SIDE.1);
        let (width, height) = (between(&mut rng, 1, 30), between(&mut rng, 1, 30));
        let weights = plane(&mut rng, width * height);
        let mut tinted = Surface::new(width, height).expect("a mask");
        for (slot, &weight) in tinted.pixels_mut().iter_mut().zip(&weights) {
            *slot = Pixel {
                r: 0,
                g: 0,
                b: 0,
                a: u8::try_from(weight).expect("a byte"),
            };
        }
        let signed = |value: u32| i32::try_from(value).expect("small");
        let at = (
            signed(between(&mut rng, 0, SIDE.0 + 12)) - 12,
            signed(between(&mut rng, 0, SIDE.1 + 12)) - 12,
        );
        let ink = colour(&mut rng);
        let mut expected = under.clone();
        expected.blit_tinted(at.0, at.1, &tinted, ink);
        // A glyph lays its coverage a row at a time through `cover_span`, at
        // nearest, as `lib/font` does: the same pixels.
        let mut glyph = under.clone();
        for row in 0..height {
            let Some(y) = u32::try_from(at.1 + signed(row))
                .ok()
                .filter(|y| *y < SIDE.1)
            else {
                continue;
            };
            let first = at.0.max(0);
            let last = (at.0 + signed(width)).min(signed(SIDE.0));
            if first >= last {
                continue;
            }
            let from = usize::try_from(first - at.0).expect("inside");
            let line: Vec<u8> = weights[(row * width) as usize..((row + 1) * width) as usize]
                .iter()
                .map(|&weight| u8::try_from(weight).expect("a byte"))
                .collect();
            let start = (y * SIDE.0) as usize + usize::try_from(first).expect("on the surface");
            let span = usize::try_from(last - first).expect("a span");
            cover_span(
                &mut glyph.pixels_mut()[start..start + span],
                ink.premultiply(),
                &line[from..from + span],
                DitherRow::NEAREST,
                first.cast_unsigned(),
            );
        }
        assert_eq!(
            glyph.pixels(),
            expected.pixels(),
            "case {case}: a glyph's row is its tinted blit"
        );
        let laid = mask::Mask {
            x: at.0,
            y: at.1,
            width,
            height,
            base: 0,
            stride: width,
            ink,
        };
        let push = laid
            .push(&over((0, 0, SIDE.0, SIDE.1), false))
            .expect("bounded");
        let ran = painted(&gpu, mask::SPIRV, &push, &[&words(&under), &weights], SIDE);
        same(&ran, &expected, &format!("case {case} at {at:?}"));
    }
}

#[test]
fn the_picture_pass_lays_rounded_and_toned_pictures_as_the_blits_do() {
    let gpu = gpu();
    let mut rng = rng("the_picture_pass_lays_rounded_and_toned_pictures_as_the_blits_do");
    for case in 0..32 {
        let under = drawn_picture(&mut rng, SIDE.0, SIDE.1);
        let (width, height) = (between(&mut rng, 1, 32), between(&mut rng, 1, 28));
        let mut source = Surface::new(width, height).expect("a picture");
        for slot in source.pixels_mut() {
            *slot = super::device::presented(&mut rng);
        }
        let signed = |value: u32| i32::try_from(value).expect("small");
        let at = (
            signed(between(&mut rng, 0, SIDE.0 + 12)) - 12,
            signed(between(&mut rng, 0, SIDE.1 + 12)) - 12,
        );
        let laid = if rng.below(2) == 0 {
            picture::Laid::Rounded(between(&mut rng, 0, 12))
        } else {
            picture::Laid::Toned {
                saturation: rng.edge_biased_u8(),
                strength: rng.edge_biased_u8(),
            }
        };
        let mut expected = under.clone();
        match laid {
            picture::Laid::Rounded(radius) => expected.blit_rounded(at.0, at.1, &source, radius),
            picture::Laid::Toned {
                saturation,
                strength,
            } => expected.blit_desaturated(at.0, at.1, &source, saturation, strength),
        }
        let placed = picture::Picture {
            x: at.0,
            y: at.1,
            width,
            height,
            base: 0,
            stride: width,
            laid,
        };
        let push = placed
            .push(&over((0, 0, SIDE.0, SIDE.1), false))
            .expect("bounded");
        let ran = painted(
            &gpu,
            picture::SPIRV,
            &push,
            &[&words(&under), &words(&source)],
            SIDE,
        );
        same(&ran, &expected, &format!("case {case}: {laid:?} at {at:?}"));
    }
}

#[test]
fn the_ring_pass_washes_and_frames_rings_to_the_bit() {
    let gpu = gpu();
    let mut rng = rng("the_ring_pass_washes_and_frames_rings_to_the_bit");
    for case in 0..48 {
        let under = drawn_picture(&mut rng, SIDE.0, SIDE.1);
        let (x, y, width, height) = rect(&mut rng);
        let Some(area) = cut((x, y, width, height)) else {
            continue;
        };
        let ring_of = Ring {
            top_radius: between(&mut rng, 0, 14),
            bottom_radius: between(&mut rng, 0, 14),
            thickness: between(&mut rng, 0, 9),
        };
        let laid = match rng.below(3) {
            0 => ring::Laid::Washed(RingInk::Solid(colour(&mut rng))),
            1 => ring::Laid::Washed(RingInk::Bevel {
                light: colour(&mut rng),
                shade: colour(&mut rng),
            }),
            _ => ring::Laid::Framed(colour(&mut rng)),
        };
        let mut expected = under.clone();
        match laid {
            ring::Laid::Washed(ink) => expected.wash_ring(x, y, width, height, ring_of, ink),
            ring::Laid::Framed(color) => expected.frame_ring(x, y, width, height, ring_of, color),
        }
        let placed = ring::Placed {
            x,
            y,
            width,
            height,
            ring: ring_of,
            laid,
        };
        let push = placed.push(&over(area, true)).expect("a bounded ring");
        let ran = painted(
            &gpu,
            ring::SPIRV,
            &push,
            &[&words(&under)],
            (area.2, area.3),
        );
        same(&ran, &expected, &format!("case {case}: {placed:?}"));
    }
}

#[test]
fn a_paint_past_a_bound_is_refused() {
    let whole = over((0, 0, SIDE.0, SIDE.1), true);
    let laid = plate::Plate {
        x: 0,
        y: 0,
        width: SIDE.0,
        height: SIDE.1,
        radius: tairix_shaders::MAX_PASS_RADIUS + 1,
        color: Color::rgb(1, 2, 3),
        replace: false,
    };
    assert_eq!(laid.push(&whole), Err(paint::Refused::Radius));
    let smaller = plate::Plate {
        width: 8,
        radius: 2,
        ..laid
    };
    assert_eq!(
        smaller.push(&whole),
        Err(paint::Refused::Extent),
        "a rectangle not within the plate"
    );
    let ringed = ring::Placed {
        x: 0,
        y: 0,
        width: SIDE.0,
        height: SIDE.1,
        ring: Ring::uniform(2, tairix_shaders::MAX_PASS_RADIUS + 1),
        laid: ring::Laid::Framed(Color::rgb(1, 2, 3)),
    };
    assert_eq!(ringed.push(&whole), Err(paint::Refused::Radius));
    let washed = wash::Wash {
        x: 0,
        y: 0,
        width: SIDE.0,
        height: SIDE.1,
        top: Color::rgb(1, 2, 3),
        bottom: Color::rgb(1, 2, 3),
        planes: [
            None,
            Some(wash::Plane {
                base: 0,
                stride: SIDE.0,
            }),
        ],
    };
    assert_eq!(
        washed.push(&whole),
        Err(paint::Refused::Extent),
        "a second plane without a first"
    );
    let widest = tairix_raster::MAX_DRAWING_EXTENT;
    let masked = mask::Mask {
        x: 0,
        y: 0,
        width: widest,
        height: widest,
        base: 0,
        stride: widest,
        ink: Color::rgb(1, 2, 3),
    };
    assert_eq!(
        masked.push(&whole),
        Err(paint::Refused::Extent),
        "a plane of more pixels than a surface holds"
    );
    let shown = picture::Picture {
        x: 0,
        y: 0,
        width: widest,
        height: widest,
        base: 0,
        stride: widest,
        laid: picture::Laid::Rounded(4),
    };
    assert_eq!(shown.push(&whole), Err(paint::Refused::Extent));
}
