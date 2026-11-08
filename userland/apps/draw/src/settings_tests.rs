//! Draw's settings window: each category's rows, the edit each asks for,
//! what a unit or a paint of none reshapes, and Restore.

use tairix_colour::Rgba;
use tairix_controls::{FieldControl, FieldRow, SettingsLayout};
use tairix_drawing::Unit;
use tairix_geometry::{Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey};
use tairix_theme::ThemeRegistry;
use tairix_window::docapp::settings::{Edit, SettingsRequest, Then};
use tairix_window::docapp::{AppRequest, AppView, Outcome, Relayout};

use super::{Category, DrawSettings};
use crate::layout::Faces;
use crate::preferences::Preferences;
use crate::tools::Tool;

struct Fixture {
    registry: ThemeRegistry,
    window: DrawSettings,
    layout: SettingsLayout,
}

type Answer = Outcome<AppRequest<SettingsRequest<Preferences>>>;

impl Fixture {
    fn at(category: Category) -> Self {
        Self::with(Preferences::default(), category)
    }

    fn with(record: Preferences, category: Category) -> Self {
        let mut fixture = Self {
            registry: ThemeRegistry::with_builtins(),
            window: DrawSettings::new(record, category.index()),
            layout: SettingsLayout::default(),
        };
        fixture.lay_out();
        fixture
    }

    fn lay_out(&mut self) {
        let theme = self.registry.active();
        self.layout = self
            .window
            .layout(600, 440, theme, Scale::ONE, Faces::of(theme, Scale::ONE));
        self.window.settle(&self.layout, &mut Region::new());
    }

    fn key(&mut self, key: Key) -> Answer {
        let event = InputEvent::KeyPressed {
            key,
            modifiers: Modifiers::default(),
        };
        let theme = self.registry.active();
        let answer = self.window.input(
            &event,
            0,
            &self.layout,
            Scale::ONE,
            theme,
            &mut Region::new(),
        );
        if answer.relayout != Relayout::None {
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

    /// Choose entry `index` of the choice row the cursor is on, from its
    /// first.
    fn choose(&mut self, index: usize) -> Answer {
        let _ = self.key(Key::Named(NamedKey::Enter));
        let _ = self.key(Key::Named(NamedKey::Home));
        for _ in 0..index {
            let _ = self.key(Key::Named(NamedKey::Down));
        }
        self.key(Key::Named(NamedKey::Enter))
    }

    /// Type `text` over the whole of what the row at the cursor holds.
    fn retype(&mut self, text: &str) -> Answer {
        let _ = self.key(Key::Named(NamedKey::End));
        for _ in 0..16 {
            let _ = self.key(Key::Named(NamedKey::Backspace));
        }
        let mut last = Outcome::none();
        for character in text.chars() {
            last = self.key(Key::Char(character));
        }
        last
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
fn the_categories_show_their_own_groups() {
    let mut fixture = Fixture::at(Category::General);
    assert_eq!(fixture.window.title(), "Draw settings");
    let captions = ["NEW WINDOWS", "PAGE", "FILL", "MOVING"];
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
fn the_starting_tool_is_one_settled_edit() {
    let mut fixture = Fixture::at(Category::General);
    let pen = Tool::ALL
        .iter()
        .position(|&tool| tool == Tool::Pen)
        .expect("a tool");
    fixture.reach_row(0);
    let edit = edited(fixture.choose(pen));
    assert_eq!(
        (edit.was.tool, edit.now.tool, edit.settled),
        (Tool::Select, Tool::Pen, true)
    );
}

#[test]
fn a_new_drawings_grid_is_typed_in_its_unit_and_kept_in_pixels() {
    let mut fixture = Fixture::at(Category::NewDrawing);
    let FieldControl::Number(spacing) = fixture.row(1, 0).control() else {
        panic!("the spacing row");
    };
    assert_eq!(
        spacing.value(),
        1000,
        "a centimetre, in hundredths of a millimetre"
    );
    // Inches: the grid's centimetre is shown in them, not changed.
    fixture.reach_row(2);
    let inches = Unit::ALL
        .iter()
        .position(|&unit| unit == Unit::In)
        .expect("a unit");
    let edit = edited(fixture.choose(inches));
    assert_eq!(edit.now.unit, Unit::In);
    assert_eq!(edit.now.grid_spacing, edit.was.grid_spacing);
    let FieldControl::Number(spacing) = fixture.row(1, 0).control() else {
        panic!("the spacing row");
    };
    assert_eq!(spacing.value(), 39, "a centimetre is 0.39 inches");
    let _ = fixture.key(Key::Named(NamedKey::Down));
    let typed = edited(fixture.retype("0.5"));
    assert_eq!(typed.now.grid_spacing, 4800, "half an inch is 48 pixels");
}

#[test]
fn a_paint_chosen_where_there_was_none_starts_from_draws_own() {
    let record = Preferences {
        fill: None,
        ..Preferences::default()
    };
    let mut fixture = Fixture::with(record, Category::Style);
    assert!(
        !fixture.row(0, 1).state().is_actionable(),
        "no fill holds no colour"
    );
    fixture.reach_row(0);
    let edit = edited(fixture.choose(1));
    assert_eq!(edit.now.fill, Preferences::default().fill);
    assert!(fixture.row(0, 1).state().is_actionable());
    let _ = fixture.key(Key::Named(NamedKey::Down));
    let typed = edited(fixture.retype("#10203040"));
    assert_eq!(typed.now.fill, Some(Rgba::new(0x10, 0x20, 0x30, 0x40)));
}

#[test]
fn a_nudge_and_a_pick_are_typed_and_held_to_their_bounds() {
    let mut fixture = Fixture::at(Category::Editing);
    fixture.reach_row(0);
    let typed = edited(fixture.retype("2.5"));
    assert_eq!(typed.now.nudge, 250);
    let left = edited(fixture.key(Key::Named(NamedKey::Down)));
    assert!(left.settled);
    let _ = fixture.key(Key::Named(NamedKey::Down));
    let _ = fixture.retype("99");
    let held = edited(fixture.key(Key::Named(NamedKey::Enter)));
    assert_eq!(held.now.pick, crate::preferences::PICK_REACH.1);
}

#[test]
fn restore_is_asked_for() {
    let mut fixture = Fixture::at(Category::General);
    for _ in 0..3 {
        let _ = fixture.key(Key::Named(NamedKey::Tab));
    }
    let restored = request(fixture.key(Key::Char(' ')));
    assert_eq!(restored.then, Some(Then::Restore));
}
