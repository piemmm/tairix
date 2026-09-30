//! Host tests of the ray-traced screensaver as the serve loop runs it: what a
//! frame paints and repaints, the hold, the fade, the rest and the next scene,
//! a lost buffer, the share of the machine a reveal is traced on, and a whole
//! reveal ending as a reveal traced alone ends.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec::Vec;
use core::cell::RefCell;

use tairix_parallel::Reversed;
use tairix_raster::Pixel;
use tairix_raytrace::Block;
use tairix_wallpaper::CpuUse;
use tairix_wm::{Color, Compositor, Point, Surface, WindowId};

use super::{
    Engine, Phase, Raytrace, Request, Status, TraceHost, TraceLink, Traced, FADE_MS, HOLD_NS,
};
use crate::saver::{seed_from, SAVER_FRAME_NS};
use crate::tests::compositor;

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

/// A step painting `LIT` over a `width` by `height` block at `(x, y)`.
fn lit(x: u32, y: u32, width: u32, height: u32) -> Traced {
    Traced {
        block: Block {
            x,
            y,
            width,
            height,
        },
        pixel: LIT,
    }
}

/// What a scripted crew hands the loop, frame by frame, and what the loop
/// asked of it.
#[derive(Default)]
struct Script {
    frames: VecDeque<(Vec<Traced>, Status)>,
    asked: Vec<Request>,
    launched: Option<CpuUse>,
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
    fn launch(&self, _engine: Engine, cpu: CpuUse) -> Result<Box<dyn TraceLink>, Engine> {
        self.0.borrow_mut().launched = Some(cpu);
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
            None => Status::Working,
        }
    }

    fn request(&self, request: Request) {
        self.0.borrow_mut().asked.push(request);
    }
}

/// A saver traced by a crew playing `script`.
fn scripted(script: &Rc<RefCell<Script>>, calm: bool) -> Raytrace {
    Raytrace::new(
        SIZE,
        calm,
        0,
        CpuUse::Idle,
        Some(&Scripted(Rc::clone(script))),
    )
    .expect("a reveal")
}

/// A whole picture is held a minute, costing nothing meanwhile, then fades to
/// black, and the next scene is asked for and revealed.
#[test]
fn a_whole_picture_is_held_then_faded_then_the_next_scene_asked_for() {
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let script = Script::shared([(alloc::vec![lit(0, 0, SIZE.0, SIZE.1)], Status::Whole)]);
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
        now = saver.due_ns().max(now + SAVER_FRAME_NS);
    }
    assert_eq!(brightness(&comp, wm), 0, "faded to black");
    assert!(matches!(saver.phase, Phase::Revealing));
    assert_eq!(script.borrow().asked, [Request::Next]);
}

#[test]
fn under_reduced_motion_the_picture_is_cut_to_black() {
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let script = Script::shared([(alloc::vec![lit(0, 0, SIZE.0, SIZE.1)], Status::Whole)]);
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
    let script = Script::shared([(alloc::vec![lit(0, 0, 8, 8)], Status::Working)]);
    let host = Scripted(Rc::clone(&script));
    let mut saver = Raytrace::new(REFUSED, false, 0, CpuUse::Idle, Some(&host)).expect("a reveal");
    let mut clock = ticking(MS);
    saver.advance(0, wm, &mut comp, &mut clock);
    assert!(matches!(saver.phase, Phase::Resting { .. }));
    assert_eq!(saver.due_ns(), HOLD_NS);
    saver.advance(SAVER_FRAME_NS, wm, &mut comp, &mut clock);
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
        (alloc::vec![lit(0, 0, 8, 8)], Status::Working),
        (alloc::vec![lit(8, 8, 1, 1)], Status::Working),
    ]);
    let mut saver = scripted(&script, false);
    let mut clock = ticking(MS);
    saver.advance(0, wm, &mut comp, &mut clock);
    assert!(script.borrow().asked.is_empty());
    let _ = comp.set_surface(wm, Surface::new(4, 4).expect("a small surface"));
    saver.advance(SAVER_FRAME_NS, wm, &mut comp, &mut clock);
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

/// A frame paints each step's block whole and repaints those blocks and
/// nothing else while they are few, and the box they span once they are many.
#[test]
fn a_frame_paints_its_blocks_and_repaints_only_them() {
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let few: Vec<Traced> = (0..5).map(|at| lit(at * 3, at, 2, 3)).collect();
    let more: Vec<Traced> = (0..40).map(|at| lit(at, at % SIZE.1, 1, 1)).collect();
    let many: Vec<Traced> = (0..400)
        .map(|at| lit(at % SIZE.0, at / SIZE.0, 1, 1))
        .collect();
    let script = Script::shared([
        (few.clone(), Status::Working),
        (more.clone(), Status::Working),
        (many, Status::Working),
    ]);
    let mut saver = scripted(&script, false);
    let mut clock = ticking(MS);
    let mut now = 0;
    for steps in [few, more] {
        saver.advance(now, wm, &mut comp, &mut clock);
        now = saver.due_ns();
        let covered: u32 = saver
            .damage
            .rects()
            .iter()
            .map(|rect| rect.width * rect.height)
            .sum();
        let painted: u32 = steps
            .iter()
            .map(|traced| traced.block.width * traced.block.height)
            .sum();
        assert_eq!(covered, painted, "{:?}", saver.damage.rects());
        for traced in &steps {
            let block = traced.block;
            for y in block.y..block.y + block.height {
                for x in block.x..block.x + block.width {
                    let at = Point::new(
                        i32::try_from(x).expect("small"),
                        i32::try_from(y).expect("small"),
                    );
                    assert!(saver.damage.contains(at), "{at:?} not repainted");
                    assert_eq!(content(&comp, wm).get(x, y), Some(LIT));
                }
            }
        }
    }
    saver.advance(now, wm, &mut comp, &mut clock);
    assert_eq!(saver.damage.rects().len(), 1, "past the budget, one box");
    assert_eq!(saver.damage.rects()[0], saver.damage.bounds());
}

/// A frame that brought nothing repaints nothing.
#[test]
fn a_frame_with_nothing_traced_repaints_nothing() {
    let mut comp = compositor();
    let wm = canvas(&mut comp, Color::rgb(0, 0, 0));
    let script = Script::shared([(Vec::new(), Status::Working)]);
    let mut saver = scripted(&script, false);
    comp.composite();
    saver.advance(0, wm, &mut comp, &mut ticking(MS));
    assert!(!comp.has_damage());
    assert_eq!(saver.due_ns(), SAVER_FRAME_NS);
}

/// The share of the machine the user chose is what the crew is launched with.
#[test]
fn a_crew_is_launched_with_the_share_of_the_machine_asked_for() {
    for cpu in CpuUse::ALL {
        let script = Script::shared([]);
        let _saver = Raytrace::new(SIZE, false, 0, cpu, Some(&Scripted(Rc::clone(&script))))
            .expect("a reveal");
        assert_eq!(script.borrow().launched, Some(cpu));
    }
}

#[test]
fn a_screen_with_no_pixels_has_no_reveal() {
    assert!(Raytrace::new((0, 10), false, 0, CpuUse::Idle, None).is_none());
    assert!(Raytrace::new((10, 0), false, 0, CpuUse::Idle, None).is_none());
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
    let mut saver = Raytrace::new(SIZE, false, 0, CpuUse::Idle, None).expect("a reveal");
    reveal_whole(&mut saver, wm, &mut comp);

    let mut alone = Engine::new(SIZE, seed_from(0)).expect("an engine");
    let mut clock = ticking(MS);
    let mut steps = Vec::new();
    while alone.step(&tairix_parallel::SERIAL, &mut steps, &mut clock) == Status::Working {}
    let mut expected = Surface::new(SIZE.0, SIZE.1).expect("a surface");
    for traced in &steps {
        super::fill_block(&mut expected, traced);
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
        let mut saver = Raytrace::new(SIZE, false, 0, cpu, None).expect("a reveal");
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
