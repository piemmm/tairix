//! A Draw window driven by its input: drawing, picking, moving, typing,
//! menu commands and the inspector, each landing as the steps it should.

use alloc::string::String;
use alloc::vec;

use tairix_bezier::Point;
use tairix_colour::Rgba;
use tairix_drawing::unkept::{Lost, Unkept};
use tairix_drawing::{Ids, Kind, Paint};
use tairix_geometry::{Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_theme::Theme;
use tairix_window::docapp::{DocumentView, Request};
use tairix_window::document::{Access, SavedDocument};

use crate::document::{blank, Document};
use crate::inspector::{Change, Target};
use crate::layout::Layout;
use crate::menu::Command;
use crate::tools::Tool;
use crate::view::{Own, View};
use crate::window::CopyFormat;

struct Window {
    view: View,
    layout: Layout,
    theme: Theme,
}

impl Window {
    fn new() -> Self {
        let mut ids = Ids::new();
        let drawing = blank(&mut ids).expect("a drawing");
        let mut view = View::new(
            Document::new(drawing, ids, true),
            String::from("Untitled"),
            Access::Untitled,
            Unkept::NONE,
            1.0,
        );
        let layout = Layout::of(1080, 720, Scale::ONE, &Theme::light());
        view.settle(&layout, &mut Region::new());
        Self {
            view,
            layout,
            theme: Theme::light(),
        }
    }

    fn feed(&mut self, event: InputEvent) -> Option<Request<crate::view::MenuKind, Own>> {
        let outcome = self.view.on_input(
            &event,
            &self.layout,
            Scale::ONE,
            &self.theme,
            &mut Region::new(),
        );
        outcome.request
    }

    /// The window point over the drawing point `(x, y)`.
    fn at(&self, x: f64, y: f64) -> tairix_geometry::Point {
        let point = self.view.window_at(&self.layout, Point::new(x, y));
        tairix_geometry::Point::new(
            tairix_util::mathf::round_i32(point.x),
            tairix_util::mathf::round_i32(point.y),
        )
    }

    fn drag(&mut self, from: (f64, f64), to: (f64, f64)) {
        let (a, b) = (self.at(from.0, from.1), self.at(to.0, to.1));
        self.feed(InputEvent::PointerMoved { to: a });
        self.feed(InputEvent::PointerPressed {
            button: PointerButton::Primary,
        });
        self.feed(InputEvent::PointerMoved { to: b });
        self.feed(InputEvent::PointerReleased {
            button: PointerButton::Primary,
        });
    }

    fn key(&mut self, key: Key) {
        self.feed(InputEvent::KeyPressed {
            key,
            modifiers: Modifiers::default(),
        });
    }

    fn chord(&mut self, character: char) {
        self.feed(InputEvent::KeyPressed {
            key: Key::Char(character),
            modifiers: Modifiers {
                ctrl: true,
                ..Modifiers::default()
            },
        });
    }

    /// Press and let go with the text tool at drawing point `(x, y)`.
    fn text_at(&mut self, x: f64, y: f64) {
        self.view.choose_tool(Tool::Text);
        let at = self.at(x, y);
        self.feed(InputEvent::PointerMoved { to: at });
        self.feed(InputEvent::PointerPressed {
            button: PointerButton::Primary,
        });
        self.feed(InputEvent::PointerReleased {
            button: PointerButton::Primary,
        });
    }

    fn change(&mut self, change: Change) {
        let layout = self.layout;
        self.view.apply_change(change, &layout, &mut Region::new());
    }

    fn command(&mut self, command: Command) {
        let layout = self.layout;
        self.view.carry_out(command, &layout, &mut Region::new());
    }

    fn objects(&self) -> usize {
        self.view.document().drawing().objects() - self.view.document().drawing().layers().len()
    }
}

#[test]
fn a_rectangle_drawn_by_a_drag_is_one_selected_step_undone_by_one_undo() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Rectangle);
    window.drag((100.0, 100.0), (300.0, 200.0));
    assert_eq!(window.objects(), 1);
    assert_eq!(window.view.document().depth(), 1);
    assert_eq!(window.view.selection, vec![vec![0, 0]]);
    let bounds = window.view.selection_bounds();
    assert!(
        (bounds.min.x - 100.0).abs() < 2.0 && (bounds.max.x - 300.0).abs() < 2.0,
        "{bounds:?}"
    );
    window.view.undo(false, &mut Region::new());
    assert_eq!(window.objects(), 0);
    assert!(
        window.view.selection.is_empty(),
        "what was undone is not selected"
    );
}

#[test]
fn the_select_tool_moves_what_it_picks_as_one_step_and_a_marquee_takes_what_it_holds() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Rectangle);
    window.drag((100.0, 100.0), (200.0, 200.0));
    window.drag((400.0, 100.0), (500.0, 200.0));
    window.view.choose_tool(Tool::Select);
    window.drag((150.0, 150.0), (150.0, 350.0));
    assert_eq!(window.view.document().depth(), 3);
    let moved = window.view.selection_bounds();
    assert!((moved.min.y - 300.0).abs() < 3.0, "moved down: {moved:?}");
    window.drag((50.0, 50.0), (600.0, 250.0));
    assert_eq!(
        window.view.selection,
        vec![vec![0, 1]],
        "only what lies wholly within"
    );
    let before = window.view.selection_bounds();
    window.feed(InputEvent::KeyPressed {
        key: Key::Named(NamedKey::Right),
        modifiers: Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    });
    let nudged = window.view.selection_bounds();
    assert!(
        (nudged.min.x - before.min.x - 10.0).abs() < 1e-9,
        "nudged ten: {nudged:?}"
    );
    window.key(Key::Named(NamedKey::Delete));
    assert_eq!(window.objects(), 1);
}

#[test]
fn typed_text_lands_as_one_step_and_text_left_empty_is_none() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Text);
    let at = window.at(100.0, 100.0);
    window.feed(InputEvent::PointerMoved { to: at });
    window.feed(InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    window.feed(InputEvent::PointerReleased {
        button: PointerButton::Primary,
    });
    for character in "Hi!".chars() {
        window.key(Key::Char(character));
    }
    window.key(Key::Named(NamedKey::Backspace));
    window.key(Key::Named(NamedKey::Escape));
    assert_eq!(
        window.view.document().depth(),
        1,
        "one step for the whole typing"
    );
    let text = window
        .view
        .document()
        .drawing()
        .node_at(&[0, 0])
        .expect("the text");
    let Kind::Text { text, .. } = text.kind() else {
        panic!("a text object");
    };
    assert_eq!(&*text.content, "Hi");
    let empty_at = window.at(400.0, 400.0);
    window.feed(InputEvent::PointerMoved { to: empty_at });
    window.feed(InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    window.key(Key::Named(NamedKey::Escape));
    assert_eq!(window.objects(), 1, "nothing typed, nothing kept");
}

#[test]
fn menu_commands_group_copy_and_remove_as_steps() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Ellipse);
    window.drag((100.0, 100.0), (200.0, 200.0));
    window.drag((300.0, 100.0), (400.0, 200.0));
    let layout = window.layout;
    let mut damage = Region::new();
    window
        .view
        .carry_out(Command::SelectAll, &layout, &mut damage);
    assert_eq!(window.view.selection.len(), 2);
    window.view.carry_out(Command::Group, &layout, &mut damage);
    assert_eq!(window.view.selection, vec![vec![0, 0]]);
    assert_eq!(window.objects(), 3, "a group of two");
    let copied = window.view.carry_out(Command::Copy, &layout, &mut damage);
    assert!(
        matches!(copied.request, Some(Request::Own(Own::Copy(ref svg))) if svg.contains("<ellipse"))
    );
    window
        .view
        .carry_out(Command::Ungroup, &layout, &mut damage);
    assert_eq!(window.view.selection.len(), 2);
    window
        .view
        .carry_out(Command::Duplicate, &layout, &mut damage);
    assert_eq!(window.objects(), 4);
    let reshape = window.view.carry_out(Command::Union, &layout, &mut damage);
    assert!(
        matches!(reshape.request, Some(Request::Own(Own::Compute(_)))),
        "booleans run on the queue"
    );
    let close = window.view.carry_out(Command::Close, &layout, &mut damage);
    assert!(
        matches!(close.request, Some(Request::AskToClose)),
        "changes are never thrown away unasked"
    );
}

#[test]
fn a_colour_being_chosen_is_shown_live_and_lands_once_settled() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Rectangle);
    window.drag((100.0, 100.0), (200.0, 200.0));
    let depth = window.view.document().depth();
    let red = Paint::Colour(Rgba::new(255, 0, 0, 255));
    for settled in [false, false, true] {
        let change = Change::Paint {
            target: Target::Fill,
            paint: red.clone(),
            settled,
        };
        let layout = window.layout;
        window
            .view
            .apply_change(change, &layout, &mut Region::new());
    }
    assert_eq!(
        window.view.document().depth(),
        depth + 1,
        "one step for the whole choice"
    );
    let node = window
        .view
        .document()
        .drawing()
        .node_at(&[0, 0])
        .expect("the rectangle");
    assert_eq!(node.style().map(|style| style.fill.clone()), Some(red));
}

#[test]
fn the_canvas_asks_for_its_tiles_once_and_takes_them_as_they_land() {
    let mut window = Window::new();
    let layout = window.layout;
    let job = window.view.wanted_render(&layout).expect("tiles to render");
    assert!(!job.keys.is_empty());
    let again = window.view.wanted_render(&layout);
    assert!(
        again.is_none_or(|next| next.keys.iter().all(|key| !job.keys.contains(key))),
        "nothing asked twice"
    );
    let tiles = job.keys.iter().map(|key| {
        let tile =
            tairix_raster::Surface::new(crate::tiles::TILE, crate::tiles::TILE).expect("a tile");
        (*key, tile)
    });
    window
        .view
        .rendered(job.scene, (&job.keys, tiles), &layout, &mut Region::new());
    assert!(job
        .keys
        .iter()
        .all(|key| window.view.tiles.tile(key).is_some()));
}

#[test]
fn a_file_holding_what_a_drawing_cannot_keep_opens_read_only_and_names_its_formats() {
    let mut ids = Ids::new();
    let drawing = blank(&mut ids).expect("a drawing");
    let mut lost = Unkept::NONE;
    lost.insert(Lost::Filters);
    let view = View::new(
        Document::new(drawing, ids, true),
        String::from("art.svg"),
        Access::Writable,
        lost,
        1.0,
    );
    assert_eq!(view.access(), Access::ReadOnly);
    assert!(view
        .message
        .as_deref()
        .is_some_and(|message| message.contains("filter effects")));
    assert!(
        view.refuse_save("art.svg").is_some(),
        "never written back over its own file"
    );
    assert!(view.refuse_save("other.svg").is_none());
    assert!(
        view.refuse_save("art.png").is_some(),
        "Draw saves SVG; PNG is a copy"
    );
    assert_eq!(CopyFormat::of("plan.PDF"), Some(CopyFormat::Pdf));
    assert_eq!(CopyFormat::of("plan"), None);
    assert!(view.copy_formats().is_ok());
    let fresh = Window::new();
    assert!(
        fresh.view.is_empty() && !fresh.view.is_modified(),
        "a blank window is pristine"
    );
}

/// Every object's identity in `node` and what it holds, clips too.
fn identities(node: &tairix_drawing::Node, into: &mut alloc::vec::Vec<tairix_drawing::ObjectId>) {
    into.push(node.id());
    if let Kind::Group { children, clip } = node.kind() {
        for child in children.iter() {
            identities(child, into);
        }
        if let Some(clip) = clip {
            identities(clip, into);
        }
    }
}

#[test]
fn a_pasted_group_and_all_it_holds_take_identities_of_their_own() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Ellipse);
    window.drag((100.0, 100.0), (200.0, 200.0));
    window.drag((300.0, 100.0), (400.0, 200.0));
    let layout = window.layout;
    let mut damage = Region::new();
    window
        .view
        .carry_out(Command::SelectAll, &layout, &mut damage);
    window.view.carry_out(Command::Group, &layout, &mut damage);
    let copied = window.view.selection_drawing().expect("the group");
    window.view.pasted(Ok(copied), &mut damage, &layout);
    assert_eq!(window.objects(), 6, "two groups of two");
    let mut seen = alloc::vec::Vec::new();
    for layer in window.view.document().drawing().layers().iter() {
        identities(layer, &mut seen);
    }
    let all = seen.len();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), all, "no identity is shared");
}

#[test]
fn a_guide_step_is_named_for_what_it_did() {
    let mut window = Window::new();
    let ruler = window.layout.top_ruler;
    let from = tairix_geometry::Point::new(ruler.left() + 300, ruler.top() + 4);
    window.feed(InputEvent::PointerMoved { to: from });
    window.feed(InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    let onto = window.at(0.0, 150.0);
    window.feed(InputEvent::PointerMoved {
        to: tairix_geometry::Point::new(from.x, onto.y),
    });
    window.feed(InputEvent::PointerReleased {
        button: PointerButton::Primary,
    });
    assert_eq!(window.view.document().undo_label(), Some("Add guide"));
    assert_eq!(window.view.document().drawing().guides().len(), 1);
    window.feed(InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    window.feed(InputEvent::PointerMoved { to: from });
    window.feed(InputEvent::PointerReleased {
        button: PointerButton::Primary,
    });
    assert_eq!(window.view.document().undo_label(), Some("Remove guide"));
    assert!(window.view.document().drawing().guides().is_empty());
}

#[test]
fn undo_while_typing_lands_the_typing_and_takes_it_back_whole() {
    let mut window = Window::new();
    window.text_at(100.0, 100.0);
    for character in "ñandú".chars() {
        window.key(Key::Char(character));
    }
    window.chord('z');
    assert!(window.view.typing.is_none(), "the typing ended");
    assert!(window.view.live.is_none());
    assert_eq!(window.objects(), 0, "and was taken back whole");
    window.chord('y');
    assert_eq!(window.objects(), 1, "and done again");
    window.text_at(100.0, 100.0);
    assert!(window.view.typing.is_some(), "typing into the text again");
    window.key(Key::Named(NamedKey::Left));
    window.key(Key::Named(NamedKey::Left));
    window.chord('z');
    assert!(window.view.typing.is_none());
    window.key(Key::Char('x'));
    assert!(
        window.view.document().drawing().node_at(&[0, 0]).is_none(),
        "nothing typed after the undo reaches a text"
    );
}

#[test]
fn moving_the_caret_changes_no_drawing() {
    let mut window = Window::new();
    window.text_at(100.0, 100.0);
    window.key(Key::Char('a'));
    window.key(Key::Char('b'));
    let scene = window.view.scene;
    window.key(Key::Named(NamedKey::Left));
    window.key(Key::Named(NamedKey::Home));
    window.key(Key::Named(NamedKey::End));
    assert_eq!(window.view.scene, scene, "nothing new to show");
    assert_eq!(
        window.view.typing.as_ref().map(|typing| typing.caret),
        Some(2)
    );
}

#[test]
fn a_change_that_changes_nothing_is_no_step() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Rectangle);
    window.drag((100.0, 100.0), (200.0, 200.0));
    let depth = window.view.document().depth();
    let join = window
        .view
        .document()
        .drawing()
        .node_at(&[0, 0])
        .and_then(|node| node.style().map(|style| style.stroke.join))
        .expect("a style");
    window.change(Change::Join(join));
    window.change(Change::Opacity {
        opacity: 1.0,
        settled: true,
    });
    window.change(Change::Corners(0.0));
    assert_eq!(window.view.document().depth(), depth);
}

#[test]
fn a_locked_or_hidden_layer_drops_out_of_the_selection_and_takes_nothing_new() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Rectangle);
    window.drag((100.0, 100.0), (200.0, 200.0));
    assert_eq!(window.view.selection.len(), 1);
    window.change(Change::Layer(crate::inspector::LayerCommand::Lock(0)));
    assert!(
        window.view.selection.is_empty(),
        "a locked object is not edited"
    );
    window.drag((300.0, 100.0), (400.0, 200.0));
    assert_eq!(window.objects(), 1, "nothing drawn into a locked layer");
    assert!(window.view.message.is_some(), "and the user is told why");
    window.change(Change::Layer(crate::inspector::LayerCommand::Lock(0)));
    window.change(Change::Layer(crate::inspector::LayerCommand::Visible(0)));
    window.text_at(500.0, 500.0);
    assert!(
        window.view.typing.is_none(),
        "no text begun in a hidden layer"
    );
}

#[test]
fn an_undone_rearrangement_keeps_the_object_selected_not_whatever_took_its_place() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Rectangle);
    for left in [100.0, 300.0, 500.0] {
        window.drag((left, 100.0), (left + 100.0, 200.0));
    }
    window.view.selection = vec![vec![0, 0]];
    let chosen = window
        .view
        .document()
        .drawing()
        .node_at(&[0, 0])
        .map(|node| node.id());
    window.command(Command::ToFront);
    assert_eq!(window.view.selection, vec![vec![0, 2]]);
    window.command(Command::Undo);
    assert_eq!(
        window.view.selection,
        vec![vec![0, 0]],
        "found again where it went back to"
    );
    let selected = window
        .view
        .document()
        .drawing()
        .node_at(&window.view.selection[0])
        .map(|node| node.id());
    assert_eq!(selected, chosen);
    window.command(Command::Redo);
    assert_eq!(window.view.selection, vec![vec![0, 2]]);
}

#[test]
fn a_paste_of_many_objects_lands_as_one_step_with_all_selected() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Ellipse);
    for left in [100.0, 250.0, 400.0] {
        window.drag((left, 100.0), (left + 100.0, 200.0));
    }
    window.command(Command::SelectAll);
    let copied = window.view.selection_drawing().expect("the selection");
    let depth = window.view.document().depth();
    let layout = window.layout;
    window.view.pasted(Ok(copied), &mut Region::new(), &layout);
    assert_eq!(window.view.document().depth(), depth + 1);
    assert_eq!(window.objects(), 6);
    assert_eq!(
        window.view.selection,
        vec![vec![0, 3], vec![0, 4], vec![0, 5]]
    );
}

#[test]
fn a_marquee_adding_to_the_selection_holds_each_object_once() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Rectangle);
    window.drag((100.0, 100.0), (150.0, 150.0));
    window.drag((200.0, 100.0), (250.0, 150.0));
    window.view.choose_tool(Tool::Select);
    window.view.selection = vec![vec![0, 0]];
    let area = tairix_bezier::Bounds::new(Point::new(50.0, 50.0), Point::new(300.0, 300.0));
    window.view.select_within(area, true).expect("selected");
    let mut selected = window.view.selection.clone();
    selected.sort_unstable();
    assert_eq!(selected, vec![vec![0, 0], vec![0, 1]]);
}

#[test]
fn undo_with_text_just_begun_takes_back_only_the_text() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Rectangle);
    window.drag((100.0, 100.0), (200.0, 200.0));
    assert_eq!(window.objects(), 1);
    window.text_at(400.0, 400.0);
    assert!(window.view.typing.is_some());
    window.chord('z');
    assert!(window.view.typing.is_none());
    assert_eq!(window.objects(), 1, "the rectangle drawn before stays");
}

#[test]
fn a_press_on_a_field_of_the_inspector_ends_typing_where_it_is() {
    let mut window = Window::new();
    window.text_at(100.0, 100.0);
    window.key(Key::Char('H'));
    let layout = window.layout;
    let theme = Theme::light();
    let field = window
        .view
        .inspect(|inspector, shown| inspector.stroke_width_at((&layout, Scale::ONE, &theme), shown))
        .expect("the stroke width's field shows");
    let width = tairix_geometry::Point::new(
        field.left() + i32::try_from(field.width / 2).expect("a width"),
        field.top() + i32::try_from(field.height / 2).expect("a height"),
    );
    window.feed(InputEvent::PointerMoved { to: width });
    window.feed(InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    assert!(window.view.inspector.has_keyboard());
    assert!(window.view.typing.is_none(), "the keyboard moved on");
    assert_eq!(window.objects(), 1, "and what was typed landed");
    assert!(window.view.live.is_none());
}

#[test]
fn a_live_change_back_where_the_document_stands_leaves_nothing_under_way() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Rectangle);
    window.drag((100.0, 100.0), (200.0, 200.0));
    let label = window.view.document().undo_label();
    window.change(Change::Opacity {
        opacity: 0.1,
        settled: false,
    });
    assert!(window.view.live.is_some());
    window.change(Change::Opacity {
        opacity: 1.0,
        settled: false,
    });
    assert!(window.view.live.is_none(), "nothing left under way");
    assert_eq!(window.view.document().undo_label(), label, "and no step");
}

#[test]
fn a_guide_dragged_out_while_typing_keeps_what_was_typed() {
    let mut window = Window::new();
    window.text_at(100.0, 100.0);
    for character in "Hi".chars() {
        window.key(Key::Char(character));
    }
    let ruler = window.layout.top_ruler;
    let from = tairix_geometry::Point::new(ruler.left() + 300, ruler.top() + 4);
    window.feed(InputEvent::PointerMoved { to: from });
    window.feed(InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    let onto = window.at(0.0, 300.0);
    window.feed(InputEvent::PointerMoved {
        to: tairix_geometry::Point::new(from.x, onto.y),
    });
    window.feed(InputEvent::PointerReleased {
        button: PointerButton::Primary,
    });
    assert_eq!(window.view.document().drawing().guides().len(), 1);
    let text = window
        .view
        .document()
        .drawing()
        .node_at(&[0, 0])
        .expect("the text");
    let Kind::Text { text, .. } = text.kind() else {
        panic!("a text object");
    };
    assert_eq!(&*text.content, "Hi");
}

#[test]
fn a_pasted_hidden_or_locked_object_is_not_selected() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Ellipse);
    window.drag((100.0, 100.0), (200.0, 200.0));
    window.drag((300.0, 100.0), (400.0, 200.0));
    window.command(Command::SelectAll);
    let copied = window.view.selection_drawing().expect("the selection");
    let layer = copied.node_at(&[0]).expect("a layer");
    let Kind::Group { children, clip } = layer.kind() else {
        panic!("a group");
    };
    let hidden = children.get(0).expect("the first").with_visible(false);
    let locked = children.get(1).expect("the second").with_locked(true);
    let held = layer
        .with_kind(Kind::Group {
            children: tairix_drawing::Children::from_nodes(vec![
                alloc::sync::Arc::new(hidden),
                alloc::sync::Arc::new(locked),
            ])
            .expect("held"),
            clip: clip.clone(),
        })
        .expect("a layer");
    let pasted = copied
        .with_node_at(&[0], alloc::sync::Arc::new(held))
        .expect("pasted");
    let layout = window.layout;
    window.view.pasted(Ok(pasted), &mut Region::new(), &layout);
    assert_eq!(window.objects(), 4);
    assert!(
        window.view.selection.is_empty(),
        "{:?}",
        window.view.selection
    );
}

#[test]
fn a_reshape_lands_while_the_caret_waits_in_a_text_with_nothing_typed() {
    let mut window = Window::new();
    window.text_at(500.0, 500.0);
    window.key(Key::Char('T'));
    window.key(Key::Named(NamedKey::Escape));
    window.view.choose_tool(Tool::Rectangle);
    window.drag((100.0, 100.0), (200.0, 200.0));
    window.drag((150.0, 150.0), (250.0, 250.0));
    window.view.selection = vec![vec![0, 1], vec![0, 2]];
    let layout = window.layout;
    let asked = window
        .view
        .carry_out(Command::Union, &layout, &mut Region::new());
    let Some(Request::Own(Own::Compute(compute))) = asked.request else {
        panic!("a reshape for the queue");
    };
    let computed = crate::compute::work_out(
        *compute,
        &crate::glyphs::Lettering::new(crate::glyphs::Arrived),
    );
    window.text_at(500.0, 500.0);
    assert!(window.view.typing.is_some() && window.view.live.is_none());
    window.view.computed(computed, &layout, &mut Region::new());
    assert_eq!(window.objects(), 2, "the union landed");
}
