use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;

use tairix_abi::window_ipc::{
    AppMenuItemId, AppMenuRowView, CursorShape, DialogModality, WindowSizing,
};
use tairix_colour::Rgba;
use tairix_controls::{Keystroke, ScrollOrientation, WindowControlKind};
use tairix_geometry::{to_i32, Point, Rect, Region, Scale};
use tairix_image::{IndexDepth, SpriteMode, SpriteName, SpritePalette, Unkept};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_parallel::SERIAL;
use tairix_reclaim::PressureBand;
use tairix_theme::{Theme, ThemeRegistry};
use tairix_window::docapp::{DocumentView, Relayout, ToolGone, ToolMove, ToolOpening};
use tairix_window::document::{Access, SavedDocument};

use super::input::shortcut;
use super::{
    compute, Action, Computed, Marking, MenuKind, Outcome, Own, Request, View, GO_TO_ENTRY,
    GO_TO_LAYER, RENAME_ENTRY,
};
use crate::brush::{AIRBRUSH_INTERVAL_NS, AIRBRUSH_STILL_NS};
use crate::canvas::{Canvas, Kind, OutOfMemory, Sample};
use crate::colour::Ink;
use crate::document::{Document, Entry, NewPicture, Origin, Picture, SpriteInfo};
use crate::layout::Faces;
use crate::mask::Mask;
use crate::op::{Family, Halted, Op};
use crate::op_pane::Spot;
use crate::pane::{PaneKind, Side};
use crate::preview::{PreviewAnswer, PreviewJob, PreviewWork, Previewed};
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
    registry: ThemeRegistry,
    now: u64,
}

impl Window {
    fn new(document: Document) -> Self {
        Self::over(View::new(
            document,
            String::from("picture.png"),
            None,
            Access::Writable,
        ))
    }

    fn over(view: View) -> Self {
        let mut window = Self {
            view,
            registry: ThemeRegistry::with_builtins(),
            now: 1_000_000_000,
        };
        window.relayout();
        window
    }

    fn white(width: u32, height: u32) -> Self {
        let canvas = Canvas::new(width, height, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits");
        Self::new(Document::new(Picture::plain(canvas)))
    }

    /// Lay the window out again, as the host does when an outcome asks.
    fn relayout(&mut self) {
        let theme = self.registry.active();
        self.view.lay_out(
            WINDOW.0,
            WINDOW.1,
            theme,
            Scale::ONE,
            faces(theme),
            &mut Region::new(),
        );
    }

    fn apply(&mut self, outcome: &Outcome) {
        if outcome.relayout != Relayout::None {
            self.relayout();
        }
    }

    fn pointer(&mut self, event: InputEvent) -> Outcome {
        self.pointer_at(event, self.now + 10_000_000)
    }

    /// Feed `event` as its device reported it at `at_ns`.
    fn pointer_at(&mut self, event: InputEvent, at_ns: u64) -> Outcome {
        self.now = at_ns;
        let theme = self.registry.active();
        let outcome = self
            .view
            .on_pointer(&event, at_ns, Scale::ONE, theme, &mut Region::new());
        self.apply(&outcome);
        outcome
    }

    fn move_to(&mut self, at: Point) -> Outcome {
        self.pointer(InputEvent::PointerMoved { to: at })
    }

    /// Serve the view's deadlines as its window's loop does, up to `until_ns`.
    fn run_until(&mut self, until_ns: u64) {
        for _ in 0..10_000 {
            let theme = self.registry.active();
            self.view.arm_deadline(self.now, Scale::ONE, theme);
            let Some(due) = self.view.deadline_ns().filter(|&due| due <= until_ns) else {
                self.now = until_ns;
                return;
            };
            self.now = self.now.max(due);
            self.view
                .tick(self.now, Scale::ONE, theme, &mut Region::new());
        }
        panic!("a deadline that never moves on");
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
            Scale::ONE,
            theme,
            &mut Region::new(),
        );
        self.apply(&outcome);
        outcome
    }

    fn act(&mut self, action: Action) -> Outcome {
        let outcome = self.view.act(action, &mut Region::new());
        self.apply(&outcome);
        outcome
    }

    /// A key pressed in the window's dialog.
    fn dialog_key(&mut self, key: Key) -> Outcome {
        let theme = self.registry.active();
        let outcome = self.view.on_dialog(
            &InputEvent::KeyPressed {
                key,
                modifiers: Modifiers::default(),
            },
            self.now,
            (Scale::ONE, theme),
            &mut Region::new(),
        );
        self.apply(&outcome);
        outcome
    }

    /// Click setting `index` of the tool-controls bar.
    fn click_setting(&mut self, index: usize) {
        let at = self
            .view
            .layout()
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
            .popup_rect(self.view.layout().bar(), Scale::ONE, theme)
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
        let (ox, oy) = self
            .view
            .viewport()
            .origin(size, self.view.layout().canvas());
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
        let answer = compute(work, &SERIAL);
        let outcome = self.view.computed(job, answer, &mut Region::new());
        self.apply(&outcome);
        outcome
    }

    /// What the window's loop would ask of the preview worker now.
    fn wanted_preview(&mut self) -> Option<Request> {
        self.view.wanted_preview(Scale::ONE)
    }

    /// Carry out preview request `request` as the preview worker would, and
    /// land its answer.
    fn serve_preview(&mut self, request: Request) {
        let Request::Own(Own::Preview { job, stamp, work }) = request else {
            panic!("a request for the preview worker, not {request:?}");
        };
        let asked = PreviewJob {
            window: 0,
            job,
            stamp,
            work,
        };
        let answer = crate::preview::serve(asked, &SERIAL, &|| false);
        self.view.previewed(answer, &mut Region::new());
    }

    /// Serve the preview worker as the window's loop asks it until the window
    /// wants nothing more of it.
    fn settle_previews(&mut self) {
        for _ in 0..64 {
            let Some(request) = self.wanted_preview() else {
                return;
            };
            self.serve_preview(request);
        }
        panic!("previews that never settle");
    }

    /// A point `(across, down)` pixels into the colour dock's picker, whose
    /// plane fills its top-left corner.
    fn dock_point(&self, (across, down): (i32, i32)) -> Point {
        let picker = self.view.layout().picker();
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
    window
        .view
        .computed(99, Computed::Picture(Ok(vec![canvas])), &mut Region::new());
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

/// Press an airbrush at picture pixel `at` at `at_ns` on a white picture
/// `width` by `height`.
fn airbrush_at(width: u32, height: u32, at: (u32, u32), at_ns: u64) -> Window {
    let mut window = Window::white(width, height);
    window.act(Action::Tool(Tool::Airbrush));
    let to = window.screen_of(at);
    window.pointer_at(InputEvent::PointerMoved { to }, at_ns);
    window.pointer_at(
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        at_ns,
    );
    window
}

#[test]
fn a_held_airbrush_builds_paint_once_it_is_known_to_be_still() {
    let start = 1_000_000_000;
    let mut window = airbrush_at(80, 80, (40, 40), start);
    let middle = |window: &Window| window.colour(40, 40)[0];
    let pressed = middle(&window);
    assert!(pressed < 255, "the first dab lands at once");
    window.run_until(start + AIRBRUSH_INTERVAL_NS + AIRBRUSH_STILL_NS - 1);
    assert_eq!(
        middle(&window),
        pressed,
        "a silence this short may be a move still on its way"
    );
    window.run_until(start + AIRBRUSH_INTERVAL_NS + AIRBRUSH_STILL_NS);
    let once = middle(&window);
    assert!(once < pressed, "held still, it builds up");
    window.run_until(start + 8 * AIRBRUSH_INTERVAL_NS + AIRBRUSH_STILL_NS);
    assert!(middle(&window) < once, "and goes on building while held");
    window.release(PointerButton::Primary);
    let theme = window.registry.active();
    window.view.arm_deadline(window.now, Scale::ONE, theme);
    assert_eq!(
        window.view.deadline_ns(),
        None,
        "nothing to wake for once let go"
    );
}

#[test]
fn a_hold_too_short_to_vouch_for_is_sprayed_when_it_is_let_go() {
    let start = 1_000_000_000;
    let mut window = airbrush_at(80, 80, (40, 40), start);
    let pressed = window.colour(40, 40)[0];
    let let_go = start + AIRBRUSH_STILL_NS / 2;
    window.run_until(let_go);
    let to = window.screen_of((40, 40));
    window.pointer_at(InputEvent::PointerMoved { to }, let_go);
    window.pointer_at(
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
        let_go,
    );
    assert!(window.colour(40, 40)[0] < pressed);
}

/// A sweep the window is told of sparsely is sprayed evenly along its way: the
/// paint for the time between two reports falls where the pointer went, not
/// in a blob at each report.
#[test]
fn an_airbrush_sweep_reported_sparsely_lays_no_blob_at_its_reports() {
    let start = 1_000_000_000;
    let gap = 80_000_000;
    let mut window = airbrush_at(130, 40, (10, 20), start);
    let mut at_ns = start;
    for x in (30..=120).step_by(20) {
        at_ns += gap;
        window.run_until(at_ns);
        let to = window.screen_of((x, 20));
        window.pointer_at(InputEvent::PointerMoved { to }, at_ns);
    }
    window.pointer_at(
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
        at_ns,
    );
    let laid: Vec<u8> = (30..=90).map(|x| 255 - window.colour(x, 20)[0]).collect();
    let (least, most) = (
        laid.iter().min().copied().unwrap_or(0),
        laid.iter().max().copied().unwrap_or(0),
    );
    assert!(least > 0, "painted all along: {laid:?}");
    assert!(most - least <= 3, "{laid:?}");
}

#[test]
fn a_paste_floats_until_it_is_put_down() {
    let mut window = Window::white(30, 30);
    let pasted = Canvas::new(4, 4, Kind::Rgba, Sample::Rgba([9, 9, 9, 255])).expect("fits");
    let outcome = window
        .view
        .pasted(Ok((pasted, Kind::Rgba)), &mut Region::new());
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
fn the_window_menu_offers_the_layer_and_page_rows_with_their_fields() {
    let fields = |window: &Window| -> vec::Vec<u16> {
        window
            .view
            .menu(MenuKind::Window)
            .rows()
            .filter_map(|(row, _)| match row {
                AppMenuRowView::Item(item) => item.entry.map(|entry| entry.id.get()),
                _ => None,
            })
            .collect()
    };
    let window = Window::white(10, 10);
    assert_eq!(fields(&window), [GO_TO_LAYER], "one picture has no pages");
    assert_eq!(fields(&tiff_pages(2)), [GO_TO_LAYER, GO_TO_ENTRY]);
    let zoom = window.view.menu(MenuKind::Zoom);
    assert_eq!(zoom.rows().count(), crate::viewport::ZOOMS.len());
}

/// Nothing a picture window offers is left out for want of room in the
/// bounds a menu is carried in: a TIFF's pages hold every command but the
/// sprite file window's and a sprite window's own, which a sprite window
/// offers.
#[test]
fn the_window_menu_holds_every_command() {
    let offered = |window: &Window| -> vec::Vec<u16> {
        let menu = window.view.menu(MenuKind::Window);
        assert!(menu.len() < tairix_abi::window_ipc::APP_MENU_MAX_TOTAL_ROWS);
        menu.rows()
            .filter_map(|(row, _)| match row {
                AppMenuRowView::Item(item) => Some(item.id.get()),
                _ => None,
            })
            .collect()
    };
    let ids = offered(&tiff_pages(2));
    let theirs = [Action::EditEntry, Action::SpriteFile];
    let every = super::PLAIN_ACTIONS
        .into_iter()
        .filter(|action| !theirs.contains(action))
        .chain(Tool::ALL.map(Action::Tool))
        .chain((0..Op::COUNT).map(Action::Adjust))
        .chain(PaneKind::ALL.map(Action::Pane));
    for action in every {
        assert!(ids.contains(&action.id()), "{action:?} is offered");
    }
    assert!(!ids.contains(&Action::SpriteFile.id()));
    let sprite_window = offered(&sprite_window());
    assert!(sprite_window.contains(&Action::SpriteFile.id()));
    assert!(
        !sprite_window.contains(&Action::NewEntry.id()),
        "no list of its own"
    );
}

/// A row keeps the window menu up under a secondary click exactly where its
/// action is done to the picture in place.
#[test]
fn the_window_menu_stays_up_for_every_action_done_in_place() {
    use tairix_window::menu::MenuCommand;

    let check = |window: &Window| {
        let menu = window.view.menu(MenuKind::Window);
        let mut kept = 0;
        for (row, _) in menu.rows() {
            let AppMenuRowView::Item(item) = row else {
                continue;
            };
            let action = Action::from_id(item.id.get()).expect("a row names an action");
            assert_eq!(item.repeatable, action.acts_in_place(), "{action:?}");
            kept += usize::from(item.repeatable);
        }
        assert!(kept > 0);
    };
    check(&tiff_pages(2));
    check(&sprite_window());
    for action in [
        Action::Undo,
        Action::ZoomIn,
        Action::RotateLeft,
        Action::Copy,
    ] {
        assert!(action.acts_in_place(), "{action:?}");
    }
    for action in [
        Action::Open,
        Action::Resize,
        Action::EditPrimary,
        Action::DeleteLayer,
        Action::FloatPalette,
    ] {
        assert!(!action.acts_in_place(), "{action:?}");
    }
}

/// A TIFF of `count` white pages.
fn tiff_pages(count: usize) -> Window {
    let page = || {
        Entry::Picture(Picture::plain(
            Canvas::new(6, 4, Kind::Rgba, Sample::Rgba([255; 4])).expect("fits"),
        ))
    };
    let document = Document::of(
        (0..count).map(|_| page()).collect(),
        Origin::Read(tairix_sandbox::imagerender::ViewFormat::Tiff),
        Unkept::default(),
        SaveSettings::default(),
    )
    .expect("entries");
    Window::new(document)
}

/// A sprite window over the sprite `one` of the file `Sprites`.
fn sprite_window() -> Window {
    let file = Document::of(
        vec![sprite("one", 10), sprite("two", 20)],
        Origin::Read(tairix_sandbox::imagerender::ViewFormat::Sprite),
        Unkept::default(),
        SaveSettings::default(),
    )
    .expect("entries");
    let id = file.ids()[0];
    let Some(Entry::Picture(picture)) = file.entries().first() else {
        panic!("a picture");
    };
    let document = Document::of(
        vec![Entry::Picture(picture.try_clone().expect("room"))],
        Origin::Read(tairix_sandbox::imagerender::ViewFormat::Sprite),
        Unkept::default(),
        SaveSettings::default(),
    )
    .expect("one sprite");
    let view = View::sprite_window(
        document,
        (String::from("Sprites"), false),
        (id, SpriteName::new("one").expect("a name")),
        Access::Writable,
    );
    Window::over(view)
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

#[test]
fn pages_are_stepped_through_and_gone_to_by_number() {
    let mut window = tiff_pages(2);
    window.key(Key::Named(NamedKey::PageDown), plain());
    assert_eq!(window.view.document().current(), 1);
    let go_to = |window: &mut Window, text: &str| {
        let id = AppMenuItemId::new(GO_TO_ENTRY).expect("an id");
        let outcome = window.view.entered(id, text, &mut Region::new());
        window.apply(&outcome);
    };
    go_to(&mut window, "1");
    assert_eq!(window.view.document().current(), 0);
    go_to(&mut window, "9");
    assert_eq!(
        window.view.message(),
        Some("There is no page of that number")
    );
}

/// A sprite window is titled by its sprite and the file it is in, marked as
/// that file is.
#[test]
fn a_sprite_window_is_titled_by_its_sprite_and_its_file() {
    let mut window = sprite_window();
    let mut title = String::new();
    window.view.write_title(&mut title);
    assert_eq!(title, "one in Sprites \u{2014} Paint");
    let leaf = SpriteName::new("leaf").expect("a name");
    assert!(window.view.set_member(leaf, "Sprites", true));
    window.view.write_title(&mut title);
    assert_eq!(title, "*leaf in Sprites \u{2014} Paint");
    assert!(!window.view.set_member(leaf, "Sprites", true));
    // Saved, its file holds no changes.
    assert!(window.view.set_member(leaf, "Sprites", false));
    window.view.write_title(&mut title);
    assert_eq!(title, "leaf in Sprites \u{2014} Paint");
    // Saved under another name, it is in that file.
    assert!(window.view.set_member(leaf, "Leaves", false));
    window.view.write_title(&mut title);
    assert_eq!(title, "leaf in Leaves \u{2014} Paint");
}

/// A sprite window hands its sprite to its file once a change, and never
/// asks whether to save: the file window does.
#[test]
fn a_sprite_window_hands_each_change_to_its_file_and_closes_unasked() {
    let mut window = sprite_window();
    assert!(window.view.take_edit().is_none(), "nothing changed yet");
    window.act(Action::Tool(Tool::Pencil));
    window.drag((1, 1), (1, 1));
    assert!(window.view.take_edit().is_some());
    assert!(window.view.take_edit().is_none(), "handed once");
    let outcome = window.view.close_requested(&mut Region::new());
    assert!(matches!(outcome.request, Some(Request::Close)));
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
fn closing_with_changes_asks_the_host_to_ask_first() {
    let mut window = Window::white(10, 10);
    let outcome = window.view.close_requested(&mut Region::new());
    assert!(matches!(outcome.request, Some(Request::Close)));
    window.act(Action::Tool(Tool::Pencil));
    window.drag((1, 1), (1, 1));
    let outcome = window.view.close_requested(&mut Region::new());
    assert!(matches!(outcome.request, Some(Request::AskToClose)));
    assert!(
        !window.view.asking(),
        "the question is the host's, not the window's"
    );
}

/// A quit is never left waiting on a question that is not part of closing.
#[test]
fn a_form_open_over_the_picture_does_not_hold_a_quit() {
    let mut window = Window::white(10, 10);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((1, 1), (1, 1));
    window.act(Action::NewPicture);
    assert!(window.view.asking());
    let outcome = window.view.close_requested(&mut Region::new());
    assert!(matches!(outcome.request, Some(Request::AskToClose)));
    assert!(!window.view.asking_to_close());
}

/// Saving before the window closes puts the floating selection down first,
/// as any save does, and a quit waits on that while it is under way.
#[test]
fn saving_before_closing_puts_the_floating_selection_down_first() {
    let mut window = Window::white(60, 60);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((10, 10), (10, 10));
    window.lift((8, 8), (12, 12), (20, 10));
    let putting_down = window.view.save_then_close(&mut Region::new());
    assert!(
        window.view.asking_to_close(),
        "a quit waits on the put-down"
    );
    let saving = window.run_worker(putting_down);
    assert!(window.view.floating().is_none(), "down");
    assert_eq!(window.colour(30, 20), [0, 0, 0, 255]);
    assert!(matches!(saving.request, Some(Request::SaveThenClose)));
    assert!(!window.view.asking_to_close());
}

/// An operation open with its settings moved is a change a close asks about,
/// and Save there applies it before it saves; one opened and never moved is
/// none.
#[test]
fn an_operation_moved_counts_as_unsaved_and_saving_before_closing_applies_it() {
    let mut window = greys();
    window.act(Action::Adjust(op_index("Posterise")));
    assert!(
        !SavedDocument::is_modified(&window.view),
        "opened, never moved"
    );
    press_on(&mut window, PaneKind::Adjustments, Spot::Number(900));
    assert!(
        SavedDocument::is_modified(&window.view),
        "its settings moved"
    );
    let closing = window.view.close_requested(&mut Region::new());
    assert!(
        matches!(closing.request, Some(Request::AskToClose)),
        "{:?}",
        closing.request
    );
    window.settle_previews();
    let saving = window.view.save_then_close(&mut Region::new());
    assert!(
        matches!(saving.request, Some(Request::SaveThenClose)),
        "{:?}",
        saving.request
    );
    assert_eq!(
        window.view.document().history_depth(),
        1,
        "applied before it saves"
    );
}

/// Work out that changes the picture once it lands — a fill — is a change a
/// close asks about; a selection being made is none.
#[test]
fn work_out_on_the_picture_counts_as_unsaved_and_a_selection_does_not() {
    let mut window = Window::white(40, 30);
    marking_with(&mut window, Marquee::Ellipse);
    let selecting = window.drag((5, 5), (24, 14));
    assert!(window.view.busy());
    assert!(
        !SavedDocument::is_modified(&window.view),
        "a selection changes no pixel"
    );
    window.run_worker(selecting);
    window.act(Action::Tool(Tool::Fill));
    let at = window.screen_of((15, 10));
    window.move_to(at);
    window.press(PointerButton::Primary);
    assert!(window.view.busy());
    assert!(SavedDocument::is_modified(&window.view), "a fill out");
}

/// An Apply still being worked out is a change a close asks about, and Save
/// there waits for it to land, a quit waiting with it, before it saves.
#[test]
fn saving_before_closing_waits_for_an_apply_still_being_worked_out() {
    let grey = Sample::Rgba([101, 101, 101, 255]);
    let canvas = Canvas::new(1200, 900, Kind::Rgba, grey).expect("fits");
    let mut window = Window::new(Document::new(Picture::plain(canvas)));
    window.act(Action::Adjust(op_index("Posterise")));
    press_on(&mut window, PaneKind::Adjustments, Spot::Number(900));
    let first = window.wanted_preview().expect("asked");
    window.serve_preview(first);
    let applying = press_on(&mut window, PaneKind::Adjustments, Spot::Apply);
    assert!(window.view.busy(), "completed on the queue");
    let closing = window.view.close_requested(&mut Region::new());
    assert!(
        matches!(closing.request, Some(Request::AskToClose)),
        "{:?}",
        closing.request
    );
    let waiting = window.view.save_then_close(&mut Region::new());
    assert!(waiting.request.is_none(), "nothing saved before it lands");
    assert!(window.view.asking_to_close(), "a quit waits on it");
    let saving = window.run_worker(applying);
    assert!(
        matches!(saving.request, Some(Request::SaveThenClose)),
        "{:?}",
        saving.request
    );
    assert_eq!(
        window.view.document().history_depth(),
        1,
        "the Apply landed first"
    );
}

/// Closing a sprite window applies the operation moved in it first, as it
/// puts down what floats: its sprite is its file's, and nothing is asked.
#[test]
fn closing_a_sprite_window_applies_its_operation_first() {
    let mut window = sprite_window();
    window.act(Action::Adjust(op_index("Brightness & contrast")));
    press_on(&mut window, PaneKind::Adjustments, Spot::Number(900));
    let closing = window.view.close_requested(&mut Region::new());
    assert!(
        matches!(closing.request, Some(Request::Close)),
        "{:?}",
        closing.request
    );
    assert_eq!(window.view.document().history_depth(), 1, "applied first");
    assert!(window.view.take_edit().is_some(), "and handed to its file");
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
    let outcome = window.dialog_key(Key::Named(NamedKey::Enter));
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

/// A form is asked in a dialog of the window's own, drawn beside the window
/// and its floating panes, holding the window until it is answered; the
/// window's own input is never filtered for it.
#[test]
fn a_form_is_asked_in_a_dialog_of_its_own_that_holds_the_window() {
    let mut window = Window::white(10, 10);
    window.act(Action::NewPicture);
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let wanted = window
        .view
        .dialog_window(Scale::ONE, theme)
        .expect("the form asks for a dialog");
    assert_eq!(wanted.modality, DialogModality::OwnerModal);
    assert!(tairix_window::docapp::fits_beside(
        wanted.rect,
        window.view.layout().window()
    ));
    for slot in window.view.layout().floating() {
        assert!(slot.frame.intersection(&wanted.rect).is_empty());
    }
    window.key(Key::Char('e'), plain());
    assert_eq!(
        window.view.tool(),
        Tool::Eraser,
        "the window's key is its own"
    );
    assert!(window.view.asking(), "and leaves the form up");
    window.dialog_key(Key::Named(NamedKey::Escape));
    assert!(!window.view.asking(), "Escape in the dialog turns it down");
    assert!(window.view.dialog_window(Scale::ONE, theme).is_none());
}

#[test]
fn save_as_asks_how_first_and_then_where_in_the_format_chosen() {
    let mut window = Window::white(10, 10);
    let mut damage = Region::new();
    let surveying = window.view.ask_how(false, &mut damage);
    window.run_worker(surveying.expect("asked"));
    assert!(window.view.asking());
    assert!(!window.view.asking_to_close());
    let outcome = window.dialog_key(Key::Named(NamedKey::Enter));
    assert!(matches!(
        outcome.request,
        Some(Request::SaveWhere { then_close: false })
    ));
    assert_eq!(window.view.offered_extension(), "png");
    let surveying = window.view.ask_how(true, &mut damage);
    window.run_worker(surveying.expect("asked"));
    assert!(window.view.asking_to_close(), "a quit waits on the sheet");
    window.dialog_key(Key::Named(NamedKey::Escape));
    assert!(!window.view.asking() && !window.view.asking_to_close());
}

/// A Save As that lands leaves the document typed as the format its sheet
/// chose — a PNG made a sprite file called plain "photo" — so a later save to
/// that file is held to that format, and the sheet's choice is spent.
///
/// Regression: the rename after the save forgot the file's type, so a save to
/// "photo" was checked as the picture's own format.
#[test]
fn a_landed_save_as_types_the_document_as_its_chosen_format() {
    use tairix_window::document::SavedDocument as _;
    let mut window = Window::white(10, 10);
    window.view.save_as = Some(SaveFormat::Sprites);
    window.view.rename(alloc::string::String::from("photo"));
    assert_eq!(window.view.recorded, tairix_abi::fs::Filetype::new(0xFF9));
    assert_eq!(window.view.save_as, None, "the choice is spent");
    assert_eq!(window.view.save_format("photo"), Ok(SaveFormat::Sprites));
}

/// A Save As that never lands — its picker cancelled, or its save refused —
/// leaves the document's own file as it was: a later save to it is checked in
/// the format the file's name says, not the one the abandoned sheet chose.
///
/// Regression: the sheet's choice was kept for every later save, so saving
/// "photo.png" after a Save As as GIF was cancelled was checked as a GIF.
#[test]
fn a_save_as_that_did_not_land_leaves_the_files_own_format() {
    let mut window = Window::white(10, 10);
    window.view.name = alloc::string::String::from("photo.png");
    window.view.save_as = Some(SaveFormat::Gif);
    assert_eq!(window.view.save_format("photo.png"), Ok(SaveFormat::Png));
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
            window.now,
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
        .pixel_at(at, window.size(), window.view.layout().canvas());
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
        window.now,
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
        viewport.pixel_at(at, window.size(), window.view.layout().canvas()),
        Some((30, 40)),
        "the pixel under the pinch stays under it"
    );
    // The fingers' centre moves: the picture follows it, toward the far
    // edges, where it has room to go.
    let moved = Point::new(at.x - 25, at.y - 15);
    assert!(window.view.layout().canvas().contains(moved));
    window.pointer(pinching(PinchPhase::Update, PINCH_SCALE_ONE * 3 / 2, moved));
    assert_eq!(
        window
            .view
            .viewport()
            .pixel_at(moved, window.size(), window.view.layout().canvas()),
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
    assert!(!window.view.layout().canvas().contains(off));
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
    let mut view = Window::white(40, 40).view;
    let mut damage = Region::new();
    let outcome = view.act(Action::Tool(Tool::Pencil), &mut damage);
    assert_eq!(outcome.relayout, Relayout::Reported);
    let canvas = view.layout().canvas();
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
    let outcome = window.view.act(Action::Tool(Tool::Pencil), &mut damage);
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
        .computed(job, compute(work, &SERIAL), &mut damage);
    let canvas = window.view.layout().canvas();
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
        Scale::ONE,
        theme,
        &mut damage,
    );
    assert!(!damage.is_empty(), "the squared preview is drawn");
    let mut again = Region::new();
    let _ = window.view.on_pointer(
        &InputEvent::ModifiersChanged { modifiers: shift },
        window.now,
        Scale::ONE,
        theme,
        &mut again,
    );
    assert!(
        again.is_empty(),
        "modifiers restated unchanged draw nothing"
    );
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
    let outcome = window
        .view
        .computed(job, Computed::Tiles(Err(OutOfMemory)), &mut Region::new());
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
        .computed(job, compute(work, &SERIAL), &mut Region::new());
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
        .pasted(Ok((pasted, palette)), &mut Region::new());
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
    let over = popup.intersection(&window.view.layout().canvas());
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
        .cell_rect(window.view.layout().swatches(), clear)
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
    let secondary = window.view.layout().secondary_well();
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
    let convert = window.dialog_key(Key::Named(NamedKey::Enter));
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
        .tool_rect(
            tool_index(tool),
            window.view.layout().tools(),
            Scale::ONE,
            theme,
        )
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
        .tool_rect(index, window.view.layout().view_strip(), Scale::ONE, theme)
        .expect("the command is seated")
}

fn well_rect(window: &Window, index: usize) -> Rect {
    window
        .view
        .swatches
        .cell_rect(window.view.swatch_bounds(), index)
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
    let bar = window.view.layout().bar();
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
    let status = window.view.layout().message();
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

/// A 256-colour palette is one row longer than the strip: the wheel scrolls
/// it along, and a well scrolled into view is chosen where it is shown.
#[test]
fn a_long_palette_scrolls_along_its_strip() {
    let kind = Kind::Indexed {
        depth: IndexDepth::Eight,
        palette: tairix_image::desktop_palette(IndexDepth::Eight)
            .iter()
            .map(|&[r, g, b]| [r, g, b, 255])
            .collect(),
        masked: false,
    };
    let canvas = Canvas::new(8, 8, kind, Sample::Index(0, 255)).expect("fits");
    let mut window = Window::new(Document::new(Picture::plain(canvas)));
    assert_eq!(window.view.wells.len(), 256);
    assert_eq!(window.view.swatches.rows(), 1, "one row");
    assert!(!window.view.layout().palette_bar().is_empty());
    let before = window.view.swatch_bounds().left();
    window.move_to(window.view.layout().wells_view().center());
    for _ in 0..400 {
        window.pointer(InputEvent::PointerScrolled {
            dx: 0,
            dy: SCROLL_UNITS_PER_DETENT,
        });
    }
    assert!(
        window.view.swatch_bounds().left() < before,
        "scrolled along"
    );
    let last = well_rect(&window, 255);
    assert_eq!(
        last.intersection(&window.view.layout().wells_view()),
        last,
        "the last well shows at the end"
    );
    click_on(&mut window, last);
    assert_eq!(window.view.inks().0, window.view.wells[255]);
}

/// The tool box, the view strip and the bar each carry their tips.
#[test]
fn tips_name_tools_commands_and_settings() {
    let mut window = Window::white(16, 16);
    let tip = |window: &Window| {
        let theme = window.registry.active();
        window
            .view
            .tool_tip(Scale::ONE, theme)
            .map(|(_, text)| text)
    };
    window.move_to(tool_rect(&window, Tool::Fill).center());
    assert_eq!(tip(&window), Some("Fill (F)"));
    window.move_to(command_rect(&window, ViewCommand::PixelGrid).center());
    assert_eq!(tip(&window), Some("Pixel grid (G)"));
    let size = window.view.layout().bar().control(0).expect("seated");
    window.move_to(size.center());
    assert_eq!(tip(&window), Some(Setting::Size.tip()));
}

/// The pointer shows text entry over a number field and the arrow while a
/// list holds it.
#[test]
fn the_pointer_shows_text_entry_over_a_number_field() {
    let mut window = Window::white(16, 16);
    let size = window
        .view
        .layout()
        .bar()
        .control(0)
        .expect("seated")
        .center();
    let canvas = window.screen_of((4, 4));
    let cursor = |window: &Window, at| DocumentView::cursor(&window.view, at);
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
    let smooth = window.view.layout().bar().control(1).expect("seated");
    window.move_to(smooth.center());
    let theme = window.registry.active();
    let tip = window.view.tool_tip(Scale::ONE, theme);
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
        window.view.lay_out(
            width,
            height,
            theme,
            Scale::ONE,
            faces(theme),
            &mut Region::new(),
        );
        let least = window.view.layout();
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
    window.view.lay_out(
        900,
        300,
        theme,
        Scale::ONE,
        faces(theme),
        &mut Region::new(),
    );
    let tools = window.view.layout().tools();
    let offset = |window: &Window| {
        let theme = window.registry.active();
        window
            .view
            .tool_box
            .scroll_model(window.view.layout().tools(), Scale::ONE, theme)
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
    let outcome = window
        .view
        .on_key(stroke, Scale::ONE, theme, &mut Region::new());
    assert_eq!(outcome.relayout, Relayout::Reported);
    window.view.lay_out(
        900,
        300,
        theme,
        Scale::ONE,
        faces(theme),
        &mut Region::new(),
    );
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
    window.dialog_key(Key::Named(NamedKey::Enter));
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
    let at = window.view.layout().canvas().center();
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
    let at = window.view.layout().canvas().center();
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
    let area = window.view.layout().canvas();
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
    let canvas = window.view.layout().canvas();
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
        window.now,
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
    let edge = window.view.layout().canvas();
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

/// The Adjust menu's entry for the operation labelled `label`.
fn op_index(label: &str) -> usize {
    Op::every()
        .position(|op| op.label() == label)
        .expect("an operation")
}

/// Where `spot` on pane `kind` shows, its body scrolled to it first where its
/// dock scrolls the body.
fn spot_on(window: &mut Window, kind: PaneKind, spot: Spot) -> Point {
    let theme = window.registry.active();
    let at = window
        .view
        .operation_pane(kind)
        .spot(
            spot,
            window.view.layout().operation_settings(kind),
            (faces(theme), Scale::ONE, theme),
        )
        .expect("on the pane");
    let dot = Rect::new(at.x, at.y, 1, 1);
    window.view.reveal_body(kind, dot, &mut Region::new());
    let shown = window.view.body_view(kind).to_window(dot).expect("shown");
    Point::new(shown.left(), shown.top())
}

/// Press and let go on `spot` of pane `kind`, answering what asked for more:
/// the press or the release.
fn press_on(window: &mut Window, kind: PaneKind, spot: Spot) -> Outcome {
    let at = spot_on(window, kind, spot);
    window.move_to(at);
    let pressed = window.press(PointerButton::Primary);
    let released = window.release(PointerButton::Primary);
    if pressed.request.is_some() {
        pressed
    } else {
        released
    }
}

/// Choose in pane `kind`'s selector the operation `downs` past the one
/// chosen, as the keyboard does with its list open.
fn choose_in_selector(window: &mut Window, kind: PaneKind, downs: usize) -> Outcome {
    press_on(window, kind, Spot::Selector);
    assert!(
        window.view.operation_pane(kind).listing(),
        "the list opened"
    );
    for _ in 0..downs {
        window.key(Key::Named(NamedKey::Down), plain());
    }
    window.key(Key::Named(NamedKey::Enter), plain())
}

/// Every pixel of `canvas`, row by row.
fn every_pixel(canvas: &Canvas) -> Vec<[u8; 4]> {
    (0..canvas.height())
        .flat_map(|y| (0..canvas.width()).filter_map(move |x| canvas.colour_at(x, y)))
        .collect()
}

/// The tiles a preview request asks to be worked out.
fn tiles_asked(request: &Request) -> &[usize] {
    match request {
        Request::Own(Own::Preview {
            work: PreviewWork::Tiles { tiles, .. },
            ..
        }) => tiles,
        other => panic!("tiles, not {other:?}"),
    }
}

#[test]
fn an_operation_opens_in_its_pane_from_the_menu_and_leaves_the_window_live() {
    let mut window = Window::white(30, 30);
    let opened = window.act(Action::Adjust(op_index("Gaussian blur")));
    assert!(opened.request.is_none(), "the loop asks the preview worker");
    assert!(!window.view.asking(), "nothing modal");
    assert!(window.view.shows(PaneKind::Effects), "a blur is an effect");
    assert!(!window
        .view
        .layout()
        .operation_settings(PaneKind::Effects)
        .is_empty());
    assert_eq!(
        window.view.adjusting(),
        Some(Op::GaussianBlur { radius: 2 })
    );
    window.settle_previews();
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

/// Each row of the Adjust menu opens its own operation, at its starting
/// settings, in the pane of its family.
#[test]
fn every_adjust_row_opens_its_operation_in_its_own_pane() {
    for (index, op) in Op::every().enumerate() {
        let mut window = Window::white(20, 20);
        window.act(Action::Adjust(index));
        let pane = match op.family() {
            Family::Adjustments => PaneKind::Adjustments,
            Family::Effects => PaneKind::Effects,
        };
        assert!(window.view.shows(pane), "{op:?} shows its pane");
        assert_eq!(window.view.open_operation(), Some(pane), "{op:?}");
        assert_eq!(window.view.adjusting(), Some(op), "{op:?} as it starts");
    }
}

#[test]
fn the_preview_is_worked_out_in_view_first_then_the_rest() {
    let mut window = Window::white(1200, 900);
    window.act(Action::Adjust(op_index("Invert")));
    let total = window
        .view
        .document()
        .picture()
        .expect("a picture")
        .canvas()
        .tile_count();
    let first = window
        .wanted_preview()
        .expect("the tiles in view are asked");
    let seen = tiles_asked(&first).len();
    assert!(seen > 0 && seen < total, "{seen} of {total}: those in view");
    assert!(window.wanted_preview().is_none(), "one job out at a time");
    window.serve_preview(first);
    let rest = window.wanted_preview().expect("then the rest");
    assert_eq!(tiles_asked(&rest).len(), total - seen);
    window.serve_preview(rest);
    assert!(window.wanted_preview().is_none(), "the preview is whole");
    let shown = window.view.preview_canvas().expect("previewed");
    assert_eq!(
        shown.colour_at(1199, 899),
        Some([0, 0, 0, 255]),
        "out of view too"
    );
    assert_eq!(
        window.colour(1199, 899),
        [255; 4],
        "the picture itself untouched"
    );
}

/// A sample abandons the job asked before it: the answer that job still
/// sends is dropped, and the newest is asked at once.
#[test]
fn a_preview_asked_before_a_sample_is_abandoned_and_its_answer_dropped() {
    let grey = Sample::Rgba([101, 101, 101, 255]);
    let canvas = Canvas::new(64, 64, Kind::Rgba, grey).expect("fits");
    let mut window = Window::new(Document::new(Picture::plain(canvas)));
    window.act(Action::Adjust(op_index("Posterise")));
    let stale = window.wanted_preview().expect("asked");
    press_on(&mut window, PaneKind::Adjustments, Spot::Number(900));
    let Some(Op::Posterise { levels }) = window.view.adjusting() else {
        panic!("posterise open");
    };
    assert_ne!(levels, 4, "the sample moved it");
    let fresh = window
        .wanted_preview()
        .expect("the sample asks afresh at once");
    let shown = |window: &Window| {
        window
            .view
            .preview_canvas()
            .and_then(|shown| shown.colour_at(9, 9))
    };
    window.serve_preview(stale);
    assert_eq!(
        shown(&window),
        Some([101, 101, 101, 255]),
        "the stale answer is dropped"
    );
    window.serve_preview(fresh);
    let level = crate::tone::posterise_table(levels)[101];
    assert_eq!(
        shown(&window),
        Some([level, level, level, 255]),
        "the fresh one lands"
    );
}

#[test]
fn apply_adopts_a_whole_preview_as_one_step_and_its_undo_is_exact() {
    let mut window = Window::white(150, 100);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((10, 10), (140, 90));
    let picture = |window: &Window| {
        every_pixel(
            window
                .view
                .document()
                .picture()
                .expect("a picture")
                .canvas(),
        )
    };
    let before = picture(&window);
    window.act(Action::Adjust(op_index("Gaussian blur")));
    window.settle_previews();
    let shown = every_pixel(window.view.preview_canvas().expect("previewed"));
    assert_ne!(shown, before, "the blur shows");
    let depth = window.view.document().history_depth();
    let applied = press_on(&mut window, PaneKind::Effects, Spot::Apply);
    assert!(applied.request.is_none(), "the preview's own tiles land");
    assert_eq!(
        window.view.document().history_depth(),
        depth + 1,
        "as one step"
    );
    assert_eq!(picture(&window), shown, "what the preview showed");
    assert_eq!(window.view.adjusting(), None, "the pane back at its start");
    window.key(Key::Char('z'), ctrl());
    assert_eq!(picture(&window), before, "undone to the pixel");
}

#[test]
fn an_incomplete_preview_is_completed_on_the_queue_and_lands_as_one_step() {
    let mut window = Window::white(1200, 900);
    window.act(Action::Adjust(op_index("Invert")));
    let first = window.wanted_preview().expect("asked");
    window.serve_preview(first);
    let applying = press_on(&mut window, PaneKind::Adjustments, Spot::Apply);
    let Some(Request::Own(Own::Compute {
        work: super::Compute::Op { ref done, .. },
        ..
    })) = applying.request
    else {
        panic!("completed on the queue, not {:?}", applying.request);
    };
    assert!(!done.is_empty(), "what the preview worked out is kept");
    assert!(window.view.busy());
    assert!(
        window.view.preview_canvas().is_some(),
        "the preview shows until it lands"
    );
    window.run_worker(applying);
    assert_eq!(window.view.document().history_depth(), 1, "as one step");
    assert_eq!(window.colour(1199, 899), [0, 0, 0, 255], "out of view too");
    assert_eq!(window.view.adjusting(), None);
    window.key(Key::Char('z'), ctrl());
    assert_eq!(window.colour(1199, 899), [255; 4]);
}

#[test]
fn an_operation_reset_or_its_pane_closed_leaves_the_picture_as_it_was() {
    let mut window = Window::white(20, 20);
    window.act(Action::Adjust(op_index("Brightness & contrast")));
    assert!(window.wanted_preview().is_none(), "nothing to preview yet");
    press_on(&mut window, PaneKind::Adjustments, Spot::Number(100));
    window.settle_previews();
    assert!(window.view.preview_canvas().is_some());
    press_on(&mut window, PaneKind::Adjustments, Spot::Reset);
    assert!(
        window.view.preview_canvas().is_none(),
        "back where it started"
    );
    let close = band_point(
        &window,
        PaneKind::Adjustments,
        Some(WindowControlKind::Close),
    );
    click_at(&mut window, close);
    assert_eq!(window.view.adjusting(), None);
    assert!(!window.view.shows(PaneKind::Adjustments));
    assert_eq!(window.view.document().history_depth(), 0);
    assert_eq!(window.colour(5, 5), [255; 4]);
}

#[test]
fn a_palette_picture_is_adjusted_through_its_palette_and_refuses_an_effect() {
    let mut window = four_colours();
    let palette = window.palette();
    window.act(Action::Adjust(op_index("Desaturate")));
    assert!(
        window.view.preview_kind().is_some(),
        "a palette is previewed at once"
    );
    assert!(
        window.wanted_preview().is_none(),
        "on the loop, not a worker"
    );
    assert_eq!(
        window.palette(),
        palette,
        "the palette itself untouched until applied"
    );
    press_on(&mut window, PaneKind::Adjustments, Spot::Apply);
    let grey = window.palette();
    assert_ne!(grey, palette, "applied");
    assert!(
        grey.iter().all(|&[r, g, b, _]| r == g && g == b),
        "each entry its grey"
    );
    assert_eq!(window.view.document().history_depth(), 1);
    assert_eq!(window.view.adjusting(), None);
    window.act(Action::Adjust(op_index("Gaussian blur")));
    assert_eq!(window.view.adjusting(), None, "not opened");
    assert!(window
        .view
        .message()
        .is_some_and(|said| said.contains("colour picture")));
}

#[test]
fn a_preview_of_the_picture_before_an_undo_is_dropped_and_asked_again() {
    let mut window = Window::white(20, 20);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((5, 5), (5, 15));
    window.act(Action::Adjust(op_index("Gaussian blur")));
    let out = window.wanted_preview().expect("asked");
    window.act(Action::Undo);
    assert!(window.view.adjusting().is_some(), "undo leaves it open");
    window.serve_preview(out);
    let again = window
        .wanted_preview()
        .expect("asked again of the picture as it now stands");
    window.serve_preview(again);
    assert_eq!(
        window
            .view
            .preview_canvas()
            .and_then(|shown| shown.colour_at(5, 10)),
        Some([255; 4]),
        "worked from the picture after the undo, the stroke gone"
    );
}

#[test]
fn a_stroke_applies_the_operation_first_and_one_finding_it_unfinished_paints_nothing() {
    let grey = Sample::Rgba([101, 101, 101, 255]);
    let canvas = Canvas::new(1200, 900, Kind::Rgba, grey).expect("fits");
    let mut window = Window::new(Document::new(Picture::plain(canvas)));
    window.act(Action::Tool(Tool::Pencil));
    window.act(Action::Adjust(op_index("Posterise")));
    press_on(&mut window, PaneKind::Adjustments, Spot::Number(900));
    let first = window.wanted_preview().expect("asked");
    window.serve_preview(first);
    let applying = click(&mut window, (3, 3));
    assert_eq!(
        window.colour(3, 3),
        [101, 101, 101, 255],
        "the press painted nothing"
    );
    let Some(Request::Own(Own::Compute { .. })) = applying.request else {
        panic!(
            "the operation completed on the queue, not {:?}",
            applying.request
        );
    };
    window.run_worker(applying);
    assert_eq!(window.view.adjusting(), None, "applied and closed");
    assert_eq!(window.view.document().history_depth(), 1);
    let posterised = window.colour(3, 3);
    assert_ne!(posterised, [101, 101, 101, 255]);
    assert_eq!(
        window.colour(1199, 899),
        posterised,
        "the whole layer, as one step"
    );
    window.drag((3, 3), (3, 3));
    assert_eq!(window.colour(3, 3), [0, 0, 0, 255], "then the tool paints");
}

#[test]
fn preview_turned_off_shows_the_picture_and_on_again_shows_the_preview_at_once() {
    let mut window = Window::white(20, 20);
    window.act(Action::Tool(Tool::Pencil));
    window.drag((5, 5), (5, 15));
    window.act(Action::Adjust(op_index("Gaussian blur")));
    window.settle_previews();
    assert!(window.view.preview_canvas().is_some());
    press_on(&mut window, PaneKind::Effects, Spot::Preview);
    assert!(
        window.view.preview_canvas().is_none(),
        "the picture as it is"
    );
    assert!(window.wanted_preview().is_none(), "nothing asked while off");
    press_on(&mut window, PaneKind::Effects, Spot::Preview);
    assert!(window.view.preview_canvas().is_some(), "kept to compare");
    assert!(window.wanted_preview().is_none(), "and nothing asked again");
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
    window.act(Action::Adjust(op_index("Levels")));
    assert!(window.view.histogram().is_none());
    window.settle_previews();
    assert!(window.view.histogram().is_some());
    press_on(&mut window, PaneKind::Adjustments, Spot::Picker(3));
    let Some(Op::Levels(levels)) = window.view.adjusting() else {
        panic!("levels open");
    };
    let red = levels.of(crate::tone::Channel::Red);
    assert!(red.black >= 40 && red.white <= 167, "{red:?}");
    window.settle_previews();
    assert!(window.view.preview_canvas().is_some());
    press_on(&mut window, PaneKind::Adjustments, Spot::Apply);
    assert_eq!(window.colour(0, 0)[0], 0, "the darkest grey is black");
}

/// A histogram is a reading of the picture, whichever adjustment reads it, so
/// moving from Levels to Curves keeps it rather than asking for it again.
#[test]
fn moving_between_adjustments_that_read_a_histogram_keeps_it() {
    let mut window = greys();
    window.act(Action::Adjust(op_index("Levels")));
    window.settle_previews();
    let held = window.view.histogram().cloned();
    assert!(held.is_some());

    window.act(Action::Adjust(op_index("Curves")));

    assert_eq!(window.view.histogram().cloned(), held, "the reading stands");
    assert!(
        window.wanted_preview().is_none(),
        "and nothing is worked out again"
    );
}

/// Moving on while the preview worker reads the histogram leaves its job to
/// land on the adjustment now open, rather than abandoning it and asking for
/// another.
#[test]
fn a_histogram_being_read_when_the_adjustment_moves_lands_on_the_next() {
    let mut window = greys();
    window.act(Action::Adjust(op_index("Levels")));
    let reading = window.wanted_preview().expect("the histogram is asked");

    window.act(Action::Adjust(op_index("Curves")));
    assert!(
        window.wanted_preview().is_none(),
        "the one job out is waited for"
    );
    window.serve_preview(reading);

    assert!(window.view.histogram().is_some(), "it lands on Curves");
    assert!(
        window.wanted_preview().is_none(),
        "and is not asked for again"
    );
}

#[test]
fn an_eyedropper_takes_the_colour_beneath_the_preview() {
    let canvas = Canvas::new(10, 10, Kind::Rgba, Sample::Rgba([30, 20, 10, 255])).expect("fits");
    let mut window = Window::new(Document::new(Picture::plain(canvas)));
    window.act(Action::Adjust(op_index("Levels")));
    window.settle_previews();
    press_on(&mut window, PaneKind::Adjustments, Spot::Picker(0));
    let picking = |window: &Window| window.view.operation_pane(PaneKind::Adjustments).picking();
    assert_eq!(picking(&window), Some(crate::op_pane::Pick::Black));
    click(&mut window, (4, 4));
    let Some(Op::Levels(levels)) = window.view.adjusting() else {
        panic!("levels open");
    };
    assert_eq!(levels.of(crate::tone::Channel::Green).black, 20);
    assert_eq!(picking(&window), None, "put down");
    window.settle_previews();
    assert_eq!(window.view.document().history_depth(), 0, "nothing painted");
}

/// Choosing another operation applies the one left only where its settings
/// were moved and it changes something: looking down the list applies
/// nothing, an untouched Invert included.
#[test]
fn leaving_an_operation_applies_it_only_where_it_was_moved() {
    let mut window = greys();
    window.act(Action::Adjust(op_index("Invert")));
    window.settle_previews();
    let switched = choose_in_selector(&mut window, PaneKind::Adjustments, 1);
    assert!(switched.request.is_none());
    assert_eq!(
        window.view.document().history_depth(),
        0,
        "an untouched invert is dropped"
    );
    assert!(matches!(
        window.view.adjusting(),
        Some(Op::Posterise { .. })
    ));
    press_on(&mut window, PaneKind::Adjustments, Spot::Number(900));
    window.settle_previews();
    let shown = every_pixel(window.view.preview_canvas().expect("previewed"));
    let switched = choose_in_selector(&mut window, PaneKind::Adjustments, 1);
    assert!(switched.request.is_none(), "the whole preview adopted");
    assert_eq!(window.view.document().history_depth(), 1, "as one step");
    let picture = window.view.document().picture().expect("a picture");
    assert_eq!(every_pixel(picture.canvas()), shown);
    assert!(matches!(
        window.view.adjusting(),
        Some(Op::Threshold { .. })
    ));
}

/// One preview runs across both panes: opening an operation in the other
/// pane settles the open one, applying it where it was moved.
#[test]
fn opening_an_operation_in_the_other_pane_settles_the_open_one() {
    let mut window = greys();
    window.act(Action::Adjust(op_index("Posterise")));
    press_on(&mut window, PaneKind::Adjustments, Spot::Number(900));
    window.settle_previews();
    window.act(Action::Adjust(op_index("Pixelate")));
    assert_eq!(
        window.view.document().history_depth(),
        1,
        "the posterise applied as one step"
    );
    assert_eq!(window.view.open_operation(), Some(PaneKind::Effects));
    assert!(!window.view.operation_pane(PaneKind::Adjustments).is_open());
    window.act(Action::Adjust(op_index("Brightness & contrast")));
    assert_eq!(
        window.view.document().history_depth(),
        1,
        "an untouched pixelate is dropped"
    );
    assert_eq!(window.view.open_operation(), Some(PaneKind::Adjustments));
}

/// A window gone abandons the preview it asked: its job stops at the next
/// block it would start.
#[test]
fn a_window_closed_abandons_the_preview_it_asked() {
    let mut window = Window::white(64, 64);
    window.act(Action::Adjust(op_index("Invert")));
    let Some(Request::Own(Own::Preview { job, stamp, work })) = window.wanted_preview() else {
        panic!("a preview asked");
    };
    drop(window);
    let asked = PreviewJob {
        window: 0,
        job,
        stamp,
        work,
    };
    let answer = crate::preview::serve(asked, &SERIAL, &|| false);
    assert!(
        matches!(answer.previewed, Previewed::Tiles(Err(Halted::Abandoned))),
        "nothing is left to show it"
    );
}

#[test]
fn severe_memory_pressure_drops_the_preview_until_the_next_sample() {
    let mut window = greys();
    window.act(Action::Adjust(op_index("Posterise")));
    window.settle_previews();
    let pressed = |window: &mut Window, band| {
        window.view.adopt_pressure(band, &mut Region::new());
    };
    pressed(&mut window, PressureBand::Moderate);
    assert!(
        window.view.preview_canvas().is_some(),
        "kept short of severe"
    );
    pressed(&mut window, PressureBand::Severe);
    assert!(window.view.preview_canvas().is_none(), "let go");
    window.move_to(Point::new(1, 1));
    assert!(
        window.wanted_preview().is_none(),
        "not made again by any input"
    );
    press_on(&mut window, PaneKind::Adjustments, Spot::Number(900));
    assert!(
        window.wanted_preview().is_some(),
        "the next sample asks again"
    );
}

#[test]
fn a_refused_preview_is_asked_again_by_the_next_input_never_at_once() {
    let mut window = Window::white(64, 64);
    window.act(Action::Adjust(op_index("Invert")));
    let Some(Request::Own(Own::Preview { job, .. })) = window.wanted_preview() else {
        panic!("a preview asked");
    };
    window.view.preview_refused(job);
    assert!(window.wanted_preview().is_none(), "not at once");
    window.move_to(Point::new(2, 2));
    assert!(
        window.wanted_preview().is_some(),
        "the next input asks again"
    );
}

/// A preview the worker ran short of memory for is asked again only once
/// something it rests on moves — the memory pressure falling, the window laid
/// out anew, a sample — never for the pointer merely moving.
#[test]
fn a_preview_short_of_memory_waits_for_what_could_let_it_fit() {
    let mut window = greys();
    window.act(Action::Adjust(op_index("Posterise")));
    press_on(&mut window, PaneKind::Adjustments, Spot::Number(900));
    let short = |window: &mut Window| {
        let Some(Request::Own(Own::Preview { job, stamp, .. })) = window.wanted_preview() else {
            panic!("a preview asked");
        };
        let answer = PreviewAnswer {
            window: 0,
            job,
            ticket: stamp.value(),
            previewed: Previewed::Tiles(Err(Halted::OutOfMemory)),
        };
        window.view.previewed(answer, &mut Region::new());
    };
    let pressed = |window: &mut Window, band| {
        window.view.adopt_pressure(band, &mut Region::new());
    };
    short(&mut window);
    window.move_to(Point::new(2, 2));
    assert!(window.wanted_preview().is_none(), "not for a hover");
    pressed(&mut window, PressureBand::Moderate);
    assert!(window.wanted_preview().is_none(), "nor for pressure rising");
    pressed(&mut window, PressureBand::Mild);
    short(&mut window);
    window.relayout();
    short(&mut window);
    press_on(&mut window, PaneKind::Adjustments, Spot::Number(500));
    assert!(window.wanted_preview().is_some(), "a sample asks again");
}

/// Given less room than shows it whole, an operation's pane is laid out whole
/// and scrolled through its body, and a press reaches a part scrolled into
/// view.
#[test]
fn an_operation_given_less_room_scrolls_within_its_pane_and_works_through_it() {
    let mut window = layered(&[([128, 128, 128, 255], 255); 40], 0);
    with_layers_pane(&mut window);
    window.act(Action::Adjust(op_index("Levels")));
    let slot = *window
        .view
        .layout()
        .pane(PaneKind::Adjustments)
        .expect("docked");
    assert!(!slot.bar.is_empty(), "the pane's body scrolls");
    assert!(
        window
            .view
            .layout()
            .operation_settings(PaneKind::Adjustments)
            .height
            > slot.body.height,
        "laid out whole"
    );
    press_on(&mut window, PaneKind::Adjustments, Spot::Apply);
    assert!(
        window.view.body_offset(PaneKind::Adjustments) > 0,
        "scrolled to Apply, at its foot"
    );
    assert_eq!(
        window.view.adjusting(),
        None,
        "the press reached Apply through the scroll"
    );
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
    let asked = window.view.entered(id, "2", &mut Region::new());
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

/// A merge or a flatten handed the pool lays its layers together across it,
/// to the bit as on one core.
#[test]
fn a_merge_is_laid_together_across_the_pool_it_is_handed() {
    let layers = || {
        let ground =
            crate::document::Layer::new(crate::effect::patterned(150, 100), String::from("ground"));
        let mut faint =
            crate::document::Layer::new(crate::effect::patterned(150, 100), String::from("faint"));
        faint.opacity = 90;
        vec![ground, faint]
    };
    let across = tairix_parallel::Reversed::new(4);
    let Computed::Composed(Ok(spread)) =
        compute(super::Compute::Compose { layers: layers() }, &across)
    else {
        panic!("laid together");
    };
    assert!(across.widest() > 1, "a tile a piece across the pool");
    let Computed::Composed(Ok(serial)) =
        compute(super::Compute::Compose { layers: layers() }, &SERIAL)
    else {
        panic!("laid together");
    };
    assert_eq!(spread, serial);
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
        let outcome = window
            .view
            .transform(convert, "change its colours", &mut Region::new());
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
    let asked = window
        .view
        .transform(convert, "change its colours", &mut Region::new());
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

/// Where pane `kind`'s band seats `control`, or the middle of the span it is
/// dragged by.
fn band_point(window: &Window, kind: PaneKind, control: Option<WindowControlKind>) -> Point {
    let slot = window.view.layout().pane(kind).expect("shown");
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
    let canvas = window.view.layout().canvas();
    assert!(ticked(&window, PaneKind::Colour));
    let close = band_point(&window, PaneKind::Colour, Some(WindowControlKind::Close));
    click_at(&mut window, close);
    assert!(!window.view.shows(PaneKind::Colour));
    assert!(window.view.layout().pane(PaneKind::Colour).is_none());
    assert!(window.view.layout().dock_on(Side::Right).rect.is_empty());
    assert!(
        window.view.layout().canvas().width > canvas.width,
        "the canvas takes its room"
    );
    assert!(!ticked(&window, PaneKind::Colour));
    window.act(Action::Pane(PaneKind::Colour));
    assert_eq!(
        window.view.arrangement().place(PaneKind::Colour),
        Some((Side::Right, 0))
    );
    assert_eq!(window.view.layout().canvas(), canvas);
}

#[test]
fn a_pane_rolls_up_to_its_band_and_opens_again() {
    let mut window = Window::white(10, 10);
    let roll = band_point(&window, PaneKind::Tools, Some(WindowControlKind::Minimize));
    click_at(&mut window, roll);
    let slot = window
        .view
        .layout()
        .pane(PaneKind::Tools)
        .expect("still shown");
    assert!(slot.body.is_empty());
    assert!(window.view.layout().tool_box().is_empty());
    assert!(window.view.shows(PaneKind::Tools));
    let roll = band_point(&window, PaneKind::Tools, Some(WindowControlKind::Minimize));
    click_at(&mut window, roll);
    assert!(!window.view.layout().tool_box().is_empty());
}

#[test]
fn a_band_dragged_over_the_other_dock_marks_where_it_lands_and_lands_there() {
    let mut window = Window::white(10, 10);
    let grip = band_point(&window, PaneKind::Tools, None);
    window.move_to(grip);
    window.press(PointerButton::Primary);
    let right = window.view.layout().dock_on(Side::Right).rect;
    let under_colour = Point::new(right.center().x, right.bottom() - 2);
    window.move_to(under_colour);
    let drag = window.view.pane_drag.expect("dragging");
    assert_eq!(drag.kind, PaneKind::Tools);
    assert_eq!(
        drag.landing,
        Some(super::Landing::Dock {
            side: Side::Right,
            before: 1
        })
    );
    let over_colour = Point::new(right.center().x, right.top() + 4);
    window.move_to(over_colour);
    assert_eq!(
        window.view.pane_drag.and_then(|drag| drag.landing),
        Some(super::Landing::Dock {
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
    assert!(window.view.layout().dock_on(Side::Left).rect.is_empty());
    let tools = window.view.layout().pane(PaneKind::Tools).expect("shown");
    let colour = window.view.layout().pane(PaneKind::Colour).expect("shown");
    assert!(
        tools.frame.bottom() < colour.frame.top(),
        "above the colour pane"
    );
}

#[test]
fn a_band_dragged_to_the_edge_of_an_empty_dock_lands_in_it() {
    let mut window = Window::white(10, 10);
    window.act(Action::Pane(PaneKind::Tools));
    assert!(window.view.layout().dock_on(Side::Left).rect.is_empty());
    let grip = band_point(&window, PaneKind::Colour, None);
    window.move_to(grip);
    window.press(PointerButton::Primary);
    let middle = window.view.layout().canvas().center().y;
    window.move_to(Point::new(2, middle));
    assert_eq!(
        window.view.pane_drag.and_then(|drag| drag.landing),
        Some(super::Landing::Dock {
            side: Side::Left,
            before: 0
        })
    );
    window.release(PointerButton::Primary);
    assert_eq!(
        window.view.arrangement().place(PaneKind::Colour),
        Some((Side::Left, 0))
    );
    assert!(window.view.layout().dock_on(Side::Right).rect.is_empty());
}

#[test]
fn a_pane_drag_holds_the_pointer_and_one_let_go_away_from_a_dock_floats_where_it_went() {
    let mut window = Window::white(10, 10);
    window.act(Action::Tool(Tool::Pencil));
    let grip = band_point(&window, PaneKind::Tools, None);
    let band = window
        .view
        .layout()
        .pane(PaneKind::Tools)
        .expect("docked")
        .header;
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
    let band = window
        .view
        .layout()
        .pane(PaneKind::Colour)
        .expect("docked")
        .header;
    let edge = Point::new(to_i32(WINDOW.0) - 1, grip.y + 40);
    let outcome = drag_band(&mut window, PaneKind::Colour, edge);
    assert_eq!(outcome.relayout, Relayout::Whole);
    assert!(
        window.view.pane_drag.is_none(),
        "the press is the tool window's now"
    );
    assert!(window.view.arrangement().floats(PaneKind::Colour));
    assert!(window.view.layout().dock_on(Side::Right).rect.is_empty());
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
    let wanted = window.view.tool_window(0).expect("one tool window");
    assert_eq!((wanted.id, wanted.title), (id, "Colour"));
    assert!(wanted
        .rect
        .intersection(&window.view.layout().window())
        .is_empty());
    assert_eq!(
        window.view.layout().pane_window(PaneKind::Colour),
        wanted.rect
    );
    assert!(wanted.rect.contains(window.view.layout().picker().center()));
    assert!(window.view.tool_window(1).is_none());
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
    let left = window.view.layout().window().left() + 2;
    let middle = window.view.layout().canvas().center().y;
    let moved = |over, ended| ToolMove { id, over, ended };
    let mut damage = Region::new();
    let theme = window.registry.active();
    let outcome = window.view.tool_moved(
        moved(Some(Point::new(left, middle)), false),
        Scale::ONE,
        theme,
        &mut damage,
    );
    assert_eq!(outcome.relayout, Relayout::None);
    assert_eq!(
        window.view.landing(),
        Some(super::Landing::Dock {
            side: Side::Left,
            before: 0
        })
    );
    assert!(!damage.is_empty(), "the mark is drawn");
    let outcome = window
        .view
        .tool_moved(moved(None, false), Scale::ONE, theme, &mut damage);
    assert_eq!(outcome.relayout, Relayout::None);
    assert_eq!(
        window.view.landing(),
        None,
        "off the window, nothing is marked"
    );
    let outcome = window.view.tool_moved(
        moved(Some(Point::new(left, middle)), true),
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
    assert!(window.view.tool_window(0).is_none(), "its tool window goes");
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
        &mut Region::new(),
    );
    window.apply(&outcome);
    assert!(!window.view.shows(PaneKind::Colour));
    let outcome = window.view.tool_gone(
        super::tool_id(PaneKind::Tools),
        ToolGone::Refused,
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
    window.act(Action::Pane(PaneKind::Adjustments));
    for kind in PaneKind::ALL {
        window.view.panes.float(kind);
    }
    window.relayout();
    let slots = window.view.layout().floating();
    assert_eq!(slots.len(), PaneKind::ALL.len());
    for (index, slot) in slots.iter().enumerate() {
        assert!(slot
            .frame
            .intersection(&window.view.layout().window())
            .is_empty());
        assert!(slot.header.is_empty(), "the window manager draws its band");
        for other in &slots[index + 1..] {
            assert!(slot.frame.intersection(&other.frame).is_empty());
        }
    }
    let tools = window.view.tool_opening(super::tool_id(PaneKind::Tools));
    let colour = window.view.tool_opening(super::tool_id(PaneKind::Colour));
    let below = window.view.layout().top().bottom();
    assert_eq!(
        tools,
        ToolOpening {
            offset: (0, below),
            carry: None
        }
    );
    let width = window
        .view
        .layout()
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
        let right = window.view.layout().dock_on(Side::Right).rect;
        window.move_to(right.center());
        assert!(window.view.pane_drag.is_some());
        if lose_focus {
            window.view.focus_changed(false, &mut Region::new());
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
    let start = window.view.layout().clone();
    window.act(Action::Pane(PaneKind::Adjustments));
    window.act(Action::Pane(PaneKind::Colour));
    let roll = band_point(&window, PaneKind::Tools, Some(WindowControlKind::Minimize));
    click_at(&mut window, roll);
    window.act(Action::ResetPanes);
    assert_eq!(
        window.view.arrangement(),
        &crate::pane::Arrangement::default()
    );
    assert_eq!(window.view.layout(), &start);
}

/// Where control `cell` of part `part` of the colour pane's panel is.
fn colour_control(window: &Window, part: usize, cell: usize) -> Point {
    let theme = window.registry.active();
    window
        .view
        .colour_controls
        .place_of(
            part,
            window.view.layout().colour_controls(),
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
    let recents = window.view.recents_rect();
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
    window.view.adopt(&preferences, &mut Region::new());
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
    window.view.adopt(&preferences, &mut Region::new());
    assert!(!window.view.pixel_grid_shown(), "never");
}

#[test]
fn a_style_change_repaints_only_what_it_draws_differently() {
    use crate::preferences::{Preferences, Surround};
    use tairix_colour::Rgb;
    let mut window = Window::white(16, 16);
    let canvas = window.view.layout().canvas();
    let picture = window
        .view
        .viewport()
        .to_screen(Bounds::picture(16, 16), (16, 16), canvas);
    assert!(
        !picture.is_empty() && picture != canvas,
        "the surround shows"
    );
    let mut preferences = Preferences::default();
    let adopt = |window: &mut Window, preferences: &Preferences| {
        let mut damage = Region::new();
        window.view.adopt(preferences, &mut damage);
        damage
    };
    preferences.grid.shown = true;
    preferences.grid.snap = true;
    assert!(
        adopt(&mut window, &preferences).is_empty(),
        "flags draw nothing"
    );
    preferences.grid.colour = Rgb::new(1, 2, 3);
    assert!(
        adopt(&mut window, &preferences).is_empty(),
        "no grid shown here"
    );
    preferences.pixel_grid_from = 900;
    assert!(
        adopt(&mut window, &preferences).is_empty(),
        "shown at neither"
    );
    window.act(Action::Grid);
    preferences.grid.opacity = 900;
    assert_eq!(adopt(&mut window, &preferences).rects(), [picture]);
    preferences.checker_side = 4;
    assert_eq!(adopt(&mut window, &preferences).rects(), [picture]);
    preferences.pixel_grid_from = 100;
    assert_eq!(adopt(&mut window, &preferences).rects(), [picture]);
    preferences.surround = Surround::Chosen(Rgb::new(9, 9, 9));
    let damage = adopt(&mut window, &preferences);
    assert!(
        !damage.intersects(picture),
        "the picture is not the surround"
    );
    let area = |rect: &Rect| u64::from(rect.width) * u64::from(rect.height);
    assert_eq!(
        damage.rects().iter().map(area).sum::<u64>(),
        area(&canvas) - area(&picture),
        "every pixel about the picture",
    );
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

/// A well pressed and let go off the strip is let go: a release over it
/// later, with no press, chooses nothing.
#[test]
fn a_well_pressed_and_let_go_elsewhere_is_let_go() {
    let mut window = Window::white(16, 16);
    let inks = window.view.inks();
    let red = well_rect(&window, 11);
    window.move_to(red.center());
    window.press(PointerButton::Primary);
    window.move_to(window.view.layout().message().center());
    window.release(PointerButton::Primary);
    assert_eq!(window.view.inks(), inks);
    window.move_to(red.center());
    window.release(PointerButton::Primary);
    assert_eq!(window.view.inks(), inks, "a release alone is no choice");
}

/// A palette of the desktop's 256 colours, which one row of the strip
/// cannot show at once.
fn long_palette() -> Window {
    let kind = Kind::Indexed {
        depth: IndexDepth::Eight,
        palette: tairix_image::desktop_palette(IndexDepth::Eight)
            .iter()
            .map(|&[r, g, b]| [r, g, b, 255])
            .collect(),
        masked: false,
    };
    let canvas = Canvas::new(8, 8, kind, Sample::Index(0, 255)).expect("fits");
    Window::new(Document::new(Picture::plain(canvas)))
}

/// The strip scrolls to the ink that changed, and repaints its bar's thumb
/// as it does.
#[test]
fn the_strip_scrolls_to_the_ink_that_changed_and_repaints_its_bar() {
    let mut window = long_palette();
    window.view.secondary = window.view.wells[255];
    let mut damage = Region::new();
    window
        .view
        .inks_changed(tairix_controls::SwatchMark::Secondary, &mut damage);
    let last = well_rect(&window, 255);
    assert_eq!(
        last.intersection(&window.view.layout().wells_view()),
        last,
        "the secondary's well shows"
    );
    let bar = window.view.layout().palette_bar();
    assert!(
        damage
            .rects()
            .iter()
            .any(|rect| rect.intersection(&bar) == bar),
        "its bar is repainted"
    );
}

/// The button at the strip's right end floats the palette on a press and a
/// release over it: its tool window, titled as the pane, opens against the
/// canvas's lower right corner, and its tip says what it does.
#[test]
fn the_pop_out_floats_the_palette_over_the_canvas_corner() {
    let mut window = Window::white(16, 16);
    let pop_out = window.view.layout().pop_out();
    window.move_to(pop_out.center());
    let theme = window.registry.active();
    let tip = window.view.tool_tip(Scale::ONE, theme).map(|(_, tip)| tip);
    assert_eq!(tip, Some("Float the palette in a window of its own"));
    window.press(PointerButton::Primary);
    assert!(
        !window.view.panes().floats(PaneKind::Palette),
        "a press alone floats nothing"
    );
    let outcome = window.release(PointerButton::Primary);
    assert_eq!(outcome.relayout, Relayout::Whole);
    assert!(window.view.panes().floats(PaneKind::Palette));
    assert!(window.view.layout().pop_out().is_empty());
    let wanted = window.view.tool_window(0).expect("its tool window");
    assert_eq!(
        (wanted.id, wanted.title),
        (super::tool_id(PaneKind::Palette), "Palette")
    );
    let (id, frame) = (wanted.id, wanted.rect);
    assert!(frame
        .intersection(&window.view.layout().window())
        .is_empty());
    let canvas = window.view.layout().canvas();
    let opening = window.view.tool_opening(id);
    assert_eq!(
        opening,
        ToolOpening {
            offset: (
                canvas.right() - to_i32(frame.width),
                canvas.bottom() - to_i32(frame.height)
            ),
            carry: None,
        }
    );
}

/// Floating, the palette's wells set both inks as they do in its strip.
#[test]
fn the_floating_palette_sets_both_inks() {
    let mut window = Window::white(16, 16);
    window.act(Action::FloatPalette);
    let red = well_rect(&window, 11);
    assert!(
        red.intersection(&window.view.layout().window()).is_empty(),
        "in its tool window"
    );
    click_on(&mut window, red);
    assert_eq!(window.view.inks().0, window.view.wells[11]);
    let blue = well_rect(&window, 8);
    window.move_to(blue.center());
    window.press(PointerButton::Middle);
    window.release(PointerButton::Middle);
    assert_eq!(window.view.inks().1, window.view.wells[8]);
}

/// A floating palette's grid is walked by the arrows a row at a time.
#[test]
fn the_arrows_walk_the_floating_palette_by_rows() {
    let mut window = Window::white(16, 16);
    window.act(Action::FloatPalette);
    window.act(Action::Tool(Tool::Pencil));
    window.key(Key::Named(NamedKey::Tab), plain());
    assert!(window.view.swatches.state().focus.focused);
    let black = window
        .view
        .swatches
        .selected()
        .expect("black is a Wimp colour");
    window.key(Key::Named(NamedKey::Down), plain());
    assert_eq!(
        window.view.inks().0,
        window.view.wells[black + 4],
        "a row of four down"
    );
    window.key(Key::Named(NamedKey::Up), plain());
    assert_eq!(window.view.inks().0, window.view.wells[black]);
}

/// Moved over the window's foot, a floating palette's landing there is
/// marked, a dock down a side is no landing of its, and let go there it docks
/// in its strip, closing its tool window.
#[test]
fn a_palette_moved_over_its_strip_docks_there() {
    let mut window = Window::white(10, 10);
    window.act(Action::FloatPalette);
    let id = super::tool_id(PaneKind::Palette);
    let foot = Point::new(
        window.view.layout().canvas().center().x,
        window.view.layout().status().top() - 2,
    );
    let over_dock = window.view.layout().dock_on(Side::Right).rect.center();
    let moved = |over, ended| ToolMove { id, over, ended };
    let theme = window.registry.active();
    let mut damage = Region::new();
    let outcome = window
        .view
        .tool_moved(moved(Some(foot), false), Scale::ONE, theme, &mut damage);
    assert_eq!(outcome.relayout, Relayout::None);
    assert_eq!(window.view.landing(), Some(super::Landing::Strip));
    assert!(!damage.is_empty(), "the mark is drawn");
    let outcome = window.view.tool_moved(
        moved(Some(over_dock), false),
        Scale::ONE,
        theme,
        &mut damage,
    );
    assert_eq!(outcome.relayout, Relayout::None);
    assert_eq!(window.view.landing(), None, "a side's dock is not its");
    let outcome = window
        .view
        .tool_moved(moved(Some(foot), true), Scale::ONE, theme, &mut damage);
    window.apply(&outcome);
    assert!(window.view.panes().in_strip());
    assert!(window.view.tool_window(0).is_none(), "its tool window goes");
}

/// Closed, a floating palette's tool window docks the palette in its strip —
/// it is never lost — as a refused one does.
#[test]
fn closing_the_floating_palette_docks_it_in_the_strip() {
    let mut window = Window::white(10, 10);
    let strip = window.view.layout().palette();
    for why in [ToolGone::Closed, ToolGone::Refused] {
        window.act(Action::FloatPalette);
        let outcome =
            window
                .view
                .tool_gone(super::tool_id(PaneKind::Palette), why, &mut Region::new());
        window.apply(&outcome);
        assert!(window.view.panes().in_strip(), "{why:?}");
        assert_eq!(window.view.layout().palette(), strip, "{why:?}");
    }
}

/// View ▸ Panes floats the palette and docks it again, marked while it
/// floats, and its Palette row hides it.
#[test]
fn the_float_palette_row_toggles_and_is_marked() {
    let mut window = Window::white(10, 10);
    let marked = |window: &Window| {
        window
            .view
            .menu(MenuKind::Window)
            .rows()
            .any(|(row, _)| match row {
                AppMenuRowView::Item(item) => {
                    item.id.get() == Action::FloatPalette.id()
                        && item.mark == tairix_abi::window_ipc::AppMenuMark::Check
                }
                _ => false,
            })
    };
    assert!(!marked(&window) && ticked(&window, PaneKind::Palette));
    window.act(Action::FloatPalette);
    assert!(window.view.panes().floats(PaneKind::Palette));
    assert!(marked(&window) && ticked(&window, PaneKind::Palette));
    window.act(Action::FloatPalette);
    assert!(window.view.panes().in_strip() && !marked(&window));
    window.act(Action::Pane(PaneKind::Palette));
    assert!(!window.view.shows(PaneKind::Palette));
    assert!(
        window.view.layout().palette().is_empty(),
        "it claims no strip"
    );
}

/// Floating, the palette's window follows its wells: a mask taken away takes
/// its clear ink's row with it.
#[test]
fn a_mask_change_resizes_the_floating_palette() {
    let mut window = four_colours();
    window.act(Action::FloatPalette);
    let before = window.view.layout().palette();
    let row = window.view.layout().swatches().height / 2;
    let outcome = window.act(Action::RemoveMask);
    window.run_worker(outcome);
    assert!(window.view.panes().floats(PaneKind::Palette));
    assert_eq!(
        window.view.layout().swatches().height,
        row,
        "one row of four"
    );
    assert_eq!(window.view.layout().palette().height, before.height - row);
}

/// Held on its arrow, the palette bar goes on scrolling the strip.
#[test]
fn a_held_palette_arrow_goes_on_scrolling() {
    let mut window = long_palette();
    let theme = window.registry.active();
    let arrow = window
        .view
        .palette_bar
        .part_rect(
            tairix_controls::ScrollPart::Increment,
            window.view.layout().palette_bar(),
            Scale::ONE,
            theme,
        )
        .expect("its forward arrow");
    window.move_to(arrow.center());
    window.press(PointerButton::Primary);
    let stepped = window.view.palette_bar.model().range().offset();
    assert!(stepped > 0, "the press steps once");
    window.run_until(window.now + 1_000_000_000);
    assert!(
        window.view.palette_bar.model().range().offset() > stepped,
        "and the hold steps on"
    );
    window.release(PointerButton::Primary);
}

/// A torn-out Effects pane keeps the name of the effect open in it, on its
/// band and on its tool window.
#[test]
fn a_torn_out_adjustment_keeps_its_name() {
    let mut window = Window::white(30, 30);
    window.act(Action::Adjust(op_index("Gaussian blur")));
    assert_eq!(
        window.view.header(PaneKind::Effects).title(),
        "Gaussian blur"
    );
    let grip = band_point(&window, PaneKind::Effects, None);
    drag_band(
        &mut window,
        PaneKind::Effects,
        Point::new(to_i32(WINDOW.0) - 1, grip.y + 40),
    );
    assert!(window.view.arrangement().floats(PaneKind::Effects));
    assert_eq!(
        window.view.header(PaneKind::Effects).title(),
        "Gaussian blur"
    );
    let wanted = window.view.tool_window(0).expect("its tool window");
    assert_eq!(wanted.title, "Gaussian blur");
}

/// A change of ink repaints the wells its marks leave and reach, and none of
/// the rest of the palette.
#[test]
fn an_ink_change_repaints_only_the_wells_its_marks_move_between() {
    let mut window = Window::white(16, 16);
    let untouched = well_rect(&window, 11);
    let mut damage = Region::new();
    window.view.act(Action::SwapColours, &mut damage);
    for well in [0, 7] {
        let mut uncovered = Region::new();
        uncovered.add(well_rect(&window, well));
        uncovered.subtract_region(&damage);
        assert!(uncovered.is_empty(), "well {well} is repainted");
    }
    assert!(!damage.intersects(untouched), "a swap");
    window.move_to(window.dock_point((40, 30)));
    let theme = window.registry.active();
    let mut damage = Region::new();
    window.view.on_pointer(
        &InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        window.now,
        Scale::ONE,
        theme,
        &mut damage,
    );
    window.release(PointerButton::Primary);
    assert!(!damage.intersects(untouched), "a colour picked in the dock");
}

/// The middle button chooses a well as the primary does: on its release over
/// the well it was pressed on, never on the press alone.
#[test]
fn the_middle_button_chooses_a_well_on_its_release_over_it() {
    let mut window = Window::white(16, 16);
    let before = window.view.inks().1;
    let (blue, red) = (well_rect(&window, 8), well_rect(&window, 11));
    window.move_to(blue.center());
    window.press(PointerButton::Middle);
    assert_eq!(
        window.view.inks().1,
        before,
        "a press alone chooses nothing"
    );
    window.move_to(red.center());
    window.release(PointerButton::Middle);
    assert_eq!(window.view.inks().1, before, "let go over another well");
    window.press(PointerButton::Middle);
    window.release(PointerButton::Middle);
    assert_eq!(window.view.inks().1, window.view.wells[11]);
}

/// A secondary press on a floating pane's tool window opens the window's
/// menu where it was pressed, as one anywhere in the window does.
#[test]
fn a_secondary_press_on_a_tool_window_opens_the_window_menu() {
    let mut window = Window::white(10, 10);
    window.view.panes.float(PaneKind::Tools);
    window.relayout();
    let at = window
        .view
        .layout()
        .pane(PaneKind::Tools)
        .expect("floating")
        .frame
        .center();
    window.move_to(at);
    let outcome = window.press(PointerButton::Secondary);
    let Some(Request::Menu {
        kind: MenuKind::Window,
        anchor,
    }) = outcome.request
    else {
        panic!("the window's menu, not {:?}", outcome.request);
    };
    assert_eq!((anchor.left(), anchor.top()), (at.x, at.y));
}

/// The floating palette's tool window as the view asks for it: its
/// rectangle and how it may be sized.
fn palette_window(window: &Window) -> (Rect, WindowSizing) {
    let wanted = (0..)
        .map_while(|place| window.view.tool_window(place))
        .find(|wanted| wanted.id == super::tool_id(PaneKind::Palette))
        .expect("the palette's tool window");
    (wanted.rect, wanted.sizing)
}

/// Resize the floating palette's tool window to `size`, as its edges do, and
/// lay the window out again as the host does.
fn resize_palette(window: &mut Window, size: (u32, u32)) -> (Outcome, Region) {
    let mut damage = Region::new();
    let outcome = window.view.tool_resized(
        super::tool_id(PaneKind::Palette),
        size,
        Scale::ONE,
        &mut damage,
    );
    window.apply(&outcome);
    (outcome, damage)
}

/// A white picture's window, its palette floating in a tool window resized
/// to `across` wells and `down` rows of them.
fn sized_palette(across: u32, down: u32) -> Window {
    let mut window = Window::white(16, 16);
    window.act(Action::FloatPalette);
    let gap = window.registry.active().metrics().control_gap;
    let well = window.view.layout().palette_well();
    let _ = resize_palette(
        &mut window,
        (across * well + 2 * gap, down * well + 2 * gap),
    );
    window
}

/// Resized by its edges, the floating palette keeps that size, sets its wells
/// out again to fit it, and repaints within its own tool window alone.
#[test]
fn a_resized_palette_keeps_its_size_and_reflows_within_its_own_window() {
    let mut window = Window::white(16, 16);
    window.act(Action::FloatPalette);
    let (opened, sizing) = palette_window(&window);
    assert!(sizing.resizable(), "a floating palette is resizable");
    let gap = window.registry.active().metrics().control_gap;
    let well = window.view.layout().palette_well();
    let size = (6 * well + 2 * gap, 3 * well + 2 * gap);
    let (outcome, damage) = resize_palette(&mut window, size);
    assert_eq!(outcome.relayout, Relayout::Reported);
    assert_eq!(damage.bounds(), opened, "its own tool window");
    assert!(
        !damage.intersects(window.view.layout().window()),
        "not the window"
    );
    let (resized, _) = palette_window(&window);
    assert_eq!((resized.width, resized.height), size, "it keeps its size");
    assert_eq!(window.view.layout().columns(), 6);
    assert_eq!(window.view.swatches.columns(), 6, "six wells across");
    window.relayout();
    assert_eq!(palette_window(&window).0, resized, "laid out again, kept");
}

/// Docked in its strip again, the palette forgets the size its tool window
/// was given, and floats again at its own.
#[test]
fn a_docked_palette_forgets_the_size_it_was_given() {
    let mut fresh = Window::white(16, 16);
    fresh.act(Action::FloatPalette);
    let own = palette_window(&fresh).0;
    let mut window = sized_palette(6, 3);
    assert_ne!(palette_window(&window).0, own);
    window.act(Action::FloatPalette);
    assert!(window.view.panes().in_strip());
    window.act(Action::FloatPalette);
    assert_eq!(palette_window(&window).0, own, "it opens at its own size");
}

/// A pane floating at its own size is never resized: told it was, it keeps
/// its size and repaints nothing.
#[test]
fn a_fixed_tool_window_takes_no_size_of_its_own() {
    let mut window = Window::white(10, 10);
    window.view.panes.float(PaneKind::Tools);
    window.relayout();
    let slot = *window
        .view
        .layout()
        .pane(PaneKind::Tools)
        .expect("floating");
    assert!(!slot.sizing.resizable());
    let mut damage = Region::new();
    let outcome = window.view.tool_resized(
        super::tool_id(PaneKind::Tools),
        (300, 300),
        Scale::ONE,
        &mut damage,
    );
    assert_eq!(outcome.relayout, Relayout::None);
    assert!(damage.is_empty());
    window.relayout();
    let after = window
        .view
        .layout()
        .pane(PaneKind::Tools)
        .expect("floating")
        .frame;
    assert_eq!(after, slot.frame);
}

/// The size a palette was resized to is kept in logical pixels: at twice the
/// scale its tool window is twice as large.
#[test]
fn a_resized_palette_keeps_its_size_across_a_change_of_scale() {
    let mut window = sized_palette(6, 3);
    let (at_one, _) = palette_window(&window);
    let double = Scale::from_percent(200).expect("a scale");
    let theme = window.registry.active();
    window.view.lay_out(
        WINDOW.0 * 2,
        WINDOW.1 * 2,
        theme,
        double,
        Faces::of(theme, double),
        &mut Region::new(),
    );
    let frame = window
        .view
        .layout()
        .pane(PaneKind::Palette)
        .expect("floating")
        .frame;
    assert_eq!(
        (frame.width, frame.height),
        (at_one.width * 2, at_one.height * 2)
    );
}

/// The arrows walk a resized palette by the rows it reflowed to.
#[test]
fn up_and_down_walk_the_columns_a_resized_palette_reflowed_to() {
    let mut window = sized_palette(6, 3);
    window.act(Action::Tool(Tool::Pencil));
    window.key(Key::Named(NamedKey::Tab), plain());
    assert!(window.view.swatches.state().focus.focused);
    let black = window
        .view
        .swatches
        .selected()
        .expect("black is a Wimp colour");
    window.key(Key::Named(NamedKey::Down), plain());
    assert_eq!(
        window.view.inks().0,
        window.view.wells[black + 6],
        "a row of six down"
    );
    window.key(Key::Named(NamedKey::Up), plain());
    assert_eq!(window.view.inks().0, window.view.wells[black]);
}

/// A palette resized too short for its wells scrolls down by the wheel, and
/// a press finds the well drawn under it.
#[test]
fn a_short_palette_scrolls_down_by_the_wheel_and_a_press_finds_the_well_drawn_there() {
    let mut window = sized_palette(4, 2);
    assert_eq!(
        window.view.layout().columns(),
        3,
        "its bar takes a column's room"
    );
    assert!(
        !window.view.layout().palette_bar().is_empty(),
        "its wells overflow"
    );
    assert_eq!(
        window.view.palette_bar.orientation(),
        ScrollOrientation::Vertical
    );
    let view = window.view.layout().wells_view();
    window.move_to(view.center());
    window.pointer(InputEvent::PointerScrolled {
        dx: 0,
        dy: SCROLL_UNITS_PER_DETENT,
    });
    let offset = window.view.palette_bar.model().range().offset();
    assert!(offset > 0, "the wheel scrolls the wells down");
    let shown = window.view.swatch_bounds();
    let up = to_i32(u32::try_from(offset).expect("small"));
    assert_eq!(shown.top(), window.view.layout().swatches().top() - up);
    let under = window
        .view
        .swatches
        .well_at(shown, view.center())
        .expect("a well");
    window.press(PointerButton::Primary);
    window.release(PointerButton::Primary);
    assert_eq!(window.view.inks().0, window.view.wells[under]);
}

/// The arrows scroll a palette too short for its wells to the well they
/// reach.
#[test]
fn the_arrows_scroll_a_short_palette_to_the_well_they_reach() {
    let mut window = sized_palette(4, 2);
    window.act(Action::Tool(Tool::Pencil));
    window.key(Key::Named(NamedKey::Tab), plain());
    assert!(window.view.swatches.state().focus.focused);
    for _ in 0..3 {
        window.key(Key::Named(NamedKey::Down), plain());
    }
    let marked = window.view.swatches.selected().expect("a well marked");
    assert_eq!(marked, window.view.wells.len() - 1, "the last row");
    assert!(window.view.palette_bar.model().range().offset() > 0);
    let cell = well_rect(&window, marked);
    assert_eq!(
        cell.intersection(&window.view.layout().wells_view()),
        cell,
        "the well the arrows reached shows"
    );
}

/// The palette numbers its wells with RISC OS colour numbers: a colour
/// picture's are the Wimp colours, a palette picture's its own entries, and
/// the clear ink has none.
#[test]
fn the_palette_numbers_the_wimp_colours_or_the_entries_never_the_clear_ink() {
    // Each window is built in turn: three at once would not fit on the stack.
    let pictures: [(fn() -> Window, usize); 3] = [
        (|| Window::white(16, 16), 16),
        (four_colours, 4),
        (long_palette, 256),
    ];
    for (picture, numbered) in pictures {
        assert_eq!(picture().view.swatches.numbered(), numbered);
    }
}

/// The Layers pane shown in its column, its rows laid out.
fn with_layers_pane(window: &mut Window) {
    window.act(Action::Pane(PaneKind::Layers));
    assert!(
        !window.view.layout().layers().rows.is_empty(),
        "its rows show"
    );
}

/// Where the Layers pane shows the parts of layer `layer`'s row.
fn layer_row(window: &Window, layer: usize) -> crate::layers_pane::RowParts {
    let theme = window.registry.active();
    window
        .view
        .layers
        .parts_of(
            layer,
            window.view.layout().layers(),
            faces(theme),
            Scale::ONE,
        )
        .expect("its row shows")
}

/// A press on a row of the Layers pane paints on its layer, which is no step
/// to undo; its eye shows or hides its layer as one, painting staying put.
#[test]
fn picking_a_layers_row_takes_no_step_and_its_eye_one() {
    let mut window = layered(&[([255; 4], 255), ([0, 0, 0, 255], 255), ([9; 4], 255)], 2);
    with_layers_pane(&mut window);
    let at = layer_row(&window, 0).name.center();
    click_at(&mut window, at);
    assert_eq!(painted_on(&window), 0);
    assert_eq!(
        window.view.document().history_depth(),
        0,
        "picking is no step"
    );
    let at = layer_row(&window, 1).eye.center();
    click_at(&mut window, at);
    assert!(!layers_of(&window)[1].visible);
    assert_eq!(painted_on(&window), 0, "still painting on the bottom layer");
    assert_eq!(window.view.document().history_depth(), 1);
    window.key(Key::Char('z'), ctrl());
    assert!(layers_of(&window)[1].visible, "undone whole");
}

/// A click on the name of the layer painted on renames it in place, as one
/// step; a name left empty keeps the one it had.
#[test]
fn a_layer_renamed_in_its_row_is_one_step() {
    let mut window = layered(&[([255; 4], 255), ([0, 0, 0, 255], 255)], 1);
    with_layers_pane(&mut window);
    let name = layer_row(&window, 1).name.center();
    click_at(&mut window, name);
    assert_eq!(
        window.view.layers.renaming(),
        Some(layers_of(&window)[1].revision)
    );
    for ch in "Sky".chars() {
        window.key(Key::Char(ch), plain());
    }
    window.key(Key::Named(NamedKey::Enter), plain());
    assert_eq!(window.view.layers.renaming(), None);
    assert_eq!(layers_of(&window)[1].name, "Sky");
    assert_eq!(window.view.document().history_depth(), 1);
    click_at(&mut window, name);
    window.key(Key::Named(NamedKey::Backspace), plain());
    window.key(Key::Named(NamedKey::Enter), plain());
    assert_eq!(layers_of(&window)[1].name, "Sky", "an empty name keeps it");
    assert_eq!(window.view.document().history_depth(), 1);
    window.key(Key::Char('z'), ctrl());
    assert_eq!(layers_of(&window)[1].name, "layer");
}

/// A row dragged up the Layers pane moves its layer up the stack, as one
/// step.
#[test]
fn a_layers_row_dragged_moves_its_layer_as_one_step() {
    let mut window = layered(&[([255; 4], 255), ([0, 0, 0, 255], 255), ([9; 4], 255)], 0);
    with_layers_pane(&mut window);
    let from = layer_row(&window, 0).name.center();
    let top = layer_row(&window, 2).name;
    window.move_to(from);
    window.press(PointerButton::Primary);
    window.move_to(Point::new(from.x, top.top() + 1));
    window.release(PointerButton::Primary);
    let colours: Vec<_> = layers_of(&window)
        .iter()
        .map(|layer| layer.canvas.colour_at(0, 0))
        .collect();
    assert_eq!(
        colours,
        [Some([0, 0, 0, 255]), Some([9; 4]), Some([255; 4])],
        "the bottom layer now on top"
    );
    assert_eq!(window.view.document().history_depth(), 1);
}

/// An opacity or a palette entry being changed live is a change a quit asks
/// about: the window counts as modified while it stands away from where it
/// began, and closing lands it first so the question covers it.
#[test]
fn a_change_being_made_live_counts_as_unsaved() {
    let mut window = layered(&[([255; 4], 255), ([0, 0, 0, 255], 255)], 1);
    assert!(!SavedDocument::is_modified(&window.view));
    window.view.layer_opacity(40, false, &mut Region::new());
    assert!(
        SavedDocument::is_modified(&window.view),
        "an opacity moving"
    );
    window.view.layer_opacity(100, false, &mut Region::new());
    assert!(
        !SavedDocument::is_modified(&window.view),
        "back where it began"
    );

    let mut window = four_colours();
    window.move_to(window.dock_point((4, 4)));
    window.press(PointerButton::Primary);
    assert!(SavedDocument::is_modified(&window.view), "an entry moving");
    let closing = window.view.close_requested(&mut Region::new());
    assert_eq!(
        window.view.document().history_depth(),
        1,
        "landed before the question"
    );
    assert!(
        matches!(closing.request, Some(Request::AskToClose)),
        "{:?}",
        closing.request
    );
}

/// An opacity changed live in the Layers pane asks the preview worker for the
/// layers beneath laid together, takes no step while it moves, and lands as
/// one.
#[test]
fn an_opacity_changed_live_lands_as_one_step() {
    let mut window = layered(&[([255; 4], 255), ([0, 0, 0, 255], 255)], 1);
    window.view.layer_opacity(70, false, &mut Region::new());
    window.settle_previews();
    assert!(
        window.view.composite_under().is_some(),
        "kept for every sample"
    );
    window.view.layer_opacity(30, false, &mut Region::new());
    assert_eq!(
        window.view.document().history_depth(),
        0,
        "no step while it moves"
    );
    window.view.layer_opacity(30, true, &mut Region::new());
    assert_eq!(window.view.document().history_depth(), 1);
    assert!(window.view.composite_under().is_none(), "let go");
    assert_eq!(
        layers_of(&window)[1].opacity,
        tairix_colour::Fraction::from_percent(30).byte()
    );
    window.key(Key::Char('z'), ctrl());
    assert_eq!(layers_of(&window)[1].opacity, 255, "undone whole");
}

/// A press on the Layers pane's opacity field takes the keyboard from its
/// rows: a digit and Backspace typed there are the field's, never the
/// window's zoom or the clearing of the selection's pixels.
#[test]
fn a_press_on_the_layers_panes_opacity_takes_the_keyboard_from_its_rows() {
    let mut window = layered(&[([255; 4], 255), ([0, 0, 0, 255], 255)], 1);
    with_layers_pane(&mut window);
    window.act(Action::SelectAll);
    window.act(Action::ZoomIn);
    let zoom = window.view.viewport().percent();
    let back = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    window.key(Key::Named(NamedKey::Tab), back);
    assert!(window.view.layers.has_focus(), "the rows have the keyboard");
    let theme = window.registry.active();
    let look = (faces(theme), Scale::ONE, theme);
    let field = window
        .view
        .layers
        .opacity_field(window.view.layout().layers(), look)
        .expect("the opacity is seated");
    click_at(&mut window, field.center());
    let typed = window.key(Key::Char('1'), plain());
    let erased = window.key(Key::Named(NamedKey::Backspace), plain());
    assert!(
        typed.request.is_none() && erased.request.is_none(),
        "nothing asked of a worker"
    );
    assert!(!window.view.busy(), "the selection's pixels not cleared");
    assert_eq!(
        window.view.viewport().percent(),
        zoom,
        "the view keeps its zoom"
    );
    assert_eq!(painted_on(&window), 1);
}

/// Showing or hiding the layer from the Layers pane's rows while its opacity
/// is still moving — the slider held — lands that opacity first, so undoing
/// both puts back the opacity it began at.
#[test]
fn a_row_shown_while_its_opacity_moves_lands_the_opacity_first() {
    let mut window = layered(&[([255; 4], 255), ([0, 0, 0, 255], 255)], 1);
    with_layers_pane(&mut window);
    let back = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    window.key(Key::Named(NamedKey::Tab), back);
    window.view.layer_opacity(40, false, &mut Region::new());
    window.key(Key::Char(' '), plain());
    assert!(!layers_of(&window)[1].visible, "hidden from its row");
    window.view.layer_opacity(40, true, &mut Region::new());
    assert_eq!(
        window.view.document().history_depth(),
        2,
        "the opacity, then the eye"
    );
    window.key(Key::Char('z'), ctrl());
    window.key(Key::Char('z'), ctrl());
    let layer = &layers_of(&window)[1];
    assert_eq!(
        (layer.opacity, layer.visible),
        (255, true),
        "back where it began"
    );
}

/// Severe memory pressure lets the composite beneath an opacity changing go
/// with the preview, and it is not laid again while that opacity moves: each
/// paint lays every layer until it settles.
#[test]
fn severe_memory_pressure_lets_the_composite_beneath_an_opacity_go() {
    let mut window = layered(&[([255; 4], 255), ([0, 0, 0, 255], 255)], 1);
    window.view.layer_opacity(70, false, &mut Region::new());
    window.settle_previews();
    assert!(window.view.composite_under().is_some());
    window
        .view
        .adopt_pressure(PressureBand::Severe, &mut Region::new());
    assert!(window.view.composite_under().is_none(), "let go");
    window.view.layer_opacity(30, false, &mut Region::new());
    assert!(
        window.wanted_preview().is_none(),
        "not laid again while it moves"
    );
    // Severe pressure lets the history hold nothing, so the step is shown
    // recorded once the pressure falls.
    window
        .view
        .adopt_pressure(PressureBand::Normal, &mut Region::new());
    assert!(
        window.wanted_preview().is_none(),
        "nor once the pressure falls"
    );
    window.view.layer_opacity(30, true, &mut Region::new());
    assert_eq!(
        layers_of(&window)[1].opacity,
        tairix_colour::Fraction::from_percent(30).byte(),
        "it lands"
    );
    assert_eq!(window.view.document().history_depth(), 1, "one step still");
}

/// The Layers pane's thumbnails are drawn on the preview worker, one job at a
/// time, each kept by the revision of the layer it shows: a stroke asks again
/// for its own layer's alone.
#[test]
fn layer_thumbnails_are_drawn_on_the_preview_worker_by_revision() {
    let mut window = layered(&[([255; 4], 255), ([0, 0, 0, 255], 255)], 1);
    with_layers_pane(&mut window);
    let side = crate::layers_pane::thumb_side(Scale::ONE);
    let asked = window
        .wanted_preview()
        .expect("the rows' thumbnails are asked for");
    assert!(window.wanted_preview().is_none(), "one job at a time");
    window.serve_preview(asked);
    let revisions: Vec<u64> = layers_of(&window)
        .iter()
        .map(|layer| layer.revision)
        .collect();
    for &revision in &revisions {
        assert!(window.view.layer_thumb(revision, side).is_some(), "drawn");
    }
    assert!(window.wanted_preview().is_none(), "none asked twice");
    window.act(Action::Tool(Tool::Pencil));
    window.drag((2, 2), (2, 2));
    let stroked = layers_of(&window)[1].revision;
    assert_ne!(
        stroked, revisions[1],
        "the stroke moved its layer's revision"
    );
    assert_eq!(layers_of(&window)[0].revision, revisions[0], "and no other");
    let again = window.wanted_preview().expect("asked again");
    let Request::Own(Own::Preview {
        work: PreviewWork::LayerThumbnails { layers, .. },
        ..
    }) = &again
    else {
        panic!("the thumbnails, not {again:?}");
    };
    let wanted: Vec<u64> = layers.iter().map(|(revision, _)| *revision).collect();
    assert_eq!(wanted, [stroked]);
}

/// Thumbnails given up to make way for an Apply's completion are asked for
/// again, never taken for rows with nothing to draw.
#[test]
fn thumbnails_given_up_for_a_completion_are_asked_again() {
    let mut window = layered(&[([255; 4], 255), ([0, 0, 0, 255], 255)], 1);
    with_layers_pane(&mut window);
    let Some(Request::Own(Own::Preview { job, stamp, work })) = window.wanted_preview() else {
        panic!("the rows' thumbnails asked");
    };
    let asked = PreviewJob {
        window: 0,
        job,
        stamp,
        work,
    };
    let given_way = crate::preview::serve(asked, &SERIAL, &|| true);
    window.view.previewed(given_way, &mut Region::new());
    let again = window.wanted_preview();
    assert!(
        matches!(
            again,
            Some(Request::Own(Own::Preview {
                work: PreviewWork::LayerThumbnails { .. },
                ..
            }))
        ),
        "the thumbnails asked again, not {again:?}"
    );
}

/// Once the Layers pane shares its column, the Colour pane, given less than
/// it shows whole at, scrolls its own body: the wheel over it moves it, and
/// the picker Tabbed to is scrolled into view.
#[test]
fn the_colour_pane_scrolls_once_the_layers_pane_takes_its_room() {
    let mut window = layered(&[([255; 4], 255); 40], 0);
    with_layers_pane(&mut window);
    let slot = *window.view.layout().pane(PaneKind::Colour).expect("docked");
    assert!(!slot.bar.is_empty(), "the colour pane scrolls");
    assert!(slot.content.height > slot.body.height);
    window.move_to(slot.viewport().center());
    window.pointer(InputEvent::PointerScrolled {
        dx: 0,
        dy: SCROLL_UNITS_PER_DETENT,
    });
    assert!(
        window.view.body_offset(PaneKind::Colour) > 0,
        "the wheel scrolls it"
    );
    window.tab_into_dock();
    let picker = window.view.layout().picker();
    assert!(
        window
            .view
            .held_view(PaneKind::Colour, false)
            .to_window(picker)
            .is_some(),
        "the picker shows"
    );
}

/// Floating, the Layers pane's tool window sizes between its controls over
/// two rows and every row, and keeps the size it is given, the rows it
/// cannot show scrolling beneath its whole controls.
#[test]
fn a_floating_layers_pane_keeps_the_size_it_was_given() {
    let mut window = layered(&[([255; 4], 255); 8], 0);
    with_layers_pane(&mut window);
    window.view.panes.float(PaneKind::Layers);
    window.relayout();
    let slot = *window
        .view
        .layout()
        .pane(PaneKind::Layers)
        .expect("floating");
    assert!(slot.sizing.resizable());
    let (least, most) = (slot.sizing.min_height_px(), slot.sizing.max_height_px());
    assert!(least < most, "from two rows to all eight");
    let height = least + (most - least) / 2;
    let outcome = window.view.tool_resized(
        super::tool_id(PaneKind::Layers),
        (slot.frame.width, height),
        Scale::ONE,
        &mut Region::new(),
    );
    window.apply(&outcome);
    let resized = *window
        .view
        .layout()
        .pane(PaneKind::Layers)
        .expect("floating");
    assert_eq!(resized.frame.height, height, "the size it was given");
    let placed = *window.view.layout().layers();
    assert!(!placed.bar.is_empty(), "the rows it cannot show scroll");
    assert!(
        placed.panel.bottom() <= placed.rows.top(),
        "beneath its controls"
    );
}

/// A name being typed in the Layers pane lands before a menu's action, so
/// deleting its layer takes it with its new name and leaves nothing to land on
/// the layer that takes its place.
#[test]
fn a_name_being_typed_lands_before_a_menus_action() {
    let mut window = layered(&[([1; 4], 255), ([2; 4], 255), ([3; 4], 255)], 1);
    with_layers_pane(&mut window);
    let spot = layer_row(&window, 1).name.center();
    click_at(&mut window, spot);
    assert!(window.view.layers.renaming().is_some(), "renaming");
    for ch in "Sky".chars() {
        window.key(Key::Char(ch), plain());
    }
    let delete = AppMenuItemId::new(Action::DeleteLayer.id()).expect("an id");
    let outcome = window.view.chosen(delete, &mut Region::new());
    window.apply(&outcome);
    assert_eq!(window.view.layers.renaming(), None, "landed first");
    window.key(Key::Named(NamedKey::Enter), plain());
    let names: Vec<&str> = layers_of(&window)
        .iter()
        .map(|layer| layer.name.as_str())
        .collect();
    assert_eq!(names, ["layer", "layer"], "no other layer took its name");
    assert_eq!(
        window.view.document().history_depth(),
        2,
        "the rename, then the deletion"
    );
}

/// A name lands only on the layer it was begun on, found by its revision: once
/// that layer has gone, Enter renames none in its place.
#[test]
fn a_name_lands_on_its_own_layer_or_on_none() {
    let mut window = layered(&[([1; 4], 255), ([2; 4], 255), ([3; 4], 255)], 1);
    with_layers_pane(&mut window);
    let spot = layer_row(&window, 1).name.center();
    click_at(&mut window, spot);
    for ch in "Sky".chars() {
        window.key(Key::Char(ch), plain());
    }
    assert!(window.view.document.remove_layer(1).is_ok());
    window.key(Key::Named(NamedKey::Enter), plain());
    assert!(
        layers_of(&window).iter().all(|layer| layer.name == "layer"),
        "the layer now in its place keeps its name"
    );
}

/// A name being typed lands as the window loses the keyboard, and as it is
/// closed, which then asks whether to keep it.
#[test]
fn a_name_being_typed_lands_as_the_window_loses_the_keyboard_or_closes() {
    let mut window = layered(&[([1; 4], 255), ([2; 4], 255)], 1);
    with_layers_pane(&mut window);
    let name = layer_row(&window, 1).name.center();
    click_at(&mut window, name);
    for ch in "Sky".chars() {
        window.key(Key::Char(ch), plain());
    }
    window.view.focus_changed(false, &mut Region::new());
    assert_eq!(layers_of(&window)[1].name, "Sky");
    let generation = window.view.document().generation();
    window.view.saved(generation);
    click_at(&mut window, name);
    for ch in "Sea".chars() {
        window.key(Key::Char(ch), plain());
    }
    let closing = window.view.close_requested(&mut Region::new());
    assert_eq!(layers_of(&window)[1].name, "Sea");
    assert!(
        matches!(closing.request, Some(Request::AskToClose)),
        "{:?}",
        closing.request
    );
}

/// An undo past a conversion closes the effect left open over it, which a
/// palette picture cannot take, saying so, and leaves a save live.
#[test]
fn an_effect_open_over_a_conversion_undone_is_closed() {
    use crate::transform::{Depth, PaletteChoice, Transform};
    let mut window = four_colours();
    let colour = Transform::Convert {
        depth: Depth::Rgba,
        palette: PaletteChoice::Desktop,
        dither: false,
    };
    let converting = window
        .view
        .transform(colour, "change its colours", &mut Region::new());
    window.run_worker(converting);
    window.act(Action::Adjust(op_index("Gaussian blur")));
    press_on(&mut window, PaneKind::Effects, Spot::Number(500));
    assert!(window.view.operation_pane(PaneKind::Effects).moved());
    window.key(Key::Char('z'), ctrl());
    assert!(
        window
            .view
            .document()
            .picture()
            .is_some_and(|picture| picture.kind().palette().is_some()),
        "a palette picture again"
    );
    assert_eq!(window.view.adjusting(), None, "the blur closed");
    assert!(window
        .view
        .message()
        .is_some_and(|said| said.contains("needs a colour picture")));
    let saving = window.act(Action::Save);
    assert!(
        matches!(saving.request, Some(Request::Save)),
        "{:?}",
        saving.request
    );
}

/// Apply pressed in one pane while the other's operation is completed on the
/// queue applies this one's once that has landed, each as one step.
#[test]
fn apply_in_one_pane_follows_the_others_completion() {
    let canvas = crate::effect::patterned(1200, 900);
    let mut window = Window::new(Document::new(Picture::plain(canvas)));
    window.act(Action::Pane(PaneKind::Effects));
    window.act(Action::Adjust(op_index("Posterise")));
    press_on(&mut window, PaneKind::Adjustments, Spot::Number(900));
    let first = window.wanted_preview().expect("asked");
    window.serve_preview(first);
    let completing = press_on(&mut window, PaneKind::Effects, Spot::Apply);
    let Some(Request::Own(Own::Compute {
        work: super::Compute::Op { op, .. },
        ..
    })) = &completing.request
    else {
        panic!("completed on the queue, not {:?}", completing.request);
    };
    assert!(matches!(op, Op::Posterise { .. }), "the posterise first");
    let applying = window.run_worker(completing);
    assert_eq!(window.view.document().history_depth(), 1);
    let Some(Request::Own(Own::Compute { .. })) = applying.request else {
        panic!("then the blur's own, not {:?}", applying.request);
    };
    window.run_worker(applying);
    assert_eq!(window.view.document().history_depth(), 2, "then the blur");
}

/// An operation left for another keeps its settings until it has landed: one
/// whose completion is refused stays open as it was set.
#[test]
fn an_operation_left_keeps_its_settings_until_it_has_landed() {
    let grey = Sample::Rgba([101, 101, 101, 255]);
    let canvas = Canvas::new(1200, 900, Kind::Rgba, grey).expect("fits");
    let mut window = Window::new(Document::new(Picture::plain(canvas)));
    window.act(Action::Adjust(op_index("Posterise")));
    press_on(&mut window, PaneKind::Adjustments, Spot::Number(900));
    let moved = window.view.adjusting().expect("open");
    let first = window.wanted_preview().expect("asked");
    window.serve_preview(first);
    let leaving = choose_in_selector(&mut window, PaneKind::Adjustments, 1);
    let Some(Request::Own(Own::Compute { job, .. })) = leaving.request else {
        panic!("completed on the queue, not {:?}", leaving.request);
    };
    assert_eq!(
        window.view.operation_pane(PaneKind::Adjustments).op(),
        moved,
        "held until it lands"
    );
    let refused = window.view.computed(
        job,
        Computed::Op(Err(Halted::OutOfMemory)),
        &mut Region::new(),
    );
    window.apply(&refused);
    assert_eq!(
        window.view.adjusting(),
        Some(moved),
        "still open as it was set"
    );
    assert_eq!(window.view.document().history_depth(), 0);
}

/// An Apply's completion refused names what waited on it as not done: a save,
/// or a save before the window closes, which then stays open.
#[test]
fn a_refused_completion_names_the_save_it_held_as_not_done() {
    let grey = Sample::Rgba([101, 101, 101, 255]);
    let canvas = Canvas::new(1200, 900, Kind::Rgba, grey).expect("fits");
    let mut window = Window::new(Document::new(Picture::plain(canvas)));
    window.act(Action::Adjust(op_index("Posterise")));
    press_on(&mut window, PaneKind::Adjustments, Spot::Number(900));
    let first = window.wanted_preview().expect("asked");
    window.serve_preview(first);
    let refuse = |window: &mut Window, asked: Outcome| {
        let Some(Request::Own(Own::Compute { job, .. })) = asked.request else {
            panic!("completed on the queue, not {:?}", asked.request);
        };
        let refused = window.view.computed(
            job,
            Computed::Op(Err(Halted::OutOfMemory)),
            &mut Region::new(),
        );
        window.apply(&refused);
        String::from(window.view.message().unwrap_or_default())
    };
    let saving = window.act(Action::Save);
    let said = refuse(&mut window, saving);
    assert!(said.ends_with("the picture was not saved"), "{said}");
    let closing = window.view.save_then_close(&mut Region::new());
    let said = refuse(&mut window, closing);
    assert!(said.ends_with("and the window stays open"), "{said}");
    let applying = press_on(&mut window, PaneKind::Adjustments, Spot::Apply);
    let waiting = window.view.save_then_close(&mut Region::new());
    assert!(waiting.request.is_none(), "waiting on the Apply");
    let said = refuse(&mut window, applying);
    assert!(said.ends_with("and the window stays open"), "{said}");
    assert!(
        !window.view.asking_to_close(),
        "nothing left for a quit to wait on"
    );
}

/// A sample in the other pane keeps the open operation's settings until it has
/// landed: one refused stays open, and the pane the sample opened goes back to
/// its start, so one pane is open.
#[test]
fn a_sample_in_the_other_pane_keeps_the_open_operation_until_it_has_landed() {
    let grey = Sample::Rgba([101, 101, 101, 255]);
    let canvas = Canvas::new(1200, 900, Kind::Rgba, grey).expect("fits");
    let mut window = Window::new(Document::new(Picture::plain(canvas)));
    window.act(Action::Pane(PaneKind::Effects));
    window.act(Action::Adjust(op_index("Posterise")));
    press_on(&mut window, PaneKind::Adjustments, Spot::Number(900));
    let moved = window.view.adjusting().expect("open");
    let first = window.wanted_preview().expect("asked");
    window.serve_preview(first);
    let sampled = press_on(&mut window, PaneKind::Effects, Spot::Number(500));
    let Some(Request::Own(Own::Compute { job, .. })) = sampled.request else {
        panic!(
            "the posterise completed on the queue, not {:?}",
            sampled.request
        );
    };
    assert_eq!(
        window.view.operation_pane(PaneKind::Adjustments).op(),
        moved,
        "held until it lands"
    );
    let refused = window.view.computed(
        job,
        Computed::Op(Err(Halted::OutOfMemory)),
        &mut Region::new(),
    );
    window.apply(&refused);
    assert_eq!(
        window.view.adjusting(),
        Some(moved),
        "kept open as it was set"
    );
    assert!(
        !window.view.operation_pane(PaneKind::Effects).is_open(),
        "the sample turned down"
    );
}

/// Picking another layer in the Layers pane, or going to one by its number,
/// leaves the open operation on the layer it was opened on first, as stepping
/// to a layer does.
#[test]
fn picking_another_layer_leaves_the_open_operation_first() {
    let mut window = layered(&[([10, 20, 30, 255], 255), ([200, 100, 50, 255], 255)], 1);
    with_layers_pane(&mut window);
    window.act(Action::Adjust(op_index("Posterise")));
    press_on(&mut window, PaneKind::Adjustments, Spot::Number(100));
    window.settle_previews();
    let spot = layer_row(&window, 0).name.center();
    click_at(&mut window, spot);
    assert_eq!(painted_on(&window), 0);
    assert_eq!(window.view.adjusting(), None, "applied, not carried over");
    assert_eq!(window.view.document().history_depth(), 1);
    assert_ne!(
        layers_of(&window)[1].canvas.colour_at(3, 3),
        Some([200, 100, 50, 255]),
        "to the layer it was opened on"
    );
    assert_eq!(
        layers_of(&window)[0].canvas.colour_at(3, 3),
        Some([10, 20, 30, 255])
    );
    window.act(Action::Adjust(op_index("Invert")));
    let to = AppMenuItemId::new(GO_TO_LAYER).expect("an id");
    let outcome = window.view.entered(to, "2", &mut Region::new());
    window.apply(&outcome);
    assert_eq!(painted_on(&window), 1);
    assert_eq!(window.view.adjusting(), None, "the invert left first");
}

/// What an operation shows is kept by what it reads — the layer painted on, by
/// its revision, and the selection — so another layer's eye, or an opacity,
/// changing works nothing out again.
#[test]
fn how_the_layers_show_works_no_preview_out_again() {
    let mut window = layered(&[([10, 20, 30, 255], 255), ([200, 100, 50, 255], 255)], 1);
    with_layers_pane(&mut window);
    window.act(Action::Adjust(op_index("Posterise")));
    press_on(&mut window, PaneKind::Adjustments, Spot::Number(100));
    window.settle_previews();
    assert!(window.view.preview_canvas().is_some());
    let spot = layer_row(&window, 0).eye.center();
    click_at(&mut window, spot);
    assert!(!layers_of(&window)[0].visible, "hidden");
    assert!(
        window.wanted_preview().is_none(),
        "nothing worked out again"
    );
    window.view.layer_opacity(40, true, &mut Region::new());
    assert!(
        window.wanted_preview().is_none(),
        "nor for an opacity settled"
    );
    assert!(
        window.view.preview_canvas().is_some(),
        "the preview still shows"
    );
    assert!(
        window.view.adjusting().is_some(),
        "over an operation still open"
    );
}

/// Undoing another layer's eye keeps painting where it was, as the eye did,
/// so the open operation stays on the layer it was opened on, its preview
/// standing.
#[test]
fn undoing_another_layers_eye_keeps_painting_and_the_open_operation_put() {
    let mut window = layered(&[([10, 20, 30, 255], 255), ([200, 100, 50, 255], 255)], 1);
    with_layers_pane(&mut window);
    window.act(Action::Adjust(op_index("Posterise")));
    press_on(&mut window, PaneKind::Adjustments, Spot::Number(100));
    window.settle_previews();
    let spot = layer_row(&window, 0).eye.center();
    click_at(&mut window, spot);
    assert!(!layers_of(&window)[0].visible, "hidden");
    window.key(Key::Char('z'), ctrl());
    assert!(layers_of(&window)[0].visible, "shown again");
    assert_eq!(painted_on(&window), 1, "painting stayed where it was");
    assert!(
        window.wanted_preview().is_none(),
        "nothing worked out again"
    );
    press_on(&mut window, PaneKind::Adjustments, Spot::Apply);
    assert_ne!(
        layers_of(&window)[1].canvas.colour_at(3, 3),
        Some([200, 100, 50, 255]),
        "applied to the layer it was opened on"
    );
    assert_eq!(
        layers_of(&window)[0].canvas.colour_at(3, 3),
        Some([10, 20, 30, 255])
    );
}

/// How many pixels of `rect` `damage` repaints.
fn repainted_within(damage: &Region, rect: Rect) -> u64 {
    let mut within = damage.clone();
    within.clip(rect);
    within
        .rects()
        .iter()
        .map(|part| u64::from(part.width) * u64::from(part.height))
        .sum()
}

/// A hover repaints the controls it crosses and the pointer's reading, never
/// the canvas, whatever tool is in hand.
#[test]
fn a_hover_never_repaints_the_canvas() {
    for tool in Tool::ALL {
        let mut window = Window::white(64, 48);
        window.act(Action::Tool(tool));
        let canvas = window.view.layout().canvas();
        let (right, bottom) = (to_i32(WINDOW.0) - 1, to_i32(WINDOW.1) - 1);
        for y in (0..bottom).step_by(12).chain([bottom]) {
            for x in (0..right).step_by(12).chain([right]) {
                let theme = window.registry.active();
                let mut damage = Region::new();
                window.now += 10_000_000;
                let event = InputEvent::PointerMoved {
                    to: Point::new(x, y),
                };
                let _ = window
                    .view
                    .on_pointer(&event, window.now, Scale::ONE, theme, &mut damage);
                assert_eq!(
                    repainted_within(&damage, canvas),
                    0,
                    "{tool:?} over ({x}, {y}) repainted the canvas: {:?}",
                    damage.rects()
                );
            }
        }
    }
}

/// A stroke repaints what each move reaches, not the canvas it lies on.
#[test]
fn a_stroke_repaints_what_each_move_reaches() {
    for tool in [
        Tool::Pencil,
        Tool::Brush,
        Tool::Line,
        Tool::Rectangle,
        Tool::Select,
    ] {
        let mut window = Window::white(400, 300);
        window.act(Action::Tool(tool));
        let canvas = window.view.layout().canvas();
        let whole = u64::from(canvas.width) * u64::from(canvas.height);
        let start = Point::new(canvas.left() + 100, canvas.top() + 100);
        window.move_to(start);
        window.press(PointerButton::Primary);
        for step in 1..8 {
            let theme = window.registry.active();
            let mut damage = Region::new();
            window.now += 10_000_000;
            let event = InputEvent::PointerMoved {
                to: Point::new(start.x + step * 9, start.y + step * 5),
            };
            let _ = window
                .view
                .on_pointer(&event, window.now, Scale::ONE, theme, &mut damage);
            let repainted = repainted_within(&damage, canvas);
            assert!(
                repainted * 8 < whole,
                "{tool:?} repainted {repainted} pixels of a canvas of {whole}"
            );
        }
        window.release(PointerButton::Primary);
    }
}
