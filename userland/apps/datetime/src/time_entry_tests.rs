//! The time entry: both clocks, the keyboard's walk through the parts, typing,
//! stepping, the half of the day, and a press giving a part the keyboard.

use tairix_controls::testkit::{high_contrast, text_ladder};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::Surface;
use tairix_theme::Theme;
use tairix_tz::HourCycle;

use super::{TimeAction, TimeEntry, TimeOfDay, TimePart};

const W: u32 = 360;

fn bounds() -> Rect {
    Rect::new(4, 6, W, 60)
}

fn at(hour: u32, minute: u32, second: u32) -> TimeOfDay {
    TimeOfDay::new(hour, minute, second).expect("a time")
}

fn sink() -> Region {
    tairix_controls::damage::sink()
}

fn focused(time: TimeOfDay, cycle: HourCycle, part: TimePart) -> TimeEntry {
    let mut entry = TimeEntry::new(time, cycle);
    let _ = entry.set_focus(
        Some(part),
        bounds(),
        Scale::ONE,
        &Theme::dark(),
        &mut sink(),
    );
    entry
}

fn key(entry: &mut TimeEntry, key: Key) -> Option<TimeAction> {
    entry.on_key(
        key,
        Modifiers::default(),
        bounds(),
        Scale::ONE,
        &Theme::dark(),
        &mut sink(),
    )
}

fn centre_of(entry: &TimeEntry, part: TimePart) -> Point {
    let rect = entry
        .part_rect(part, bounds(), Scale::ONE, &Theme::dark())
        .expect("the part is drawn");
    Point::new(
        rect.left() + i32::try_from(rect.width / 2).expect("fits"),
        rect.top() + i32::try_from(rect.height / 2).expect("fits"),
    )
}

fn click(entry: &mut TimeEntry, point: Point) -> Option<TimeAction> {
    let theme = Theme::dark();
    let mut last = None;
    for event in [
        InputEvent::PointerMoved { to: point },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    ] {
        if let Some(action) = entry.on_pointer(&event, bounds(), Scale::ONE, &theme, &mut sink()) {
            last = Some(action);
        }
    }
    last
}

#[test]
fn a_time_of_day_is_held_to_the_clock() {
    assert_eq!(TimeOfDay::new(23, 59, 59), Some(at(23, 59, 59)));
    assert_eq!(TimeOfDay::new(24, 0, 0), None);
    assert_eq!(TimeOfDay::new(0, 60, 0), None);
    assert_eq!(TimeOfDay::new(0, 0, 60), None);
}

#[test]
fn the_twelve_hour_clock_names_its_hours_and_halves_and_reads_back_whole() {
    for hour in 0..24 {
        let entry = TimeEntry::new(at(hour, 7, 9), HourCycle::Twelve);
        assert_eq!(entry.time(), at(hour, 7, 9), "hour {hour} round-trips");
    }
    assert_eq!(
        TimeEntry::new(at(0, 0, 0), HourCycle::Twelve).parts(),
        &[
            TimePart::Hour,
            TimePart::Minute,
            TimePart::Second,
            TimePart::Meridiem
        ]
    );
    assert_eq!(
        TimeEntry::new(at(0, 0, 0), HourCycle::TwentyFour).parts(),
        &[TimePart::Hour, TimePart::Minute, TimePart::Second]
    );
}

#[test]
fn tab_walks_the_parts_and_lets_the_owner_carry_the_focus_on_past_either_end() {
    let theme = Theme::dark();
    let mut entry = TimeEntry::new(at(9, 30, 0), HourCycle::Twelve);
    let mut walked = alloc::vec::Vec::new();
    loop {
        let (moved, _) = entry.step_focus(false, bounds(), Scale::ONE, &theme, &mut sink());
        if !moved {
            break;
        }
        walked.push(entry.focus().expect("a part holds the keyboard"));
    }
    assert_eq!(walked, entry.parts());
    assert_eq!(entry.focus(), None, "past the last part it lets go");
    let (moved, _) = entry.step_focus(true, bounds(), Scale::ONE, &theme, &mut sink());
    assert!(moved);
    assert_eq!(
        entry.focus(),
        Some(TimePart::Meridiem),
        "backward enters at the end"
    );
}

#[test]
fn typing_edits_and_leaving_a_part_settles_it() {
    let theme = Theme::dark();
    let mut entry = focused(at(9, 30, 0), HourCycle::TwentyFour, TimePart::Minute);
    let _ = key(&mut entry, Key::Named(NamedKey::Backspace));
    let _ = key(&mut entry, Key::Named(NamedKey::Backspace));
    assert_eq!(
        key(&mut entry, Key::Char('4')),
        Some(TimeAction::Edited(at(9, 4, 0)))
    );
    assert_eq!(
        key(&mut entry, Key::Char('5')),
        Some(TimeAction::Edited(at(9, 45, 0)))
    );
    assert_eq!(
        key(&mut entry, Key::Char('x')),
        None,
        "nothing that cannot be part of a number is taken"
    );
    let (_, settled) = entry.step_focus(false, bounds(), Scale::ONE, &theme, &mut sink());
    assert_eq!(settled, Some(TimeAction::Settled(at(9, 45, 0))));
    assert_eq!(entry.focus(), Some(TimePart::Second));
}

#[test]
fn a_colon_moves_to_the_next_part() {
    let mut entry = focused(at(9, 30, 0), HourCycle::TwentyFour, TimePart::Hour);
    let _ = key(&mut entry, Key::Char(':'));
    assert_eq!(entry.focus(), Some(TimePart::Minute));
}

#[test]
fn the_arrows_step_a_part_round_the_clock_without_carrying() {
    let mut entry = focused(at(23, 59, 0), HourCycle::TwentyFour, TimePart::Minute);
    assert_eq!(
        key(&mut entry, Key::Named(NamedKey::Up)),
        Some(TimeAction::Settled(at(23, 0, 0))),
        "59 runs round to 0, and the hour stays"
    );
    let mut entry = focused(at(23, 0, 0), HourCycle::TwentyFour, TimePart::Hour);
    assert_eq!(
        key(&mut entry, Key::Named(NamedKey::Up)),
        Some(TimeAction::Settled(at(0, 0, 0)))
    );
    let mut entry = focused(at(1, 0, 0), HourCycle::Twelve, TimePart::Hour);
    assert_eq!(
        key(&mut entry, Key::Named(NamedKey::Down)),
        Some(TimeAction::Settled(at(0, 0, 0))),
        "1 AM steps down to 12 AM, which is midnight"
    );
}

#[test]
fn the_half_of_the_day_turns_by_keys_and_by_a_click() {
    let mut entry = focused(at(9, 15, 0), HourCycle::Twelve, TimePart::Meridiem);
    assert_eq!(
        key(&mut entry, Key::Char('p')),
        Some(TimeAction::Settled(at(21, 15, 0)))
    );
    assert_eq!(key(&mut entry, Key::Char('P')), None, "already after noon");
    assert_eq!(
        key(&mut entry, Key::Named(NamedKey::Down)),
        Some(TimeAction::Settled(at(9, 15, 0)))
    );
    assert_eq!(
        key(&mut entry, Key::Char(' ')),
        Some(TimeAction::Settled(at(21, 15, 0))),
        "Space presses the button"
    );
    let point = centre_of(&entry, TimePart::Meridiem);
    assert_eq!(
        click(&mut entry, point),
        Some(TimeAction::Settled(at(9, 15, 0)))
    );
}

#[test]
fn a_press_gives_its_part_the_keyboard_and_settles_the_one_it_leaves() {
    let mut entry = focused(at(9, 30, 0), HourCycle::TwentyFour, TimePart::Hour);
    let _ = key(&mut entry, Key::Named(NamedKey::Backspace));
    let _ = key(&mut entry, Key::Named(NamedKey::Backspace));
    let _ = key(&mut entry, Key::Char('7'));
    let point = centre_of(&entry, TimePart::Second);
    assert_eq!(
        click(&mut entry, point),
        Some(TimeAction::Settled(at(7, 30, 0)))
    );
    assert_eq!(entry.focus(), Some(TimePart::Second));
}

#[test]
fn a_disabled_entry_takes_nothing_and_lets_the_keyboard_go() {
    let mut entry = focused(at(9, 30, 0), HourCycle::TwentyFour, TimePart::Minute);
    entry.set_state(entry.state().with_enabled(false));
    assert_eq!(entry.focus(), None);
    assert_eq!(key(&mut entry, Key::Named(NamedKey::Up)), None);
    let point = centre_of(&entry, TimePart::Hour);
    assert_eq!(click(&mut entry, point), None);
    assert_eq!(entry.focus(), None);
}

#[test]
fn set_time_replaces_what_was_typed() {
    let mut entry = focused(at(9, 30, 0), HourCycle::Twelve, TimePart::Minute);
    let _ = key(&mut entry, Key::Char('1'));
    entry.set_time(at(18, 5, 59));
    assert_eq!(entry.time(), at(18, 5, 59));
}

#[test]
fn every_part_fits_the_width_it_measures_under_every_theme_and_ladder() {
    for theme in [
        Theme::dark(),
        Theme::light(),
        high_contrast(),
        text_ladder(22),
    ] {
        for scale in [Scale::ONE, Scale::from_percent(200).expect("a scale")] {
            for cycle in [HourCycle::TwentyFour, HourCycle::Twelve] {
                let entry = TimeEntry::new(at(12, 0, 0), cycle);
                let width = entry.width(scale, &theme);
                let height = TimeEntry::height(scale, &theme);
                let placed = Rect::new(0, 0, width, height);
                let mut surface = Surface::new(width, height).expect("a surface");
                entry.render(&mut surface, placed, scale, &theme);
                for &part in entry.parts() {
                    let rect = entry.part_rect(part, placed, scale, &theme).expect("drawn");
                    assert!(
                        rect.right() <= placed.right() && rect.width > 0,
                        "{part:?} fits under {} at {}%",
                        theme.name(),
                        scale.percent()
                    );
                }
            }
        }
    }
}
