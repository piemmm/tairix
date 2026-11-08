//! The settings frame: its layout, the category chosen, the keyboard walk and
//! the settle it makes, Escape, the commands, the scroll, and the note.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_icon::NoArtwork;
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::Theme;

use crate::button::Button;
use crate::column::ColumnAction;
use crate::combo::ComboBox;
use crate::damage::sink;
use crate::form::{FieldAction, FieldControl, FieldGroup, FieldGroupAction, FieldRow};
use crate::number::{NumberAction, NumberField};
use crate::selector::Toggle;
use crate::settings::{SettingsAction, SettingsFrame, SettingsLayout};
use crate::tabs::Tab;
use crate::text::Keystroke;

const SCALE: Scale = Scale::ONE;

fn toggle(label: &str) -> FieldRow {
    FieldRow::new(label, FieldControl::Toggle(Toggle::new("", false)))
}

/// A plate of a number over `toggles` toggles.
fn groups(toggles: usize) -> Vec<FieldGroup> {
    let mut rows = vec![FieldRow::new(
        "Across",
        FieldControl::Number(NumberField::new(16, 1, 64)),
    )];
    rows.extend((0..toggles).map(|index| toggle(&format!("Setting {index}"))));
    vec![FieldGroup::new("GRID", rows)]
}

fn frame(toggles: usize) -> SettingsFrame {
    let categories = vec![Tab::new("General"), Tab::new("Grid"), Tab::new("Canvas")];
    SettingsFrame::new(
        categories,
        0,
        groups(toggles),
        vec![Button::labelled("Restore defaults")],
    )
}

/// The frame's layout in a `width`×`height` window, its scroll settled.
fn laid(frame: &mut SettingsFrame, (width, height): (u32, u32), theme: &Theme) -> SettingsLayout {
    let layout = frame.layout(width, height, SCALE, theme);
    frame.settle(&layout, &mut sink());
    layout
}

fn stroke(key: Key, shift: bool) -> Keystroke {
    Keystroke {
        key,
        modifiers: Modifiers {
            shift,
            ..Modifiers::default()
        },
        at_ns: 0,
    }
}

fn key(
    frame: &mut SettingsFrame,
    key: Key,
    layout: &SettingsLayout,
    theme: &Theme,
) -> Vec<SettingsAction> {
    frame
        .on_key(stroke(key, false), layout, SCALE, theme, &mut sink())
        .into_iter()
        .collect()
}

fn tab(
    frame: &mut SettingsFrame,
    back: bool,
    layout: &SettingsLayout,
    theme: &Theme,
) -> Vec<SettingsAction> {
    frame
        .on_key(
            stroke(Key::Named(NamedKey::Tab), back),
            layout,
            SCALE,
            theme,
            &mut sink(),
        )
        .into_iter()
        .collect()
}

/// A primary click at `at`, answering what the press and the release asked.
fn click(
    frame: &mut SettingsFrame,
    at: Point,
    layout: &SettingsLayout,
    theme: &Theme,
) -> Vec<SettingsAction> {
    let mut asked = Vec::new();
    for event in [
        InputEvent::PointerMoved { to: at },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    ] {
        asked.extend(frame.on_pointer(&event, layout, SCALE, theme, &mut sink()));
    }
    asked
}

fn centre(rect: Rect) -> Point {
    Point::new(
        rect.left() + to_i32(rect.width) / 2,
        rect.top() + to_i32(rect.height) / 2,
    )
}

/// Where the sidebar draws category `index`.
fn category(frame: &SettingsFrame, index: usize, layout: &SettingsLayout, theme: &Theme) -> Point {
    let rect = frame
        .sidebar
        .tab_area(index, layout.sidebar, SCALE, theme)
        .expect("the category is drawn");
    centre(rect)
}

fn settled(value: i32) -> SettingsAction {
    SettingsAction::Field(ColumnAction {
        group: 0,
        action: FieldGroupAction {
            row: 0,
            action: FieldAction::Number(NumberAction::Settled { value }),
        },
    })
}

#[test]
fn the_column_stands_beside_the_sidebar_over_the_footer_and_gains_a_bar_when_tall() {
    let theme = Theme::light();
    let mut short = frame(2);
    let layout = laid(&mut short, (600, 440), &theme);
    assert!(layout.sidebar.right() < layout.view.left());
    assert!(layout.view.bottom() < layout.footer.top());
    assert_eq!(layout.bar, None, "a short column needs no bar");
    assert_eq!(layout.extent, layout.view.height);
    let mut tall = frame(30);
    let layout = laid(&mut tall, (600, 440), &theme);
    let bar = layout.bar.expect("a tall column scrolls");
    assert_eq!(bar.left(), layout.view.right(), "the bar beside the view");
    assert!(layout.extent > layout.view.height);
    let (_, least) = short.min_size(SCALE, &theme);
    assert!(least >= short.sidebar.measured_height(SCALE, &theme));
}

#[test]
fn choosing_a_category_marks_it_and_asks_for_its_groups() {
    let theme = Theme::light();
    let mut frame = frame(2);
    let layout = laid(&mut frame, (600, 440), &theme);
    let second = category(&frame, 2, &layout, &theme);
    let asked = click(&mut frame, second, &layout, &theme);
    assert_eq!(asked, [SettingsAction::Shown { category: 2 }]);
    assert_eq!(frame.shown(), 0, "the owner shows it");
    frame.show(2, groups(1), &layout, &mut sink());
    assert_eq!(frame.shown(), 2);
    assert_eq!(frame.sidebar.selected(), Some(2), "the sidebar marks it");
    let again = click(&mut frame, second, &layout, &theme);
    assert!(
        again.is_empty(),
        "the category on show is not asked for again"
    );
}

#[test]
fn a_row_settled_by_choosing_a_category_is_of_the_category_on_show() {
    let theme = Theme::light();
    let mut frame = frame(2);
    let layout = laid(&mut frame, (600, 440), &theme);
    tab(&mut frame, false, &layout, &theme);
    tab(&mut frame, false, &layout, &theme);
    for typed in [
        Key::Named(NamedKey::End),
        Key::Named(NamedKey::Backspace),
        Key::Char('8'),
    ] {
        key(&mut frame, typed, &layout, &theme);
    }
    let other = category(&frame, 1, &layout, &theme);
    let asked = click(&mut frame, other, &layout, &theme);
    assert_eq!(asked, [settled(18), SettingsAction::Shown { category: 1 }]);
    assert_eq!(
        frame.shown(),
        0,
        "the settle is answered while it is on show"
    );
}

#[test]
fn tab_walks_the_regions_and_a_typed_number_settles_as_it_leaves() {
    let theme = Theme::light();
    let mut frame = frame(2);
    let layout = laid(&mut frame, (600, 440), &theme);
    assert!(
        tab(&mut frame, false, &layout, &theme).is_empty(),
        "onto the sidebar"
    );
    assert!(
        tab(&mut frame, false, &layout, &theme).is_empty(),
        "into the column"
    );
    assert_eq!(frame.column().cursor(), Some((0, 0)));
    for typed in [
        Key::Named(NamedKey::End),
        Key::Named(NamedKey::Backspace),
        Key::Named(NamedKey::Backspace),
        Key::Char('9'),
    ] {
        key(&mut frame, typed, &layout, &theme);
    }
    assert_eq!(
        tab(&mut frame, false, &layout, &theme),
        [settled(9)],
        "on to the command"
    );
    assert_eq!(frame.column().cursor(), None);
    assert!(
        tab(&mut frame, false, &layout, &theme).is_empty(),
        "round to the sidebar"
    );
    assert!(
        tab(&mut frame, true, &layout, &theme).is_empty(),
        "back to the command"
    );
    let pressed = key(&mut frame, Key::Char(' '), &layout, &theme);
    assert_eq!(pressed, [SettingsAction::Command { index: 0 }]);
}

#[test]
fn escape_takes_typing_back_before_it_closes() {
    let theme = Theme::light();
    let mut frame = frame(2);
    let layout = laid(&mut frame, (600, 440), &theme);
    tab(&mut frame, false, &layout, &theme);
    tab(&mut frame, false, &layout, &theme);
    key(&mut frame, Key::Named(NamedKey::End), &layout, &theme);
    key(&mut frame, Key::Named(NamedKey::Backspace), &layout, &theme);
    key(&mut frame, Key::Char('9'), &layout, &theme);
    let back = key(&mut frame, Key::Named(NamedKey::Escape), &layout, &theme);
    assert_eq!(back, [settled(16)], "the typing goes, the window stays");
    let close = key(&mut frame, Key::Named(NamedKey::Escape), &layout, &theme);
    assert_eq!(close, [SettingsAction::Close]);
}

#[test]
fn a_command_is_asked_for_when_clicked() {
    let theme = Theme::light();
    let mut frame = frame(2);
    let layout = laid(&mut frame, (600, 440), &theme);
    let width = Button::labelled("Restore defaults").measured_width(SCALE, &theme);
    let at = Point::new(
        layout.footer.left() + to_i32(width) / 2,
        layout.footer.top() + to_i32(layout.footer.height) / 2,
    );
    assert_eq!(
        click(&mut frame, at, &layout, &theme),
        [SettingsAction::Command { index: 0 }]
    );
}

#[test]
fn a_tall_column_scrolls_by_the_wheel_and_keeps_the_cursor_in_view() {
    let theme = Theme::light();
    let mut frame = frame(30);
    let layout = laid(&mut frame, (600, 440), &theme);
    let over = centre(layout.view);
    let mut damage = sink();
    let _ = frame.on_pointer(
        &InputEvent::PointerMoved { to: over },
        &layout,
        SCALE,
        &theme,
        &mut damage,
    );
    let _ = frame.on_pointer(
        &InputEvent::PointerScrolled { dx: 0, dy: 3 },
        &layout,
        SCALE,
        &theme,
        &mut damage,
    );
    let scrolled = frame.bar.model().offset();
    assert!(scrolled > 0, "the wheel scrolled the column");
    assert!(!damage.is_empty());
    let mut frame = self::frame(30);
    let layout = laid(&mut frame, (600, 440), &theme);
    tab(&mut frame, false, &layout, &theme);
    tab(&mut frame, false, &layout, &theme);
    for _ in 0..30 {
        key(&mut frame, Key::Named(NamedKey::Down), &layout, &theme);
    }
    assert_eq!(frame.column().cursor(), Some((0, 30)));
    assert!(
        frame.bar.model().offset() > 0,
        "the last row was brought into view"
    );
}

#[test]
fn the_note_is_said_in_the_footer() {
    let theme = Theme::light();
    let mut frame = frame(2);
    let layout = laid(&mut frame, (600, 440), &theme);
    let mut damage = sink();
    let said = String::from("the settings were not saved");
    assert!(frame.say(Some(said.clone()), &layout, &mut damage));
    assert!(
        !frame.say(Some(said), &layout, &mut Region::default()),
        "said once"
    );
    assert!(damage.rects().contains(&layout.footer));
    let mut surface = Surface::new(600, 440).expect("a window");
    frame.render(&mut surface, &layout, (SCALE, &theme), &mut NoArtwork);
    let danger = Color::from(theme.palette().danger).premultiply();
    let footer = layout.footer;
    let inked = (footer.left()..footer.right()).any(|x| {
        (footer.top()..footer.bottom()).any(|y| {
            surface.get(u32::try_from(x).unwrap_or(0), u32::try_from(y).unwrap_or(0))
                == Some(danger)
        })
    });
    assert!(inked, "the note is drawn in the footer");
    assert_eq!(frame.note(), Some("the settings were not saved"));
}

#[test]
fn an_open_list_keeps_the_pointer_from_the_sidebar() {
    let theme = Theme::light();
    let choices = vec![String::from("Lines"), String::from("Dots")];
    let groups = vec![FieldGroup::new(
        "STYLE",
        vec![FieldRow::new(
            "Style",
            FieldControl::Combo(ComboBox::new(choices)),
        )],
    )];
    let mut frame = SettingsFrame::new(
        vec![Tab::new("One"), Tab::new("Two")],
        0,
        groups,
        Vec::new(),
    );
    let layout = laid(&mut frame, (600, 440), &theme);
    tab(&mut frame, false, &layout, &theme);
    tab(&mut frame, false, &layout, &theme);
    key(&mut frame, Key::Named(NamedKey::Enter), &layout, &theme);
    assert!(frame.holding(), "the list is open");
    let other = category(&frame, 1, &layout, &theme);
    let pressed = click(&mut frame, other, &layout, &theme);
    assert!(
        !pressed.contains(&SettingsAction::Shown { category: 1 }),
        "{pressed:?}"
    );
    assert_eq!(frame.shown(), 0);
}

#[test]
fn a_held_scroll_arrow_steps_on_its_own_deadline() {
    use crate::scrollbar::ScrollPart;

    let theme = Theme::light();
    let mut frame = frame(30);
    let layout = laid(&mut frame, (600, 440), &theme);
    let bar = layout.bar.expect("a tall column scrolls");
    let arrow = frame
        .bar
        .part_rect(ScrollPart::Increment, bar, SCALE, &theme)
        .expect("the bar has its arrow");
    let mut damage = sink();
    for event in [
        InputEvent::PointerMoved { to: centre(arrow) },
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
    ] {
        let _ = frame.on_pointer(&event, &layout, SCALE, &theme, &mut damage);
    }
    let pressed = frame.bar.model().offset();
    assert!(pressed > 0, "the press stepped once");
    let due = frame.deadline_ns(0).expect("a held arrow repeats");
    frame.advance(due, &layout, SCALE, &theme, &mut damage);
    assert!(frame.bar.model().offset() > pressed, "and again when due");
}
