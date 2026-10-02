//! Host tests of a reveal's work: a scene prepared over slices before any
//! pixel, its progress told as it goes, traced coarse to fine exactly as each
//! pixel traces alone at the tracer's best however slowly, the pace each
//! slice keeps, the next scene, and each whole picture handed over to be kept
//! once.

use alloc::vec::Vec;

use tairix_raster::Pixel;
use tairix_raytrace::{Quality, Reveal, Setting, Tracer};

use super::{draw_setting, pace, Engine, Stage, Traced, MIN_BATCH, QUALITY, SLICE_NS};
use crate::saver::raytrace::album::Unkept;
use crate::saver::raytrace::crew::{Status, TraceDesk, Turn};

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

/// Each pixel of a whole reveal, each step's colour laid at its own pixel,
/// from a transparent picture.
fn painted(steps: &[Traced]) -> Vec<Pixel> {
    let (width, height) = SIZE;
    let mut picture = alloc::vec![Pixel::TRANSPARENT; (width * height) as usize];
    for traced in steps {
        picture[(traced.step.y * width + traced.step.x) as usize] = traced.pixel;
    }
    picture
}

/// What tracing every pixel of `engine`'s scene alone at the best quality
/// shows, the engine being about to trace.
fn traced_alone(engine: &Engine) -> Vec<Pixel> {
    let Stage::Tracing(scene) = &engine.stage else {
        panic!("tracing");
    };
    let direct = Tracer::new(scene, &engine.encoder, SIZE, engine.plan.key);
    (0..SIZE.1)
        .flat_map(|y| (0..SIZE.0).map(move |x| (x, y)))
        .map(|at| direct.pixel(at, Quality::Fine).0)
        .collect()
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
    assert!(matches!(
        engine.step(&tairix_parallel::SERIAL, &mut traced, &mut clock),
        Status::Preparing(_)
    ));
    let mut slices = 0;
    let mut told = 0;
    while let Stage::Preparing(_) = &engine.stage {
        match engine.step(&tairix_parallel::SERIAL, &mut traced, &mut clock) {
            Status::Preparing(done) => {
                assert!(done >= told && done < 1000, "{told} then {done}");
                told = done;
            }
            Status::Tracing(done) => assert_eq!(done, 0, "tracing begins at its start"),
            other => panic!("{other:?} while preparing"),
        }
        slices += 1;
    }
    assert!(slices > 1, "the grids take more than one slice");
    assert!(told > 0, "the preparation's progress was told");
    assert!(tracing(&engine));
    assert!(traced.is_empty(), "nothing is traced while preparing");
}

/// Tracing tells its progress as the share of its steps traced, rising to
/// the whole only once every pixel is.
#[test]
fn tracing_tells_its_progress_until_the_picture_is_whole() {
    let mut engine = Engine::new(SIZE, 21).expect("an engine");
    let _ = run_until(&mut engine, tracing);
    let mut clock = ticking(MS);
    let mut traced = Vec::new();
    let mut told = 0;
    loop {
        match engine.step(&tairix_parallel::SERIAL, &mut traced, &mut clock) {
            Status::Tracing(done) => {
                assert!(done >= told && done < 1000, "{told} then {done}");
                told = done;
            }
            Status::Whole => break,
            other => panic!("{other:?} while tracing"),
        }
    }
    assert!(told > 500, "{told}");
    assert_eq!(traced.len(), (SIZE.0 * SIZE.1) as usize);
}

/// The reveal takes its steps in the order's own sequence, each showing
/// exactly what tracing that pixel on its own shows, and once it is painted
/// whole every pixel shows its own trace.
#[test]
fn the_reveal_traces_every_pixel_as_tracing_it_alone_shows() {
    let mut engine = Engine::new(SIZE, 11).expect("an engine");
    let _ = run_until(&mut engine, tracing);
    let expected = traced_alone(&engine);
    let order = engine.reveal.clone();
    let steps = run_until(&mut engine, whole);
    assert_eq!(steps.len(), (SIZE.0 * SIZE.1) as usize);
    for (index, traced) in steps.iter().enumerate() {
        assert_eq!(
            order.step(u32::try_from(index).expect("few")),
            Some(traced.step)
        );
        let at = (traced.step.y * SIZE.0 + traced.step.x) as usize;
        assert_eq!(
            Some(traced.pixel),
            expected.get(at).copied(),
            "step {index}"
        );
    }
    assert_eq!(painted(&steps), expected);
    assert_eq!(QUALITY, Quality::Fine, "the best the tracer has");
}

/// The first pass, the grid every later pass refines, is traced within a
/// small share of the steps, and nothing else comes first.
#[test]
fn the_first_pass_spans_the_picture_in_a_few_steps() {
    let mut engine = Engine::new(SIZE, 23).expect("an engine");
    let _ = run_until(&mut engine, tracing);
    let steps = run_until(&mut engine, whole);
    let coarsest = Reveal::coarsest(SIZE);
    let first = steps
        .iter()
        .take_while(|traced| traced.step.side == coarsest)
        .count();
    let points = SIZE.0.div_ceil(coarsest) * SIZE.1.div_ceil(coarsest);
    assert_eq!(first, points as usize);
    assert!(
        first * 3 < steps.len(),
        "the first pass took {first} of {} steps",
        steps.len()
    );
    assert!(steps[first..]
        .iter()
        .all(|traced| traced.step.side < coarsest));
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
    assert_eq!(pace(1 << 14, 1, most), most, "never past its bound");
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

/// However slowly the machine traces, every pixel is traced at the best
/// quality: there is no budget a slow reveal is cut down to meet.
#[test]
fn a_slow_reveal_still_traces_every_pixel_at_the_best_quality() {
    let mut slow = Engine::new(SIZE, 7).expect("an engine");
    let _ = run_until(&mut slow, tracing);
    let expected = traced_alone(&slow);
    // Each slice takes an hour of the clock.
    let mut clock = ticking(3_600_000 * MS);
    let mut traced = Vec::new();
    while tracing(&slow) {
        slow.batch = 32;
        let _ = slow.step(&tairix_parallel::SERIAL, &mut traced, &mut clock);
    }
    assert_eq!(painted(&traced), expected);
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
/// step.
#[test]
fn the_next_scene_is_set_elsewhere_and_revealed_from_its_start() {
    let mut engine = Engine::new(SIZE, 17).expect("an engine");
    let _ = run_until(&mut engine, tracing);
    let _ = run_until(&mut engine, whole);
    let first = engine.plan.setting;
    engine.next();
    assert_ne!(engine.plan.setting, first);
    assert!(matches!(engine.stage, Stage::Composing));
    let _ = run_until(&mut engine, tracing);
    assert_eq!(engine.shown, 0);
}

/// A slice ordered through the desk begins the scene the loop asked for
/// before it traces.
#[test]
fn an_order_begins_the_scene_asked_for_before_its_slice() {
    let mut engine = Engine::new(SIZE, 29).expect("an engine");
    let first = engine.plan.setting;
    let mut desk = TraceDesk::new();
    desk.next();
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
    assert!(matches!(status, Status::Preparing(_)), "{status:?}");
    assert_ne!(engine.plan.setting, first);
    assert!(matches!(engine.stage, Stage::Preparing(_)));
}

/// An engine keeping pictures hands each whole one over once, as traced,
/// however long it is stepped after, and the next scene's is kept in its
/// turn. One not keeping them hands nothing over.
#[test]
fn a_whole_picture_is_handed_over_once_as_traced() {
    let mut engine = Engine::new(SIZE, 31).expect("an engine");
    engine.keep_pictures();
    let _ = run_until(&mut engine, tracing);
    assert!(engine.take_finished().is_none(), "nothing is whole yet");
    let steps = run_until(&mut engine, whole);
    let kept = engine
        .take_finished()
        .expect("a picture handed over")
        .expect("held");
    assert_eq!(kept.setting, engine.plan.setting);
    assert_eq!(kept.seed, engine.plan.seed);
    assert_eq!(kept.size, SIZE);
    assert_eq!(kept.pixels, painted(&steps));
    let mut clock = ticking(MS);
    let mut after = Vec::new();
    for _ in 0..3 {
        let _ = engine.step(&tairix_parallel::SERIAL, &mut after, &mut clock);
    }
    assert!(after.is_empty(), "a whole scene traces nothing more");
    assert!(engine.take_finished().is_none(), "handed over once");

    engine.next();
    let _ = run_until(&mut engine, tracing);
    let _ = run_until(&mut engine, whole);
    let next = engine
        .take_finished()
        .expect("the next scene's picture")
        .expect("held");
    assert_ne!(next.setting, kept.setting);

    let mut plain = Engine::new(SIZE, 31).expect("an engine");
    let _ = run_until(&mut plain, tracing);
    let _ = run_until(&mut plain, whole);
    assert!(plain.take_finished().is_none());
}

/// A picture the heap would not hold a copy of is reported, not kept.
#[test]
fn a_picture_too_large_to_hold_is_reported_unheld() {
    let mut engine = Engine::new(SIZE, 41).expect("an engine");
    engine.keep_pictures();
    let _ = run_until(&mut engine, tracing);
    // As the heap refusing it leaves it.
    engine.album = super::Album::Filling(None);
    let _ = run_until(&mut engine, whole);
    assert_eq!(
        engine.take_finished(),
        Some(Err(Unkept::Unheld(engine.plan.setting)))
    );
}
