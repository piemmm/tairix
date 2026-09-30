//! Host tests of the ray-traced screensaver: a scene prepared before it is
//! revealed, every pixel shown once, the hold, the fade, the next scene, the
//! pace each frame keeps, and the governor that spares a slow machine.

use alloc::vec::Vec;

use tairix_raytrace::{Draft, Quality, Setting, Tracer};
use tairix_wm::{Color, Compositor, Pixel, Point, Surface, WindowId};

use super::{
    aspect, draw_setting, pace, Phase, Raytrace, FADE_MS, HOLD_NS, MAX_BATCH, MAX_VERTICES,
    MIN_BATCH, REVEAL_BUDGET_NS, SLICE_NS,
};
use crate::saver::SAVER_FRAME_NS;
use crate::tests::compositor;

const SIZE: (u32, u32) = (48, 27);
const MS: u64 = 1_000_000;

/// A black window of `SIZE` for a reveal to draw in.
fn canvas(comp: &mut Compositor) -> WindowId {
    let mut surface = Surface::new(SIZE.0, SIZE.1).expect("a surface");
    surface.fill(Color::rgb(0, 0, 0));
    comp.add_window(Point::ORIGIN, surface)
}

/// A clock that reads `step` later every time it is read.
fn ticking(step: u64) -> impl FnMut() -> u64 {
    let mut now = 0u64;
    move || {
        now += step;
        now
    }
}

/// Advance `saver` frame by frame from `now` until `done` holds of it,
/// answering the time of the frame that brought it there.
fn advance_until(
    saver: &mut Raytrace,
    wm: WindowId,
    comp: &mut Compositor,
    mut now: u64,
    done: fn(&Phase) -> bool,
) -> u64 {
    let mut clock = ticking(MS);
    if done(&saver.phase) {
        return now;
    }
    for _ in 0..100_000 {
        saver.advance(now, wm, comp, &mut clock);
        if done(&saver.phase) {
            return now;
        }
        now = saver.due_ns().max(now);
    }
    panic!("the saver never got there");
}

fn revealing(phase: &Phase) -> bool {
    matches!(phase, Phase::Revealing(_))
}

fn holding(phase: &Phase) -> bool {
    matches!(phase, Phase::Holding { .. })
}

fn content(comp: &Compositor, wm: WindowId) -> &Surface {
    comp.window(wm)
        .and_then(tairix_wm::Window::content)
        .expect("the window's picture")
}

fn brightness(comp: &Compositor, wm: WindowId) -> u64 {
    content(comp, wm)
        .pixels()
        .iter()
        .map(|pixel| u64::from(pixel.r) + u64::from(pixel.g) + u64::from(pixel.b))
        .sum()
}

/// The reveal ends with every pixel showing exactly what tracing that pixel
/// on its own shows: none missed, none drawn from another.
#[test]
fn the_reveal_shows_every_pixel_as_it_traces() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut saver = Raytrace::new(SIZE, false, 0).expect("a scene");
    let now = advance_until(&mut saver, wm, &mut comp, 0, revealing);
    let Phase::Revealing(scene) = &saver.phase else {
        panic!("revealing");
    };
    let direct = Tracer::new(scene, &saver.encoder, SIZE, saver.key);
    let expected: Vec<Pixel> = (0..SIZE.1)
        .flat_map(|y| (0..SIZE.0).map(move |x| (x, y)))
        .map(|at| direct.pixel(at, saver.quality).0)
        .collect();
    advance_until(&mut saver, wm, &mut comp, now, holding);
    assert_eq!(saver.shown, SIZE.0 * SIZE.1);
    let picture = content(&comp, wm);
    for y in 0..SIZE.1 {
        for x in 0..SIZE.0 {
            let at = (y * SIZE.0 + x) as usize;
            assert_eq!(
                picture.get(x, y),
                expected.get(at).copied(),
                "pixel ({x}, {y})"
            );
        }
    }
}

/// A scene with land to fill is prepared over several frames, drawing
/// nothing meanwhile, and only then revealed.
#[test]
fn a_scene_is_prepared_over_frames_before_it_is_revealed() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut saver = Raytrace::new(SIZE, false, 0).expect("a scene");
    saver.phase = Phase::Preparing(Draft::new(Setting::Bubbles, 7, aspect(SIZE)).expect("a draft"));
    let mut frames = 0;
    let mut now = 0;
    let mut clock = ticking(MS);
    while !revealing(&saver.phase) {
        let Phase::Preparing(draft) = &saver.phase else {
            panic!("preparing until revealed");
        };
        let left = draft.remaining();
        saver.advance(now, wm, &mut comp, &mut clock);
        if let Phase::Preparing(draft) = &saver.phase {
            assert!(draft.remaining() < left, "every frame fills rows");
        }
        now = saver.due_ns();
        frames += 1;
    }
    assert!(frames > 1, "the grids take more than one frame");
    assert_eq!(saver.shown, 0);
    assert_eq!(brightness(&comp, wm), 0, "nothing is drawn while preparing");
}

/// A quarter of the way through, the pixels shown are spread over every
/// part of the picture, not gathered in a band of it.
#[test]
fn part_way_the_revealed_pixels_are_scattered() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut saver = Raytrace::new(SIZE, false, 0).expect("a scene");
    let mut now = advance_until(&mut saver, wm, &mut comp, 0, revealing);
    let mut clock = ticking(MS);
    let total = SIZE.0 * SIZE.1;
    while saver.shown < total / 4 {
        saver.advance(now, wm, &mut comp, &mut clock);
        now = saver.due_ns();
    }
    assert!(revealing(&saver.phase));
    let mut shown = alloc::vec![false; total as usize];
    for index in 0..saver.shown {
        shown[saver.order.pixel(index) as usize] = true;
    }
    for band in 0..3 {
        let rows = band * SIZE.1 / 3..(band + 1) * SIZE.1 / 3;
        let pixels = u32::try_from(rows.len()).expect("a few rows") * SIZE.0;
        let lit = rows
            .flat_map(|y| (0..SIZE.0).map(move |x| (y * SIZE.0 + x) as usize))
            .filter(|at| shown[*at])
            .count();
        let lit = u32::try_from(lit).expect("a few pixels");
        assert!(
            lit * 8 > pixels && lit * 8 < pixels * 4,
            "band {band}: {lit} of {pixels}"
        );
    }
}

/// The whole picture is held a minute, costing nothing meanwhile, then fades
/// to black and gives way to a scene in another setting.
#[test]
fn the_whole_picture_is_held_then_faded_then_replaced() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut saver = Raytrace::new(SIZE, false, 0).expect("a scene");
    let done = advance_until(&mut saver, wm, &mut comp, 0, holding);
    let first = saver.setting;
    assert_eq!(saver.due_ns(), done + HOLD_NS);
    let lit = brightness(&comp, wm);
    assert!(lit > 0);
    comp.composite();
    let mut clock = ticking(MS);
    saver.advance(done + HOLD_NS / 2, wm, &mut comp, &mut clock);
    assert!(!comp.has_damage(), "a held picture draws nothing");
    assert_eq!(saver.due_ns(), done + HOLD_NS);
    let fade_start = done + HOLD_NS;
    saver.advance(fade_start, wm, &mut comp, &mut clock);
    assert!(matches!(saver.phase, Phase::Fading { .. }));
    let half = fade_start + u64::from(FADE_MS) * MS / 2;
    let mut now = saver.due_ns();
    while now < half {
        saver.advance(now, wm, &mut comp, &mut clock);
        now = saver.due_ns();
    }
    let dimmed = brightness(&comp, wm);
    assert!(
        dimmed < lit * 3 / 4 && dimmed > lit / 4,
        "{dimmed} of {lit} half way"
    );
    while matches!(saver.phase, Phase::Fading { .. }) {
        saver.advance(now, wm, &mut comp, &mut clock);
        now = saver.due_ns().max(now + SAVER_FRAME_NS);
    }
    assert_eq!(brightness(&comp, wm), 0, "faded to black");
    assert!(matches!(saver.phase, Phase::Preparing(_)));
    assert_ne!(saver.setting, first, "the next scene is set elsewhere");
    advance_until(&mut saver, wm, &mut comp, now, revealing);
    assert_eq!(saver.shown, 0);
    assert_eq!(saver.quality, Quality::Fine);
}

#[test]
fn under_reduced_motion_the_picture_is_cut_to_black() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut saver = Raytrace::new(SIZE, true, 0).expect("a scene");
    let done = advance_until(&mut saver, wm, &mut comp, 0, holding);
    let mut clock = ticking(MS);
    saver.advance(done + HOLD_NS, wm, &mut comp, &mut clock);
    assert_eq!(brightness(&comp, wm), 0);
    assert!(matches!(saver.phase, Phase::Preparing(_)));
}

/// Resting, the saver asks for nothing until its time is up, then composes
/// the next scene.
#[test]
fn a_rest_ends_in_the_next_scene() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut saver = Raytrace::new(SIZE, false, 0).expect("a scene");
    saver.phase = Phase::Resting { until_ns: HOLD_NS };
    let mut clock = ticking(MS);
    saver.advance(0, wm, &mut comp, &mut clock);
    assert_eq!(saver.due_ns(), HOLD_NS);
    assert!(matches!(saver.phase, Phase::Resting { .. }));
    saver.advance(HOLD_NS, wm, &mut comp, &mut clock);
    assert!(matches!(saver.phase, Phase::Preparing(_)));
}

/// A frame does what fits half a desktop frame at the pace the last one
/// kept, growing at most twofold, within fixed bounds.
#[test]
fn each_frame_does_what_fits_its_slice() {
    assert_eq!(pace(100, SLICE_NS, MAX_BATCH), 100);
    assert_eq!(
        pace(100, SLICE_NS / 10, MAX_BATCH),
        200,
        "grows at most twofold"
    );
    assert_eq!(pace(100, SLICE_NS * 4, MAX_BATCH), 25);
    assert_eq!(pace(MAX_BATCH, 1, MAX_BATCH), MAX_BATCH);
    assert_eq!(pace(MIN_BATCH, u64::MAX, MAX_BATCH), MIN_BATCH);
    assert_eq!(pace(0, 0, MAX_BATCH), MIN_BATCH);
    assert_eq!(
        pace(1 << 18, 1, MAX_VERTICES),
        1 << 19,
        "vertices have their own bound"
    );
    assert_eq!(pace(MAX_VERTICES, 1, MAX_VERTICES), MAX_VERTICES);
}

#[test]
fn the_first_frame_traces_the_fewest_pixels_and_quick_frames_grow_the_batch() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut saver = Raytrace::new(SIZE, false, 0).expect("a scene");
    let now = advance_until(&mut saver, wm, &mut comp, 0, revealing);
    assert_eq!(saver.batch, MIN_BATCH);
    let mut instant = ticking(0);
    saver.advance(now, wm, &mut comp, &mut instant);
    assert_eq!(saver.shown, MIN_BATCH);
    assert_eq!(saver.batch, MIN_BATCH * 2);
    assert_eq!(saver.due_ns(), now + SAVER_FRAME_NS);
    // A frame that took far longer than its slice shrinks the next.
    let mut slow = ticking(SLICE_NS * 8);
    saver.advance(now + SAVER_FRAME_NS, wm, &mut comp, &mut slow);
    assert_eq!(saver.batch, MIN_BATCH);
}

/// A reveal that would outrun its budget takes fewer samples a pixel for the
/// rest of it, one step at a time; one well within it keeps the finest.
#[test]
fn a_slow_reveal_takes_fewer_samples_and_a_quick_one_keeps_them() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut quick = Raytrace::new(SIZE, false, 0).expect("a scene");
    advance_until(&mut quick, wm, &mut comp, 0, revealing);
    let total = SIZE.0 * SIZE.1;
    let mut now = quick.due_ns();
    let mut clock = ticking(MS);
    while revealing(&quick.phase) {
        quick.advance(now, wm, &mut comp, &mut clock);
        now = quick.due_ns();
    }
    assert_eq!(quick.quality, Quality::Fine);
    let mut slow = Raytrace::new(SIZE, false, 0).expect("a scene");
    let mut now = advance_until(&mut slow, wm, &mut comp, 0, revealing);
    // Each frame's pixels, a sixty-fourth of the picture, take as long as
    // the whole budget allows for the picture at that rate, and more.
    let frame_ns = REVEAL_BUDGET_NS / 32;
    let mut clock = ticking(0);
    let mut seen = Vec::new();
    while revealing(&slow.phase) {
        slow.batch = (total / 64).max(1);
        slow.advance(now, wm, &mut comp, &mut clock);
        now += frame_ns;
        if seen.last() != Some(&slow.quality) {
            seen.push(slow.quality);
        }
    }
    assert_eq!(seen.first(), Some(&Quality::Fine));
    assert!(seen.len() > 1, "the quality stepped down: {seen:?}");
    for pair in seen.windows(2) {
        assert!(pair[1] < pair[0], "one step down at a time: {seen:?}");
    }
}

/// A window whose buffer the compositor let go of shows none of the picture,
/// so the reveal starts again from black rather than keeping a copy.
#[test]
fn a_lost_buffer_starts_the_picture_again() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut saver = Raytrace::new(SIZE, false, 0).expect("a scene");
    let mut now = advance_until(&mut saver, wm, &mut comp, 0, revealing);
    let mut clock = ticking(MS);
    for _ in 0..6 {
        saver.advance(now, wm, &mut comp, &mut clock);
        now = saver.due_ns();
    }
    assert!(saver.shown > MIN_BATCH);
    let _ = comp.set_surface(wm, Surface::new(4, 4).expect("a small surface"));
    let batch = saver.batch;
    saver.advance(now, wm, &mut comp, &mut clock);
    assert_eq!(saver.shown, batch);
    let picture = content(&comp, wm);
    assert_eq!((picture.width(), picture.height()), SIZE);
    let lit = picture
        .pixels()
        .iter()
        .filter(|pixel| pixel.a == u8::MAX)
        .count();
    assert_eq!(
        lit,
        (SIZE.0 * SIZE.1) as usize,
        "the fresh buffer is opaque black under the pixels"
    );
}

#[test]
fn a_new_setting_is_never_the_last_and_every_other_comes_up() {
    let mut dice = tairix_rng::NonCryptoRng::seed_from_u64(5);
    for last in Setting::ALL {
        let mut seen = Vec::new();
        for _ in 0..400 {
            let next = draw_setting(&mut dice, Some(last));
            assert_ne!(next, last);
            if !seen.contains(&next) {
                seen.push(next);
            }
        }
        assert_eq!(seen.len(), Setting::ALL.len() - 1);
    }
}

#[test]
fn a_screen_with_no_pixels_has_no_reveal() {
    assert!(Raytrace::new((0, 10), false, 0).is_none());
    assert!(Raytrace::new((10, 0), false, 0).is_none());
}

/// A frame's pixels are handed out as few as one to a worker, and come out
/// as they do traced in order on one core.
#[test]
fn a_frame_splits_its_pixels_across_the_workers() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut saver = Raytrace::new(SIZE, false, 0).expect("a scene");
    advance_until(&mut saver, wm, &mut comp, 0, revealing);
    let Phase::Revealing(scene) =
        core::mem::replace(&mut saver.phase, Phase::Resting { until_ns: 0 })
    else {
        panic!("revealing");
    };
    saver.trace(&scene, &tairix_parallel::SERIAL, 7);
    let alone = saver.traced.clone();
    let runner = tairix_parallel::Reversed::new(4);
    saver.trace(&scene, &runner, 7);
    assert_eq!(runner.widest(), 7, "a pixel a piece");
    assert_eq!(saver.traced, alone);
}

/// A frame repaints the pixels it traced and nothing else while they are few,
/// and the box they span once they are many.
#[test]
fn a_frame_repaints_only_the_pixels_it_traced() {
    let mut comp = compositor();
    let wm = canvas(&mut comp);
    let mut saver = Raytrace::new(SIZE, false, 0).expect("a scene");
    let now = advance_until(&mut saver, wm, &mut comp, 0, revealing);
    for batch in [5, 40] {
        saver.batch = batch;
        let mut clock = ticking(MS);
        saver.advance(now.max(saver.due_ns()), wm, &mut comp, &mut clock);
        let rects = saver.damage.rects();
        assert!(rects.len() <= saver.traced.len());
        let covered: u32 = rects.iter().map(|rect| rect.width * rect.height).sum();
        assert_eq!(covered, batch, "{batch} pixels, {rects:?}");
        for &(at, _) in &saver.traced {
            let pixel = Point::new(
                i32::try_from(at % SIZE.0).expect("small"),
                i32::try_from(at / SIZE.0).expect("small"),
            );
            assert!(saver.damage.contains(pixel), "{pixel:?} not repainted");
        }
    }
    saver.batch = 400;
    let mut clock = ticking(MS);
    saver.advance(saver.due_ns(), wm, &mut comp, &mut clock);
    assert_eq!(saver.damage.rects().len(), 1, "past the budget, one box");
    assert_eq!(saver.damage.rects()[0], saver.damage.bounds());
}
