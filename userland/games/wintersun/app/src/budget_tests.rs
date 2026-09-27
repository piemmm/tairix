//! The budget adds up, and `auto` reads the machine over seconds: it rides
//! out a moment of other work, sheds a notch at a time where the detail is
//! too dear, stops where frames fit rather than at the bottom, and gives
//! notches back on a prediction it is prepared to be wrong about.

use alloc::vec::Vec;

use super::*;
use crate::quality::Rung;

/// A frame where every pass spent exactly its allocation.
fn on_budget() -> FrameTimes {
    let mut times = FrameTimes::new();
    for pass in Pass::ALL {
        times.record(pass, pass.budget_ns());
    }
    times
}

/// The window the budget is stated at.
const BASELINE: (u32, u32) = (BASELINE_WIDTH, BASELINE_HEIGHT);

/// A window of two and a quarter times the baseline's pixels.
const LARGER: (u32, u32) = (1920, 1080);

/// Pixels in the baseline window.
const BASELINE_PIXELS: u64 = 1280 * 720;

/// A simulated machine: what a pixel costs it at full detail when nothing
/// else wants it, and how much slower than that it is drawing now.
#[derive(Copy, Clone)]
struct Machine {
    /// Picoseconds a render pixel at full detail, at the machine's best.
    full_ps: u64,
    /// How slowly it draws against its best, in percent.
    busy: u64,
}

impl Machine {
    /// A machine that draws the baseline window at full detail in `ms`.
    const fn drawing_baseline_in(ms: u64) -> Self {
        Self {
            full_ps: ms * 1_000_000_000 / BASELINE_PIXELS,
            busy: 100,
        }
    }

    /// This machine drawing at `busy` percent of its best.
    const fn busy(self, busy: u64) -> Self {
        Self { busy, ..self }
    }

    /// What a frame at `ladder` in `window` costs it: each lighting and
    /// shadow notch a twenty-fifth cheaper a pixel, and the render scale
    /// cheaper by the pixels it drops.
    fn frame(self, ladder: Ladder, window: (u32, u32)) -> FrameTimes {
        let pixels = render_pixels(window, ladder).expect("a window with pixels");
        let shed = u64::from(ladder.step().min(4));
        let ps = self.full_ps * (100 - 4 * shed) / 100 * self.busy / 100;
        let mut times = FrameTimes::new();
        times.record(Pass::Terrain, pixels * ps / 1000);
        times
    }
}

/// A governor fed a frame every sixtieth of a second, and every move it
/// made.
struct Sim {
    governor: Governor,
    window: (u32, u32),
    now_ns: u64,
    moves: Vec<(u64, Ladder)>,
}

impl Sim {
    fn new(window: (u32, u32)) -> Self {
        Self {
            governor: Governor::new(),
            window,
            now_ns: 0,
            moves: Vec::new(),
        }
    }

    /// Draw `ns` of frames on `machine`.
    fn run(&mut self, machine: Machine, ns: u64) {
        let end = self.now_ns + ns;
        while self.now_ns < end {
            self.now_ns += FRAME_NS;
            let times = machine.frame(self.governor.ladder(), self.window);
            if self.governor.observe(&times, self.window, self.now_ns) {
                self.moves.push((self.now_ns, self.governor.ladder()));
            }
        }
    }

    /// The moves made from `since` on.
    fn moves_since(&self, since: u64) -> Vec<(u64, Ladder)> {
        self.moves
            .iter()
            .copied()
            .filter(|&(at, _)| at >= since)
            .collect()
    }

    /// What the current step costs `machine` now.
    fn cost(&self, machine: Machine) -> u64 {
        machine.frame(self.governor.ladder(), self.window).total()
    }
}

/// Whether each move went exactly one notch from the step before it.
fn one_notch_at_a_time(start: Ladder, moves: &[(u64, Ladder)]) -> bool {
    let mut at = start;
    moves.iter().all(|&(_, next)| {
        let one = at.shed() == Some(next) || at.restore() == Some(next);
        at = next;
        one
    })
}

/// The shortest gap between consecutive moves.
fn closest(moves: &[(u64, Ladder)]) -> u64 {
    moves
        .windows(2)
        .map(|pair| pair[1].0 - pair[0].0)
        .min()
        .unwrap_or(u64::MAX)
}

#[test]
fn the_passes_and_the_headroom_are_the_whole_frame() {
    let drawing: u64 = Pass::ALL.iter().map(|p| p.budget_ns()).sum();
    assert_eq!(drawing + headroom_ns(), FRAME_NS);
    assert_eq!(drawing, drawing_ns());
    assert!(
        headroom_ns() > 0,
        "a frame with no headroom cannot be presented"
    );
    assert_eq!(FRAME_NS, 16_666_666, "sixty frames a second");
    assert_eq!((BASELINE_WIDTH, BASELINE_HEIGHT), (1280, 720));
}

#[test]
fn a_frame_on_budget_fits_and_names_no_overrun() {
    let times = on_budget();
    assert!(times.within_budget());
    assert_eq!(times.overruns().count(), 0);
    assert_eq!(times.total(), FRAME_NS - headroom_ns());
}

#[test]
fn an_overrunning_pass_is_named_with_what_it_cost() {
    let mut times = FrameTimes::new();
    times.record(Pass::Terrain, Pass::Terrain.budget_ns() + 1);
    times.record(Pass::Light, Pass::Light.budget_ns());
    let named: Vec<_> = times.overruns().collect();
    assert_eq!(
        named,
        alloc::vec![(Pass::Terrain, Pass::Terrain.budget_ns() + 1)]
    );
}

#[test]
fn synthesising_tiles_is_not_a_cost_of_drawing() {
    let mut times = on_budget();
    times.record_warm(80_000_000);
    assert_eq!(times.warm(), 80_000_000);
    assert_eq!(times.total(), drawing_ns(), "the passes alone are the cost");
    assert!(times.within_budget());
}

#[test]
fn the_governor_starts_at_full_quality() {
    assert_eq!(Governor::new().ladder(), Ladder::FULL);
    assert_eq!(Governor::default(), Governor::new());
}

#[test]
fn a_moment_of_other_work_sheds_nothing() {
    let machine = Machine::drawing_baseline_in(9);
    let mut sim = Sim::new(BASELINE);
    sim.run(machine, 10 * NS_PER_SEC);
    // Something else takes the machine for three seconds and every frame
    // runs at more than four times its best.
    sim.run(machine.busy(450), 3 * NS_PER_SEC);
    sim.run(machine, 10 * NS_PER_SEC);
    assert!(sim.moves.is_empty(), "shed on a moment: {:?}", sim.moves);
    assert_eq!(sim.governor.ladder(), Ladder::FULL);
}

#[test]
fn a_busy_machine_that_has_drawn_this_well_holds_on_for_seconds() {
    let machine = Machine::drawing_baseline_in(9);
    let mut sim = Sim::new(BASELINE);
    sim.run(machine, 10 * NS_PER_SEC);
    let busy_from = sim.now_ns;
    // Frames now overrun for good: the machine is busy, not incapable.
    sim.run(machine.busy(180), SHED_BUSY_NS);
    assert!(
        sim.moves.is_empty(),
        "a machine that has shown it can keep up shed within {} s",
        SHED_BUSY_NS / NS_PER_SEC
    );
    sim.run(machine.busy(180), 3 * NS_PER_SEC);
    let moves = sim.moves_since(busy_from);
    assert_eq!(moves.len(), 1, "one notch, and only one: {moves:?}");
    assert_eq!(moves[0].1.rung(), Rung::LightResolution);
}

#[test]
fn detail_too_dear_for_the_machine_goes_a_notch_at_a_time_and_stops_where_frames_fit() {
    // Twice the drawing budget at full detail: nothing it has drawn fits.
    let machine = Machine::drawing_baseline_in(27);
    let mut sim = Sim::new(BASELINE);
    sim.run(machine, 30 * NS_PER_SEC);
    assert!(
        one_notch_at_a_time(Ladder::FULL, &sim.moves),
        "{:?}",
        sim.moves
    );
    assert!(
        closest(&sim.moves) >= SHED_UNABLE_NS,
        "two notches went {} ms apart",
        closest(&sim.moves) / 1_000_000
    );
    let settled = sim.governor.ladder();
    assert!(
        sim.cost(machine) <= drawing_ns(),
        "stopped before frames fit"
    );
    let finer = settled.restore().expect("something was shed");
    assert!(
        machine.frame(finer, BASELINE).total() > drawing_ns(),
        "shed a notch it did not need to"
    );
    assert!(settled.step() < Ladder::MAX_STEP, "went to the bottom");
}

#[test]
fn a_larger_window_does_not_send_the_ladder_to_the_bottom() {
    let machine = Machine::drawing_baseline_in(9);
    let mut sim = Sim::new(BASELINE);
    sim.run(machine, 10 * NS_PER_SEC);
    assert_eq!(sim.governor.ladder(), Ladder::FULL);

    let resized = sim.now_ns;
    sim.window = LARGER;
    sim.run(machine, 30 * NS_PER_SEC);
    let moves = sim.moves_since(resized);
    assert!(one_notch_at_a_time(Ladder::FULL, &moves), "{moves:?}");
    let settled = sim.governor.ladder();
    assert!(
        sim.cost(machine) <= drawing_ns(),
        "the frames still overrun"
    );
    let finer = settled.restore().expect("the larger window cost a notch");
    assert!(
        machine.frame(finer, LARGER).total() > drawing_ns(),
        "shed further than the window needed"
    );
    assert!(settled.step() < Ladder::MAX_STEP);

    // The window goes back, and so do the notches, one at a time.
    let shrunk = sim.now_ns;
    sim.window = BASELINE;
    sim.run(machine, 60 * NS_PER_SEC);
    let moves = sim.moves_since(shrunk);
    assert!(one_notch_at_a_time(settled, &moves), "{moves:?}");
    assert!(
        closest(&moves) >= RESTORE_AFTER_NS,
        "two notches came back {} ms apart",
        closest(&moves) / 1_000_000
    );
    assert_eq!(sim.governor.ladder(), Ladder::FULL);
}

#[test]
fn the_frames_that_pay_for_a_resize_are_not_counted() {
    let machine = Machine::drawing_baseline_in(9);
    let mut sim = Sim::new(BASELINE);
    sim.run(machine, 10 * NS_PER_SEC);
    sim.window = (1280, 800);
    // The first frames at the new size allocate its buffers.
    for _ in 0..SETTLE_FRAMES {
        sim.now_ns += NS_PER_SEC / 8;
        let mut stalled = FrameTimes::new();
        stalled.record(Pass::Terrain, 120_000_000);
        assert!(!sim.governor.observe(&stalled, sim.window, sim.now_ns));
    }
    sim.run(machine, 10 * NS_PER_SEC);
    assert!(sim.moves.is_empty(), "{:?}", sim.moves);
}

#[test]
fn a_notch_given_back_that_overruns_is_taken_away_within_its_trial() {
    // Full detail is far dearer than the next step down here, so the
    // governor sheds to the next step and stays.
    let mut sim = Sim::new(BASELINE);
    let costs = |ladder: Ladder| -> u64 {
        if ladder == Ladder::FULL {
            20_000_000
        } else {
            5_000_000
        }
    };
    let frame = |sim: &mut Sim, cost: u64| {
        sim.now_ns += FRAME_NS;
        let mut times = FrameTimes::new();
        times.record(Pass::Terrain, cost);
        if sim.governor.observe(&times, sim.window, sim.now_ns) {
            sim.moves.push((sim.now_ns, sim.governor.ladder()));
        }
    };
    while sim.now_ns < 5 * NS_PER_SEC {
        let cost = costs(sim.governor.ladder());
        frame(&mut sim, cost);
    }
    assert_eq!(sim.governor.ladder(), Ladder::new(1));

    // Its evidence against full detail ages, until the cheap step beneath
    // it says full detail should now fit, and it is tried.
    let aged_from = sim.now_ns;
    while sim.governor.ladder() != Ladder::FULL {
        let cost = costs(sim.governor.ladder());
        frame(&mut sim, cost);
        assert!(
            sim.now_ns - aged_from < 10 * EVIDENCE_TAU_NS,
            "never tried again"
        );
    }
    let tried = sim.now_ns;
    assert!(
        tried - aged_from >= EVIDENCE_TAU_NS,
        "tried again before the evidence against it had aged"
    );

    // It still overruns, so the trial ends it within a second or two, and
    // the fresh evidence keeps it from being tried again soon.
    while sim.now_ns - tried < EVIDENCE_TAU_NS {
        let cost = costs(sim.governor.ladder());
        frame(&mut sim, cost);
    }
    let after = sim.moves_since(tried + 1);
    assert_eq!(after.len(), 1, "{after:?}");
    assert_eq!(after[0].1, Ladder::new(1));
    assert!(after[0].0 - tried <= SETTLE_NS + SHED_UNABLE_NS + RECENT_TAU_NS);
}

#[test]
fn shedding_stops_at_the_floor_and_says_so() {
    let machine = Machine::drawing_baseline_in(40);
    let mut sim = Sim::new(BASELINE);
    let floor = Ladder::new(2);
    assert!(
        !sim.governor.hold(floor),
        "a floor below the ladder moved it"
    );
    sim.run(machine, 20 * NS_PER_SEC);
    assert_eq!(sim.governor.ladder(), floor);
    assert!(
        sim.governor.floored(),
        "an overrun at the floor went unreported"
    );
}

#[test]
fn a_floor_that_rises_takes_the_ladder_back_before_the_next_frame() {
    let machine = Machine::drawing_baseline_in(40);
    let mut sim = Sim::new(BASELINE);
    sim.run(machine, 30 * NS_PER_SEC);
    let deep = sim.governor.ladder();
    assert!(deep.step() > 2);

    let risen = Ladder::new(2);
    assert!(sim.governor.hold(risen), "the ladder stayed past its floor");
    assert_eq!(sim.governor.ladder(), risen);
    assert!(
        !sim.governor.floored(),
        "a moved ladder is not the one that floored"
    );
    assert!(
        !sim.governor.hold(risen),
        "holding the same floor moved it again"
    );
}

#[test]
fn a_floor_that_falls_lets_shedding_resume() {
    let machine = Machine::drawing_baseline_in(40);
    let mut sim = Sim::new(BASELINE);
    sim.governor.hold(Ladder::new(1));
    sim.run(machine, 10 * NS_PER_SEC);
    assert!(sim.governor.floored());

    assert!(
        !sim.governor.hold(Ladder::new(6)),
        "a deeper floor moved the ladder"
    );
    assert!(
        !sim.governor.floored(),
        "the ladder is no longer at its floor"
    );
    sim.run(machine, 5 * NS_PER_SEC);
    assert!(sim.governor.ladder() > Ladder::new(1));
}

#[test]
fn restarting_goes_back_to_full_detail() {
    let machine = Machine::drawing_baseline_in(27);
    let mut sim = Sim::new(BASELINE);
    sim.run(machine, 20 * NS_PER_SEC);
    assert!(sim.governor.ladder() > Ladder::FULL);
    sim.governor.restart();
    assert_eq!(sim.governor.ladder(), Ladder::FULL);
    assert!(!sim.governor.floored());
}

#[test]
fn smoothing_follows_time_not_frame_count() {
    let step = smoothed(0, 1000, NS_PER_SEC, NS_PER_SEC);
    assert_eq!(step, 500, "one time constant goes halfway");
    assert_eq!(smoothed(700, 1000, 0, NS_PER_SEC), 700, "no time, no move");
    assert_eq!(smoothed(700, 1000, 0, 0), 1000);
    assert_eq!(
        scaled(u64::MAX, 2, 1),
        u64::MAX,
        "saturates rather than wraps"
    );
    assert_eq!(
        cost(per_pixel(9_000_000, BASELINE_PIXELS), BASELINE_PIXELS),
        9_000_000,
        "a cost survives the round trip through a per-pixel rate"
    );
}

/// The model found this: overruns before a pause, and a pause long enough to
/// pass for having overrun throughout it, shed a notch on the comfortable
/// frames after.
#[test]
fn a_pause_is_neither_overrunning_nor_comfortable() {
    let mut sim = Sim::new(BASELINE);
    let heavy = Machine::drawing_baseline_in(40);
    // Long enough past the first frames' settling for half a second of
    // overruns to be counted, and short of what sheds a notch.
    sim.run(heavy, NS_PER_SEC);
    assert!(sim.moves.is_empty());
    // Nothing drawn for four and a half seconds: minimized, or descheduled.
    sim.now_ns += 9 * NS_PER_SEC / 2;
    let light = Machine::drawing_baseline_in(1);
    sim.run(light, 3 * NS_PER_SEC);
    assert!(sim.moves.is_empty(), "{:?}", sim.moves);
}
