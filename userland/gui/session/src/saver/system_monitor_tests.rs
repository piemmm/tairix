//! Host tests of the System Monitor: what it says before, while and after
//! readings come, what a reading repaints, and that it moves only at the
//! minute.

use tairix_abi::switchboard_ipc::{MachineInterface, MachineInterfaceName, MachineNetwork};
use tairix_theme::Theme;
use tairix_wallpaper::SystemMonitorOptions;
use tairix_wm::{Color, Compositor, Point, Rect, Region, Surface, WindowId};

use super::board::Part;
use super::fixtures::{report, PERIOD_NS};
use super::verdict::Verdict;
use super::{SystemMonitor, FIRST_READING_NS, STALE_PERIODS};
use crate::saver::clock::SaverIdentity;
use crate::saver::telling::fixtures::{after, SEC};
use crate::tests::compositor;

/// The compositor fixture's screen.
const SCREEN: (u32, u32) = (1920, 1080);

const MARK: Color = Color::rgb(1, 2, 3);

fn board() -> SystemMonitor {
    let identity = SaverIdentity {
        user: "ann".into(),
        host: "rack-07".into(),
    };
    SystemMonitor::new(
        &identity,
        &Theme::dark(),
        SCREEN,
        (Some(after(0)), 0),
        SystemMonitorOptions::default(),
    )
    .expect("a board")
}

/// A window showing `monitor` as it first paints.
fn shown(comp: &mut Compositor, monitor: &SystemMonitor) -> WindowId {
    let mut first = Surface::new(SCREEN.0, SCREEN.1).expect("a surface");
    monitor.paint(&mut first);
    comp.add_window(Point::new(0, 0), first)
}

fn mark_all(comp: &mut Compositor, wm: WindowId) {
    let mut all = Region::new();
    all.add(Rect::new(0, 0, SCREEN.0, SCREEN.1));
    assert!(comp.repaint_window(wm, SCREEN, &all, |surface, _| surface.fill(MARK)));
}

fn content(comp: &Compositor, wm: WindowId) -> Surface {
    comp.window(wm)
        .and_then(tairix_wm::Window::content)
        .cloned()
        .expect("the window keeps its pixels")
}

/// Whether anything inside `rect` was repainted over the mark.
fn repainted(surface: &Surface, rect: Rect) -> bool {
    let (left, top) = rect.surface_origin().expect("on screen");
    (top..top + rect.height).any(|y| {
        (left..left + rect.width).any(|x| {
            surface
                .get(x, y)
                .is_some_and(|p| (p.r, p.g, p.b) != (MARK.r, MARK.g, MARK.b))
        })
    })
}

fn no_wall() -> impl FnMut() -> Option<tairix_abi::time::WallClockReading> {
    || Some(after(0))
}

#[test]
fn a_board_waits_for_its_first_reading_and_then_says_none_came() {
    let mut comp = compositor();
    let mut monitor = board();
    let wm = shown(&mut comp, &monitor);
    assert_eq!(monitor.verdict(), Verdict::Waiting);
    assert!(monitor.due_ns() <= FIRST_READING_NS);
    monitor.advance(FIRST_READING_NS, wm, &mut comp, &mut no_wall());
    assert_eq!(monitor.verdict(), Verdict::Silent);
}

#[test]
fn a_reading_is_believed_for_three_of_its_periods_and_no_longer() {
    let mut comp = compositor();
    let mut monitor = board();
    let wm = shown(&mut comp, &monitor);
    monitor.adopt(report(), SEC, wm, &mut comp);
    assert_eq!(monitor.verdict(), Verdict::of(&report()));
    let stale_at = SEC + STALE_PERIODS * PERIOD_NS;
    assert_eq!(
        monitor.due_ns(),
        stale_at,
        "nothing is due before the readings go stale"
    );
    monitor.advance(stale_at - 1, wm, &mut comp, &mut no_wall());
    assert_eq!(monitor.verdict(), Verdict::of(&report()));
    monitor.advance(stale_at, wm, &mut comp, &mut no_wall());
    assert!(matches!(monitor.verdict(), Verdict::Stale { .. }));
    // A fresh reading makes the board live again.
    monitor.adopt(report(), stale_at + SEC, wm, &mut comp);
    assert_eq!(monitor.verdict(), Verdict::of(&report()));
}

#[test]
fn a_reading_repaints_only_the_parts_it_changed() {
    let mut comp = compositor();
    let mut monitor = board();
    let wm = shown(&mut comp, &monitor);
    monitor.adopt(report(), SEC, wm, &mut comp);
    mark_all(&mut comp, wm);

    let mut busier = report();
    busier.network = Some(
        MachineNetwork::new(
            1,
            &[MachineInterface {
                name: MachineInterfaceName::new("eth0").expect("a name"),
                link_up: Some(true),
                receive_rate: Some(1 << 24),
                send_rate: Some(1 << 16),
            }],
        )
        .expect("network"),
    );
    monitor.adopt(busier, 3 * SEC, wm, &mut comp);

    let after = content(&comp, wm);
    for part in Part::ALL {
        assert_eq!(
            repainted(&after, monitor.board.slot(part)),
            part == Part::Network,
            "{part:?}"
        );
    }
}

#[test]
fn an_unchanged_reading_repaints_nothing() {
    let mut comp = compositor();
    let mut monitor = board();
    let wm = shown(&mut comp, &monitor);
    monitor.adopt(report(), SEC, wm, &mut comp);
    mark_all(&mut comp, wm);
    monitor.adopt(report(), 3 * SEC, wm, &mut comp);
    let after = content(&comp, wm);
    assert!(!repainted(&after, Rect::new(0, 0, SCREEN.0, SCREEN.1)));
}

#[test]
fn a_monitor_that_stops_running_is_said_to_have_stopped() {
    let mut comp = compositor();
    let mut monitor = board();
    let wm = shown(&mut comp, &monitor);
    monitor.adopt(report(), SEC, wm, &mut comp);
    monitor.unmonitored(wm, &mut comp);
    assert_eq!(monitor.verdict(), Verdict::Unmonitored);
    assert!(
        monitor.stale_ns().is_none(),
        "nothing is waited for from a stopped monitor"
    );
    monitor.adopt(report(), 5 * SEC, wm, &mut comp);
    assert_eq!(
        monitor.verdict(),
        Verdict::of(&report()),
        "a revived monitor is live again"
    );
}

#[test]
fn the_board_moves_only_at_the_minute_and_never_far() {
    let mut comp = compositor();
    let mut monitor = board();
    let wm = shown(&mut comp, &monitor);
    let rest = monitor.board.slot(Part::Cpu);
    let tick = monitor.telling.tick_ns();
    monitor.advance(tick - 1, wm, &mut comp, &mut no_wall());
    assert_eq!(monitor.board.slot(Part::Cpu), rest);
    monitor.advance(tick, wm, &mut comp, &mut || Some(after(60)));
    let moved = monitor.board.slot(Part::Cpu);
    assert_ne!(
        moved, rest,
        "a minute on, the board has stepped round its orbit"
    );
    let reach = monitor.board.scale().scale_length(super::board::ORBIT);
    assert!(moved.left().abs_diff(rest.left()) <= reach);
    assert!(moved.top().abs_diff(rest.top()) <= reach);
}

#[test]
fn a_reduced_motion_desktop_draws_the_board_on_its_own_axes() {
    let reduced = Theme::dark().with_axes(tairix_theme::Accessibility {
        motion: tairix_theme::Motion::Reduced,
        ..tairix_theme::Accessibility::default()
    });
    let theme = super::board_theme(&reduced);
    assert!(theme.motion().reduced_motion());
    assert_eq!(theme.appearance(), Theme::dark().appearance());
}

#[test]
fn the_board_is_set_in_the_text_the_desktop_is_drawn_in() {
    let family = tairix_theme::FamilyKey::new("noto-serif").expect("a family key");
    let chosen = tairix_theme::DesktopText::new(family, 22).ok();
    let light = Theme::light().with_text(chosen);
    let board = super::board_theme(&light);
    assert_eq!(board.appearance(), Theme::dark().appearance());
    assert_eq!(board.fonts().ui_family(), family);
    assert_eq!(board.fonts().base_size_px(), 22);
}
