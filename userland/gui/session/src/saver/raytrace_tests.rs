//! Host tests of the ray-traced screensaver as the serve loop runs it: what a
//! frame paints and repaints, the readout over it, the hold, the fade, the
//! rest and the next scene, a lost buffer, the options a reveal is launched
//! with, and a whole reveal ending as a reveal traced alone ends.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_parallel::Reversed;
use tairix_raster::Pixel;
use tairix_raytrace::{Reveal, Step};
use tairix_theme::Theme;
use tairix_wallpaper::{CpuUse, RaytraceOptions};
use tairix_wm::{Color, Compositor, Point, Scale, Surface, WindowId};

use super::{
    Engine, Phase, Raytrace, Request, Status, TraceHost, TraceLink, Traced, FADE_MS, HOLD_NS,
};
use crate::saver::seed_from;
use crate::tests::compositor;
use tairix_theme::motion::SceneClock;

const SIZE: (u32, u32) = (48, 27);
const MS: u64 = 1_000_000;
const LIT: Pixel = Pixel {
    r: 200,
    g: 180,
    b: 160,
    a: u8::MAX,
};

/// A window of `SIZE` filled with `fill` for a reveal to draw in.
fn canvas(comp: &mut Compositor, fill: Color) -> WindowId {
    let mut surface = Surface::new(SIZE.0, SIZE.1).expect("a surface");
    surface.fill(fill);
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

/// A step tracing `(x, y)` to `LIT`, a point of the grid of spacing `side`.
fn lit(x: u32, y: u32, side: u32) -> Traced {
    Traced {
        step: Step { x, y, side },
        pixel: LIT,
    }
}

/// Every step of a whole reveal of `SIZE`, each traced to `LIT`.
fn every_step_lit() -> Vec<Traced> {
    let order = Reveal::new(SIZE, 1).expect("a picture");
    (0..order.count())
        .map(|index| Traced {
            step: order.step(index).expect("a step"),
            pixel: LIT,
        })
        .collect()
}

/// A saver for `size` traced as `options` ask on `host`.
fn launched(
    size: (u32, u32),
    calm: bool,
    options: RaytraceOptions,
    host: Option<&dyn TraceHost>,
) -> Option<Raytrace> {
    Raytrace::new(size, (calm, 0), options, host, (&Theme::dark(), Scale::ONE))
}

fn idle() -> RaytraceOptions {
    RaytraceOptions::default()
}

/// What a scripted crew hands the loop, frame by frame, and what the loop
/// asked of it.
#[derive(Default)]
struct Script {
    frames: VecDeque<(Vec<Traced>, Status)>,
    asked: Vec<Request>,
    launched: Option<RaytraceOptions>,
}

impl Script {
    fn shared(frames: impl IntoIterator<Item = (Vec<Traced>, Status)>) -> Rc<RefCell<Self>> {
        Rc::new(RefCell::new(Self {
            frames: frames.into_iter().collect(),
            ..Self::default()
        }))
    }
}

/// A host whose crew plays a [`Script`] back.
struct Scripted(Rc<RefCell<Script>>);

impl TraceHost for Scripted {
    fn launch(
        &self,
        _engine: Engine,
        options: RaytraceOptions,
    ) -> Result<Box<dyn TraceLink>, Engine> {
        self.0.borrow_mut().launched = Some(options);
        Ok(Box::new(ScriptedLink(Rc::clone(&self.0))))
    }
}

struct ScriptedLink(Rc<RefCell<Script>>);

impl TraceLink for ScriptedLink {
    fn collect(&self, into: &mut Vec<Traced>) -> Status {
        match self.0.borrow_mut().frames.pop_front() {
            Some((steps, status)) => {
                into.extend(steps);
                status
            }
            None => Status::Tracing(0),
        }
    }

    fn request(&self, request: Request) {
        self.0.borrow_mut().asked.push(request);
    }
}

/// A saver traced by a crew playing `script`.
fn scripted(script: &Rc<RefCell<Script>>, calm: bool) -> Raytrace {
    launched(SIZE, calm, idle(), Some(&Scripted(Rc::clone(script)))).expect("a reveal")
}

/// A whole picture is held a minute, costing nothing meanwhile, then fades to
/// black, and the next scene is asked for and revealed.
#[test]
fn a_whole_picture_is_held_then_faded_then_the_next_scene_asked_for() {
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let script = Script::shared([(every_step_lit(), Status::Whole)]);
    let mut saver = scripted(&script, false);
    let mut clock = ticking(MS);
    saver.advance(0, wm, &mut comp, &mut clock);
    assert!(matches!(saver.phase, Phase::Holding { .. }));
    assert_eq!(saver.due_ns(), HOLD_NS);
    let lit = brightness(&comp, wm);
    assert!(lit > 0);
    comp.composite();
    saver.advance(HOLD_NS / 2, wm, &mut comp, &mut clock);
    assert!(!comp.has_damage(), "a held picture draws nothing");
    assert_eq!(saver.due_ns(), HOLD_NS);
    saver.advance(HOLD_NS, wm, &mut comp, &mut clock);
    assert!(matches!(saver.phase, Phase::Fading { .. }));
    let half = HOLD_NS + u64::from(FADE_MS) * MS / 2;
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
    assert!(
        script.borrow().asked.is_empty(),
        "nothing asked while fading"
    );
    while matches!(saver.phase, Phase::Fading { .. }) {
        saver.advance(now, wm, &mut comp, &mut clock);
        now = saver.due_ns().max(now + SceneClock::FRAME_NS);
    }
    assert_eq!(brightness(&comp, wm), 0, "faded to black");
    assert!(matches!(saver.phase, Phase::Revealing));
    assert_eq!(script.borrow().asked, [Request::Next]);
}

#[test]
fn under_reduced_motion_the_picture_is_cut_to_black() {
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let script = Script::shared([(every_step_lit(), Status::Whole)]);
    let mut saver = scripted(&script, true);
    let mut clock = ticking(MS);
    saver.advance(0, wm, &mut comp, &mut clock);
    assert!(brightness(&comp, wm) > 0);
    saver.advance(HOLD_NS, wm, &mut comp, &mut clock);
    assert_eq!(brightness(&comp, wm), 0);
    assert!(matches!(saver.phase, Phase::Revealing));
    assert_eq!(script.borrow().asked, [Request::Next]);
}

/// A scene the heap refused leaves the screen black a minute, asking for
/// nothing meanwhile, before the next is asked for.
#[test]
fn a_refused_scene_rests_the_screen_then_asks_for_the_next() {
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let script = Script::shared([(Vec::new(), Status::Failed)]);
    let mut saver = scripted(&script, false);
    let mut clock = ticking(MS);
    saver.advance(0, wm, &mut comp, &mut clock);
    assert!(matches!(saver.phase, Phase::Resting { .. }));
    assert_eq!(saver.due_ns(), HOLD_NS);
    saver.advance(HOLD_NS / 2, wm, &mut comp, &mut clock);
    assert!(script.borrow().asked.is_empty());
    saver.advance(HOLD_NS, wm, &mut comp, &mut clock);
    assert!(matches!(saver.phase, Phase::Revealing));
    assert_eq!(script.borrow().asked, [Request::Next]);
}

/// A picture the heap will not give a buffer rests the saver as a refused
/// scene does, asking nothing more meanwhile, rather than restarting the
/// reveal and retrying the allocation every frame.
#[test]
fn a_buffer_the_heap_refuses_rests_the_saver_rather_than_restarting_it() {
    // Past the raster's surface bound, the fresh buffer is refused exactly as
    // an exhausted heap refuses it.
    const REFUSED: (u32, u32) = (8192, 8193);
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let script = Script::shared([(alloc::vec![lit(0, 0, 8)], Status::Tracing(0))]);
    let host = Scripted(Rc::clone(&script));
    let mut saver = launched(REFUSED, false, idle(), Some(&host)).expect("a reveal");
    let mut clock = ticking(MS);
    saver.advance(0, wm, &mut comp, &mut clock);
    assert!(matches!(saver.phase, Phase::Resting { .. }));
    assert_eq!(saver.due_ns(), HOLD_NS);
    saver.advance(SceneClock::FRAME_NS, wm, &mut comp, &mut clock);
    assert_eq!(
        script.borrow().asked,
        [Request::Again],
        "nothing restarts while it rests"
    );
    saver.advance(HOLD_NS, wm, &mut comp, &mut clock);
    assert_eq!(script.borrow().asked, [Request::Again, Request::Next]);
}

/// A window whose buffer the compositor let go shows none of the picture, so
/// the loop asks for the scene again and paints what comes over black rather
/// than keeping a copy.
#[test]
fn a_lost_buffer_asks_for_the_scene_again_over_black() {
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let script = Script::shared([
        (alloc::vec![lit(0, 0, 2)], Status::Tracing(0)),
        (alloc::vec![lit(8, 8, 2)], Status::Tracing(0)),
    ]);
    let mut saver = scripted(&script, false);
    let mut clock = ticking(MS);
    saver.advance(0, wm, &mut comp, &mut clock);
    assert!(script.borrow().asked.is_empty());
    let _ = comp.set_surface(wm, Surface::new(4, 4).expect("a small surface"));
    saver.advance(SceneClock::FRAME_NS, wm, &mut comp, &mut clock);
    assert_eq!(script.borrow().asked, [Request::Again]);
    let picture = content(&comp, wm);
    assert_eq!((picture.width(), picture.height()), SIZE);
    assert!(picture.pixels().iter().all(|pixel| pixel.a == u8::MAX));
    assert_eq!(picture.get(8, 8), Some(LIT));
    assert_eq!(
        picture.get(0, 0),
        Some(Pixel {
            r: 0,
            g: 0,
            b: 0,
            a: u8::MAX
        }),
        "what the lost buffer held is gone"
    );
}

/// A frame paints each step's own pixel and repaints the cells about it and
/// nothing else while they are few, and the box they span once they are
/// many.
#[test]
fn a_frame_repaints_the_cells_its_steps_change_and_only_them() {
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let few: Vec<Traced> = (0..4).map(|at| lit(2 + at * 12, 10, 2)).collect();
    let many: Vec<Traced> = (0..400)
        .map(|at| lit(at % SIZE.0, at / SIZE.0, 1))
        .collect();
    let script = Script::shared([
        (few.clone(), Status::Tracing(0)),
        (many, Status::Tracing(0)),
    ]);
    let mut saver = scripted(&script, false);
    let mut clock = ticking(MS);
    saver.advance(0, wm, &mut comp, &mut clock);
    // Each step is a corner of four 2-pixel cells: sixteen pixels apiece.
    let covered: u32 = saver
        .damage
        .rects()
        .iter()
        .map(|rect| rect.width * rect.height)
        .sum();
    assert_eq!(covered, 16 * 4, "{:?}", saver.damage.rects());
    for traced in &few {
        let at = Point::new(
            i32::try_from(traced.step.x).expect("small"),
            i32::try_from(traced.step.y).expect("small"),
        );
        assert!(saver.damage.contains(at), "{at:?} not repainted");
        assert_eq!(
            content(&comp, wm).get(traced.step.x, traced.step.y),
            Some(LIT)
        );
    }
    assert!(
        !saver.damage.contains(Point::new(30, 20)),
        "far off is left alone"
    );
    let now = saver.due_ns();
    saver.advance(now, wm, &mut comp, &mut clock);
    assert_eq!(saver.damage.rects().len(), 1, "past the budget, one box");
    assert_eq!(saver.damage.rects()[0], saver.damage.bounds());
}

/// While the scene is prepared, and then traced, a readout above the picture
/// says how far it is, in the lower right; once the picture is whole it goes.
#[test]
fn a_readout_tells_the_preparing_then_the_tracing_and_goes_once_whole() {
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let script = Script::shared([
        (Vec::new(), Status::Preparing(370)),
        (Vec::new(), Status::Preparing(371)),
        (alloc::vec![lit(0, 0, 2)], Status::Tracing(520)),
        (every_step_lit(), Status::Whole),
    ]);
    let mut saver = scripted(&script, false);
    let mut clock = ticking(MS);
    saver.advance(0, wm, &mut comp, &mut clock);
    let readout = saver.readout.window().expect("a readout while preparing");
    assert!(comp.window(readout).is_some());
    let shown = content(&comp, readout).clone();
    assert!(
        shown.pixels().iter().any(|pixel| pixel.a > 0),
        "the readout is lettered"
    );
    let now = saver.due_ns();
    saver.advance(now, wm, &mut comp, &mut clock);
    assert_eq!(
        content(&comp, readout),
        &shown,
        "37.1% reads as 37%: nothing is redrawn"
    );
    let now = saver.due_ns();
    saver.advance(now, wm, &mut comp, &mut clock);
    assert_eq!(
        saver.readout.window(),
        Some(readout),
        "one window throughout"
    );
    assert_ne!(
        content(&comp, readout),
        &shown,
        "the tracing reads differently"
    );
    let now = saver.due_ns();
    saver.advance(now, wm, &mut comp, &mut clock);
    assert!(matches!(saver.phase, Phase::Holding { .. }));
    assert!(saver.readout.window().is_none());
    assert!(comp.window(readout).is_none(), "taken off the screen");
}

/// On a screen of a real size the readout stands a margin in from the lower
/// right corner, in grey, over the picture's own window.
#[test]
fn the_readout_stands_in_the_lower_right_over_the_picture() {
    let screen = (1920u32, 1080u32);
    let mut comp = compositor();
    let mut surface = Surface::new(screen.0, screen.1).expect("a surface");
    surface.fill(Color::rgb(0, 0, 0));
    let wm = comp.add_window(Point::ORIGIN, surface);
    let script = Script::shared([(Vec::new(), Status::Preparing(0))]);
    let mut saver =
        launched(screen, false, idle(), Some(&Scripted(Rc::clone(&script)))).expect("a reveal");
    saver.advance(0, wm, &mut comp, &mut ticking(MS));
    let readout = saver.readout.window().expect("a readout");
    let bounds = comp.window(readout).expect("the readout").bounds();
    let (right, bottom) = (
        u32::try_from(bounds.left()).expect("on screen") + bounds.width,
        u32::try_from(bounds.top()).expect("on screen") + bounds.height,
    );
    assert!(right < screen.0 && screen.0 - right <= 48, "{bounds:?}");
    assert!(bottom < screen.1 && screen.1 - bottom <= 48, "{bounds:?}");
    assert!(
        bounds.left() > i32::try_from(screen.0 / 2).expect("small"),
        "{bounds:?}"
    );
    let inked = content(&comp, readout)
        .pixels()
        .iter()
        .filter(|pixel| pixel.a == u8::MAX)
        .collect::<Vec<_>>();
    assert!(
        inked
            .iter()
            .all(|pixel| pixel.r == pixel.g && pixel.g == pixel.b),
        "grey ink"
    );
    assert_eq!(
        comp.family_front(wm),
        Some(readout),
        "the readout stands above the picture, and rises with it"
    );
}

/// Taking the saver down takes its readout with it.
#[test]
fn taking_the_saver_down_takes_its_readout_down() {
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let script = Script::shared([(Vec::new(), Status::Preparing(10))]);
    let mut saver = scripted(&script, false);
    saver.advance(0, wm, &mut comp, &mut ticking(MS));
    let readout = saver.readout.window().expect("a readout");
    saver.take_down(&mut comp);
    assert!(comp.window(readout).is_none());
}

/// A frame that brought nothing repaints nothing.
#[test]
fn a_frame_with_nothing_traced_repaints_nothing() {
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let script = Script::shared([
        (Vec::new(), Status::Tracing(0)),
        (Vec::new(), Status::Tracing(0)),
    ]);
    let mut saver = scripted(&script, false);
    saver.advance(0, wm, &mut comp, &mut ticking(MS));
    comp.composite();
    let now = saver.due_ns();
    saver.advance(now, wm, &mut comp, &mut ticking(MS));
    assert!(!comp.has_damage());
    assert_eq!(saver.due_ns(), 2 * SceneClock::FRAME_NS);
}

/// The options the user chose — the share of the machine, and whether
/// pictures are kept — are what the crew is launched with.
#[test]
fn a_crew_is_launched_with_the_options_asked_for() {
    for cpu in CpuUse::ALL {
        for save in [false, true] {
            let options = RaytraceOptions { cpu, save };
            let script = Script::shared([]);
            let _saver = launched(SIZE, false, options, Some(&Scripted(Rc::clone(&script))))
                .expect("a reveal");
            assert_eq!(script.borrow().launched, Some(options));
        }
    }
}

#[test]
fn a_screen_with_no_pixels_has_no_reveal() {
    assert!(launched((0, 10), false, idle(), None).is_none());
    assert!(launched((10, 0), false, idle(), None).is_none());
}

/// Run `saver`, traced on the loop, until its picture is whole.
fn reveal_whole(saver: &mut Raytrace, wm: WindowId, comp: &mut Compositor) {
    let mut clock = ticking(MS);
    let mut now = 0;
    for _ in 0..100_000 {
        saver.advance(now, wm, comp, &mut clock);
        if matches!(saver.phase, Phase::Holding { .. }) {
            return;
        }
        now = saver.due_ns().max(now);
    }
    panic!("the reveal never ended");
}

/// Traced on the loop with no thread granted, the reveal ends showing exactly
/// what the same scene's reveal, traced alone and painted step by step, shows.
#[test]
fn a_reveal_on_the_loop_ends_as_the_reveal_traced_alone() {
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let mut saver = launched(SIZE, false, idle(), None).expect("a reveal");
    reveal_whole(&mut saver, wm, &mut comp);

    let mut alone = Engine::new(SIZE, seed_from(0)).expect("an engine");
    let mut clock = ticking(MS);
    let mut steps = Vec::new();
    while alone
        .step(&tairix_parallel::SERIAL, &mut steps, &mut clock)
        .is_working()
    {}
    let mut expected = Surface::new(SIZE.0, SIZE.1).expect("a surface");
    for traced in &steps {
        expected.set(traced.step.x, traced.step.y, traced.pixel);
    }
    assert_eq!(content(&comp, wm).pixels(), expected.pixels());
}

/// On the loop, the idle setting traces on the loop's own thread alone however
/// wide the desktop's pool, and performance spreads its slices over the pool.
#[test]
fn on_the_loop_idle_keeps_to_one_core_and_performance_uses_the_pool() {
    static IDLE_POOL: Reversed = Reversed::new(4);
    static PERFORMANCE_POOL: Reversed = Reversed::new(4);
    for (cpu, pool) in [
        (CpuUse::Idle, &IDLE_POOL),
        (CpuUse::Performance, &PERFORMANCE_POOL),
    ] {
        let mut comp = compositor();
        comp.set_job_runner(pool);
        let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
        let options = RaytraceOptions { cpu, save: false };
        let mut saver = launched(SIZE, false, options, None).expect("a reveal");
        let mut clock = ticking(MS);
        let mut now = 0;
        for _ in 0..64 {
            saver.advance(now, wm, &mut comp, &mut clock);
            if !matches!(saver.phase, Phase::Revealing) {
                break;
            }
            now = saver.due_ns();
        }
        let widest = pool.widest();
        match cpu {
            CpuUse::Idle => assert_eq!(widest, 0, "the pool is never asked"),
            CpuUse::Performance => assert!(widest > 1, "the pool is asked for {widest}"),
        }
    }
}
