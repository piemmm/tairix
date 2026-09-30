//! Host tests for the composed window: input in, commands and requests and
//! damage out.

use alloc::string::String;
use alloc::vec::Vec;

use tairix_abi::time::Duration64;
use tairix_abi::window_ipc::{AppMenuItemId, AppMenuMark, AppMenuRowView, SCROLL_UNITS_PER_DETENT};
use tairix_controls::WHEEL_STEP;
use tairix_font::BitmapFont;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_syntax::Format;
use tairix_theme::{TextRole, Theme, ThemeRegistry};

use super::{
    Access, Action, MenuKind, Request, View, APP_TITLE, CHECK_SETTLE_NS, ENDINGS, INDENTS, MODES,
    PLAIN_ACTIONS, TAB_WIDTHS,
};
use crate::detect::LineEnding;
use crate::document::{Document, MAX_ROW_BYTES};
use crate::editor::{Editor, Mode};
use crate::find::Step;
use crate::layout::{Faces, Layout};

const CLICK: Duration64 = Duration64::from_millis(500);

struct Harness {
    view: View,
    layout: Layout,
    registry: ThemeRegistry,
    now: u64,
}

impl Harness {
    fn new(text: &[u8], format: Format) -> Self {
        let registry = ThemeRegistry::with_builtins();
        let document = Document::from_chunks(alloc::vec![text.to_vec()]).expect("loads");
        let view = View::new(
            Editor::new(document, format),
            String::from("notes.txt"),
            Access::Writable,
            CLICK,
        );
        let layout = view.layout(
            900,
            600,
            registry.active(),
            Scale::ONE,
            faces(registry.active()),
        );
        let mut harness = Self {
            view,
            layout,
            registry,
            now: 1_000_000_000,
        };
        harness.view.settle(&harness.layout, &mut Region::new());
        harness
    }

    fn theme(&self) -> &Theme {
        self.registry.active()
    }

    fn relayout(&mut self) {
        self.layout = self.view.layout(
            900,
            600,
            self.registry.active(),
            Scale::ONE,
            faces(self.registry.active()),
        );
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) -> (super::Outcome, Region) {
        let mut damage = Region::new();
        let outcome = self.view.on_key(
            key,
            modifiers,
            &self.layout,
            Scale::ONE,
            self.registry.active(),
            &mut damage,
        );
        if outcome.relayout {
            self.relayout();
        }
        (outcome, damage)
    }

    fn press(&mut self, key: Key) -> super::Outcome {
        self.key(key, Modifiers::default()).0
    }

    fn ctrl(&mut self, ch: char, shift: bool) -> super::Outcome {
        self.key(
            Key::Char(ch),
            Modifiers {
                ctrl: true,
                shift,
                ..Modifiers::default()
            },
        )
        .0
    }

    fn type_str(&mut self, text: &str) {
        for ch in text.chars() {
            self.press(Key::Char(ch));
        }
    }

    fn pointer(&mut self, event: InputEvent) -> super::Outcome {
        let theme = ThemeRegistry::with_builtins();
        let mut damage = Region::new();
        let outcome = self.view.on_pointer(
            &event,
            self.now,
            &self.layout,
            Scale::ONE,
            theme.active(),
            &mut damage,
        );
        if outcome.relayout {
            self.relayout();
        }
        outcome
    }

    /// Click the grid cell at `row` and `column`, at its left edge.
    fn click(&mut self, row: usize, column: usize) -> super::Outcome {
        self.click_in(row, column, 1)
    }

    /// Click the grid cell at `row` and `column`, `dx` pixels into it.
    fn click_in(&mut self, row: usize, column: usize, dx: u32) -> super::Outcome {
        let (cell_w, cell_h) = self.layout.cell();
        let grid = self.layout.grid();
        let across = |index: usize, cell: u32, into: u32| {
            i32::try_from(u32::try_from(index).expect("small") * cell + into).expect("on screen")
        };
        let at = Point::new(
            grid.left() + across(column, cell_w, dx),
            grid.top() + across(row, cell_h, 1),
        );
        self.pointer(InputEvent::PointerMoved { to: at });
        let outcome = self.pointer(InputEvent::PointerPressed {
            button: PointerButton::Primary,
        });
        self.pointer(InputEvent::PointerReleased {
            button: PointerButton::Primary,
        });
        self.now += 1_000;
        outcome
    }

    fn text(&self) -> Vec<u8> {
        self.view.editor().document().to_vec().expect("room")
    }
}

fn faces(theme: &Theme) -> Faces {
    Faces {
        grid: BitmapFont::for_role(theme.fonts(), TextRole::Monospace, Scale::ONE),
        status: BitmapFont::for_role(theme.fonts(), TextRole::Caption, Scale::ONE),
    }
}

/// Whether `damage` touches `rect`.
fn touches(damage: &Region, rect: Rect) -> bool {
    damage
        .rects()
        .iter()
        .any(|hit| !hit.intersection(&rect).is_empty())
}

#[test]
fn a_keystroke_repaints_its_row_and_the_status_band_not_the_window() {
    let mut harness = Harness::new(b"one\ntwo\nthree\n", Format::PlainText);
    harness.click(1, 3);
    let (_, damage) = harness.key(Key::Char('!'), Modifiers::default());
    assert_eq!(harness.text(), b"one\ntwo!\nthree\n");
    assert!(touches(&damage, harness.layout.row_rect(1)));
    assert!(touches(&damage, harness.layout.status()));
    assert!(
        !touches(&damage, harness.layout.row_rect(0)),
        "the row above is untouched"
    );
    assert!(
        !touches(&damage, harness.layout.row_rect(2)),
        "the row below is untouched"
    );
}

#[test]
fn a_new_line_repaints_every_row_below_it() {
    let mut harness = Harness::new(b"one\ntwo\nthree\n", Format::PlainText);
    harness.click(0, 3);
    let (_, damage) = harness.key(Key::Named(NamedKey::Enter), Modifiers::default());
    assert_eq!(harness.text(), b"one\n\ntwo\nthree\n");
    for row in 0..4 {
        assert!(touches(&damage, harness.layout.row_rect(row)), "row {row}");
    }
}

#[test]
fn the_gutter_widening_lays_the_window_out_again() {
    let text = alloc::vec![b'\n'; 998];
    let mut harness = Harness::new(&text, Format::PlainText);
    assert_eq!(harness.layout.gutter_digits(), 3);
    harness.ctrl('a', false);
    harness.press(Key::Named(NamedKey::End));
    let outcome = harness.press(Key::Named(NamedKey::Enter));
    assert!(outcome.relayout, "1000 lines need a fourth digit");
}

#[test]
fn clicks_place_the_caret_select_a_word_then_a_line() {
    let mut harness = Harness::new(b"let value = 1;\nnext\n", Format::PlainText);
    harness.click(0, 6);
    assert_eq!(harness.view.editor().selection().range(), 6..6);
    harness.now -= 900;
    harness.click(0, 6);
    assert_eq!(
        harness.view.editor().selection().range(),
        4..9,
        "a double click selects the word"
    );
    harness.now -= 900;
    harness.click(0, 6);
    assert_eq!(
        harness.view.editor().selection().range(),
        0..15,
        "a third selects the line and its break"
    );
}

#[test]
fn a_double_click_selects_the_word_under_the_pointer_whichever_half() {
    let mut harness = Harness::new(b"let value = 1;\nnext\n", Format::PlainText);
    let right = harness.layout.cell().0 - 1;
    harness.click_in(0, 8, right);
    harness.now -= 900;
    harness.click_in(0, 8, right);
    assert_eq!(
        harness.view.editor().selection().range(),
        4..9,
        "the right half of the word's last letter is still the word"
    );
    harness.click(1, 9);
    harness.now -= 900;
    harness.click(1, 9);
    assert_eq!(
        harness.view.editor().selection().range(),
        15..19,
        "past the row's end, its last word"
    );
}

#[test]
fn the_find_bar_opens_seeded_searches_and_closes() {
    let mut harness = Harness::new(b"alpha beta alpha", Format::PlainText);
    harness.click(0, 0);
    harness.view.editor_mut().click(5, true);
    let outcome = harness.ctrl('f', false);
    assert!(outcome.relayout);
    assert!(harness.view.find_open());
    assert_eq!(harness.view.find_controls().0.text(), "alpha");
    let outcome = harness.press(Key::Named(NamedKey::Enter));
    let Some(Request::Search {
        id,
        search,
        replacement: None,
    }) = outcome.request
    else {
        panic!("Enter asks for a search");
    };
    let mut step = search;
    let found = step.step(harness.view.editor().document(), usize::MAX);
    assert_eq!(found, Step::Found(11..16));
    let generation = harness.view.editor().generation();
    let mut damage = Region::new();
    harness
        .view
        .found(id, generation, found, None, &harness.layout, &mut damage);
    assert_eq!(harness.view.editor().selection().range(), 11..16);
    let outcome = harness.press(Key::Named(NamedKey::Escape));
    assert!(outcome.relayout);
    assert!(!harness.view.find_open());
}

#[test]
fn an_answer_for_an_older_document_is_not_believed() {
    let mut harness = Harness::new(b"find me", Format::PlainText);
    harness.ctrl('f', false);
    harness.type_str("me");
    let Some(Request::Search { id, .. }) = harness.press(Key::Named(NamedKey::Enter)).request
    else {
        panic!("a search");
    };
    let stale = harness.view.editor().generation() + 1;
    let mut damage = Region::new();
    harness.view.found(
        id,
        stale,
        Step::Found(5..7),
        None,
        &harness.layout,
        &mut damage,
    );
    assert_eq!(harness.view.editor().selection().range(), 0..0);
    assert!(harness
        .view
        .message()
        .is_some_and(|message| message.contains("changed")));
}

#[test]
fn replace_all_replaces_every_match_as_one_step_and_counts_them() {
    let mut harness = Harness::new(b"cat cat cat", Format::PlainText);
    harness.ctrl('h', false);
    harness.type_str("dog");
    harness.press(Key::Named(NamedKey::Tab));
    harness.type_str("cat");
    let all = harness.layout.find_buttons()[6];
    harness.pointer(InputEvent::PointerMoved { to: all.center() });
    harness.pointer(InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    let outcome = harness.pointer(InputEvent::PointerReleased {
        button: PointerButton::Primary,
    });
    let Some(Request::Search {
        id,
        replacement: Some(bytes),
        ..
    }) = outcome.request
    else {
        panic!("All asks for every match");
    };
    assert_eq!(
        bytes, b"dog",
        "the replace field holds what was typed into it"
    );
    let generation = harness.view.editor().generation();
    let step = Step::All {
        matches: alloc::vec![0..3, 4..7, 8..11],
        more: false,
    };
    let mut damage = Region::new();
    harness.view.found(
        id,
        generation,
        step,
        Some(&bytes),
        &harness.layout,
        &mut damage,
    );
    assert_eq!(harness.text(), b"dog dog dog");
    assert_eq!(harness.view.message(), Some("Replaced 3"));
    harness.ctrl('z', false);
    assert_eq!(
        harness.text(),
        b"dog dog dog",
        "the replace field has the keyboard"
    );
    harness.press(Key::Named(NamedKey::Escape));
    harness.ctrl('z', false);
    assert_eq!(
        harness.text(),
        b"cat cat cat",
        "one undo takes every replacement back"
    );
}

#[test]
fn closing_a_changed_document_asks_first() {
    let mut harness = Harness::new(b"text", Format::PlainText);
    assert!(
        matches!(harness.ctrl('w', false).request, Some(Request::Close)),
        "an unchanged window just closes"
    );
    harness.type_str("x");
    assert!(harness.ctrl('w', false).request.is_none());
    assert!(harness.view.modal().is_some());
    assert!(
        harness
            .press(Key::Named(NamedKey::Escape))
            .request
            .is_none(),
        "Escape cancels"
    );
    assert!(harness.view.modal().is_none());
    harness.ctrl('w', false);
    let theme = harness.theme().clone();
    let (dialog, _) = harness.view.modal().expect("asked again");
    let bounds = View::modal_rect(dialog, harness.layout.window(), false, Scale::ONE, &theme);
    let actions = dialog.action_rects(bounds, Scale::ONE, &theme);
    let dont_save = actions[1].center();
    harness.pointer(InputEvent::PointerMoved { to: dont_save });
    harness.pointer(InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    let outcome = harness.pointer(InputEvent::PointerReleased {
        button: PointerButton::Primary,
    });
    assert!(matches!(outcome.request, Some(Request::Close)));
}

#[test]
fn go_to_line_asks_for_a_number_and_goes_there() {
    let mut harness = Harness::new(b"a\nb\nc\nd\n", Format::PlainText);
    harness.ctrl('l', false);
    assert!(harness
        .view
        .modal()
        .is_some_and(|(_, field)| field.is_some()));
    harness.type_str("3");
    harness.press(Key::Named(NamedKey::Enter));
    assert!(harness.view.modal().is_none());
    assert_eq!(harness.view.editor().selection().head, 4);
}

#[test]
fn typing_a_line_number_repaints_the_question_not_the_window() {
    let mut harness = Harness::new(b"a\nb\nc\nd\n", Format::PlainText);
    harness.ctrl('l', false);
    let (_, damage) = harness.key(Key::Char('3'), Modifiers::default());
    let (dialog, _) = harness.view.modal().expect("the question stays");
    let question = View::modal_rect(
        dialog,
        harness.layout.window(),
        true,
        Scale::ONE,
        harness.registry.active(),
    );
    assert!(!damage.is_empty(), "the typed digit is shown");
    assert!(
        damage
            .rects()
            .iter()
            .all(|rect| rect.intersection(&question) == *rect),
        "{damage:?} reaches past {question:?}"
    );
}

#[test]
fn moving_the_caret_in_the_hex_view_repaints_the_rows_it_left_and_reached() {
    let mut harness = Harness::new(&[0u8; 4096], Format::PlainText);
    assert_eq!(harness.view.editor().mode(), Mode::Hex);
    let (_, damage) = harness.key(Key::Named(NamedKey::Down), Modifiers::default());
    let grid = harness.layout.grid();
    let rows = harness
        .layout
        .row_rect(0)
        .union(&harness.layout.row_rect(1));
    assert!(
        damage
            .rects()
            .iter()
            .any(|rect| !rect.intersection(&grid).is_empty()),
        "the caret's rows are repainted"
    );
    assert!(
        damage.rects().iter().all(|rect| {
            let in_grid = rect.intersection(&grid);
            in_grid.is_empty() || in_grid.intersection(&rows) == in_grid
        }),
        "{damage:?} reaches past the two rows {rows:?}"
    );
}

#[test]
fn copy_cut_and_paste_go_through_the_clipboard_requests() {
    let mut harness = Harness::new(b"hello world", Format::PlainText);
    harness.ctrl('a', false);
    let Some(Request::Copy(bytes)) = harness.ctrl('c', false).request else {
        panic!("copy asks to put the selection on the clipboard");
    };
    assert_eq!(bytes, b"hello world");
    assert!(matches!(
        harness.ctrl('v', false).request,
        Some(Request::Paste)
    ));
    let mut damage = Region::new();
    harness.view.paste(b"bye", &harness.layout, &mut damage);
    assert_eq!(harness.text(), b"bye");
    harness.ctrl('a', false);
    assert!(matches!(
        harness.ctrl('x', false).request,
        Some(Request::Copy(_))
    ));
    assert_eq!(harness.text(), b"");
}

#[test]
fn every_action_round_trips_through_its_menu_id() {
    let mut actions: Vec<Action> = PLAIN_ACTIONS.to_vec();
    actions.extend(MODES.map(Action::Mode));
    actions.extend(Format::ALL.map(Action::Format));
    actions.extend(TAB_WIDTHS.map(Action::TabWidth));
    actions.extend(INDENTS.map(Action::Indentation));
    actions.extend(ENDINGS.map(Action::LineEnding));
    let mut ids: Vec<u16> = actions.iter().map(|action| action.id()).collect();
    for (action, id) in actions.iter().zip(&ids) {
        assert_eq!(Action::from_id(*id), Some(*action), "{action:?}");
    }
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), actions.len(), "no two actions share an id");
    assert_eq!(Action::from_id(0), None);
    assert_eq!(Action::from_id(203), None, "a tab width no menu offers");
}

/// Every action a key or the old menu bar reached is a row of the one menu
/// a secondary press opens, exactly once: the clipboard on the plate it
/// opens with, and File, Edit, Find and View as submenus of it — so the model
/// held all of it and nothing was left out.
#[test]
fn the_window_menu_holds_every_menu_whole() {
    let harness = Harness::new(b"fn main() {}\n", Format::Rust);
    let menu = harness.view.menu(MenuKind::Window);
    assert_eq!(menu.title(), APP_TITLE);
    let root: Vec<String> = menu
        .rows()
        .filter(|(_, parent)| parent.is_none())
        .map(|(row, _)| match row {
            AppMenuRowView::Item(item) => String::from(item.label),
            AppMenuRowView::Submenu { label, .. } => alloc::format!("{label} >"),
            AppMenuRowView::Separator => String::from("-"),
            AppMenuRowView::Info => String::from("info"),
        })
        .collect();
    assert_eq!(
        root,
        [
            "Cut",
            "Copy",
            "Paste",
            "Select all",
            "-",
            "File >",
            "Edit >",
            "Find >",
            "View >"
        ]
    );

    let mut every: Vec<Action> = PLAIN_ACTIONS.to_vec();
    every.extend(MODES.map(Action::Mode));
    every.extend(Format::ALL.map(Action::Format));
    every.extend(TAB_WIDTHS.map(Action::TabWidth));
    every.extend(INDENTS.map(Action::Indentation));
    every.extend(ENDINGS.map(Action::LineEnding));
    let mut offered: Vec<u16> = menu
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some(item.id.get()),
            _ => None,
        })
        .collect();
    offered.sort_unstable();
    let mut wanted: Vec<u16> = every.iter().map(|action| action.id()).collect();
    wanted.sort_unstable();
    assert_eq!(offered, wanted, "every action once, and nothing else");
}

/// A secondary press anywhere in the window opens its menu at the press:
/// over the text, the gutter, a scrollbar, the find bar and the status band.
#[test]
fn a_secondary_press_anywhere_opens_the_window_menu_where_it_landed() {
    let mut harness = Harness::new(b"one\ntwo\n", Format::PlainText);
    harness.ctrl('f', false);
    harness.relayout();
    let layout = harness.layout.clone();
    for (what, band) in [
        ("grid", layout.grid()),
        ("gutter", layout.gutter()),
        ("scrollbar", layout.vertical_bar()),
        ("find bar", layout.find()),
        ("status band", layout.status()),
    ] {
        assert!(!band.is_empty(), "the {what} is laid out");
        let at = band.center();
        harness.pointer(InputEvent::PointerMoved { to: at });
        let outcome = harness.pointer(InputEvent::PointerPressed {
            button: PointerButton::Secondary,
        });
        assert!(
            matches!(
                outcome.request,
                Some(Request::Menu { kind: MenuKind::Window, anchor })
                    if anchor == Rect::new(at.x, at.y, 0, 0)
            ),
            "the {what}: {:?}",
            outcome.request
        );
        harness.pointer(InputEvent::PointerReleased {
            button: PointerButton::Secondary,
        });
    }
}

#[test]
fn every_menu_builds_and_marks_the_current_choices() {
    let harness = Harness::new(b"fn main() {}\n", Format::Rust);
    for kind in [
        MenuKind::Window,
        MenuKind::Format,
        MenuKind::Mode,
        MenuKind::LineEnding,
        MenuKind::Indent,
    ] {
        assert!(!harness.view.menu(kind).is_empty(), "{kind:?}");
    }
    let view_menu = harness.view.menu(MenuKind::Window);
    let marked: Vec<&str> = view_menu
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) if item.mark == AppMenuMark::Radio => Some(item.label),
            _ => None,
        })
        .collect();
    assert!(marked.contains(&"Text"));
    assert!(marked.contains(&"Rust"));
    assert!(marked.contains(&"LF"));
    let formats = harness.view.menu(MenuKind::Format);
    assert_eq!(formats.len(), Format::COUNT);
}

#[test]
fn a_focus_change_repaints_only_the_caret_row() {
    let mut harness = Harness::new(b"one\ntwo\nthree\n", Format::PlainText);
    harness.click(1, 1);
    let mut damage = Region::new();
    harness
        .view
        .focus_changed(false, &harness.layout, &mut damage);
    assert!(touches(&damage, harness.layout.row_rect(1)));
    assert!(!touches(&damage, harness.layout.row_rect(0)));
    assert!(!touches(&damage, harness.layout.row_rect(2)));
}

#[test]
fn a_focus_change_in_hex_repaints_only_the_caret_row() {
    let text: Vec<u8> = (0..64).collect();
    let mut harness = Harness::new(&text, Format::PlainText);
    let id = AppMenuItemId::new(Action::Mode(Mode::Hex).id()).expect("an id");
    harness.view.chosen(id, &harness.layout, &mut Region::new());
    harness.relayout();
    harness.press(Key::Named(NamedKey::Down));
    assert_eq!(harness.view.editor().selection().head, 16);
    let mut damage = Region::new();
    harness
        .view
        .focus_changed(true, &harness.layout, &mut damage);
    assert!(touches(&damage, harness.layout.row_rect(1)));
    assert!(!touches(&damage, harness.layout.row_rect(0)));
    assert!(!touches(&damage, harness.layout.row_rect(2)));
}

#[test]
fn a_caret_scrolled_out_of_view_repaints_nothing_on_a_focus_change() {
    let text: Vec<u8> = b"line\n".repeat(400);
    let mut harness = Harness::new(&text, Format::PlainText);
    let grid = harness.layout.grid();
    harness.pointer(InputEvent::PointerMoved { to: grid.center() });
    harness.pointer(InputEvent::PointerScrolled {
        dx: 0,
        dy: 100 * SCROLL_UNITS_PER_DETENT,
    });
    assert!(harness.view.scroll().0.line > harness.layout.rows());
    let mut damage = Region::new();
    harness
        .view
        .focus_changed(false, &harness.layout, &mut damage);
    assert!(damage.is_empty(), "the caret's row is not on screen");
}

#[test]
fn a_chosen_row_runs_its_action() {
    let mut harness = Harness::new(b"text", Format::PlainText);
    let id = AppMenuItemId::new(Action::Mode(Mode::Hex).id()).expect("an id");
    let mut damage = Region::new();
    let outcome = harness.view.chosen(id, &harness.layout, &mut damage);
    assert!(outcome.relayout);
    assert_eq!(harness.view.editor().mode(), Mode::Hex);
    assert!(
        harness.ctrl('h', true).relayout,
        "Ctrl+Shift+H toggles back"
    );
    assert_eq!(harness.view.editor().mode(), Mode::Text);
}

#[test]
fn the_status_fields_open_their_menus() {
    let mut harness = Harness::new(b"text", Format::PlainText);
    let field = harness.layout.status_fields()[4];
    harness.pointer(InputEvent::PointerMoved { to: field.center() });
    let outcome = harness.pointer(InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    assert!(matches!(
        outcome.request,
        Some(Request::Menu {
            kind: MenuKind::Format,
            ..
        })
    ));
    let overwrite = harness.layout.status_fields()[0];
    harness.pointer(InputEvent::PointerMoved {
        to: overwrite.center(),
    });
    harness.pointer(InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    assert!(harness.view.editor().overwrite());
}

#[test]
fn the_wheel_scrolls_and_the_next_key_brings_the_caret_back() {
    let mut text = Vec::new();
    for line in 0..500 {
        text.extend_from_slice(alloc::format!("line {line}\n").as_bytes());
    }
    let mut harness = Harness::new(&text, Format::PlainText);
    let grid = harness.layout.grid();
    harness.pointer(InputEvent::PointerMoved { to: grid.center() });
    harness.pointer(InputEvent::PointerScrolled { dx: 0, dy: 1200 });
    assert!(harness.view.scroll().0.line > 0, "the wheel moved the view");
    harness.press(Key::Named(NamedKey::Right));
    assert_eq!(
        harness.view.scroll().0.line,
        0,
        "the caret is brought back into view"
    );
}

/// `view`'s title, written over a buffer that already held another.
fn title_of(view: &View) -> String {
    let mut title = String::from("a stale title longer than the one written over it");
    view.write_title(&mut title);
    title
}

#[test]
fn the_title_says_what_the_document_is_and_whether_it_changed() {
    let mut harness = Harness::new(b"", Format::PlainText);
    assert_eq!(title_of(&harness.view), "notes.txt \u{2014} TextEdit");
    harness.type_str("a");
    assert_eq!(title_of(&harness.view), "*notes.txt \u{2014} TextEdit");
    let generation = harness.view.editor().generation();
    harness
        .view
        .saved(generation, Some(String::from("renamed.txt")));
    assert_eq!(title_of(&harness.view), "renamed.txt \u{2014} TextEdit");
}

#[test]
fn a_read_only_document_says_so_in_its_title() {
    let view = View::new(
        Editor::new(Document::new(), Format::PlainText),
        String::from("hosts"),
        Access::ReadOnly,
        CLICK,
    );
    assert_eq!(title_of(&view), "hosts (read-only) \u{2014} TextEdit");
}

#[test]
fn a_long_or_odd_name_still_makes_a_title_the_desktop_accepts() {
    use tairix_abi::window_ipc::WindowTitle;
    let long = "a-document-whose-name-goes-on-far-past-what-any-title-field-holds.txt";
    let names = [
        String::from(long),
        "\u{6587}".repeat(40),
        String::from("tab\there.conf"),
    ];
    for name in names {
        for access in [Access::ReadOnly, Access::Writable] {
            let mut view = View::new(
                Editor::new(Document::new(), Format::PlainText),
                name.clone(),
                access,
                CLICK,
            );
            let _ = view.editor_mut().replace_selection(b"changed");
            let title = title_of(&view);
            assert!(WindowTitle::new(&title).is_ok(), "{title:?} is refused");
            assert!(title.starts_with('*') && title.ends_with("\u{2014} TextEdit"));
            assert_eq!(
                title.contains(" (read-only)"),
                access == Access::ReadOnly,
                "{title:?}"
            );
        }
    }
}

/// What the sandbox would answer for `job`.
fn lexed_answer(job: &crate::highlight::LexJob) -> tairix_sandbox::textsyntax::LexedBatch {
    let mut batch = tairix_sandbox::textsyntax::LexedBatch::default();
    let mut state = job.state;
    for line in job.lines() {
        state = tairix_syntax::lex_line(job.format, state, line, &mut batch.spans);
        batch.lines.push((batch.spans.len(), state));
    }
    batch
}

/// A lexed batch repaints only the rows it coloured, and an answer for a
/// batch nobody is waiting on repaints nothing.
#[test]
fn a_lexed_batch_repaints_only_the_rows_it_coloured() {
    let mut harness = Harness::new(b"[a]\nkey = 1\nother = 2\n", Format::Toml);
    let job = harness
        .view
        .lex_job(&harness.layout)
        .expect("a batch to colour the window");
    let mut damage = Region::new();
    harness.view.lexed(
        job.id + 1,
        &lexed_answer(&job),
        &harness.layout,
        &mut damage,
    );
    assert!(damage.is_empty(), "a stale answer colours nothing");
    harness
        .view
        .lexed(job.id, &lexed_answer(&job), &harness.layout, &mut damage);
    assert!(!damage.is_empty());
    let grid = harness.layout.grid();
    assert!(
        damage.rects().iter().all(|rect| rect.height < grid.height),
        "row rectangles, not the whole grid"
    );
}

/// A conversion lands as one edit when the document is unchanged since its
/// snapshot, and is refused with its reason when it has moved on.
#[test]
fn a_conversion_is_adopted_only_for_the_document_it_was_taken_from() {
    let mut harness = Harness::new(b"one\ntwo\n", Format::PlainText);
    let (generation, _) = harness.view.editor_mut().snapshot().expect("room");
    let mut damage = Region::new();
    let chunks = Some(alloc::vec![b"one\r\ntwo\r\n".to_vec()]);
    let _ = harness.view.converted(
        generation,
        chunks,
        LineEnding::CrLf,
        &harness.layout,
        &mut damage,
    );
    assert_eq!(harness.view.editor().line_ending(), LineEnding::CrLf);
    assert!(harness.view.editor().is_modified());
    let stale = Some(alloc::vec![b"x".to_vec()]);
    let _ = harness.view.converted(
        generation,
        stale,
        LineEnding::Lf,
        &harness.layout,
        &mut damage,
    );
    assert_eq!(
        harness.view.editor().document().to_vec().expect("room"),
        b"one\r\ntwo\r\n"
    );
    assert!(harness.view.message().is_some(), "the refusal is said");
}

#[test]
fn a_detected_format_applies_until_the_user_chooses_one() {
    let mut harness = Harness::new(b"<!doctype html>\n<p>hi</p>\n", Format::PlainText);
    let mut damage = Region::new();
    let outcome = harness
        .view
        .detected(Format::Html, &harness.layout, &mut damage);
    assert!(outcome.relayout, "a new format redraws the window");
    assert_eq!(harness.view.editor().format(), Format::Html);

    harness.relayout();
    let chosen = harness
        .view
        .act(Action::Format(Format::Xml), &harness.layout, &mut damage);
    assert!(chosen.relayout);
    harness.relayout();
    let late = harness
        .view
        .detected(Format::Html, &harness.layout, &mut damage);
    assert!(
        !late.relayout && late.request.is_none(),
        "a late detection changes nothing"
    );
    assert_eq!(
        harness.view.editor().format(),
        Format::Xml,
        "the user's choice stands"
    );
}

/// A store as opened is checked at once; an edited one once typing pauses;
/// one already checked as it is, never.
#[test]
fn a_store_is_checked_once_its_edits_pause() {
    let mut harness = Harness::new(b"hostname = tairix\n", Format::SystemConfig);
    assert_eq!(
        harness.view.check_due(5),
        Some(0),
        "the document as opened is due at once"
    );

    harness.press(Key::Char('x'));
    let typed = 1_000_000_000;
    assert_eq!(harness.view.check_due(typed), Some(typed + CHECK_SETTLE_NS));
    assert_eq!(
        harness.view.check_due(typed + 1_000),
        Some(typed + CHECK_SETTLE_NS),
        "looking again without an edit keeps the deadline"
    );
    harness.press(Key::Char('y'));
    let later = typed + CHECK_SETTLE_NS / 2;
    assert_eq!(
        harness.view.check_due(later),
        Some(later + CHECK_SETTLE_NS),
        "another keystroke moves it"
    );

    let generation = harness.view.editor().generation();
    let mut damage = Region::new();
    harness
        .view
        .checked(generation, Vec::new(), &harness.layout, &mut damage);
    assert_eq!(harness.view.check_due(later), None, "checked as it is");

    let mut plain = Harness::new(b"notes\n", Format::PlainText);
    assert_eq!(plain.view.check_due(0), None, "only a store is checked");
}

#[test]
fn the_clipboard_keys_act_on_the_find_field_that_has_the_keyboard() {
    let mut harness = Harness::new(b"alpha beta alpha", Format::PlainText);
    harness.ctrl('a', false);
    harness.ctrl('f', false);
    harness.ctrl('a', false);
    harness.type_str("beta");
    assert_eq!(
        harness.view.find.text(),
        "beta",
        "select-all took the field, not the document"
    );
    let document = |harness: &Harness| harness.view.editor().document().to_vec().expect("room");
    harness.ctrl('a', false);
    let Some(Request::Copy(bytes)) = harness.ctrl('c', false).request else {
        panic!("the field's selection is copied");
    };
    assert_eq!(bytes, b"beta");
    let outcome = harness.ctrl('v', false);
    assert!(matches!(outcome.request, Some(Request::Paste)));
    let mut damage = Region::new();
    let _ = harness.view.paste(b"alpha", &harness.layout, &mut damage);
    assert_eq!(
        harness.view.find.text(),
        "alpha",
        "the paste went into the field"
    );
    assert_eq!(
        document(&harness),
        b"alpha beta alpha",
        "and nowhere near the document"
    );
    harness.ctrl('z', false);
    assert_eq!(
        document(&harness),
        b"alpha beta alpha",
        "undo belongs to the document's focus"
    );
    harness.ctrl('a', false);
    let Some(Request::Copy(cut)) = harness.ctrl('x', false).request else {
        panic!("a cut copies");
    };
    assert_eq!(
        (cut.as_slice(), harness.view.find.text()),
        (&b"alpha"[..], "")
    );
    assert_eq!(document(&harness), b"alpha beta alpha");
}

/// Whether the menu offers `label` enabled.
fn enabled(menu: &tairix_abi::window_ipc::AppMenu, label: &str) -> bool {
    menu.rows().any(
        |(row, _)| matches!(row, AppMenuRowView::Item(item) if item.label == label && item.enabled),
    )
}

/// The window menu's rows do what the keys they name do, on what the
/// secondary press that opened it landed on: the find field takes its
/// clipboard rows, and the text takes them back.
#[test]
fn the_window_menu_acts_on_what_the_secondary_press_landed_on() {
    let mut harness = Harness::new(b"alpha beta", Format::PlainText);
    harness.ctrl('f', false);
    harness.view.find.set_text("beta");
    harness.relayout();
    let layout = harness.layout.clone();
    let secondary = |harness: &mut Harness, at: Point| {
        harness.pointer(InputEvent::PointerMoved { to: at });
        let outcome = harness.pointer(InputEvent::PointerPressed {
            button: PointerButton::Secondary,
        });
        harness.pointer(InputEvent::PointerReleased {
            button: PointerButton::Secondary,
        });
        outcome
    };
    let choose = |harness: &mut Harness, action: Action| {
        let id = AppMenuItemId::new(action.id()).expect("an id");
        harness.view.chosen(id, &harness.layout, &mut Region::new())
    };

    secondary(&mut harness, layout.find_field().center());
    let menu = harness.view.menu(MenuKind::Window);
    assert!(!enabled(&menu, "Copy"), "the field has nothing selected");
    assert!(!enabled(&menu, "Undo"), "the field keeps no history");
    choose(&mut harness, Action::SelectAll);
    assert!(enabled(&harness.view.menu(MenuKind::Window), "Copy"));
    let Some(Request::Copy(bytes)) = choose(&mut harness, Action::Copy).request else {
        panic!("the field's selection is copied");
    };
    assert_eq!(bytes, b"beta");

    secondary(&mut harness, layout.grid().center());
    choose(&mut harness, Action::SelectAll);
    let Some(Request::Copy(bytes)) = choose(&mut harness, Action::Copy).request else {
        panic!("the document's selection is copied");
    };
    assert_eq!(bytes, b"alpha beta");
    assert_eq!(harness.view.find.text(), "beta");
}

#[test]
fn find_next_moves_on_past_a_match_inside_a_character() {
    let mut harness = Harness::new("\u{e9} \u{e9}\r\n".as_bytes(), Format::PlainText);
    harness.view.hex_pattern = true;
    harness.ctrl('f', false);
    harness.ctrl('a', false);
    let mut found = Vec::new();
    for (pattern, rounds) in [("A9", 3), ("0D", 2)] {
        let mut damage = Region::new();
        harness.view.find.set_text(pattern);
        harness.view.editor_mut().click(0, false);
        for _ in 0..rounds {
            let outcome = harness.press(Key::Named(NamedKey::Function { number: 3 }));
            let Some(Request::Search { id, mut search, .. }) = outcome.request else {
                panic!("find next asks for a search");
            };
            let generation = harness.view.editor().generation();
            let step = loop {
                match search.step(harness.view.editor().document(), usize::MAX) {
                    Step::Partial => {}
                    step => break step,
                }
            };
            harness
                .view
                .found(id, generation, step, None, &harness.layout, &mut damage);
            found.push(harness.view.editor().selection().range());
        }
    }
    assert_eq!(
        found,
        [0..2, 3..5, 0..2, 5..7, 5..7],
        "each find covers its character, and the next starts past it"
    );

    let mut harness = Harness::new("\u{1f600}\u{1f600}".as_bytes(), Format::PlainText);
    harness.view.hex_pattern = true;
    harness.ctrl('f', false);
    harness.view.find.set_text("98");
    let mut found = Vec::new();
    for _ in 0..3 {
        let mut damage = Region::new();
        let outcome = harness.press(Key::Named(NamedKey::Function { number: 3 }));
        let Some(Request::Search { id, mut search, .. }) = outcome.request else {
            panic!("find next asks for a search");
        };
        let generation = harness.view.editor().generation();
        let step = loop {
            match search.step(harness.view.editor().document(), usize::MAX) {
                Step::Partial => {}
                step => break step,
            }
        };
        harness
            .view
            .found(id, generation, step, None, &harness.layout, &mut damage);
        found.push(harness.view.editor().selection().range());
    }
    assert_eq!(
        found,
        [0..4, 4..8, 0..4],
        "a match inside one character never stalls"
    );
}

#[test]
fn replacing_one_match_lays_the_window_out_again_when_the_gutter_widens() {
    let mut text = b"x\n".repeat(998);
    text.extend_from_slice(b"line");
    let mut harness = Harness::new(&text, Format::PlainText);
    harness.ctrl('h', false);
    harness.view.find.set_text("line");
    harness.view.replace.set_text("a\nb");
    let at = harness.view.editor().document().len() - 4;
    harness.view.editor_mut().click(at, false);
    harness.view.editor_mut().click(at + 4, true);
    let (outcome, _) = harness.key(Key::Named(NamedKey::Enter), Modifiers::default());
    assert_eq!(harness.view.editor().document().line_count(), 1000);
    assert!(outcome.relayout, "a thousandth line widens the gutter");
}

#[test]
fn a_blank_document_is_measured_once_not_on_every_event() {
    let mut harness = Harness::new(b"\n\n\n", Format::PlainText);
    let mut damage = Region::new();
    harness.view.settle(&harness.layout, &mut damage);
    let grid = harness.layout.grid();
    let covers = |rect: &&Rect| {
        rect.left() <= grid.left()
            && rect.top() <= grid.top()
            && rect.right() >= grid.right()
            && rect.bottom() >= grid.bottom()
    };
    assert!(
        !damage.rects().iter().any(|rect| covers(&rect)),
        "a settled blank document owes no whole-grid repaint: {:?}",
        damage.rects()
    );
}

impl Harness {
    /// Turn the wheel `units` scroll units over the grid.
    fn wheel(&mut self, units: i32) {
        let centre = self.layout.grid().center();
        self.pointer(InputEvent::PointerMoved { to: centre });
        self.pointer(InputEvent::PointerScrolled { dx: 0, dy: units });
    }

    fn top_index(&self) -> usize {
        crate::text::row_index(self.view.editor().document(), self.view.scroll().0)
    }
}

#[test]
fn a_wheel_detent_scrolls_the_rows_its_pixels_span() {
    let mut harness = Harness::new(&b"line\n".repeat(500), Format::PlainText);
    let (_, cell_h) = harness.layout.cell();
    harness.wheel(SCROLL_UNITS_PER_DETENT);
    let rows = (WHEEL_STEP / cell_h) as usize;
    assert!(
        rows > 0 && rows < harness.layout.rows(),
        "a detent is a few rows"
    );
    assert_eq!(harness.top_index(), rows);
}

#[test]
fn a_slow_wheel_adds_up_to_a_row() {
    let mut harness = Harness::new(&b"line\n".repeat(500), Format::PlainText);
    let (_, cell_h) = harness.layout.cell();
    // A tenth of a detent at a time: well short of a row each.
    let step = SCROLL_UNITS_PER_DETENT / 10;
    let signed = |n: u32| i32::try_from(n).expect("small");
    let per_row = signed(cell_h) * SCROLL_UNITS_PER_DETENT / signed(WHEEL_STEP);
    let mut turned = 0;
    while harness.top_index() == 0 {
        assert!(turned <= per_row + step, "{turned} units never moved a row");
        harness.wheel(step);
        turned += step;
    }
    assert_eq!(harness.top_index(), 1, "a row's worth moves one row");
}

#[test]
fn the_wheel_reaches_every_row_of_a_long_line_and_back() {
    let mut text = alloc::vec![b'x'; 40 * MAX_ROW_BYTES];
    text.extend_from_slice(b"\nend");
    let mut harness = Harness::new(&text, Format::PlainText);
    let rows = harness.layout.rows();
    let (_, cell_h) = harness.layout.cell();
    let per_detent = (WHEEL_STEP / cell_h) as usize;
    assert!(harness.view.scrollbars().0.model().range().is_scrollable());
    harness.wheel(SCROLL_UNITS_PER_DETENT);
    assert_eq!(
        harness.view.scroll().0,
        crate::text::Row {
            line: 0,
            part: per_detent
        },
        "down into the line's own rows"
    );
    harness.wheel(100 * SCROLL_UNITS_PER_DETENT);
    assert_eq!(
        harness.top_index() + rows,
        41,
        "forty rows, a line, all shown"
    );
    let bottom = harness.top_index();
    harness.wheel(-SCROLL_UNITS_PER_DETENT);
    let cell = cell_h as usize;
    assert_eq!(
        harness.top_index(),
        (bottom * cell - WHEEL_STEP as usize) / cell,
        "up by the rows the pixels span"
    );
    assert_eq!(
        harness.view.scroll().0.line,
        0,
        "still within the long line"
    );
}

#[test]
fn a_one_line_document_longer_than_the_window_scrolls() {
    let text = alloc::vec![b'y'; 128 * MAX_ROW_BYTES];
    let mut harness = Harness::new(&text, Format::PlainText);
    let rows = harness.layout.rows();
    let model = harness.view.scrollbars().0.model();
    let (_, cell_h) = harness.layout.cell();
    assert_eq!(
        model.range().max_offset(),
        (128 - rows) as u64 * u64::from(cell_h)
    );
    harness.wheel(1000 * SCROLL_UNITS_PER_DETENT);
    assert_eq!(
        harness.view.scroll().0,
        crate::text::Row {
            line: 0,
            part: 128 - rows
        }
    );
}
