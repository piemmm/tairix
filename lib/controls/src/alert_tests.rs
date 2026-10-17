//! Unit tests for the alert control: its content-sized extent, the RISC OS
//! layout, the keyboard and pointer answers, and the text that scrolls only
//! when its share of the screen cannot seat it.

use tairix_abi::window_ipc::{AlertButton, AlertButtonRole, AlertSeverity, AlertSpec};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::Surface;
use tairix_theme::Theme;

use super::{Alert, AlertAction, Metrics, ALERT_BUTTON_MIN_WIDTH, ALERT_MIN_WIDTH};

/// A headline of `words` four-letter words.
fn long_headline(words: usize) -> alloc::string::String {
    "word ".repeat(words).trim_end().into()
}

/// A title band and a rim, as a dialog frame adds them.
const FRAME: (u32, u32) = (2, 34);

/// A desktop of ordinary size.
const SCREEN: (u32, u32) = (1920, 1080);

fn button(label: &str, role: AlertButtonRole) -> AlertButton {
    AlertButton::new(label, role).expect("a valid label")
}

fn alert(detail: &str, buttons: &[AlertButton]) -> Alert {
    Alert::from_spec(
        &AlertSpec::new(
            AlertSeverity::Error,
            "\u{201c}report.pdf\u{201d} could not be moved",
            detail,
            buttons,
        )
        .expect("a valid spec"),
    )
}

fn ok() -> AlertButton {
    button("OK", AlertButtonRole::Default)
}

fn outer((w, h): (u32, u32)) -> (u32, u32) {
    (w + FRAME.0, h + FRAME.1)
}

fn client(extent: (u32, u32)) -> Rect {
    Rect::new(0, 0, extent.0, extent.1)
}

fn key(alert: &mut Alert, key: Key, shift: bool, bounds: Rect) -> Option<AlertAction> {
    let modifiers = Modifiers {
        shift,
        ..Modifiers::default()
    };
    let mut damage = Region::new();
    alert.on_key(
        key,
        modifiers,
        bounds,
        Scale::ONE,
        &Theme::dark(),
        &mut damage,
    )
}

fn click(alert: &mut Alert, at: Point, bounds: Rect) -> Option<AlertAction> {
    let theme = Theme::dark();
    let mut damage = Region::new();
    let mut last = None;
    for event in [
        InputEvent::PointerMoved { to: at },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    ] {
        last = alert.on_pointer(&event, bounds, Scale::ONE, &theme, &mut damage);
    }
    last
}

#[test]
fn an_alert_is_exactly_as_tall_as_its_content() {
    let theme = Theme::dark();
    let long = "word ".repeat(150);
    for detail in ["", "The volume is full.", long.as_str()] {
        for scale in [Scale::ONE, Scale::from_percent(200).expect("a scale")] {
            let alert = alert(detail, &[ok()]);
            let extent = alert.client_extent(FRAME, SCREEN, scale, &theme);
            let metrics = Metrics::of(scale, &theme);
            assert_eq!(
                extent.1,
                alert.needed_height(extent.0, &metrics, scale, &theme),
                "{detail:.20} at {scale:?}"
            );
            let layout = alert.layout(client(extent), scale, &theme);
            assert_eq!(layout.scroll_room, 0, "nothing scrolls while it fits");
        }
    }
}

#[test]
fn a_one_line_alert_is_short_and_a_long_one_grows_to_the_measure() {
    let theme = Theme::dark();
    let short = alert("", &[ok()]).client_extent(FRAME, SCREEN, Scale::ONE, &theme);
    let narrow = alert("", &[ok()]);
    let metrics = Metrics::of(Scale::ONE, &theme);
    assert!(
        outer(short).0 >= ALERT_MIN_WIDTH,
        "never narrower than the minimum"
    );
    assert!(
        short.1 < short.0 * 3 / 4,
        "a short alert is not padded out to a shape: {short:?}"
    );
    assert!(short.1 <= metrics.pad * 3 + metrics.mark + metrics.button_h * 2);
    let long = "word ".repeat(150);
    let grown = alert(&long, &[ok()]).client_extent(FRAME, SCREEN, Scale::ONE, &theme);
    let measure = Alert::measure(Scale::ONE, &theme);
    assert!(outer(grown).0 > outer(short).0);
    assert!(
        metrics.text_width(grown.0) <= measure,
        "a long text is set to the measure, not wider"
    );
    assert!(
        metrics.text_width(grown.0) + 10 * 8 >= measure,
        "it uses it"
    );
    assert!(narrow.client_extent(FRAME, SCREEN, Scale::ONE, &theme).0 <= grown.0);
}

#[test]
fn a_long_headline_wraps_whole_and_nothing_is_cut() {
    let theme = Theme::dark();
    let headline = long_headline(32);
    let spec =
        AlertSpec::new(AlertSeverity::Warning, &headline, "", &[ok()]).expect("a valid spec");
    let alert = Alert::from_spec(&spec);
    let scale = Scale::from_percent(200).expect("a scale");
    let narrow = (480, 1080);
    let extent = alert.client_extent(FRAME, narrow, scale, &theme);
    let layout = alert.layout(client(extent), scale, &theme);
    let font = layout.lines.headline.font;
    let wrapped: alloc::vec::Vec<_> = font
        .wrap_to_width(&headline, layout.text.width, usize::MAX)
        .collect();
    assert!(
        wrapped.len() > 6,
        "past the six lines a headline was cut at"
    );
    assert_eq!(
        layout.lines.headline_lines,
        wrapped.len(),
        "every line is kept"
    );
    let words: usize = wrapped
        .iter()
        .map(|line| line.text.split_whitespace().count())
        .sum();
    assert_eq!(words, 32, "every word is laid out, none elided");
    assert_eq!(
        layout.scroll_room, 0,
        "and the window is tall enough for it"
    );
}

#[test]
fn text_past_the_screens_share_scrolls_whole_and_the_buttons_stay() {
    let theme = Theme::dark();
    let lines = "line\n".repeat(200);
    let spec = AlertSpec::new(AlertSeverity::Error, &long_headline(30), &lines, &[ok()])
        .expect("a valid spec");
    let mut alert = Alert::from_spec(&spec);
    let small = (640, 480);
    let extent = alert.client_extent(FRAME, small, Scale::ONE, &theme);
    assert!(outer(extent).0 <= 640 && outer(extent).1 <= 480);
    let bounds = client(extent);
    let layout = alert.layout(bounds, Scale::ONE, &theme);
    assert!(layout.scroll_room > 0);
    let pad = i32::try_from(Metrics::of(Scale::ONE, &theme).pad).expect("small");
    assert_eq!(
        layout.buttons[0].bottom(),
        bounds.bottom() - pad,
        "the buttons stay at the foot"
    );
    assert!(layout.text.bottom() <= layout.buttons[0].top());
    key(&mut alert, Key::Named(NamedKey::Down), false, bounds);
    assert_eq!(alert.scroll, 1, "the headline scrolls too");
    key(&mut alert, Key::Named(NamedKey::End), false, bounds);
    assert_eq!(
        alert.scroll, layout.scroll_room,
        "the end shows the last line"
    );
    let (headline, detail) = layout.lines.fitting(alert.scroll, layout.text.height);
    assert_eq!(alert.scroll + headline + detail, layout.lines.total());
    key(&mut alert, Key::Named(NamedKey::Home), false, bounds);
    assert_eq!(alert.scroll, 0);
    let mut damage = Region::new();
    alert.on_pointer(
        &InputEvent::PointerScrolled { dx: 0, dy: 120 },
        bounds,
        Scale::ONE,
        &theme,
        &mut damage,
    );
    assert_eq!(alert.scroll, 3, "a detent scrolls three lines");
    assert!(!damage.is_empty());
}

#[test]
fn a_density_change_lays_the_window_out_again() {
    let theme = Theme::dark();
    let alert = alert("The volume is full.", &[ok()]);
    let one = alert.client_extent(FRAME, SCREEN, Scale::ONE, &theme);
    let two = alert.client_extent(
        FRAME,
        SCREEN,
        Scale::from_percent(200).expect("a scale"),
        &theme,
    );
    assert!(two.0 > one.0 && two.1 > one.1, "{one:?} then {two:?}");
}

#[test]
fn the_buttons_stand_along_the_foot_at_the_right() {
    let theme = Theme::dark();
    let alert = alert(
        "",
        &[
            button("Cancel", AlertButtonRole::Cancel),
            button("Replace", AlertButtonRole::Default),
        ],
    );
    let extent = alert.client_extent(FRAME, SCREEN, Scale::ONE, &theme);
    let bounds = client(extent);
    let metrics = Metrics::of(Scale::ONE, &theme);
    let layout = alert.layout(bounds, Scale::ONE, &theme);
    let [cancel, replace] = [layout.buttons[0], layout.buttons[1]];
    let pad = i32::try_from(metrics.pad).expect("small");
    assert_eq!(replace.right(), bounds.right() - pad);
    assert_eq!(replace.bottom(), bounds.bottom() - pad);
    assert_eq!(cancel.top(), replace.top());
    assert!(
        cancel.right() < replace.left(),
        "left to right in spec order"
    );
    assert!(cancel.width >= ALERT_BUTTON_MIN_WIDTH);
    assert!(
        layout.mark.right() < layout.text.left(),
        "the mark leads the text"
    );
    assert!(layout.text.bottom() <= replace.top());
}

#[test]
fn the_keyboard_starts_on_the_default_else_the_cancel_else_the_first() {
    let default = alert(
        "",
        &[
            button("Cancel", AlertButtonRole::Cancel),
            button("Save", AlertButtonRole::Default),
        ],
    );
    assert_eq!(default.focused(), 1);
    let cancel = alert(
        "",
        &[
            button("Delete", AlertButtonRole::Destructive),
            button("Cancel", AlertButtonRole::Cancel),
        ],
    );
    assert_eq!(cancel.focused(), 1);
    let plain = alert(
        "",
        &[
            button("Left", AlertButtonRole::Plain),
            button("Right", AlertButtonRole::Plain),
        ],
    );
    assert_eq!(plain.focused(), 0);
}

#[test]
fn enter_presses_the_focused_button_and_escape_the_cancel() {
    let bounds = client((400, 270));
    let mut alert = alert(
        "",
        &[
            button("Cancel", AlertButtonRole::Cancel),
            button("Keep Both", AlertButtonRole::Plain),
            button("Replace", AlertButtonRole::Default),
        ],
    );
    assert_eq!(
        key(&mut alert, Key::Named(NamedKey::Enter), false, bounds),
        Some(AlertAction::Chosen { index: 2 }),
        "the keyboard starts on the default"
    );
    assert_eq!(
        key(&mut alert, Key::Named(NamedKey::Escape), false, bounds),
        Some(AlertAction::Chosen { index: 0 })
    );
    key(&mut alert, Key::Named(NamedKey::Left), false, bounds);
    assert_eq!(
        key(&mut alert, Key::Named(NamedKey::Enter), false, bounds),
        Some(AlertAction::Chosen { index: 1 }),
        "Enter follows the focus"
    );
}

#[test]
fn a_stray_enter_never_destroys() {
    let bounds = client((400, 270));
    let mut cancel_first = alert(
        "",
        &[
            button("Cancel", AlertButtonRole::Cancel),
            button("Delete", AlertButtonRole::Destructive),
        ],
    );
    assert_eq!(
        key(
            &mut cancel_first,
            Key::Named(NamedKey::Enter),
            false,
            bounds
        ),
        Some(AlertAction::Chosen { index: 0 })
    );
    let destroy_first = alert(
        "",
        &[
            button("Delete", AlertButtonRole::Destructive),
            button("Keep", AlertButtonRole::Plain),
        ],
    );
    assert_eq!(destroy_first.focused(), 1, "the keyboard starts on Keep");
}

#[test]
fn the_focus_walks_the_row_and_space_presses_it() {
    let bounds = client((400, 270));
    let mut alert = alert(
        "",
        &[
            button("Cancel", AlertButtonRole::Cancel),
            button("Delete", AlertButtonRole::Destructive),
        ],
    );
    assert_eq!(alert.focused(), 0, "the keyboard starts on Cancel");
    key(&mut alert, Key::Named(NamedKey::Tab), false, bounds);
    assert_eq!(alert.focused(), 1, "Tab moves on to Delete");
    key(&mut alert, Key::Named(NamedKey::Right), false, bounds);
    assert_eq!(alert.focused(), 0, "the focus wraps along the row");
    key(&mut alert, Key::Named(NamedKey::Right), false, bounds);
    assert_eq!(
        key(&mut alert, Key::Char(' '), false, bounds),
        Some(AlertAction::Chosen { index: 1 }),
        "Space presses the button the user moved to"
    );
}

#[test]
fn escape_on_a_lone_button_answers_it() {
    let mut alert = alert("", &[ok()]);
    assert_eq!(
        key(
            &mut alert,
            Key::Named(NamedKey::Escape),
            false,
            client((400, 270))
        ),
        Some(AlertAction::Chosen { index: 0 })
    );
}

#[test]
fn shift_tab_moves_the_focus_back() {
    let bounds = client((400, 270));
    let mut alert = alert(
        "",
        &[
            button("A", AlertButtonRole::Plain),
            button("B", AlertButtonRole::Plain),
            button("C", AlertButtonRole::Default),
        ],
    );
    key(&mut alert, Key::Named(NamedKey::Tab), true, bounds);
    assert_eq!(alert.focused(), 1);
    key(&mut alert, Key::Named(NamedKey::Left), false, bounds);
    assert_eq!(alert.focused(), 0);
}

#[test]
fn a_click_on_a_button_chooses_it_and_a_click_beside_them_does_not() {
    let theme = Theme::dark();
    let mut alert = alert(
        "",
        &[
            button("Cancel", AlertButtonRole::Cancel),
            button("Replace", AlertButtonRole::Default),
        ],
    );
    let bounds = client(alert.client_extent(FRAME, SCREEN, Scale::ONE, &theme));
    let layout = alert.layout(bounds, Scale::ONE, &theme);
    let centre = |rect: Rect| Point::new(rect.left() + 4, rect.top() + 4);
    assert_eq!(click(&mut alert, centre(layout.text), bounds), None);
    assert_eq!(
        click(&mut alert, centre(layout.buttons[0]), bounds),
        Some(AlertAction::Chosen { index: 0 })
    );
}

#[test]
fn a_press_carried_off_its_button_chooses_nothing() {
    let theme = Theme::dark();
    let mut alert = alert("", &[ok()]);
    let bounds = client(alert.client_extent(FRAME, SCREEN, Scale::ONE, &theme));
    let layout = alert.layout(bounds, Scale::ONE, &theme);
    let on = Point::new(layout.buttons[0].left() + 2, layout.buttons[0].top() + 2);
    let mut damage = Region::new();
    for event in [
        InputEvent::PointerMoved { to: on },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerMoved {
            to: Point::new(1, 1),
        },
    ] {
        assert_eq!(
            alert.on_pointer(&event, bounds, Scale::ONE, &theme, &mut damage),
            None
        );
    }
    assert_eq!(
        alert.on_pointer(
            &InputEvent::PointerReleased {
                button: PointerButton::Primary
            },
            bounds,
            Scale::ONE,
            &theme,
            &mut damage
        ),
        None
    );
}

#[test]
fn an_alert_paints_its_mark_its_words_and_its_buttons() {
    let theme = Theme::dark();
    let alert = alert("The volume is full.", &[ok()]);
    let extent = alert.client_extent(FRAME, SCREEN, Scale::ONE, &theme);
    let mut surface = Surface::new(extent.0, extent.1).expect("a surface");
    let mark = alert.mark(Scale::ONE).expect("the mark draws");
    alert.render(
        &mut surface,
        client(extent),
        Scale::ONE,
        &theme,
        Some(&mark),
    );
    let layout = alert.layout(client(extent), Scale::ONE, &theme);
    let ground = surface.get(1, 1).expect("inside");
    let differs = |rect: Rect| {
        (0..rect.height).any(|dy| {
            (0..rect.width).any(|dx| {
                let x = u32::try_from(rect.left()).expect("inside") + dx;
                let y = u32::try_from(rect.top()).expect("inside") + dy;
                surface.get(x, y) != Some(ground)
            })
        })
    };
    assert!(differs(layout.mark), "the mark is drawn");
    assert!(differs(layout.text), "the words are drawn");
    assert!(differs(layout.buttons[0]), "the button is drawn");
}

#[test]
fn a_row_too_wide_for_the_window_wraps_and_every_button_stays_inside() {
    let theme = Theme::dark();
    let label = "A label as long as one may be..."; // 32 bytes
    let buttons = [
        button(label, AlertButtonRole::Cancel),
        button(label, AlertButtonRole::Plain),
        button(label, AlertButtonRole::Plain),
        button(label, AlertButtonRole::Default),
    ];
    let alert = alert("", &buttons);
    let small = (640, 480);
    let extent = alert.client_extent(FRAME, small, Scale::ONE, &theme);
    let bounds = client(extent);
    let rects = alert.button_rects(bounds, Scale::ONE, &theme);
    assert_eq!(rects.len(), 4);
    for rect in &rects {
        assert!(
            rect.left() >= 0 && rect.right() <= bounds.right(),
            "{rect:?} leaves {bounds:?}"
        );
        assert!(
            rect.top() >= 0 && rect.bottom() <= bounds.bottom(),
            "{rect:?} leaves {bounds:?}"
        );
    }
    for (index, a) in rects.iter().enumerate() {
        for b in &rects[index + 1..] {
            assert!(a.intersection(b).is_empty(), "{a:?} overlaps {b:?}");
        }
    }
    let layout = alert.layout(bounds, Scale::ONE, &theme);
    let highest = rects.iter().map(Rect::top).min().expect("buttons");
    assert!(
        layout.text.bottom() <= highest,
        "the text stands above every row"
    );
}
