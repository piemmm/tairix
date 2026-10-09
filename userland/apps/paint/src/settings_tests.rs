use tairix_geometry::{Point, Region, Scale};
use tairix_image::IndexDepth;
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_theme::{Theme, ThemeRegistry};
use tairix_window::docapp::{AppRequest, AppView, Outcome};

use super::{Category, Keyboard, SettingsLayout, SettingsRequest, SettingsWindow};
use crate::layout::Faces;
use crate::pane::{Arrangement, PaneKind};
use crate::panel::{Part, Placed};
use crate::preferences::{GridStyle, Preferences};
use crate::save::SaveFormat;
use crate::tool::Tool;

struct Fixture {
    registry: ThemeRegistry,
    window: SettingsWindow,
    layout: SettingsLayout,
}

type Answer = Outcome<AppRequest<SettingsRequest>>;

impl Fixture {
    fn at(category: Category) -> Self {
        let registry = ThemeRegistry::with_builtins();
        let window = SettingsWindow::new(Preferences::default(), category);
        let layout = window.layout(
            600,
            440,
            registry.active(),
            Scale::ONE,
            Faces::of(registry.active(), Scale::ONE),
        );
        Self {
            registry,
            window,
            layout,
        }
    }

    fn theme(&self) -> &Theme {
        self.registry.active()
    }

    fn feed(&mut self, input: InputEvent) -> Answer {
        let theme = self.registry.active();
        self.window.input(
            &input,
            0,
            &self.layout,
            Scale::ONE,
            theme,
            &mut Region::new(),
        )
    }

    fn click(&mut self, at: Point) -> Answer {
        let _ = self.feed(InputEvent::PointerMoved { to: at });
        let pressed = self.feed(InputEvent::PointerPressed {
            button: PointerButton::Primary,
        });
        let released = self.feed(InputEvent::PointerReleased {
            button: PointerButton::Primary,
        });
        if released.request.is_some() {
            released
        } else {
            pressed
        }
    }

    fn key(&mut self, key: Key, shift: bool) -> Answer {
        let modifiers = Modifiers {
            shift,
            ..Modifiers::default()
        };
        self.feed(InputEvent::KeyPressed { key, modifiers })
    }

    /// Where part `part` of the category's panel is laid out.
    fn placed(&self, part: usize) -> Placed {
        let faces = Faces::of(self.theme(), Scale::ONE);
        self.window
            .panel
            .place_of(part, self.layout.panel, faces, Scale::ONE, self.theme())
            .expect("laid out")
    }

    /// Choose entry `index` of choice part `part`'s list.
    fn choose(&mut self, part: usize, index: usize) -> Answer {
        let control = self.placed(part).controls[0];
        let _ = self.click(control.center());
        let faces = Faces::of(self.theme(), Scale::ONE);
        let popup = self.window.panel.popup_rect(
            self.layout.panel,
            self.layout.window,
            faces,
            Scale::ONE,
            self.theme(),
        );
        let Some(Part::Choice { combo, .. }) = self.window.panel.parts().get(part) else {
            panic!("part {part} is a choice");
        };
        // The list's rows share its height evenly.
        let count = i32::try_from(combo.choices().len()).expect("a short list");
        let along = i32::try_from(index).expect("a short list") * 2 + 1;
        let height = i32::try_from(popup.height).expect("on screen");
        self.click(Point::new(
            popup.center().x,
            popup.top() + along * height / (count * 2),
        ))
    }

    fn sidebar_row(&self, category: Category) -> Point {
        let index = Category::ALL
            .iter()
            .position(|&at| at == category)
            .expect("listed");
        self.window
            .sidebar
            .tab_area(index, self.layout.sidebar, Scale::ONE, self.theme())
            .expect("a row")
            .center()
    }
}

fn edited(answer: Answer) -> (Preferences, Preferences, bool) {
    match answer.request {
        Some(AppRequest::Own(SettingsRequest::Edit { was, now, settled })) => (was, now, settled),
        other => panic!("not an edit: {other:?}"),
    }
}

#[test]
fn the_sidebar_lists_every_category_and_shows_the_one_chosen() {
    let mut fixture = Fixture::at(Category::General);
    assert_eq!(fixture.window.category(), Category::General);
    for category in Category::ALL {
        let row = fixture.sidebar_row(category);
        let _ = fixture.click(row);
        assert_eq!(fixture.window.category(), category);
    }
    assert!(
        fixture.window.panel.parts().len() >= 2,
        "the panes' note and buttons"
    );
}

#[test]
fn a_choice_asks_for_one_settled_edit() {
    let mut fixture = Fixture::at(Category::General);
    let index = Tool::ALL
        .iter()
        .position(|&tool| tool == Tool::Pencil)
        .expect("a tool");
    let (was, now, settled) = edited(fixture.choose(0, index));
    assert_eq!(
        (was.tool, now.tool, settled),
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
    let (_, now, _) = edited(fixture.choose(1, gif));
    assert_eq!(
        (now.format, now.new.depth),
        (SaveFormat::Gif, Some(IndexDepth::Eight))
    );
    let Some(Part::Choice { combo, .. }) = fixture.window.panel.parts().get(2) else {
        panic!("the colours choice");
    };
    assert_eq!(
        combo.choices().len(),
        4,
        "a GIF holds no millions of colours"
    );
}

#[test]
fn a_typed_number_is_an_edit_settled_by_enter() {
    let mut fixture = Fixture::at(Category::Grid);
    let across = fixture.placed(0).controls[0];
    let _ = fixture.click(across.center());
    for _ in 0..4 {
        let _ = fixture.key(Key::Named(NamedKey::Backspace), false);
    }
    let (_, typed, settled) = edited(fixture.key(Key::Char('8'), false));
    assert_eq!(
        (typed.grid.spacing.0, settled),
        (8, false),
        "shown as typed, not written"
    );
    let (_, now, settled) = edited(fixture.key(Key::Named(NamedKey::Enter), false));
    assert_eq!((now.grid.spacing.0, settled), (8, true), "Enter settles it");
}

#[test]
fn restore_take_and_reset_are_asked_of_the_application() {
    let mut fixture = Fixture::at(Category::Panes);
    let buttons = fixture.placed(1);
    let taken = fixture.click(buttons.controls[0].center());
    assert_eq!(
        taken.request,
        Some(AppRequest::Own(SettingsRequest::TakePanes))
    );

    let mut moved = Preferences::default();
    moved.panes.hide(PaneKind::Colour);
    fixture
        .window
        .adopt(&moved, &fixture.layout, &mut Region::new());
    let (_, now, _) = edited(fixture.click(buttons.controls[1].center()));
    assert_eq!(now.panes, Arrangement::default(), "reset");

    let faces = Faces::of(fixture.theme(), Scale::ONE);
    let restore = fixture
        .window
        .footer
        .place_of(0, fixture.layout.footer, faces, Scale::ONE, fixture.theme())
        .expect("laid out")
        .controls[0];
    let restored = fixture.click(restore.center());
    assert_eq!(
        restored.request,
        Some(AppRequest::Own(SettingsRequest::Restore))
    );
}

#[test]
fn what_the_store_says_is_shown_and_a_refusal_said() {
    let mut fixture = Fixture::at(Category::Grid);
    let mut stored = Preferences::default();
    stored.grid.style = GridStyle::Dots;
    let layout = fixture.layout;
    fixture.window.adopt(&stored, &layout, &mut Region::new());
    assert_eq!(fixture.window.record().grid.style, GridStyle::Dots);
    let Some(Part::Choice { combo, .. }) = fixture.window.panel.parts().get(4) else {
        panic!("the style choice");
    };
    assert_eq!(combo.selected(), Some(2));

    fixture.window.say(
        Some(alloc::string::String::from("the settings were not saved")),
        &layout,
        &mut Region::new(),
    );
    assert_eq!(fixture.window.footer.parts().len(), 2);
    fixture.window.say(None, &layout, &mut Region::new());
    assert_eq!(fixture.window.footer.parts().len(), 1);
}

#[test]
fn tab_walks_the_regions_and_escape_closes() {
    let mut fixture = Fixture::at(Category::General);
    let _ = fixture.key(Key::Named(NamedKey::Tab), false);
    assert_eq!(fixture.window.keyboard, Some(Keyboard::Sidebar));
    let _ = fixture.key(Key::Named(NamedKey::Tab), false);
    assert_eq!(fixture.window.keyboard, Some(Keyboard::Panel));
    let _ = fixture.key(Key::Named(NamedKey::Tab), true);
    assert_eq!(fixture.window.keyboard, Some(Keyboard::Sidebar), "back");
    let _ = fixture.key(Key::Named(NamedKey::Down), false);
    let _ = fixture.key(Key::Named(NamedKey::Enter), false);
    assert_eq!(
        fixture.window.category(),
        Category::NewPicture,
        "the arrows choose a category"
    );
    let closed = fixture.key(Key::Named(NamedKey::Escape), false);
    assert_eq!(closed.request, Some(AppRequest::Close));
}
