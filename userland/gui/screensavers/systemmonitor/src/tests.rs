//! Host tests of the System Monitor: what it says before, while and after
//! readings come, what a reading repaints, and that it moves only at the
//! minute.

extern crate std;

use tairix_abi::bounded_text::BoundedText;
use tairix_abi::saver_ipc::Start;
use tairix_abi::switchboard_ipc::{MachineInterface, MachineInterfaceName, MachineNetwork};
use tairix_geometry::Rect;
use tairix_raster::{Color, Surface};
use tairix_saver::testing::{self, Screen};
use tairix_saver::{NextFrame, Scene, WallReading};
use tairix_theme::Theme;

use super::board::Part;
use super::fixtures::{report, PERIOD_NS};
use super::verdict::Verdict;
use super::{SystemMonitor, FIRST_READING_NS, STALE_PERIODS};

const SCREEN: (u32, u32) = (1920, 1080);

const SEC: u64 = 1_000_000_000;

const MARK: Color = Color::rgb(1, 2, 3);

/// The reading `time` on 2024-02-29, next changing a minute after `at_ns`.
fn reading(time: &str, at_ns: u64) -> WallReading {
    WallReading {
        time: BoundedText::new(time).expect("a time"),
        numeric_date: BoundedText::new("2024-02-29").expect("a date"),
        long_date: BoundedText::EMPTY,
        next_change_ns: at_ns + 60 * SEC,
    }
}

fn start() -> Start {
    tairix_font::install_test_transport();
    let mut given = testing::start(SCREEN.0, SCREEN.1);
    given.machine = BoundedText::new("rack-07").expect("a name");
    given
}

/// A board and its screen, its first frame drawn at nought.
fn shown() -> (SystemMonitor, Screen) {
    let given = start();
    let mut monitor = SystemMonitor::new(&testing::setup(&given, 0)).expect("a board");
    let mut screen = Screen::new(given);
    draw(&mut screen, &mut monitor, 0, "13:46");
    (monitor, screen)
}

/// Draw `monitor`'s frame at `now_ns` telling `time`, answering when it wants
/// the next.
fn draw(screen: &mut Screen, monitor: &mut SystemMonitor, now_ns: u64, time: &str) -> u64 {
    screen.wall = reading(time, now_ns);
    let (next, asked) = screen.draw(monitor, 0, now_ns);
    assert!(asked.is_empty(), "the board asks nothing of its host");
    let NextFrame::At(due) = next else {
        panic!("a board always wants another frame: {next:?}");
    };
    due
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

#[test]
fn a_board_waits_for_its_first_reading_and_then_says_none_came() {
    let (mut monitor, mut screen) = shown();
    assert_eq!(monitor.verdict(), Verdict::Waiting);
    assert_eq!(monitor.stale_ns(), Some(FIRST_READING_NS));
    let due = draw(&mut screen, &mut monitor, SEC, "13:46");
    assert_eq!(due, FIRST_READING_NS, "nothing is owed before then");
    draw(&mut screen, &mut monitor, FIRST_READING_NS, "13:46");
    assert_eq!(monitor.verdict(), Verdict::Silent);
}

#[test]
fn a_reading_is_believed_for_three_of_its_periods_and_no_longer() {
    let (mut monitor, mut screen) = shown();
    monitor.machine(&report());
    let stale_at = SEC + STALE_PERIODS * PERIOD_NS;
    assert_eq!(
        draw(&mut screen, &mut monitor, SEC, "13:46"),
        stale_at,
        "nothing is due before the readings go stale"
    );
    assert_eq!(monitor.verdict(), Verdict::of(&report()));
    draw(&mut screen, &mut monitor, stale_at - 1, "13:46");
    assert_eq!(monitor.verdict(), Verdict::of(&report()));
    draw(&mut screen, &mut monitor, stale_at, "13:46");
    assert!(matches!(monitor.verdict(), Verdict::Stale { .. }));
    // A fresh reading makes the board live again.
    monitor.machine(&report());
    draw(&mut screen, &mut monitor, stale_at + SEC, "13:46");
    assert_eq!(monitor.verdict(), Verdict::of(&report()));
}

#[test]
fn a_reading_repaints_only_the_parts_it_changed() {
    let (mut monitor, mut screen) = shown();
    monitor.machine(&report());
    draw(&mut screen, &mut monitor, SEC, "13:46");
    screen.surface.fill(MARK);

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
    monitor.machine(&busier);
    draw(&mut screen, &mut monitor, 3 * SEC, "13:46");

    for part in Part::ALL {
        assert_eq!(
            repainted(&screen.surface, monitor.board.slot(part)),
            part == Part::Network,
            "{part:?}"
        );
    }
    assert_eq!(screen.damage.bounds(), monitor.board.slot(Part::Network));
}

#[test]
fn an_unchanged_reading_repaints_nothing() {
    let (mut monitor, mut screen) = shown();
    monitor.machine(&report());
    draw(&mut screen, &mut monitor, SEC, "13:46");
    screen.surface.fill(MARK);
    monitor.machine(&report());
    draw(&mut screen, &mut monitor, 3 * SEC, "13:46");
    assert!(screen.damage.is_empty());
    assert!(!repainted(
        &screen.surface,
        Rect::new(0, 0, SCREEN.0, SCREEN.1)
    ));
}

#[test]
fn a_monitor_that_stops_running_is_said_to_have_stopped() {
    let (mut monitor, mut screen) = shown();
    monitor.machine(&report());
    draw(&mut screen, &mut monitor, SEC, "13:46");
    monitor.machine_lost();
    draw(&mut screen, &mut monitor, 2 * SEC, "13:46");
    assert_eq!(monitor.verdict(), Verdict::Unmonitored);
    assert!(
        monitor.stale_ns().is_none(),
        "nothing is waited for from a stopped monitor"
    );
    monitor.machine(&report());
    draw(&mut screen, &mut monitor, 5 * SEC, "13:46");
    assert_eq!(
        monitor.verdict(),
        Verdict::of(&report()),
        "a revived monitor is live again"
    );
}

#[test]
fn the_board_moves_only_at_the_minute_and_never_far() {
    let (mut monitor, mut screen) = shown();
    let rest = monitor.board.slot(Part::Cpu);
    draw(&mut screen, &mut monitor, 30 * SEC, "13:46");
    assert_eq!(monitor.board.slot(Part::Cpu), rest);
    draw(&mut screen, &mut monitor, 53 * SEC, "13:47");
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
