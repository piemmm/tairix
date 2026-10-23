//! Host tests of the ray-traced screensaver as its frames run it: what a
//! paint paints and marks and when paints come, a change fading in over the
//! wait until the next and laid straight on once the picture is formed or the
//! memory is wanted back, the readout over it, the hold, the fade, the rest
//! and the next scene, a whole picture handed over to keep, the options a
//! reveal is launched with, and a whole reveal ending as a reveal traced alone
//! ends.

extern crate std;

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::{Cell, RefCell};

use tairix_abi::saver::SaverFeeds;
use tairix_abi::saver_ipc::{SaverMessage, Start};
use tairix_abi::time::NANOS_PER_MILLI as MS;
use tairix_abi::APPINFO_FLAG_FEED_KEEP;
use tairix_geometry::{Point, Region};
use tairix_raster::{Pixel, Surface};
use tairix_raytrace::{Detail, Reveal, Setting, Step};
use tairix_reclaim::pressure::{ReportedPressure, Unpressured};
use tairix_reclaim::PressureBand;
use tairix_saver::testing::{self, Screen, FRAME_NS as TEST_SCENE_FRAME_NS};
use tairix_saver::{NextFrame, SaverValue};

use super::crossfade::{self, Crossfade};
#[cfg(feature = "scene-tests")]
use super::engine::PLAIN;
use tairix_abi::saver_ipc::SAVER_DAMAGE_RECTS;
use tairix_geometry::Tiles;

use super::TILE;
use super::{
    crew_painter, paint_wait, painter_workers, CpuUse, Engine, Feed, Laying, Memory, Phase,
    Raytrace, Status, TraceHost, TraceLink, Traced, DETAIL_WAIT_NS, FADE_MS, FIRST_WAIT_NS,
    HOLD_NS, MOST_WAIT_NS, READOUT_WAIT_NS,
};

const SIZE: (u32, u32) = (48, 27);
const LIT: Pixel = Pixel {
    r: 200,
    g: 180,
    b: 160,
    a: u8::MAX,
};

/// A whole scene's status.
const WHOLE: Status = Status::Whole(Setting::Classic);

/// What a test asks of a reveal: the share of the machine, whether pictures
/// are kept, and how much each scene sets out.
#[derive(Copy, Clone)]
struct Asked {
    cpu: &'static str,
    save: bool,
    detail: &'static str,
}

const IDLE: Asked = Asked {
    cpu: "idle",
    save: false,
    detail: "simple",
};

/// The start the host would send for a `size` screen, `calm` under reduced
/// motion, as `asked` asks, with the keeping feed.
fn start(size: (u32, u32), calm: bool, asked: Asked) -> Start {
    tairix_font::install_test_transport();
    let feeds = SaverFeeds::from_bits(APPINFO_FLAG_FEED_KEEP).expect("the keeping feed");
    let mut given = testing::start_fed(size.0, size.1, feeds);
    given.reduced_motion = calm;
    testing::give(&mut given, "cpu", testing::choice(asked.cpu));
    testing::give(&mut given, "save", SaverValue::Switch(asked.save));
    testing::give(&mut given, "detail", testing::choice(asked.detail));
    given
}

/// A clock that reads `step` later every time it is read.
fn ticking(step: u64) -> impl FnMut() -> u64 {
    let mut now = 0u64;
    move || {
        now += step;
        now
    }
}

/// A saver for `given` traced on `host`, or on its frames, in a process told
/// of the normal band, as `serve` tells it the band the host's start carries.
fn launched(given: &Start, host: Option<&dyn TraceHost>) -> Raytrace {
    tairix_rt::pressure::report(PressureBand::Normal);
    Raytrace::with_host(&testing::setup(given, 0), host, Box::new(ticking(MS))).expect("a reveal")
}

/// Draw `saver`'s frame at `now`, answering when it wants the next.
fn draw(screen: &mut Screen, saver: &mut Raytrace, now: u64) -> u64 {
    let (next, _) = screen.draw(saver, 0, now);
    let NextFrame::At(due) = next else {
        panic!("a reveal always names its next frame: {next:?}");
    };
    due
}

/// How bright the picture on `screen` is, the readout's place aside.
fn brightness(saver: &Raytrace, screen: &Screen) -> u64 {
    let place = saver.readout.place();
    let surface = &screen.surface;
    let mut sum = 0;
    for y in 0..surface.height() {
        for x in 0..surface.width() {
            if place.contains(at(x, y)) {
                continue;
            }
            let pixel = surface.get(x, y).expect("in bounds");
            sum += u64::from(pixel.r) + u64::from(pixel.g) + u64::from(pixel.b);
        }
    }
    sum
}

fn at(x: u32, y: u32) -> Point {
    Point::new(
        i32::try_from(x).expect("small"),
        i32::try_from(y).expect("small"),
    )
}

/// A step tracing `(x, y)` to `LIT`, a point of the grid of spacing `side`.
fn lit(x: u32, y: u32, side: u32) -> Traced {
    Traced {
        step: Step { x, y, side },
        pixel: LIT,
    }
}

/// Every step of a whole reveal of `size`, each traced to `LIT`.
fn every_step_lit(size: (u32, u32)) -> Vec<Traced> {
    let order = Reveal::new(size, 1).expect("a picture");
    (0..order.count())
        .map(|index| Traced {
            step: order.step(index).expect("a step"),
            pixel: LIT,
        })
        .collect()
}

/// A screen whose first pass is a sliver of its picture, so a reveal of it
/// starts at the shortest wait.
const SCREEN: (u32, u32) = (320, 180);

/// The steps `range` of a reveal of `SCREEN`, each traced to `LIT`.
fn screen_steps(range: core::ops::Range<u32>) -> Vec<Traced> {
    let order = Reveal::new(SCREEN, 1).expect("a picture");
    range
        .map(|index| Traced {
            step: order.step(index).expect("a step"),
            pixel: LIT,
        })
        .collect()
}

/// Every step of the first pass of a reveal of `SCREEN`, each traced to
/// `LIT`: a change to the whole picture.
fn first_pass_lit() -> Vec<Traced> {
    let order = Reveal::new(SCREEN, 1).expect("a picture");
    screen_steps(0..order.pass_end(0))
}

/// The change fading in, if one is.
fn fading(saver: &Raytrace) -> Option<&Crossfade> {
    match &saver.laying {
        Laying::Fading(fade) if !fade.settled() => Some(fade),
        _ => None,
    }
}

/// Carry `saver` on frame by frame from `now` until no change is fading in;
/// the time it then stands at.
fn faded_in(saver: &mut Raytrace, screen: &mut Screen, mut now: u64) -> u64 {
    for _ in 0..10_000 {
        if fading(saver).is_none() {
            return now;
        }
        now = saver.due_ns.max(now);
        draw(screen, saver, now);
    }
    panic!("the change never faded in");
}

/// What a scripted crew hands the frames, frame by frame, how many times the
/// frames asked it for the next scene, and what it was launched with.
#[derive(Default)]
struct Script {
    frames: VecDeque<(Vec<Traced>, Status)>,
    asked: usize,
    launched: Option<(CpuUse, Detail)>,
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
    fn launch(&self, engine: Engine, cpu: CpuUse) -> Result<Box<dyn TraceLink>, Engine> {
        self.0.borrow_mut().launched = Some((cpu, engine.asked()));
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

/// A saver of `size` traced by a crew playing `script`, and its screen.
fn scripted(script: &Rc<RefCell<Script>>, size: (u32, u32), calm: bool) -> (Raytrace, Screen) {
    let given = start(size, calm, IDLE);
    let saver = launched(&given, Some(&Scripted(Rc::clone(script))));
    (saver, Screen::new(given))
}

/// What the paints of `paints`, one batch each and laid straight on over
/// black, leave on `SCREEN`.
fn laid_straight(paints: &[Vec<Traced>]) -> Surface {
    let frames = paints
        .iter()
        .map(|steps| (steps.clone(), Status::Tracing(0)));
    let (mut saver, mut screen) = scripted(&Script::shared(frames), SCREEN, true);
    let mut now = 0;
    for _ in paints {
        now = draw(&mut screen, &mut saver, now);
    }
    assert!(matches!(saver.laying, Laying::Straight));
    screen.surface
}

/// A whole picture is held a minute, costing nothing meanwhile, then fades to
/// black, and the next scene is asked for and revealed.
#[test]
fn a_whole_picture_is_held_then_faded_then_the_next_scene_asked_for() {
    let script = Script::shared([(every_step_lit(SIZE), WHOLE)]);
    let (mut saver, mut screen) = scripted(&script, SIZE, false);
    assert_eq!(draw(&mut screen, &mut saver, 0), HOLD_NS);
    assert!(matches!(saver.phase, Phase::Holding { .. }));
    let lit = brightness(&saver, &screen);
    assert!(lit > 0);
    assert_eq!(draw(&mut screen, &mut saver, HOLD_NS / 2), HOLD_NS);
    assert!(screen.damage.is_empty(), "a held picture draws nothing");
    let mut now = draw(&mut screen, &mut saver, HOLD_NS);
    assert!(matches!(saver.phase, Phase::Fading { .. }));
    let half = HOLD_NS + u64::from(FADE_MS) * MS / 2;
    while now < half {
        now = draw(&mut screen, &mut saver, now);
    }
    let dimmed = brightness(&saver, &screen);
    assert!(
        dimmed < lit * 3 / 4 && dimmed > lit / 4,
        "{dimmed} of {lit} half way"
    );
    assert_eq!(script.borrow().asked, 0, "nothing asked while fading");
    while matches!(saver.phase, Phase::Fading { .. }) {
        let due = draw(&mut screen, &mut saver, now);
        now = due.max(now + TEST_SCENE_FRAME_NS);
    }
    assert_eq!(brightness(&saver, &screen), 0, "faded to black");
    assert!(matches!(saver.phase, Phase::Revealing));
    assert_eq!(script.borrow().asked, 1);
}

#[test]
fn under_reduced_motion_the_picture_is_cut_to_black() {
    let script = Script::shared([(every_step_lit(SIZE), WHOLE)]);
    let (mut saver, mut screen) = scripted(&script, SIZE, true);
    draw(&mut screen, &mut saver, 0);
    assert!(brightness(&saver, &screen) > 0);
    draw(&mut screen, &mut saver, HOLD_NS);
    assert_eq!(brightness(&saver, &screen), 0);
    assert!(matches!(saver.phase, Phase::Revealing));
    assert_eq!(script.borrow().asked, 1);
}

/// A scene the heap refused leaves the screen black a minute, asking for
/// nothing meanwhile, before the next is asked for.
#[test]
fn a_refused_scene_rests_the_screen_then_asks_for_the_next() {
    let script = Script::shared([(Vec::new(), Status::Failed)]);
    let (mut saver, mut screen) = scripted(&script, SIZE, false);
    assert_eq!(draw(&mut screen, &mut saver, 0), HOLD_NS);
    assert!(matches!(saver.phase, Phase::Resting { .. }));
    draw(&mut screen, &mut saver, HOLD_NS / 2);
    assert_eq!(script.borrow().asked, 0);
    draw(&mut screen, &mut saver, HOLD_NS);
    assert!(matches!(saver.phase, Phase::Revealing));
    assert_eq!(script.borrow().asked, 1);
}

/// A change marks the tiles about its steps and nothing else: no frame of its
/// fade writes beyond them, the readout's own place aside.
#[test]
fn a_change_marks_the_tiles_about_its_steps_and_only_them() {
    for calm in [true, false] {
        // Points of the last pass, far apart.
        let few: Vec<Traced> = [(21, 41), (101, 41), (181, 121), (261, 161)]
            .into_iter()
            .map(|(x, y)| lit(x, y, 1))
            .collect();
        let script = Script::shared([(few.clone(), Status::Tracing(0))]);
        let (mut saver, mut screen) = scripted(&script, SCREEN, calm);
        let black = screen.surface.clone();
        draw(&mut screen, &mut saver, 0);
        assert_eq!(fading(&saver).is_some(), !calm, "faded in unless calm");
        let mut marked = Region::new();
        let mut room = Tiles::new(SCREEN, TILE).expect("room");
        saver
            .preview
            .changed()
            .cover(SAVER_DAMAGE_RECTS, &mut room, &mut marked);
        let covered: u32 = marked
            .rects()
            .iter()
            .map(|rect| rect.width * rect.height)
            .sum();
        // A step of the last pass reaches two pixels each way, so touches at
        // most four of the finest tiles.
        assert!(covered <= 4 * 4 * 16 * 16, "{:?}", marked.rects());
        for traced in &few {
            let point = at(traced.step.x, traced.step.y);
            assert!(marked.contains(point), "{point:?} not repainted");
        }
        assert!(!marked.contains(at(160, 100)), "between them is left alone");
        assert!(!marked.contains(at(300, 10)), "far off is left alone");
        let place = saver.readout.place();
        let mut now = 0;
        loop {
            for y in 0..SCREEN.1 {
                for x in 0..SCREEN.0 {
                    let point = at(x, y);
                    if !marked.contains(point) && !place.contains(point) {
                        assert_eq!(screen.surface.get(x, y), black.get(x, y), "{point:?}");
                    }
                }
            }
            if fading(&saver).is_none() {
                break;
            }
            now = saver.due_ns.max(now);
            draw(&mut screen, &mut saver, now);
        }
        assert!(
            screen.surface.get(21, 41).is_some_and(|pixel| pixel.r > 0),
            "the change is shown"
        );
    }
}

/// Whether anything in the readout's place on `screen` is lettered over the
/// black it began on.
fn lettered(saver: &Raytrace, screen: &Screen) -> bool {
    let place = saver.readout.place();
    (0..screen.surface.height()).any(|y| {
        (0..screen.surface.width()).any(|x| {
            place.contains(at(x, y)) && screen.surface.get(x, y).is_some_and(|pixel| pixel.r > 0)
        })
    })
}

/// While the scene is prepared, and then traced, a readout over the picture
/// says how far it is, in the lower right, redrawn only when what it reads
/// changes; once the picture is whole it comes off, leaving the picture
/// beneath it.
#[test]
fn a_readout_tells_the_preparing_then_the_tracing_and_goes_once_whole() {
    let script = Script::shared([
        (Vec::new(), Status::Preparing(370)),
        (Vec::new(), Status::Preparing(371)),
        (alloc::vec![lit(0, 0, 2)], Status::Tracing(520)),
        (every_step_lit(SCREEN), WHOLE),
    ]);
    let (mut saver, mut screen) = scripted(&script, SCREEN, true);
    let place = saver.readout.place();
    let now = draw(&mut screen, &mut saver, 0);
    assert!(lettered(&saver, &screen), "the readout is lettered");
    assert_eq!(screen.damage.bounds(), place);
    let shown = screen.surface.clone();
    let now = draw(&mut screen, &mut saver, now);
    assert!(
        screen.damage.is_empty(),
        "37.1% reads as 37%: nothing is redrawn"
    );
    assert_eq!(screen.surface.pixels(), shown.pixels());
    let now = draw(&mut screen, &mut saver, now);
    assert!(
        screen.damage.bounds().contains(place.origin),
        "the tracing reads differently"
    );
    assert_ne!(screen.surface.pixels(), shown.pixels());
    draw(&mut screen, &mut saver, now);
    assert!(matches!(saver.phase, Phase::Holding { .. }));
    assert!(
        screen.surface.pixels().iter().all(|pixel| *pixel == LIT),
        "the readout is off, and the picture is whole beneath it"
    );
}

/// The picture painted beneath the readout while it is up is the picture's
/// own: lifting the readout off shows it untouched by any letter.
#[test]
fn a_picture_painted_under_the_readout_is_shown_when_it_comes_off() {
    let script = Script::shared([
        (Vec::new(), Status::Tracing(0)),
        (every_step_lit(SIZE), Status::Tracing(999)),
        (Vec::new(), WHOLE),
    ]);
    let (mut saver, mut screen) = scripted(&script, SIZE, true);
    let mut now = draw(&mut screen, &mut saver, 0);
    while !matches!(saver.phase, Phase::Holding { .. }) {
        now = draw(&mut screen, &mut saver, now);
    }
    assert!(
        screen.surface.pixels().iter().all(|pixel| *pixel == LIT),
        "no letter is left in the picture"
    );
}

/// On a screen of a real size the readout stands a margin in from the lower
/// right corner, in grey.
#[test]
fn the_readout_stands_in_the_lower_right_in_grey() {
    let screen_size = (1920u32, 1080u32);
    let script = Script::shared([(Vec::new(), Status::Preparing(0))]);
    let (mut saver, mut screen) = scripted(&script, screen_size, false);
    draw(&mut screen, &mut saver, 0);
    let place = saver.readout.place();
    let (right, bottom) = (
        u32::try_from(place.right()).expect("on screen"),
        u32::try_from(place.bottom()).expect("on screen"),
    );
    assert!(
        right < screen_size.0 && screen_size.0 - right <= 48,
        "{place:?}"
    );
    assert!(
        bottom < screen_size.1 && screen_size.1 - bottom <= 48,
        "{place:?}"
    );
    assert!(place.left() > 960, "{place:?}");
    let mut inked = 0;
    for y in 0..screen_size.1 {
        for x in 0..screen_size.0 {
            let pixel = screen.surface.get(x, y).expect("in bounds");
            if pixel.r == 0 {
                continue;
            }
            assert!(
                place.contains(at(x, y)),
                "ink at ({x}, {y}) outside the readout"
            );
            assert!(pixel.r == pixel.g && pixel.g == pixel.b, "grey ink");
            inked += 1;
        }
    }
    assert!(inked > 0);
}

/// A frame that brought nothing repaints nothing.
#[test]
fn a_frame_with_nothing_traced_repaints_nothing() {
    let script = Script::shared([
        (Vec::new(), Status::Tracing(0)),
        (Vec::new(), Status::Tracing(0)),
    ]);
    let (mut saver, mut screen) = scripted(&script, SIZE, false);
    let now = draw(&mut screen, &mut saver, 0);
    assert_eq!(draw(&mut screen, &mut saver, now), 2 * TEST_SCENE_FRAME_NS);
    assert!(screen.damage.is_empty());
}

/// The wait between paints grows with the share of the picture shown: a scene
/// frame until anything is, half a second while the coarse passes form it,
/// half the detail's wait half way through the 4 px pass and all of it as the
/// 2 px pass begins, never shrinking and never past the most.
#[test]
fn the_wait_between_paints_grows_with_the_picture_shown() {
    // A 1920 × 1080 picture's 4 px grid, and its 32 px grid.
    let detail_from = 480 * 270;
    let formed = 60 * 34;
    assert_eq!(
        paint_wait(0, detail_from, TEST_SCENE_FRAME_NS),
        TEST_SCENE_FRAME_NS
    );
    assert_eq!(
        paint_wait(0, detail_from, 2 * FIRST_WAIT_NS),
        FIRST_WAIT_NS,
        "a slow display's frame still shows the first steps within the first wait"
    );
    assert_eq!(
        paint_wait(1, detail_from, TEST_SCENE_FRAME_NS),
        FIRST_WAIT_NS
    );
    assert_eq!(
        paint_wait(formed, detail_from, TEST_SCENE_FRAME_NS),
        FIRST_WAIT_NS
    );
    assert_eq!(
        paint_wait(detail_from / 2, detail_from, TEST_SCENE_FRAME_NS),
        DETAIL_WAIT_NS / 2
    );
    assert_eq!(
        paint_wait(detail_from, detail_from, TEST_SCENE_FRAME_NS),
        DETAIL_WAIT_NS
    );
    assert_eq!(
        paint_wait(4 * detail_from, detail_from, TEST_SCENE_FRAME_NS),
        MOST_WAIT_NS
    );
    assert_eq!(
        paint_wait(u64::MAX, detail_from, TEST_SCENE_FRAME_NS),
        MOST_WAIT_NS
    );
    let mut last = 0;
    for shown in (1..=8 * detail_from).step_by(997) {
        let wait = paint_wait(shown, detail_from, TEST_SCENE_FRAME_NS);
        assert!(wait >= last, "{shown}: {wait} after {last}");
        assert!((FIRST_WAIT_NS..=MOST_WAIT_NS).contains(&wait));
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
        let due = (self.now.get() * self.per_frame / TEST_SCENE_FRAME_NS).min(u64::from(count));
        let due = u32::try_from(due).expect("within the reveal");
        for index in self.handed.get()..due {
            into.push(Traced {
                step: self.order.step(index).expect("a step"),
                pixel: LIT,
            });
        }
        self.handed.set(due);
        if due == count {
            WHOLE
        } else {
            Status::Tracing(0)
        }
    }

    fn next(&self) {}
}

/// A host launching one [`Steady`] crew.
struct SteadyHost(RefCell<Option<Steady>>);

impl TraceHost for SteadyHost {
    fn launch(&self, _engine: Engine, _cpu: CpuUse) -> Result<Box<dyn TraceLink>, Engine> {
        Ok(Box::new(self.0.borrow_mut().take().expect("launched once")))
    }
}

/// Traced at a steady pace, a reveal is painted half a second apart while its
/// coarse passes form the picture, then further apart in step with the share
/// shown — never closer than the last two, at least the detail's wait once
/// the fine detail is under way, and never more than the most — in a small
/// share of the frames it takes, ending on the whole picture.
#[test]
fn paints_come_half_a_second_apart_as_the_picture_forms_then_slow_to_the_most() {
    let now = Rc::new(Cell::new(0u64));
    let handed = Rc::new(Cell::new(0u32));
    let host = SteadyHost(RefCell::new(Some(Steady {
        order: Reveal::new(SCREEN, 7).expect("a picture"),
        per_frame: 40,
        now: Rc::clone(&now),
        handed: Rc::clone(&handed),
    })));
    let given = start(SCREEN, false, IDLE);
    let mut saver = launched(&given, Some(&host));
    let mut screen = Screen::new(given);
    let detail_from = saver.detail_from;
    assert_eq!(detail_from, 80 * 45, "every point of the 4 px grid");
    let mut paints = Vec::new();
    for _ in 0..10_000 {
        now.set(saver.due_ns);
        let shown = saver.shown;
        draw(&mut screen, &mut saver, now.get());
        if saver.shown > shown {
            paints.push((now.get(), saver.shown));
        }
        if !matches!(saver.phase, Phase::Revealing) {
            break;
        }
    }
    assert!(matches!(saver.phase, Phase::Holding { .. }));
    let frames = u64::from(SCREEN.0 * SCREEN.1) / 40;
    let count = u64::try_from(paints.len()).expect("few");
    assert!(count * 20 < frames, "{count} paints over {frames} frames");
    let mut last = 0;
    for pair in paints.windows(2) {
        let [(at, shown), (next, _)] = pair else {
            continue;
        };
        let wait = next - at;
        assert!(wait >= last && wait <= MOST_WAIT_NS, "{wait} after {last}");
        if *shown * 3 < detail_from {
            assert_eq!(wait, FIRST_WAIT_NS, "{shown} shown");
        }
        if *shown >= detail_from && *shown < u64::from(SCREEN.0 * SCREEN.1) {
            assert!(wait >= DETAIL_WAIT_NS, "{shown} shown: {wait}");
        }
        last = wait;
    }
    assert!(
        screen.surface.pixels().iter().all(|pixel| *pixel == LIT),
        "every pixel its own trace"
    );
}

/// A paint's change fades in a scene frame at a time over the wait until the
/// next paint, brighter every frame, from the black the reveal began over to
/// just what laying it straight on shows: all of it once the wait is up.
#[test]
fn a_paints_change_fades_in_over_the_wait_until_the_next_paint() {
    let first = first_pass_lit();
    let whole = laid_straight(core::slice::from_ref(&first));
    let script = Script::shared([(first, Status::Tracing(0))]);
    let (mut saver, mut screen) = scripted(&script, SCREEN, false);
    draw(&mut screen, &mut saver, 0);
    assert_eq!(brightness(&saver, &screen), 0, "nothing of it at once");
    let (mut now, mut last) = (0, 0);
    while fading(&saver).is_some() {
        let next = saver.due_ns;
        assert_eq!(
            next,
            (now + TEST_SCENE_FRAME_NS).min(FIRST_WAIT_NS),
            "a scene frame on"
        );
        now = next;
        draw(&mut screen, &mut saver, now);
        let lit = brightness(&saver, &screen);
        assert!(lit > last, "brighter at {now}");
        last = lit;
    }
    assert_eq!(now, FIRST_WAIT_NS, "all of it once the wait is up");
    assert_eq!(screen.surface.pixels(), whole.pixels());
}

/// The paints that leave less than a twentieth of the picture shown fade
/// their changes in; from there each is laid straight on, at once, and the
/// crossfade's room let go.
#[test]
fn past_a_twentieth_of_the_picture_changes_are_laid_straight_on() {
    let twentieth = SCREEN.0 * SCREEN.1 / 20;
    let paints = [
        screen_steps(0..twentieth / 2),
        screen_steps(twentieth / 2..twentieth + 10),
        screen_steps(twentieth + 10..twentieth + 400),
    ];
    let script = Script::shared([(paints[0].clone(), Status::Tracing(0))]);
    let (mut saver, mut screen) = scripted(&script, SCREEN, false);
    draw(&mut screen, &mut saver, 0);
    for (painted, steps) in paints.iter().enumerate().skip(1) {
        assert!(fading(&saver).is_some(), "paint {painted} fades in");
        let mut now = saver.due_ns;
        while now < saver.paint_due_ns {
            now = draw(&mut screen, &mut saver, now);
        }
        script
            .borrow_mut()
            .frames
            .push_back((steps.clone(), Status::Tracing(0)));
        draw(&mut screen, &mut saver, now);
    }
    assert!(
        matches!(saver.laying, Laying::Straight),
        "the room is let go"
    );
    assert_eq!(
        screen.surface.pixels(),
        laid_straight(&paints).pixels(),
        "laid on at once"
    );
}

/// A band wanting the memory back has the change fading in laid on whole at
/// the next frame and its room let go, and a reveal whose first change comes
/// under such a band lays it, and every change after, straight on.
#[test]
fn a_band_wanting_the_memory_back_ends_the_crossfade() {
    static TIGHTENING: ReportedPressure = ReportedPressure::unknown();
    static TIGHT: ReportedPressure = ReportedPressure::unknown();
    TIGHTENING.report(PressureBand::Normal);
    TIGHT.report(PressureBand::Mild);
    let first = first_pass_lit();
    let whole = laid_straight(core::slice::from_ref(&first));
    let script = Script::shared([(first.clone(), Status::Tracing(0))]);
    let (mut saver, mut screen) = scripted(&script, SCREEN, false);
    saver.memory = Memory {
        total: u64::MAX,
        gauge: &TIGHTENING,
    };
    draw(&mut screen, &mut saver, 0);
    draw(&mut screen, &mut saver, TEST_SCENE_FRAME_NS);
    assert!(fading(&saver).is_some());
    assert_ne!(screen.surface.pixels(), whole.pixels(), "part way in");
    TIGHTENING.report(PressureBand::Mild);
    draw(&mut screen, &mut saver, 2 * TEST_SCENE_FRAME_NS);
    assert!(matches!(saver.laying, Laying::Straight));
    assert_eq!(screen.surface.pixels(), whole.pixels(), "laid on whole");

    let script = Script::shared([(first, Status::Tracing(0))]);
    let (mut pressed, mut screen) = scripted(&script, SCREEN, false);
    pressed.memory = Memory {
        total: u64::MAX,
        gauge: &TIGHT,
    };
    draw(&mut screen, &mut pressed, 0);
    assert!(matches!(pressed.laying, Laying::Straight));
    assert_eq!(
        screen.surface.pixels(),
        whole.pixels(),
        "laid straight on at once"
    );
}

/// Frames coming slower than a change's fade runs still show it a step at a
/// time, brighter each frame, and the next paint waits until it is shown
/// whole: a slow frame rate lengthens the fade rather than losing it.
#[test]
fn a_fade_shown_at_a_slow_frame_rate_takes_its_frames_and_holds_the_next_paint() {
    let first = first_pass_lit();
    let painted = u64::try_from(first.len()).expect("small");
    let second = screen_steps(first.len().try_into().expect("small")..1_200);
    let script = Script::shared([(first, Status::Tracing(0)), (second, Status::Tracing(0))]);
    let (mut saver, mut screen) = scripted(&script, SCREEN, false);
    draw(&mut screen, &mut saver, 0);
    assert_eq!(saver.shown, painted);
    let slow = 3 * FIRST_WAIT_NS / 4;
    let (mut now, mut frames, mut last) = (0, 0u32, 0);
    while saver.shown == painted {
        now += slow;
        draw(&mut screen, &mut saver, now);
        frames += 1;
        assert!(frames < 100, "the next paint never came");
        if saver.shown == painted {
            assert!(fading(&saver).is_some(), "still fading in at {now}");
            let lit = brightness(&saver, &screen);
            assert!(lit > last, "brighter at {now}");
            last = lit;
        }
    }
    assert!(now > 2 * FIRST_WAIT_NS, "the paint was held past its wait");
    assert!(
        frames >= u32::from(crossfade::FADE_FRAMES),
        "{frames} frames"
    );
}

/// A reveal that could not hold a crossfade's room when its first change
/// came fades its next change in once the band lets the room go.
#[test]
fn a_reveal_fades_again_once_the_band_lets_the_room_go() {
    static EASING: ReportedPressure = ReportedPressure::unknown();
    EASING.report(PressureBand::Mild);
    let first = first_pass_lit();
    let second = screen_steps(first.len().try_into().expect("small")..1_200);
    let script = Script::shared([(first, Status::Tracing(0))]);
    let (mut saver, mut screen) = scripted(&script, SCREEN, false);
    saver.memory = Memory {
        total: u64::MAX,
        gauge: &EASING,
    };
    draw(&mut screen, &mut saver, 0);
    assert!(matches!(saver.laying, Laying::Straight), "laid straight on");
    let before = screen.surface.clone();
    EASING.report(PressureBand::Normal);
    script
        .borrow_mut()
        .frames
        .push_back((second, Status::Tracing(0)));
    let due = saver.paint_due_ns;
    draw(&mut screen, &mut saver, due);
    assert!(fading(&saver).is_some(), "the next change fades in");
    assert!(screen.surface == before, "nothing of it at once");
}

/// A saver whose share of memory cannot hold a crossfade's room lays every
/// change straight on.
#[test]
fn a_saver_too_small_for_the_room_lays_changes_straight_on() {
    let first = first_pass_lit();
    let script = Script::shared([(first.clone(), Status::Tracing(0))]);
    let (mut saver, mut screen) = scripted(&script, SCREEN, false);
    saver.memory = Memory {
        total: Crossfade::bytes(SCREEN) * 8,
        gauge: &Unpressured,
    };
    draw(&mut screen, &mut saver, 0);
    assert!(matches!(saver.laying, Laying::Straight));
    assert_eq!(screen.surface.pixels(), laid_straight(&[first]).pixels());
}

/// A scene refused once some of it is on screen rests the screen black, laid
/// straight on or fading in, and a fade's room goes with it.
#[test]
fn a_scene_refused_once_shown_rests_the_screen_black() {
    for calm in [true, false] {
        let script = Script::shared([
            (first_pass_lit(), Status::Tracing(0)),
            (Vec::new(), Status::Tracing(0)),
            (Vec::new(), Status::Failed),
        ]);
        let (mut saver, mut screen) = scripted(&script, SCREEN, calm);
        let mut now = draw(&mut screen, &mut saver, 0);
        now = draw(&mut screen, &mut saver, now);
        assert!(brightness(&saver, &screen) > 0, "some of it is shown");
        draw(&mut screen, &mut saver, now);
        assert!(matches!(saver.phase, Phase::Resting { .. }));
        assert!(matches!(saver.laying, Laying::Straight));
        assert_eq!(brightness(&saver, &screen), 0, "black while it rests");
        assert!(!lettered(&saver, &screen), "and the readout is off");
    }
}

/// While a scene is prepared on its thread the frames come back at the
/// readout's cadence, so a scene readied meanwhile is collected within it, and
/// its first steps fade in from the frame after.
#[test]
fn a_scene_readied_on_its_thread_is_collected_at_the_readouts_cadence() {
    let script = Script::shared([
        (Vec::new(), Status::Preparing(500)),
        (Vec::new(), Status::Tracing(0)),
        (alloc::vec![lit(0, 0, 2)], Status::Tracing(0)),
    ]);
    let (mut saver, mut screen) = scripted(&script, SIZE, false);
    assert_eq!(draw(&mut screen, &mut saver, 0), READOUT_WAIT_NS);
    let readied = draw(&mut screen, &mut saver, READOUT_WAIT_NS);
    assert_eq!(readied, READOUT_WAIT_NS + TEST_SCENE_FRAME_NS);
    let next = draw(&mut screen, &mut saver, readied);
    assert!(fading(&saver).is_some(), "the first step is fading in");
    assert_eq!(next, readied + TEST_SCENE_FRAME_NS);
    draw(&mut screen, &mut saver, next);
    assert!(
        screen
            .surface
            .get(0, 0)
            .is_some_and(|pixel| pixel.r > 0 && pixel.r < LIT.r),
        "the first step is showing, but not yet all of it"
    );
}

/// Steps collected while a change fades in, before their paint is due, wait
/// for it, and are painted with what comes after them.
#[test]
fn steps_collected_early_are_painted_with_the_next() {
    let script = Script::shared([
        (alloc::vec![lit(0, 0, 2)], Status::Tracing(0)),
        (alloc::vec![lit(46, 26, 2)], Status::Tracing(0)),
    ]);
    let (mut saver, mut screen) = scripted(&script, SIZE, false);
    let mut now = draw(&mut screen, &mut saver, 0);
    assert!(fading(&saver).is_some());
    draw(&mut screen, &mut saver, now);
    assert_eq!(saver.drawn.len(), 1, "not yet due");
    while saver.paint_due_ns > now {
        now = draw(&mut screen, &mut saver, now);
    }
    draw(&mut screen, &mut saver, now);
    assert!(saver.drawn.is_empty(), "painted");
    faded_in(&mut saver, &mut screen, now);
    for (x, y) in [(0, 0), (46, 26)] {
        assert!(
            screen.surface.get(x, y).is_some_and(|pixel| pixel.r > 0),
            "({x}, {y})"
        );
    }
}

/// What was collected of a refused scene and never painted goes with it: the
/// next scene begins over black.
#[test]
fn a_refused_scene_leaves_nothing_of_itself_to_the_next() {
    let script = Script::shared([
        (Vec::new(), Status::Tracing(0)),
        (alloc::vec![lit(0, 0, 2)], Status::Failed),
        (Vec::new(), Status::Tracing(0)),
    ]);
    let (mut saver, mut screen) = scripted(&script, SIZE, false);
    let now = draw(&mut screen, &mut saver, 0);
    let rested = draw(&mut screen, &mut saver, now);
    assert!(matches!(saver.phase, Phase::Resting { .. }));
    let next = draw(&mut screen, &mut saver, rested);
    assert!(matches!(saver.phase, Phase::Revealing));
    assert_eq!(script.borrow().asked, 1);
    draw(&mut screen, &mut saver, next);
    assert_eq!(
        brightness(&saver, &screen),
        0,
        "nothing of the refused scene"
    );
}

/// The options the user chose — the share of the machine and how much each
/// scene sets out — are what the crew is launched with, and pictures are
/// kept only where the user asked and the keeping feed was given.
#[test]
fn a_crew_is_launched_with_the_options_asked_for() {
    for (cpu, word) in [(CpuUse::Idle, "idle"), (CpuUse::Performance, "performance")] {
        for save in [false, true] {
            for (detail, spelled) in [(Detail::Simple, "simple"), (Detail::Maximum, "maximum")] {
                let asked = Asked {
                    cpu: word,
                    save,
                    detail: spelled,
                };
                let script = Script::shared([]);
                let saver = launched(
                    &start(SIZE, false, asked),
                    Some(&Scripted(Rc::clone(&script))),
                );
                assert_eq!(script.borrow().launched, Some((cpu, detail)));
                assert_eq!(saver.keeping, save);
            }
        }
    }
    let mut unfed = start(SIZE, false, Asked { save: true, ..IDLE });
    unfed.feeds = SaverFeeds::NONE;
    let saver = launched(&unfed, None);
    assert!(!saver.keeping, "no feed, nothing kept");
}

/// On the frames with no thread granted, the idle setting traces serially and
/// performance across a pool of its own.
#[test]
fn on_the_frames_idle_traces_serially_and_performance_on_a_pool() {
    for (word, pooled) in [("idle", false), ("performance", true)] {
        let saver = launched(&start(SIZE, false, Asked { cpu: word, ..IDLE }), None);
        assert!(
            matches!(&saver.feed, Feed::Inline { runner, .. } if runner.is_some() == pooled),
            "{word}"
        );
    }
}

/// A whole picture is handed to the host to keep, once, under its setting's
/// name: the picture as traced, the readout off it.
#[test]
fn a_whole_picture_is_handed_to_the_host_to_keep_once() {
    let given = start(SIZE, false, Asked { save: true, ..IDLE });
    let script = Script::shared([
        (Vec::new(), Status::Tracing(0)),
        (every_step_lit(SIZE), WHOLE),
    ]);
    let mut saver = launched(&given, Some(&Scripted(Rc::clone(&script))));
    let mut screen = Screen::new(given);
    let now = draw(&mut screen, &mut saver, 0);
    let (_, asked) = screen.draw(&mut saver, 0, now);
    let [SaverMessage::Keep {
        title,
        width,
        height,
    }] = asked.as_slice()
    else {
        panic!("one picture to keep: {asked:?}");
    };
    assert_eq!(title.as_str(), Setting::Classic.name());
    assert_eq!((*width, *height), SIZE);
    let kept = screen.keep(&mut saver, SIZE.0, SIZE.1, true);
    assert!(
        kept.pixels().iter().all(|pixel| *pixel == LIT),
        "no letter in it"
    );
    let (_, asked) = screen.draw(&mut saver, 0, now + MS);
    assert!(asked.is_empty(), "kept once");
}

/// A picture finished while the host still holds the last one is handed over
/// at the frame the host's answer brings, while it is still held whole.
#[test]
fn a_picture_the_host_cannot_take_yet_is_handed_over_once_it_can() {
    let given = start(SIZE, true, Asked { save: true, ..IDLE });
    let script = Script::shared([(every_step_lit(SIZE), WHOLE)]);
    let mut saver = launched(&given, Some(&Scripted(Rc::clone(&script))));
    let mut screen = Screen::new(given);
    let (_, asked) = screen.draw(&mut saver, 0, 0);
    assert_eq!(asked.len(), 1, "the first is handed over");
    // Cut to black and revealed again, the host never having answered.
    draw(&mut screen, &mut saver, HOLD_NS);
    script
        .borrow_mut()
        .frames
        .push_back((every_step_lit(SIZE), WHOLE));
    let (_, asked) = screen.draw(&mut saver, 0, HOLD_NS + MS);
    assert!(matches!(saver.phase, Phase::Holding { .. }));
    assert!(asked.is_empty(), "the feed area is still the host's");
    screen.keep(&mut saver, SIZE.0, SIZE.1, true);
    let (_, asked) = screen.draw(&mut saver, 0, HOLD_NS + 2 * MS);
    assert!(
        matches!(asked.as_slice(), [SaverMessage::Keep { .. }]),
        "handed over at the answer's frame: {asked:?}"
    );
}

/// A saver not asked to keep pictures hands none over.
#[test]
fn a_saver_not_asked_to_keep_hands_nothing_over() {
    let script = Script::shared([(every_step_lit(SIZE), WHOLE)]);
    let (mut saver, mut screen) = scripted(&script, SIZE, false);
    let (_, asked) = screen.draw(&mut saver, 0, 0);
    assert!(asked.is_empty());
    assert!(matches!(saver.phase, Phase::Holding { .. }));
}

/// On the frames with no thread granted, a slice is traced each scene frame
/// while the paints keep the reveal's cadence, so steps traced between paints
/// wait for the next.
#[cfg(feature = "scene-tests")]
#[test]
fn on_the_frames_a_slice_is_traced_each_frame_and_painted_on_the_cadence() {
    let given = start(SIZE, false, IDLE);
    let mut saver = launched(&given, None);
    let mut screen = Screen::new(given);
    let mut now = 0;
    let mut waited = false;
    for _ in 0..100_000 {
        let due = draw(&mut screen, &mut saver, now);
        if !matches!(saver.phase, Phase::Revealing) {
            break;
        }
        assert!(due <= now + TEST_SCENE_FRAME_NS, "a slice a frame");
        waited |= !saver.drawn.is_empty();
        now = due;
    }
    assert!(matches!(saver.phase, Phase::Holding { .. }));
    assert!(waited, "steps traced between paints wait for the next");
}

/// Traced on the frames with no thread granted, the reveal ends showing
/// exactly what the same scene's reveal, traced alone and painted step by
/// step, shows.
#[cfg(feature = "scene-tests")]
#[test]
fn a_reveal_on_the_frames_ends_as_the_reveal_traced_alone() {
    let given = start(SIZE, false, IDLE);
    let mut saver = launched(&given, None);
    let mut screen = Screen::new(given);
    let mut now = 0;
    for _ in 0..100_000 {
        let due = draw(&mut screen, &mut saver, now);
        if matches!(saver.phase, Phase::Holding { .. }) {
            break;
        }
        now = due.max(now);
    }
    assert!(matches!(saver.phase, Phase::Holding { .. }));

    let slice = tairix_theme::motion::work_slice_ns(testing::TEST_REFRESH);
    let mut alone = Engine::new(SIZE, 0, PLAIN, slice).expect("an engine");
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
    assert_eq!(screen.surface.pixels(), expected.pixels());
}

/// Beside a tracing crew the frames paint across the threads it leaves them:
/// under idle every one granted past the frames' and the tracer's, under
/// performance none, and never more than were granted.
#[test]
fn a_crew_leaves_the_frames_the_threads_it_does_not_trace_on() {
    assert_eq!(
        painter_workers(4, CpuUse::Idle),
        2,
        "the tracer's thread is left to it"
    );
    assert_eq!(painter_workers(16, CpuUse::Idle), 14);
    assert_eq!(painter_workers(4, CpuUse::Performance), 0);
    assert_eq!(
        painter_workers(2, CpuUse::Idle),
        0,
        "the frames and the tracer hold both"
    );
    assert_eq!(painter_workers(1, CpuUse::Idle), 0);
    let mut given = start(SIZE, false, IDLE);
    (given.threads, given.cores) = (8, 3);
    let setup = testing::setup(&given, 0);
    assert_eq!(
        painter_workers(setup.threads(), CpuUse::Idle),
        1,
        "never more than the machine's cores"
    );
    given.threads = 2;
    let setup = testing::setup(&given, 0);
    assert!(crew_painter(&setup, CpuUse::Idle).is_none());
}
