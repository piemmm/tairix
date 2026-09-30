//! Host tests of a reveal's work: a scene prepared over slices before any
//! pixel, traced coarse to fine exactly as each pixel traces alone, the whole
//! picture showing after the first pass, the pace each slice keeps, the
//! governor that spares a slow machine, and the next scene and the same one
//! again.

use alloc::vec::Vec;

use tairix_raster::Pixel;
use tairix_raytrace::{Quality, Setting, Tracer};

use super::{
    draw_setting, pace, Engine, Stage, Traced, MAX_VERTICES, MIN_BATCH, REVEAL_BUDGET_NS, SLICE_NS,
};
use crate::saver::raytrace::crew::{Request, Status, TraceDesk, Turn};

const SIZE: (u32, u32) = (48, 27);
const MS: u64 = 1_000_000;

/// A clock that reads `step` later every time it is read.
fn ticking(step: u64) -> impl FnMut() -> u64 {
    let mut now = 0u64;
    move || {
        now += step;
        now
    }
}

fn tracing(engine: &Engine) -> bool {
    matches!(engine.stage, Stage::Tracing(_))
}

fn whole(engine: &Engine) -> bool {
    matches!(engine.stage, Stage::Whole)
}

/// Step `engine` across the calling thread until `done` holds of it,
/// gathering what it traces.
fn run_until(engine: &mut Engine, done: fn(&Engine) -> bool) -> Vec<Traced> {
    let mut clock = ticking(MS);
    let mut traced = Vec::new();
    for _ in 0..100_000 {
        if done(engine) {
            return traced;
        }
        let _ = engine.step(&tairix_parallel::SERIAL, &mut traced, &mut clock);
    }
    panic!("the engine never got there");
}

/// Each pixel of a whole reveal, painted step by step as the saver paints
/// them, from a transparent picture.
fn painted(steps: &[Traced]) -> Vec<Pixel> {
    let (width, height) = SIZE;
    let mut picture = alloc::vec![Pixel::TRANSPARENT; (width * height) as usize];
    for traced in steps {
        let block = traced.block;
        for y in block.y..block.y + block.height {
            for x in block.x..block.x + block.width {
                picture[(y * width + x) as usize] = traced.pixel;
            }
        }
    }
    picture
}

#[test]
fn a_screen_with_no_pixels_has_no_reveal() {
    assert!(Engine::new((0, 10), 1).is_none());
    assert!(Engine::new((10, 0), 1).is_none());
}

/// A scene with grids to fill is prepared over several slices, tracing
/// nothing meanwhile, and only then traced.
#[test]
fn a_scene_is_prepared_over_slices_before_any_pixel_is_traced() {
    let mut engine = Engine::new(SIZE, 5).expect("an engine");
    engine.plan.setting = Setting::Bubbles;
    let mut clock = ticking(MS);
    let mut traced = Vec::new();
    assert_eq!(
        engine.step(&tairix_parallel::SERIAL, &mut traced, &mut clock),
        Status::Working
    );
    let mut slices = 0;
    while let Stage::Preparing(draft) = &engine.stage {
        let left = draft.remaining();
        assert_eq!(
            engine.step(&tairix_parallel::SERIAL, &mut traced, &mut clock),
            Status::Working
        );
        if let Stage::Preparing(draft) = &engine.stage {
            assert!(draft.remaining() < left, "every slice fills rows");
        }
        slices += 1;
    }
    assert!(slices > 1, "the grids take more than one slice");
    assert!(tracing(&engine));
    assert!(traced.is_empty(), "nothing is traced while preparing");
}

/// The reveal takes its steps in the order's own sequence, each showing
/// exactly what tracing that pixel on its own shows, and once it is painted
/// whole every pixel shows its own trace.
#[test]
fn the_reveal_traces_every_pixel_as_tracing_it_alone_shows() {
    let mut engine = Engine::new(SIZE, 11).expect("an engine");
    let _ = run_until(&mut engine, tracing);
    let Stage::Tracing(scene) = &engine.stage else {
        panic!("tracing");
    };
    let direct = Tracer::new(scene, &engine.encoder, SIZE, engine.plan.key);
    let expected: Vec<Pixel> = (0..SIZE.1)
        .flat_map(|y| (0..SIZE.0).map(move |x| (x, y)))
        .map(|at| direct.pixel(at, engine.quality).0)
        .collect();
    let order = engine.reveal.clone();
    let steps = run_until(&mut engine, whole);
    assert_eq!(steps.len(), (SIZE.0 * SIZE.1) as usize);
    for (index, traced) in steps.iter().enumerate() {
        assert_eq!(
            order.block(u32::try_from(index).expect("few")),
            Some(traced.block)
        );
        let at = (traced.block.y * SIZE.0 + traced.block.x) as usize;
        assert_eq!(
            Some(traced.pixel),
            expected.get(at).copied(),
            "step {index}"
        );
    }
    assert_eq!(painted(&steps), expected);
}

/// The first pass's blocks cover the whole picture, so it all shows after a
/// small share of the steps; before the pass ends some of it is still bare.
#[test]
fn the_first_pass_covers_the_whole_picture_in_rough_blocks() {
    let mut engine = Engine::new(SIZE, 23).expect("an engine");
    let _ = run_until(&mut engine, tracing);
    let steps = run_until(&mut engine, whole);
    let (width, height) = SIZE;
    let mut shown = alloc::vec![false; (width * height) as usize];
    let mut bare = shown.len();
    let mut covering = None;
    for (taken, traced) in steps.iter().enumerate() {
        let block = traced.block;
        for y in block.y..block.y + block.height {
            for x in block.x..block.x + block.width {
                let pixel = &mut shown[(y * width + x) as usize];
                bare -= usize::from(!*pixel);
                *pixel = true;
            }
        }
        if bare == 0 {
            covering = Some(taken + 1);
            break;
        }
    }
    let covering = covering.expect("the reveal covers the picture");
    assert!(
        covering * 3 < steps.len(),
        "the picture showed only after {covering} of {} steps",
        steps.len()
    );
    assert!(
        steps[..covering]
            .iter()
            .all(|traced| traced.block.width > 1 || traced.block.height > 1),
        "the steps before it are the first pass's blocks"
    );
}

/// A slice does what fits half a desktop frame at the pace the last one
/// kept, growing at most twofold, within its bounds.
#[test]
fn each_slice_does_what_fits_half_a_frame() {
    let most = 1 << 15;
    assert_eq!(pace(100, SLICE_NS, most), 100);
    assert_eq!(pace(100, SLICE_NS / 10, most), 200, "grows at most twofold");
    assert_eq!(pace(100, SLICE_NS * 4, most), 25);
    assert_eq!(pace(most, 1, most), most);
    assert_eq!(pace(MIN_BATCH, u64::MAX, most), MIN_BATCH);
    assert_eq!(pace(0, 0, most), MIN_BATCH);
    assert_eq!(
        pace(1 << 18, 1, MAX_VERTICES),
        1 << 19,
        "vertices have their own bound"
    );
    assert_eq!(pace(MAX_VERTICES, 1, MAX_VERTICES), MAX_VERTICES);
    assert_eq!(
        pace(5, 1, 0),
        MIN_BATCH,
        "a bound of nothing still does one"
    );
}

#[test]
fn the_first_slice_traces_one_step_and_quick_slices_grow_the_batch() {
    let mut engine = Engine::new(SIZE, 3).expect("an engine");
    let _ = run_until(&mut engine, tracing);
    assert_eq!(engine.batch, MIN_BATCH);
    let mut traced = Vec::new();
    let mut instant = ticking(0);
    let _ = engine.step(&tairix_parallel::SERIAL, &mut traced, &mut instant);
    assert_eq!(traced.len(), 1);
    assert_eq!(engine.batch, MIN_BATCH * 2);
    // A slice that took far longer than half a frame shrinks the next.
    let mut slow = ticking(SLICE_NS * 8);
    let _ = engine.step(&tairix_parallel::SERIAL, &mut traced, &mut slow);
    assert_eq!(engine.batch, MIN_BATCH);
}

/// A reveal that would outrun its budget takes fewer samples a pixel for the
/// rest of it, one step at a time; one well within it keeps the finest.
#[test]
fn a_slow_reveal_takes_fewer_samples_and_a_quick_one_keeps_them() {
    let mut quick = Engine::new(SIZE, 7).expect("an engine");
    let _ = run_until(&mut quick, tracing);
    let _ = run_until(&mut quick, whole);
    assert_eq!(quick.quality, Quality::Fine);

    let mut slow = Engine::new(SIZE, 7).expect("an engine");
    let _ = run_until(&mut slow, tracing);
    let total = SIZE.0 * SIZE.1;
    // Each sixty-fourth of the picture takes as long as the whole budget
    // allows the picture at that rate, and more: a slice reads the clock
    // twice.
    let mut clock = ticking(REVEAL_BUDGET_NS / 64);
    let mut traced = Vec::new();
    let mut seen = alloc::vec![slow.quality];
    while tracing(&slow) {
        slow.batch = (total / 64).max(1);
        let _ = slow.step(&tairix_parallel::SERIAL, &mut traced, &mut clock);
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

/// A slice's steps are handed out as few as one to a worker, and come out as
/// they do traced in order on one core, whether the pieces run backwards or
/// on real threads at once.
#[test]
fn a_slice_splits_its_steps_across_the_workers() {
    let mut engine = Engine::new(SIZE, 13).expect("an engine");
    let _ = run_until(&mut engine, tracing);
    let Stage::Tracing(scene) = core::mem::replace(&mut engine.stage, Stage::Composing) else {
        panic!("tracing");
    };
    let trace = |runner: &dyn tairix_parallel::JobRunner| {
        let mut slots = alloc::vec![Traced::NONE; 7];
        engine.trace_into(&scene, runner, &mut slots);
        slots
    };
    let alone = trace(&tairix_parallel::SERIAL);
    assert!(alone.iter().all(|traced| *traced != Traced::NONE));
    let backwards = tairix_parallel::Reversed::new(4);
    assert_eq!(trace(&backwards), alone);
    assert_eq!(backwards.widest(), 7, "a step a piece");
    assert_eq!(trace(&tairix_parallel::Threaded::new(4)), alone);
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

/// The next scene is set elsewhere, and its reveal starts from its first
/// step at the finest quality.
#[test]
fn the_next_scene_is_set_elsewhere_and_revealed_from_its_start() {
    let mut engine = Engine::new(SIZE, 17).expect("an engine");
    let _ = run_until(&mut engine, tracing);
    let _ = run_until(&mut engine, whole);
    let first = engine.plan.setting;
    engine.apply(Request::Next);
    assert_ne!(engine.plan.setting, first);
    assert!(matches!(engine.stage, Stage::Composing));
    let _ = run_until(&mut engine, tracing);
    assert_eq!(engine.shown, 0);
    assert_eq!(engine.quality, Quality::Fine);
}

/// The same scene again is the same picture: mid-reveal it starts over from
/// the scene it holds, and once the scene is let go it is composed afresh
/// from its plan.
#[test]
fn the_same_scene_again_is_the_same_picture() {
    let mut engine = Engine::new(SIZE, 19).expect("an engine");
    let _ = run_until(&mut engine, tracing);
    let mut clock = ticking(MS);
    let mut partial = Vec::new();
    for _ in 0..4 {
        let _ = engine.step(&tairix_parallel::SERIAL, &mut partial, &mut clock);
    }
    assert!(engine.shown > 0 && tracing(&engine));
    engine.apply(Request::Again);
    assert_eq!(engine.shown, 0);
    let whole_picture = run_until(&mut engine, whole);
    assert_eq!(
        partial,
        whole_picture[..partial.len()],
        "the start again is the start it had"
    );
    engine.apply(Request::Again);
    assert!(matches!(engine.stage, Stage::Composing));
    let _ = run_until(&mut engine, tracing);
    assert_eq!(run_until(&mut engine, whole), whole_picture);
}

/// A slice ordered through the desk takes up what the loop asked before it
/// traces.
#[test]
fn an_order_takes_up_the_loops_request_before_its_slice() {
    let mut engine = Engine::new(SIZE, 29).expect("an engine");
    let first = engine.plan.setting;
    let mut desk = TraceDesk::new();
    desk.request(Request::Next);
    let Turn::Trace(order) = desk.turn() else {
        panic!("a slice");
    };
    let mut traced = Vec::new();
    let status = order.carry_out(
        &mut engine,
        &tairix_parallel::SERIAL,
        &mut traced,
        &mut ticking(MS),
    );
    assert_eq!(status, Status::Working);
    assert_ne!(engine.plan.setting, first);
    assert!(matches!(engine.stage, Stage::Preparing(_)));
}
