use super::*;
use tairix_appconf::{Document, Registry};

#[test]
fn the_default_posture_is_polite() {
    assert_eq!(ProbeSettings::default().posture, Posture::Polite);
    assert_eq!(Posture::default(), Posture::Polite);
}

#[test]
fn each_posture_resolves_to_its_preset() {
    assert_eq!(Posture::Passive.profile(), ProbeProfile::PASSIVE);
    assert_eq!(Posture::Polite.profile(), ProbeProfile::POLITE);
    assert_eq!(Posture::Thorough.profile(), ProbeProfile::THOROUGH);
    assert!(!Posture::Passive.profile().probe, "passive sends nothing");
}

#[test]
fn a_posture_round_trips_through_the_store() {
    for posture in Posture::ALL {
        let doc = Document::parse(&alloc::format!("probe.posture = {}\n", posture.token()))
            .expect("the line parses");
        let (settings, refused) = ProbeSettings::load(&doc);
        assert_eq!(settings.posture, posture);
        assert!(refused.is_empty());
    }
}

#[test]
fn an_unknown_value_keeps_the_default() {
    let doc = Document::parse("probe.posture = aggressive\n").expect("the line parses");
    let (settings, refused) = ProbeSettings::load(&doc);
    assert_eq!(settings.posture, Posture::default(), "the default stands");
    assert_eq!(refused, [ProbeKey::Posture], "the bad value is reported");
}

#[test]
fn the_pane_opens_on_a_posture_and_adopts_a_new_one() {
    use tairix_geometry::{Region, Scale};
    use tairix_theme::ThemeRegistry;

    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    let mut pane = ProbePane::new(Posture::Polite, Scale::ONE, theme);
    let (width, height) = pane.extent();
    assert!(width > 0 && height > 0, "the pane wants a real extent");

    let mut damage = Region::new();
    pane.adopt(Posture::Thorough, &mut damage);
    assert!(!damage.is_empty(), "adopting a new posture repaints");

    let mut again = Region::new();
    pane.adopt(Posture::Thorough, &mut again);
    assert!(again.is_empty(), "adopting the shown posture moves nothing");
}

#[test]
fn tokens_are_distinct_and_recognised() {
    for posture in Posture::ALL {
        assert_eq!(Posture::from_token(posture.token()), Some(posture));
    }
    assert_eq!(Posture::from_token("nonsense"), None);
}

fn covers(drew: &tairix_geometry::Region, rect: Rect) -> bool {
    let mut uncovered = tairix_geometry::Region::new();
    uncovered.add(rect);
    for covered in drew.rects() {
        uncovered.subtract(*covered);
    }
    uncovered.is_empty()
}

fn press(key: NamedKey) -> Keystroke {
    Keystroke {
        key: Key::Named(key),
        modifiers: tairix_input::Modifiers::default(),
        at_ns: 0,
    }
}

/// Where each posture is drawn while the chooser's list is open.
fn drawn_choices(pane: &ProbePane, scale: Scale, theme: &Theme) -> Vec<Rect> {
    let popup = pane.place(scale, theme).popup;
    let Some(FieldControl::Combo(combo)) = pane.group.rows().first().map(FieldRow::control) else {
        panic!("the pane's one row is the chooser");
    };
    (0..Posture::ALL.len())
        .map(|index| {
            combo
                .menu()
                .row_rect(index, popup, scale, theme)
                .expect("an open list draws every posture")
        })
        .collect()
}

/// Opening the chooser reports every posture its list draws, whole and inside
/// the window, so the frame presented shows the list rather than only the
/// part of it overlapping the field — from a click and from the keyboard.
#[test]
fn opening_the_chooser_reports_every_posture_it_draws() {
    use tairix_geometry::{to_i32, Point, Region};
    use tairix_input::PointerButton;
    use tairix_theme::ThemeRegistry;

    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    for scale in [Scale::ONE, Scale::from_percent(200).expect("a valid scale")] {
        let mut pane = ProbePane::new(Posture::Polite, scale, theme);
        let (width, height) = pane.extent();
        let window = Rect::new(0, 0, width, height);
        let layout = pane.place(scale, theme);
        let row = pane
            .group
            .row_rect(0, layout, scale, theme)
            .expect("the chooser's row is drawn");
        let field = pane.group.rows()[0]
            .control_rect(
                FieldLayout {
                    bounds: row,
                    ..layout
                },
                scale,
                theme,
            )
            .expect("the chooser is drawn");
        let centre = Point::new(
            field.left() + to_i32(field.width) / 2,
            field.top() + to_i32(field.height) / 2,
        );
        let mut clicked = Region::new();
        for event in [
            InputEvent::PointerMoved { to: centre },
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            },
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            },
        ] {
            assert_eq!(pane.on_pointer(&event, scale, theme, &mut clicked), None);
        }
        assert!(pane.list_open(), "the click opens the list");
        let popup = pane.place(scale, theme).popup;
        assert_eq!(
            window.intersection(&popup),
            popup,
            "the list fits the window"
        );
        for (index, choice) in drawn_choices(&pane, scale, theme).into_iter().enumerate() {
            assert_eq!(
                popup.intersection(&choice),
                choice,
                "posture {index} is whole"
            );
            assert!(
                covers(&clicked, choice),
                "posture {index} at {choice:?} was drawn but not reported"
            );
        }

        let mut closed = Region::new();
        assert_eq!(
            pane.on_key(press(NamedKey::Escape), scale, theme, &mut closed),
            None,
            "Escape closes the list, not the window"
        );
        assert!(!pane.list_open());
        assert!(covers(&closed, popup), "the plate it vacates is reported");

        let mut keyed = Region::new();
        assert_eq!(
            pane.on_key(press(NamedKey::Enter), scale, theme, &mut keyed),
            None
        );
        assert!(pane.list_open(), "Enter opens the list");
        for choice in drawn_choices(&pane, scale, theme) {
            assert!(
                covers(&keyed, choice),
                "{choice:?} was drawn but not reported"
            );
        }
    }
}
