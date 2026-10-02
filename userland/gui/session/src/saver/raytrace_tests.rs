//! Host tests of the ray-traced screensaver as the serve loop runs it: what a
//! paint paints and marks and when paints come, the readout over it, the hold,
//! the fade, the rest and the next scene, a lost buffer painted afresh, the
//! options a reveal is launched with, and a whole reveal ending as a reveal
//! traced alone ends.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use tairix_parallel::Reversed;
use tairix_raster::Pixel;
use tairix_raytrace::{Reveal, Step};
use tairix_theme::Theme;
use tairix_wallpaper::{CpuUse, RaytraceOptions, SceneDetail};
use tairix_wm::{Color, Compositor, Point, Scale, Surface, WindowId};

use super::{
    paint_wait, Engine, Phase, Raytrace, Status, TraceHost, TraceLink, Traced, DETAIL_WAIT_NS,
    FADE_MS, HOLD_NS, MOST_WAIT_NS, PLAIN, READOUT_WAIT_NS,
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
    Raytrace::new(
        size,
        (calm, 0),
        (options, PLAIN.memory, PLAIN.tell),
        host,
        (&Theme::dark(), Scale::ONE),
    )
}

fn idle() -> RaytraceOptions {
    RaytraceOptions::default()
}

/// What a scripted crew hands the loop, frame by frame, and how many times
/// the loop asked it for the next scene.
#[derive(Default)]
struct Script {
    frames: VecDeque<(Vec<Traced>, Status)>,
    asked: usize,
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

    fn next(&self) {
        self.0.borrow_mut().asked += 1;
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
    saver.landed(MS);
    assert_eq!(saver.due_ns(), HOLD_NS, "a wake brings no hold forward");
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
    assert_eq!(script.borrow().asked, 0, "nothing asked while fading");
    while matches!(saver.phase, Phase::Fading { .. }) {
        saver.advance(now, wm, &mut comp, &mut clock);
        now = saver.due_ns().max(now + SceneClock::FRAME_NS);
    }
    assert_eq!(brightness(&comp, wm), 0, "faded to black");
    assert!(matches!(saver.phase, Phase::Revealing));
    assert_eq!(script.borrow().asked, 1);
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
    assert_eq!(script.borrow().asked, 1);
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
    assert_eq!(script.borrow().asked, 0);
    saver.advance(HOLD_NS, wm, &mut comp, &mut clock);
    assert!(matches!(saver.phase, Phase::Revealing));
    assert_eq!(script.borrow().asked, 1);
}

/// A picture with nowhere to be painted — its window gone, as a buffer the
/// heap refused leaves it — rests the saver as a refused scene does, asking
/// nothing meanwhile.
#[test]
fn a_picture_with_nowhere_to_paint_rests_the_saver() {
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let _ = comp.remove(wm);
    let script = Script::shared([(alloc::vec![lit(0, 0, 2)], Status::Tracing(0))]);
    let mut saver = scripted(&script, false);
    let mut clock = ticking(MS);
    saver.advance(0, wm, &mut comp, &mut clock);
    assert!(matches!(saver.phase, Phase::Resting { .. }));
    assert_eq!(saver.due_ns(), HOLD_NS);
    saver.advance(SceneClock::FRAME_NS, wm, &mut comp, &mut clock);
    assert_eq!(script.borrow().asked, 0, "nothing is asked while it rests");
    saver.advance(HOLD_NS, wm, &mut comp, &mut clock);
    assert_eq!(script.borrow().asked, 1);
}

/// A buffer the compositor let go is painted afresh from what the painter
/// kept — the picture a kept buffer shows — and nothing is traced again.
#[test]
fn a_lost_buffer_is_painted_afresh_without_tracing_again() {
    let frames = || {
        [
            (alloc::vec![lit(0, 0, 2)], Status::Tracing(0)),
            (alloc::vec![lit(8, 8, 2)], Status::Tracing(0)),
        ]
    };
    let mut kept_comp = compositor();
    let kept_wm = canvas(&mut kept_comp, Color::rgb(0, 0, 0));
    let mut kept = scripted(&Script::shared(frames()), false);
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let script = Script::shared(frames());
    let mut saver = scripted(&script, false);
    let mut clock = ticking(MS);
    for now in [0, SceneClock::FRAME_NS] {
        if now > 0 {
            let _ = comp.set_surface(wm, Surface::new(4, 4).expect("a small surface"));
        }
        saver.advance(now, wm, &mut comp, &mut clock);
        kept.advance(now, kept_wm, &mut kept_comp, &mut clock);
    }
    assert_eq!(script.borrow().asked, 0, "nothing is traced again");
    let picture = content(&comp, wm);
    assert_eq!((picture.width(), picture.height()), SIZE);
    assert!(picture.pixels().iter().all(|pixel| pixel.a == u8::MAX));
    assert_eq!(picture.pixels(), content(&kept_comp, kept_wm).pixels());
    assert!(
        picture.get(0, 0).is_some_and(|pixel| pixel.r > 0),
        "the first frame's step shows again"
    );
}

/// A frame repaints what its steps change and marks only the tiles about
/// them: the steps' own neighbourhoods, never the box they span.
#[test]
fn a_frame_marks_the_tiles_about_its_steps_and_only_them() {
    let screen = (320u32, 180u32);
    let mut comp = compositor();
    let mut surface = Surface::new(screen.0, screen.1).expect("a surface");
    surface.fill(Color::rgb(0, 0, 0));
    let wm = comp.add_window(Point::ORIGIN, surface);
    // Points of the last pass, far apart.
    let few: Vec<Traced> = [(21, 41), (101, 41), (181, 121), (261, 161)]
        .into_iter()
        .map(|(x, y)| lit(x, y, 1))
        .collect();
    let script = Script::shared([(few.clone(), Status::Tracing(0))]);
    let mut saver =
        launched(screen, false, idle(), Some(&Scripted(Rc::clone(&script)))).expect("a reveal");
    saver.advance(0, wm, &mut comp, &mut ticking(MS));
    let covered: u32 = saver
        .damage
        .rects()
        .iter()
        .map(|rect| rect.width * rect.height)
        .sum();
    // A step of the last pass reaches two pixels each way, so touches at most
    // four of the finest tiles.
    assert!(covered <= 4 * 4 * 16 * 16, "{:?}", saver.damage.rects());
    for traced in &few {
        let at = Point::new(
            i32::try_from(traced.step.x).expect("small"),
            i32::try_from(traced.step.y).expect("small"),
        );
        assert!(saver.damage.contains(at), "{at:?} not repainted");
    }
    assert!(
        !saver.damage.contains(Point::new(160, 100)),
        "between them is left alone"
    );
    assert!(
        !saver.damage.contains(Point::new(300, 10)),
        "far off is left alone"
    );
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

/// The wait between paints grows with the share of the picture shown: a scene
/// frame while the coarse passes form it, a quarter of the detail's wait as
/// the 4 px pass begins and all of it as the 2 px pass does, never shrinking
/// and never past the most.
#[test]
fn the_wait_between_paints_grows_with_the_picture_shown() {
    // A 1920 × 1080 picture's 4 px grid, and its 32 px grid.
    let detail_from = 480 * 270;
    let formed = 60 * 34;
    assert_eq!(paint_wait(0, detail_from), SceneClock::FRAME_NS);
    assert_eq!(paint_wait(formed, detail_from), SceneClock::FRAME_NS);
    assert_eq!(paint_wait(detail_from / 4, detail_from), DETAIL_WAIT_NS / 4);
    assert_eq!(paint_wait(detail_from, detail_from), DETAIL_WAIT_NS);
    assert_eq!(paint_wait(4 * detail_from, detail_from), MOST_WAIT_NS);
    assert_eq!(paint_wait(u64::MAX, detail_from), MOST_WAIT_NS);
    let mut last = 0;
    for shown in (0..=8 * detail_from).step_by(997) {
        let wait = paint_wait(shown, detail_from);
        assert!(wait >= last, "{shown}: {wait} after {last}");
        assert!((SceneClock::FRAME_NS..=MOST_WAIT_NS).contains(&wait));
        last = wait;
    }
}

/// A crew tracing a reveal of its picture at a steady `per_frame` steps a
/// scene frame by the test's clock, each to `LIT`.
struct Steady {
    order: Reveal,
    per_frame: u64,
    now: Rc<Cell<u64>>,
    handed: Rc<Cell<u32>>,
}

impl TraceLink for Steady {
    fn collect(&self, into: &mut Vec<Traced>) -> Status {
        let count = self.order.count();
        let due = (self.now.get() * self.per_frame / SceneClock::FRAME_NS).min(u64::from(count));
        let due = u32::try_from(due).expect("within the reveal");
        for index in self.handed.get()..due {
            into.push(Traced {
                step: self.order.step(index).expect("a step"),
                pixel: LIT,
            });
        }
        self.handed.set(due);
        if due == count {
            Status::Whole
        } else {
            Status::Tracing(0)
        }
    }

    fn next(&self) {}
}

/// A host launching one [`Steady`] crew.
struct SteadyHost(RefCell<Option<Steady>>);

impl TraceHost for SteadyHost {
    fn launch(
        &self,
        _engine: Engine,
        _options: RaytraceOptions,
    ) -> Result<Box<dyn TraceLink>, Engine> {
        Ok(Box::new(self.0.borrow_mut().take().expect("launched once")))
    }
}

/// Traced at a steady pace, a reveal is painted a scene frame apart while its
/// coarse passes form the picture, then further apart in step with the share
/// shown — never closer than the last two, at least the detail's wait once
/// the fine detail is under way, and never more than the most — in a small
/// share of the frames it takes, ending on the whole picture.
#[test]
fn paints_come_a_frame_apart_as_the_picture_forms_then_slow_to_the_most() {
    let screen = (320u32, 180u32);
    let now = Rc::new(Cell::new(0u64));
    let handed = Rc::new(Cell::new(0u32));
    let host = SteadyHost(RefCell::new(Some(Steady {
        order: Reveal::new(screen, 7).expect("a picture"),
        per_frame: 40,
        now: Rc::clone(&now),
        handed: Rc::clone(&handed),
    })));
    let mut comp = compositor();
    let mut surface = Surface::new(screen.0, screen.1).expect("a surface");
    surface.fill(Color::rgb(0, 0, 0));
    let wm = comp.add_window(Point::ORIGIN, surface);
    let mut saver = launched(screen, false, idle(), Some(&host)).expect("a reveal");
    let detail_from = saver.detail_from;
    assert_eq!(detail_from, 80 * 45, "every point of the 4 px grid");
    let mut clock = ticking(MS);
    let mut paints = Vec::new();
    for _ in 0..10_000 {
        now.set(saver.due_ns());
        let shown = saver.shown;
        saver.advance(now.get(), wm, &mut comp, &mut clock);
        if saver.shown > shown {
            paints.push((now.get(), saver.shown));
        }
        if !matches!(saver.phase, Phase::Revealing) {
            break;
        }
    }
    assert!(matches!(saver.phase, Phase::Holding { .. }));
    let frames = u64::from(screen.0 * screen.1) / 40;
    let count = u64::try_from(paints.len()).expect("few");
    assert!(count * 20 < frames, "{count} paints over {frames} frames");
    let mut last = 0;
    for pair in paints.windows(2) {
        let [(at, shown), (next, _)] = pair else {
            continue;
        };
        let wait = next - at;
        assert!(wait >= last && wait <= MOST_WAIT_NS, "{wait} after {last}");
        if *shown * 45 < detail_from {
            assert_eq!(wait, SceneClock::FRAME_NS, "{shown} shown");
        }
        if *shown >= detail_from && *shown < u64::from(screen.0 * screen.1) {
            assert!(wait >= DETAIL_WAIT_NS, "{shown} shown: {wait}");
        }
        last = wait;
    }
    assert!(
        content(&comp, wm)
            .pixels()
            .iter()
            .all(|pixel| *pixel == LIT),
        "every pixel its own trace"
    );
}

/// While a scene is prepared on its thread the loop comes back only to bring
/// the readout up to date; once the thread readies the scene, the loop comes
/// back at once and paints its first passes a scene frame apart.
#[test]
fn a_scene_readied_on_its_thread_is_shown_from_its_first_steps() {
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let script = Script::shared([
        (Vec::new(), Status::Preparing(500)),
        (Vec::new(), Status::Tracing(0)),
        (alloc::vec![lit(0, 0, 2)], Status::Tracing(0)),
    ]);
    let mut saver = scripted(&script, false);
    let mut clock = ticking(MS);
    saver.advance(0, wm, &mut comp, &mut clock);
    assert_eq!(saver.due_ns(), READOUT_WAIT_NS);
    let readied = 10 * MS;
    saver.landed(readied);
    assert_eq!(saver.due_ns(), readied, "back at once");
    saver.advance(readied, wm, &mut comp, &mut clock);
    assert_eq!(saver.due_ns(), SceneClock::FRAME_NS);
    saver.advance(SceneClock::FRAME_NS, wm, &mut comp, &mut clock);
    assert!(
        content(&comp, wm)
            .get(0, 0)
            .is_some_and(|pixel| pixel.r > 0),
        "the first step is shown"
    );
    assert_eq!(saver.due_ns(), 2 * SceneClock::FRAME_NS);
}

/// Steps collected before their paint is due — the loop woken early — wait
/// for it, and are painted with the steps that came after them.
#[test]
fn steps_collected_early_are_painted_with_the_next() {
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let script = Script::shared([
        (Vec::new(), Status::Tracing(0)),
        (alloc::vec![lit(0, 0, 2)], Status::Tracing(0)),
        (alloc::vec![lit(46, 26, 2)], Status::Tracing(0)),
    ]);
    let mut saver = scripted(&script, false);
    let mut clock = ticking(MS);
    saver.advance(0, wm, &mut comp, &mut clock);
    comp.composite();
    let early = SceneClock::FRAME_NS / 2;
    saver.landed(early);
    saver.advance(early, wm, &mut comp, &mut clock);
    assert!(!comp.has_damage(), "not yet due");
    assert_eq!(saver.drawn.len(), 1);
    saver.advance(SceneClock::FRAME_NS, wm, &mut comp, &mut clock);
    let picture = content(&comp, wm);
    for (x, y) in [(0, 0), (46, 26)] {
        assert!(
            picture.get(x, y).is_some_and(|pixel| pixel.r > 0),
            "({x}, {y})"
        );
    }
}

/// What was collected of a refused scene and never painted goes with it: the
/// next scene begins over black.
#[test]
fn a_refused_scene_leaves_nothing_of_itself_to_the_next() {
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let script = Script::shared([
        (Vec::new(), Status::Tracing(0)),
        (alloc::vec![lit(0, 0, 2)], Status::Failed),
        (Vec::new(), Status::Tracing(0)),
    ]);
    let mut saver = scripted(&script, false);
    let mut clock = ticking(MS);
    saver.advance(0, wm, &mut comp, &mut clock);
    saver.landed(MS);
    saver.advance(MS, wm, &mut comp, &mut clock);
    assert!(matches!(saver.phase, Phase::Resting { .. }));
    let rested = saver.due_ns();
    saver.advance(rested, wm, &mut comp, &mut clock);
    assert!(matches!(saver.phase, Phase::Revealing));
    assert_eq!(script.borrow().asked, 1);
    saver.advance(saver.due_ns(), wm, &mut comp, &mut clock);
    assert_eq!(brightness(&comp, wm), 0, "nothing of the refused scene");
}

/// On the loop with no thread granted, a slice is traced each scene frame
/// while the paints keep the reveal's cadence, so steps traced between paints
/// wait for the next.
#[test]
fn on_the_loop_a_slice_is_traced_each_frame_and_painted_on_the_cadence() {
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let mut saver = launched(SIZE, false, idle(), None).expect("a reveal");
    let mut clock = ticking(MS);
    let mut now = 0;
    let mut waited = false;
    for _ in 0..100_000 {
        saver.advance(now, wm, &mut comp, &mut clock);
        if !matches!(saver.phase, Phase::Revealing) {
            break;
        }
        assert!(
            saver.due_ns() <= now + SceneClock::FRAME_NS,
            "a slice a frame"
        );
        waited |= !saver.drawn.is_empty();
        now = saver.due_ns();
    }
    assert!(matches!(saver.phase, Phase::Holding { .. }));
    assert!(waited, "steps traced between paints wait for the next");
}

/// The options the user chose — the share of the machine, whether pictures
/// are kept, and how much each scene sets out — are what the crew is
/// launched with.
#[test]
fn a_crew_is_launched_with_the_options_asked_for() {
    for cpu in CpuUse::ALL {
        for save in [false, true] {
            for detail in SceneDetail::ALL {
                let options = RaytraceOptions { cpu, save, detail };
                let script = Script::shared([]);
                let _saver = launched(SIZE, false, options, Some(&Scripted(Rc::clone(&script))))
                    .expect("a reveal");
                assert_eq!(script.borrow().launched, Some(options));
            }
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

    let mut alone = Engine::new(SIZE, seed_from(0), PLAIN).expect("an engine");
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
        let options = RaytraceOptions {
            cpu,
            ..RaytraceOptions::default()
        };
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
