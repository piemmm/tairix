//! Paint's settings window: each category's rows, what each edit asks of
//! Paint, what a format or a chosen colour reshapes, and what the store says.

use alloc::string::String;

use tairix_colour::Rgb;
use tairix_controls::{FieldControl, FieldRow};
use tairix_geometry::{Region, Scale};
use tairix_image::IndexDepth;
use tairix_input::{InputEvent, Key, Modifiers, NamedKey};
use tairix_theme::ThemeRegistry;
use tairix_window::docapp::settings::{Edit, SettingsRequest, Then};
use tairix_window::docapp::{AppRequest, AppView, Outcome};

use super::{described, Ask, Category, PaintSettings};
use crate::layout::Faces;
use crate::pane::{Arrangement, PaneKind};
use crate::preferences::{GridStyle, Preferences, Shades};
use crate::save::SaveFormat;
use crate::tool::Tool;

struct Fixture {
    registry: ThemeRegistry,
    window: PaintSettings,
}

type Answer = Outcome<AppRequest<SettingsRequest<Preferences>>>;

impl Fixture {
    fn at(category: Category) -> Self {
        let registry = ThemeRegistry::with_builtins();
        let window = PaintSettings::new(Preferences::default(), category.index());
        let mut fixture = Self { registry, window };
        fixture.lay_out();
        fixture
    }

    /// Lay the window out again, as the host does when it is asked.
    fn lay_out(&mut self) {
        let theme = self.registry.active();
        let faces = Faces::of(theme, Scale::ONE);
        self.window
            .lay_out(600, 440, theme, Scale::ONE, faces, &mut Region::new());
    }

    fn key(&mut self, key: Key) -> Answer {
        let event = InputEvent::KeyPressed {
            key,
            modifiers: Modifiers::default(),
        };
        let theme = self.registry.active();
        let answer = self
            .window
            .input(&event, 0, Scale::ONE, theme, &mut Region::new());
        if answer.relayout != tairix_window::docapp::Relayout::None {
            self.lay_out();
        }
        answer
    }

    /// Give the column the keyboard and walk its cursor to item `item`,
    /// counted down every group's rows.
    fn reach_row(&mut self, item: usize) {
        for _ in 0..2 {
            let _ = self.key(Key::Named(NamedKey::Tab));
        }
        for _ in 0..item {
            let _ = self.key(Key::Named(NamedKey::Down));
        }
    }

    /// Choose entry `index` of the choice row the cursor is on.
    fn choose(&mut self, index: usize) -> Answer {
        let _ = self.key(Key::Named(NamedKey::Enter));
        let selected = self.cursor_row().and_then(|row| match row.control() {
            FieldControl::Combo(combo) => combo.selected(),
            _ => None,
        });
        let from = selected.unwrap_or(0);
        let (step, count) = if index >= from {
            (NamedKey::Down, index - from)
        } else {
            (NamedKey::Up, from - index)
        };
        for _ in 0..count {
            let _ = self.key(Key::Named(step));
        }
        self.key(Key::Named(NamedKey::Enter))
    }

    /// Type `text` over what the row at the cursor holds.
    fn retype(&mut self, text: &str) -> Answer {
        let _ = self.key(Key::Named(NamedKey::End));
        for _ in 0..8 {
            let _ = self.key(Key::Named(NamedKey::Backspace));
        }
        let mut last = Outcome::none();
        for character in text.chars() {
            last = self.key(Key::Char(character));
        }
        last
    }

    fn cursor_row(&self) -> Option<&FieldRow> {
        let column = self.window.frame().column();
        let (group, row) = column.cursor()?;
        column.groups().get(group)?.rows().get(row)
    }

    fn row(&self, group: usize, row: usize) -> &FieldRow {
        &self.window.frame().column().groups()[group].rows()[row]
    }
}

fn request(answer: Answer) -> SettingsRequest<Preferences> {
    match answer.request {
        Some(AppRequest::Own(request)) => request,
        other => panic!("not a settings request: {other:?}"),
    }
}

fn edited(answer: Answer) -> Edit<Preferences> {
    request(answer).edit.expect("an edit")
}

#[test]
fn every_category_is_listed_and_shows_its_own_groups() {
    let mut fixture = Fixture::at(Category::General);
    assert_eq!(fixture.window.title(), "Paint settings");
    let captions = [
        "NEW WINDOWS",
        "NEW PICTURE",
        "GRID",
        "CHECKERBOARD",
        "NEW WINDOWS",
    ];
    let _ = fixture.key(Key::Named(NamedKey::Tab));
    for (index, caption) in captions.into_iter().enumerate() {
        if index > 0 {
            let _ = fixture.key(Key::Named(NamedKey::Down));
            let _ = fixture.key(Key::Named(NamedKey::Enter));
        }
        assert_eq!(fixture.window.category(), index);
        let groups = fixture.window.frame().column().groups();
        assert_eq!(groups[0].caption(), caption, "{:?}", Category::at(index));
    }
}

#[test]
fn a_choice_asks_for_one_settled_edit() {
    let mut fixture = Fixture::at(Category::General);
    let index = Tool::ALL
        .iter()
        .position(|&tool| tool == Tool::Pencil)
        .expect("a tool");
    fixture.reach_row(0);
    let edit = edited(fixture.choose(index));
    assert_eq!(
        (edit.was.tool, edit.now.tool, edit.settled),
        (Tool::Brush, Tool::Pencil, true)
    );
    assert_eq!(fixture.window.record().tool, Tool::Pencil);
}

#[test]
fn a_format_that_holds_fewer_colours_takes_them_and_offers_only_those() {
    let mut fixture = Fixture::at(Category::NewPicture);
    let gif = SaveFormat::ALL
        .iter()
        .position(|&format| format == SaveFormat::Gif)
        .expect("a format");
    fixture.reach_row(2);
    let edit = edited(fixture.choose(gif));
    assert_eq!(
        (edit.now.format, edit.now.new.depth),
        (SaveFormat::Gif, Some(IndexDepth::Eight))
    );
    let FieldControl::Combo(colours) = fixture.row(0, 3).control() else {
        panic!("the colours choice");
    };
    assert_eq!(
        colours.choices().len(),
        4,
        "a GIF holds no millions of colours"
    );
}

#[test]
fn a_typed_number_is_a_live_edit_settled_by_enter() {
    let mut fixture = Fixture::at(Category::Grid);
    fixture.reach_row(0);
    let typed = edited(fixture.retype("8"));
    assert_eq!(
        (typed.now.grid.spacing.0, typed.settled),
        (8, false),
        "shown as typed, not written"
    );
    let settled = edited(fixture.key(Key::Named(NamedKey::Enter)));
    assert_eq!((settled.now.grid.spacing.0, settled.settled), (8, true));
}

#[test]
fn a_typed_size_is_settled_by_enter_in_the_field_it_was_typed_in() {
    let mut fixture = Fixture::at(Category::NewPicture);
    fixture.reach_row(0);
    let typed = edited(fixture.retype("80"));
    assert_eq!((typed.now.new.size.0, typed.settled), (80, false));
    let settled = edited(fixture.key(Key::Named(NamedKey::Enter)));
    assert_eq!((settled.now.new.size.0, settled.settled), (80, true));
}

#[test]
fn a_grid_colour_is_typed_and_settled_as_the_keyboard_leaves() {
    let mut fixture = Fixture::at(Category::Grid);
    fixture.reach_row(4);
    let typed = edited(fixture.retype("#102030"));
    assert_eq!(typed.now.grid.colour, Rgb::new(0x10, 0x20, 0x30));
    let left = edited(fixture.key(Key::Named(NamedKey::Down)));
    assert!(left.settled, "leaving the row settles it");
}

#[test]
fn choosing_shades_of_ones_own_enables_their_colours() {
    let mut fixture = Fixture::at(Category::Canvas);
    assert!(
        !fixture.row(0, 2).state().is_actionable(),
        "the theme's shades hold no colour of their own"
    );
    fixture.reach_row(1);
    let edit = edited(fixture.choose(1));
    assert!(matches!(edit.now.shades, Shades::Chosen(..)));
    assert!(fixture.row(0, 2).state().is_actionable());
    assert!(fixture.row(0, 3).state().is_actionable());
}

#[test]
fn take_reset_and_restore_are_asked_of_paint() {
    let mut fixture = Fixture::at(Category::Panes);
    fixture.reach_row(0);
    let taken = request(fixture.key(Key::Char(' ')));
    assert_eq!(taken.then, Some(Then::Ask(Ask::TakePanes)));

    let mut moved = Preferences::default();
    moved.panes.hide(PaneKind::Colour);
    let _ = fixture.window.adopt(&moved, &mut Region::new());
    fixture.lay_out();
    let _ = fixture.key(Key::Named(NamedKey::Down));
    let reset = edited(fixture.key(Key::Char(' ')));
    assert_eq!(reset.now.panes, Arrangement::default(), "reset");

    let _ = fixture.key(Key::Named(NamedKey::Tab));
    let restored = request(fixture.key(Key::Char(' ')));
    assert_eq!(restored.then, Some(Then::Restore));
}

#[test]
fn what_the_store_says_is_shown_and_a_refusal_said() {
    let mut fixture = Fixture::at(Category::Grid);
    let mut stored = Preferences::default();
    stored.grid.style = GridStyle::Dots;
    assert!(fixture.window.adopt(&stored, &mut Region::new()));
    assert_eq!(fixture.window.record().grid.style, GridStyle::Dots);
    let FieldControl::Combo(style) = fixture.row(0, 6).control() else {
        panic!("the style choice");
    };
    assert_eq!(style.selected(), Some(2));
    fixture.window.say(
        Some(String::from("the settings were not saved")),
        &mut Region::new(),
    );
    assert_eq!(
        fixture.window.frame().note(),
        Some("the settings were not saved")
    );
    fixture.window.say(None, &mut Region::new());
    assert_eq!(fixture.window.frame().note(), None);
}

#[test]
fn the_panes_are_described_where_each_stands() {
    let mut panes = Arrangement::default();
    assert_eq!(
        described(&panes),
        "Tools on the left; Colour on the right; Palette beneath the picture; \
         Layers, Adjustments, Effects hidden"
    );
    panes.float(PaneKind::Palette);
    assert_eq!(
        described(&panes),
        "Tools on the left; Colour on the right; Palette floating; \
         Layers, Adjustments, Effects hidden"
    );
}
