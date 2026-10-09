use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;

use tairix_abi::window_ipc::{AppMenuItemId, AppMenuRowView, CursorShape};
use tairix_colour::Rgba;
use tairix_controls::{Keystroke, WindowControlKind};
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_image::{IndexDepth, SpriteMode, SpriteName, SpritePalette, Unkept};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_theme::{Theme, ThemeRegistry};
use tairix_window::docapp::{DocumentView, Relayout, ToolGone, ToolMove, ToolOpening};
use tairix_window::document::{Access, SavedDocument};

use super::input::shortcut;
use super::{
    compute, Action, Computed, Marking, MenuKind, Outcome, Own, Request, View, GO_TO_ENTRY,
    GO_TO_LAYER, RENAME_ENTRY,
};
use crate::canvas::{Canvas, Kind, OutOfMemory, Sample};
use crate::colour::Ink;
use crate::document::{Document, Entry, NewPicture, Origin, Picture, SpriteInfo};
use crate::layout::{Faces, Layout};
use crate::mask::Mask;
use crate::pane::{PaneKind, Side};
use crate::save::SaveRefusal;
use crate::save::{SaveFormat, SaveSettings};
use crate::shape::{Bounds, Point as Fx};
use crate::tool::{
    tool_index, Marquee, Setting, Tool, ViewCommand, MAX_SIZE, VIEW_COMMANDS, WHOLE_PIXELS,
};
use crate::tool_controls::ToolControls;

const WINDOW: (u32, u32) = (900, 640);

fn faces(theme: &Theme) -> Faces {
    Faces::of(theme, Scale::ONE)
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
        let view = View::new(document, String::from("picture.png"), Access::Writable);
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
        let outcome =
            self.view
                .on_pointer(&event, &self.layout, Scale::ONE, theme, &mut Region::new());
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

    /// Click setting `index` of the tool-controls bar.
    fn click_setting(&mut self, index: usize) {
        let at = self
            .layout
            .bar()
            .control(index)
            .expect("the setting is seated")
            .center();
        self.move_to(at);
        self.press(PointerButton::Primary);
        self.release(PointerButton::Primary);
    }

    /// Open the shape tools' style list, answering where it hangs.
    fn open_style_list(&mut self) -> Rect {
        self.click_setting(1);
        assert!(self.view.bar.listing(), "the style list is open");
        let theme = self.registry.active();
        self.view
            .bar
            .popup_rect(self.layout.bar(), Scale::ONE, theme)
    }

    /// Tab from the picture until the dock has the keyboard.
    fn tab_into_dock(&mut self) {
        for _ in 0..16 {
            if self.view.picker.state().focus.focused {
                return;
            }
            self.key(Key::Named(NamedKey::Tab), plain());
        }
        panic!("Tab never reached the dock");
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
        (picture.canvas().width(), picture.canvas().height())
    }

    fn colour(&self, x: u32, y: u32) -> [u8; 4] {
        self.view
            .document()
            .picture()
            .and_then(|picture| picture.canvas().colour_at(x, y))
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

    /// A point `(across, down)` pixels into the colour dock's picker, whose
    /// plane fills its top-left corner.
    fn dock_point(&self, (across, down): (i32, i32)) -> Point {
        let picker = self.layout.picker();
        Point::new(picker.left() + across, picker.top() + down)
    }

    /// The palette the picture showing holds.
    fn palette(&self) -> Vec<[u8; 4]> {
        self.view
            .document()
            .picture()
            .and_then(|picture| picture.canvas().kind().palette())
            .expect("a palette picture")
            .to_vec()
    }

    /// Mark out picture pixels `from` to `to` with the select tool and drag
    /// them by `by`, lifting them into a floating selection.
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
        Computed::Picture(Ok(vec![canvas])),
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
    assert!(window.view.marking().is_some(), "outlined while dragged");
    window.key(Key::Named(NamedKey::Escape), plain());
    window.release(PointerButton::Primary);
    assert_eq!(window.view.selection, None);
    assert_eq!(window.view.marking(), None);
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
    window.act(Action::Tool(Tool::Eyedropper));
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
fn the_airbrush_lays_more_paint_while_held_still() {
    let mut window = Window::white(80, 80);
    window.act(Action::Tool(Tool::Airbrush));
    let at = window.screen_of((40, 40));
    window.move_to(at);
    window.press(PointerButton::Primary);
    let middle = |window: &Window| window.colour(40, 40)[0];
    let first = middle(&window);
    assert!(first < 255, "the first dab lands at once");
    window.view.arm_deadline(window.now);
    let due = window.view.deadline_ns().expect("an airbrush tick is due");
    let theme = window.registry.active();
    window
        .view
        .tick(due, &window.layout, Scale::ONE, theme, &mut Region::new());
    assert!(middle(&window) < first, "the tick built more up");
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
fn the_window_menu_offers_the_layer_and_sprite_rows_with_their_fields() {
    let window = Window::white(10, 10);
    let menu = window.view.menu(MenuKind::Window);
    let fields: vec::Vec<u16> = menu
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => item.entry.map(|entry| entry.id.get()),
            _ => None,
        })
        .collect();
    assert_eq!(fields, [GO_TO_LAYER, GO_TO_ENTRY, RENAME_ENTRY]);
    let zoom = window.view.menu(MenuKind::Zoom);
    assert_eq!(zoom.rows().count(), crate::viewport::ZOOMS.len());
}

/// Nothing the window menu offers is left out for want of room in the
/// bounds a menu is carried in.
#[test]
fn the_window_menu_holds_every_command() {
    let window = Window::white(10, 10);
    let menu = window.view.menu(MenuKind::Window);
    assert!(menu.len() < tairix_abi::window_ipc::APP_MENU_MAX_TOTAL_ROWS);
    let ids: Vec<u16> = menu
        .rows()
        .filter_map(|(row, _)| match row {
            AppMenuRowView::Item(item) => Some(item.id.get()),
            _ => None,
        })
        .collect();
    let offered = super::PLAIN_ACTIONS
        .into_iter()
        .chain(Tool::ALL.map(Action::Tool))
        .chain((0..crate::filter::Filter::ALL.len()).map(Action::Adjust))
        .chain(PaneKind::ALL.map(Action::Pane))
        .chain([Action::Rename]);
    for action in offered {
        assert!(ids.contains(&action.id()), "{action:?} is offered");
    }
}

fn sprite(name: &str, shade: u8) -> Entry {
    let kind = Kind::Indexed {
        depth: IndexDepth::Four,
        palette: vec![[shade, 0, 0, 255], [255, 255, 255, 255]],
        masked: false,
    };
    let mut picture = Picture::plain(Canvas::new(8, 8, kind, Sample::Index(0, 255)).expect("fits"));
    picture.sprite = Some(SpriteInfo {
        name: SpriteName::new(name).expect("a name"),
        mode: SpriteMode::indexed(IndexDepth::Four, (1, 1), false),
        palette: SpritePalette::Full,
        masked: false,
    });
    Entry::Picture(picture)
}

/// A white truecolour sprite called `name`.
fn colour_sprite(name: &str) -> Entry {
    let canvas = Canvas::new(8, 8, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits");
    let mut picture = Picture::plain(canvas);
    picture.sprite = Some(SpriteInfo {
        name: SpriteName::new(name).expect("a name"),
        mode: SpriteMode::truecolour((1, 1), false),
        palette: SpritePalette::Implied,
        masked: false,
    });
    Entry::Picture(picture)
}

fn colour_sprites() -> Window {
    let document = Document::of(
        vec![colour_sprite("one"), colour_sprite("two")],
        Origin::Read(tairix_sandbox::imagerender::ViewFormat::Sprite),
        Unkept::default(),
        SaveSettings::default(),
    )
    .expect("entries");
    Window::new(document)
}

fn sprites() -> Window {
    let document = Document::of(
        vec![sprite("one", 10), sprite("two", 20)],
        Origin::Read(tairix_sandbox::imagerender::ViewFormat::Sprite),
        Unkept::default(),
        SaveSettings::default(),
    )
    .expect("entries");
    Window::new(document)
}

/// A copy of a sprite takes the least numbered name its own leaves free, and
/// a new sprite is offered the stem itself while no sprite has it.
#[test]
fn a_copied_sprite_takes_a_name_of_its_own() {
    let mut window = sprites();
    window.act(Action::DuplicateEntry);
    assert_eq!(window.view.document().current(), 1, "the copy shows");
    window.act(Action::PreviousEntry);
    window.act(Action::DuplicateEntry);
    let names: vec::Vec<String> = window
        .view
        .document()
        .names()
        .map(|name| alloc::format!("{name}"))
        .collect();
    assert_eq!(names, ["one", "one2", "one1", "two"]);
    window.act(Action::NewEntry);
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
        .and_then(|picture| picture.canvas().sample(4, 4));
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
fn a_new_picture_asked_for_starts_from_the_settings_and_is_handed_to_run() {
    let mut window = Window::white(10, 10);
    let mut preferences = crate::preferences::Preferences::default();
    preferences.new.size = (24, 12);
    preferences.format = SaveFormat::Bmp;
    window.view.begin(&preferences);
    window.act(Action::NewPicture);
    assert!(window.view.asking());
    let outcome = window.key(Key::Named(NamedKey::Enter), plain());
    let Some(Request::Own(Own::NewWindow { picture, format })) = outcome.request else {
        panic!("a new window");
    };
    assert_eq!(
        picture,
        NewPicture {
            size: (24, 12),
            ..NewPicture::DEFAULT
        }
    );
    assert_eq!(format, SaveFormat::Bmp);
}

#[test]
fn save_as_asks_how_first_and_then_where_in_the_format_chosen() {
    let mut window = Window::white(10, 10);
    let mut damage = Region::new();
    let surveying = window.view.ask_how(false, &window.layout, &mut damage);
    window.run_worker(surveying.expect("asked"));
    assert!(window.view.asking());
    assert!(!window.view.asking_to_close());
    let outcome = window.key(Key::Named(NamedKey::Enter), plain());
    assert!(matches!(
        outcome.request,
        Some(Request::SaveWhere { then_close: false })
    ));
    assert_eq!(window.view.offered_extension(), "png");
    let surveying = window.view.ask_how(true, &window.layout, &mut damage);
    window.run_worker(surveying.expect("asked"));
    assert!(window.view.asking_to_close(), "a quit waits on the sheet");
    window.key(Key::Named(NamedKey::Escape), plain());
    assert!(!window.view.asking() && !window.view.asking_to_close());
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
    window.act(Action::NextEntry);
    assert_eq!(window.view.document().current(), 1);
    window.move_to(window.screen_of((5, 5)));
    window.release(PointerButton::Primary);
    assert_eq!(
        window.colour(5, 5),
        [255; 4],
        "the drag ended with the stroke"
    );
    window.act(Action::PreviousEntry);
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
    window.act(Action::NextEntry);
    assert_eq!(window.view.document().current(), 0, "the sprite stays");
    assert_eq!(
        window.view.message(),
        Some("Wait: the picture is being worked on")
    );
    window.act(Action::DeleteEntry);
    window.act(Action::DuplicateEntry);
    assert_eq!(window.view.document().entries().len(), 2);
    window.run_worker(outcome);
    assert_eq!(window.colour(7, 7), [0, 0, 0, 255], "filled where asked");
    window.act(Action::NextEntry);
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

/// A tool change repaints the tool box and the bar its settings sit in,
/// never the canvas.
#[test]
fn a_tool_change_repaints_the_tool_box_and_bar_not_the_canvas() {
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
    window.open_style_list();
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
        &window.layout,
        Scale::ONE,
        theme,
        &mut damage,
    );
    assert!(!damage.is_empty(), "the squared preview is drawn");
}

/// An action that needs the floating selection down waits for it: the
/// selection is put down on a worker first, and only once that lands does the
/// action follow. A put-down refused leaves the selection floating and does nothing.
#[test]
fn an_action_waits_for_the_floating_selection_to_be_put_down() {
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
    let popup = window.open_style_list();
    let over = popup.intersection(&window.layout.canvas());
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

/// The dock offers a palette entry's opacity where the palette can hold one —
/// a picture that is not a sprite — and not on a sprite, whose palette is
/// colours alone.
#[test]
fn the_dock_offers_a_palette_entrys_opacity_where_it_can_have_one() {
    let kind = Kind::Indexed {
        depth: IndexDepth::Two,
        palette: vec![[0, 0, 0, 255], [255, 255, 255, 128]],
        masked: false,
    };
    let canvas = Canvas::new(4, 4, kind, Sample::Index(1, 255)).expect("fits");
    let window = Window::new(Document::new(Picture::plain(canvas)));
    assert!(
        window.view.picker.has_opacity(),
        "a plain picture's entry has an opacity"
    );
    let Entry::Picture(picture) = sprite("icon", 9) else {
        panic!("a picture");
    };
    let sprite = Window::new(Document::new(picture));
    assert!(
        !sprite.view.picker.has_opacity(),
        "a sprite's entry is a colour alone"
    );
}

/// A palette picture whose primary ink is entry 0 of four colours.
fn four_colours() -> Window {
    let kind = Kind::Indexed {
        depth: IndexDepth::Two,
        palette: vec![
            [0, 0, 0, 255],
            [255, 255, 255, 255],
            [255, 0, 0, 255],
            [0, 0, 255, 255],
        ],
        masked: true,
    };
    let canvas = Canvas::new(4, 4, kind, Sample::Index(0, 255)).expect("fits");
    let window = Window::new(Document::new(Picture::plain(canvas)));
    assert_eq!(window.view.inks().0, Ink::Index(0));
    window
}

#[test]
fn the_dock_edits_a_colour_pictures_ink_live_and_records_nothing() {
    let mut window = Window::white(16, 16);
    let before = window.view.inks().0;
    window.move_to(window.dock_point((4, 4)));
    window.press(PointerButton::Primary);
    let pressed = window.view.inks().0;
    assert_ne!(pressed, before, "the ink follows the press at once");
    window.move_to(window.dock_point((40, 30)));
    assert_ne!(window.view.inks().0, pressed, "and the drag");
    window.release(PointerButton::Primary);
    assert!(
        !SavedDocument::is_modified(&window.view),
        "an ink is not the picture"
    );
    assert_eq!(window.view.document().history_depth(), 0);
}

#[test]
fn a_palette_entry_dragged_in_the_dock_changes_live_and_is_one_step() {
    let mut window = four_colours();
    window.move_to(window.dock_point((4, 4)));
    window.press(PointerButton::Primary);
    let live = window.palette()[0];
    assert_ne!(live, [0, 0, 0, 255], "the entry takes the colour at once");
    assert_eq!(
        window.view.document().history_depth(),
        0,
        "nothing is recorded mid-drag"
    );
    window.move_to(window.dock_point((30, 30)));
    window.release(PointerButton::Primary);
    let edited = window.palette()[0];
    assert_ne!(edited, live);
    assert_eq!(
        window.view.document().history_depth(),
        1,
        "the drag is one step"
    );
    assert_eq!(
        &window.palette()[1..],
        &[[255; 4], [255, 0, 0, 255], [0, 0, 255, 255]]
    );
    window.act(Action::Undo);
    assert_eq!(
        window.palette()[0],
        [0, 0, 0, 255],
        "undo puts the entry back"
    );
    window.act(Action::Redo);
    assert_eq!(window.palette()[0], edited);
}

#[test]
fn escape_turns_a_dock_drag_down_and_records_nothing() {
    let mut window = four_colours();
    window.move_to(window.dock_point((4, 4)));
    window.press(PointerButton::Primary);
    assert_ne!(window.palette()[0], [0, 0, 0, 255]);
    window.key(Key::Named(NamedKey::Escape), plain());
    assert_eq!(window.palette()[0], [0, 0, 0, 255], "the entry is back");
    window.release(PointerButton::Primary);
    assert_eq!(window.view.document().history_depth(), 0);
    assert!(!SavedDocument::is_modified(&window.view));
}

#[test]
fn the_dock_takes_no_palette_edit_while_a_worker_has_the_picture() {
    let mut window = four_colours();
    window.act(Action::Tool(Tool::Fill));
    window.move_to(window.screen_of((1, 1)));
    let outcome = window.press(PointerButton::Primary);
    assert!(window.view.busy());
    assert!(!window.view.picker.state().enabled, "the dock is withheld");
    window.move_to(window.dock_point((4, 4)));
    window.press(PointerButton::Primary);
    window.release(PointerButton::Primary);
    assert_eq!(window.palette()[0], [0, 0, 0, 255]);
    window.run_worker(outcome);
    assert!(
        window.view.picker.state().enabled,
        "and given back once it lands"
    );
}

#[test]
fn a_clear_ink_on_a_masked_palette_picture_is_the_mask_not_a_colour() {
    let mut window = four_colours();
    let clear = window
        .view
        .wells
        .iter()
        .position(|&ink| ink == Ink::Clear)
        .expect("a mask well");
    let cell = window
        .view
        .swatches
        .cell_rect(window.layout.swatches(), clear)
        .expect("a well");
    window.move_to(cell.center());
    window.press(PointerButton::Primary);
    window.release(PointerButton::Primary);
    assert_eq!(window.view.inks().0, Ink::Clear);
    assert!(
        !window.view.picker.state().enabled,
        "the dock edits colours"
    );
}

#[test]
fn a_press_on_the_secondary_well_makes_the_dock_edit_it() {
    let mut window = Window::white(16, 16);
    let secondary = window.layout.secondary_well();
    let corner = Point::new(secondary.right() - 2, secondary.bottom() - 2);
    window.move_to(corner);
    window.press(PointerButton::Primary);
    window.release(PointerButton::Primary);
    assert_eq!(window.view.dock().1, tairix_controls::SwatchMark::Secondary);
    let white = tairix_colour::Rgba::rgb(255, 255, 255);
    assert_eq!(
        window.view.picker.colour(),
        white,
        "the secondary ink is white"
    );
    window.move_to(window.dock_point((40, 40)));
    window.press(PointerButton::Primary);
    window.release(PointerButton::Primary);
    assert_ne!(
        window.view.inks().1,
        Ink::Colour([255; 4]),
        "the secondary took the edit"
    );
    assert_eq!(
        window.view.inks().0,
        Ink::Colour([0, 0, 0, 255]),
        "the primary did not"
    );
}

#[test]
fn the_palettes_mark_leaves_a_well_the_ink_no_longer_is() {
    let mut window = Window::white(16, 16);
    assert!(
        window.view.swatches.selected().is_some(),
        "black is a desktop colour"
    );
    window.move_to(window.dock_point((30, 30)));
    window.press(PointerButton::Primary);
    window.release(PointerButton::Primary);
    assert_eq!(
        window.view.swatches.selected(),
        None,
        "no well holds the edited ink"
    );
}

#[test]
fn tab_takes_the_keyboard_into_the_dock_and_typing_there_is_not_a_shortcut() {
    let mut window = Window::white(16, 16);
    let tool = window.view.tool();
    window.tab_into_dock();
    assert!(window.view.picker.state().focus.focused);
    for _ in 0..4 {
        window.key(Key::Named(NamedKey::Tab), plain());
    }
    window.key(Key::Char('a'), ctrl());
    for letter in "#ff0".chars() {
        window.key(Key::Char(letter), plain());
    }
    assert_eq!(window.view.tool(), tool, "letters went into the hex field");
    assert_eq!(
        window.view.inks().0,
        Ink::Colour([255, 255, 0, 255]),
        "and were live"
    );
    window.key(Key::Named(NamedKey::Enter), plain());
    window.key(Key::Named(NamedKey::Escape), plain());
    assert!(
        !window.view.picker.state().focus.focused,
        "Escape gives the keyboard back"
    );
    window.key(Key::Char('e'), plain());
    assert_eq!(
        window.view.tool(),
        Tool::Eraser,
        "the shortcut is the window's again"
    );
}

/// A colour typed down to no alpha is the clear ink, and a worker landing
/// meanwhile leaves the picker holding the colour that was typed.
#[test]
fn a_worker_landing_leaves_a_colour_typed_to_no_alpha_as_typed() {
    let mut window = Window::white(16, 16);
    window.act(Action::Tool(Tool::Fill));
    window.move_to(window.screen_of((1, 1)));
    let fill = window.press(PointerButton::Primary);
    window.release(PointerButton::Primary);
    assert!(window.view.busy());
    window.tab_into_dock();
    for _ in 0..4 {
        window.key(Key::Named(NamedKey::Tab), plain());
    }
    window.key(Key::Char('a'), ctrl());
    for letter in "#ff000000".chars() {
        window.key(Key::Char(letter), plain());
    }
    assert_eq!(window.view.inks().0, Ink::Clear);
    let earlier = window.view.picker.earlier();
    window.run_worker(fill);
    assert_eq!(
        window.view.picker.colour(),
        tairix_colour::Rgba::new(255, 0, 0, 0)
    );
    assert_eq!(window.view.picker.earlier(), earlier);
}

/// A drag choosing a colour ink ends where a conversion to a palette lands
/// under it, rather than carry on as an edit of the entry the ink became.
#[test]
fn a_dock_drag_ends_where_a_conversion_turns_its_ink_into_an_entry() {
    let mut window = Window::white(16, 16);
    let form = crate::dialog::Form::convert(Some(IndexDepth::Eight));
    window.view.modal = Some(super::Modal::Form(alloc::boxed::Box::new(form)));
    let convert = window.key(Key::Named(NamedKey::Enter), plain());
    assert!(window.view.busy());
    window.move_to(window.dock_point((4, 4)));
    window.press(PointerButton::Primary);
    assert!(
        window.view.picker.is_dragging(),
        "a colour ink is the dock's meanwhile"
    );
    window.run_worker(convert);
    let converted = window.palette();
    assert_eq!(window.view.document().history_depth(), 1);
    assert!(!window.view.picker.is_dragging());
    window.move_to(window.dock_point((40, 30)));
    window.release(PointerButton::Primary);
    assert_eq!(window.palette(), converted, "no entry took the drag");
    assert_eq!(
        window.view.document().history_depth(),
        1,
        "the conversion alone"
    );
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

/// A sprite whose removal is refused keeps the selection floating over it.
#[test]
fn a_refused_sprite_removal_keeps_its_floating_selection() {
    let mut window = Window::white(40, 40);
    window.lift((4, 4), (8, 8), (10, 10));
    window.act(Action::DeleteEntry);
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

/// Press and release the primary button over `rect`'s centre.
fn click_on(window: &mut Window, rect: Rect) -> Outcome {
    window.move_to(rect.center());
    window.press(PointerButton::Primary);
    window.release(PointerButton::Primary)
}

fn tool_rect(window: &Window, tool: Tool) -> Rect {
    let theme = window.registry.active();
    window
        .view
        .tool_box
        .tool_rect(tool_index(tool), window.layout.tools(), Scale::ONE, theme)
        .expect("the tool is seated")
}

fn command_rect(window: &Window, command: ViewCommand) -> Rect {
    let theme = window.registry.active();
    let index = VIEW_COMMANDS
        .iter()
        .position(|&(_, held, _)| held == command)
        .expect("a command");
    window
        .view
        .commands
        .tool_rect(index, window.layout.view_strip(), Scale::ONE, theme)
        .expect("the command is seated")
}

fn well_rect(window: &Window, index: usize) -> Rect {
    window
        .view
        .swatches
        .cell_rect(window.layout.swatches(), index)
        .expect("a well")
}

/// A press on a tool in the tool box chooses it, and the bar becomes that
/// tool's, placed anew.
#[test]
fn a_tool_chosen_in_the_tool_box_brings_its_own_bar() {
    let mut window = Window::white(40, 40);
    let fill = tool_rect(&window, Tool::Fill);
    let outcome = click_on(&mut window, fill);
    assert_eq!(window.view.tool(), Tool::Fill);
    assert_eq!(outcome.relayout, Relayout::Reported);
    assert!(window.view.tool_box.is_active(tool_index(Tool::Fill)));
    assert_eq!(
        window.view.bar.settings().collect::<Vec<_>>(),
        [Setting::Tolerance, Setting::Contiguous]
    );
    let held = Tool::Fill.settings().len();
    let bar = window.layout.bar();
    assert!(
        bar.control(held - 1).is_some() && bar.control(held).is_none(),
        "placed with the fill's settings"
    );
}

/// The view strip zooms, and marks the pixel grid while it is asked for; the
/// key and the strip agree.
#[test]
fn the_view_strip_zooms_and_marks_the_grid() {
    let mut window = Window::white(40, 40);
    let zoom = window.view.viewport().zoom();
    let zoom_in = command_rect(&window, ViewCommand::ZoomIn);
    click_on(&mut window, zoom_in);
    assert!(window.view.viewport().zoom() > zoom);
    let index = VIEW_COMMANDS.len() - 1;
    assert!(
        window.view.grids.pixels && window.view.commands.is_active(index),
        "asked for at first"
    );
    let grid = command_rect(&window, ViewCommand::PixelGrid);
    click_on(&mut window, grid);
    assert!(!window.view.grids.pixels);
    assert!(!window.view.commands.is_active(index));
    window.key(Key::Char('g'), plain());
    assert!(window.view.grids.pixels);
    assert!(
        window.view.commands.is_active(index),
        "the grid's command is marked"
    );
}

/// A press held on one strip and let go over another chooses nothing on
/// either, and leaves neither holding the press.
#[test]
fn a_press_carried_from_one_strip_to_another_chooses_nothing() {
    let mut window = Window::white(40, 40);
    let zoom = window.view.viewport().zoom();
    let zoom_in = command_rect(&window, ViewCommand::ZoomIn);
    let fill = tool_rect(&window, Tool::Fill);
    window.move_to(zoom_in.center());
    window.press(PointerButton::Primary);
    window.move_to(fill.center());
    window.release(PointerButton::Primary);
    assert_eq!(window.view.viewport().zoom(), zoom);
    assert_eq!(window.view.tool(), Tool::Brush);
    click_on(&mut window, zoom_in);
    assert!(
        window.view.viewport().zoom() > zoom,
        "the strip let the carried press go"
    );
}

/// A size typed into the bar is the width the next stroke is painted at.
#[test]
fn a_size_typed_in_the_bar_is_the_width_the_brush_paints() {
    let mut window = Window::white(60, 60);
    window.click_setting(0);
    assert!(window.view.bar.focus().is_some());
    window.key(Key::Char('a'), ctrl());
    window.key(Key::Char('9'), plain());
    assert_eq!(window.view.options.brush.size, 9, "live as it is typed");
    window.drag((30, 30), (30, 30));
    assert!(
        window.view.bar.focus().is_none(),
        "the press took the keyboard back"
    );
    assert_eq!(
        window.colour(33, 30),
        [0, 0, 0, 255],
        "inside a nine-pixel dab"
    );
    assert_eq!(window.colour(36, 30), [255; 4], "and no further");
}

/// Tab walks the picture, the bar's settings, the palette and the dock and
/// back to the picture, and Shift+Tab the other way.
#[test]
fn tab_walks_the_bar_the_palette_and_the_dock_and_back() {
    let mut window = Window::white(16, 16);
    window.act(Action::Tool(Tool::Line));
    let tab = |window: &mut Window, modifiers| {
        window.key(Key::Named(NamedKey::Tab), modifiers);
    };
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    tab(&mut window, plain());
    assert_eq!(window.view.bar.focus(), Some(0), "the size first");
    tab(&mut window, plain());
    assert_eq!(window.view.bar.focus(), Some(1));
    tab(&mut window, plain());
    assert!(
        window.view.swatches.state().focus.focused,
        "then the palette"
    );
    assert!(window.view.bar.focus().is_none());
    tab(&mut window, plain());
    assert_eq!(
        window.view.colour_controls.focus(),
        Some((0, 0)),
        "then the colour pane's buttons"
    );
    assert!(!window.view.swatches.state().focus.focused);
    for _ in 0..4 {
        tab(&mut window, plain());
    }
    assert_eq!(
        window.view.colour_controls.focus(),
        Some((2, 0)),
        "and its choices"
    );
    tab(&mut window, plain());
    assert!(window.view.picker.state().focus.focused, "then the dock");
    assert_eq!(window.view.colour_controls.focus(), None);
    for _ in 0..20 {
        if !window.view.picker.state().focus.focused {
            break;
        }
        tab(&mut window, plain());
    }
    assert!(
        !window.view.picker.state().focus.focused,
        "and back to the picture"
    );
    assert!(window.view.bar.focus().is_none() && !window.view.swatches.state().focus.focused);

    tab(&mut window, shift);
    assert!(
        window.view.picker.state().focus.focused,
        "backward into the dock"
    );
    for _ in 0..20 {
        if !window.view.picker.state().focus.focused {
            break;
        }
        tab(&mut window, shift);
    }
    assert_eq!(
        window.view.colour_controls.focus(),
        Some((2, 0)),
        "the colour pane's choices before it"
    );
    for _ in 0..5 {
        tab(&mut window, shift);
    }
    assert!(
        window.view.swatches.state().focus.focused,
        "the palette before them"
    );
    tab(&mut window, shift);
    assert_eq!(
        window.view.bar.focus(),
        Some(1),
        "the bar from its last setting"
    );
    tab(&mut window, shift);
    tab(&mut window, shift);
    assert!(
        window.view.bar.focus().is_none(),
        "and the picture before that"
    );
}

/// A tool with no settings leaves the bar out of the walk.
#[test]
fn tab_passes_a_bar_with_no_settings() {
    let mut window = Window::white(16, 16);
    window.act(Action::Tool(Tool::Pencil));
    window.key(Key::Named(NamedKey::Tab), plain());
    assert!(window.view.swatches.state().focus.focused);
}

/// A letter typed into a number field is typing, never a tool's key, and
/// Escape gives the keyboard back to the picture.
#[test]
fn typing_in_the_bar_is_not_a_shortcut_and_escape_gives_the_keyboard_back() {
    let mut window = Window::white(16, 16);
    window.click_setting(0);
    window.key(Key::Char('e'), plain());
    assert_eq!(window.view.tool(), Tool::Brush);
    window.key(Key::Named(NamedKey::Escape), plain());
    assert!(window.view.bar.focus().is_none());
    window.key(Key::Char('e'), plain());
    assert_eq!(window.view.tool(), Tool::Eraser, "the window's key again");
}

/// A chord from a field is the window's, and what the field was typed
/// lands before the window acts.
#[test]
fn a_chord_from_the_bar_settles_the_field_first() {
    let mut window = Window::white(16, 16);
    window.click_setting(0);
    window.key(Key::Char('a'), ctrl());
    window.key(Key::Char('9'), plain());
    window.key(Key::Char('9'), plain());
    assert_eq!(
        window.view.options.brush.size, 9,
        "99 is past a brush's widest"
    );
    let outcome = window.key(Key::Char('s'), ctrl());
    assert!(matches!(outcome.request, Some(Request::Save)));
    assert_eq!(
        window.view.options.brush.size, MAX_SIZE,
        "settled to the bound first"
    );
}

/// A press anywhere else in the window settles the field and takes its
/// keyboard; the menu does too.
#[test]
fn a_press_elsewhere_or_the_menu_takes_the_bars_keyboard() {
    let mut window = Window::white(16, 16);
    window.click_setting(0);
    window.key(Key::Char('a'), ctrl());
    window.key(Key::Char('7'), plain());
    window.key(Key::Char('7'), plain());
    let status = window.layout.message();
    click_on(&mut window, status);
    assert!(window.view.bar.focus().is_none());
    assert_eq!(window.view.options.brush.size, MAX_SIZE);

    window.click_setting(0);
    window.move_to(window.screen_of((4, 4)));
    let outcome = window.press(PointerButton::Secondary);
    assert!(matches!(
        outcome.request,
        Some(Request::Menu {
            kind: MenuKind::Window,
            ..
        })
    ));
    assert!(window.view.bar.focus().is_none());
}

/// A well of the palette strip pressed is the primary ink, and one pressed
/// with the middle button the secondary.
#[test]
fn the_palette_strip_sets_both_inks() {
    let mut window = Window::white(16, 16);
    let red = well_rect(&window, 11);
    click_on(&mut window, red);
    assert_eq!(window.view.inks().0, window.view.wells[11]);
    let blue = well_rect(&window, 8);
    window.move_to(blue.center());
    window.press(PointerButton::Middle);
    window.release(PointerButton::Middle);
    assert_eq!(window.view.inks().1, window.view.wells[8]);
    assert_eq!(window.view.swatches.secondary(), Some(8));
}

/// The arrows walk the palette while it has the keyboard; on the picture,
/// with nothing floating, they move no ink.
#[test]
fn the_arrows_walk_the_palette_only_while_it_has_the_keyboard() {
    let mut window = Window::white(16, 16);
    let inks = window.view.inks();
    window.key(Key::Named(NamedKey::Right), plain());
    assert_eq!(window.view.inks(), inks);
    let black = window
        .view
        .swatches
        .selected()
        .expect("black is a Wimp colour");
    window.act(Action::Tool(Tool::Pencil));
    window.key(Key::Named(NamedKey::Tab), plain());
    assert!(window.view.swatches.state().focus.focused);
    window.key(Key::Named(NamedKey::Right), plain());
    assert_eq!(window.view.inks().0, window.view.wells[black + 1]);
    window.key(Key::Char('b'), plain());
    assert_eq!(
        window.view.tool(),
        Tool::Brush,
        "a key the palette has no use for is the window's"
    );
}

/// The palette's rows follow the window it is laid out in.
#[test]
fn the_palettes_rows_follow_the_layout() {
    let kind = Kind::Indexed {
        depth: IndexDepth::Eight,
        palette: tairix_image::desktop_palette(IndexDepth::Eight)
            .iter()
            .map(|&[r, g, b]| [r, g, b, 255])
            .collect(),
        masked: false,
    };
    let canvas = Canvas::new(8, 8, kind, Sample::Index(0, 255)).expect("fits");
    let window = Window::new(Document::new(Picture::plain(canvas)));
    assert_eq!(window.view.wells.len(), 256);
    assert_eq!(window.view.swatches.columns(), window.layout.columns());
    assert!(window.view.swatches.rows() > 1);
    let last = window
        .view
        .swatches
        .cell_rect(window.layout.swatches(), 255)
        .expect("the last entry has a well");
    assert_eq!(
        last.intersection(&window.layout.palette()),
        last,
        "inside the strip"
    );
}

/// The tool box, the view strip and the bar each carry their tips.
#[test]
fn tips_name_tools_commands_and_settings() {
    let mut window = Window::white(16, 16);
    let tip = |window: &Window| {
        let theme = window.registry.active();
        window
            .view
            .tool_tip(&window.layout, Scale::ONE, theme)
            .map(|(_, text)| text)
    };
    window.move_to(tool_rect(&window, Tool::Fill).center());
    assert_eq!(tip(&window), Some("Fill (F)"));
    window.move_to(command_rect(&window, ViewCommand::PixelGrid).center());
    assert_eq!(tip(&window), Some("Pixel grid (G)"));
    let size = window.layout.bar().control(0).expect("seated");
    window.move_to(size.center());
    assert_eq!(tip(&window), Some(Setting::Size.tip()));
}

/// The pointer shows text entry over a number field and the arrow while a
/// list holds it.
#[test]
fn the_pointer_shows_text_entry_over_a_number_field() {
    let mut window = Window::white(16, 16);
    let size = window.layout.bar().control(0).expect("seated").center();
    let canvas = window.screen_of((4, 4));
    let cursor = |window: &Window, at| DocumentView::cursor(&window.view, &window.layout, at);
    assert_eq!(cursor(&window, size), CursorShape::Text);
    assert_eq!(cursor(&window, canvas), CursorShape::Crosshair);
    window.act(Action::Tool(Tool::Rectangle));
    window.open_style_list();
    assert_eq!(cursor(&window, canvas), CursorShape::Arrow);
}

/// A palette picture holds smoothing off in the bar, and its tip says why.
#[test]
fn a_palette_picture_holds_smoothing_off_in_the_bar() {
    let mut window = four_colours();
    let smooth = window.layout.bar().control(1).expect("seated");
    window.move_to(smooth.center());
    let theme = window.registry.active();
    let tip = window.view.tool_tip(&window.layout, Scale::ONE, theme);
    assert_eq!(tip.map(|(_, text)| text), Some(WHOLE_PIXELS));
    click_on(&mut window, smooth);
    assert!(window.view.options.smooth, "the switch flips nothing");
}

/// The least window seats every tool's bar whole beside the view strip.
#[test]
fn the_least_window_seats_every_tools_bar() {
    let mut window = Window::white(16, 16);
    let theme = window.registry.active();
    let (width, height) = window.view.min_size(theme, Scale::ONE, faces(theme));
    let least = ToolControls::least_width(faces(theme), Scale::ONE, theme);
    assert!(width >= least + window.view.commands.natural_length(Scale::ONE, theme));
    for tool in Tool::ALL {
        window.act(Action::Tool(tool));
        let theme = window.registry.active();
        let least = window
            .view
            .layout(width, height, theme, Scale::ONE, faces(theme));
        for index in 0..tool.settings().len() {
            assert!(
                least.bar().control(index).is_some(),
                "{tool:?}'s setting {index} is seated"
            );
        }
        assert!(!least.canvas().is_empty());
    }
}

/// Choosing a tool moves the tool box's mark and nothing else: a tool box
/// scrolled down its column stays where it was.
#[test]
fn choosing_a_tool_keeps_where_a_short_tool_box_is_scrolled() {
    let mut window = Window::white(16, 16);
    let theme = window.registry.active();
    window.layout = window
        .view
        .layout(900, 300, theme, Scale::ONE, faces(theme));
    window.view.settle(&window.layout, &mut Region::new());
    let tools = window.layout.tools();
    let offset = |window: &Window| {
        let theme = window.registry.active();
        window
            .view
            .tool_box
            .scroll_model(window.layout.tools(), Scale::ONE, theme)
            .offset()
    };
    assert!(
        window
            .view
            .tool_box
            .scroll_model(tools, Scale::ONE, theme)
            .range()
            .is_scrollable(),
        "a short window's tool box scrolls"
    );
    window.move_to(tools.center());
    window.pointer(InputEvent::PointerScrolled {
        dx: 0,
        dy: SCROLL_UNITS_PER_DETENT,
    });
    let scrolled = offset(&window);
    assert!(scrolled > 0);
    let theme = window.registry.active();
    let stroke = Keystroke {
        key: Key::Char('e'),
        modifiers: plain(),
        at_ns: window.now,
    };
    let outcome = window.view.on_key(
        stroke,
        &window.layout,
        Scale::ONE,
        theme,
        &mut Region::new(),
    );
    assert_eq!(outcome.relayout, Relayout::Reported);
    window.layout = window
        .view
        .layout(900, 300, theme, Scale::ONE, faces(theme));
    window.view.settle(&window.layout, &mut Region::new());
    assert_eq!(window.view.tool(), Tool::Eraser);
    assert_eq!(offset(&window), scrolled, "the column did not jump back");
}

/// On a palette picture, Tab passes what the bar holds off: a tip's
/// hardness, opacity and flow, and smoothing.
#[test]
fn tab_passes_what_a_palette_picture_holds_off() {
    let mut window = four_colours();
    window.key(Key::Named(NamedKey::Tab), plain());
    assert_eq!(window.view.bar.focus(), Some(0), "the size");
    window.key(Key::Named(NamedKey::Tab), plain());
    assert_eq!(window.view.bar.focus(), Some(4), "the spacing next");
    window.key(Key::Named(NamedKey::Tab), plain());
    assert!(
        window.view.swatches.state().focus.focused,
        "past smoothing, straight to the palette"
    );
}

/// A TIFF's pages take a new page, unnamed, through the form a page asks;
/// the document stays pages and its menu says so.
#[test]
fn a_new_page_joins_a_tiffs_pages_unnamed() {
    let page = || {
        Entry::Picture(Picture::plain(
            Canvas::new(6, 4, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits"),
        ))
    };
    let document = Document::of(
        vec![page()],
        Origin::Read(tairix_sandbox::imagerender::ViewFormat::Tiff),
        Unkept::default(),
        SaveSettings::default(),
    )
    .expect("entries");
    let mut window = Window::new(document);
    assert!(window.view.document().is_pages());
    window.act(Action::NewEntry);
    assert!(window.view.asking());
    window.key(Key::Named(NamedKey::Enter), plain());
    let document = window.view.document();
    assert_eq!(document.entries().len(), 2);
    assert_eq!(document.current(), 1, "the new page shows");
    assert!(document
        .entries()
        .iter()
        .all(|entry| entry.name().is_none()));
    assert!(document.is_pages() && !document.is_sprite_area());
    let picture = document.picture().expect("a picture");
    assert_eq!(
        (picture.canvas().width(), picture.canvas().height()),
        (6, 4),
        "the size it was offered"
    );
}

/// Take up the select tool marking out `marquee`.
fn marking_with(window: &mut Window, marquee: Marquee) {
    window.act(Action::Tool(Tool::Select));
    window.view.options.marquee = marquee;
}

/// Hold `modifiers` down.
fn hold(window: &mut Window, modifiers: Modifiers) {
    window.pointer(InputEvent::ModifiersChanged { modifiers });
}

/// Click picture pixel `at` with the primary button, answering what asked
/// for more: the press or the release.
fn click(window: &mut Window, at: (u32, u32)) -> Outcome {
    let point = window.screen_of(at);
    window.move_to(point);
    let pressed = window.press(PointerButton::Primary);
    let released = window.release(PointerButton::Primary);
    if pressed.request.is_some() {
        pressed
    } else {
        released
    }
}

#[test]
fn an_ellipse_selection_is_made_on_a_worker_and_chooses_its_inside() {
    let mut window = Window::white(40, 30);
    marking_with(&mut window, Marquee::Ellipse);
    let outcome = window.drag((5, 5), (24, 14));
    assert!(window.view.busy(), "traced off the loop");
    window.run_worker(outcome);
    let chosen = window.view.selection().expect("selected");
    assert!(!chosen.is_rect());
    assert!(chosen.chooses(15, 10));
    assert!(!chosen.chooses(5, 5), "the corner the ellipse leaves out");
}

#[test]
fn a_plain_rectangle_is_selected_at_once_and_a_click_selects_nothing() {
    let mut window = Window::white(40, 30);
    marking_with(&mut window, Marquee::Rectangle);
    let outcome = window.drag((5, 5), (9, 8));
    assert!(
        outcome.request.is_none() && !window.view.busy(),
        "no worker"
    );
    let chosen = window.view.selection().expect("selected");
    assert!(chosen.is_rect());
    assert_eq!(
        chosen.bounds(),
        Bounds {
            x0: 5,
            y0: 5,
            x1: 10,
            y1: 9
        }
    );
    click(&mut window, (30, 20));
    assert!(
        window.view.selection().is_none(),
        "a click outside deselects"
    );
}

#[test]
fn a_lasso_selects_what_its_path_encloses() {
    let mut window = Window::white(40, 40);
    marking_with(&mut window, Marquee::Lasso);
    window.move_to(window.screen_of((4, 4)));
    window.press(PointerButton::Primary);
    for corner in [(30, 4), (30, 30), (4, 30)] {
        window.move_to(window.screen_of(corner));
        assert!(matches!(
            window.view.marking(),
            Some(Marking::Path { to: None, .. })
        ));
    }
    let outcome = window.release(PointerButton::Primary);
    window.run_worker(outcome);
    let chosen = window.view.selection().expect("selected");
    assert!(chosen.chooses(15, 15));
    assert!(!chosen.chooses(35, 35));
    assert_eq!(window.view.marking(), None);
}

#[test]
fn a_polygon_is_marked_a_corner_at_a_time_and_closed_on_its_first() {
    let mut window = Window::white(40, 40);
    marking_with(&mut window, Marquee::Polygon);
    for corner in [(2, 2), (20, 2), (20, 20), (2, 20)] {
        assert!(click(&mut window, corner).request.is_none());
    }
    window.move_to(window.screen_of((10, 30)));
    let Some(Marking::Path { points, to }) = window.view.marking() else {
        panic!("a polygon being marked out");
    };
    assert_eq!(points.len(), 4);
    assert_eq!(to.map(Fx::pixel), Some((10, 30)), "the next edge follows");
    let outcome = click(&mut window, (2, 2));
    window.run_worker(outcome);
    let chosen = window.view.selection().expect("closed and selected");
    assert!(chosen.chooses(10, 10));
    assert!(!chosen.chooses(25, 25));
    assert_eq!(window.view.marking(), None);
}

#[test]
fn enter_closes_a_polygon_backspace_takes_a_corner_back_and_escape_turns_it_down() {
    let mut window = Window::white(40, 40);
    marking_with(&mut window, Marquee::Polygon);
    for corner in [(2, 2), (20, 2), (20, 20), (30, 30)] {
        click(&mut window, corner);
    }
    window.key(Key::Named(NamedKey::Backspace), plain());
    let Some(Marking::Path { points, .. }) = window.view.marking() else {
        panic!("still being marked out");
    };
    assert_eq!(points.len(), 3, "the last corner is taken back");
    let outcome = window.key(Key::Named(NamedKey::Enter), plain());
    window.run_worker(outcome);
    let chosen = window.view.selection().expect("closed");
    assert!(chosen.chooses(15, 8) && !chosen.chooses(28, 28));
    click(&mut window, (35, 35));
    click(&mut window, (38, 35));
    window.key(Key::Named(NamedKey::Escape), plain());
    assert_eq!(window.view.marking(), None, "turned down");
    assert!(
        window.view.selection().is_none(),
        "the first corner let the old one go"
    );
}

#[test]
fn the_wand_chooses_the_pixels_joined_through_like_colours() {
    let mut window = Window::white(40, 30);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((20, 0), (20, 29));
    marking_with(&mut window, Marquee::Wand);
    let outcome = click(&mut window, (3, 3));
    window.run_worker(outcome);
    let chosen = window.view.selection().expect("selected");
    assert!(chosen.chooses(19, 29) && chosen.chooses(0, 0));
    assert!(!chosen.chooses(20, 5), "the line stops the flood");
    assert!(!chosen.chooses(30, 5));
}

#[test]
fn shift_adds_alt_takes_away_and_both_keep_what_both_choose() {
    let mut window = Window::white(40, 40);
    marking_with(&mut window, Marquee::Rectangle);
    window.drag((2, 2), (11, 11));
    let modified = |shift, alt| Modifiers {
        shift,
        alt,
        ..Modifiers::default()
    };
    hold(&mut window, modified(true, false));
    let outcome = window.drag((8, 8), (17, 17));
    window.run_worker(outcome);
    let added = window.view.selection().expect("added").clone();
    assert!(added.chooses(3, 3) && added.chooses(16, 16) && !added.chooses(16, 3));
    hold(&mut window, modified(false, true));
    let outcome = window.drag((0, 0), (5, 5));
    window.run_worker(outcome);
    let taken = window.view.selection().expect("taken from").clone();
    assert!(!taken.chooses(3, 3) && taken.chooses(9, 9));
    hold(&mut window, modified(true, true));
    let outcome = window.drag((9, 9), (30, 30));
    window.run_worker(outcome);
    let both = window.view.selection().expect("kept").clone();
    assert!(both.chooses(12, 12) && !both.chooses(7, 7) && !both.chooses(25, 25));
    hold(&mut window, plain());
}

#[test]
fn alt_with_the_select_tool_takes_away_rather_than_taking_a_colour() {
    let mut window = Window::white(20, 20);
    marking_with(&mut window, Marquee::Rectangle);
    window.drag((2, 2), (10, 10));
    hold(
        &mut window,
        Modifiers {
            alt: true,
            ..Modifiers::default()
        },
    );
    let outcome = window.drag((2, 2), (5, 5));
    window.run_worker(outcome);
    assert_eq!(
        window.view.inks().0,
        Ink::Colour([0, 0, 0, 255]),
        "no colour was taken"
    );
    assert!(!window.view.selection().expect("held").chooses(3, 3));
}

#[test]
fn a_feathered_selection_is_soft_at_its_edge() {
    let mut window = Window::white(40, 40);
    marking_with(&mut window, Marquee::Rectangle);
    window.view.options.feather = 3;
    let outcome = window.drag((10, 10), (29, 29));
    window.run_worker(outcome);
    let chosen = window.view.selection().expect("selected");
    assert!(!chosen.is_rect());
    assert_eq!(chosen.at(20, 20), 255);
    assert!(
        (1..255).contains(&chosen.at(10, 20)),
        "{}",
        chosen.at(10, 20)
    );
}

#[test]
fn painting_and_filling_are_held_to_the_selection_across_tools() {
    let mut window = Window::white(30, 30);
    marking_with(&mut window, Marquee::Rectangle);
    window.drag((5, 5), (10, 10));
    window.act(Action::Tool(Tool::Pencil));
    assert!(window.view.selection().is_some(), "the selection stays");
    window.drag((0, 7), (20, 7));
    assert_eq!(window.colour(5, 7), [0, 0, 0, 255]);
    assert_eq!(window.colour(10, 7), [0, 0, 0, 255]);
    assert_eq!(window.colour(4, 7), [255; 4], "outside the selection");
    assert_eq!(window.colour(11, 7), [255; 4]);
    window.act(Action::Tool(Tool::Fill));
    let outcome = click(&mut window, (20, 20));
    window.run_worker(outcome);
    assert_eq!(
        window.colour(20, 20),
        [255; 4],
        "outside, nothing is filled"
    );
    assert_eq!(window.colour(6, 6), [0, 0, 0, 255], "inside it is");
}

#[test]
fn select_all_selects_the_picture_and_keeps_the_tool() {
    let mut window = Window::white(30, 20);
    window.act(Action::Tool(Tool::Brush));
    window.act(Action::SelectAll);
    assert_eq!(window.view.tool(), Tool::Brush);
    assert_eq!(
        window.view.selection().map(Mask::bounds),
        Some(Bounds::picture(30, 20))
    );
}

#[test]
fn deleting_a_soft_selection_erases_in_proportion() {
    let mut window = Window::white(40, 40);
    marking_with(&mut window, Marquee::Ellipse);
    let outcome = window.drag((4, 4), (35, 35));
    window.run_worker(outcome);
    let edge = (4..20)
        .find(|&x| (1..255).contains(&window.view.selection().expect("held").at(x, 20)))
        .expect("a soft edge");
    let outcome = window.act(Action::Delete);
    window.run_worker(outcome);
    assert_eq!(window.colour(20, 20), [0; 4]);
    assert_eq!(window.colour(4, 4), [255; 4], "left out");
    let [.., alpha] = window.colour(u32::try_from(edge).expect("on it"), 20);
    assert!(alpha > 0 && alpha < 255, "{alpha}");
}

#[test]
fn the_hand_drags_the_view_and_escape_puts_it_back() {
    let mut window = Window::white(2000, 2000);
    window.act(Action::Tool(Tool::Hand));
    let start = window.view.viewport().scroll();
    let at = window.layout.canvas().center();
    window.move_to(at);
    window.press(PointerButton::Primary);
    window.move_to(Point::new(at.x - 50, at.y - 30));
    assert_eq!(
        window.view.viewport().scroll(),
        (start.0 + 50, start.1 + 30)
    );
    window.key(Key::Named(NamedKey::Escape), plain());
    assert_eq!(window.view.viewport().scroll(), start, "turned down");
    window.release(PointerButton::Primary);
    assert_eq!(
        window.view.document().history_depth(),
        0,
        "nothing was painted"
    );
}

#[test]
fn space_held_drags_the_view_whatever_the_tool() {
    let mut window = Window::white(2000, 2000);
    window.act(Action::Tool(Tool::Pencil));
    window.key(Key::Char(' '), plain());
    let start = window.view.viewport().scroll();
    let at = window.layout.canvas().center();
    window.move_to(at);
    window.press(PointerButton::Primary);
    window.move_to(Point::new(at.x - 40, at.y));
    window.release(PointerButton::Primary);
    assert_eq!(window.view.viewport().scroll().0, start.0 + 40);
    assert_eq!(
        window.view.document().history_depth(),
        0,
        "the pencil drew nothing"
    );
    let theme = window.registry.active();
    DocumentView::input(
        &mut window.view,
        &InputEvent::KeyReleased {
            key: Key::Char(' '),
            modifiers: plain(),
        },
        window.now,
        &window.layout,
        Scale::ONE,
        theme,
        &mut Region::new(),
    );
    window.drag((100, 100), (103, 100));
    assert_eq!(
        window.view.document().history_depth(),
        1,
        "let go, the pencil draws"
    );
}

#[test]
fn the_zoom_tool_steps_by_a_click_and_frames_a_dragged_box() {
    let mut window = Window::white(400, 300);
    window.act(Action::Tool(Tool::Zoom));
    let zoom = window.view.viewport().zoom();
    click(&mut window, (100, 100));
    assert!(window.view.viewport().zoom() > zoom, "a click magnifies");
    hold(
        &mut window,
        Modifiers {
            alt: true,
            ..Modifiers::default()
        },
    );
    click(&mut window, (100, 100));
    assert_eq!(window.view.viewport().zoom(), zoom, "and with Alt, reduces");
    hold(&mut window, plain());
    window.drag((10, 10), (29, 19));
    let area = window.layout.canvas();
    let shown = window.view.viewport().to_screen(
        Bounds {
            x0: 10,
            y0: 10,
            x1: 30,
            y1: 20,
        },
        (400, 300),
        area,
    );
    assert!(
        shown.width + 4 >= area.width || shown.height + 4 >= area.height,
        "the box fills the canvas: {shown:?} in {area:?}"
    );
}

/// Dragging the crop box repaints where the box was and is, with the
/// handles on their edges: what lies outside both is veiled either way.
#[test]
fn dragging_the_crop_box_repaints_only_where_it_was_and_is() {
    let mut window = Window::white(60, 40);
    window.act(Action::Tool(Tool::Crop));
    window.drag((10, 5), (29, 24));
    let size = window.size();
    let canvas = window.layout.canvas();
    let held = window.view.crop_box().expect("a box");
    let span = window.view.viewport().screen_span(held, size, canvas);
    let grab = Point::new(
        i32::try_from(span.x1 - 1).expect("on screen"),
        window.screen_of((20, 15)).y,
    );
    window.move_to(grab);
    window.press(PointerButton::Primary);
    let frame = |window: &Window| {
        let theme = window.registry.active();
        let mut surface = tairix_raster::Surface::new(WINDOW.0, WINDOW.1).expect("a surface");
        crate::render::render_into(
            &mut surface,
            &window.view,
            &window.layout,
            theme,
            Scale::ONE,
            faces(theme),
            &mut tairix_icon::NoArtwork,
        );
        surface
    };
    let before = frame(&window);
    let mut damage = Region::new();
    let theme = window.registry.active();
    let to = window.screen_of((33, 15));
    window.view.on_pointer(
        &InputEvent::PointerMoved { to },
        &window.layout,
        Scale::ONE,
        theme,
        &mut damage,
    );
    assert_eq!(window.view.crop_box().map(|b| b.x1), Some(34), "widened");
    let after = frame(&window);
    let mut changed = 0;
    for y in 0..WINDOW.1 {
        for x in 0..WINDOW.0 {
            if before.get(x, y) != after.get(x, y) {
                changed += 1;
                let at = Point::new(i32::try_from(x).expect("on"), i32::try_from(y).expect("on"));
                assert!(damage.contains(at), "({x}, {y}) changed outside the damage");
            }
        }
    }
    assert!(changed > 0, "the box was drawn anew");
    for (x, y) in [(55, 35), (2, 35), (55, 1)] {
        assert!(
            !damage.contains(window.screen_of((x, y))),
            "({x}, {y}) veiled either way"
        );
    }
    window.release(PointerButton::Primary);
}

#[test]
fn the_crop_box_is_set_out_adjusted_and_applied_with_enter() {
    let mut window = Window::white(60, 40);
    window.act(Action::Tool(Tool::Crop));
    window.drag((10, 5), (29, 24));
    assert_eq!(
        window.view.crop_box(),
        Some(Bounds {
            x0: 10,
            y0: 5,
            x1: 30,
            y1: 25
        })
    );
    // The right edge, dragged out.
    let edge = window.layout.canvas();
    let size = window.size();
    let right =
        window
            .view
            .viewport()
            .screen_span(window.view.crop_box().expect("a box"), size, edge);
    let grab = Point::new(
        i32::try_from(right.x1 - 1).expect("on screen"),
        window.screen_of((20, 15)).y,
    );
    window.move_to(grab);
    window.press(PointerButton::Primary);
    window.move_to(window.screen_of((39, 15)));
    window.release(PointerButton::Primary);
    assert_eq!(window.view.crop_box().map(|b| b.x1), Some(40), "widened");
    let outcome = window.key(Key::Named(NamedKey::Enter), plain());
    window.run_worker(outcome);
    assert_eq!(window.size(), (30, 20), "cut down to the box");
    assert_eq!(window.view.crop_box(), None);
    window.drag((2, 2), (6, 6));
    window.key(Key::Named(NamedKey::Escape), plain());
    assert_eq!(window.view.crop_box(), None, "Escape lets it go");
    window.drag((2, 2), (6, 6));
    window.act(Action::Tool(Tool::Brush));
    assert_eq!(window.view.crop_box(), None, "and so does another tool");
}

#[test]
fn a_fill_not_held_to_joined_pixels_reaches_every_like_pixel() {
    let mut window = Window::white(30, 10);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((10, 0), (10, 9));
    window.act(Action::Tool(Tool::Fill));
    window.view.options.contiguous = false;
    window.act(Action::SwapColours);
    window.act(Action::SwapColours);
    window.view.secondary = Ink::Colour([255; 4]);
    window.view.primary = Ink::Colour([0, 0, 255, 255]);
    let outcome = click(&mut window, (2, 2));
    window.run_worker(outcome);
    assert_eq!(window.colour(2, 2), [0, 0, 255, 255]);
    assert_eq!(
        window.colour(25, 5),
        [0, 0, 255, 255],
        "beyond the wall too"
    );
    assert_eq!(
        window.colour(10, 5),
        [0, 0, 0, 255],
        "the wall is not like it"
    );
}

#[test]
fn alt_backspace_fills_the_selection_with_the_primary_colour() {
    let mut window = Window::white(20, 20);
    marking_with(&mut window, Marquee::Rectangle);
    window.drag((5, 5), (9, 9));
    let outcome = window.key(
        Key::Named(NamedKey::Backspace),
        Modifiers {
            alt: true,
            ..Modifiers::default()
        },
    );
    window.run_worker(outcome);
    assert_eq!(window.colour(7, 7), [0, 0, 0, 255]);
    assert_eq!(window.colour(3, 3), [255; 4], "outside the selection");
}

#[test]
fn a_gradient_is_previewed_while_dragged_and_laid_by_a_worker() {
    let mut window = Window::white(40, 10);
    window.act(Action::Tool(Tool::Gradient));
    window.move_to(window.screen_of((0, 5)));
    window.press(PointerButton::Primary);
    window.move_to(window.screen_of((39, 5)));
    assert!(window.view.gradient().is_some(), "previewed");
    assert_eq!(window.colour(20, 5), [255; 4], "nothing laid yet");
    let outcome = window.release(PointerButton::Primary);
    window.run_worker(outcome);
    assert_eq!(window.colour(0, 5), [0, 0, 0, 255]);
    assert_eq!(window.colour(39, 5), [255; 4]);
    let [mid, ..] = window.colour(20, 5);
    assert!(mid > 100 && mid < 160, "half way: {mid}");
    let outcome = click(&mut window, (5, 5));
    assert!(outcome.request.is_none(), "a click lays nothing");
}

#[test]
fn the_clone_tool_copies_from_where_alt_clicked_at_a_fixed_distance() {
    let mut window = Window::white(40, 20);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((5, 5), (5, 5));
    window.act(Action::Tool(Tool::Clone));
    window.view.options.clone.size = 3;
    window.view.options.clone.hardness = 100;
    click(&mut window, (25, 5));
    assert!(window
        .view
        .message()
        .is_some_and(|said| said.contains("Alt-click")));
    assert_eq!(window.colour(25, 5), [255; 4], "nothing to copy from yet");
    hold(
        &mut window,
        Modifiers {
            alt: true,
            ..Modifiers::default()
        },
    );
    click(&mut window, (5, 5));
    hold(&mut window, plain());
    click(&mut window, (25, 5));
    assert_eq!(
        window.colour(25, 5),
        [0, 0, 0, 255],
        "copied from twenty to the left"
    );
    click(&mut window, (26, 5));
    assert_eq!(
        window.colour(26, 5),
        [255; 4],
        "the same distance holds: white lies there"
    );
}

#[test]
fn text_is_typed_where_clicked_and_set_down_as_one_step() {
    let mut window = Window::white(200, 80);
    window.act(Action::Tool(Tool::Text));
    window.view.options.text_size = 20;
    click(&mut window, (10, 10));
    for ch in "Hi".chars() {
        window.key(Key::Char(ch), plain());
    }
    let entry = window.view.text().expect("being typed");
    assert_eq!(entry.text(), "Hi");
    let bounds = entry.bounds();
    assert!(!bounds.is_empty());
    assert_eq!(
        window.view.document().history_depth(),
        0,
        "nothing set down yet"
    );
    window.act(Action::Tool(Tool::Brush));
    assert!(window.view.text().is_none());
    assert_eq!(
        window.view.document().history_depth(),
        1,
        "set down as one step"
    );
    let inked = (bounds.y0..bounds.y1).any(|y| {
        (bounds.x0..bounds.x1).any(|x| {
            window.colour(u32::try_from(x).expect("on"), u32::try_from(y).expect("on")) != [255; 4]
        })
    });
    assert!(inked, "its glyphs are on the picture");
    window.act(Action::Tool(Tool::Text));
    click(&mut window, (50, 40));
    window.key(Key::Char('x'), plain());
    window.key(Key::Named(NamedKey::Escape), plain());
    assert!(window.view.text().is_none(), "Escape turns it down");
    assert_eq!(window.view.document().history_depth(), 1);
}

/// A setting changed from the keyboard sets the text being typed again, as
/// one changed with the pointer does.
#[test]
fn a_text_size_typed_into_the_bar_sets_the_text_again() {
    let mut window = Window::white(200, 80);
    window.act(Action::Tool(Tool::Text));
    window.view.options.text_size = 20;
    click(&mut window, (10, 10));
    for ch in "Hi".chars() {
        window.key(Key::Char(ch), plain());
    }
    let small = window.view.text().expect("being typed").bounds();
    window.click_setting(0);
    window.key(Key::Char('a'), ctrl());
    window.key(Key::Char('4'), plain());
    window.key(Key::Char('0'), plain());
    assert_eq!(window.view.options.text_size, 40);
    let entry = window.view.text().expect("still being typed");
    assert_eq!(entry.text(), "Hi");
    let large = entry.bounds();
    assert!(
        large.y1 - large.y0 > small.y1 - small.y0,
        "set again at the size typed: {small:?} then {large:?}"
    );
}

#[test]
fn a_polygon_is_drawn_a_corner_at_a_time() {
    let mut window = Window::white(40, 40);
    window.act(Action::Tool(Tool::Polygon));
    window.view.options.style = crate::tool::Style::Filled;
    for corner in [(5, 5), (30, 5), (30, 30), (5, 30)] {
        click(&mut window, corner);
    }
    assert_eq!(window.colour(15, 15), [255; 4], "nothing until it closes");
    window.key(Key::Named(NamedKey::Enter), plain());
    assert_eq!(window.colour(15, 15), [0, 0, 0, 255]);
    assert_eq!(window.colour(35, 35), [255; 4]);
    assert_eq!(window.view.document().history_depth(), 1);
}

#[test]
fn a_rectangle_with_round_corners_leaves_its_corners_out() {
    let mut window = Window::white(40, 40);
    window.act(Action::Tool(Tool::Rectangle));
    window.view.options.style = crate::tool::Style::Filled;
    window.view.options.corners = 8;
    window.drag((5, 5), (30, 30));
    assert_eq!(window.colour(5, 5), [255; 4], "the corner is rounded off");
    assert_eq!(
        window.colour(17, 5),
        [0, 0, 0, 255],
        "the edge between is drawn"
    );
    assert_eq!(window.colour(17, 17), [0, 0, 0, 255]);
}

fn filter_index(label: &str) -> usize {
    crate::filter::Filter::ALL
        .iter()
        .position(|filter| filter.label() == label)
        .expect("a filter")
}

/// Where `spot` on the Adjustment pane is.
fn spot_at(window: &Window, spot: crate::adjust::Spot) -> Point {
    let theme = window.registry.active();
    window
        .view
        .adjustment
        .spot(
            spot,
            window.layout.adjustment_settings(),
            (faces(theme), Scale::ONE, theme),
        )
        .expect("on the pane")
}

/// Press and let go on `spot` of the Adjustment pane, answering what asked
/// for more: the press or the release.
fn press_at(window: &mut Window, spot: crate::adjust::Spot) -> Outcome {
    let at = spot_at(window, spot);
    window.move_to(at);
    let pressed = window.press(PointerButton::Primary);
    let released = window.release(PointerButton::Primary);
    if pressed.request.is_some() {
        pressed
    } else {
        released
    }
}

#[test]
fn an_adjustment_opens_docked_and_leaves_the_window_live() {
    let mut window = Window::white(30, 30);
    let outcome = window.act(Action::Adjust(filter_index("Blur")));
    assert!(!window.view.asking(), "nothing modal");
    assert!(window.view.shows(PaneKind::Adjustment));
    assert!(!window.layout.adjustment_settings().is_empty());
    assert_eq!(
        window.view.adjusting(),
        Some(crate::filter::Filter::Blur { radius: 2 })
    );
    window.run_worker(outcome);
    assert!(window.view.preview_canvas().is_some(), "previewed");
    assert_eq!(
        window.colour(3, 3),
        [255; 4],
        "the picture itself untouched"
    );
    assert_eq!(window.view.document().history_depth(), 0);
    window.act(Action::Tool(Tool::Pencil));
    window.act(Action::ZoomIn);
    assert!(
        window.view.adjusting().is_some(),
        "the view and the tools leave it open"
    );
}

#[test]
fn a_filter_is_previewed_one_job_at_a_time_and_applied_as_one_step() {
    let mut window = Window::white(30, 30);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((15, 0), (15, 29));
    let before = window.colour(14, 10);
    let first = window.act(Action::Adjust(filter_index("Blur")));
    assert!(matches!(
        first.request,
        Some(Request::Own(Own::Compute { .. }))
    ));
    let moved = press_at(&mut window, crate::adjust::Spot::Number(300));
    assert!(moved.request.is_none(), "one at a time");
    let again = window.run_worker(first);
    assert!(
        matches!(again.request, Some(Request::Own(Own::Compute { .. }))),
        "the settings moved meanwhile are asked once it lands"
    );
    window.run_worker(again);
    assert!(window.view.preview_canvas().is_some());
    assert_eq!(
        window.colour(14, 10),
        before,
        "the picture itself untouched"
    );
    let applied = press_at(&mut window, crate::adjust::Spot::Apply);
    assert!(applied.request.is_none(), "the preview's own tiles land");
    assert_eq!(
        window.view.adjusting(),
        None,
        "the pane goes back to its list"
    );
    assert!(window.view.preview_canvas().is_none());
    assert_eq!(window.view.document().history_depth(), 2, "as one step");
    assert_ne!(
        window.colour(14, 10),
        before,
        "the line blurred onto its neighbour"
    );
}

#[test]
fn an_adjustment_reset_or_closed_leaves_the_picture_as_it_was() {
    let mut window = Window::white(20, 20);
    let opened = window.act(Action::Adjust(filter_index("Brightness and contrast")));
    assert!(opened.request.is_none(), "nothing to preview yet");
    let moved = press_at(&mut window, crate::adjust::Spot::Number(100));
    window.run_worker(moved);
    assert!(window.view.preview_canvas().is_some());
    press_at(&mut window, crate::adjust::Spot::Reset);
    assert!(
        window.view.preview_canvas().is_none(),
        "back where it started"
    );
    let close = band_point(
        &window,
        PaneKind::Adjustment,
        Some(WindowControlKind::Close),
    );
    click_at(&mut window, close);
    assert_eq!(window.view.adjusting(), None);
    assert!(!window.view.shows(PaneKind::Adjustment));
    assert_eq!(window.view.document().history_depth(), 0);
    assert_eq!(window.colour(5, 5), [255; 4]);
}

#[test]
fn a_palette_picture_is_adjusted_through_its_palette_and_refuses_a_filter() {
    let mut window = four_colours();
    let palette = window.palette();
    window.act(Action::Adjust(filter_index("Desaturate")));
    let grey = window.palette();
    assert_ne!(grey, palette);
    assert!(
        grey.iter().all(|&[r, g, b, _]| r == g && g == b),
        "each entry its grey"
    );
    assert_eq!(window.view.document().history_depth(), 1);
    window.act(Action::Adjust(filter_index("Blur")));
    assert_eq!(window.view.adjusting(), None, "not opened");
    assert!(window
        .view
        .message()
        .is_some_and(|said| said.contains("colour picture")));
    window.act(Action::Adjust(filter_index("Brightness and contrast")));
    let moved = press_at(&mut window, crate::adjust::Spot::Number(900));
    assert!(moved.request.is_none(), "a palette is previewed at once");
    assert!(window.view.preview_kind().is_some());
    assert_eq!(
        window.palette(),
        grey,
        "the palette itself untouched until applied"
    );
    press_at(&mut window, crate::adjust::Spot::Apply);
    assert_ne!(window.palette(), grey, "applied");
    assert_eq!(window.view.adjusting(), None);
}

#[test]
fn a_preview_worked_from_a_picture_since_changed_is_dropped_and_asked_again() {
    let mut window = Window::white(20, 20);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((5, 5), (5, 15));
    let out = window.act(Action::Adjust(filter_index("Blur")));
    window.act(Action::Undo);
    assert!(window.view.adjusting().is_some(), "undo leaves it open");
    let again = window.run_worker(out);
    assert!(
        window.view.preview_canvas().is_none(),
        "worked from the picture before the undo"
    );
    assert!(
        matches!(again.request, Some(Request::Own(Own::Compute { .. }))),
        "asked again of the picture as it now stands"
    );
    window.run_worker(again);
    assert!(window.view.preview_canvas().is_some());
}

#[test]
fn a_stroke_applies_the_adjustment_first_and_one_that_finds_it_unfinished_paints_nothing() {
    let mut window = Window::white(20, 20);
    window.act(Action::Tool(Tool::Pencil));
    let preview = window.act(Action::Adjust(filter_index("Blur")));
    let at = window.screen_of((3, 3));
    window.move_to(at);
    let applying = window.press(PointerButton::Primary);
    window.release(PointerButton::Primary);
    assert_eq!(window.colour(3, 3), [255; 4], "the press painted nothing");
    let Some(Request::Own(Own::Compute { .. })) = applying.request else {
        panic!(
            "the adjustment applied on a worker, not {:?}",
            applying.request
        );
    };
    window.run_worker(applying);
    assert_eq!(window.view.adjusting(), None, "applied and closed");
    assert_eq!(window.view.document().history_depth(), 1);
    let late = window.run_worker(preview);
    assert!(
        late.request.is_none(),
        "the preview's late answer is dropped"
    );
    window.drag((3, 3), (3, 3));
    assert_eq!(window.colour(3, 3), [0, 0, 0, 255], "then the tool paints");
}

#[test]
fn preview_turned_off_shows_the_picture_and_on_again_shows_the_preview_at_once() {
    let mut window = Window::white(20, 20);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((5, 5), (5, 15));
    let out = window.act(Action::Adjust(filter_index("Blur")));
    window.run_worker(out);
    assert!(window.view.preview_canvas().is_some());
    let off = press_at(&mut window, crate::adjust::Spot::Preview);
    assert!(off.request.is_none());
    assert!(
        window.view.preview_canvas().is_none(),
        "the picture as it is"
    );
    let on = press_at(&mut window, crate::adjust::Spot::Preview);
    assert!(on.request.is_none(), "kept to compare");
    assert!(window.view.preview_canvas().is_some());
}

/// A 256×1 picture of greys from 40 to 167.
fn greys() -> Window {
    let mut built =
        crate::canvas::CanvasBuilder::new(256, 1, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
    for x in 0..256u32 {
        let level = u8::try_from(x / 2 + 40).expect("a level");
        built.set(x, 0, Sample::Rgba([level, level, level, 255]));
    }
    Window::new(Document::new(Picture::plain(built.finish())))
}

#[test]
fn levels_read_a_histogram_and_auto_stretches_what_it_holds() {
    let mut window = greys();
    let histogram = window.act(Action::Adjust(filter_index("Levels")));
    assert!(window.view.histogram().is_none());
    window.run_worker(histogram);
    assert!(window.view.histogram().is_some());
    let auto = press_at(&mut window, crate::adjust::Spot::Picker(3));
    let Some(crate::filter::Filter::Levels(levels)) = window.view.adjusting() else {
        panic!("levels open");
    };
    let red = levels.of(crate::tone::Channel::Red);
    assert!(red.black >= 40 && red.white <= 167, "{red:?}");
    window.run_worker(auto);
    assert!(window.view.preview_canvas().is_some());
    press_at(&mut window, crate::adjust::Spot::Apply);
    assert_eq!(window.colour(0, 0)[0], 0, "the darkest grey is black");
}

#[test]
fn an_eyedropper_takes_the_colour_beneath_the_preview() {
    let canvas = Canvas::new(10, 10, Kind::Rgba, Sample::Rgba([30, 20, 10, 255])).expect("fits");
    let mut window = Window::new(Document::new(Picture::plain(canvas)));
    let histogram = window.act(Action::Adjust(filter_index("Levels")));
    window.run_worker(histogram);
    press_at(&mut window, crate::adjust::Spot::Picker(0));
    assert_eq!(
        window.view.adjustment.picking(),
        Some(crate::adjust::Pick::Black)
    );
    let picked = click(&mut window, (4, 4));
    let Some(crate::filter::Filter::Levels(levels)) = window.view.adjusting() else {
        panic!("levels open");
    };
    assert_eq!(levels.of(crate::tone::Channel::Green).black, 20);
    assert_eq!(window.view.adjustment.picking(), None, "put down");
    window.run_worker(picked);
    assert_eq!(window.view.document().history_depth(), 0, "nothing painted");
}

/// A window on a 10×10 picture of a layer of each colour and opacity, the
/// bottom first, painting on layer `active`.
fn layered(layers: &[([u8; 4], u8)], active: usize) -> Window {
    let layers = layers
        .iter()
        .map(|&(colour, opacity)| {
            let canvas = Canvas::new(10, 10, Kind::Rgba, Sample::Rgba(colour)).expect("fits");
            let mut layer = crate::document::Layer::new(canvas, String::from("layer"));
            layer.opacity = opacity;
            layer
        })
        .collect();
    Window::new(Document::new(
        Picture::layered(layers, active).expect("alike"),
    ))
}

fn ctrl_shift() -> Modifiers {
    Modifiers {
        ctrl: true,
        shift: true,
        ..Modifiers::default()
    }
}

fn layers_of(window: &Window) -> &[crate::document::Layer] {
    window
        .view
        .document()
        .picture()
        .expect("a picture")
        .layers()
}

fn painted_on(window: &Window) -> usize {
    window
        .view
        .document()
        .picture()
        .expect("a picture")
        .active()
}

fn near(a: [u8; 4], b: [u8; 4]) -> bool {
    a.iter().zip(&b).all(|(&p, &q)| p.abs_diff(q) <= 1)
}

#[test]
fn a_new_layer_is_painted_on_and_the_one_beneath_kept_as_it_was() {
    let mut window = Window::white(10, 10);
    window.key(Key::Char('N'), ctrl_shift());
    assert_eq!((layers_of(&window).len(), painted_on(&window)), (2, 1));
    assert_eq!(layers_of(&window)[1].name, "Layer 2");
    window.act(Action::Tool(Tool::Pencil));
    window.drag((2, 2), (2, 2));
    assert_ne!(window.colour(2, 2), [0; 4], "drawn on the new layer");
    assert_eq!(layers_of(&window)[0].canvas.colour_at(2, 2), Some([255; 4]));
    window.key(Key::Char('z'), ctrl());
    window.key(Key::Char('z'), ctrl());
    assert_eq!(
        layers_of(&window).len(),
        1,
        "the layer undone with what was drawn on it"
    );
}

/// Going to a layer by its number puts a floating selection down first, on
/// the layer it was lifted from, as stepping to a layer does.
#[test]
fn going_to_a_layer_puts_the_floating_selection_down_where_it_was_lifted() {
    let mut window = layered(&[([255; 4], 255), ([255; 4], 255)], 0);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((2, 2), (2, 2));
    window.lift((1, 1), (3, 3), (4, 0));
    let id = AppMenuItemId::new(GO_TO_LAYER).expect("an id");
    let asked = window
        .view
        .entered(id, "2", &window.layout, &mut Region::new());
    assert_eq!(painted_on(&window), 0, "not gone before it is down");
    window.run_worker(asked);
    assert!(window.view.floating().is_none(), "down");
    assert_eq!(painted_on(&window), 1, "then gone to");
    assert_eq!(
        layers_of(&window)[0].canvas.colour_at(6, 2),
        Some([0, 0, 0, 255]),
        "on the layer it was lifted from"
    );
    assert_eq!(
        layers_of(&window)[1].canvas.colour_at(6, 2),
        Some([255; 4]),
        "not the one gone to"
    );
}

#[test]
fn the_keyboard_steps_through_the_layers_and_moves_them() {
    let mut window = layered(&[([1; 4], 255), ([2; 4], 255), ([3; 4], 255)], 0);
    window.key(Key::Named(NamedKey::PageUp), ctrl());
    assert_eq!(painted_on(&window), 1);
    window.key(Key::Named(NamedKey::PageUp), ctrl_shift());
    assert_eq!(painted_on(&window), 2, "raised, and still painted on");
    assert_eq!(layers_of(&window)[2].canvas.colour_at(0, 0), Some([2; 4]));
    window.key(Key::Named(NamedKey::PageDown), ctrl());
    assert_eq!(painted_on(&window), 1);
    let mut said = String::new();
    crate::render::write_shape(
        &mut said,
        window.view.document().picture().expect("a picture"),
    );
    assert!(said.ends_with("layer 2 of 3: layer"), "{said}");
    window.act(Action::DeleteLayer);
    window.act(Action::DeleteLayer);
    window.act(Action::DeleteLayer);
    assert_eq!(layers_of(&window).len(), 1, "the last is kept");
    assert_eq!(
        window.view.message(),
        Some("A picture keeps at least one layer")
    );
}

#[test]
fn layers_merge_and_flatten_on_a_worker_keeping_the_look() {
    let mut window = layered(&[([0, 0, 255, 255], 255), ([255, 0, 0, 255], 128)], 1);
    let asked = window.key(Key::Char('e'), ctrl());
    window.run_worker(asked);
    assert_eq!(layers_of(&window).len(), 1);
    assert!(
        near(window.colour(4, 4), [128, 0, 127, 255]),
        "{:?}",
        window.colour(4, 4)
    );
    assert_eq!(layers_of(&window)[0].opacity, 255);
    window.key(Key::Char('z'), ctrl());
    assert_eq!(layers_of(&window).len(), 2, "the merge undone whole");
    window.act(Action::NewLayer);
    let asked = window.key(Key::Char('E'), ctrl_shift());
    window.run_worker(asked);
    assert_eq!(layers_of(&window).len(), 1, "flattened");
}

/// A hidden layer is not merged, painted on or beneath: a merge keeps the
/// look, so its pixels would be lost. The menu offers no merge then either.
#[test]
fn a_hidden_layer_is_not_merged() {
    let merge_offered = |window: &Window| {
        window
            .view
            .menu(MenuKind::Window)
            .rows()
            .any(|(row, _)| match row {
                AppMenuRowView::Item(item) => {
                    item.id.get() == Action::MergeDown.id() && item.enabled
                }
                _ => false,
            })
    };
    let two = [([0, 0, 255, 255], 255), ([255, 0, 0, 255], 128)];
    assert!(merge_offered(&layered(&two, 1)), "offered while both show");
    for hidden in [0, 1] {
        let mut window = layered(&two, hidden);
        window.act(Action::ShowLayer);
        if hidden == 0 {
            window.act(Action::LayerAbove);
        }
        assert_eq!(painted_on(&window), 1);
        assert!(
            !merge_offered(&window),
            "layer {hidden} hidden: not offered"
        );
        let refused = window.act(Action::MergeDown);
        assert!(refused.request.is_none(), "no worker asked");
        assert_eq!(
            window.view.message(),
            Some("A hidden or wholly faint layer is not merged")
        );
        assert_eq!(layers_of(&window).len(), 2, "both kept");
        assert!(!layers_of(&window)[hidden].visible, "and as they showed");
    }
    for faint in [0, 1] {
        let mut layers = two;
        layers[faint].1 = 0;
        let mut window = layered(&layers, 1);
        assert!(
            !merge_offered(&window),
            "layer {faint} wholly faint: not offered"
        );
        assert!(
            window.act(Action::MergeDown).request.is_none(),
            "no worker asked"
        );
        assert_eq!(layers_of(&window).len(), 2, "both kept");
        assert_eq!(layers_of(&window)[faint].opacity, 0, "and as they showed");
    }
}

#[test]
fn a_palette_picture_holds_one_layer() {
    let kind = Kind::Indexed {
        depth: IndexDepth::One,
        palette: vec![[0, 0, 0, 255], [255; 4]],
        masked: false,
    };
    let canvas = Canvas::new(8, 8, kind, Sample::Index(0, 255)).expect("fits");
    let mut window = Window::new(Document::new(Picture::plain(canvas)));
    window.act(Action::NewLayer);
    assert_eq!(layers_of(&window).len(), 1);
    assert!(window
        .view
        .message()
        .is_some_and(|said| said.starts_with("A palette picture holds one layer")));
}

#[test]
fn a_picture_of_layers_is_not_made_a_palette_picture() {
    use crate::transform::{Depth, PaletteChoice, Transform};
    let mut window = layered(&[([255; 4], 255), ([0; 4], 255)], 1);
    let outcome = window.view.transform(
        Transform::Convert {
            depth: Depth::Indexed(IndexDepth::Four),
            palette: PaletteChoice::Desktop,
            dither: false,
        },
        "change its colours",
        &window.layout,
        &mut Region::new(),
    );
    assert!(outcome.request.is_none(), "no worker asked");
    assert_eq!(
        window.view.message(),
        Some("A palette picture holds one layer, shown wholly: flatten the picture first")
    );
}

/// One layer faded or hidden is not shown wholly, so it is not made a
/// palette picture either: its look would change, or it would hold a layer
/// a palette picture cannot.
#[test]
fn a_layer_not_shown_wholly_is_not_made_a_palette_picture() {
    use crate::transform::{Depth, PaletteChoice, Transform};
    let convert = Transform::Convert {
        depth: Depth::Indexed(IndexDepth::Four),
        palette: PaletteChoice::Desktop,
        dither: false,
    };
    // One hidden layer flattened would come out clear, so it is shown instead.
    for (opacity, visible, advice) in [
        (128, true, "flatten the picture first"),
        (255, false, "show the layer first"),
        (128, false, "show the layer first"),
    ] {
        let mut window = layered(&[([255; 4], opacity)], 0);
        if !visible {
            window.act(Action::ShowLayer);
        }
        let outcome = window.view.transform(
            convert,
            "change its colours",
            &window.layout,
            &mut Region::new(),
        );
        assert!(outcome.request.is_none(), "no worker asked");
        assert!(
            window
                .view
                .message()
                .is_some_and(|said| said.ends_with(advice)),
            "{opacity} shown {visible}: {:?}",
            window.view.message()
        );
        assert_eq!(
            window.view.document().picture().map(|p| p.layers().len()),
            Some(1)
        );
    }
    let mut window = layered(&[([255; 4], 255)], 0);
    let asked = window.view.transform(
        convert,
        "change its colours",
        &window.layout,
        &mut Region::new(),
    );
    window.run_worker(asked);
    assert!(
        !matches!(
            window.view.document().picture().map(|p| p.canvas().kind()),
            Some(Kind::Rgba)
        ),
        "one layer shown wholly is converted"
    );
}

#[test]
fn a_turn_turns_every_layer_and_a_new_canvas_is_filled_beneath_them_alone() {
    use crate::transform::{Anchor, Transform};
    let mut window = layered(&[([255; 4], 255), ([0; 4], 255)], 1);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((0, 0), (0, 0));
    let asked = window.act(Action::RotateRight);
    window.run_worker(asked);
    assert_ne!(
        window.colour(9, 0),
        [0; 4],
        "the top layer's dot turned with it"
    );
    assert_eq!(layers_of(&window)[0].canvas.colour_at(9, 0), Some([255; 4]));
    let asked = window.view.transform(
        Transform::Resize {
            width: 12,
            height: 10,
            anchor: Anchor::TopLeft,
            fill: Sample::Rgba([9, 9, 9, 255]),
        },
        "resize the canvas",
        &window.layout,
        &mut Region::new(),
    );
    window.run_worker(asked);
    assert_eq!(
        layers_of(&window)[0].canvas.colour_at(11, 5),
        Some([9, 9, 9, 255])
    );
    assert_eq!(
        layers_of(&window)[1].canvas.colour_at(11, 5),
        Some([0; 4]),
        "left clear"
    );
}

#[test]
fn an_adjustment_runs_on_the_layer_painted_on_alone() {
    let mut window = layered(&[([10, 20, 30, 255], 255), ([0; 4], 255)], 0);
    let asked = window.act(Action::Invert);
    window.run_worker(asked);
    assert_eq!(window.colour(3, 3), [245, 235, 225, 255]);
    assert_eq!(layers_of(&window)[1].canvas.colour_at(3, 3), Some([0; 4]));
}

#[test]
fn the_eyedropper_takes_what_the_layers_show_together() {
    let mut window = layered(&[([0, 0, 255, 255], 255), ([255, 0, 0, 255], 128)], 1);
    window.act(Action::Tool(Tool::Eyedropper));
    window.drag((4, 4), (4, 4));
    let (primary, _) = window.view.inks();
    let Ink::Colour(taken) = primary else {
        panic!("a colour, not {primary:?}");
    };
    assert!(near(taken, [128, 0, 127, 255]), "{taken:?}");
}

#[test]
fn a_layer_is_renamed_and_faded_through_its_form_and_undone_whole() {
    let mut window = layered(&[([255; 4], 255), ([0; 4], 255)], 1);
    window.act(Action::LayerProperties);
    assert!(window.view.asking());
    for _ in 0.."layer".len() {
        window.key(Key::Named(NamedKey::Backspace), plain());
    }
    for ch in "Sky".chars() {
        window.key(Key::Char(ch), plain());
    }
    window.key(Key::Named(NamedKey::Enter), plain());
    assert!(!window.view.asking());
    assert_eq!(layers_of(&window)[1].name, "Sky");
    window.key(Key::Char('z'), ctrl());
    assert_eq!(layers_of(&window)[1].name, "layer");
}

/// Where pane `kind`'s band seats `control`, or the middle of the span it is
/// dragged by.
fn band_point(window: &Window, kind: PaneKind, control: Option<WindowControlKind>) -> Point {
    let slot = window.layout.pane(kind).expect("shown");
    let band = window
        .view
        .header(kind)
        .layout(slot.header, Scale::ONE, window.registry.active());
    let rect = match control {
        Some(control) => band
            .controls()
            .iter()
            .find_map(|&(seated, rect)| (seated == control).then_some(rect))
            .expect("seated"),
        None => band.drag,
    };
    rect.center()
}

fn click_at(window: &mut Window, at: Point) {
    window.move_to(at);
    window.press(PointerButton::Primary);
    window.release(PointerButton::Primary);
}

/// Whether View ▸ Panes ticks pane `kind`.
fn ticked(window: &Window, kind: PaneKind) -> bool {
    window
        .view
        .menu(MenuKind::Window)
        .rows()
        .any(|(row, _)| match row {
            AppMenuRowView::Item(item) => {
                item.id.get() == Action::Pane(kind).id()
                    && item.mark == tairix_abi::window_ipc::AppMenuMark::Check
            }
            _ => false,
        })
}

#[test]
fn a_pane_closed_by_its_band_gives_up_its_dock_and_the_view_menu_shows_it_again() {
    let mut window = Window::white(10, 10);
    let canvas = window.layout.canvas();
    assert!(ticked(&window, PaneKind::Colour));
    let close = band_point(&window, PaneKind::Colour, Some(WindowControlKind::Close));
    click_at(&mut window, close);
    assert!(!window.view.shows(PaneKind::Colour));
    assert!(window.layout.pane(PaneKind::Colour).is_none());
    assert!(window.layout.dock_on(Side::Right).rect.is_empty());
    assert!(
        window.layout.canvas().width > canvas.width,
        "the canvas takes its room"
    );
    assert!(!ticked(&window, PaneKind::Colour));
    window.act(Action::Pane(PaneKind::Colour));
    assert_eq!(
        window.view.arrangement().place(PaneKind::Colour),
        Some((Side::Right, 0))
    );
    assert_eq!(window.layout.canvas(), canvas);
}

#[test]
fn a_pane_rolls_up_to_its_band_and_opens_again() {
    let mut window = Window::white(10, 10);
    let roll = band_point(&window, PaneKind::Tools, Some(WindowControlKind::Minimize));
    click_at(&mut window, roll);
    let slot = window.layout.pane(PaneKind::Tools).expect("still shown");
    assert!(slot.body.is_empty());
    assert!(window.layout.tool_box().is_empty());
    assert!(window.view.shows(PaneKind::Tools));
    let roll = band_point(&window, PaneKind::Tools, Some(WindowControlKind::Minimize));
    click_at(&mut window, roll);
    assert!(!window.layout.tool_box().is_empty());
}

#[test]
fn a_band_dragged_over_the_other_dock_marks_where_it_lands_and_lands_there() {
    let mut window = Window::white(10, 10);
    let grip = band_point(&window, PaneKind::Tools, None);
    window.move_to(grip);
    window.press(PointerButton::Primary);
    let right = window.layout.dock_on(Side::Right).rect;
    let under_colour = Point::new(right.center().x, right.bottom() - 2);
    window.move_to(under_colour);
    let drag = window.view.pane_drag.expect("dragging");
    assert_eq!(drag.kind, PaneKind::Tools);
    assert_eq!(
        drag.landing,
        Some(super::Landing {
            side: Side::Right,
            before: 1
        })
    );
    let over_colour = Point::new(right.center().x, right.top() + 4);
    window.move_to(over_colour);
    assert_eq!(
        window.view.pane_drag.and_then(|drag| drag.landing),
        Some(super::Landing {
            side: Side::Right,
            before: 0
        })
    );
    window.release(PointerButton::Primary);
    assert!(window.view.pane_drag.is_none());
    assert_eq!(
        window.view.arrangement().place(PaneKind::Tools),
        Some((Side::Right, 0))
    );
    assert!(window.layout.dock_on(Side::Left).rect.is_empty());
    let tools = window.layout.pane(PaneKind::Tools).expect("shown");
    let colour = window.layout.pane(PaneKind::Colour).expect("shown");
    assert!(
        tools.frame.bottom() < colour.frame.top(),
        "above the colour pane"
    );
}

#[test]
fn a_band_dragged_to_the_edge_of_an_empty_dock_lands_in_it() {
    let mut window = Window::white(10, 10);
    window.act(Action::Pane(PaneKind::Tools));
    assert!(window.layout.dock_on(Side::Left).rect.is_empty());
    let grip = band_point(&window, PaneKind::Colour, None);
    window.move_to(grip);
    window.press(PointerButton::Primary);
    let middle = window.layout.canvas().center().y;
    window.move_to(Point::new(2, middle));
    assert_eq!(
        window.view.pane_drag.and_then(|drag| drag.landing),
        Some(super::Landing {
            side: Side::Left,
            before: 0
        })
    );
    window.release(PointerButton::Primary);
    assert_eq!(
        window.view.arrangement().place(PaneKind::Colour),
        Some((Side::Left, 0))
    );
    assert!(window.layout.dock_on(Side::Right).rect.is_empty());
}

#[test]
fn a_pane_drag_holds_the_pointer_and_one_let_go_away_from_a_dock_floats_where_it_went() {
    let mut window = Window::white(10, 10);
    window.act(Action::Tool(Tool::Pencil));
    let grip = band_point(&window, PaneKind::Tools, None);
    let band = window.layout.pane(PaneKind::Tools).expect("docked").header;
    window.move_to(grip);
    window.press(PointerButton::Primary);
    window.move_to(window.screen_of((2, 2)));
    assert_eq!(window.view.pane_drag.and_then(|drag| drag.landing), None);
    let to = window.screen_of((7, 7));
    window.move_to(to);
    window.release(PointerButton::Primary);
    assert!(window.view.arrangement().floats(PaneKind::Tools));
    assert_eq!(window.colour(2, 2), [255; 4], "the canvas took nothing");
    assert_eq!(window.colour(7, 7), [255; 4]);
    let opening = window.view.tool_opening(super::tool_id(PaneKind::Tools));
    let grab = (grip.x - band.left(), grip.y - band.top());
    assert_eq!(
        opening,
        ToolOpening {
            offset: (to.x - grab.0, to.y - grab.1 + to_i32(band.height)),
            carry: None,
        },
        "its band lies where the pane's would have, and nothing carries it"
    );
}

/// Drag pane `kind` by its band to `to`, the press still held.
fn drag_band(window: &mut Window, kind: PaneKind, to: Point) -> Outcome {
    let grip = band_point(window, kind, None);
    window.move_to(grip);
    window.press(PointerButton::Primary);
    window.move_to(Point::new(grip.x, grip.y + 30));
    window.move_to(to)
}

#[test]
fn a_band_carried_to_the_windows_edge_tears_its_pane_out_under_the_press() {
    let mut window = Window::white(10, 10);
    let grip = band_point(&window, PaneKind::Colour, None);
    let band = window.layout.pane(PaneKind::Colour).expect("docked").header;
    let edge = Point::new(to_i32(WINDOW.0) - 1, grip.y + 40);
    let outcome = drag_band(&mut window, PaneKind::Colour, edge);
    assert_eq!(outcome.relayout, Relayout::Whole);
    assert!(
        window.view.pane_drag.is_none(),
        "the press is the tool window's now"
    );
    assert!(window.view.arrangement().floats(PaneKind::Colour));
    assert!(window.layout.dock_on(Side::Right).rect.is_empty());
    let id = super::tool_id(PaneKind::Colour);
    let opening = window.view.tool_opening(id);
    assert_eq!(
        opening.carry,
        Some(u32::try_from(grip.x - band.left()).expect("on the band"))
    );
    assert_eq!(
        window.view.tool_opening(id).carry,
        None,
        "a carry is for the open it was asked for"
    );
    // The tool window shows the pane laid out beside the window, never over it.
    let wanted = window
        .view
        .tool_window(&window.layout, 0)
        .expect("one tool window");
    assert_eq!((wanted.id, wanted.title), (id, "Colour"));
    assert!(wanted.rect.intersection(&window.layout.window()).is_empty());
    assert_eq!(window.layout.pane_window(PaneKind::Colour), wanted.rect);
    assert!(wanted.rect.contains(window.layout.picker().center()));
    assert!(window.view.tool_window(&window.layout, 1).is_none());
    // Its band's press went with it: the next sample over the old band is a
    // hover, not a drag.
    window.release(PointerButton::Primary);
    window.move_to(grip);
    assert!(window.view.pane_drag.is_none());
}

#[test]
fn a_tool_window_moved_over_a_dock_marks_it_and_let_go_there_docks_its_pane() {
    let mut window = Window::white(10, 10);
    window.view.panes.float(PaneKind::Tools);
    window.relayout();
    let id = super::tool_id(PaneKind::Tools);
    let left = window.layout.window().left() + 2;
    let middle = window.layout.canvas().center().y;
    let moved = |over, ended| ToolMove { id, over, ended };
    let mut damage = Region::new();
    let theme = window.registry.active();
    let outcome = window.view.tool_moved(
        moved(Some(Point::new(left, middle)), false),
        &window.layout,
        Scale::ONE,
        theme,
        &mut damage,
    );
    assert_eq!(outcome.relayout, Relayout::None);
    assert_eq!(
        window.view.landing(),
        Some(super::Landing {
            side: Side::Left,
            before: 0
        })
    );
    assert!(!damage.is_empty(), "the mark is drawn");
    let outcome = window.view.tool_moved(
        moved(None, false),
        &window.layout,
        Scale::ONE,
        theme,
        &mut damage,
    );
    assert_eq!(outcome.relayout, Relayout::None);
    assert_eq!(
        window.view.landing(),
        None,
        "off the window, nothing is marked"
    );
    let outcome = window.view.tool_moved(
        moved(Some(Point::new(left, middle)), true),
        &window.layout,
        Scale::ONE,
        theme,
        &mut damage,
    );
    window.apply(&outcome);
    assert_eq!(window.view.landing(), None);
    assert_eq!(
        window.view.arrangement().place(PaneKind::Tools),
        Some((Side::Left, 0))
    );
    assert!(
        window.view.tool_window(&window.layout, 0).is_none(),
        "its tool window goes"
    );
}

#[test]
fn a_tool_windows_close_mark_hides_its_pane_and_a_refusal_docks_it_home() {
    let mut window = Window::white(10, 10);
    window.view.panes.float(PaneKind::Colour);
    window.view.panes.float(PaneKind::Tools);
    window.relayout();
    let outcome = window.view.tool_gone(
        super::tool_id(PaneKind::Colour),
        ToolGone::Closed,
        &window.layout,
        &mut Region::new(),
    );
    window.apply(&outcome);
    assert!(!window.view.shows(PaneKind::Colour));
    let outcome = window.view.tool_gone(
        super::tool_id(PaneKind::Tools),
        ToolGone::Refused,
        &window.layout,
        &mut Region::new(),
    );
    window.apply(&outcome);
    assert_eq!(
        window.view.arrangement().place(PaneKind::Tools),
        Some((Side::Left, 0))
    );
}

#[test]
fn floating_panes_are_laid_out_apart_and_open_at_home_when_nothing_tore_them_out() {
    let mut window = Window::white(10, 10);
    window.act(Action::Pane(PaneKind::Adjustment));
    for kind in PaneKind::ALL {
        window.view.panes.float(kind);
    }
    window.relayout();
    let slots = window.layout.floating();
    assert_eq!(slots.len(), 3);
    for (index, slot) in slots.iter().enumerate() {
        assert!(slot.frame.intersection(&window.layout.window()).is_empty());
        assert!(slot.header.is_empty(), "the window manager draws its band");
        for other in &slots[index + 1..] {
            assert!(slot.frame.intersection(&other.frame).is_empty());
        }
    }
    let tools = window.view.tool_opening(super::tool_id(PaneKind::Tools));
    let colour = window.view.tool_opening(super::tool_id(PaneKind::Colour));
    let below = window.layout.top().bottom();
    assert_eq!(
        tools,
        ToolOpening {
            offset: (0, below),
            carry: None
        }
    );
    let width = window
        .layout
        .pane(PaneKind::Colour)
        .expect("floating")
        .frame
        .width;
    assert_eq!(
        colour.offset,
        (to_i32(WINDOW.0) - to_i32(width), below),
        "against the right edge, its home"
    );
}

#[test]
fn escape_or_losing_the_keyboard_turns_a_pane_drag_down() {
    for lose_focus in [false, true] {
        let mut window = Window::white(10, 10);
        let before = window.view.arrangement().clone();
        let grip = band_point(&window, PaneKind::Tools, None);
        window.move_to(grip);
        window.press(PointerButton::Primary);
        let right = window.layout.dock_on(Side::Right).rect;
        window.move_to(right.center());
        assert!(window.view.pane_drag.is_some());
        if lose_focus {
            window
                .view
                .focus_changed(false, &window.layout, &mut Region::new());
        } else {
            window.key(Key::Named(NamedKey::Escape), plain());
        }
        assert!(window.view.pane_drag.is_none());
        window.release(PointerButton::Primary);
        assert_eq!(
            window.view.arrangement(),
            &before,
            "lose focus: {lose_focus}"
        );
    }
}

#[test]
fn reset_panes_puts_every_pane_back_open_where_it_starts() {
    let mut window = Window::white(10, 10);
    let start = window.layout.clone();
    window.act(Action::Pane(PaneKind::Adjustment));
    window.act(Action::Pane(PaneKind::Colour));
    let roll = band_point(&window, PaneKind::Tools, Some(WindowControlKind::Minimize));
    click_at(&mut window, roll);
    window.act(Action::ResetPanes);
    assert_eq!(
        window.view.arrangement(),
        &crate::pane::Arrangement::default()
    );
    assert_eq!(window.layout, start);
}

/// Where control `cell` of part `part` of the colour pane's panel is.
fn colour_control(window: &Window, part: usize, cell: usize) -> Point {
    let theme = window.registry.active();
    window
        .view
        .colour_controls
        .place_of(
            part,
            window.layout.colour_controls(),
            faces(theme),
            Scale::ONE,
            theme,
        )
        .expect("laid out")
        .controls[cell]
        .center()
}

#[test]
fn the_colour_pane_swaps_resets_and_picks_once() {
    let canvas = Canvas::new(10, 10, Kind::Rgba, Sample::Rgba([200, 30, 60, 255])).expect("fits");
    let mut window = Window::new(Document::new(Picture::plain(canvas)));
    window.act(Action::Tool(Tool::Pencil));
    let at = colour_control(&window, 0, 0);
    click_at(&mut window, at);
    assert_eq!(
        window.view.inks(),
        (Ink::Colour([255; 4]), Ink::Colour([0, 0, 0, 255])),
        "swapped"
    );
    let at = colour_control(&window, 0, 1);
    click_at(&mut window, at);
    assert_eq!(
        window.view.inks(),
        (Ink::Colour([0, 0, 0, 255]), Ink::Colour([255; 4])),
        "reset"
    );
    let at = colour_control(&window, 0, 2);
    click_at(&mut window, at);
    assert!(window.view.picking_colour);
    click(&mut window, (4, 4));
    assert_eq!(
        window.view.inks().0,
        Ink::Colour([200, 30, 60, 255]),
        "picked once"
    );
    assert!(!window.view.picking_colour);
    assert_eq!(
        window.view.tool(),
        Tool::Pencil,
        "the tool in use carries on"
    );
    assert_eq!(
        window.colour(4, 4),
        [200, 30, 60, 255],
        "the press painted nothing"
    );
    assert_eq!(
        window.view.recents.first(),
        Some(&Rgba::from_array([200, 30, 60, 255]))
    );
    window.act(Action::PickColour);
    window.key(Key::Named(NamedKey::Escape), plain());
    assert!(!window.view.picking_colour, "Escape puts it down");
}

#[test]
fn the_colour_pane_chooses_the_pickers_view_and_fields() {
    let mut window = Window::white(10, 10);
    let at = colour_control(&window, 1, 0);
    click_at(&mut window, at);
    window.key(Key::Named(NamedKey::Down), plain());
    window.key(Key::Named(NamedKey::Enter), plain());
    assert_eq!(
        window.view.picker.view(),
        tairix_controls::PickerView::Wheel
    );
    let at = colour_control(&window, 2, 0);
    click_at(&mut window, at);
    for _ in 0..3 {
        window.key(Key::Named(NamedKey::Down), plain());
    }
    window.key(Key::Named(NamedKey::Enter), plain());
    assert_eq!(
        window.view.picker.model(),
        tairix_controls::ColourModel::Cmyk
    );
}

#[test]
fn colours_settled_are_remembered_and_chosen_again() {
    let mut built =
        crate::canvas::CanvasBuilder::new(4, 4, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits");
    built.set(1, 1, Sample::Rgba([10, 20, 30, 255]));
    built.set(2, 2, Sample::Rgba([40, 50, 60, 255]));
    let mut window = Window::new(Document::new(Picture::plain(built.finish())));
    window.act(Action::Tool(Tool::Eyedropper));
    for pixel in [(1, 1), (2, 2), (1, 1)] {
        window.drag(pixel, pixel);
    }
    assert_eq!(
        window.view.recents,
        [
            Rgba::from_array([10, 20, 30, 255]),
            Rgba::from_array([40, 50, 60, 255])
        ],
        "the latest first, once"
    );
    let recents = window.view.recents_rect(&window.layout);
    let second = window
        .view
        .recent_grid
        .cell_rect(recents, 1)
        .expect("a well");
    click_at(&mut window, second.center());
    assert_eq!(window.view.inks().0, Ink::Colour([40, 50, 60, 255]));
    let mut palette = four_colours();
    assert!(
        !palette.view.recent_grid.state().enabled,
        "a palette picture's inks are its entries"
    );
    palette.act(Action::ResetColours);
    assert!(matches!(palette.view.inks().0, Ink::Index(_)));
}

/// Settings with the grid shown and snapped to, its cells `spacing` across.
fn snapping(spacing: u32) -> crate::preferences::Preferences {
    let mut preferences = crate::preferences::Preferences::default();
    preferences.grid.spacing = (spacing, spacing);
    preferences.grid.shown = true;
    preferences.grid.snap = true;
    preferences
}

#[test]
fn a_new_window_starts_with_the_tool_panes_grid_and_fit_the_settings_name() {
    let mut window = Window::white(2000, 1600);
    let mut preferences = crate::preferences::Preferences {
        tool: Tool::Line,
        ..crate::preferences::Preferences::default()
    };
    preferences.panes.hide(PaneKind::Colour);
    preferences.grid.shown = true;
    window.view.begin(&preferences);
    window.relayout();
    assert_eq!(window.view.tool(), Tool::Line);
    assert!(!window.view.panes().shows(PaneKind::Colour));
    assert!(window.view.grid_shown());
    assert!(
        window.view.viewport().zoom()
            < crate::viewport::Zoom::of(crate::viewport::ZOOMS[crate::viewport::ACTUAL]),
        "a picture larger than the window opens fitted"
    );
    // Reset panes puts back what the settings name, not the shipped ones.
    window.act(Action::Pane(PaneKind::Colour));
    window.act(Action::ResetPanes);
    assert!(!window.view.panes().shows(PaneKind::Colour));
}

#[test]
fn a_picture_opened_at_actual_size_is_not_fitted() {
    let mut window = Window::white(2000, 1600);
    let mut preferences = crate::preferences::Preferences::default();
    preferences.open_at = crate::preferences::OpenAt::Actual;
    window.view.begin(&preferences);
    window.relayout();
    assert_eq!(window.view.viewport().rung(), Some(crate::viewport::ACTUAL));
}

#[test]
fn the_pixel_grid_shows_from_the_zoom_the_settings_name() {
    let mut window = Window::white(16, 16);
    let mut preferences = crate::preferences::Preferences {
        pixel_grid_from: 300,
        ..crate::preferences::Preferences::default()
    };
    window
        .view
        .adopt(&preferences, &window.layout, &mut Region::new());
    let rung = |percent| {
        crate::viewport::ZOOMS
            .iter()
            .position(|&(n, d)| n * 100 / d == percent)
            .expect("a rung")
    };
    window.act(Action::Zoom(rung(200)));
    assert!(!window.view.pixel_grid_shown());
    window.act(Action::Zoom(rung(300)));
    assert!(window.view.pixel_grid_shown());
    preferences.pixel_grid_from = 0;
    window
        .view
        .adopt(&preferences, &window.layout, &mut Region::new());
    assert!(!window.view.pixel_grid_shown(), "never");
}

#[test]
fn snapping_puts_a_marquee_on_whole_cells_only_while_the_grid_shows() {
    let mut window = Window::white(64, 64);
    window.view.begin(&snapping(16));
    window.act(Action::Tool(Tool::Select));
    window.drag((3, 2), (29, 20));
    let bounds = window.view.selection().expect("a selection").bounds();
    assert_eq!((bounds.x0, bounds.y0, bounds.x1, bounds.y1), (0, 0, 32, 16));
    window.act(Action::Deselect);
    window.act(Action::Grid);
    window.drag((3, 2), (29, 20));
    let bounds = window.view.selection().expect("a selection").bounds();
    assert_eq!(
        (bounds.x0, bounds.y0),
        (3, 2),
        "the grid hidden, nothing snaps"
    );
}

#[test]
fn snapping_puts_a_shapes_corners_and_a_lines_ends_on_the_grid() {
    let mut window = Window::white(64, 64);
    window.view.begin(&snapping(8));
    window.act(Action::Tool(Tool::Rectangle));
    window.drag((3, 3), (13, 13));
    assert_eq!(
        window.colour(0, 0),
        [0, 0, 0, 255],
        "the outline starts on the line"
    );
    assert_eq!(
        window.colour(15, 15),
        [0, 0, 0, 255],
        "and ends on the last pixel of the cell"
    );
    assert_eq!(window.colour(8, 8), [255; 4], "inside, untouched");
    assert_eq!(window.colour(16, 16), [255; 4], "nothing past the cell");

    let mut lined = Window::white(64, 64);
    lined.view.begin(&snapping(8));
    lined.act(Action::Tool(Tool::Line));
    lined.drag((3, 30), (13, 30));
    assert_eq!(
        lined.colour(0, 32),
        [0, 0, 0, 255],
        "from the crossing's pixel"
    );
    assert_eq!(
        lined.colour(16, 32),
        [0, 0, 0, 255],
        "to the next crossing's"
    );
    assert_eq!(lined.colour(21, 32), [255; 4], "and no further");
}

#[test]
fn a_moved_selections_corner_lands_on_the_grid() {
    let mut window = Window::white(64, 64);
    window.view.begin(&snapping(8));
    window.act(Action::Tool(Tool::Select));
    window.drag((0, 0), (7, 7));
    window.drag((2, 2), (13, 2));
    let floating = window.view.floating().expect("lifted and moved").bounds();
    assert_eq!(
        (floating.x0, floating.y0),
        (8, 0),
        "eleven across lands on the cell beyond"
    );
}
