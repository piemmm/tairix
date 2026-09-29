//! The settings window opens on the player's choice, a drag previews and
//! writes once where it settles, every slider sits on a setting, every row
//! fits the window whatever it says, and a scoped repaint is the same pixels
//! a whole one would draw.

use tairix_controls::damage::{self, Repaint};
use tairix_controls::testkit::keystroke;

use super::*;
use crate::quality::Ladder;

const SCALE: Scale = Scale::ONE;

fn theme() -> Theme {
    Theme::dark()
}

fn shown(graphics: Graphics, detail: Detail) -> Shown {
    Shown {
        graphics,
        detail,
        readable: Resolution::Half,
    }
}

fn open(shown: Shown) -> SettingsWindow {
    SettingsWindow::new(shown, SCALE, &theme())
}

/// A custom detail with every knob somewhere between its ends.
const MIDDLING: Detail = Detail {
    lighting: Lighting::Medium,
    shadows: Shadows::Hard,
    ground: MaterialQuality::new(3),
    resolution: Resolution::TwoThirds,
};

/// The rectangle the slider on knob row `row` is drawn and hit-tested in.
fn slider_rect(window: &SettingsWindow, row: usize) -> Rect {
    let theme = theme();
    let layout = window.place(SCALE, &theme).groups[KNOBS];
    let bounds = window.groups[KNOBS]
        .row_rect(row, layout, SCALE, &theme)
        .expect("the row is laid out");
    window.groups[KNOBS].rows()[row]
        .control_rect(FieldLayout::new(bounds, layout.column), SCALE, &theme)
        .expect("the row seats its slider")
}

/// The slider's value on knob row `row`.
fn slider_value(window: &SettingsWindow, row: usize) -> u16 {
    match window.groups[KNOBS].rows()[row].control() {
        FieldControl::Slider(slider) => slider.value(),
        other => unreachable!("row {row} holds {other:?}"),
    }
}

/// Every request a press at `from`, a drag through `through`, and a release
/// there asked for.
fn drag(window: &mut SettingsWindow, from: Point, through: &[Point]) -> Vec<Request> {
    let theme = theme();
    let mut asked = Vec::new();
    let mut sink = damage::sink();
    let mut feed = |window: &mut SettingsWindow, event: InputEvent| {
        if let Some(request) = window.on_pointer(&event, SCALE, &theme, &mut sink) {
            asked.push(request);
        }
    };
    feed(window, InputEvent::PointerMoved { to: from });
    feed(
        window,
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
    );
    for &to in through {
        feed(window, InputEvent::PointerMoved { to });
    }
    feed(
        window,
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    );
    asked
}

fn key(window: &mut SettingsWindow, key: Key) -> (Option<Request>, Region) {
    let mut sink = damage::sink();
    let asked = window.on_key(keystroke(key), SCALE, &theme(), &mut sink);
    (asked, sink)
}

fn rendered(window: &SettingsWindow) -> Surface {
    let (width, height) = window.extent();
    let mut surface = Surface::new(width, height).expect("the window fits");
    window.render(&mut surface, SCALE, &theme());
    surface
}

#[test]
fn the_window_opens_on_the_players_choice() {
    let window = open(shown(Graphics::Ultra, Detail::FINEST));
    match window.groups[QUALITY].rows()[0].control() {
        FieldControl::Combo(combo) => assert_eq!(combo.selected_text(), Some("Ultra")),
        other => unreachable!("the mode row holds {other:?}"),
    }
    for row in 0..Knob::ALL.len() {
        assert_eq!(
            slider_value(&window, row),
            1000,
            "row {row} is not at its finest"
        );
    }
    let (width, height) = window.extent();
    assert!(width > 0 && height > 0);
}

#[test]
fn every_knob_setting_round_trips_through_its_slider() {
    for knob in Knob::ALL {
        for position in 0..knob.settings() {
            let detail = knob.set(Detail::FINEST, position);
            assert_eq!(knob.position(detail), position, "{knob:?} at {position}");
            let permille = permille_of(position, knob.settings());
            assert_eq!(position_of(permille, knob.settings()), position);
        }
        assert_eq!(
            knob.position(Detail::FINEST),
            knob.settings() - 1,
            "finest is at the right"
        );
        assert_eq!(
            knob.position(Detail::PLAINEST),
            0,
            "plainest is at the left"
        );
    }
}

#[test]
fn dragging_a_slider_previews_as_it_goes_and_settles_once() {
    let mut window = open(shown(Graphics::Ultra, Detail::FINEST));
    let slider = slider_rect(&window, 0);
    let y = slider.top() + to_i32(slider.height / 2);
    let from = Point::new(slider.right() - 1, y);
    let through: Vec<Point> = (0..24)
        .map(|i| Point::new(slider.right() - 1 - i * to_i32(slider.width) / 24, y))
        .collect();
    let asked = drag(&mut window, from, &through);

    let settles: Vec<&Request> = asked
        .iter()
        .filter(|r| matches!(r, Request::Settle(_)))
        .collect();
    assert_eq!(
        settles.len(),
        1,
        "a drag wrote {} times: {asked:?}",
        settles.len()
    );
    assert!(
        matches!(asked.last(), Some(Request::Settle(_))),
        "the write is where the drag ends"
    );
    assert!(
        asked.iter().any(|r| matches!(r, Request::Preview(_))),
        "the drag previewed nothing"
    );
    assert_eq!(
        asked.last(),
        Some(&Request::Settle(Graphics::Custom(Detail {
            lighting: Lighting::Coarse,
            ..Detail::FINEST
        }))),
        "dragged to the left end, the light is at its coarsest and nothing else moved"
    );
}

#[test]
fn a_slider_sits_on_the_setting_nearest_where_it_was_left() {
    let mut window = open(shown(Graphics::Ultra, Detail::FINEST));
    let slider = slider_rect(&window, 0);
    let y = slider.top() + to_i32(slider.height / 2);
    // Two fifths of the way along three settings is nearer the middle one.
    let at = Point::new(slider.left() + to_i32(slider.width * 2 / 5), y);
    let asked = drag(&mut window, at, &[at]);
    assert_eq!(
        asked.last(),
        Some(&Request::Settle(Graphics::Custom(Detail {
            lighting: Lighting::Medium,
            ..Detail::FINEST
        })))
    );
    assert_eq!(
        slider_value(&window, 0),
        500,
        "the thumb was left between two settings"
    );
}

#[test]
fn a_key_on_a_slider_steps_one_setting_and_settles() {
    let mut window = open(shown(Graphics::Ultra, Detail::FINEST));
    key(&mut window, Key::Named(NamedKey::Tab));
    assert_eq!(window.focus, Focus::Group(KNOBS));
    let (asked, _) = key(&mut window, Key::Named(NamedKey::Left));
    assert_eq!(
        asked,
        Some(Request::Settle(Graphics::Custom(Detail {
            lighting: Lighting::Medium,
            ..Detail::FINEST
        })))
    );
}

#[test]
fn choosing_a_mode_writes_it() {
    let mut window = open(shown(Graphics::Ultra, Detail::FINEST));
    let (opened, damage) = key(&mut window, Key::Named(NamedKey::Enter));
    assert_eq!(opened, None);
    assert!(
        window.open_group().is_some(),
        "Enter did not open the choice list"
    );
    assert!(
        !damage.is_empty(),
        "the list opened without reporting where"
    );
    // The list opens on Ultra; Auto is the one above it.
    key(&mut window, Key::Named(NamedKey::Up));
    let (asked, _) = key(&mut window, Key::Named(NamedKey::Enter));
    assert_eq!(asked, Some(Request::Settle(Graphics::Auto)));
    assert!(window.open_group().is_none());
}

#[test]
fn choosing_custom_starts_from_the_detail_on_screen() {
    let settled = Ladder::new(4).detail();
    let mut window = open(shown(Graphics::Auto, settled));
    key(&mut window, Key::Named(NamedKey::Enter));
    key(&mut window, Key::Named(NamedKey::End));
    let (asked, _) = key(&mut window, Key::Named(NamedKey::Enter));
    assert_eq!(asked, Some(Request::Settle(Graphics::Custom(settled))));
}

#[test]
fn escape_closes_and_tab_walks_every_region() {
    let mut window = open(shown(Graphics::Ultra, Detail::FINEST));
    let start = window.focus;
    for _ in 0..3 {
        key(&mut window, Key::Named(NamedKey::Tab));
    }
    assert_eq!(window.focus, start, "three tabs came back round");
    let (asked, _) = key(&mut window, Key::Named(NamedKey::Escape));
    assert_eq!(asked, Some(Request::Close));
}

#[test]
fn showing_what_is_shown_reports_nothing_and_one_knob_reports_its_row() {
    let theme = theme();
    let mut window = open(shown(Graphics::Custom(MIDDLING), MIDDLING));
    let mut sink = damage::sink();
    window.show(
        shown(Graphics::Custom(MIDDLING), MIDDLING),
        SCALE,
        &theme,
        &mut sink,
    );
    assert!(sink.is_empty(), "restating nothing cost a repaint");

    let moved = Detail {
        shadows: Shadows::Soft,
        ..MIDDLING
    };
    window.show(
        shown(Graphics::Custom(moved), moved),
        SCALE,
        &theme,
        &mut sink,
    );
    let placed = window.place(SCALE, &theme);
    let row = window.groups[KNOBS]
        .row_rect(1, placed.groups[KNOBS], SCALE, &theme)
        .expect("the shadows row is laid out");
    assert_eq!(
        sink.bounds(),
        row,
        "reported more than the one row that moved"
    );
}

#[test]
fn every_row_fits_the_window_whatever_it_says() {
    let details = [
        Detail::FINEST,
        Detail::PLAINEST,
        crate::graphics::BASIC,
        MIDDLING,
    ];
    let double = Scale::from_percent(200).expect("a valid scale");
    for (theme, scale) in [
        (Theme::dark(), SCALE),
        (Theme::light(), SCALE),
        (Theme::dark(), double),
    ] {
        let mut window = SettingsWindow::new(shown(Graphics::Ultra, Detail::FINEST), scale, &theme);
        for graphics in [Graphics::Auto, Graphics::Ultra, Graphics::Basic] {
            for detail in details {
                for readable in Resolution::ALL {
                    let mut sink = damage::sink();
                    let shown = Shown {
                        graphics,
                        detail,
                        readable,
                    };
                    window.show(shown, scale, &theme, &mut sink);
                    let placed = window.place(scale, &theme);
                    for (group, layout) in window.groups.iter().zip(placed.groups) {
                        for index in 0..group.len() {
                            assert!(
                                group.row_rect(index, layout, scale, &theme).is_some(),
                                "{graphics:?} at {detail:?}, {scale:?}, dropped row {index} of {}",
                                group.caption()
                            );
                        }
                        assert!(
                            layout.bounds.bottom() <= to_i32(window.extent().1),
                            "{} overhangs the window at {scale:?}",
                            group.caption()
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn a_render_below_the_readable_floor_says_so() {
    let plain = Detail {
        resolution: Resolution::Half,
        ..Detail::FINEST
    };
    let window = open(Shown {
        graphics: Graphics::Custom(plain),
        detail: plain,
        readable: Resolution::FourFifths,
    });
    let row = &window.groups[KNOBS].rows()[3];
    assert!(
        row.description()
            .is_some_and(|d| d.contains("may not read clearly")),
        "{:?}",
        row.description()
    );
}

#[test]
fn a_scoped_repaint_draws_what_a_whole_one_would() {
    let theme = theme();
    let mut window = open(shown(Graphics::Ultra, Detail::FINEST));
    let mut retained = rendered(&window);
    let (width, height) = window.extent();

    let mut owed = Repaint::clean();
    let report = |sink: Region, owed: &mut Repaint| owed.merge(Repaint::Parts(sink));
    let mut sink = damage::sink();
    window.show(
        shown(Graphics::Custom(MIDDLING), MIDDLING),
        SCALE,
        &theme,
        &mut sink,
    );
    report(sink, &mut owed);
    let (_, sink) = key(&mut window, Key::Named(NamedKey::Enter));
    report(sink, &mut owed);

    let area = owed.area(width, height);
    assert!(
        u64::from(area.bounds().width) * u64::from(area.bounds().height)
            < u64::from(width) * u64::from(height),
        "the change reported the whole window"
    );
    damage::paint_parts(&mut retained, area.rects(), |surface| {
        window.render(surface, SCALE, &theme);
    });
    let whole = rendered(&window);
    assert!(
        retained.pixels() == whole.pixels(),
        "the scoped repaint left pixels a whole one would have changed"
    );
}
