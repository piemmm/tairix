//! Host tests for the widget-gallery model: tab identity, panel population,
//! render-without-panic on every tab, keyboard tab switching, a selector
//! reaction, radio-group single selection, and the damage reports the `Run`
//! binary presents by.

use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
use tairix_controls::testkit::keystroke;
use tairix_controls::{damage, SelectionState, WHEEL_STEP};
use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, NamedKey, PointerButton};
use tairix_raster::Surface;
use tairix_theme::{Theme, ThemeRegistry};

use crate::gallery::{Gallery, GalleryTab};
use crate::widget::DemoWidget;

/// A 14px font, as the `Run` binary resolves at the theme's UI size.
fn font() -> BitmapFont {
    BitmapFont::monospace(14)
}

/// The gallery window the `Run` binary creates, as the viewport every layout
/// derives from.
fn window() -> Rect {
    Rect::new(0, 0, 820, 620)
}

/// A press-then-release click sequence at `point`, each preceded by the move
/// that positions the pointer (as a real device reports it).
fn click(gallery: &mut Gallery, point: Point, viewport: Rect, themes: &ThemeRegistry) {
    let theme = themes.active();
    let seq = [
        InputEvent::PointerMoved { to: point },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    ];
    let mut damage = damage::sink();
    for event in &seq {
        gallery.on_pointer(event, viewport, Scale::ONE, theme, &mut damage);
    }
}

/// Drive the tab strip from the keyboard until `target` is selected.
///
/// `Home` puts the focus cursor at the first tab regardless of where it was,
/// then `Right` walks it to the target and `Enter` selects it — deterministic
/// no matter the gallery's prior state (the tab strip holds keyboard focus
/// after any selection).
fn select_tab(mut gallery: Gallery, target: GalleryTab) -> Gallery {
    let themes = ThemeRegistry::with_builtins();
    press(&mut gallery, Key::Named(NamedKey::Home), &themes);
    for _ in 0..target.index() {
        press(&mut gallery, Key::Named(NamedKey::Right), &themes);
    }
    press(&mut gallery, Key::Named(NamedKey::Enter), &themes);
    gallery
}

/// One unmodified key press, laid out at the gallery's window geometry,
/// reporting whether the view changed.
fn press(gallery: &mut Gallery, key: Key, themes: &ThemeRegistry) -> bool {
    gallery.on_key(
        keystroke(key),
        window(),
        Scale::ONE,
        themes.active(),
        &mut damage::sink(),
    )
}

#[test]
fn tab_identity_round_trips() {
    // The count is stated, because `index` answers by position in `ALL` and a
    // variant left out of the strip would silently take the first tab's index.
    assert_eq!(GalleryTab::ALL.len(), 10);
    for (i, tab) in GalleryTab::ALL.iter().enumerate() {
        assert_eq!(tab.index(), i);
        assert_eq!(GalleryTab::from_index(i), Some(*tab));
        assert!(!tab.title().is_empty());
    }
    assert_eq!(GalleryTab::from_index(GalleryTab::ALL.len()), None);
}

#[test]
fn every_panel_is_populated() {
    let gallery = Gallery::new();
    assert_eq!(gallery.current_tab(), GalleryTab::Buttons);
    for tab in GalleryTab::ALL {
        assert!(
            !crate::panels::build(tab).is_empty(),
            "panel {tab:?} has no demo widgets"
        );
    }
}

#[test]
fn renders_every_tab_without_panic() {
    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    let viewport = window();
    let mut gallery = Gallery::new();
    for tab in GalleryTab::ALL {
        gallery = select_tab(gallery, tab);
        let mut surface = Surface::new(viewport.width, viewport.height).expect("surface");
        gallery.render(&mut surface, viewport, Scale::ONE, theme, font());
        assert_eq!(gallery.current_tab(), tab);
    }
}

/// Whether any pixel of `rect` differs from `ground`.
fn inked(surface: &Surface, rect: Rect, ground: tairix_raster::Pixel) -> bool {
    let (Ok(left), Ok(top)) = (u32::try_from(rect.left()), u32::try_from(rect.top())) else {
        return false;
    };
    (top..top + rect.height)
        .flat_map(|y| (left..left + rect.width).map(move |x| (x, y)))
        .any(|(x, y)| surface.get(x, y).is_some_and(|pixel| pixel != ground))
}

/// Every row of every item of every tab is shown by the wheel, turned over
/// the body from the column's top to its end, and every item is drawn, in the
/// gallery's own window and on a screen shorter than any column. The gallery
/// never scrolled before, so the Collections tab's last items ran past its
/// window and were never drawn.
#[test]
fn every_item_of_every_tab_scrolls_into_view_and_is_drawn() {
    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    let ground = {
        let mut probe = Surface::new(1, 1).expect("surface");
        probe.fill_rect(0, 0, 1, 1, theme.palette().surface.into());
        probe.get(0, 0).expect("the probe's one pixel")
    };
    let turn = InputEvent::PointerScrolled {
        dx: 0,
        dy: SCROLL_UNITS_PER_DETENT,
    };
    for viewport in [window(), Rect::new(0, 0, 800, 300)] {
        for tab in GalleryTab::ALL {
            let mut gallery = select_tab(Gallery::new(), tab);
            let count = gallery.current_panel().len();
            // Per item: which of its own rows have been shown, and whether any
            // shown part of it was drawn.
            let mut rows: Vec<Vec<bool>> = (0..count)
                .map(|index| vec![false; gallery.current_panel()[index].height as usize])
                .collect();
            let mut drawn = vec![false; count];
            // Over the caption column, where no widget takes the wheel.
            let rest = Point::new(4, i32::try_from(viewport.height).expect("small") - 4);
            let moved = InputEvent::PointerMoved { to: rest };
            gallery.on_pointer(&moved, viewport, Scale::ONE, theme, &mut damage::sink());
            let mut reached_end = false;
            for _ in 0..200 {
                let mut surface = Surface::new(viewport.width, viewport.height).expect("surface");
                gallery.render(&mut surface, viewport, Scale::ONE, theme, font());
                for index in 0..count {
                    let Some(shown) =
                        gallery.widget_rect_for_test(index, viewport, Scale::ONE, theme)
                    else {
                        continue;
                    };
                    let (laid, offset) = gallery
                        .column_place_for_test(index, viewport, Scale::ONE, theme)
                        .expect("a laid-out item");
                    drawn[index] |= inked(&surface, shown, ground);
                    let first = shown.top() + i32::try_from(offset).expect("small") - laid.top();
                    let first = usize::try_from(first).expect("inside its item");
                    for row in &mut rows[index][first..first + shown.height as usize] {
                        *row = true;
                    }
                }
                if !gallery.on_pointer(&turn, viewport, Scale::ONE, theme, &mut damage::sink()) {
                    reached_end = true;
                    break;
                }
            }
            assert!(reached_end, "{tab:?} in {viewport:?} never reached its end");
            let hidden: Vec<usize> = (0..count)
                .filter(|&index| !drawn[index] || rows[index].iter().any(|shown| !shown))
                .collect();
            assert!(
                hidden.is_empty(),
                "{tab:?} in {viewport:?}: items {hidden:?} were never wholly shown and drawn"
            );
        }
    }
}

/// Keyboard focus scrolls the column the least that shows the widget it lands
/// on; while the column scrolls, the bar joins the focus ring and moves it from
/// the keyboard; and another tab starts from its own top.
#[test]
fn focus_reveals_what_it_lands_on_and_the_bar_scrolls_from_the_keyboard() {
    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    let offset = |gallery: &Gallery| {
        gallery
            .column_place_for_test(0, window(), Scale::ONE, theme)
            .map(|(_, offset)| offset)
    };
    let mut gallery = select_tab(Gallery::new(), GalleryTab::Collections);
    let last = gallery.current_panel().len() - 1;
    let height = gallery.current_panel()[last].height;
    let shown = |gallery: &Gallery| {
        gallery
            .widget_rect_for_test(last, window(), Scale::ONE, theme)
            .map_or(0, |rect| rect.height)
    };
    assert!(
        shown(&gallery) < height,
        "the last item starts below the fold"
    );

    for _ in 0..=last {
        press(&mut gallery, Key::Named(NamedKey::Tab), &themes);
    }
    assert_eq!(shown(&gallery), height, "focus on it scrolled it into view");

    press(&mut gallery, Key::Named(NamedKey::Tab), &themes);
    assert!(press(&mut gallery, Key::Named(NamedKey::Home), &themes));
    assert_eq!(
        offset(&gallery),
        Some(0),
        "the focused bar scrolls to the top"
    );
    assert!(press(&mut gallery, Key::Named(NamedKey::End), &themes));
    assert!(offset(&gallery).is_some_and(|at| at > 0), "and to the end");

    // The ring wraps from the bar back to the strip.
    press(&mut gallery, Key::Named(NamedKey::Tab), &themes);
    let gallery = select_tab(
        select_tab(gallery, GalleryTab::Bars),
        GalleryTab::Collections,
    );
    assert_eq!(
        offset(&gallery),
        Some(0),
        "a tab switched to starts at its top"
    );
}

#[test]
fn keyboard_switches_tabs() {
    let gallery = select_tab(Gallery::new(), GalleryTab::Selectors);
    assert_eq!(gallery.current_tab(), GalleryTab::Selectors);
}

#[test]
fn focused_toggle_flips_on_space() {
    let mut gallery = select_tab(Gallery::new(), GalleryTab::Selectors);

    // The first Selectors item is the "Wi-Fi" toggle, which starts on.
    assert!(toggle_on(&gallery, 0));

    // Tab once to focus that first item, then Space actuates it.
    let themes = ThemeRegistry::with_builtins();
    press(&mut gallery, Key::Named(NamedKey::Tab), &themes);
    let changed = press(&mut gallery, Key::Char(' '), &themes);

    assert!(changed);
    assert!(
        !toggle_on(&gallery, 0),
        "Space should have flipped the toggle off"
    );
}

#[test]
fn radio_group_keeps_single_selection() {
    let mut gallery = select_tab(Gallery::new(), GalleryTab::Selectors);

    // The Selectors panel ends with two radios: index 5 (off) and 6 (on).
    assert!(!radio_selected(&gallery, 5));
    assert!(radio_selected(&gallery, 6));

    // Focus the first radio (item 5): Tab moves Tabs -> item0 .. -> item5.
    let themes = ThemeRegistry::with_builtins();
    for _ in 0..6 {
        press(&mut gallery, Key::Named(NamedKey::Tab), &themes);
    }
    let changed = press(&mut gallery, Key::Char(' '), &themes);

    assert!(changed);
    assert!(
        radio_selected(&gallery, 5),
        "the actuated radio should be selected"
    );
    assert!(
        !radio_selected(&gallery, 6),
        "the sibling radio should have been cleared"
    );
}

#[test]
fn pointer_click_selects_a_checkbox() {
    let themes = ThemeRegistry::with_builtins();
    let viewport = window();
    let mut gallery = select_tab(Gallery::new(), GalleryTab::Selectors);

    // The fourth Selectors item is the "Accept terms" checkbox (checked).
    assert_eq!(checkbox_selection(&gallery, 3), SelectionState::Selected);

    // Click the centre of that checkbox's actual on-screen rectangle.
    let rect = gallery
        .widget_rect_for_test(3, viewport, Scale::ONE, themes.active())
        .expect("checkbox rect");
    let centre = Point::new(
        rect.left() + i32::try_from(rect.width / 2).unwrap_or(0),
        rect.top() + i32::try_from(rect.height / 2).unwrap_or(0),
    );
    click(&mut gallery, centre, viewport, &themes);

    assert_eq!(
        checkbox_selection(&gallery, 3),
        SelectionState::Unselected,
        "clicking the checked checkbox should clear it"
    );
}

fn toggle_on(gallery: &Gallery, index: usize) -> bool {
    match &gallery.current_panel()[index].widget {
        DemoWidget::Toggle(t) => t.is_on(),
        other => panic!("item {index} is not a toggle: {other:?}"),
    }
}

fn radio_selected(gallery: &Gallery, index: usize) -> bool {
    match &gallery.current_panel()[index].widget {
        DemoWidget::Radio(r) => r.is_selected(),
        other => panic!("item {index} is not a radio: {other:?}"),
    }
}

fn checkbox_selection(gallery: &Gallery, index: usize) -> SelectionState {
    match &gallery.current_panel()[index].widget {
        DemoWidget::Checkbox(c) => c.selection(),
        other => panic!("item {index} is not a checkbox: {other:?}"),
    }
}

/// Drives the gallery while proving that every pixel a round changes lies
/// inside what that round reported.
///
/// This is the invariant the `Run` binary's narrowed present rests on: it
/// converts and declares only the reported rectangle, so a pixel that changed
/// outside it is one the session never copies — a stale pixel left on screen.
/// Reporting *more* than changed is safe and passes here; reporting less does
/// not.
struct Prover {
    gallery: Gallery,
    theme: Theme,
    /// The frame as it stands after every round proven so far.
    shown: Surface,
}

impl Prover {
    fn new() -> Self {
        let gallery = Gallery::new();
        let theme = ThemeRegistry::with_builtins().active().clone();
        let shown = painted(&gallery, &theme);
        Self {
            gallery,
            theme,
            shown,
        }
    }

    /// Run one round through `act`, then assert its report covered it.
    fn prove(&mut self, what: &str, act: impl FnOnce(&mut Gallery, &Theme, &mut Region)) {
        let Self {
            gallery,
            theme,
            shown,
        } = self;
        let mut reported = damage::sink();
        act(gallery, theme, &mut reported);
        let after = painted(gallery, theme);
        let width = i32::try_from(after.width()).expect("window width fits an i32");
        for (i, (was, now)) in shown.pixels().iter().zip(after.pixels()).enumerate() {
            if was == now {
                continue;
            }
            let at = i32::try_from(i).expect("pixel index fits an i32");
            let point = Point::new(at % width, at / width);
            assert!(
                reported.contains(point),
                "{what}: ({}, {}) changed but was not reported; reported {:?}",
                point.x,
                point.y,
                reported.rects()
            );
        }
        *shown = after;
    }

    /// Move the pointer to `point`, then press and release there, proving each.
    fn prove_click(&mut self, what: &str, point: Point) {
        for (step, event) in [
            ("move", InputEvent::PointerMoved { to: point }),
            (
                "press",
                InputEvent::PointerPressed {
                    button: PointerButton::Primary,
                },
            ),
            (
                "release",
                InputEvent::PointerReleased {
                    button: PointerButton::Primary,
                },
            ),
        ] {
            self.prove(
                &alloc::format!("{what} {step}"),
                |gallery, theme, damage| {
                    gallery.on_pointer(&event, window(), Scale::ONE, theme, damage);
                },
            );
        }
    }

    /// Press `key`, proving the round.
    fn prove_key(&mut self, what: &str, key: Key) {
        self.prove(what, |gallery, theme, damage| {
            gallery.on_key(keystroke(key), window(), Scale::ONE, theme, damage);
        });
    }
}

/// The gallery rendered whole at the window geometry the `Run` binary uses.
fn painted(gallery: &Gallery, theme: &Theme) -> Surface {
    let viewport = window();
    let mut surface = Surface::new(viewport.width, viewport.height).expect("surface");
    gallery.render(&mut surface, viewport, Scale::ONE, theme, font());
    surface
}

/// The centre of demo item `index`'s on-screen rectangle.
fn item_centre(gallery: &Gallery, theme: &Theme, index: usize) -> Option<Point> {
    let rect = gallery.widget_rect_for_test(index, window(), Scale::ONE, theme)?;
    Some(Point::new(
        rect.left() + i32::try_from(rect.width / 2).unwrap_or(0),
        rect.top() + i32::try_from(rect.height / 2).unwrap_or(0),
    ))
}

/// The centre of tab `index`'s cell in the strip.
fn tab_centre(index: usize) -> Point {
    let viewport = window();
    let span = viewport.width / u32::try_from(GalleryTab::ALL.len()).expect("tab count fits");
    let x = i32::try_from(span).unwrap_or(0) * i32::try_from(index).unwrap_or(0)
        + i32::try_from(span / 2).unwrap_or(0);
    Point::new(x, viewport.top() + 8)
}

#[test]
fn a_field_rows_choice_list_reports_the_pixels_it_covers() {
    let mut prover = Prover::new();
    prover.gallery = select_tab(prover.gallery, GalleryTab::Forms);
    prover.shown = painted(&prover.gallery, &prover.theme);

    // Tab puts the ring on the only item — the field group — whose own cursor
    // then walks down to the choice row and opens its list.
    prover.prove_key("focus the group", Key::Named(NamedKey::Tab));
    prover.prove_key("cursor to the choice row", Key::Named(NamedKey::Down));
    prover.prove_key("open the list", Key::Char(' '));
    assert!(
        field_group_popup_open(&prover.gallery),
        "the choice row's list should be showing"
    );
    prover.prove_key("close the list", Key::Named(NamedKey::Escape));
    assert!(!field_group_popup_open(&prover.gallery));
}

/// The Forms tab's group is given room for every row it demonstrates: a row
/// the plate had to omit is a control the catalogue silently stops showing.
#[test]
fn the_forms_group_seats_every_row_it_demonstrates() {
    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    let gallery = select_tab(Gallery::new(), GalleryTab::Forms);
    let rect = gallery
        .widget_rect_for_test(0, window(), Scale::ONE, theme)
        .expect("the group is laid out");
    let DemoWidget::FieldGroup(group) = &gallery.current_panel()[0].widget else {
        panic!("the Forms tab shows a field group");
    };
    assert!(
        group
            .rows()
            .iter()
            .any(|row| matches!(row.control(), tairix_controls::FieldControl::Flags(_))),
        "the flag-set slot is demonstrated"
    );
    let column = group.slot_column(rect.width, Scale::ONE, theme);
    let needed = group.measured_height(rect.width, column, Scale::ONE, theme);
    assert!(
        needed <= rect.height,
        "the group needs {needed} of the {} it is given",
        rect.height
    );
    assert!(group
        .row_rect(
            group.len() - 1,
            tairix_controls::FieldLayout::new(rect, column),
            Scale::ONE,
            theme
        )
        .is_some());
}

/// The picture choice is given room for all of its pictures.
#[test]
fn the_forms_picture_choice_seats_every_picture() {
    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    let gallery = select_tab(Gallery::new(), GalleryTab::Forms);
    // The item may start below the fold, so it is judged at the height it
    // is given rather than the part the window shows.
    let shown = gallery
        .widget_rect_for_test(1, window(), Scale::ONE, theme)
        .expect("the chooser is laid out");
    let item = &gallery.current_panel()[1];
    let rect = Rect::new(shown.left(), shown.top(), shown.width, item.height);
    let DemoWidget::FieldGroup(group) = &item.widget else {
        panic!("the Forms tab shows a picture choice");
    };
    let column = group.slot_column(rect.width, Scale::ONE, theme);
    let needed = group.measured_height(rect.width, column, Scale::ONE, theme);
    assert!(
        needed <= rect.height,
        "the chooser needs {needed} of the {} it is given",
        rect.height
    );
    let pictures = group.pictures().expect("a chooser");
    let layout = tairix_controls::FieldLayout::new(rect, column);
    let bounds = group
        .row_rect(0, layout, Scale::ONE, theme)
        .expect("the chooser is seated");
    for index in 0..pictures.len() {
        assert!(
            pictures
                .item_rect(index, bounds, Scale::ONE, theme)
                .is_some_and(|tile| tile.intersection(&rect) == tile),
            "picture {index} is cut"
        );
    }
}

/// Whether the Forms panel's field group is showing a choice list.
fn field_group_popup_open(gallery: &Gallery) -> bool {
    gallery
        .current_panel()
        .iter()
        .any(|item| match &item.widget {
            DemoWidget::FieldGroup(group) => group
                .rows()
                .iter()
                .any(tairix_controls::FieldRow::popup_open),
            _ => false,
        })
}

#[test]
fn every_round_reports_every_pixel_it_changes() {
    let mut prover = Prover::new();
    for tab in GalleryTab::ALL {
        // A list the last panel's walk left open holds the pointer, so the
        // first click on the strip may only close it.
        prover.prove_click(&alloc::format!("{tab:?} tab"), tab_centre(tab.index()));
        if prover.gallery.current_tab() != tab {
            prover.prove_click(
                &alloc::format!("{tab:?} tab again"),
                tab_centre(tab.index()),
            );
        }
        assert_eq!(prover.gallery.current_tab(), tab);

        // Hover, press and release each widget in turn: enter/leave marks, the
        // press look, and the value the owner writes back on release.
        let items = prover.gallery.current_panel().len();
        for index in 0..items {
            let Some(centre) = item_centre(&prover.gallery, &prover.theme, index) else {
                continue;
            };
            prover.prove_click(&alloc::format!("{tab:?} item {index}"), centre);
        }

        // Then walk the whole focus ring and actuate each stop from the
        // keyboard, which is the path that moves the ring between two widgets.
        for step in 0..=items {
            prover.prove_key(
                &alloc::format!("{tab:?} focus step {step}"),
                Key::Named(NamedKey::Tab),
            );
            prover.prove_key(&alloc::format!("{tab:?} actuate {step}"), Key::Char(' '));
        }
    }
}

#[test]
fn a_hover_reports_the_widget_it_entered_and_nothing_else() {
    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    let mut gallery = select_tab(Gallery::new(), GalleryTab::Buttons);
    let centre = item_centre(&gallery, theme, 0).expect("first item is laid out");
    let rect = gallery
        .widget_rect_for_test(0, window(), Scale::ONE, theme)
        .expect("first item is laid out");

    let mut reported = damage::sink();
    gallery.on_pointer(
        &InputEvent::PointerMoved { to: centre },
        window(),
        Scale::ONE,
        theme,
        &mut reported,
    );
    assert_eq!(reported.rects(), &[rect], "the hovered widget, exactly");

    // A second sample inside the same widget changes nothing and costs nothing.
    let mut again = damage::sink();
    gallery.on_pointer(
        &InputEvent::PointerMoved {
            to: Point::new(centre.x + 1, centre.y),
        },
        window(),
        Scale::ONE,
        theme,
        &mut again,
    );
    assert!(
        again.is_empty(),
        "a sample inside one widget reports nothing"
    );
}

#[test]
fn a_tab_switch_reports_the_content_it_redraws() {
    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    let mut gallery = Gallery::new();

    // The keyboard cursor starts off the strip, so the first Right lands it on
    // the first tab and the second moves it to the next one.
    for _ in 0..2 {
        press(&mut gallery, Key::Named(NamedKey::Right), &themes);
    }
    let mut reported = damage::sink();
    gallery.on_key(
        keystroke(Key::Named(NamedKey::Enter)),
        window(),
        Scale::ONE,
        theme,
        &mut reported,
    );
    assert_ne!(
        gallery.current_tab(),
        GalleryTab::Buttons,
        "Enter selected the tab the cursor was on"
    );

    // A different panel is drawn, so the report must cover the content band and
    // not merely the two tab cells the selection moved between.
    let content = gallery
        .widget_rect_for_test(0, window(), Scale::ONE, theme)
        .expect("first item is laid out");
    assert!(
        reported.contains(Point::new(content.left(), content.top())),
        "the content band is reported: {:?}",
        reported.rects()
    );
}

// ---- pointer routing -----------------------------------------------------

/// One unmodified pointer event at the gallery's window geometry.
fn point(gallery: &mut Gallery, event: &InputEvent, damage: &mut Region) -> bool {
    let themes = ThemeRegistry::with_builtins();
    gallery.on_pointer(event, window(), Scale::ONE, themes.active(), damage)
}

/// The on-screen rectangle of demo item `index`.
fn item_rect(gallery: &Gallery, index: usize) -> Rect {
    let themes = ThemeRegistry::with_builtins();
    gallery
        .widget_rect_for_test(index, window(), Scale::ONE, themes.active())
        .expect("the item is laid out")
}

/// The centre of `rect`.
fn centre_of(rect: Rect) -> Point {
    Point::new(
        rect.left() + i32::try_from(rect.width / 2).unwrap_or(0),
        rect.top() + i32::try_from(rect.height / 2).unwrap_or(0),
    )
}

/// The pointer state demo item `index` draws, for a widget that has one.
fn pointer_state(gallery: &Gallery, index: usize) -> tairix_controls::PointerState {
    match &gallery.current_panel()[index].widget {
        DemoWidget::Button(b) => b.state().pointer,
        DemoWidget::Checkbox(c) => c.state().pointer,
        DemoWidget::Toggle(t) => t.state().pointer,
        other => panic!("item {index} has no pointer state this test reads: {other:?}"),
    }
}

/// A panel switched from the keyboard moves the hover with it: the widget the
/// pointer rested on is told the pointer left, and the widget of the panel
/// switched in that lies beneath the resting pointer shows it.
#[test]
fn a_keyboard_panel_switch_moves_the_hover_with_the_panel() {
    let themes = ThemeRegistry::with_builtins();
    let hover = tairix_controls::PointerState::Hover;
    let mut gallery = select_tab(Gallery::new(), GalleryTab::Buttons);
    let resting = centre_of(item_rect(&gallery, 0));
    let moved = |to: Point| InputEvent::PointerMoved { to };
    point(&mut gallery, &moved(resting), &mut damage::sink());
    assert_eq!(pointer_state(&gallery, 0), hover);

    // Focus is on the strip, so these switch panels with the pointer still.
    press(&mut gallery, Key::Named(NamedKey::Right), &themes);
    press(&mut gallery, Key::Named(NamedKey::Enter), &themes);
    assert_eq!(gallery.current_tab(), GalleryTab::Selectors);
    assert!(item_rect(&gallery, 0).contains(resting));
    assert_eq!(pointer_state(&gallery, 0), hover, "the toggle beneath it");

    // Off every widget, then back: nothing is under the pointer, so the
    // button it rested on before must not come back hovered.
    point(
        &mut gallery,
        &moved(Point::new(2, resting.y)),
        &mut damage::sink(),
    );
    press(&mut gallery, Key::Named(NamedKey::Left), &themes);
    press(&mut gallery, Key::Named(NamedKey::Enter), &themes);
    assert_eq!(gallery.current_tab(), GalleryTab::Buttons);
    assert_ne!(pointer_state(&gallery, 0), hover);
}

/// Whether every pixel of `rect` lies in one of the rectangles `reported`
/// holds, not merely inside their bounding box.
fn covers(reported: &Region, rect: Rect) -> bool {
    let mut uncovered = Region::new();
    uncovered.add(rect);
    for part in reported.rects() {
        uncovered.subtract(*part);
    }
    uncovered.is_empty()
}

/// The regression: every event went to whichever widget last took a press, so
/// the next widget clicked had never seen the pointer arrive and ignored the
/// click as landing somewhere else. Each widget acts on its first click.
#[test]
fn a_second_widget_acts_on_its_first_click() {
    let themes = ThemeRegistry::with_builtins();
    let mut gallery = select_tab(Gallery::new(), GalleryTab::Selectors);
    // Items 3 and 4 are the checked and the mixed checkbox.
    assert_eq!(checkbox_selection(&gallery, 3), SelectionState::Selected);
    assert_eq!(checkbox_selection(&gallery, 4), SelectionState::Mixed);

    let (checked, mixed) = (item_rect(&gallery, 3), item_rect(&gallery, 4));
    click(&mut gallery, centre_of(checked), window(), &themes);
    assert_eq!(checkbox_selection(&gallery, 3), SelectionState::Unselected);
    click(&mut gallery, centre_of(mixed), window(), &themes);
    assert_ne!(
        checkbox_selection(&gallery, 4),
        SelectionState::Mixed,
        "the second checkbox acted on the one click"
    );
}

/// A hover follows the pointer from widget to widget: the one it left drops
/// its hover look, and both are reported.
#[test]
fn the_widget_the_pointer_leaves_drops_its_hover() {
    let mut gallery = select_tab(Gallery::new(), GalleryTab::Buttons);
    let (first, second) = (item_rect(&gallery, 0), item_rect(&gallery, 1));
    let hover = tairix_controls::PointerState::Hover;

    // Focus on a widget used to take every later event, so press one first.
    click(
        &mut gallery,
        centre_of(first),
        window(),
        &ThemeRegistry::with_builtins(),
    );
    assert_eq!(pointer_state(&gallery, 0), hover);

    let mut damage = damage::sink();
    point(
        &mut gallery,
        &InputEvent::PointerMoved {
            to: centre_of(second),
        },
        &mut damage,
    );
    assert_ne!(pointer_state(&gallery, 0), hover, "the widget it left");
    assert_eq!(pointer_state(&gallery, 1), hover, "the widget it entered");
    assert!(covers(&damage, first) && covers(&damage, second));
}

/// The regression the `Run` binary presented by: a hover changes no value, so
/// it answered "nothing to repaint" and the look it reported was never shown.
#[test]
fn a_hover_asks_for_a_repaint() {
    let mut gallery = select_tab(Gallery::new(), GalleryTab::Buttons);
    let moved = InputEvent::PointerMoved {
        to: centre_of(item_rect(&gallery, 0)),
    };
    assert!(point(&mut gallery, &moved, &mut damage::sink()));
}

/// A panel returned to shows no stale focus ring: switching panels puts focus
/// on the strip.
#[test]
fn a_panel_returned_to_shows_no_stale_focus_ring() {
    let themes = ThemeRegistry::with_builtins();
    let mut gallery = select_tab(Gallery::new(), GalleryTab::Selectors);
    press(&mut gallery, Key::Named(NamedKey::Tab), &themes);
    let focused = |gallery: &Gallery| match &gallery.current_panel()[0].widget {
        DemoWidget::Toggle(t) => t.state().focus.focused,
        other => panic!("item 0 is the first toggle, not {other:?}"),
    };
    assert!(focused(&gallery));

    click(
        &mut gallery,
        tab_centre(GalleryTab::Buttons.index()),
        window(),
        &themes,
    );
    click(
        &mut gallery,
        tab_centre(GalleryTab::Selectors.index()),
        window(),
        &themes,
    );
    assert_eq!(gallery.current_tab(), GalleryTab::Selectors);
    assert!(!focused(&gallery), "the ring stays on the strip");
}

// ---- open lists ----------------------------------------------------------

/// An open choice list holds the pointer: a click on one of its rows chooses
/// it even where the list hangs over another widget, which never sees it.
#[test]
fn an_open_list_takes_the_click_over_the_widget_beneath() {
    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    let mut gallery = select_tab(Gallery::new(), GalleryTab::Choice);
    let field = item_rect(&gallery, 0);
    click(&mut gallery, centre_of(field), window(), &themes);
    let DemoWidget::ComboBox(combo) = &gallery.current_panel()[0].widget else {
        panic!("the Choice tab opens on a combo box");
    };
    assert!(combo.is_expanded());
    let popup = combo.popup_rect(field, window(), Scale::ONE, theme);
    let below = item_rect(&gallery, 1);
    let middle_row = Point::new(
        popup.left() + 8,
        popup.top() + i32::try_from(popup.height / 2).unwrap_or(0),
    );
    assert!(
        below.contains(middle_row),
        "the list hangs over the widget beneath"
    );

    click(&mut gallery, middle_row, window(), &themes);
    let DemoWidget::ComboBox(combo) = &gallery.current_panel()[0].widget else {
        panic!("the item kept its kind");
    };
    assert!(!combo.is_expanded(), "choosing closes the list");
    assert_eq!(combo.selected(), Some(1), "the middle row was chosen");
    let DemoWidget::ComboBox(beneath) = &gallery.current_panel()[1].widget else {
        panic!("the second Choice item is a combo box");
    };
    assert!(!beneath.is_expanded(), "the widget beneath saw nothing");
}

/// An open list holds the keyboard too: `Tab` does not walk focus off it and
/// leave it open while another list opens.
#[test]
fn an_open_list_keeps_the_keyboard_until_it_closes() {
    let themes = ThemeRegistry::with_builtins();
    let mut gallery = select_tab(Gallery::new(), GalleryTab::Choice);
    press(&mut gallery, Key::Named(NamedKey::Tab), &themes);
    press(&mut gallery, Key::Char(' '), &themes);
    press(&mut gallery, Key::Named(NamedKey::Tab), &themes);
    press(&mut gallery, Key::Char(' '), &themes);
    let expanded = |gallery: &Gallery, index: usize| match &gallery.current_panel()[index].widget {
        DemoWidget::ComboBox(combo) => combo.is_expanded(),
        other => panic!("item {index} is a combo box, not {other:?}"),
    };
    assert!(!expanded(&gallery, 1), "no second list opened");

    press(&mut gallery, Key::Named(NamedKey::Escape), &themes);
    assert!(!expanded(&gallery, 0));
    press(&mut gallery, Key::Named(NamedKey::Tab), &themes);
    press(&mut gallery, Key::Char(' '), &themes);
    assert!(expanded(&gallery, 1), "once closed, Tab walks on");
}

// ---- the wheel -----------------------------------------------------------

/// The regression: the wheel reached whichever widget held keyboard focus, so
/// the bar under the pointer never scrolled. It scrolls one wheel step a
/// detent, reporting its own rectangle, and the focused bar stays put.
#[test]
fn the_wheel_scrolls_the_bar_under_the_pointer() {
    let themes = ThemeRegistry::with_builtins();
    let mut gallery = select_tab(Gallery::new(), GalleryTab::Bars);
    // Items 1 and 2 are the vertical and the horizontal bar.
    let (vertical, horizontal) = (item_rect(&gallery, 1), item_rect(&gallery, 2));
    let offset = |gallery: &Gallery, index: usize| match &gallery.current_panel()[index].widget {
        DemoWidget::ScrollBar(bar) => bar.model().offset(),
        other => panic!("item {index} is a scroll bar, not {other:?}"),
    };
    // A click on the horizontal bar's thumb focuses it without scrolling it.
    click(&mut gallery, centre_of(horizontal), window(), &themes);
    let DemoWidget::ScrollBar(bar) = &gallery.current_panel()[2].widget else {
        panic!("item 2 is the horizontal bar");
    };
    assert!(bar.state().focus.focused);
    let (was_vertical, was_horizontal) = (offset(&gallery, 1), offset(&gallery, 2));

    let mut damage = damage::sink();
    point(
        &mut gallery,
        &InputEvent::PointerMoved {
            to: centre_of(vertical),
        },
        &mut damage::sink(),
    );
    assert!(point(
        &mut gallery,
        &InputEvent::PointerScrolled {
            dx: 0,
            dy: SCROLL_UNITS_PER_DETENT,
        },
        &mut damage
    ));
    assert_eq!(
        offset(&gallery, 1),
        was_vertical + u64::from(WHEEL_STEP),
        "one wheel step at 100%"
    );
    assert_eq!(
        offset(&gallery, 2),
        was_horizontal,
        "the focused bar stays put"
    );
    assert!(covers(&damage, vertical));
}

/// A wheel delivering a detent a unit at a time scrolls as far as one
/// delivering it whole: what each turn leaves short is carried.
#[test]
fn a_fine_wheel_adds_up_to_whole_steps() {
    let mut gallery = select_tab(Gallery::new(), GalleryTab::Bars);
    let vertical = item_rect(&gallery, 1);
    let offset = |gallery: &Gallery| match &gallery.current_panel()[1].widget {
        DemoWidget::ScrollBar(bar) => bar.model().offset(),
        other => panic!("item 1 is a scroll bar, not {other:?}"),
    };
    let was = offset(&gallery);
    point(
        &mut gallery,
        &InputEvent::PointerMoved {
            to: centre_of(vertical),
        },
        &mut damage::sink(),
    );
    for _ in 0..SCROLL_UNITS_PER_DETENT {
        point(
            &mut gallery,
            &InputEvent::PointerScrolled { dx: 0, dy: 1 },
            &mut damage::sink(),
        );
    }
    assert_eq!(offset(&gallery), was + u64::from(WHEEL_STEP));
}

/// The text area scrolls its own lines under the wheel.
#[test]
fn the_wheel_scrolls_the_text_area_under_the_pointer() {
    let mut gallery = select_tab(Gallery::new(), GalleryTab::Text);
    // Item 5 is the text area shown with more text than it has room for.
    let area = item_rect(&gallery, 5);
    let scrolled = |gallery: &Gallery| match &gallery.current_panel()[5].widget {
        DemoWidget::TextArea(area) => area.scroll_offset(),
        other => panic!("item 5 is the text area, not {other:?}"),
    };
    assert_eq!(scrolled(&gallery), 0);
    point(
        &mut gallery,
        &InputEvent::PointerMoved {
            to: centre_of(area),
        },
        &mut damage::sink(),
    );
    let mut damage = damage::sink();
    point(
        &mut gallery,
        &InputEvent::PointerScrolled {
            dx: 0,
            dy: SCROLL_UNITS_PER_DETENT,
        },
        &mut damage,
    );
    assert!(scrolled(&gallery) > 0, "the lines moved");
    assert!(covers(&damage, area));
}

/// The toolbar strip takes the wheel through its own entry, one tool a
/// detent, when it is too narrow for its tools.
#[test]
fn the_wheel_scrolls_a_toolbar_too_narrow_for_its_tools() {
    let themes = ThemeRegistry::with_builtins();
    let theme = themes.active();
    let narrow = Rect::new(0, 0, 360, 620);
    let mut gallery = select_tab(Gallery::new(), GalleryTab::Bars);
    let strip = gallery
        .widget_rect_for_test(0, narrow, Scale::ONE, theme)
        .expect("the toolbar is laid out");
    let first = |gallery: &Gallery| match &gallery.current_panel()[0].widget {
        DemoWidget::Toolbar(bar) => bar.scroll_model(strip, Scale::ONE, theme).offset(),
        other => panic!("item 0 is the toolbar, not {other:?}"),
    };
    assert_eq!(first(&gallery), 0);
    gallery.on_pointer(
        &InputEvent::PointerMoved {
            to: centre_of(strip),
        },
        narrow,
        Scale::ONE,
        theme,
        &mut damage::sink(),
    );
    assert!(gallery.on_pointer(
        &InputEvent::PointerScrolled {
            dx: 0,
            dy: SCROLL_UNITS_PER_DETENT,
        },
        narrow,
        Scale::ONE,
        theme,
        &mut damage::sink(),
    ));
    assert_eq!(first(&gallery), 1, "one tool a detent");
}

/// Every wheel turn reports every pixel it changes, over every widget of
/// every panel.
#[test]
fn every_wheel_turn_reports_every_pixel_it_changes() {
    let mut prover = Prover::new();
    for tab in GalleryTab::ALL {
        prover.prove_click(&alloc::format!("{tab:?} tab"), tab_centre(tab.index()));
        assert_eq!(prover.gallery.current_tab(), tab);
        for index in 0..prover.gallery.current_panel().len() {
            let Some(centre) = item_centre(&prover.gallery, &prover.theme, index) else {
                continue;
            };
            prover.prove(
                &alloc::format!("{tab:?} item {index} hover"),
                |gallery, theme, damage| {
                    gallery.on_pointer(
                        &InputEvent::PointerMoved { to: centre },
                        window(),
                        Scale::ONE,
                        theme,
                        damage,
                    );
                },
            );
            for dy in [SCROLL_UNITS_PER_DETENT, -SCROLL_UNITS_PER_DETENT / 3] {
                prover.prove(
                    &alloc::format!("{tab:?} item {index} wheel {dy}"),
                    |gallery, theme, damage| {
                        gallery.on_pointer(
                            &InputEvent::PointerScrolled { dx: 0, dy },
                            window(),
                            Scale::ONE,
                            theme,
                            damage,
                        );
                    },
                );
            }
        }
    }
}
