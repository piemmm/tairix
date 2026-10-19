//! The Date & Time window: its layout under every theme and ladder, the
//! keyboard's walk, network time taking the hand-set controls away, a day or
//! a time made the user's own and set, and the damage a tick reports.

use alloc::string::String;

use tairix_controls::testkit::{high_contrast, text_ladder};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::Surface;
use tairix_theme::Theme;
use tairix_tz::{HourCycle, Weekday};

use super::{Focus, Request, Window, NETWORK_KEEPS_IT};
use crate::calendar::Date;
use crate::time_entry::{TimeOfDay, TimePart};
use crate::Readout;

fn date(year: i64, month: u32, day: u32) -> Date {
    Date::new(year, month, day).expect("a date")
}

fn at(hour: u32, minute: u32, second: u32) -> TimeOfDay {
    TimeOfDay::new(hour, minute, second).expect("a time")
}

fn readout(time: &str) -> Readout {
    Readout {
        time: String::from(time),
        date: String::from("Monday 19 October 2026"),
        zone: String::from("London, United Kingdom \u{b7} BST, UTC+01:00"),
        source: String::from("Set automatically from the network today at 13:58."),
        change: String::from("Clocks go back an hour on Sunday 25 October at 02:00."),
    }
}

fn window(network: bool, theme: &Theme, scale: Scale) -> Window {
    Window::new(
        readout("14:02:09"),
        (network, "London, United Kingdom"),
        (Weekday::Monday, HourCycle::TwentyFour),
        (date(2026, 10, 19), at(14, 2, 9)),
        (scale, theme),
    )
}

fn sink() -> Region {
    tairix_controls::damage::sink()
}

fn centre(rect: Rect) -> Point {
    Point::new(
        rect.left() + i32::try_from(rect.width / 2).expect("fits"),
        rect.top() + i32::try_from(rect.height / 2).expect("fits"),
    )
}

fn click(window: &mut Window, point: Point, theme: &Theme) -> Option<Request> {
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
        if let Some(request) = window.on_pointer(&event, Scale::ONE, theme, &mut sink()) {
            last = Some(request);
        }
    }
    last
}

fn key(window: &mut Window, key: Key, theme: &Theme) -> Option<Request> {
    window.on_key(key, Modifiers::default(), Scale::ONE, theme, &mut sink())
}

fn tab(window: &mut Window, backward: bool, theme: &Theme) {
    let modifiers = Modifiers {
        shift: backward,
        ..Modifiers::default()
    };
    let _ = window.on_key(
        Key::Named(NamedKey::Tab),
        modifiers,
        Scale::ONE,
        theme,
        &mut sink(),
    );
}

#[test]
fn every_part_stands_inside_the_window_and_none_overlaps_another() {
    for theme in [
        Theme::dark(),
        Theme::light(),
        high_contrast(),
        text_ladder(22),
    ] {
        for scale in [Scale::ONE, Scale::from_percent(200).expect("a scale")] {
            for network in [false, true] {
                let window = window(network, &theme, scale);
                let layout = window.layout(scale, &theme);
                let (width, height) = window.extent(scale, &theme);
                let within = |rect: Rect| {
                    rect.left() >= 0
                        && rect.top() >= 0
                        && rect.right() <= i32::try_from(width).expect("fits")
                        && rect.bottom() <= i32::try_from(height).expect("fits")
                };
                let band = window
                    .clock
                    .band_rect(layout.clock, scale, &theme)
                    .expect("the hand-set controls' band is drawn");
                let close = window
                    .dialog
                    .action_rects(layout.bounds, scale, &theme)
                    .first()
                    .copied()
                    .expect("Close");
                let what = alloc::format!("{} at {}%", theme.name(), scale.percent());
                for rect in [layout.hero, layout.clock.bounds, layout.zone.bounds, close] {
                    assert!(within(rect), "{rect:?} spills out under {what}");
                }
                assert!(layout.hero.bottom() <= layout.clock.bounds.top(), "{what}");
                assert!(
                    layout.clock.bounds.bottom() <= layout.zone.bounds.top(),
                    "{what}"
                );
                assert!(layout.zone.bounds.bottom() <= close.top(), "{what}");
                for part in [layout.calendar, layout.time, layout.set, layout.time_label] {
                    assert!(
                        part.left() >= band.left()
                            && part.right() <= band.right()
                            && part.top() >= band.top()
                            && part.bottom() <= band.bottom(),
                        "{part:?} leaves the band {band:?} under {what}"
                    );
                }
                assert!(
                    layout.calendar.bottom() <= layout.time_label.top()
                        || layout.calendar.right() <= layout.time_label.left(),
                    "the time column stands beside or beneath the calendar under {what}"
                );
                let mut surface = Surface::new(width, height).expect("a surface");
                window.render(&mut surface, scale, &theme);
            }
        }
    }
}

#[test]
fn network_time_takes_the_hand_set_controls_away_and_says_why() {
    let theme = Theme::dark();
    let mut on = window(true, &theme, Scale::ONE);
    assert_eq!(on.clock.footnote(), Some(NETWORK_KEEPS_IT));
    assert!(!on.calendar.state().enabled && !on.time.state().enabled);
    let layout = on.layout(Scale::ONE, &theme);
    assert_eq!(click(&mut on, centre(layout.calendar), &theme), None);
    assert!(!on.edited(), "a disabled calendar takes no choice");

    let mut off = window(false, &theme, Scale::ONE);
    assert_eq!(off.clock.footnote(), None);
    assert!(off.calendar.state().enabled);
    off.set_network(true, Scale::ONE, &theme, &mut sink());
    assert_eq!(off.clock.footnote(), Some(NETWORK_KEEPS_IT));
    assert!(!off.time.state().enabled);
}

#[test]
fn the_switch_asks_for_network_time_and_change_asks_for_the_chooser() {
    let theme = Theme::dark();
    let mut shown = window(false, &theme, Scale::ONE);
    let layout = shown.layout(Scale::ONE, &theme);
    let toggle = shown
        .clock
        .rows()
        .first()
        .and_then(|row| {
            let rect = shown.clock.row_rect(0, layout.clock, Scale::ONE, &theme)?;
            row.control_rect(
                tairix_controls::FieldLayout {
                    bounds: rect,
                    ..layout.clock
                },
                Scale::ONE,
                &theme,
            )
        })
        .expect("the switch is drawn");
    assert_eq!(
        click(&mut shown, centre(toggle), &theme),
        Some(Request::Network(true))
    );
    let change = shown
        .zone
        .row_rect(0, layout.zone, Scale::ONE, &theme)
        .and_then(|rect| {
            shown.zone.rows()[0].control_rect(
                tairix_controls::FieldLayout {
                    bounds: rect,
                    ..layout.zone
                },
                Scale::ONE,
                &theme,
            )
        })
        .expect("Change is drawn");
    assert_eq!(
        click(&mut shown, centre(change), &theme),
        Some(Request::ChooseZone)
    );
    assert_eq!(
        key(&mut shown, Key::Char(' '), &theme),
        Some(Request::ChooseZone)
    );
}

#[test]
fn a_chosen_day_is_the_users_own_and_set_asks_for_it() {
    let theme = Theme::dark();
    let mut shown = window(false, &theme, Scale::ONE);
    assert!(
        !shown.set.state().enabled,
        "nothing of the user's own to set"
    );
    let layout = shown.layout(Scale::ONE, &theme);
    let _ = click(&mut shown, centre(layout.calendar), &theme);
    let _ = key(&mut shown, Key::Named(NamedKey::Right), &theme);
    assert!(shown.edited());
    assert!(shown.set.state().enabled);
    assert_eq!(
        click(&mut shown, centre(layout.set), &theme),
        Some(Request::SetClock {
            date: shown.calendar.chosen().expect("a day"),
            time: at(14, 2, 9),
        })
    );
}

#[test]
fn a_typed_time_is_set_with_enter() {
    let theme = Theme::dark();
    let mut shown = window(false, &theme, Scale::ONE);
    let layout = shown.layout(Scale::ONE, &theme);
    let hour = shown
        .time
        .part_rect(TimePart::Hour, layout.time, Scale::ONE, &theme)
        .expect("the hour");
    let _ = click(&mut shown, centre(hour), &theme);
    assert_eq!(shown.focus, Some(Focus::Time));
    for _ in 0..2 {
        let _ = key(&mut shown, Key::Named(NamedKey::Backspace), &theme);
    }
    let _ = key(&mut shown, Key::Char('9'), &theme);
    assert_eq!(
        key(&mut shown, Key::Named(NamedKey::Enter), &theme),
        Some(Request::SetClock {
            date: date(2026, 10, 19),
            time: at(9, 2, 9),
        })
    );
}

#[test]
fn tab_walks_every_part_that_can_act_and_passes_over_the_rest() {
    let theme = Theme::dark();
    let mut shown = window(false, &theme, Scale::ONE);
    let mut walked = alloc::vec::Vec::new();
    for _ in 0..8 {
        tab(&mut shown, false, &theme);
        walked.push((shown.focus, shown.time.focus()));
    }
    assert_eq!(
        walked,
        [
            (Some(Focus::Automatic), None),
            (Some(Focus::Calendar), None),
            (Some(Focus::Time), Some(TimePart::Hour)),
            (Some(Focus::Time), Some(TimePart::Minute)),
            (Some(Focus::Time), Some(TimePart::Second)),
            (Some(Focus::Zone), None),
            (Some(Focus::Close), None),
            (Some(Focus::Automatic), None),
        ],
        "Set is passed over while there is nothing of the user's own to set"
    );
    let mut on = window(true, &theme, Scale::ONE);
    tab(&mut on, false, &theme);
    tab(&mut on, false, &theme);
    assert_eq!(
        on.focus,
        Some(Focus::Zone),
        "network time takes the hand-set parts away"
    );
    tab(&mut on, true, &theme);
    assert_eq!(on.focus, Some(Focus::Automatic));
}

#[test]
fn escape_takes_typing_back_before_it_closes() {
    let theme = Theme::dark();
    let mut shown = window(false, &theme, Scale::ONE);
    shown.focus_time(Scale::ONE, &theme, &mut sink());
    let _ = key(&mut shown, Key::Named(NamedKey::Backspace), &theme);
    assert_eq!(key(&mut shown, Key::Named(NamedKey::Escape), &theme), None);
    assert_eq!(
        key(&mut shown, Key::Named(NamedKey::Escape), &theme),
        Some(Request::Close)
    );
}

#[test]
fn the_controls_follow_the_clock_until_they_are_the_users_own() {
    let theme = Theme::dark();
    let mut shown = window(false, &theme, Scale::ONE);
    shown.follow(
        Some((date(2026, 10, 20), at(0, 0, 1))),
        Scale::ONE,
        &theme,
        &mut sink(),
    );
    assert_eq!(shown.calendar.chosen(), Some(date(2026, 10, 20)));
    assert_eq!(shown.time.time(), at(0, 0, 1));
    let layout = shown.layout(Scale::ONE, &theme);
    let _ = click(&mut shown, centre(layout.calendar), &theme);
    let _ = key(&mut shown, Key::Named(NamedKey::Left), &theme);
    let chosen = shown.calendar.chosen();
    shown.follow(
        Some((date(2026, 10, 20), at(0, 0, 2))),
        Scale::ONE,
        &theme,
        &mut sink(),
    );
    assert_eq!(shown.calendar.chosen(), chosen, "the user's day is kept");
    assert_eq!(shown.time.time(), at(0, 0, 1), "and so is the time shown");
    shown.release_edit(Scale::ONE, &theme, &mut sink());
    shown.follow(
        Some((date(2026, 10, 20), at(0, 0, 3))),
        Scale::ONE,
        &theme,
        &mut sink(),
    );
    assert_eq!(shown.time.time(), at(0, 0, 3), "given back to the clock");
    assert!(!shown.set.state().enabled);
}

#[test]
fn a_tick_repaints_the_time_line_and_the_entry_and_nothing_else() {
    let theme = Theme::dark();
    let mut shown = window(false, &theme, Scale::ONE);
    let layout = shown.layout(Scale::ONE, &theme);
    let mut damage = sink();
    shown.adopt_readout(readout("14:02:10"), Scale::ONE, &theme, &mut damage);
    shown.follow(
        Some((date(2026, 10, 19), at(14, 2, 10))),
        Scale::ONE,
        &theme,
        &mut damage,
    );
    let time_line = super::hero_line_rect(layout.hero, 0, Scale::ONE, &theme);
    let inside = |outer: Rect, rect: &Rect| outer.intersection(rect) == *rect;
    for rect in damage.rects() {
        assert!(
            inside(time_line, rect) || inside(layout.time, rect),
            "{rect:?} is neither the time line nor the entry"
        );
    }
    assert!(!damage.is_empty());
    let mut unchanged = sink();
    shown.adopt_readout(readout("14:02:10"), Scale::ONE, &theme, &mut unchanged);
    assert!(unchanged.is_empty(), "the same reading repaints nothing");
}

#[test]
fn a_reason_is_stated_above_close_and_the_window_grows_to_hold_it() {
    let theme = Theme::dark();
    let mut shown = window(false, &theme, Scale::ONE);
    let (_, before) = shown.extent(Scale::ONE, &theme);
    shown.set_reason(
        Some(String::from(
            "01:30:00 does not happen on Sunday 29 March 2026.",
        )),
        Scale::ONE,
        &theme,
        &mut sink(),
    );
    let (_, after) = shown.extent(Scale::ONE, &theme);
    assert!(after > before);
}

/// A tick can rewrap a line as surely as an answer can: the provenance line
/// turning at midnight, or the next change rewritten once one has passed.
#[test]
fn a_reading_that_rewraps_its_lines_grows_the_window_and_repaints_it_whole() {
    let theme = Theme::dark();
    let mut shown = window(false, &theme, Scale::ONE);
    let (_, before) = shown.extent(Scale::ONE, &theme);
    let mut damage = sink();
    shown.adopt_readout(
        Readout {
            source: String::from(
                "Set automatically from the network on Wednesday 30 September 2026 at 23:59, \
                 the last time a time server on the network answered this machine.",
            ),
            ..readout("14:02:10")
        },
        Scale::ONE,
        &theme,
        &mut damage,
    );
    let (width, after) = shown.extent(Scale::ONE, &theme);
    assert!(after > before, "the window grows to hold the longer line");
    let whole = Rect::new(0, 0, width, after);
    assert!(
        damage.rects().contains(&whole),
        "a line that moved the bands repaints the window whole"
    );
}
