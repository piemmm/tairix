//! The calendar month: its dates, its grid, and what the keys and a click
//! choose.

use tairix_geometry::{Point, Rect, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::Surface;
use tairix_theme::Theme;
use tairix_tz::Weekday;

use super::{CalendarAction, CalendarMonth, Date};

const W: u32 = 336;
const H: u32 = 300;

fn bounds() -> Rect {
    Rect::new(0, 0, W, H)
}

fn date(year: i64, month: u32, day: u32) -> Date {
    Date::new(year, month, day).expect("a date")
}

fn sink() -> tairix_geometry::Region {
    tairix_controls::damage::sink()
}

fn october(first: Weekday) -> CalendarMonth {
    let mut calendar = CalendarMonth::new(date(2026, 10, 18), first);
    calendar.set_chosen(Some(date(2026, 10, 18)));
    calendar.set_focused(true);
    calendar
}

fn key(calendar: &mut CalendarMonth, named: NamedKey, shift: bool) -> Option<CalendarAction> {
    let theme = Theme::dark();
    let modifiers = Modifiers {
        shift,
        ..Modifiers::default()
    };
    calendar.on_key(
        Key::Named(named),
        modifiers,
        bounds(),
        Scale::ONE,
        &theme,
        &mut sink(),
    )
}

fn chose(calendar: &mut CalendarMonth, named: NamedKey, shift: bool) -> Option<Date> {
    key(calendar, named, shift).map(|CalendarAction::Chose(date)| date)
}

/// The centre of the grid cell `index` days in.
fn cell_centre(index: u32) -> Point {
    let theme = Theme::dark();
    let layout = CalendarMonth::layout(bounds(), Scale::ONE, &theme);
    let cell = layout.cell(index);
    Point::new(
        cell.left() + i32::try_from(cell.width / 2).expect("fits"),
        cell.top() + i32::try_from(cell.height / 2).expect("fits"),
    )
}

fn click(calendar: &mut CalendarMonth, point: Point) -> Option<CalendarAction> {
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
        if let Some(action) = calendar.on_pointer(&event, bounds(), Scale::ONE, &theme, &mut sink())
        {
            last = Some(action);
        }
    }
    last
}

#[test]
fn a_date_steps_by_days_and_months_and_holds_to_a_shorter_month() {
    let leap = date(2028, 1, 31);
    assert_eq!(leap.plus_months(1), Some(date(2028, 2, 29)));
    assert_eq!(date(2027, 1, 31).plus_months(1), Some(date(2027, 2, 28)));
    assert_eq!(date(2026, 12, 15).plus_months(1), Some(date(2027, 1, 15)));
    assert_eq!(date(2026, 1, 15).plus_months(-1), Some(date(2025, 12, 15)));
    assert_eq!(date(1969, 12, 31).plus_days(1), Some(date(1970, 1, 1)));
    assert_eq!(date(2038, 1, 19).plus_days(1), Some(date(2038, 1, 20)));
    assert_eq!(Date::new(2026, 2, 29), None);
    assert_eq!(Date::new(2026, 13, 1), None);
    assert_eq!(
        date(crate::YEAR_MAX, 12, 31).plus_days(1),
        None,
        "past the calendar's years"
    );
    assert_eq!(date(2026, 10, 25).weekday(), Weekday::Sunday);
}

#[test]
fn the_grid_begins_on_the_regions_first_day() {
    // 1 October 2026 is a Thursday.
    let monday = CalendarMonth::new(date(2026, 10, 1), Weekday::Monday);
    assert_eq!(monday.day_at(0), Some(date(2026, 9, 28)));
    assert_eq!(monday.index_of(date(2026, 10, 1)), Some(3));
    let sunday = CalendarMonth::new(date(2026, 10, 1), Weekday::Sunday);
    assert_eq!(sunday.day_at(0), Some(date(2026, 9, 27)));
    assert_eq!(sunday.index_of(date(2026, 10, 1)), Some(4));
    let saturday = CalendarMonth::new(date(2026, 10, 1), Weekday::Saturday);
    assert_eq!(saturday.day_at(0), Some(date(2026, 9, 26)));
    // Six weeks hold every month: a 31-day month starting on the week's
    // last day.
    let late = CalendarMonth::new(date(2026, 8, 1), Weekday::Sunday);
    assert!(late.index_of(date(2026, 8, 31)).is_some());
}

#[test]
fn the_arrows_move_a_day_and_a_week_and_the_page_keys_a_month_and_a_year() {
    let mut calendar = october(Weekday::Monday);
    assert_eq!(
        chose(&mut calendar, NamedKey::Right, false),
        Some(date(2026, 10, 19))
    );
    assert_eq!(
        chose(&mut calendar, NamedKey::Left, false),
        Some(date(2026, 10, 18))
    );
    assert_eq!(
        chose(&mut calendar, NamedKey::Down, false),
        Some(date(2026, 10, 25))
    );
    assert_eq!(
        chose(&mut calendar, NamedKey::Up, false),
        Some(date(2026, 10, 18))
    );
    assert_eq!(
        chose(&mut calendar, NamedKey::PageDown, false),
        Some(date(2026, 11, 18))
    );
    assert_eq!(
        calendar.shown(),
        date(2026, 11, 1),
        "the month follows the choice"
    );
    assert_eq!(
        chose(&mut calendar, NamedKey::PageUp, true),
        Some(date(2025, 11, 18))
    );
    // Sunday 18 October's week, from Monday: Home to the 12th, End to the 18th.
    let mut calendar = calendar_on(date(2026, 10, 18), Weekday::Monday);
    assert_eq!(
        chose(&mut calendar, NamedKey::Home, false),
        Some(date(2026, 10, 12))
    );
    assert_eq!(
        chose(&mut calendar, NamedKey::End, false),
        Some(date(2026, 10, 18))
    );
    // From Sunday the same day begins its week, so Home has nowhere to go.
    let mut calendar = calendar_on(date(2026, 10, 18), Weekday::Sunday);
    assert_eq!(chose(&mut calendar, NamedKey::Home, false), None);
    assert_eq!(
        chose(&mut calendar, NamedKey::End, false),
        Some(date(2026, 10, 24))
    );
}

fn calendar_on(day: Date, first: Weekday) -> CalendarMonth {
    let mut calendar = CalendarMonth::new(day, first);
    calendar.set_chosen(Some(day));
    calendar.set_focused(true);
    calendar
}

#[test]
fn with_nothing_chosen_the_first_key_chooses_today() {
    let mut calendar = CalendarMonth::new(date(2026, 10, 1), Weekday::Monday);
    calendar.set_focused(true);
    calendar.set_today(Some(date(2026, 10, 18)));
    assert_eq!(
        chose(&mut calendar, NamedKey::Right, false),
        Some(date(2026, 10, 18))
    );
    assert_eq!(
        chose(&mut calendar, NamedKey::Right, false),
        Some(date(2026, 10, 19))
    );
}

#[test]
fn an_unfocused_or_disabled_calendar_takes_nothing() {
    let mut calendar = october(Weekday::Monday);
    calendar.set_focused(false);
    assert_eq!(key(&mut calendar, NamedKey::Right, false), None);
    let mut calendar = october(Weekday::Monday);
    calendar.set_state(tairix_controls::ControlState::disabled());
    calendar.set_focused(true);
    assert_eq!(key(&mut calendar, NamedKey::Right, false), None);
    let point = cell_centre(10);
    assert_eq!(click(&mut calendar, point), None);
}

#[test]
fn a_click_chooses_the_day_under_it_and_a_neighbouring_months_day_brings_its_month() {
    let mut calendar = october(Weekday::Monday);
    let point = cell_centre(3);
    assert_eq!(
        click(&mut calendar, point),
        Some(CalendarAction::Chose(date(2026, 10, 1)))
    );
    let point = cell_centre(0);
    assert_eq!(
        click(&mut calendar, point),
        Some(CalendarAction::Chose(date(2026, 9, 28)))
    );
    assert_eq!(calendar.shown(), date(2026, 9, 1));
}

#[test]
fn a_press_released_on_another_day_chooses_nothing() {
    let theme = Theme::dark();
    let mut calendar = october(Weekday::Monday);
    let first = cell_centre(10);
    let other = cell_centre(11);
    calendar.on_pointer(
        &InputEvent::PointerMoved { to: first },
        bounds(),
        Scale::ONE,
        &theme,
        &mut sink(),
    );
    calendar.on_pointer(
        &InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        bounds(),
        Scale::ONE,
        &theme,
        &mut sink(),
    );
    calendar.on_pointer(
        &InputEvent::PointerMoved { to: other },
        bounds(),
        Scale::ONE,
        &theme,
        &mut sink(),
    );
    assert_eq!(
        calendar.on_pointer(
            &InputEvent::PointerReleased {
                button: PointerButton::Primary,
            },
            bounds(),
            Scale::ONE,
            &theme,
            &mut sink(),
        ),
        None
    );
}

#[test]
fn the_steppers_browse_months_and_years_without_changing_the_choice() {
    let mut calendar = october(Weekday::Monday);
    let theme = Theme::dark();
    let layout = CalendarMonth::layout(bounds(), Scale::ONE, &theme);
    let centre = |rect: Rect| {
        Point::new(
            rect.left() + i32::try_from(rect.width / 2).expect("fits"),
            rect.top() + i32::try_from(rect.height / 2).expect("fits"),
        )
    };
    click(&mut calendar, centre(layout.steppers[1]));
    assert_eq!(calendar.shown(), date(2026, 11, 1));
    click(&mut calendar, centre(layout.steppers[3]));
    assert_eq!(calendar.shown(), date(2027, 11, 1));
    click(&mut calendar, centre(layout.steppers[2]));
    click(&mut calendar, centre(layout.steppers[0]));
    assert_eq!(calendar.shown(), date(2026, 10, 1));
    assert_eq!(calendar.chosen(), Some(date(2026, 10, 18)));
}

#[test]
fn a_choice_within_the_month_reports_its_two_days_and_a_move_out_of_it_the_grid() {
    let theme = Theme::dark();
    let mut calendar = october(Weekday::Monday);
    let layout = CalendarMonth::layout(bounds(), Scale::ONE, &theme);
    let mut damage = sink();
    calendar.on_key(
        Key::Named(NamedKey::Right),
        Modifiers::default(),
        bounds(),
        Scale::ONE,
        &theme,
        &mut damage,
    );
    let rects = damage.rects();
    assert!(
        rects.iter().all(|rect| rect.height <= layout.cell_h),
        "{rects:?}"
    );
    let mut damage = sink();
    calendar.on_key(
        Key::Named(NamedKey::PageDown),
        Modifiers::default(),
        bounds(),
        Scale::ONE,
        &theme,
        &mut damage,
    );
    assert!(damage
        .rects()
        .iter()
        .any(|rect| rect.height >= layout.grid.height));
}

#[test]
fn every_theme_draws_today_the_choice_and_the_disabled_month_apart() {
    for theme in [
        Theme::dark(),
        Theme::light(),
        tairix_controls::testkit::high_contrast(),
    ] {
        let mut calendar = october(Weekday::Monday);
        calendar.set_today(Some(date(2026, 10, 20)));
        let mut lit = Surface::new(W, H).expect("a surface");
        calendar.render(&mut lit, bounds(), Scale::ONE, &theme);
        calendar.set_state(tairix_controls::ControlState::disabled());
        let mut quiet = Surface::new(W, H).expect("a surface");
        calendar.render(&mut quiet, bounds(), Scale::ONE, &theme);
        assert_ne!(lit.pixels(), quiet.pixels());
    }
}

#[test]
fn a_tiny_or_zero_bound_draws_without_fault() {
    let theme = Theme::dark();
    let calendar = october(Weekday::Monday);
    for bounds in [
        Rect::new(0, 0, 0, 0),
        Rect::new(0, 0, 5, 5),
        Rect::new(0, 0, W, 20),
    ] {
        let mut surface = Surface::new(W, H).expect("a surface");
        calendar.render(&mut surface, bounds, Scale::ONE, &theme);
    }
}
