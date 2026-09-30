use alloc::vec::Vec;

use super::{Ring, RingGeometry, RingInk, RingPixel, Walk};
use crate::color::{Color, Pixel};
use crate::dither::DitherRow;
use crate::round::round_rect_coverage;
use crate::surface::Surface;

const GREY: Color = Color::rgb(0x80, 0x80, 0x80);
const LIGHT: Color = Color::rgba(255, 255, 255, 90);
const SHADE: Color = Color::rgba(0, 0, 0, 120);
const BEVEL: RingInk = RingInk::Bevel {
    light: LIGHT,
    shade: SHADE,
};

fn ring(top_radius: u32, bottom_radius: u32, thickness: u32) -> Ring {
    Ring {
        top_radius,
        bottom_radius,
        thickness,
    }
}

fn filled(w: u32, h: u32, color: Color) -> Surface {
    Surface::filled(w, h, color.premultiply()).expect("surface")
}

fn at(surface: &Surface, x: u32, y: u32) -> Pixel {
    surface.get(x, y).expect("in bounds")
}

/// What washing `color` at full strength over `under` lands at `(x, y)`.
fn washed(color: Color, under: Color, x: u32, y: u32) -> Pixel {
    color.over_biased(under.premultiply(), DitherRow::at(y).bias(x))
}

#[test]
fn straight_edges_face_the_light_by_side() {
    let (w, h) = (20, 16);
    let geometry = RingGeometry::new(w, h, ring(0, 0, 3));
    assert_eq!(geometry.pixel(10, 0).tone, 255, "top");
    assert_eq!(geometry.pixel(0, 8).tone, 255, "left");
    assert_eq!(geometry.pixel(10, h - 1).tone, -255, "bottom");
    assert_eq!(geometry.pixel(w - 1, 8).tone, -255, "right");
    assert_eq!(
        geometry.pixel(0, 0).tone,
        255,
        "the top-left corner faces the light"
    );
    assert_eq!(
        geometry.pixel(w - 1, h - 1).tone,
        -255,
        "the bottom-right corner faces away"
    );
}

#[test]
fn a_square_corner_is_mitred_with_its_diagonal_edge_on() {
    let (w, h) = (20, 16);
    let geometry = RingGeometry::new(w, h, ring(0, 0, 3));
    for step in 0..3 {
        assert_eq!(
            geometry.pixel(w - 3 + step, 2 - step).tone,
            0,
            "the top-right diagonal is edge-on to the light"
        );
        assert_eq!(
            geometry.pixel(step, h - 1 - step).tone,
            0,
            "and the bottom-left"
        );
    }
    assert!(
        geometry.pixel(w - 2, 0).tone > 0,
        "above the mitre is the top edge"
    );
    assert!(
        geometry.pixel(w - 1, 1).tone < 0,
        "below it is the right edge"
    );
    assert!(
        geometry.pixel(0, h - 2).tone > 0,
        "beside it is the left edge"
    );
    assert!(
        geometry.pixel(1, h - 1).tone < 0,
        "under it is the bottom edge"
    );
}

#[test]
fn the_rising_and_falling_arcs_turn_through_the_same_tones() {
    // The light comes from along the main diagonal, so reflecting in it swaps
    // the top-right arc for the bottom-left one without changing what either
    // catches; reflecting an arc in the anti-diagonal instead turns it end for
    // end, which is what makes its midpoint edge-on to the light.
    let side = 32;
    let geometry = RingGeometry::new(side, side, ring(10, 10, 3));
    let mut edge_on = 0;
    for y in 0..10 {
        for x in side - 10..side {
            let rising = geometry.pixel(x, y);
            let falling = geometry.pixel(y, x);
            assert_eq!(rising.tone, falling.tone, "({x},{y})");
            assert_eq!(rising.band(), falling.band(), "({x},{y})");
            let turned = geometry.pixel(side - 1 - y, side - 1 - x);
            assert_eq!(rising.tone, -turned.tone, "({x},{y})");
            if rising.band() > 0 && rising.tone == 0 {
                edge_on += 1;
            }
        }
    }
    assert!(edge_on > 0, "each arc has a midpoint edge-on to the light");
}

#[test]
fn the_shapes_round_exactly_as_the_shared_rounded_rectangle() {
    let (w, h, r, t) = (30, 22, 8, 3);
    let geometry = RingGeometry::new(w, h, ring(r, r, t));
    for y in 0..h {
        for x in 0..w {
            let pixel = geometry.pixel(x, y);
            assert_eq!(pixel.outer, round_rect_coverage(x, y, w, h, r), "({x},{y})");
            let inside = x >= t && y >= t && x < w - t && y < h - t;
            let inner = if inside {
                round_rect_coverage(x - t, y - t, w - 2 * t, h - 2 * t, r - t)
            } else {
                0
            };
            assert_eq!(pixel.inner, inner, "({x},{y})");
        }
    }
}

#[test]
fn top_and_bottom_corners_round_by_their_own_radii() {
    let (w, h) = (30, 20);
    let geometry = RingGeometry::new(w, h, ring(8, 0, 2));
    assert_eq!(geometry.pixel(0, 0).outer, 0, "the top corner is rounded");
    assert_eq!(geometry.pixel(w - 1, 0).outer, 0);
    assert_eq!(
        geometry.pixel(0, h - 1).outer,
        255,
        "the bottom corner is square"
    );
    assert_eq!(geometry.pixel(w - 1, h - 1).outer, 255);
}

#[test]
fn a_ring_writes_only_the_band() {
    let (w, h) = (40, 30);
    let geometry = RingGeometry::new(w, h, ring(9, 9, 2));
    let mut surface = filled(w, h, GREY);
    surface.wash_ring(
        0,
        0,
        w,
        h,
        ring(9, 9, 2),
        RingInk::Solid(Color::rgb(200, 10, 10)),
    );
    let grey = GREY.premultiply();
    for y in 0..h {
        for x in 0..w {
            if geometry.pixel(x, y).band() == 0 {
                assert_eq!(at(&surface, x, y), grey, "({x},{y}) is off the band");
            } else {
                assert_ne!(at(&surface, x, y), grey, "({x},{y}) is on the band");
            }
        }
    }
}

#[test]
fn a_solid_ring_is_its_colour_where_the_band_covers_a_pixel() {
    let (w, h) = (40, 30);
    let red = Color::rgba(200, 10, 10, 160);
    let mut surface = filled(w, h, GREY);
    surface.wash_ring(0, 0, w, h, ring(9, 9, 2), RingInk::Solid(red));
    assert_eq!(at(&surface, 20, 0), washed(red, GREY, 20, 0));
    assert_eq!(at(&surface, 20, 1), washed(red, GREY, 20, 1));
    assert_eq!(at(&surface, 0, 15), washed(red, GREY, 0, 15));
    assert_eq!(at(&surface, w - 1, 15), washed(red, GREY, w - 1, 15));
}

#[test]
fn a_bevel_lights_the_top_and_left_and_shades_the_bottom_and_right() {
    let (w, h) = (40, 30);
    let mut surface = filled(w, h, GREY);
    surface.wash_ring(0, 0, w, h, ring(0, 0, 2), BEVEL);
    assert_eq!(at(&surface, 20, 0), washed(LIGHT, GREY, 20, 0));
    assert_eq!(at(&surface, 0, 15), washed(LIGHT, GREY, 0, 15));
    assert_eq!(at(&surface, 20, h - 1), washed(SHADE, GREY, 20, h - 1));
    assert_eq!(at(&surface, w - 1, 15), washed(SHADE, GREY, w - 1, 15));
    assert_eq!(
        at(&surface, w - 2, 1),
        GREY.premultiply(),
        "the mitre's diagonal is left alone"
    );
    let luma = |pixel: Pixel| pixel.unpremultiply().luma();
    assert!(luma(at(&surface, 20, 0)) > GREY.luma());
    assert!(luma(at(&surface, 20, h - 1)) < GREY.luma());
}

#[test]
fn a_ring_with_nothing_to_draw_draws_nothing() {
    let (w, h) = (24, 18);
    let untouched = filled(w, h, GREY);
    let mut thin = untouched.clone();
    thin.wash_ring(0, 0, w, h, ring(6, 6, 0), BEVEL);
    assert_eq!(thin, untouched, "a ring of no thickness has no band");
    let mut clear = untouched.clone();
    let invisible = RingInk::Bevel {
        light: Color::TRANSPARENT,
        shade: Color::TRANSPARENT,
    };
    clear.wash_ring(0, 0, w, h, ring(6, 6, 2), invisible);
    clear.wash_ring(
        0,
        0,
        w,
        h,
        ring(6, 6, 2),
        RingInk::Solid(Color::TRANSPARENT),
    );
    assert_eq!(clear, untouched, "a transparent ink changes nothing");
}

/// Paint `paint` over a whole `w`×`h` drawing, and again into four strips of
/// it, each standing in for its rectangle of the drawing; the strips must
/// hold exactly the whole paint's pixels.
fn assert_strips_match(w: u32, h: u32, paint: impl Fn(&mut Surface)) {
    let mut whole = filled(w, h, GREY);
    paint(&mut whole);
    let strips = [
        (0, 0, w, 7),
        (0, h - 5, w, 5),
        (0, 7, 4, h - 12),
        (w - 6, 7, 6, h - 12),
    ];
    for (sx, sy, sw, sh) in strips {
        let mut strip = filled(sw, sh, GREY);
        strip.with_origin(sx, sy, |strip| paint(strip));
        for y in 0..sh {
            for x in 0..sw {
                assert_eq!(
                    at(&strip, x, y),
                    at(&whole, sx + x, sy + y),
                    "strip ({sx},{sy}) pixel ({x},{y})"
                );
            }
        }
    }
}

#[test]
fn a_strip_of_a_ring_is_that_rectangle_of_the_whole_ring() {
    assert_strips_match(40, 30, |surface| {
        surface.wash_ring(0, 0, 40, 30, ring(9, 4, 2), BEVEL);
    });
    assert_strips_match(40, 30, |surface| {
        surface.frame_ring(0, 0, 40, 30, ring(9, 9, 2), Color::rgb(30, 40, 50));
    });
}

#[test]
fn a_clipped_ring_writes_only_inside_the_clip() {
    let (w, h) = (40, 30);
    let mut whole = filled(w, h, GREY);
    whole.wash_ring(0, 0, w, h, ring(9, 9, 2), BEVEL);
    let mut clipped = filled(w, h, GREY);
    clipped.with_clip(30, 0, 10, 12, |surface| {
        surface.wash_ring(0, 0, w, h, ring(9, 9, 2), BEVEL);
    });
    for y in 0..h {
        for x in 0..w {
            let inside = x >= 30 && y < 12;
            let expected = if inside {
                at(&whole, x, y)
            } else {
                GREY.premultiply()
            };
            assert_eq!(at(&clipped, x, y), expected, "({x},{y})");
        }
    }
}

#[test]
fn an_edge_laid_last_leaves_every_pixel_a_plate_laid_first_would() {
    let (w, h, r, t) = (48, 36, 11, 2);
    let rim = Color::rgb(0x23, 0x2b, 0x30);
    for ground in [
        Color::rgb(0x15, 0x1b, 0x1f),
        Color::rgba(0x15, 0x1b, 0x1f, 204),
    ] {
        let mut first = Surface::new(w, h).expect("surface");
        first.set_round_rect(0, 0, w, h, r, rim);
        first.set_round_rect(t, t, w - 2 * t, h - 2 * t, r - t, ground);

        let mut last = Surface::new(w, h).expect("surface");
        last.fill_rect(0, 0, w, h, ground);
        last.frame_ring(0, 0, w, h, ring(r, r, t), rim);
        assert_eq!(last, first, "ground {ground:?}");
    }
}

#[test]
fn an_edge_laid_last_cuts_away_whatever_strayed_past_the_plate() {
    let (w, h, r, t) = (48, 36, 11, 2);
    let geometry = RingGeometry::new(w, h, ring(r, r, t));
    let rim = Color::rgb(0x23, 0x2b, 0x30);
    let mut surface = Surface::new(w, h).expect("surface");
    surface.fill_rect(0, 0, w, h, Color::rgb(0x15, 0x1b, 0x1f));
    // Square content hard against every corner, as a plate seated flush in a
    // rounder surface would be.
    surface.fill_rect(0, 0, 20, h, Color::rgb(250, 0, 0));
    surface.fill_rect(w - 20, 0, 20, h, Color::rgb(250, 0, 0));
    surface.frame_ring(0, 0, w, h, ring(r, r, t), rim);
    let rim_only = rim.premultiply();
    for y in 0..h {
        for x in 0..w {
            let pixel = geometry.pixel(x, y);
            let drawn = at(&surface, x, y);
            assert!(
                drawn.a <= pixel.outer,
                "({x},{y}) is drawn past the plate's own shape"
            );
            if pixel.outer == u8::MAX && pixel.inner == 0 {
                assert_eq!(drawn, rim_only, "({x},{y}) is rim alone");
            }
        }
    }
}

/// What a walk over a `w`×`h` ring handed each pixel, row by row, or `None`
/// where it passed one over.
fn walked(w: u32, h: u32, ring: Ring, walk: Walk) -> Vec<Option<RingPixel>> {
    let mut surface = Surface::new(w, h).expect("surface");
    surface.walk_ring((0, 0, w, h), ring, walk, |pixel, dst, _| {
        let tone = u8::try_from(pixel.tone.unsigned_abs()).expect("a tone is at most 255");
        let sign = if pixel.tone < 0 { 1 } else { 2 };
        *dst = Pixel {
            r: pixel.outer,
            g: pixel.inner,
            b: tone,
            a: sign,
        };
    });
    surface
        .pixels()
        .iter()
        .map(|seen| {
            (seen.a != 0).then(|| RingPixel {
                outer: seen.r,
                inner: seen.g,
                tone: if seen.a == 1 {
                    -i32::from(seen.b)
                } else {
                    i32::from(seen.b)
                },
            })
        })
        .collect()
}

/// The walk hands over every pixel its ink could change, with that pixel's
/// own coverage, and passes over every other one: what it skips can never
/// change what a ring draws.
#[test]
fn a_walk_hands_over_exactly_the_pixels_its_ink_can_change() {
    let walks = [
        Walk {
            outside: false,
            toned: false,
        },
        Walk {
            outside: false,
            toned: true,
        },
        Walk {
            outside: true,
            toned: false,
        },
    ];
    let toned = |pixel: RingPixel, walk: Walk| RingPixel {
        tone: if walk.toned { pixel.tone } else { 0 },
        ..pixel
    };
    for (w, h) in [
        (1, 1),
        (2, 3),
        (7, 7),
        (8, 8),
        (16, 9),
        (24, 24),
        (33, 17),
        (40, 30),
    ] {
        for (top, bottom) in [(0, 0), (1, 1), (3, 8), (8, 0), (12, 12), (50, 50)] {
            for t in [1, 2, 3, 5, 9, 30] {
                let ring = ring(top, bottom, t);
                let geometry = RingGeometry::new(w, h, ring);
                for walk in walks {
                    let mut seen = walked(w, h, ring, walk).into_iter();
                    for y in 0..h {
                        for x in 0..w {
                            let whole = geometry.pixel(x, y);
                            let changes =
                                whole.inner < u8::MAX && (walk.outside || whole.outer > 0);
                            assert_eq!(
                                seen.next().flatten().map(|pixel| toned(pixel, walk)),
                                changes.then(|| toned(whole, walk)),
                                "({x},{y}) of {w}x{h} {ring:?} {walk:?}"
                            );
                        }
                    }
                }
            }
        }
    }
}

/// A circle is corner all the way round, yet its ring is worked out along
/// the band alone, never across the square it sits in.
#[test]
fn a_circles_ring_is_worked_out_along_its_band_alone() {
    let side = 200;
    let geometry = RingGeometry::new(side, side, ring(side / 2, side / 2, 3));
    for outside in [false, true] {
        let worked: u32 = (0..side)
            .flat_map(|ly| geometry.spans(ly, outside))
            .filter(|(_, _, shared)| shared.is_none())
            .map(|(_, len, _)| len)
            .sum();
        assert!(
            worked * 8 < side * side,
            "{worked} pixels worked out of a {side}-sided square"
        );
    }
}

/// A ring of any size draws into a small surface without overflowing: its
/// corners and its band are bounded as any rounded rectangle's are.
#[test]
fn a_ring_past_the_drawing_extent_draws_without_overflow() {
    let (huge, thick) = (u32::MAX, ring(u32::MAX, u32::MAX, u32::MAX));
    let mut washed_surface = filled(10, 10, GREY);
    washed_surface.wash_ring(0, 0, huge, huge, thick, BEVEL);
    washed_surface.wash_ring(0, 0, huge, huge, ring(huge, huge, 3), RingInk::Solid(GREY));
    assert_eq!(
        at(&washed_surface, 0, 0),
        GREY.premultiply(),
        "the far corner lies outside the arc"
    );
    let mut framed = filled(10, 10, GREY);
    framed.frame_ring(0, 0, huge, huge, thick, Color::rgb(30, 40, 50));
    assert_eq!(
        at(&framed, 0, 0),
        Pixel::TRANSPARENT,
        "an edge clears what lies past it"
    );
}
