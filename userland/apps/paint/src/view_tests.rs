use alloc::string::String;
use alloc::vec;
use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;

use tairix_abi::time::Duration64;
use tairix_abi::window_ipc::{AppMenuItemId, AppMenuRowView};
use tairix_controls::{FieldLayout, Keystroke};
use tairix_font::BitmapFont;
use tairix_geometry::{Point, Region, Scale};
use tairix_image::{IndexDepth, SpriteMode, SpriteName, SpritePalette, Unkept};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_theme::{TextRole, Theme, ThemeRegistry};
use tairix_window::docapp::{DocumentView, Relayout};
use tairix_window::document::{Access, SavedDocument};

use super::input::shortcut;
use super::{
    compute, Action, Computed, MenuKind, Outcome, Own, Request, View, GO_TO_ENTRY, RENAME_ENTRY,
};
use crate::canvas::{Canvas, Kind, OutOfMemory, Sample};
use crate::colour::Ink;
use crate::document::{Document, Entry, NewPicture, Origin, Picture, SpriteInfo};
use crate::layout::{Faces, Layout};
use crate::save::SaveRefusal;
use crate::tool::Tool;

const WINDOW: (u32, u32) = (900, 640);

fn faces(theme: &Theme) -> Faces {
    Faces {
        status: BitmapFont::for_role(theme.fonts(), TextRole::Caption, Scale::ONE),
    }
}

/// A window on `document`, laid out, with the theme it is drawn in.
struct Window {
    view: View,
    layout: Layout,
    registry: ThemeRegistry,
    now: u64,
}

impl Window {
    fn new(document: Document) -> Self {
        let registry = ThemeRegistry::with_builtins();
        let view = View::new(
            document,
            String::from("picture.png"),
            Access::Writable,
            Duration64::from_millis(500),
        );
        let layout = view.layout(
            WINDOW.0,
            WINDOW.1,
            registry.active(),
            Scale::ONE,
            faces(registry.active()),
        );
        let mut window = Self {
            view,
            layout,
            registry,
            now: 1_000_000_000,
        };
        window.view.settle(&window.layout, &mut Region::new());
        window
    }

    fn white(width: u32, height: u32) -> Self {
        let canvas = Canvas::new(width, height, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits");
        Self::new(Document::new(Picture::plain(canvas)))
    }

    fn relayout(&mut self) {
        let theme = self.registry.active();
        self.layout = self
            .view
            .layout(WINDOW.0, WINDOW.1, theme, Scale::ONE, faces(theme));
        self.view.settle(&self.layout, &mut Region::new());
    }

    fn apply(&mut self, outcome: &Outcome) {
        if outcome.relayout != Relayout::None {
            self.relayout();
        }
    }

    fn pointer(&mut self, event: InputEvent) -> Outcome {
        self.now += 10_000_000;
        let theme = self.registry.active();
        let outcome = self.view.on_pointer(
            &event,
            self.now,
            &self.layout,
            Scale::ONE,
            theme,
            &mut Region::new(),
        );
        self.apply(&outcome);
        outcome
    }

    fn move_to(&mut self, at: Point) -> Outcome {
        self.pointer(InputEvent::PointerMoved { to: at })
    }

    fn press(&mut self, button: PointerButton) -> Outcome {
        self.pointer(InputEvent::PointerPressed { button })
    }

    fn release(&mut self, button: PointerButton) -> Outcome {
        self.pointer(InputEvent::PointerReleased { button })
    }

    /// Drag with the primary button from picture pixel `from` to `to`.
    fn drag(&mut self, from: (u32, u32), to: (u32, u32)) -> Outcome {
        let start = self.screen_of(from);
        let end = self.screen_of(to);
        self.move_to(start);
        self.press(PointerButton::Primary);
        self.move_to(end);
        self.release(PointerButton::Primary)
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) -> Outcome {
        let theme = self.registry.active();
        let outcome = self.view.on_key(
            Keystroke {
                key,
                modifiers,
                at_ns: self.now,
            },
            &self.layout,
            Scale::ONE,
            theme,
            &mut Region::new(),
        );
        self.apply(&outcome);
        outcome
    }

    fn act(&mut self, action: Action) -> Outcome {
        let outcome = self.view.act(action, &self.layout, &mut Region::new());
        self.apply(&outcome);
        outcome
    }

    /// The screen point at the centre of picture pixel `(x, y)`.
    fn screen_of(&self, (x, y): (u32, u32)) -> Point {
        let size = self.size();
        let (ox, oy) = self.view.viewport().origin(size, self.layout.canvas());
        let (across, down) = self.view.viewport().pixel_span();
        let (across, down) = (
            i64::try_from(across.max(1)).expect("small"),
            i64::try_from(down.max(1)).expect("small"),
        );
        Point::new(
            i32::try_from(ox + i64::from(x) * across + across / 2).expect("on screen"),
            i32::try_from(oy + i64::from(y) * down + down / 2).expect("on screen"),
        )
    }

    fn size(&self) -> (u32, u32) {
        let picture = self.view.document().picture().expect("a picture");
        (picture.canvas.width(), picture.canvas.height())
    }

    fn colour(&self, x: u32, y: u32) -> [u8; 4] {
        self.view
            .document()
            .picture()
            .and_then(|picture| picture.canvas.colour_at(x, y))
            .expect("on the picture")
    }

    /// Carry out a compute request the way the worker would, answering what
    /// its landing led to.
    fn run_worker(&mut self, outcome: Outcome) -> Outcome {
        let Some(Request::Own(Own::Compute { job, work })) = outcome.request else {
            panic!("a request for a worker, not {:?}", outcome.request);
        };
        let answer = compute(work);
        let outcome = self
            .view
            .computed(job, answer, &self.layout, &mut Region::new());
        self.apply(&outcome);
        outcome
    }

    /// Mark out picture pixels `from` to `to` with the select tool and drag
    /// them by `by`, lifting them into a floating layer.
    fn lift(&mut self, from: (u32, u32), to: (u32, u32), by: (u32, u32)) {
        self.act(Action::Tool(Tool::Select));
        self.drag(from, to);
        self.drag(from, (from.0 + by.0, from.1 + by.1));
        assert!(self.view.floating().is_some(), "lifted");
    }
}

fn plain() -> Modifiers {
    Modifiers::default()
}

fn ctrl() -> Modifiers {
    Modifiers {
        ctrl: true,
        ..Modifiers::default()
    }
}

#[test]
fn a_pencil_line_sets_exact_pixels_and_undo_takes_it_back() {
    let mut window = Window::white(40, 30);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((2, 2), (12, 2));
    for x in 2..=12 {
        assert_eq!(window.colour(x, 2), [0, 0, 0, 255], "pixel {x}");
    }
    assert_eq!(window.colour(2, 3), [255; 4]);
    assert!(SavedDocument::is_modified(&window.view));
    window.key(Key::Char('z'), ctrl());
    assert_eq!(window.colour(5, 2), [255; 4]);
    assert!(!SavedDocument::is_modified(&window.view));
    window.key(Key::Char('y'), ctrl());
    assert_eq!(window.colour(5, 2), [0, 0, 0, 255]);
}

#[test]
fn the_middle_button_paints_in_the_secondary_colour() {
    let mut window = Window::white(20, 20);
    window.act(Action::Tool(Tool::Pencil));
    window.act(Action::SwapColours);
    let at = window.screen_of((4, 4));
    window.move_to(at);
    window.press(PointerButton::Middle);
    window.release(PointerButton::Middle);
    assert_eq!(
        window.colour(4, 4),
        [0, 0, 0, 255],
        "black is secondary once swapped"
    );
}

#[test]
fn a_rectangle_is_shown_while_dragged_and_drawn_when_let_go() {
    let mut window = Window::white(50, 50);
    window.act(Action::Tool(Tool::Rectangle));
    let (start, end) = (window.screen_of((5, 5)), window.screen_of((20, 15)));
    window.move_to(start);
    window.press(PointerButton::Primary);
    window.move_to(end);
    assert!(
        window.view.preview().is_some(),
        "the shape follows the pointer"
    );
    assert_eq!(
        window.colour(5, 5),
        [255; 4],
        "nothing is drawn until it lets go"
    );
    window.release(PointerButton::Primary);
    assert!(window.view.preview().is_none());
    assert_eq!(window.colour(5, 5), [0, 0, 0, 255]);
    assert_eq!(
        window.colour(12, 10),
        [255; 4],
        "an outline leaves its middle"
    );
}

#[test]
fn a_fill_is_done_by_a_worker_and_edits_wait_for_it() {
    let mut window = Window::white(30, 30);
    window.act(Action::Tool(Tool::Fill));
    let at = window.screen_of((10, 10));
    window.move_to(at);
    let outcome = window.press(PointerButton::Primary);
    assert!(window.view.busy());
    window.act(Action::Tool(Tool::Pencil));
    window.drag((1, 1), (3, 1));
    assert_eq!(
        window.colour(2, 1),
        [255; 4],
        "no edit lands while the fill is out"
    );
    window.run_worker(outcome);
    assert!(!window.view.busy());
    assert_eq!(window.colour(0, 0), [0, 0, 0, 255]);
    assert_eq!(window.colour(29, 29), [0, 0, 0, 255]);
    window.key(Key::Char('z'), ctrl());
    assert_eq!(window.colour(0, 0), [255; 4], "the fill is one step");
}

#[test]
fn an_answer_to_another_job_is_not_taken() {
    let mut window = Window::white(10, 10);
    let canvas = Canvas::new(3, 3, Kind::Rgba, Sample::Rgba([1; 4])).expect("fits");
    window.view.computed(
        99,
        Computed::Picture(Ok(canvas)),
        &window.layout,
        &mut Region::new(),
    );
    assert_eq!(window.size(), (10, 10));
}

#[test]
fn a_quarter_turn_swaps_the_sides() {
    let mut window = Window::white(40, 10);
    let outcome = window.act(Action::RotateRight);
    window.run_worker(outcome);
    assert_eq!(window.size(), (10, 40));
    window.key(Key::Char('z'), ctrl());
    assert_eq!(window.size(), (40, 10));
}

#[test]
fn a_selection_is_lifted_moved_and_put_down_as_one_step() {
    let mut window = Window::white(60, 60);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((10, 10), (10, 10));
    window.act(Action::Tool(Tool::Select));
    window.drag((8, 8), (12, 12));
    assert!(window.view.selection().is_some());
    window.drag((10, 10), (30, 20));
    assert!(window.view.floating().is_some());
    let put = window.key(Key::Named(NamedKey::Enter), plain());
    assert!(
        window.view.floating().is_some(),
        "it floats until the worker has put it down"
    );
    window.run_worker(put);
    assert!(window.view.floating().is_none());
    assert_eq!(
        window.colour(30, 20),
        [0, 0, 0, 255],
        "the dot went with it"
    );
    assert_eq!(
        window.colour(10, 10)[3],
        0,
        "and left the picture clear behind it"
    );
    window.key(Key::Char('z'), ctrl());
    assert_eq!(window.colour(10, 10), [0, 0, 0, 255]);
    assert_eq!(window.colour(30, 20), [255; 4]);
}

#[test]
fn escape_puts_a_lifted_selection_back() {
    let mut window = Window::white(40, 40);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((5, 5), (5, 5));
    window.act(Action::Tool(Tool::Select));
    window.drag((4, 4), (6, 6));
    window.drag((5, 5), (20, 20));
    window.key(Key::Named(NamedKey::Escape), plain());
    assert!(window.view.floating().is_none());
    assert_eq!(window.colour(5, 5), [0, 0, 0, 255]);
    assert_eq!(window.colour(20, 20), [255; 4]);
}

/// Escape mid-drag keeps nothing the drag did: a marquee marks nothing, and
/// a dragged layer goes back to where that drag began, still floating.
#[test]
fn escape_mid_drag_keeps_nothing_the_drag_did() {
    let mut window = Window::white(40, 40);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((5, 5), (5, 5));
    window.act(Action::Tool(Tool::Select));
    window.move_to(window.screen_of((2, 2)));
    window.press(PointerButton::Primary);
    window.move_to(window.screen_of((8, 8)));
    assert!(window.view.selection.is_some(), "marked while dragged");
    window.key(Key::Named(NamedKey::Escape), plain());
    window.release(PointerButton::Primary);
    assert_eq!(window.view.selection, None);
    window.lift((4, 4), (6, 6), (10, 0));
    let at = |window: &Window| {
        window
            .view
            .floating()
            .map(|floating| (floating.bounds().x0, floating.bounds().y0))
    };
    assert_eq!(at(&window), Some((14, 4)));
    window.move_to(window.screen_of((15, 5)));
    window.press(PointerButton::Primary);
    window.move_to(window.screen_of((30, 30)));
    window.key(Key::Named(NamedKey::Escape), plain());
    window.release(PointerButton::Primary);
    assert_eq!(at(&window), Some((14, 4)), "back where the drag began");
    assert_eq!(
        window
            .view
            .floating()
            .and_then(|floating| floating.sample_at(15, 5)),
        Some(Sample::Rgba([0, 0, 0, 255])),
        "carrying what it lifted"
    );
}

#[test]
fn a_picked_colour_becomes_the_primary() {
    let canvas = Canvas::new(10, 10, Kind::Rgba, Sample::Rgba([12, 34, 56, 255])).expect("fits");
    let mut window = Window::new(Document::new(Picture::plain(canvas)));
    window.act(Action::Tool(Tool::Picker));
    let at = window.screen_of((3, 3));
    window.move_to(at);
    window.press(PointerButton::Primary);
    assert_eq!(window.view.inks().0, Ink::Colour([12, 34, 56, 255]));
}

#[test]
fn the_eraser_clears_a_picture_that_can_be_clear() {
    let mut window = Window::white(20, 20);
    window.act(Action::Tool(Tool::Eraser));
    window.drag((10, 10), (10, 10));
    assert_eq!(window.colour(10, 10)[3], 0);
}

#[test]
fn spray_lays_more_paint_while_held_still() {
    let mut window = Window::white(80, 80);
    window.act(Action::Tool(Tool::Spray));
    let at = window.screen_of((40, 40));
    window.move_to(at);
    window.press(PointerButton::Primary);
    let painted = |window: &Window| {
        (30..50)
            .flat_map(|y| (30..50).map(move |x| (x, y)))
            .filter(|&(x, y)| window.colour(x, y) != [255; 4])
            .count()
    };
    let first = painted(&window);
    assert!(first > 0);
    window.view.arm_deadline(window.now);
    let due = window.view.deadline_ns().expect("a spray tick is due");
    let theme = window.registry.active();
    window
        .view
        .tick(due, &window.layout, Scale::ONE, theme, &mut Region::new());
    assert!(painted(&window) > first, "the tick sprayed more");
    window.release(PointerButton::Primary);
    window.view.arm_deadline(window.now);
    assert_eq!(
        window.view.deadline_ns(),
        None,
        "nothing to wake for once let go"
    );
}

#[test]
fn a_paste_floats_until_it_is_put_down() {
    let mut window = Window::white(30, 30);
    let pasted = Canvas::new(4, 4, Kind::Rgba, Sample::Rgba([9, 9, 9, 255])).expect("fits");
    let outcome = window
        .view
        .pasted(Ok((pasted, Kind::Rgba)), &window.layout, &mut Region::new());
    window.apply(&outcome);
    assert_eq!(window.view.tool(), Tool::Select);
    let floating = window.view.floating().expect("floating").bounds();
    assert_eq!(
        (floating.x0, floating.y0),
        (0, 0),
        "at the picture's visible corner"
    );
    assert_eq!(window.colour(1, 1), [255; 4], "not yet down");
    let put = window.key(Key::Named(NamedKey::Enter), plain());
    window.run_worker(put);
    assert_eq!(window.colour(1, 1), [9, 9, 9, 255]);
}

#[test]
fn copying_needs_a_selection_and_hands_over_its_pixels() {
    let mut window = Window::white(30, 30);
    assert!(window.act(Action::Copy).request.is_none());
    window.act(Action::SelectAll);
    let Some(Request::Own(Own::Copy(clip))) = window.act(Action::Copy).request else {
        panic!("a copy");
    };
    let pixels = clip.pixels().expect("room");
    assert_eq!((pixels.width(), pixels.height()), (30, 30));
}

#[test]
fn keys_choose_tools_and_swap_colours() {
    let mut window = Window::white(10, 10);
    window.key(Key::Char('e'), plain());
    assert_eq!(window.view.tool(), Tool::Eraser);
    let (primary, secondary) = window.view.inks();
    window.key(Key::Char('x'), plain());
    assert_eq!(window.view.inks(), (secondary, primary));
    assert_eq!(shortcut(Key::Char('s'), ctrl()), Some(Action::Save));
    assert_eq!(
        shortcut(
            Key::Char('S'),
            Modifiers {
                shift: true,
                ..ctrl()
            }
        ),
        Some(Action::SaveAs)
    );
}

#[test]
fn every_action_is_chosen_by_its_own_id() {
    let mut actions = vec![
        Action::Tool(Tool::Ellipse),
        Action::Zoom(0),
        Action::Zoom(15),
        Action::Rename,
    ];
    actions.extend(super::PLAIN_ACTIONS);
    let mut ids = alloc::collections::BTreeSet::new();
    for action in actions {
        assert_eq!(Action::from_id(action.id()), Some(action), "{action:?}");
        assert!(ids.insert(action.id()), "{action:?} shares an id");
    }
    assert!(!ids.contains(&GO_TO_ENTRY) && !ids.contains(&RENAME_ENTRY));
    assert_eq!(Action::from_id(0), None);
}

#[test]
fn the_window_menu_offers_the_sprite_rows_with_their_fields() {
    let window = Window::white(10, 10);
    let menu = window.view.menu(MenuKind::Window);
    let fields: vec::Vec<u16> = menu
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => item.entry.map(|entry| entry.id.get()),
            _ => None,
        })
        .collect();
    assert_eq!(fields, [GO_TO_ENTRY, RENAME_ENTRY]);
    let zoom = window.view.menu(MenuKind::Zoom);
    assert_eq!(zoom.rows().count(), crate::viewport::ZOOMS.len());
}

fn sprite(name: &str, shade: u8) -> Entry {
    let kind = Kind::Indexed {
        depth: IndexDepth::Four,
        palette: vec![[shade, 0, 0, 255], [255, 255, 255, 255]],
        masked: false,
    };
    Entry::Picture(Picture {
        canvas: Canvas::new(8, 8, kind, Sample::Index(0, 255)).expect("fits"),
        sprite: Some(SpriteInfo {
            name: SpriteName::new(name).expect("a name"),
            mode: SpriteMode::indexed(IndexDepth::Four, (1, 1), false),
            palette: SpritePalette::Full,
            masked: false,
        }),
    })
}

/// A white truecolour sprite called `name`.
fn colour_sprite(name: &str) -> Entry {
    Entry::Picture(Picture {
        canvas: Canvas::new(8, 8, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits"),
        sprite: Some(SpriteInfo {
            name: SpriteName::new(name).expect("a name"),
            mode: SpriteMode::truecolour((1, 1), false),
            palette: SpritePalette::Implied,
            masked: false,
        }),
    })
}

fn colour_sprites() -> Window {
    let document = Document::of(
        vec![colour_sprite("one"), colour_sprite("two")],
        Origin::Read(tairix_sandbox::imagerender::ViewFormat::Sprite),
        Unkept::default(),
    )
    .expect("entries");
    Window::new(document)
}

fn sprites() -> Window {
    let document = Document::of(
        vec![sprite("one", 10), sprite("two", 20)],
        Origin::Read(tairix_sandbox::imagerender::ViewFormat::Sprite),
        Unkept::default(),
    )
    .expect("entries");
    Window::new(document)
}

/// A copy of a sprite takes the least numbered name its own leaves free, and
/// a new sprite is offered the stem itself while no sprite has it.
#[test]
fn a_copied_sprite_takes_a_name_of_its_own() {
    let mut window = sprites();
    window.act(Action::DuplicateSprite);
    assert_eq!(window.view.document().current(), 1, "the copy shows");
    window.act(Action::PreviousSprite);
    window.act(Action::DuplicateSprite);
    let names: vec::Vec<String> = window
        .view
        .document()
        .names()
        .map(|name| alloc::format!("{name}"))
        .collect();
    assert_eq!(names, ["one", "one2", "one1", "two"]);
    window.act(Action::NewSprite);
    let Some(super::Modal::Form(form)) = &window.view.modal else {
        panic!("the new sprite form shows");
    };
    let offered = form.new_sprite_answer().expect("the offer stands");
    assert_eq!(offered.name.as_bytes(), b"sprite");
}

#[test]
fn sprites_are_stepped_through_found_by_name_and_renamed() {
    let mut window = sprites();
    window.key(Key::Named(NamedKey::PageDown), plain());
    assert_eq!(window.view.document().current(), 1);
    let entered = |window: &mut Window, id: u16, text: &str| {
        let id = AppMenuItemId::new(id).expect("an id");
        let outcome = window
            .view
            .entered(id, text, &window.layout, &mut Region::new());
        window.apply(&outcome);
    };
    entered(&mut window, GO_TO_ENTRY, "ONE");
    assert_eq!(window.view.document().current(), 0);
    entered(&mut window, RENAME_ENTRY, "two");
    assert_eq!(
        window.view.message(),
        Some("A sprite of that name is already here")
    );
    entered(&mut window, RENAME_ENTRY, "first");
    let name = window
        .view
        .document()
        .entry()
        .name()
        .map(|n| n.as_bytes().to_vec());
    assert_eq!(name.as_deref(), Some(&b"first"[..]));
    entered(&mut window, GO_TO_ENTRY, "2");
    assert_eq!(window.view.document().current(), 1);
}

#[test]
fn a_palette_picture_paints_in_its_entries_and_offers_them_as_wells() {
    let mut window = sprites();
    let (primary, secondary) = window.view.inks();
    assert!(matches!(primary, Ink::Index(_)) && matches!(secondary, Ink::Index(_)));
    window.act(Action::Tool(Tool::Brush));
    window.drag((4, 4), (4, 4));
    let sample = window
        .view
        .document()
        .picture()
        .and_then(|picture| picture.canvas.sample(4, 4));
    assert!(matches!(sample, Some(Sample::Index(_, 255))));
}

#[test]
fn closing_with_changes_asks_first() {
    let mut window = Window::white(10, 10);
    let outcome = window
        .view
        .close_requested(&window.layout, &mut Region::new());
    assert!(matches!(outcome.request, Some(Request::Close)));
    window.act(Action::Tool(Tool::Pencil));
    window.drag((1, 1), (1, 1));
    let outcome = window
        .view
        .close_requested(&window.layout, &mut Region::new());
    assert!(outcome.request.is_none());
    assert!(window.view.asking());
    window.key(Key::Named(NamedKey::Escape), plain());
    assert!(!window.view.asking(), "Escape keeps the window open");
}

/// A quit is never left waiting on a question it did not ask.
#[test]
fn closing_puts_the_save_question_in_place_of_another() {
    let mut window = Window::white(10, 10);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((1, 1), (1, 1));
    window.act(Action::NewPicture);
    assert!(window.view.asking());
    assert!(!window.view.asking_to_close());
    let outcome = window
        .view
        .close_requested(&window.layout, &mut Region::new());
    assert!(outcome.request.is_none());
    assert!(window.view.asking_to_close());
}

#[test]
fn a_document_of_several_sprites_is_not_saved_as_a_png() {
    let window = sprites();
    assert_eq!(
        window.view.save_format("both.png"),
        Err(SaveRefusal::SeveralPictures(2))
    );
    assert!(window.view.save_format("both.spr").is_ok());
}

#[test]
fn the_title_marks_a_changed_and_a_read_only_document() {
    let mut window = Window::white(10, 10);
    let mut title = String::new();
    window.view.write_title(&mut title);
    assert_eq!(title, "picture.png \u{2014} Paint");
    window.act(Action::Tool(Tool::Pencil));
    window.drag((1, 1), (1, 1));
    window.view.write_title(&mut title);
    assert!(title.starts_with('*'));
}

#[test]
fn a_new_picture_asked_for_is_handed_to_run() {
    let mut window = Window::white(10, 10);
    window.act(Action::NewPicture);
    assert!(window.view.asking());
    let outcome = window.key(Key::Named(NamedKey::Enter), plain());
    let Some(Request::Own(Own::NewWindow(picture))) = outcome.request else {
        panic!("a new window");
    };
    assert_eq!(
        picture,
        NewPicture {
            size: (10, 10),
            ..NewPicture::DEFAULT
        }
    );
}

#[test]
fn zooming_with_the_wheel_keeps_the_pixel_under_the_pointer() {
    // Larger than the window at every rung, so it scrolls rather than
    // sitting centred.
    let mut window = Window::white(2000, 2000);
    let at = window.screen_of((30, 40));
    window.move_to(at);
    window.key(Key::Named(NamedKey::Escape), plain());
    let start = window.view.viewport().rung().expect("on a rung");
    for step in 1..=3 {
        window.view.on_pointer(
            &InputEvent::ModifiersChanged { modifiers: ctrl() },
            0,
            &window.layout,
            Scale::ONE,
            window.registry.active(),
            &mut Region::new(),
        );
        window.pointer(InputEvent::PointerScrolled {
            dx: 0,
            dy: -SCROLL_UNITS_PER_DETENT,
        });
        assert_eq!(
            window.view.viewport().rung(),
            Some(start + step),
            "a rung a detent"
        );
    }
    assert_eq!(window.view.hover(), Some((30, 40)));
    let still = window
        .view
        .viewport()
        .pixel_at(at, window.size(), window.layout.canvas());
    assert_eq!(still, Some((30, 40)));
}

/// A fine wheel's fractions of a detent add up to a rung, and a turn back
/// starts afresh rather than being shortened by what the turn in left.
#[test]
fn a_fine_ctrl_wheel_zooms_a_rung_per_detent_turned() {
    let mut window = Window::white(2000, 2000);
    window.move_to(window.screen_of((30, 40)));
    window.view.on_pointer(
        &InputEvent::ModifiersChanged { modifiers: ctrl() },
        0,
        &window.layout,
        Scale::ONE,
        window.registry.active(),
        &mut Region::new(),
    );
    let start = window.view.viewport().rung().expect("on a rung");
    let turn = |window: &mut Window, dy: i32, times: usize| {
        for _ in 0..times {
            window.pointer(InputEvent::PointerScrolled { dx: 0, dy });
        }
    };
    turn(&mut window, -15, 7);
    assert_eq!(
        window.view.viewport().rung(),
        Some(start),
        "seven eighths of a detent"
    );
    turn(&mut window, -15, 1);
    assert_eq!(window.view.viewport().rung(), Some(start + 1));
    turn(&mut window, 15, 7);
    assert_eq!(
        window.view.viewport().rung(),
        Some(start + 1),
        "the turn back starts afresh"
    );
    turn(&mut window, 15, 1);
    assert_eq!(window.view.viewport().rung(), Some(start));
}

fn pinching(phase: tairix_input::PinchPhase, scale: u32, at: Point) -> InputEvent {
    InputEvent::Pinch { phase, scale, at }
}

/// A pinch zooms continuously by the fingers' spread, follows their centre
/// as it moves, and once it ends the stepping commands go on from the
/// nearest rung in their direction.
#[test]
fn a_pinch_zooms_smoothly_about_where_it_began_and_follows_the_fingers() {
    use tairix_abi::touch::PINCH_SCALE_ONE;
    use tairix_input::PinchPhase;

    let mut window = Window::white(2000, 2000);
    let at = window.screen_of((30, 40));
    window.move_to(at);
    let start = window.view.viewport().zoom();
    window.pointer(pinching(PinchPhase::Begin, PINCH_SCALE_ONE, at));
    window.pointer(pinching(PinchPhase::Update, PINCH_SCALE_ONE * 3 / 2, at));
    let viewport = window.view.viewport();
    assert_eq!(viewport.zoom(), start.scaled(PINCH_SCALE_ONE * 3 / 2));
    assert_eq!(viewport.rung(), None, "between two rungs");
    assert_eq!(
        viewport.pixel_at(at, window.size(), window.layout.canvas()),
        Some((30, 40)),
        "the pixel under the pinch stays under it"
    );
    // The fingers' centre moves: the picture follows it, toward the far
    // edges, where it has room to go.
    let moved = Point::new(at.x - 25, at.y - 15);
    assert!(window.layout.canvas().contains(moved));
    window.pointer(pinching(PinchPhase::Update, PINCH_SCALE_ONE * 3 / 2, moved));
    assert_eq!(
        window
            .view
            .viewport()
            .pixel_at(moved, window.size(), window.layout.canvas()),
        Some((30, 40)),
        "carried to the fingers"
    );
    window.pointer(pinching(PinchPhase::End, PINCH_SCALE_ONE * 3 / 2, moved));
    let ended = window.view.viewport().zoom();
    assert_eq!(
        ended,
        start.scaled(PINCH_SCALE_ONE * 3 / 2),
        "the zoom stands"
    );
    window.act(Action::ZoomOut);
    assert_eq!(
        window.view.viewport().zoom(),
        start,
        "zooming out lands on the rung just below"
    );
}

/// A cancelled pinch puts the view back; a step that never began, or one that
/// began off the canvas, moves nothing.
#[test]
fn a_cancelled_pinch_puts_the_view_back_and_a_stray_step_moves_nothing() {
    use tairix_abi::touch::PINCH_SCALE_ONE;
    use tairix_input::PinchPhase;

    let mut window = Window::white(2000, 2000);
    let at = window.screen_of((30, 40));
    window.move_to(at);
    let before = *window.view.viewport();
    window.pointer(pinching(PinchPhase::Update, 2 * PINCH_SCALE_ONE, at));
    assert_eq!(*window.view.viewport(), before, "no pinch had begun");
    window.pointer(pinching(PinchPhase::Begin, PINCH_SCALE_ONE, at));
    window.pointer(pinching(PinchPhase::Update, 4 * PINCH_SCALE_ONE, at));
    assert_ne!(*window.view.viewport(), before);
    window.pointer(pinching(PinchPhase::Cancel, 4 * PINCH_SCALE_ONE, at));
    assert_eq!(*window.view.viewport(), before, "put back");

    let off = Point::new(0, 0);
    assert!(!window.layout.canvas().contains(off));
    window.pointer(pinching(PinchPhase::Begin, PINCH_SCALE_ONE, off));
    window.pointer(pinching(PinchPhase::Update, 4 * PINCH_SCALE_ONE, off));
    assert_eq!(*window.view.viewport(), before, "begun off the canvas");
}

/// Begin a pencil stroke at picture pixel `from` and carry it to `to`,
/// leaving the button down.
fn stroke_held(window: &mut Window, from: (u32, u32), to: (u32, u32)) {
    window.act(Action::Tool(Tool::Pencil));
    window.move_to(window.screen_of(from));
    window.press(PointerButton::Primary);
    window.move_to(window.screen_of(to));
}

/// Showing another sprite mid-stroke finishes the stroke on the sprite it
/// was laid on, and the rest of the drag paints nothing.
#[test]
fn a_stroke_stays_on_its_own_sprite_when_another_is_shown() {
    let mut window = colour_sprites();
    stroke_held(&mut window, (1, 1), (3, 1));
    window.act(Action::NextSprite);
    assert_eq!(window.view.document().current(), 1);
    window.move_to(window.screen_of((5, 5)));
    window.release(PointerButton::Primary);
    assert_eq!(
        window.colour(5, 5),
        [255; 4],
        "the drag ended with the stroke"
    );
    window.act(Action::PreviousSprite);
    assert_eq!(
        window.colour(2, 1),
        [0, 0, 0, 255],
        "the stroke is on its sprite"
    );
    window.key(Key::Char('z'), ctrl());
    assert_eq!(window.colour(2, 1), [255; 4], "and is one step");
}

/// Escape turns a stroke down: its paint comes back off the picture, and
/// nothing is left for an undo to find.
#[test]
fn escape_takes_a_stroke_back_off_the_picture() {
    let mut window = Window::white(10, 10);
    stroke_held(&mut window, (1, 1), (4, 1));
    assert_eq!(window.colour(2, 1), [0, 0, 0, 255]);
    window.key(Key::Named(NamedKey::Escape), plain());
    assert_eq!(window.colour(2, 1), [255; 4]);
    window.release(PointerButton::Primary);
    assert!(!SavedDocument::is_modified(&window.view));
    assert_eq!(window.view.document().history_depth(), 0);
}

/// A drag belongs to the button that began it.
#[test]
fn another_button_neither_ends_a_drag_nor_begins_one() {
    let mut window = Window::white(10, 10);
    stroke_held(&mut window, (1, 1), (2, 1));
    window.press(PointerButton::Middle);
    window.release(PointerButton::Middle);
    window.move_to(window.screen_of((4, 1)));
    window.release(PointerButton::Primary);
    assert_eq!(window.colour(4, 1), [0, 0, 0, 255], "the drag went on");
    assert_eq!(window.view.document().history_depth(), 1, "as one stroke");
}

/// The window's menu takes the pointer, so it finishes a drag first.
#[test]
fn a_menu_opened_mid_drag_finishes_the_drag() {
    let mut window = Window::white(10, 10);
    stroke_held(&mut window, (1, 1), (3, 1));
    let outcome = window.press(PointerButton::Secondary);
    assert!(matches!(
        outcome.request,
        Some(Request::Menu {
            kind: MenuKind::Window,
            ..
        })
    ));
    assert_eq!(window.view.document().history_depth(), 1);
    window.move_to(window.screen_of((6, 1)));
    assert_eq!(window.colour(6, 1), [255; 4], "nothing more is drawn");
}

/// Nothing changes the document, or the sprite showing, while a worker has
/// the picture, so its answer lands on the sprite it was asked of.
#[test]
fn a_worker_answer_lands_on_the_sprite_it_was_asked_of() {
    let mut window = colour_sprites();
    window.act(Action::Tool(Tool::Fill));
    window.move_to(window.screen_of((2, 2)));
    let outcome = window.press(PointerButton::Primary);
    assert!(window.view.busy());
    window.act(Action::NextSprite);
    assert_eq!(window.view.document().current(), 0, "the sprite stays");
    assert_eq!(
        window.view.message(),
        Some("Wait: the picture is being worked on")
    );
    window.act(Action::DeleteSprite);
    window.act(Action::DuplicateSprite);
    assert_eq!(window.view.document().entries().len(), 2);
    window.run_worker(outcome);
    assert_eq!(window.colour(7, 7), [0, 0, 0, 255], "filled where asked");
    window.act(Action::NextSprite);
    assert_eq!(window.colour(7, 7), [255; 4], "and nowhere else");
}

/// A transform asked mid-stroke finishes the stroke first, so its answer
/// replaces a picture no stroke is still being laid on.
#[test]
fn a_transform_asked_mid_stroke_takes_the_stroke_with_it() {
    let mut window = Window::white(10, 10);
    stroke_held(&mut window, (0, 0), (0, 0));
    let outcome = window.act(Action::FlipAcross);
    window.run_worker(outcome);
    window.move_to(window.screen_of((5, 5)));
    window.release(PointerButton::Primary);
    assert_eq!(
        window.colour(9, 0),
        [0, 0, 0, 255],
        "the stroke turned with it"
    );
    assert_eq!(window.colour(5, 5), [255; 4]);
}

/// A tool change repaints the toolbar and the panel its settings sit in,
/// never the canvas.
#[test]
fn a_tool_change_repaints_the_toolbar_and_panel_not_the_canvas() {
    let window = Window::white(40, 40);
    let mut view = window.view;
    let mut damage = Region::new();
    let outcome = view.act(Action::Tool(Tool::Pencil), &window.layout, &mut damage);
    assert_eq!(outcome.relayout, Relayout::Reported);
    let canvas = window.layout.canvas();
    assert!(!damage.is_empty());
    assert!(
        damage
            .rects()
            .iter()
            .all(|rect| rect.intersection(&canvas).is_empty()),
        "the canvas is not drawn again"
    );
}

/// A tool change with a setting's choice list open repaints the whole
/// window, because the list it closes hangs over the canvas.
#[test]
fn a_tool_change_with_a_list_open_repaints_the_window() {
    let mut window = Window::white(40, 40);
    window.act(Action::Tool(Tool::Rectangle));
    let theme = window.registry.active();
    let bounds = window.layout.settings();
    let settings = window
        .view
        .settings
        .layout(bounds, window.layout.window(), Scale::ONE, theme);
    let style = 1;
    let row = window
        .view
        .settings
        .row_rect(style, settings, Scale::ONE, theme)
        .expect("the style row is laid out");
    let slot = window.view.settings.rows()[style]
        .control_rect(FieldLayout::new(row, settings.column), Scale::ONE, theme)
        .expect("the style slot is laid out");
    window.move_to(slot.center());
    window.press(PointerButton::Primary);
    window.release(PointerButton::Primary);
    assert!(
        window.view.settings.rows()[style].popup_open(),
        "the style list is open"
    );
    let mut damage = Region::new();
    let outcome = window
        .view
        .act(Action::Tool(Tool::Pencil), &window.layout, &mut damage);
    assert_eq!(outcome.relayout, Relayout::Whole);
}

/// A fill repaints what it filled, not the whole canvas.
#[test]
fn a_fill_repaints_only_what_it_filled() {
    let mut window = Window::white(200, 200);
    window.act(Action::Tool(Tool::Rectangle));
    window.drag((0, 0), (10, 10));
    window.act(Action::Tool(Tool::Fill));
    window.move_to(window.screen_of((5, 5)));
    let outcome = window.press(PointerButton::Primary);
    let Some(Request::Own(Own::Compute { job, work })) = outcome.request else {
        panic!("a fill for a worker");
    };
    let mut damage = Region::new();
    let _ = window
        .view
        .computed(job, compute(work), &window.layout, &mut damage);
    let canvas = window.layout.canvas();
    let painted: u64 = damage
        .rects()
        .iter()
        .map(|rect| {
            u64::from(rect.intersection(&canvas).width)
                * u64::from(rect.intersection(&canvas).height)
        })
        .sum();
    assert!(
        painted < u64::from(canvas.width) * u64::from(canvas.height),
        "a fill inside a small square does not repaint the whole canvas"
    );
}

/// Shift squares a shape being dragged, and the preview follows at once.
#[test]
fn a_modifier_change_mid_shape_repaints_the_preview() {
    let mut window = Window::white(40, 40);
    window.act(Action::Tool(Tool::Rectangle));
    window.move_to(window.screen_of((2, 2)));
    window.press(PointerButton::Primary);
    window.move_to(window.screen_of((20, 8)));
    let theme = window.registry.active();
    let mut damage = Region::new();
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    let _ = window.view.on_pointer(
        &InputEvent::ModifiersChanged { modifiers: shift },
        window.now,
        &window.layout,
        Scale::ONE,
        theme,
        &mut damage,
    );
    assert!(!damage.is_empty(), "the squared preview is drawn");
}

/// An action that needs the floating layer down waits for it: the layer is
/// put down on a worker first, and only once that lands does the action
/// follow. A put-down refused leaves the layer floating and does nothing.
#[test]
fn an_action_waits_for_the_floating_layer_to_be_put_down() {
    let mut window = Window::white(60, 60);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((10, 10), (10, 10));
    window.lift((8, 8), (12, 12), (20, 10));
    let refused = window.act(Action::FlipAcross);
    let Some(Request::Own(Own::Compute { job, .. })) = refused.request else {
        panic!("a put-down for the worker");
    };
    let outcome = window.view.computed(
        job,
        Computed::Tiles(Err(OutOfMemory)),
        &window.layout,
        &mut Region::new(),
    );
    assert!(outcome.request.is_none(), "the flip did not follow");
    assert!(window.view.floating().is_some(), "still floating");
    assert_eq!(window.colour(30, 20), [255; 4], "nothing was put down");
    let asked = window.act(Action::FlipAcross);
    let flip = window.run_worker(asked);
    assert!(window.view.floating().is_none(), "down");
    assert_eq!(window.colour(30, 20), [0, 0, 0, 255]);
    window.run_worker(flip);
    assert_eq!(window.colour(29, 20), [0, 0, 0, 255], "then flipped");
}

/// Cutting is one request: the copy taken from the picture as it stood, and
/// the clearing a worker's.
#[test]
fn a_cut_copies_and_clears_in_one_request() {
    let mut window = Window::white(30, 30);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((5, 5), (5, 5));
    window.act(Action::SelectAll);
    let Some(Request::Own(Own::Cut { clip, job, work })) = window.act(Action::Cut).request else {
        panic!("a cut");
    };
    let pixels = clip.pixels().expect("room");
    assert_eq!(
        pixels.colour_at(5, 5),
        Some([0, 0, 0, 255]),
        "copied as it stood"
    );
    let outcome = window
        .view
        .computed(job, compute(work), &window.layout, &mut Region::new());
    window.apply(&outcome);
    assert_eq!(window.colour(5, 5)[3], 0, "and cleared");
}

/// A paste decoded for a picture of another kind than the one now showing
/// is refused rather than floated in the wrong colours.
#[test]
fn a_paste_adapted_to_another_kind_is_refused() {
    let mut window = Window::white(10, 10);
    let pasted = Canvas::new(2, 2, Kind::Rgba, Sample::Rgba([1; 4])).expect("fits");
    let palette = Kind::Indexed {
        depth: IndexDepth::One,
        palette: vec![[0, 0, 0, 255], [255; 4]],
        masked: true,
    };
    let outcome = window
        .view
        .pasted(Ok((pasted, palette)), &window.layout, &mut Region::new());
    window.apply(&outcome);
    assert!(window.view.floating().is_none());
}

/// A press on a setting's open list where it hangs over the canvas is the
/// list's: nothing is drawn on the picture beneath it.
#[test]
fn a_press_on_a_list_over_the_canvas_draws_nothing() {
    let mut window = Window::white(40, 40);
    window.act(Action::Tool(Tool::Rectangle));
    let theme = window.registry.active();
    let bounds = window.layout.settings();
    let settings = window
        .view
        .settings
        .layout(bounds, window.layout.window(), Scale::ONE, theme);
    let style = 1;
    let row = window
        .view
        .settings
        .row_rect(style, settings, Scale::ONE, theme)
        .expect("the style row is laid out");
    let slot = window.view.settings.rows()[style]
        .control_rect(FieldLayout::new(row, settings.column), Scale::ONE, theme)
        .expect("the style slot is laid out");
    window.move_to(slot.center());
    window.press(PointerButton::Primary);
    window.release(PointerButton::Primary);
    let theme = window.registry.active();
    let open = window
        .view
        .settings
        .layout(bounds, window.layout.window(), Scale::ONE, theme);
    let over = open.popup.intersection(&window.layout.canvas());
    assert!(!over.is_empty(), "the list hangs over the canvas");
    window.move_to(over.center());
    window.press(PointerButton::Primary);
    window.move_to(window.screen_of((20, 20)));
    window.release(PointerButton::Primary);
    assert!(
        !SavedDocument::is_modified(&window.view),
        "no shape was drawn"
    );
}

/// Editing the palette entry an ink names offers its opacity where the
/// palette can hold one — a picture that is not a sprite — and not on a
/// sprite, whose palette is colours alone.
#[test]
fn a_palette_entry_is_edited_with_its_opacity_where_it_can_have_one() {
    use crate::dialog::Purpose;
    let kind = Kind::Indexed {
        depth: IndexDepth::Two,
        palette: vec![[0, 0, 0, 255], [255, 255, 255, 128]],
        masked: false,
    };
    let canvas = Canvas::new(4, 4, kind, Sample::Index(1, 255)).expect("fits");
    let mut window = Window::new(Document::new(Picture::plain(canvas)));
    window.act(Action::EditPrimary);
    let offered = |window: &Window| match &window.view.modal {
        Some(super::Modal::Form(form)) => match form.purpose() {
            Purpose::Colour { alpha, .. } => alpha,
            other => panic!("a colour form, not {other:?}"),
        },
        _ => panic!("a form"),
    };
    assert!(offered(&window), "a plain picture's entry has an opacity");
    let Entry::Picture(picture) = sprite("icon", 9) else {
        panic!("a picture");
    };
    let mut sprite = Window::new(Document::new(picture));
    sprite.act(Action::EditPrimary);
    assert!(!offered(&sprite), "a sprite's entry is a colour alone");
}

/// The pixel the status band states follows the picture when it moves under
/// a pointer that does not.
#[test]
fn the_pixel_under_a_still_pointer_follows_a_zoom() {
    let mut window = Window::white(200, 200);
    window.move_to(window.screen_of((50, 50)));
    assert_eq!(window.view.hover(), Some((50, 50)));
    window.act(Action::Zoom(crate::viewport::ACTUAL + 2));
    assert_ne!(
        window.view.hover(),
        Some((50, 50)),
        "the picture moved beneath"
    );
}

/// A stroke clears what was said before it began; what it says itself is
/// left for the user to read once it ends.
#[test]
fn a_new_stroke_clears_what_was_said_before_it() {
    let mut window = Window::white(20, 20);
    window.act(Action::Copy);
    assert!(window.view.message().is_some(), "nothing to copy, said");
    window.act(Action::Tool(Tool::Pencil));
    window.move_to(window.screen_of((3, 3)));
    window.press(PointerButton::Primary);
    assert!(window.view.message().is_none());
    window.release(PointerButton::Primary);
}

/// A sprite whose removal is refused keeps the layer floating over it.
#[test]
fn a_refused_sprite_removal_keeps_its_floating_layer() {
    let mut window = Window::white(40, 40);
    window.lift((4, 4), (8, 8), (10, 10));
    window.act(Action::DeleteSprite);
    assert!(
        window.view.floating().is_some(),
        "the only sprite stays, and so does its layer"
    );
}

/// No form opens while a worker has the picture.
#[test]
fn no_form_opens_while_the_picture_is_worked_on() {
    let mut window = Window::white(30, 30);
    window.act(Action::Tool(Tool::Fill));
    window.move_to(window.screen_of((5, 5)));
    let fill = window.press(PointerButton::Primary);
    assert!(fill.request.is_some(), "a fill for a worker");
    window.act(Action::Resize);
    assert!(window.view.modal.is_none());
}
