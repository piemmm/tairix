//! Unit tests for the effect pipeline.

use alloc::vec::Vec;

use tairix_geometry::{Rect, Region};
use tairix_parallel::{JobRunner, Reversed, SERIAL};
use tairix_raster::{Color, Pixel, Surface};

use super::{gain, Glow, Halo, Pipeline};
use crate::effects::{Effects, Phase, FULL};

fn surface(width: u32, height: u32) -> Surface {
    Surface::new(width, height).expect("surface allocation")
}

fn whole(screen: &Surface) -> Rect {
    Rect::new(0, 0, screen.width(), screen.height())
}

/// Every effect at `strength` but translucency and blur, which are not passes.
fn every_pass(strength: u16) -> Effects {
    Effects {
        opacity: FULL,
        blur: 0,
        scanlines: strength,
        glow: strength,
        fuzz: strength,
        phosphor: strength,
        wobble: strength,
    }
}

/// Nothing on but the effects `pick` sets.
fn only(pick: impl FnOnce(&mut Effects)) -> Effects {
    let mut effects = every_pass(0);
    pick(&mut effects);
    effects
}

/// What a fresh pipeline draws over the whole of `screen`.
fn drawn(effects: Effects, screen: &Surface, phase: Phase) -> Surface {
    let mut pipeline = Pipeline::new();
    pipeline
        .render(screen, whole(screen), effects, 100, phase, &SERIAL)
        .expect("a pass is on")
        .picture
        .clone()
}

/// A `side`x`side` field of `background` with a centred `block`x`block`
/// square of `lit`: a stand-in for a run of text on a screen.
fn lit_block(side: u32, block: u32, background: Pixel, lit: Pixel) -> Surface {
    let mut screen = Surface::filled(side, side, background).expect("surface allocation");
    let start = (side - block) / 2;
    for y in start..start + block {
        for x in start..start + block {
            screen.set(x, y, lit);
        }
    }
    screen
}

fn opaque(r: u8, g: u8, b: u8) -> Pixel {
    Color::rgb(r, g, b).premultiply()
}

/// The glow tests' field: a 32x32 screen with a lit 8x8 square spanning
/// `12..20` on both axes.
const FIELD: u32 = 32;
const BLOCK: u32 = 8;
const MIDDLE: u32 = FIELD / 2;
/// The first unlit column to the right of the lit square.
const OUTSIDE: u32 = MIDDLE + BLOCK / 2;
/// How far the glow reaches at the reference density.
const REACH: u32 = 5;

fn glow_only(strength: u16) -> Effects {
    only(|effects| effects.glow = strength)
}

#[test]
fn no_pass_draws_nothing_and_holds_nothing() {
    let screen = surface(4, 4);
    let mut pipeline = Pipeline::new();
    let rendered = pipeline.render(
        &screen,
        whole(&screen),
        every_pass(0),
        100,
        Phase(0),
        &SERIAL,
    );
    assert!(rendered.is_none());
    assert!(!pipeline.animated(every_pass(0), 100));
}

#[test]
fn a_render_is_deterministic_for_the_same_screen_and_phase() {
    let screen = lit_block(FIELD, BLOCK, opaque(200, 150, 50), opaque(255, 255, 255));
    let effects = Effects {
        opacity: 500,
        blur: 300,
        ..every_pass(400)
    };
    assert_eq!(
        drawn(effects, &screen, Phase(7)),
        drawn(effects, &screen, Phase(7))
    );
}

#[test]
fn scan_lines_dim_odd_rows_and_keep_their_coverage() {
    let effects = only(|effects| effects.scanlines = FULL);
    let white = drawn(
        effects,
        &Surface::filled(2, 4, opaque(255, 255, 255)).expect("surface allocation"),
        Phase(0),
    );
    assert_eq!(white.get(0, 0), Some(opaque(255, 255, 255)));
    let dimmed = white.get(0, 1).expect("in bounds");
    assert!(dimmed.r < 255, "a scan line darkens its row");
    // Dimming the light, not the coverage: a scan line made see-through would
    // show the desktop through an opaque terminal, brighter than the text
    // over a light wallpaper.
    assert_eq!(dimmed.a, 255);
    assert_eq!(white.get(0, 2), Some(opaque(255, 255, 255)));

    let translucent = Pixel {
        r: 0,
        g: 0,
        b: 0,
        a: 204,
    };
    let glass = drawn(
        effects,
        &Surface::filled(2, 4, translucent).expect("surface allocation"),
        Phase(0),
    );
    assert_eq!(glass.get(0, 1).map(|pixel| pixel.a), Some(204));
}

#[test]
fn fuzz_changes_pixels_and_another_phase_changes_them_differently() {
    let screen = Surface::filled(16, 4, opaque(128, 128, 128)).expect("surface allocation");
    let effects = only(|effects| effects.fuzz = FULL);
    let first = drawn(effects, &screen, Phase(0));
    assert_ne!(first, screen, "fuzz perturbs at least one pixel");
    assert_ne!(first, drawn(effects, &screen, Phase(1)));
}

#[test]
fn fuzz_keeps_every_pixel_premultiplied_and_its_alpha() {
    let translucent = Pixel {
        r: 60,
        g: 90,
        b: 100,
        a: 100,
    };
    let screen = Surface::filled(64, 8, translucent).expect("surface allocation");
    let jittered = drawn(only(|effects| effects.fuzz = FULL), &screen, Phase(3));
    for pixel in jittered.pixels() {
        assert_eq!(pixel.a, 100);
        assert!(pixel.r <= pixel.a && pixel.g <= pixel.a && pixel.b <= pixel.a);
    }
}

#[test]
fn wobble_displaces_content_horizontally() {
    let width = 8;
    let mut screen = surface(width, 2);
    for x in 0..width {
        let value = u8::try_from(x * 30).unwrap_or(u8::MAX);
        screen.set(x, 0, opaque(value, 0, 0));
        screen.set(x, 1, opaque(value, 0, 0));
    }
    let moved = drawn(only(|effects| effects.wobble = FULL), &screen, Phase(0));
    // Row 0's phase angle is exactly zero, so its content is untouched.
    for x in 0..width {
        assert_eq!(moved.get(x, 0), screen.get(x, 0));
    }
    // Row 1 moves right by three columns (amplitude 6 at full strength, angle
    // six sixty-fourths of a turn), so its leftmost columns turn transparent.
    assert_eq!(moved.get(0, 1), Some(Pixel::TRANSPARENT));
    for x in 3..width {
        assert_eq!(moved.get(x, 1), screen.get(x - 3, 1));
    }
}

#[test]
fn a_phosphor_trail_outlives_the_light_that_left_it() {
    let effects = only(|effects| effects.phosphor = FULL);
    let mut screen = surface(2, 2);
    screen.set(0, 0, opaque(255, 255, 255));
    let mut pipeline = Pipeline::new();
    let bounds = whole(&screen);
    let _ = pipeline.render(&screen, bounds, effects, 100, Phase(0), &SERIAL);
    // A step takes the light into the trail.
    let _ = pipeline.render(&screen, Rect::EMPTY, effects, 100, Phase(1), &SERIAL);
    screen.set(0, 0, opaque(0, 0, 0));
    let dark = Rect::new(0, 0, 1, 1);
    let frame = pipeline
        .render(&screen, dark, effects, 100, Phase(1), &SERIAL)
        .expect("a pass is on");
    assert!(
        frame.picture.get(0, 0).expect("in bounds").r > 0,
        "the earlier light still shows through"
    );
}

#[test]
fn clearing_forgets_the_trail() {
    let effects = only(|effects| effects.phosphor = FULL);
    let mut screen = surface(2, 2);
    screen.set(0, 0, opaque(255, 255, 255));
    let mut pipeline = Pipeline::new();
    let _ = pipeline.render(&screen, whole(&screen), effects, 100, Phase(0), &SERIAL);
    let _ = pipeline.render(&screen, Rect::EMPTY, effects, 100, Phase(1), &SERIAL);
    pipeline.clear();
    screen.set(0, 0, opaque(0, 0, 0));
    let frame = pipeline
        .render(&screen, whole(&screen), effects, 100, Phase(2), &SERIAL)
        .expect("a pass is on");
    assert_eq!(frame.picture.get(0, 0), Some(opaque(0, 0, 0)));
}

#[test]
fn a_phosphor_screen_that_stops_changing_settles_and_stops_animating() {
    let effects = only(|effects| effects.phosphor = FULL);
    let mut screen = lit_block(FIELD, BLOCK, opaque(0, 0, 0), opaque(255, 255, 255));
    let mut pipeline = Pipeline::new();
    assert!(
        !pipeline.animated(effects, 100),
        "nothing drawn yet, nothing to decay"
    );
    let _ = pipeline.render(&screen, whole(&screen), effects, 100, Phase(0), &SERIAL);
    assert!(pipeline.animated(effects, 100), "a fresh trail is pending");

    // The text goes; its trail decays a step at a time until it is gone.
    screen = Surface::filled(FIELD, FIELD, opaque(0, 0, 0)).expect("surface allocation");
    let _ = pipeline.render(&screen, whole(&screen), effects, 100, Phase(0), &SERIAL);
    let mut phase = Phase(0);
    let mut steps = 0;
    while pipeline.animated(effects, 100) {
        phase = phase.advance();
        steps += 1;
        assert!(
            steps < 64,
            "a trail at full persistence settles well within this"
        );
        let _ = pipeline.render(&screen, Rect::EMPTY, effects, 100, phase, &SERIAL);
    }
    // A step of a settled trail changes nothing and reports nothing.
    let frame = pipeline
        .render(&screen, Rect::EMPTY, effects, 100, phase.advance(), &SERIAL)
        .expect("a pass is on");
    assert!(frame.changed.is_empty());
    assert_eq!(frame.picture, &screen);
}

/// Turning the last effect off owes the window its plain screen whole, once:
/// the effected picture stands until it is replaced.
#[test]
fn retiring_a_drawn_pipeline_owes_the_plain_screen_once() {
    let screen = surface(8, 8);
    let mut pipeline = Pipeline::new();
    assert!(!pipeline.retire(), "nothing was ever drawn");
    let effects = only(|effects| effects.scanlines = 500);
    let _ = pipeline.render(&screen, whole(&screen), effects, 100, Phase(0), &SERIAL);
    assert!(pipeline.retire());
    assert!(!pipeline.retire(), "and only once");
}

#[test]
fn only_what_moves_whatever_the_screen_shows_keeps_animating() {
    let screen = surface(8, 8);
    for (effects, moves) in [
        (only(|effects| effects.wobble = 500), true),
        (only(|effects| effects.fuzz = 500), true),
        (only(|effects| effects.scanlines = 500), false),
        (glow_only(500), false),
    ] {
        let mut pipeline = Pipeline::new();
        let _ = pipeline.render(&screen, whole(&screen), effects, 100, Phase(0), &SERIAL);
        assert_eq!(pipeline.animated(effects, 100), moves, "{effects:?}");
    }
}

#[test]
fn a_keystroke_under_a_static_effect_presents_its_neighbourhood_not_the_window() {
    let mut screen = surface(200, 100);
    let mut pipeline = Pipeline::new();
    let effects = glow_only(FULL);
    let _ = pipeline.render(&screen, whole(&screen), effects, 100, Phase(0), &SERIAL);
    let cell = Rect::new(40, 30, 8, 16);
    for y in 30..46 {
        for x in 40..48 {
            screen.set(x, y, opaque(255, 255, 255));
        }
    }
    let frame = pipeline
        .render(&screen, cell, effects, 100, Phase(0), &SERIAL)
        .expect("a pass is on");
    let reach = i32::try_from(REACH).unwrap_or(i32::MAX);
    assert_eq!(
        frame.changed.bounds(),
        Rect::new(40 - reach, 30 - reach, 8 + 2 * REACH, 16 + 2 * REACH)
    );
    // An unchanged screen presents nothing at all.
    let frame = pipeline
        .render(&screen, Rect::EMPTY, effects, 100, Phase(0), &SERIAL)
        .expect("a pass is on");
    assert!(frame.changed.is_empty());
}

/// A keystroke under a wobble and a glow is redrawn as a few hulls of rows,
/// not as one strip per row each blurred from its own neighbourhood.
#[test]
fn a_keystroke_under_a_wobble_glows_a_few_hulls_not_a_strip_per_row() {
    let mut screen = surface(200, 100);
    let mut pipeline = Pipeline::new();
    let effects = only(|effects| {
        effects.wobble = FULL;
        effects.glow = FULL;
    });
    let _ = pipeline.render(&screen, whole(&screen), effects, 100, Phase(0), &SERIAL);
    let cell = Rect::new(40, 30, 8, 16);
    for y in 30..46 {
        for x in 40..48 {
            screen.set(x, y, opaque(255, 255, 255));
        }
    }
    let frame = pipeline
        .render(&screen, cell, effects, 100, Phase(0), &SERIAL)
        .expect("a pass is on");
    let rects = frame.changed.rects().len();
    assert!(rects <= 3, "{rects} rectangles for two hulls of rows");
}

/// A screen of `width` x `height` with something different in every pixel.
fn patterned(width: u32, height: u32) -> Surface {
    let mut screen = surface(width, height);
    for y in 0..height {
        for x in 0..width {
            let shade = |value: u32| u8::try_from(value & 0xFF).unwrap_or(0);
            let lit = if (x / 7 + y / 11) % 3 == 0 { 255 } else { 40 };
            screen.set(
                x,
                y,
                Color::rgba(shade(x * 3), shade(y * 5), shade(lit), 230).premultiply(),
            );
        }
    }
    screen
}

/// One step of a scripted session: some text redrawn, then a render at
/// `phase` with some effects in force.
struct Step {
    paint: Option<(Rect, Pixel)>,
    phase: u32,
    effects: Effects,
}

/// Run `steps` through one pipeline told only what changed and another told
/// every render that the whole screen did, asserting after every step that
/// both drew the same picture and that the first reported every pixel its
/// picture changed in.
fn assert_incremental_matches_whole(
    width: u32,
    height: u32,
    steps: &[Step],
    runner: &dyn JobRunner,
) {
    let mut screen = patterned(width, height);
    let mut told = Pipeline::new();
    let mut whole_each_time = Pipeline::new();
    let mut previous: Option<Surface> = None;
    for (index, step) in steps.iter().enumerate() {
        let mut damage = Rect::EMPTY;
        if let Some((rect, pixel)) = step.paint {
            for y in super::rows_of(rect) {
                for x in super::columns_of(rect) {
                    screen.set(u32::try_from(x).unwrap_or(0), y, pixel);
                }
            }
            damage = rect;
        }
        let phase = Phase(step.phase);
        let reference = whole_each_time
            .render(&screen, whole(&screen), step.effects, 100, phase, &SERIAL)
            .expect("a pass is on")
            .picture
            .clone();
        let frame = told
            .render(&screen, damage, step.effects, 100, phase, runner)
            .expect("a pass is on");
        let first_difference = (0..height)
            .flat_map(|y| (0..width).map(move |x| (x, y)))
            .find(|&(x, y)| frame.picture.get(x, y) != reference.get(x, y));
        assert_eq!(
            first_difference.map(|(x, y)| (x, y, frame.picture.get(x, y), reference.get(x, y))),
            None,
            "step {index}: the pictures differ"
        );
        if let Some(before) = previous.as_ref() {
            for y in 0..height {
                for x in 0..width {
                    if before.get(x, y) != frame.picture.get(x, y) {
                        let at = tairix_geometry::Point::new(
                            i32::try_from(x).unwrap_or(0),
                            i32::try_from(y).unwrap_or(0),
                        );
                        assert!(
                            frame.changed.contains(at),
                            "step {index}: ({x}, {y}) changed but was not reported"
                        );
                    }
                }
            }
        }
        previous = Some(frame.picture.clone());
    }
}

fn script(effects: &[Effects]) -> Vec<Step> {
    let mut steps = Vec::new();
    let white = opaque(255, 255, 255);
    let dark = opaque(10, 10, 10);
    for (round, &effects) in effects.iter().enumerate() {
        let round = u32::try_from(round).unwrap_or(0);
        let base = round * 4;
        steps.extend([
            Step {
                paint: None,
                phase: base,
                effects,
            },
            Step {
                paint: Some((Rect::new(5, 3, 9, 14), white)),
                phase: base,
                effects,
            },
            Step {
                paint: None,
                phase: base + 1,
                effects,
            },
            Step {
                paint: Some((Rect::new(5, 3, 9, 14), dark)),
                phase: base + 1,
                effects,
            },
            Step {
                paint: Some((Rect::new(0, 20, 30, 2), white)),
                phase: base + 2,
                effects,
            },
            Step {
                paint: None,
                phase: base + 3,
                effects,
            },
            Step {
                paint: None,
                phase: base + 3,
                effects,
            },
        ]);
    }
    steps
}

#[test]
fn every_combination_drawn_incrementally_matches_the_whole_redraw() {
    // Each of the five passes alone, then every pass together, then strengths
    // changed under a live trail: a slider must not perturb what it does not
    // name.
    let passes = [
        only(|effects| effects.wobble = 700),
        only(|effects| effects.phosphor = 900),
        only(|effects| effects.scanlines = 600),
        glow_only(800),
        only(|effects| effects.fuzz = 600),
        every_pass(700),
        Effects {
            glow: 300,
            ..every_pass(700)
        },
        Effects {
            wobble: 0,
            fuzz: 0,
            ..every_pass(500)
        },
    ];
    assert_incremental_matches_whole(48, 40, &script(&passes), &SERIAL);
}

#[test]
fn spreading_the_stages_across_a_runner_changes_no_pixel() {
    // Large enough that the stages are split, and the pieces run backwards:
    // a stage that read a piece another had not yet written would show here.
    let runner = Reversed::new(8);
    assert_incremental_matches_whole(700, 220, &script(&[every_pass(800)]), &runner);
    assert!(runner.widest() > 1, "the stages were split");
}

#[test]
fn glow_spreads_light_off_a_lit_square_and_falls_off_with_distance() {
    let before = lit_block(FIELD, BLOCK, opaque(0, 0, 0), opaque(255, 255, 255));
    let after = drawn(glow_only(FULL), &before, Phase(0));
    let red_at = |frame: &Surface, x: u32| frame.get(x, MIDDLE).expect("in bounds").r;
    let near = red_at(&after, OUTSIDE);
    let far = red_at(&after, OUTSIDE + 3);
    assert!(near > 0, "the pixel beside the square is lit");
    assert!(far > 0, "and one three further out still reached");
    assert!(near > far, "nearer the square is brighter: {near} vs {far}");
    let beyond = OUTSIDE + REACH + 1;
    assert_eq!(red_at(&after, beyond), red_at(&before, beyond));
}

#[test]
fn a_field_below_the_knee_does_not_glow_at_all() {
    let dim = opaque(100, 100, 100);
    let before = lit_block(FIELD, BLOCK, dim, dim);
    assert_eq!(drawn(glow_only(FULL), &before, Phase(0)), before);
}

#[test]
fn a_saturated_colour_glows_as_hard_as_any_other() {
    let dark = opaque(0, 0, 0);
    let red = drawn(
        glow_only(FULL),
        &lit_block(FIELD, BLOCK, dark, opaque(255, 0, 0)),
        Phase(0),
    );
    let green = drawn(
        glow_only(FULL),
        &lit_block(FIELD, BLOCK, dark, opaque(0, 255, 0)),
        Phase(0),
    );
    let red_halo = red.get(OUTSIDE, MIDDLE).expect("in bounds");
    let green_halo = green.get(OUTSIDE, MIDDLE).expect("in bounds");
    assert!(red_halo.r > 0, "a saturated red square glows");
    assert_eq!(red_halo.r, green_halo.g, "as hard as a saturated green one");
}

#[test]
fn glow_lifts_the_alpha_of_what_it_lights() {
    let translucent = Pixel {
        r: 0,
        g: 0,
        b: 0,
        a: 128,
    };
    let after = drawn(
        glow_only(FULL),
        &lit_block(FIELD, BLOCK, translucent, opaque(255, 255, 255)),
        Phase(0),
    );
    let halo = after.get(OUTSIDE, MIDDLE).expect("in bounds");
    assert!(halo.a > translucent.a);
    assert!(halo.r <= halo.a, "and the pixel stays premultiplied");
}

#[test]
fn a_stronger_glow_adds_more_light() {
    let field = lit_block(FIELD, BLOCK, opaque(0, 0, 0), opaque(255, 255, 255));
    let halo = |strength: u16| {
        let mut pipeline = Pipeline::new();
        pipeline
            .render(
                &field,
                whole(&field),
                glow_only(strength),
                100,
                Phase(0),
                &SERIAL,
            )
            .map_or(0, |frame| {
                frame.picture.get(OUTSIDE, MIDDLE).expect("in bounds").r
            })
    };
    assert_eq!(halo(0), 0, "no glow leaves the unlit pixel unlit");
    assert!(halo(FULL / 2) > halo(0));
    assert!(halo(FULL) > halo(FULL / 2));
}

/// A glow with no room to blur in still shows what changed beneath it, unlit,
/// rather than leaving the old picture standing.
#[test]
fn a_glow_without_room_shows_the_new_text_unlit() {
    let source = lit_block(FIELD, BLOCK, opaque(0, 0, 0), opaque(255, 255, 255));
    let stale = opaque(9, 9, 9);
    let mut lit = Surface::filled(FIELD, FIELD, stale).expect("surface allocation");
    let changed = Rect::new(10, 10, 12, 12);
    let mut region = Region::new();
    region.add(changed);
    Halo::default().glow(
        &mut lit,
        &source,
        &region,
        Glow {
            reach: REACH,
            intensity: u32::from(FULL),
        },
    );
    for y in 0..FIELD {
        for x in 0..FIELD {
            let inside = changed.contains(tairix_geometry::Point::new(
                i32::try_from(x).expect("small"),
                i32::try_from(y).expect("small"),
            ));
            let want = if inside {
                source.get(x, y)
            } else {
                Some(stale)
            };
            assert_eq!(lit.get(x, y), want, "({x}, {y})");
        }
    }
}

#[test]
fn the_glow_gain_is_linear_and_saturates() {
    let light = Pixel {
        r: 40,
        g: 60,
        b: 80,
        a: 80,
    };
    assert_eq!(gain(light, u32::from(FULL)), light);
    assert_eq!(gain(light, 2 * u32::from(FULL)).b, 160);
    assert_eq!(gain(light, 100 * u32::from(FULL)).a, u8::MAX);
    let lifted = gain(light, 1200);
    assert!(lifted.b > light.b && lifted.r <= lifted.a);
}

#[test]
fn degenerate_screens_draw_without_panicking() {
    for (width, height) in [(0, 0), (1, 1), (1, 9), (9, 1)] {
        let screen = surface(width, height);
        let mut pipeline = Pipeline::new();
        let _ = pipeline.render(
            &screen,
            whole(&screen),
            every_pass(500),
            100,
            Phase(0),
            &SERIAL,
        );
        let _ = pipeline.render(
            &screen,
            Rect::EMPTY,
            every_pass(500),
            100,
            Phase(1),
            &SERIAL,
        );
    }
}

#[test]
fn a_reshaped_screen_starts_its_trail_afresh() {
    let effects = only(|effects| effects.phosphor = FULL);
    let mut pipeline = Pipeline::new();
    let lit = Surface::filled(4, 4, opaque(255, 255, 255)).expect("surface allocation");
    let _ = pipeline.render(&lit, whole(&lit), effects, 100, Phase(0), &SERIAL);
    let _ = pipeline.render(&lit, Rect::EMPTY, effects, 100, Phase(1), &SERIAL);
    let dark = Surface::filled(6, 3, opaque(0, 0, 0)).expect("surface allocation");
    let frame = pipeline
        .render(&dark, whole(&dark), effects, 100, Phase(2), &SERIAL)
        .expect("a pass is on");
    assert_eq!(frame.picture, &dark, "no ghost of the old shape");
    let mut region = Region::new();
    region.add(whole(&dark));
    assert_eq!(frame.changed, &region);
}
