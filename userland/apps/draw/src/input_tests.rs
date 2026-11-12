//! A Draw window driven by its input: drawing, picking, moving, typing,
//! menu commands and the inspector, each landing as the steps it should.

use alloc::string::String;
use alloc::vec;

use tairix_bezier::Point;
use tairix_colour::Rgba;
use tairix_drawing::unkept::{Lost, Unkept};
use tairix_drawing::{Ids, Kind, Paint};
use tairix_geometry::{Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_theme::Theme;
use tairix_window::docapp::{DocumentView, Request, SaveTarget};
use tairix_window::document::{Access, SavedDocument};

use crate::copy::CopyFormat;
use crate::document::{blank, Document};
use crate::inspector::{Change, PageChange, Target};
use crate::layout::{px, Faces};
use crate::menu::Command;
use crate::tools::Tool;
use crate::view::{Own, View};

struct Window {
    view: View,
    theme: Theme,
}

impl Window {
    fn new() -> Self {
        let mut ids = Ids::new();
        let drawing =
            blank(&mut ids, &crate::preferences::Preferences::default()).expect("a drawing");
        let mut view = View::new(
            Document::new(drawing, ids, true),
            String::from("Untitled"),
            Access::Untitled,
            Unkept::NONE,
            1.0,
        );
        let theme = Theme::light();
        let faces = Faces::of(&theme, Scale::ONE);
        view.lay_out(1080, 720, &theme, Scale::ONE, faces, &mut Region::new());
        Self { view, theme }
    }

    fn feed(&mut self, event: InputEvent) -> Option<Request<crate::view::MenuKind, Own>> {
        let outcome = self
            .view
            .on_input(&event, Scale::ONE, &self.theme, &mut Region::new());
        outcome.request
    }

    /// The window point over the drawing point `(x, y)`.
    fn at(&self, x: f64, y: f64) -> tairix_geometry::Point {
        let point = self.view.window_at(Point::new(x, y));
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

    /// Hold Alt down, or let it go.
    fn modifier_alt(&mut self, alt: bool) {
        self.feed(InputEvent::ModifiersChanged {
            modifiers: Modifiers {
                alt,
                ..Modifiers::default()
            },
        });
    }

    fn change(&mut self, change: Change) {
        self.view.apply_change(change, &mut Region::new());
    }

    fn command(&mut self, command: Command) {
        self.view.carry_out(command, &mut Region::new());
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

/// A row keeps the window menu up under a secondary click exactly where its
/// command is done to the drawing in place; a tool, a zoom, a paper or a unit
/// is an alternative and always does.
#[test]
fn the_window_menu_stays_up_for_every_command_done_in_place() {
    use tairix_abi::window_ipc::AppMenuRowView;
    use tairix_window::menu::MenuCommand;

    use crate::menu::{chosen, Chosen};
    use crate::view::MenuKind;

    let window = Window::new();
    let menu = window.view.build_menu(MenuKind::Window);
    let mut kept = 0;
    for (row, _) in menu.rows() {
        let AppMenuRowView::Item(item) = row else {
            continue;
        };
        match chosen(item.id.get()).expect("a row the menu numbered") {
            Chosen::Command(command) => {
                assert_eq!(item.repeatable, command.acts_in_place(), "{command:?}");
            }
            other => assert!(item.repeatable, "{other:?}"),
        }
        kept += usize::from(item.repeatable);
    }
    assert!(kept > 0);
    assert!(Command::Undo.acts_in_place() && !Command::Delete.acts_in_place());
}

#[test]
fn menu_commands_group_copy_and_remove_as_steps() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Ellipse);
    window.drag((100.0, 100.0), (200.0, 200.0));
    window.drag((300.0, 100.0), (400.0, 200.0));
    let mut damage = Region::new();
    window.view.carry_out(Command::SelectAll, &mut damage);
    assert_eq!(window.view.selection.len(), 2);
    window.view.carry_out(Command::Group, &mut damage);
    assert_eq!(window.view.selection, vec![vec![0, 0]]);
    assert_eq!(window.objects(), 3, "a group of two");
    let copied = window.view.carry_out(Command::Copy, &mut damage);
    assert!(
        matches!(copied.request, Some(Request::Own(Own::Copy(ref drawing))) if drawing.objects() == 4),
        "the group and its two, in a layer, written as SVG on the queue"
    );
    window.view.carry_out(Command::Ungroup, &mut damage);
    assert_eq!(window.view.selection.len(), 2);
    window.view.carry_out(Command::Duplicate, &mut damage);
    assert_eq!(window.objects(), 4);
    let reshape = window.view.carry_out(Command::Union, &mut damage);
    assert!(
        matches!(reshape.request, Some(Request::Own(Own::Compute(_)))),
        "booleans run on the queue"
    );
    let close = window.view.carry_out(Command::Close, &mut damage);
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
        window.view.apply_change(change, &mut Region::new());
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
    let job = window.view.wanted_render().expect("tiles to render");
    assert!(!job.keys.is_empty());
    let again = window.view.wanted_render();
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
        .rendered(job.scene, (&job.keys, tiles), &mut Region::new());
    assert!(job
        .keys
        .iter()
        .all(|key| window.view.tiles.tile(key).is_some()));
}

#[test]
fn a_file_holding_what_a_drawing_cannot_keep_opens_read_only_and_names_its_formats() {
    let mut ids = Ids::new();
    let drawing = blank(&mut ids, &crate::preferences::Preferences::default()).expect("a drawing");
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
        view.refuse_save("art.svg", SaveTarget::Own).is_some(),
        "never written back over its own file"
    );
    assert!(
        view.refuse_save("art.svg", SaveTarget::Another).is_some(),
        "nor over a file of its name, which may be its own"
    );
    assert!(view.refuse_save("other.svg", SaveTarget::Another).is_none());
    assert!(
        view.refuse_save("art.png", SaveTarget::Another).is_some(),
        "PNG is a copy"
    );
    assert_eq!(CopyFormat::of("plan.PDF", None), Some(CopyFormat::Pdf));
    assert_eq!(CopyFormat::of("plan", None), None);
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
    let mut damage = Region::new();
    window.view.carry_out(Command::SelectAll, &mut damage);
    window.view.carry_out(Command::Group, &mut damage);
    let copied = window
        .view
        .selection_drawing()
        .expect("held")
        .expect("the group");
    window.view.pasted(Ok(copied), &mut damage);
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
    let ruler = window.view.layout.top_ruler;
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
    let copied = window
        .view
        .selection_drawing()
        .expect("held")
        .expect("the selection");
    let depth = window.view.document().depth();
    window.view.pasted(Ok(copied), &mut Region::new());
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
    let theme = Theme::light();
    let field = window
        .view
        .inspect(|inspector, shown, layout| {
            inspector.stroke_width_at((layout, Scale::ONE, &theme), shown)
        })
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
    let ruler = window.view.layout.top_ruler;
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
    let copied = window
        .view
        .selection_drawing()
        .expect("held")
        .expect("the selection");
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
    window.view.pasted(Ok(pasted), &mut Region::new());
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
    let asked = window.view.carry_out(Command::Union, &mut Region::new());
    let Some(Request::Own(Own::Compute(compute))) = asked.request else {
        panic!("a reshape for the queue");
    };
    let computed = crate::compute::work_out(
        *compute,
        &crate::glyphs::Lettering::new(crate::glyphs::Arrived),
    );
    window.text_at(500.0, 500.0);
    assert!(window.view.typing.is_some() && window.view.live.is_none());
    window.view.computed(computed, &mut Region::new());
    assert_eq!(window.objects(), 2, "the union landed");
}

#[test]
fn the_page_tab_sizes_turns_measures_and_grounds_the_page_each_as_a_step() {
    let mut window = Window::new();
    let zoom = window.view.viewport.pixels();
    let page = |window: &Window| *window.view.document().drawing().page();
    let a4 = page(&window);
    window.change(Change::Page(PageChange::Width(200.0)));
    assert_eq!(
        page(&window).width.to_bits(),
        200.0_f64.to_bits(),
        "any width, in the page's unit"
    );
    assert_eq!(window.view.document().undo_label(), Some("Page size"));
    assert_eq!(
        window.view.viewport.pixels().to_bits(),
        zoom.to_bits(),
        "the view stays where it is"
    );
    let depth = window.view.document().depth();
    window.change(Change::Page(PageChange::Landscape(false)));
    assert_eq!(window.view.document().depth(), depth, "already upright");
    window.change(Change::Page(PageChange::Landscape(true)));
    assert_eq!(
        (page(&window).width, page(&window).height),
        (a4.height, 200.0)
    );
    window.change(Change::Page(PageChange::Unit(tairix_drawing::Unit::Cm)));
    let measured = page(&window);
    assert_eq!(measured.unit, tairix_drawing::Unit::Cm);
    assert!((measured.height - 20.0).abs() < 1e-9, "the same size in cm");
    window.change(Change::Page(PageChange::GridSpacing(2.0)));
    let grid = window.view.document().drawing().grid();
    assert!((grid.spacing - 2.0 * tairix_drawing::Unit::Cm.pixels()).abs() < 1e-9);
    window.change(Change::Page(PageChange::Subdivisions(5)));
    assert_eq!(window.view.document().drawing().grid().subdivisions, 5);
    window.change(Change::Page(PageChange::Resolution(300)));
    assert_eq!(window.view.document().drawing().resolution(), 300);
    assert_eq!(window.view.document().undo_label(), Some("Copy resolution"));

    let depth = window.view.document().depth();
    let teal = Rgba::new(0, 128, 128, 255);
    for settled in [false, false, true] {
        window.change(Change::Page(PageChange::Ground {
            colour: Some(teal),
            settled,
        }));
        if !settled {
            assert_eq!(page(&window).background, None, "shown live only");
        }
    }
    assert_eq!(page(&window).background, Some(teal));
    assert_eq!(window.view.document().depth(), depth + 1, "one step");
    let width = page(&window).width;
    window.change(Change::Page(PageChange::Width(-1.0)));
    assert_eq!(
        page(&window).width.to_bits(),
        width.to_bits(),
        "a page with no width is refused"
    );
}

#[test]
fn the_ground_command_toggles_the_colour_the_page_tab_holds() {
    let mut window = Window::new();
    let page = |window: &Window| *window.view.document().drawing().page();
    assert_eq!(page(&window).background, None);
    window.command(Command::Ground);
    assert_eq!(page(&window).background, Some(tairix_drawing::PAPER_WHITE));
    let gold = Rgba::new(250, 200, 40, 255);
    window.change(Change::Page(PageChange::Ground {
        colour: Some(gold),
        settled: true,
    }));
    window.command(Command::Ground);
    assert_eq!(page(&window).background, None);
    window.command(Command::Ground);
    assert_eq!(
        page(&window).background,
        Some(gold),
        "the colour last given the page"
    );
    window.command(Command::Landscape);
    assert!(page(&window).width > page(&window).height);
    window.command(Command::PageSetup);
    assert_eq!(window.view.inspector.tab(), crate::inspector::Tab::Page);
}

/// The fill gradient of the object at `path`, which must have one.
fn fill_gradient(window: &Window, path: &[usize]) -> alloc::sync::Arc<tairix_drawing::Gradient> {
    let node = window
        .view
        .document()
        .drawing()
        .node_at(path)
        .expect("an object");
    match &node.style().expect("a style").fill {
        Paint::Gradient(gradient) => gradient.clone(),
        other => panic!("a gradient fill, not {other:?}"),
    }
}

/// The box of the rectangle at `path`, in its own space.
fn rect_box(window: &Window, path: &[usize]) -> tairix_bezier::Bounds {
    let node = window
        .view
        .document()
        .drawing()
        .node_at(path)
        .expect("an object");
    let Kind::Shape { shape, .. } = node.kind() else {
        panic!("a shape");
    };
    shape.bounds().expect("bounds")
}

fn linear_ends(gradient: &tairix_drawing::Gradient) -> (Point, Point) {
    match gradient.kind {
        tairix_drawing::GradientKind::Linear { start, end } => (start, end),
        tairix_drawing::GradientKind::Radial { .. } => panic!("a linear gradient"),
    }
}

#[test]
fn a_gradient_chosen_for_two_objects_is_fitted_to_each_and_edited_as_one_step() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Rectangle);
    window.drag((100.0, 100.0), (200.0, 150.0));
    window.drag((400.0, 300.0), (420.0, 400.0));
    window.command(Command::SelectAll);
    let form = crate::gradient::Form::Linear(0.0);
    window.change(Change::Form {
        target: Target::Fill,
        form,
    });
    assert_eq!(window.view.document().undo_label(), Some("Gradient"));
    for path in [[0, 0], [0, 1]] {
        let own = rect_box(&window, &path);
        let (start, end) = linear_ends(&fill_gradient(&window, &path));
        assert!(
            (start.x - own.min.x).abs() < 1e-6 && (end.x - own.max.x).abs() < 1e-6,
            "each across its own box: {start:?} {end:?} {own:?}"
        );
    }
    let left = rect_box(&window, &[0, 1]).min.x;

    let depth = window.view.document().depth();
    let first = fill_gradient(&window, &[0, 0]);
    let (stops, _) = crate::gradient::added(&first.stops, 0.5).expect("a stop");
    for settled in [false, false, true] {
        window.change(Change::Stops {
            target: Target::Fill,
            stops: stops.clone(),
            spread: tairix_drawing::Spread::Reflect,
            form,
            settled,
        });
    }
    assert_eq!(window.view.document().depth(), depth + 1, "one step");
    for path in [[0, 0], [0, 1]] {
        let gradient = fill_gradient(&window, &path);
        assert_eq!(gradient.stops.len(), 3);
        assert_eq!(gradient.spread, tairix_drawing::Spread::Reflect);
    }
    let (kept_start, _) = linear_ends(&fill_gradient(&window, &[0, 1]));
    assert!((kept_start.x - left).abs() < 1e-6, "where it lay is kept");
}

#[test]
fn a_shape_drawn_with_a_gradient_in_hand_takes_it_fitted_to_itself() {
    let mut window = Window::new();
    window.change(Change::Form {
        target: Target::Fill,
        form: crate::gradient::Form::Linear(90.0),
    });
    window.view.choose_tool(Tool::Rectangle);
    window.drag((300.0, 200.0), (340.0, 400.0));
    let own = rect_box(&window, &[0, 0]);
    let (start, end) = linear_ends(&fill_gradient(&window, &[0, 0]));
    assert!(
        (start.y - own.max.y).abs() < 1e-6 && (end.y - own.min.y).abs() < 1e-6,
        "upward across the new rectangle: {start:?} {end:?} {own:?}"
    );
}

#[test]
fn text_typed_with_a_gradient_in_hand_takes_it_fitted_once_typed() {
    let mut window = Window::new();
    window.change(Change::Form {
        target: Target::Fill,
        form: crate::gradient::Form::Linear(0.0),
    });
    window.text_at(100.0, 300.0);
    for character in "Gradient".chars() {
        window.key(Key::Char(character));
    }
    window.key(Key::Named(NamedKey::Escape));
    let (start, end) = linear_ends(&fill_gradient(&window, &[0, 0]));
    assert!(
        end.x - start.x > 20.0,
        "across what was typed: {start:?} {end:?}"
    );
}

#[test]
fn a_rotation_turns_one_object_to_its_angle_and_several_together() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Rectangle);
    window.drag((100.0, 100.0), (200.0, 150.0));
    window.change(Change::Rotation(90.0));
    assert_eq!(window.view.document().undo_label(), Some("Rotate"));
    let drawing = window.view.document().drawing().clone();
    let angle = crate::edit::angle_of(&drawing, &[vec![0, 0]]);
    assert!((angle - 90.0).abs() < 1e-9, "{angle}");
    let depth = window.view.document().depth();
    window.change(Change::Rotation(90.0));
    assert_eq!(
        window.view.document().depth(),
        depth,
        "already there: no step"
    );
    window.change(Change::Rotation(-45.0));
    let drawing = window.view.document().drawing().clone();
    let angle = crate::edit::angle_of(&drawing, &[vec![0, 0]]);
    assert!((angle + 45.0).abs() < 1e-9, "{angle}");
}

#[test]
fn swatches_are_kept_once_each_and_dropped_as_steps() {
    let mut window = Window::new();
    let teal = Rgba::new(0, 128, 128, 255);
    window.change(Change::KeepSwatch(teal));
    assert_eq!(window.view.document().drawing().swatches(), &[teal]);
    assert_eq!(window.view.document().undo_label(), Some("Add swatch"));
    let depth = window.view.document().depth();
    window.change(Change::KeepSwatch(teal));
    assert_eq!(window.view.document().depth(), depth, "kept once");
    window.change(Change::DropSwatch(0));
    assert!(window.view.document().drawing().swatches().is_empty());
    window.command(Command::Undo);
    assert_eq!(window.view.document().drawing().swatches(), &[teal]);
}

#[test]
fn a_gradient_taken_with_the_eyedropper_is_fitted_to_each_object_given_it() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Rectangle);
    window.drag((100.0, 100.0), (200.0, 150.0));
    window.change(Change::Form {
        target: Target::Fill,
        form: crate::gradient::Form::Linear(0.0),
    });
    window.command(Command::SelectNone);
    window.change(Change::Paint {
        target: Target::Fill,
        paint: Paint::Colour(Rgba::new(200, 0, 0, 255)),
        settled: true,
    });
    window.view.choose_tool(Tool::Rectangle);
    window.drag((400.0, 300.0), (420.0, 400.0));
    assert!(matches!(
        window
            .view
            .document()
            .drawing()
            .node_at(&[0, 1])
            .and_then(|node| node.style())
            .map(|style| style.fill.clone()),
        Some(Paint::Colour(_))
    ));
    window.view.choose_tool(Tool::Eyedropper);
    let source = window.at(150.0, 125.0);
    window.feed(InputEvent::PointerMoved { to: source });
    window.feed(InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    window.feed(InputEvent::PointerReleased {
        button: PointerButton::Primary,
    });
    assert_eq!(window.view.document().undo_label(), Some("Take style"));
    let own = rect_box(&window, &[0, 1]);
    let (start, end) = linear_ends(&fill_gradient(&window, &[0, 1]));
    assert!(
        (start.x - own.min.x).abs() < 1e-6 && (end.x - own.max.x).abs() < 1e-6,
        "across the object given it: {start:?} {end:?} {own:?}"
    );
}

#[test]
fn a_drag_with_the_gradient_tool_lays_a_gradient_live_and_lands_once() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Rectangle);
    window.drag((100.0, 100.0), (300.0, 200.0));
    window.view.choose_tool(Tool::Gradient);
    let depth = window.view.document().depth();
    let (a, b) = (window.at(120.0, 150.0), window.at(280.0, 150.0));
    window.feed(InputEvent::PointerMoved { to: a });
    window.feed(InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    window.feed(InputEvent::PointerMoved { to: b });
    assert_eq!(window.view.document().depth(), depth, "shown live only");
    assert!(matches!(
        window
            .view
            .shown()
            .node_at(&[0, 0])
            .and_then(|node| node.style())
            .map(|style| &style.fill),
        Some(Paint::Gradient(_))
    ));
    window.feed(InputEvent::PointerReleased {
        button: PointerButton::Primary,
    });
    assert_eq!(window.view.document().depth(), depth + 1, "one step");
    assert_eq!(window.view.document().undo_label(), Some("Gradient"));
    let (start, end) = linear_ends(&fill_gradient(&window, &[0, 0]));
    let drawn = |at: tairix_geometry::Point| {
        window
            .view
            .drawing_at(Point::new(f64::from(at.x), f64::from(at.y)))
    };
    assert!(start.distance(drawn(a)) < 1e-6 && end.distance(drawn(b)) < 1e-6);
}

#[test]
fn a_gradient_laid_on_a_turned_object_lies_where_it_was_dragged() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Rectangle);
    window.drag((100.0, 100.0), (300.0, 200.0));
    window.change(Change::Rotation(30.0));
    window.view.choose_tool(Tool::Gradient);
    window.modifier_alt(true);
    window.drag((200.0, 150.0), (260.0, 150.0));
    window.modifier_alt(false);
    let gradient = fill_gradient(&window, &[0, 0]);
    let tairix_drawing::GradientKind::Radial { centre, .. } = gradient.kind else {
        panic!("Alt lays a radial gradient");
    };
    let node = window
        .view
        .document()
        .drawing()
        .node_at(&[0, 0])
        .expect("the rectangle");
    let on_page = node.transform().map(centre);
    let pressed = window.view.drawing_at({
        let at = window.at(200.0, 150.0);
        Point::new(f64::from(at.x), f64::from(at.y))
    });
    assert!(
        on_page.distance(pressed) < 1e-6,
        "its centre where the drag began: {on_page:?} {pressed:?}"
    );
}

#[test]
fn a_click_with_the_gradient_tool_lays_nothing() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Rectangle);
    window.drag((100.0, 100.0), (300.0, 200.0));
    window.view.choose_tool(Tool::Gradient);
    let depth = window.view.document().depth();
    window.drag((150.0, 150.0), (150.0, 150.0));
    assert_eq!(window.view.document().depth(), depth);
    assert!(window.view.live.is_none(), "nothing left under way");
}

#[test]
fn a_window_begins_with_the_tool_view_and_style_its_preferences_name() {
    let mut window = Window::new();
    let preferences = crate::preferences::Preferences {
        tool: Tool::Pen,
        show: crate::view::Showing {
            outline: true,
            grid: true,
            snapping: true,
        },
        fill: None,
        stroke: Some(Rgba::new(10, 20, 30, 255)),
        stroke_width: 250,
        ..crate::preferences::Preferences::default()
    };
    window.view.begin(&preferences);
    assert_eq!(window.view.tool, Tool::Pen);
    assert_eq!(window.view.show, preferences.show);
    assert_eq!(window.view.style, preferences.style());
    window.view.choose_tool(Tool::Rectangle);
    window.drag((100.0, 100.0), (200.0, 200.0));
    let drawn = window
        .view
        .document()
        .drawing()
        .node_at(&window.view.selection[0])
        .and_then(|node| node.style().cloned())
        .expect("a styled rectangle");
    assert_eq!(drawn.fill, Paint::None, "drawn in the style begun with");
}

#[test]
fn the_keys_move_things_as_far_as_the_preferences_adopted_say() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Rectangle);
    window.drag((100.0, 100.0), (200.0, 200.0));
    let preferences = crate::preferences::Preferences {
        nudge: 250,
        duplicate_offset: 1250,
        ..crate::preferences::Preferences::default()
    };
    window.view.adopt(&preferences);
    let before = window.view.selection_bounds();
    window.key(Key::Named(NamedKey::Right));
    let nudged = window.view.selection_bounds();
    assert!(
        (nudged.min.x - before.min.x - 2.5).abs() < 1e-9,
        "{nudged:?}"
    );
    window.command(Command::Duplicate);
    let copy = window.view.selection_bounds();
    assert!((copy.min.x - nudged.min.x - 12.5).abs() < 1e-9, "{copy:?}");
    assert!((copy.min.y - nudged.min.y - 12.5).abs() < 1e-9, "{copy:?}");
}

/// A picture of one colour whose natural size is `width` by `height`.
fn picture(width: f64, height: f64) -> tairix_drawing::Image {
    let mut pixels = tairix_raster::Surface::new(4, 2).expect("a surface");
    pixels.fill(tairix_raster::Color::rgb(30, 120, 200));
    let place = tairix_bezier::Bounds::from_points([Point::ZERO, Point::new(width, height)]);
    tairix_drawing::Image::new(pixels, place, None).expect("a picture")
}

#[test]
fn place_picture_asks_for_a_file_to_read_in_and_a_placed_one_lands_selected_in_the_middle() {
    let mut window = Window::new();
    let layout = window.view.layout;
    let asked = window
        .view
        .carry_out(Command::PlacePicture, &mut Region::new())
        .request;
    assert!(matches!(asked, Some(Request::Import)), "{asked:?}");
    let shown = {
        let canvas = Point::new(
            f64::from(layout.canvas.width),
            f64::from(layout.canvas.height),
        );
        tairix_bezier::Bounds::from_points([
            window.view.viewport.drawing_point(Point::ZERO),
            window.view.viewport.drawing_point(canvas),
        ])
    };
    let depth = window.view.document().depth();
    window.view.placed_picture(
        ("photo.png", None),
        Ok(picture(40.0, 20.0)),
        &mut Region::new(),
    );
    assert_eq!(window.view.document().depth(), depth + 1, "one step");
    assert_eq!(window.view.selection, vec![vec![0, 0]]);
    let node = window
        .view
        .document()
        .drawing()
        .node_at(&[0, 0])
        .expect("the picture");
    assert!(matches!(node.kind(), Kind::Image { .. }));
    assert_eq!(node.name(), Some("photo"), "named after its file");
    let bounds = window.view.selection_bounds();
    assert!(
        (bounds.width() - 40.0).abs() < 1e-9,
        "at its own size: {bounds:?}"
    );
    let (middle, centre) = (bounds.centre(), shown.centre());
    assert!((middle.x - centre.x).abs() < 1e-6 && (middle.y - centre.y).abs() < 1e-6);
}

#[test]
fn a_picture_larger_than_the_canvas_is_placed_to_fit_and_one_that_failed_is_said() {
    let mut window = Window::new();
    let layout = window.view.layout;
    window.view.placed_picture(
        ("huge.jpg", None),
        Ok(picture(20_000.0, 10_000.0)),
        &mut Region::new(),
    );
    let canvas = Point::new(
        f64::from(layout.canvas.width),
        f64::from(layout.canvas.height),
    );
    let shown = tairix_bezier::Bounds::from_points([
        window.view.viewport.drawing_point(Point::ZERO),
        window.view.viewport.drawing_point(canvas),
    ]);
    let bounds = window.view.selection_bounds();
    assert!(bounds.width() <= shown.width() && bounds.height() <= shown.height());
    let depth = window.view.document().depth();
    window.view.placed_picture(
        ("broken.png", None),
        Err(String::from("it is damaged")),
        &mut Region::new(),
    );
    assert_eq!(window.view.document().depth(), depth, "nothing lands");
    assert_eq!(
        window.view.message.as_deref(),
        Some("broken.png could not be placed: it is damaged")
    );
}

#[test]
fn a_picture_dropped_on_the_canvas_is_placed_centred_where_it_was_let_go() {
    let mut window = Window::new();
    let layout = window.view.layout;
    let at = window.at(150.0, 120.0);
    let depth = window.view.document().depth();
    window.view.placed_picture(
        ("photo.png", Some(at)),
        Ok(picture(40.0, 20.0)),
        &mut Region::new(),
    );
    assert_eq!(window.view.document().depth(), depth + 1, "one step");
    let bounds = window.view.selection_bounds();
    let middle = bounds.centre();
    assert!(
        (middle.x - 150.0).abs() < 1.0 && (middle.y - 120.0).abs() < 1.0,
        "centred where it was dropped: {bounds:?}"
    );
    assert!((bounds.width() - 40.0).abs() < 1e-9, "at its own size");
    let off = tairix_geometry::Point::new(layout.panel.left() + 4, layout.panel.top() + 4);
    window.view.placed_picture(
        ("photo.png", Some(off)),
        Ok(picture(40.0, 20.0)),
        &mut Region::new(),
    );
    let shown = tairix_bezier::Bounds::from_points([
        window.view.viewport.drawing_point(Point::ZERO),
        window.view.viewport.drawing_point(Point::new(
            f64::from(layout.canvas.width),
            f64::from(layout.canvas.height),
        )),
    ]);
    let middle = window.view.selection_bounds().centre();
    assert!(
        (middle.x - shown.centre().x).abs() < 1e-6 && (middle.y - shown.centre().y).abs() < 1e-6,
        "a point off the canvas places it in the middle of what the canvas shows"
    );
}

#[test]
fn the_canvas_places_a_picture_dropped_on_it_and_nothing_else() {
    let window = Window::new();
    let layout = window.view.layout;
    let canvas = tairix_geometry::Point::new(
        layout.canvas.left() + px(layout.canvas.width) / 2,
        layout.canvas.top() + px(layout.canvas.height) / 2,
    );
    for picture in [
        "image/png",
        "image/jpeg",
        "image/webp",
        "image/openraster",
        "image/x-riscos-sprite",
    ] {
        assert!(window.view.places(canvas, picture), "{picture} is placed");
    }
    for other in [
        "image/svg+xml",
        "text/plain",
        "application/pdf",
        "not a type",
    ] {
        assert!(
            !window.view.places(canvas, other),
            "{other} is opened or refused, never placed"
        );
    }
    let inspector = tairix_geometry::Point::new(layout.panel.left() + 4, layout.panel.top() + 4);
    assert!(
        !window.view.places(inspector, "image/png"),
        "only the canvas places"
    );
}

impl Window {
    fn shifted(&mut self, key: Key) {
        self.feed(InputEvent::KeyPressed {
            key,
            modifiers: Modifiers {
                shift: true,
                ..Modifiers::default()
            },
        });
    }

    /// The text shown at `path`, typed or landed.
    fn text_shown(&self, path: &[usize]) -> alloc::sync::Arc<tairix_drawing::Text> {
        let node = self.view.shown().node_at(path).expect("the text");
        let Kind::Text { text, .. } = node.kind() else {
            panic!("a text object");
        };
        text.clone()
    }
}

#[test]
fn a_selection_made_while_typing_is_styled_as_a_run_and_typed_over_in_its_style() {
    use crate::inspector::TextChange;

    let mut window = Window::new();
    window.text_at(100.0, 100.0);
    for character in "Hello".chars() {
        window.key(Key::Char(character));
    }
    window.shifted(Key::Named(NamedKey::Left));
    window.shifted(Key::Named(NamedKey::Left));
    assert_eq!(
        window.view.typed(),
        Some(3..5),
        "shift carries the selection"
    );
    window.change(Change::Text(TextChange::Bold(true)));
    window.change(Change::Paint {
        target: Target::Fill,
        paint: Paint::Colour(Rgba::new(200, 0, 0, 255)),
        settled: true,
    });
    let styled = window.text_shown(&[0, 0]);
    assert_eq!(styled.weight, 400, "the text itself stays as it was");
    assert_eq!(styled.runs.len(), 1);
    let run = &styled.runs[0];
    assert_eq!((run.start, run.end, run.style.weight), (3, 5, Some(700)));
    assert_eq!(
        run.style.fill,
        Some(tairix_drawing::RunPaint::Colour(Rgba::new(200, 0, 0, 255)))
    );
    window.key(Key::Char('p'));
    let typed = window.text_shown(&[0, 0]);
    assert_eq!(&*typed.content, "Help");
    assert_eq!(
        (typed.runs[0].start, typed.runs[0].end),
        (3, 4),
        "typed over in the selection's own style"
    );
    window.key(Key::Named(NamedKey::Left));
    window.key(Key::Named(NamedKey::Left));
    window.key(Key::Named(NamedKey::Backspace));
    assert_eq!(&*window.text_shown(&[0, 0]).content, "Hlp");
    window.key(Key::Named(NamedKey::Escape));
    assert_eq!(window.view.document().depth(), 1, "one step for all of it");
}

#[test]
fn a_style_given_the_whole_text_reaches_every_character_of_it() {
    use crate::inspector::TextChange;

    let mut window = Window::new();
    window.text_at(100.0, 100.0);
    for character in "Hello".chars() {
        window.key(Key::Char(character));
    }
    window.shifted(Key::Named(NamedKey::Home));
    window.shifted(Key::Named(NamedKey::Right));
    window.change(Change::Text(TextChange::Size(40.0)));
    window.change(Change::Text(TextChange::Raise(0.5)));
    assert_eq!(window.text_shown(&[0, 0]).runs.len(), 1);
    window.key(Key::Named(NamedKey::Escape));
    window.change(Change::Text(TextChange::Size(30.0)));
    let whole = window.text_shown(&[0, 0]);
    assert!((whole.size - 30.0).abs() < 1e-9);
    assert!(
        whole.runs.iter().all(|run| run.style.size.is_none()),
        "no run keeps a size of its own: {:?}",
        whole.runs
    );
    assert!(whole
        .runs
        .iter()
        .any(|run| run.style.baseline_shift == Some(0.5)));
    window.change(Change::Paint {
        target: Target::Fill,
        paint: Paint::Colour(Rgba::new(0, 0, 200, 255)),
        settled: true,
    });
    assert!(window
        .text_shown(&[0, 0])
        .runs
        .iter()
        .all(|run| run.style.fill.is_none()));
}

#[test]
fn a_text_set_on_a_path_runs_along_it_and_released_is_given_its_path_back() {
    let mut window = Window::new();
    window.text_at(100.0, 100.0);
    for character in "Way".chars() {
        window.key(Key::Char(character));
    }
    window.key(Key::Named(NamedKey::Escape));
    window.view.choose_tool(Tool::Line);
    window.drag((50.0, 300.0), (400.0, 300.0));
    assert_eq!(window.objects(), 2);
    window.command(Command::SelectAll);
    window.command(Command::TextOnPath);
    assert_eq!(window.objects(), 1, "the path joined the text");
    let along = window.text_shown(&[0, 0]);
    let tairix_drawing::Baseline::Path(path) = &along.baseline else {
        panic!("set along the path");
    };
    let start = path.first_point();
    assert!(
        start.distance(Point::new(50.0, 300.0)) < 2.0,
        "where the line began: {start:?}"
    );
    assert_eq!(&*along.content, "Way");
    window.command(Command::ReleaseFromPath);
    assert_eq!(window.objects(), 2, "the path given back");
    let released = window.text_shown(&[0, 0]);
    assert!(matches!(
        released.baseline,
        tairix_drawing::Baseline::Lines { origin, .. } if origin == start
    ));
    let given = window
        .view
        .document()
        .drawing()
        .node_at(&[0, 1])
        .expect("the path");
    assert!(matches!(given.kind(), Kind::Shape { .. }));
    assert_eq!(window.view.selection.len(), 2, "both selected");
    window.command(Command::Undo);
    window.command(Command::Undo);
    assert_eq!(window.objects(), 2, "each a step of its own");
}

#[test]
fn a_press_puts_the_caret_between_characters_and_a_drag_selects_across_them() {
    let mut window = Window::new();
    window.text_at(100.0, 100.0);
    for character in "Hello".chars() {
        window.key(Key::Char(character));
    }
    window.key(Key::Named(NamedKey::Escape));
    // No glyph has arrived, so each character stands its unknown advance
    // wide: 0.6 of the 24-pixel size.
    let wide = 0.6 * 24.0;
    window.view.choose_tool(Tool::Text);
    window.drag(
        (100.0 + 2.0 * wide + 1.0, 95.0),
        (100.0 + 4.0 * wide - 1.0, 95.0),
    );
    assert_eq!(window.view.typed(), Some(2..4));
    window.key(Key::Char('-'));
    assert_eq!(&*window.text_shown(&[0, 0]).content, "He-o");
}

#[test]
fn a_node_of_the_path_a_text_is_set_along_moves_as_a_paths_does() {
    use alloc::sync::Arc;

    use tairix_drawing::{Baseline, PathShape, TextPath};

    let mut ids = Ids::new();
    let line = tairix_bezier::Path {
        subpaths: vec![{
            let mut subpath = tairix_bezier::Subpath::new(Point::new(10.0, 10.0));
            subpath
                .elements
                .push(tairix_bezier::PathEl::Line(Point::new(110.0, 10.0)));
            subpath
        }],
    };
    let text = tairix_drawing::Text {
        baseline: Baseline::Path(Arc::new(TextPath {
            path: Arc::new(PathShape::from_path(&line).expect("a path")),
            start: 4.0,
        })),
        ..crate::testing::text("On it", Point::ZERO)
    };
    let node = tairix_drawing::Node::new(
        ids.issue(),
        Kind::Text {
            text: Arc::new(text),
            style: Arc::new(tairix_drawing::Style::default()),
        },
    )
    .expect("a text");
    let drawing = blank(&mut ids, &crate::preferences::Preferences::default()).expect("a drawing");
    let (drawing, at) = crate::edit::insert(&drawing, &[0], Arc::new(node)).expect("put");
    let placed = drawing.node_at(&at).expect("the text");
    let grip = crate::view::Grip {
        contour: 0,
        node: 1,
        part: crate::view::GripPart::Point,
    };
    let moved = super::moved_node(placed, &drawing, &at, grip, Point::new(110.0, 60.0))
        .expect("moved")
        .expect("a path to move");
    let Kind::Text { text, .. } = moved.kind() else {
        panic!("still a text");
    };
    let Baseline::Path(along) = &text.baseline else {
        panic!("still along its path");
    };
    assert_eq!(
        along.path.contours[0].nodes[1].point,
        Point::new(110.0, 60.0)
    );
    assert!((along.start - 4.0).abs() < 1e-12, "its start kept");
    assert_eq!(&*text.content, "On it");
}

impl Window {
    /// The whole window as the view paints it now.
    fn rendered(&self) -> tairix_raster::Surface {
        let mut surface = tairix_raster::Surface::new(
            self.view.layout.window.width,
            self.view.layout.window.height,
        )
        .expect("a surface");
        let faces = crate::layout::Faces::of(&self.theme, Scale::ONE);
        self.view.render(
            &mut surface,
            (&self.theme, Scale::ONE, faces),
            &mut tairix_icon::NoArtwork,
        );
        surface
    }

    /// Whether `surface` shows the accent within a pixel of drawing point
    /// `at`, across or down.
    fn accent_near(&self, surface: &tairix_raster::Surface, at: (f64, f64)) -> bool {
        let accent = tairix_raster::Color::from(self.theme.palette().accent).premultiply();
        let centre = self.at(at.0, at.1);
        (-1..=1).any(|dy| {
            (-1..=1).any(|dx| {
                let (Ok(x), Ok(y)) = (u32::try_from(centre.x + dx), u32::try_from(centre.y + dy))
                else {
                    return false;
                };
                surface.get(x, y) == Some(accent)
            })
        })
    }

    /// Press at drawing point `from` and move to `to`, holding the button.
    fn hold_drag(&mut self, from: (f64, f64), to: (f64, f64)) {
        let (a, b) = (self.at(from.0, from.1), self.at(to.0, to.1));
        self.feed(InputEvent::PointerMoved { to: a });
        self.feed(InputEvent::PointerPressed {
            button: PointerButton::Primary,
        });
        self.feed(InputEvent::PointerMoved { to: b });
    }
}

#[test]
fn a_marquee_dragged_out_is_drawn_while_it_is_held() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Select);
    window.hold_drag((300.0, 300.0), (500.0, 450.0));
    assert!(
        matches!(
            window.view.gesture,
            Some(crate::view::Gesture::Marquee { .. })
        ),
        "{:?}",
        window.view.gesture
    );
    assert!(
        window.accent_near(&window.rendered(), (400.0, 300.0)),
        "the marquee's top edge"
    );
}

#[test]
fn an_object_dragged_is_drawn_where_it_is_going_while_it_is_held() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Rectangle);
    window.drag((100.0, 100.0), (200.0, 200.0));
    window.view.choose_tool(Tool::Select);
    window.hold_drag((150.0, 150.0), (150.0, 350.0));
    let surface = window.rendered();
    assert!(
        window.accent_near(&surface, (150.0, 300.0)),
        "the outline's top edge where it is going"
    );
    assert!(
        window.accent_near(&surface, (100.0, 350.0)),
        "the outline's left edge where it is going"
    );
}

#[test]
fn the_page_s_shadow_is_opaque_and_darker_than_the_pasteboard() {
    let window = Window::new();
    let surface = window.rendered();
    let page = window
        .view
        .window_rect(window.view.shown().page().bounds())
        .expect("the page is shown");
    let pixel = |x: i32, y: i32| {
        surface
            .get(
                u32::try_from(x).expect("on the window"),
                u32::try_from(y).expect("on the window"),
            )
            .expect("on the surface")
    };
    let shadow = pixel(page.right() + 1, page.top() + px(page.height) / 2);
    let pasteboard = pixel(page.right() + 20, page.top() + px(page.height) / 2);
    assert_eq!(shadow.a, 255, "the shadow leaves no hole: {shadow:?}");
    assert_eq!(pasteboard.a, 255, "{pasteboard:?}");
    assert!(
        u16::from(shadow.r) + u16::from(shadow.g) + u16::from(shadow.b)
            < u16::from(pasteboard.r) + u16::from(pasteboard.g) + u16::from(pasteboard.b),
        "darker: {shadow:?} against {pasteboard:?}"
    );
}

#[test]
fn the_status_band_states_the_tool_s_hint_and_its_zoom_opens_the_zoom_menu() {
    let mut window = Window::new();
    window.view.choose_tool(Tool::Pencil);
    let surface = window.rendered();
    let row = |slot: tairix_geometry::Rect| {
        let y = u32::try_from(slot.top() + px(slot.height) / 2).expect("on the window");
        (0..slot.width).any(|dx| {
            let x = u32::try_from(slot.left()).expect("on the window") + dx;
            surface.get(x, y) != surface.get(x, u32::try_from(slot.top()).expect("on the window"))
        })
    };
    assert!(
        row(window.view.layout.message),
        "the hint is set in the band"
    );
    assert!(row(window.view.layout.zoom), "the zoom is set in the band");
    let zoom = window.view.layout.zoom;
    let centre = tairix_geometry::Point::new(
        zoom.left() + px(zoom.width) / 2,
        zoom.top() + px(zoom.height) / 2,
    );
    window.feed(InputEvent::PointerMoved { to: centre });
    let asked = window.feed(InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    assert!(
        matches!(
            asked,
            Some(Request::Menu {
                kind: crate::view::MenuKind::Zoom,
                anchor,
            }) if anchor == zoom
        ),
        "{asked:?}"
    );
}

/// A PDF of two blank pages.
fn two_page_pdf() -> alloc::vec::Vec<u8> {
    let objects = [
        "<</Type /Catalog /Pages 2 0 R>>",
        "<</Type /Pages /Kids [3 0 R 4 0 R] /Count 2>>",
        "<</Type /Page /Parent 2 0 R /MediaBox [0 0 200 100]>>",
        "<</Type /Page /Parent 2 0 R /MediaBox [0 0 300 400]>>",
    ];
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = alloc::vec::Vec::new();
    for (num, body) in (1..).zip(objects) {
        offsets.push(out.len());
        out.extend_from_slice(alloc::format!("{num} 0 obj\n{body}\nendobj\n").as_bytes());
    }
    let table = out.len();
    let size = objects.len() + 1;
    out.extend_from_slice(alloc::format!("xref\n0 {size}\n0000000000 65535 f \n").as_bytes());
    for offset in offsets {
        out.extend_from_slice(alloc::format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        alloc::format!("trailer\n<</Size {size} /Root 1 0 R>>\nstartxref\n{table}\n%%EOF\n")
            .as_bytes(),
    );
    out
}

/// One page of a longer PDF opens read-only, so Save never writes that page
/// alone back over the whole document.
#[test]
fn a_page_of_a_longer_pdf_is_never_saved_over_its_document() {
    let mut ids = Ids::new();
    let request = tairix_drawing::pdf::Request {
        page: 1,
        password: None,
        budget: 1 << 20,
    };
    let read = tairix_drawing::read(two_page_pdf(), &request, &mut ids).expect("page two");
    assert!(read.unkept.contains(Lost::OtherPages));
    let view = View::new(
        Document::new(read.drawing, ids, true),
        String::from("plan.pdf"),
        Access::Writable,
        read.unkept,
        1.0,
    );
    assert_eq!(view.access(), Access::ReadOnly);
    assert!(view.refuse_save("plan.pdf", SaveTarget::Own).is_some());
    assert!(
        view.refuse_save("page two.pdf", SaveTarget::Another)
            .is_none(),
        "saved under another name"
    );
}

#[test]
fn a_drawing_saves_as_svg_a_draw_file_or_pdf_and_refuses_any_other_name() {
    let mut window = Window::new();
    let view = &mut window.view;
    for saved in ["a.svg", "a.pdf", "a,aff"] {
        assert_eq!(
            view.refuse_save(saved, SaveTarget::Another),
            None,
            "{saved}"
        );
    }
    for refused in ["a.png", "a.eps", "a"] {
        assert!(
            view.refuse_save(refused, SaveTarget::Another).is_some(),
            "{refused}"
        );
    }
    let formats = view.save_formats().expect("offered");
    assert_eq!(formats.len(), 3);
    let current = formats.current().expect("one is current");
    assert_eq!(formats.of("a.svg"), Some(current), "a new drawing is SVG");
    for named in ["a.pdf", "a,aff"] {
        assert!(formats.of(named).is_some(), "{named} offered");
    }
    view.name = String::from("Drawing");
    view.recorded = tairix_abi::fs::Filetype::new(0xaff);
    assert_eq!(
        view.refuse_save("Drawing", SaveTarget::Own),
        None,
        "its own file's recorded type says it is a Draw file"
    );
    assert!(
        view.refuse_save("Drawing", SaveTarget::Another).is_some(),
        "a new file of its name records nothing, so its name says nothing"
    );
    assert!(
        view.refuse_save("Other", SaveTarget::Another).is_some(),
        "another file's name says nothing"
    );
    let formats = view.save_formats().expect("offered");
    assert_eq!(
        formats.current(),
        formats.of("a,aff"),
        "a Draw file is saved as one again"
    );
}

/// A window's title names its drawing once, marked as any document window's
/// is — its unsaved changes, and a file it may not save over — then Draw.
#[test]
fn a_window_s_title_names_its_drawing_marked_as_any_document_s() {
    let mut window = Window::new();
    let title = |view: &View| {
        let mut title = String::new();
        DocumentView::write_title(view, &mut title);
        title
    };
    assert_eq!(title(&window.view), "Untitled \u{2014} Draw");
    window.view.choose_tool(Tool::Rectangle);
    window.drag((100.0, 100.0), (300.0, 200.0));
    assert_eq!(title(&window.view), "*Untitled \u{2014} Draw");
    window.view.set_access(Access::ReadOnly);
    assert_eq!(title(&window.view), "*Untitled (read-only) \u{2014} Draw");
}

/// Every window point a sweep of the pointer visits, the last row and column
/// — the window's edges — among them.
fn sweep(layout: &crate::layout::Layout) -> impl Iterator<Item = tairix_geometry::Point> {
    let edge = |length: u32| i32::try_from(length).expect("a window's size") - 1;
    let (right, bottom) = (edge(layout.window.width), edge(layout.window.height));
    let rows = (0..bottom).step_by(3).chain(core::iter::once(bottom));
    rows.flat_map(move |y| {
        let columns = (0..right).step_by(3).chain(core::iter::once(right));
        columns.map(move |x| tairix_geometry::Point::new(x, y))
    })
}

/// A move with nothing held repaints the status's reading of the pointer when
/// that changes, and a control the pointer comes to or leaves: never the
/// canvas, its rulers or the window around them.
#[test]
fn a_hover_repaints_the_pointer_s_reading_and_the_controls_it_crosses_only() {
    let mut window = Window::new();
    let layout = window.view.layout;
    let inside = |rect: Rect, of: Rect| of.intersection(&rect) == rect;
    let controls = [
        layout.tools,
        layout.horizontal_bar,
        layout.vertical_bar,
        layout.bar_corner,
        layout.tabs,
        layout.panel,
    ];
    let mut readings = 0_usize;
    for to in sweep(&layout) {
        let reading = window.view.pointer_reading();
        let mut damage = Region::new();
        let event = InputEvent::PointerMoved { to };
        let _ = window
            .view
            .on_input(&event, Scale::ONE, &window.theme, &mut damage);
        for &rect in damage.rects() {
            let on_control = controls.iter().any(|&control| inside(rect, control));
            assert!(
                on_control || inside(rect, layout.pointer),
                "a move to {to:?} repainted {rect:?}"
            );
        }
        let read_again = window.view.pointer_reading() != reading;
        let shown = damage
            .rects()
            .iter()
            .any(|&rect| inside(rect, layout.pointer));
        assert_eq!(
            shown, read_again,
            "the pointer's reading after a move to {to:?}"
        );
        readings += usize::from(read_again);
    }
    assert!(readings > 0, "the reading follows the pointer");
}

/// The pointer leaving the window across an edge, with nothing held, puts out
/// the light of the control it was over and repaints nothing else.
#[test]
fn the_pointer_leaving_repaints_only_the_control_it_left() {
    let mut window = Window::new();
    let layout = window.view.layout;
    let leave = |window: &mut Window, to: tairix_geometry::Point| {
        let mut damage = Region::new();
        let event = InputEvent::PointerMoved { to };
        let _ = window
            .view
            .on_input(&event, Scale::ONE, &window.theme, &mut damage);
        let lit = !damage
            .rects()
            .iter()
            .all(|&rect| layout.pointer.intersection(&rect) == rect);
        let mut damage = Region::new();
        let _ = window.view.on_input(
            &InputEvent::PointerCancelled,
            Scale::ONE,
            &window.theme,
            &mut damage,
        );
        (lit, damage)
    };
    let canvas = layout.canvas;
    let middle = tairix_geometry::Point::new(
        canvas.left() + i32::try_from(canvas.width / 2).expect("a canvas"),
        canvas.top() + i32::try_from(canvas.height / 2).expect("a canvas"),
    );
    let (_, left) = leave(&mut window, middle);
    assert!(left.is_empty(), "leaving the canvas repaints nothing");
    let button = (layout.tools.top()..layout.tools.bottom())
        .map(|y| tairix_geometry::Point::new(layout.tools.left() + 12, y))
        .find(|&at| {
            let (lit, _) = leave(&mut window, at);
            lit
        })
        .expect("a tool button");
    let (lit, left) = leave(&mut window, button);
    assert!(lit);
    assert!(!left.is_empty(), "the button's light goes out");
    for &rect in left.rects() {
        assert_eq!(
            layout.tools.intersection(&rect),
            rect,
            "only the button repaints"
        );
    }
}

/// How many pixels `damage` repaints.
fn area(damage: &Region) -> u64 {
    damage
        .rects()
        .iter()
        .map(|rect| u64::from(rect.width) * u64::from(rect.height))
        .sum()
}

/// Every pixel `after` shows differently from `before` lies in `damage`.
fn assert_repainted(
    before: &tairix_raster::Surface,
    after: &tairix_raster::Surface,
    damage: &Region,
    what: &dyn core::fmt::Debug,
) {
    let width = usize::try_from(before.width()).expect("a width");
    let changed = before.pixels().iter().zip(after.pixels()).enumerate();
    for (at, _) in changed.filter(|(_, (was, now))| was != now) {
        let x = i32::try_from(at % width).expect("on the window");
        let y = i32::try_from(at / width).expect("on the window");
        let point = tairix_geometry::Point::new(x, y);
        assert!(
            damage.contains(point),
            "{what:?} changed {point:?} beyond what it repainted: {:?}",
            damage.rects()
        );
    }
}

impl Window {
    /// Feed `event` as the host does, holding it to repaint every pixel it
    /// changes, and answer what it repainted.
    fn feed_repainted(&mut self, event: InputEvent) -> Region {
        let before = self.rendered();
        let mut damage = Region::new();
        let _ = self
            .view
            .on_input(&event, Scale::ONE, &self.theme, &mut damage);
        let what = (&event, &self.view.gesture, self.view.tool);
        assert_repainted(&before, &self.rendered(), &damage, &what);
        damage
    }

    /// How many pixels of the canvas `damage` repaints.
    fn on_canvas(&self, mut damage: Region) -> u64 {
        damage.clip(self.view.layout.canvas);
        area(&damage)
    }

    /// Hold the modifiers `modifiers` down, as the host says they are.
    fn hold(&mut self, modifiers: Modifiers) {
        self.feed_repainted(InputEvent::ModifiersChanged { modifiers });
    }

    /// Press at window point `at`, move through `path` — drawing points —
    /// with the modifiers `held` changing between moves where a step names
    /// them, and let go, each step holding the window to repaint every pixel
    /// it changes. Answers the most of the canvas any one move repainted.
    fn drag_repainted(
        &mut self,
        at: tairix_geometry::Point,
        path: &[((f64, f64), Option<Modifiers>)],
    ) -> u64 {
        self.feed_repainted(InputEvent::PointerMoved { to: at });
        self.feed_repainted(InputEvent::PointerPressed {
            button: PointerButton::Primary,
        });
        assert!(self.view.gesture.is_some(), "the press began a drag");
        let mut most = 0;
        for &((x, y), held) in path {
            if let Some(held) = held {
                self.hold(held);
            }
            let to = self.at(x, y);
            let damage = self.feed_repainted(InputEvent::PointerMoved { to });
            most = most.max(self.on_canvas(damage));
        }
        self.hold(Modifiers::default());
        self.feed_repainted(InputEvent::PointerReleased {
            button: PointerButton::Primary,
        });
        most
    }

    /// A rectangle drawn from drawing point `from` to `to`, selected.
    fn rectangle(&mut self, from: (f64, f64), to: (f64, f64)) {
        self.view.choose_tool(Tool::Rectangle);
        self.drag(from, to);
    }
}

const SHIFT: Modifiers = Modifiers {
    shift: true,
    ctrl: false,
    alt: false,
    meta: false,
};

const ALT: Modifiers = Modifiers {
    shift: false,
    ctrl: false,
    alt: true,
    meta: false,
};

/// Requires that the most `what` repainted of the canvas in one move is a
/// small share of it.
fn assert_small(most: u64, what: &str) {
    let canvas = Window::new().view.layout.canvas;
    let canvas = u64::from(canvas.width) * u64::from(canvas.height);
    assert!(
        most * 4 < canvas,
        "{what} repainted {most} pixels of a canvas of {canvas}"
    );
}

/// A drag through `points`, Shift held from its third move, Alt alone from
/// its fourth and neither from its fifth.
fn modified(points: &[(f64, f64)]) -> alloc::vec::Vec<((f64, f64), Option<Modifiers>)> {
    points
        .iter()
        .enumerate()
        .map(|(at, point)| {
            let held = match at {
                2 => Some(SHIFT),
                3 => Some(ALT),
                4 => Some(Modifiers::default()),
                _ => None,
            };
            (*point, held)
        })
        .collect()
}

/// The drag the tests below move through.
const PATH: [(f64, f64); 5] = [
    (220.0, 230.0),
    (260.0, 250.0),
    (300.0, 330.0),
    (330.0, 300.0),
    (360.0, 380.0),
];

/// A move during a drag repaints its preview where it was and where it now
/// is: every pixel it changes, and much less than the canvas. Shift and Alt
/// changing as it goes mark it again where it now stands.
#[test]
fn a_drag_s_preview_repaints_what_it_changes_and_not_the_canvas() {
    let path = modified(&PATH);
    for tool in [
        Tool::Select,
        Tool::Rectangle,
        Tool::Ellipse,
        Tool::Star,
        Tool::Line,
        Tool::Pencil,
        Tool::Zoom,
    ] {
        let mut window = Window::new();
        window.view.choose_tool(tool);
        let at = window.at(200.0, 200.0);
        assert_small(
            window.drag_repainted(at, &path),
            &alloc::format!("{tool:?}"),
        );
    }
    // A guide out of a ruler: a line across the canvas, repainted as one.
    let mut window = Window::new();
    let ruler = window.view.layout.top_ruler;
    let at = tairix_geometry::Point::new(ruler.left() + 40, ruler.top() + 4);
    assert_small(window.drag_repainted(at, &path), "a guide");
}

/// Moving, scaling and turning the selection repaint its marks where they
/// were and where they now are — a group's framed by everything it paints,
/// which reaches past its outlines — every pixel they change and much less
/// than the canvas.
#[test]
fn a_drag_of_the_selection_repaints_what_it_changes_and_not_the_canvas() {
    let path = modified(&PATH);
    let selected = || {
        let mut window = Window::new();
        window.rectangle((100.0, 100.0), (180.0, 160.0));
        window.view.choose_tool(Tool::Select);
        window
    };
    let mut window = selected();
    let inside = window.at(140.0, 130.0);
    assert_small(window.drag_repainted(inside, &path), "a move");
    let mut window = selected();
    let corner = window.at(180.0, 160.0);
    assert_small(window.drag_repainted(corner, &path), "a scale");
    let mut window = selected();
    let beyond = window.at(180.0, 160.0);
    let beyond = tairix_geometry::Point::new(beyond.x + 12, beyond.y + 12);
    let turns = modified(&[
        (200.0, 120.0),
        (190.0, 90.0),
        (150.0, 70.0),
        (120.0, 80.0),
        (90.0, 100.0),
    ]);
    assert_small(window.drag_repainted(beyond, &turns), "a turn");
    let mut window = Window::new();
    window.rectangle((100.0, 100.0), (140.0, 130.0));
    window.rectangle((150.0, 100.0), (180.0, 160.0));
    window.view.choose_tool(Tool::Select);
    window.chord('a');
    window.change(Change::Width {
        width: 40.0,
        settled: true,
    });
    window.command(Command::Group);
    let inside = window.at(120.0, 115.0);
    assert_small(window.drag_repainted(inside, &path), "a group moved");
}

/// Laying a gradient, dragging a path's node and the handle of the pen's
/// newest node repaint every pixel they change: their marks, and the
/// inspector a gradient laid live is shown in, not the canvas beneath.
#[test]
fn a_drag_laying_a_gradient_or_reshaping_a_path_repaints_what_it_changes() {
    let path = modified(&PATH);
    let mut window = Window::new();
    window.rectangle((100.0, 100.0), (180.0, 160.0));
    window.view.choose_tool(Tool::Gradient);
    let start = window.at(110.0, 130.0);
    let across = modified(&[
        (120.0, 130.0),
        (140.0, 140.0),
        (160.0, 120.0),
        (170.0, 135.0),
        (175.0, 150.0),
    ]);
    assert_small(window.drag_repainted(start, &across), "a gradient");

    let mut window = Window::new();
    window.view.choose_tool(Tool::Line);
    window.drag((100.0, 300.0), (200.0, 340.0));
    window.view.choose_tool(Tool::Nodes);
    let end = window.at(200.0, 340.0);
    assert_small(window.drag_repainted(end, &path), "a node");

    let mut window = Window::new();
    window.view.choose_tool(Tool::Pen);
    for x in [100.0, 160.0] {
        let at = window.at(x, 300.0);
        window.feed(InputEvent::PointerMoved { to: at });
        window.feed(InputEvent::PointerPressed {
            button: PointerButton::Primary,
        });
        window.feed(InputEvent::PointerReleased {
            button: PointerButton::Primary,
        });
    }
    let newest = window.at(220.0, 300.0);
    assert_small(window.drag_repainted(newest, &path), "a pen handle");
}

/// Typing, and selecting characters by a drag, repaint every pixel they
/// change: the marks, and the inspector showing the paint of the characters
/// selected as the selection moves into a run coloured otherwise and out.
#[test]
fn typing_and_a_drag_across_text_repaint_the_marks_and_the_inspector() {
    let mut window = Window::new();
    window.text_at(100.0, 100.0);
    for character in "Hello".chars() {
        window.feed_repainted(InputEvent::KeyPressed {
            key: Key::Char(character),
            modifiers: Modifiers::default(),
        });
    }
    window.shifted(Key::Named(NamedKey::Left));
    window.shifted(Key::Named(NamedKey::Left));
    window.change(Change::Paint {
        target: Target::Fill,
        paint: Paint::Colour(Rgba::new(200, 0, 0, 255)),
        settled: true,
    });
    window.key(Key::Named(NamedKey::Escape));
    // No glyph has arrived, so each character stands its unknown advance
    // wide: 0.6 of the 24-pixel size.
    let wide = 0.6 * 24.0;
    window.view.choose_tool(Tool::Text);
    let start = window.at(100.0 + 3.0 * wide + 1.0, 95.0);
    let across = [4.0, 4.5, 2.0, 1.0, 1.0].map(|at| ((100.0 + at * wide, 95.0), None));
    window.drag_repainted(start, &across);
}

/// A key that nudges the selection, chooses a tool or ends the pen's path
/// repaints every pixel it changes, and the canvas beneath only where the
/// overlay changes.
#[test]
fn keys_repaint_what_they_change() {
    let canvas = Window::new().view.layout.canvas;
    let canvas = u64::from(canvas.width) * u64::from(canvas.height);
    let mut window = Window::new();
    window.rectangle((100.0, 100.0), (180.0, 160.0));
    window.view.choose_tool(Tool::Select);
    for key in [NamedKey::Right, NamedKey::Down, NamedKey::Left] {
        let damage = window.feed_repainted(InputEvent::KeyPressed {
            key: Key::Named(key),
            modifiers: Modifiers::default(),
        });
        let repainted = window.on_canvas(damage);
        assert!(
            repainted * 4 < canvas,
            "a nudge repainted {repainted} pixels of the canvas"
        );
    }
    for character in ['n', 'v', 'p', 't'] {
        window.feed_repainted(InputEvent::KeyPressed {
            key: Key::Char(character),
            modifiers: Modifiers::default(),
        });
    }
    window.view.choose_tool(Tool::Pen);
    for x in [100.0, 160.0, 220.0] {
        let at = window.at(x, 300.0);
        window.feed_repainted(InputEvent::PointerMoved { to: at });
        window.feed_repainted(InputEvent::PointerPressed {
            button: PointerButton::Primary,
        });
        window.feed_repainted(InputEvent::PointerReleased {
            button: PointerButton::Primary,
        });
    }
    window.feed_repainted(InputEvent::KeyPressed {
        key: Key::Named(NamedKey::Backspace),
        modifiers: Modifiers::default(),
    });
    window.feed_repainted(InputEvent::KeyPressed {
        key: Key::Named(NamedKey::Enter),
        modifiers: Modifiers::default(),
    });
    window.feed_repainted(InputEvent::KeyPressed {
        key: Key::Named(NamedKey::Escape),
        modifiers: Modifiers::default(),
    });
    let damage = window.feed_repainted(InputEvent::KeyPressed {
        key: Key::Char(' '),
        modifiers: Modifiers::default(),
    });
    assert!(damage.is_empty(), "space only readies the hand");
}

/// What the inspector changes repaints every pixel it changes: the selection
/// where it was and where it now is, and the inspector, status and bars that
/// follow it, not the canvas beneath them.
#[test]
fn an_inspector_change_repaints_what_follows_it() {
    let canvas = Window::new().view.layout.canvas;
    let canvas = u64::from(canvas.width) * u64::from(canvas.height);
    let mut window = Window::new();
    window.rectangle((100.0, 100.0), (180.0, 160.0));
    window.view.choose_tool(Tool::Select);
    for change in [
        Change::Width {
            width: 12.0,
            settled: false,
        },
        Change::Width {
            width: 30.0,
            settled: true,
        },
        Change::Rotation(30.0),
        Change::Corners(10.0),
        Change::Place {
            index: 0,
            value: 240.0,
        },
        // Far past the page: the bars scroll over more.
        Change::Place {
            index: 0,
            value: 4000.0,
        },
    ] {
        let before = window.rendered();
        let mut damage = Region::new();
        window.view.apply_change(change.clone(), &mut damage);
        assert_repainted(&before, &window.rendered(), &damage, &change);
        let repainted = window.on_canvas(damage);
        assert!(
            repainted * 2 < canvas,
            "{change:?} repainted {repainted} pixels of the canvas"
        );
    }
}

/// A tile landing repaints where it lands and nothing else of the canvas.
#[test]
fn a_tile_landing_repaints_where_it_lands() {
    let mut window = Window::new();
    let colours = [
        tairix_raster::Color::rgb(220, 20, 30),
        tairix_raster::Color::rgb(20, 160, 30),
    ];
    for colour in colours {
        window.view.scene += 1;
        window.view.tiles.invalidate(window.view.scene);
        while let Some(job) = window.view.wanted_render() {
            let mut keys = job.keys.iter();
            let first = keys.next().copied().expect("a tile wanted");
            let mut tile = tairix_raster::Surface::new(crate::tiles::TILE, crate::tiles::TILE)
                .expect("a tile");
            tile.fill(colour);
            let before = window.rendered();
            let mut damage = Region::new();
            window
                .view
                .rendered(job.scene, (&job.keys, [(first, tile)]), &mut damage);
            assert_repainted(&before, &window.rendered(), &damage, &first);
            let place = {
                let (x, y) = window.view.tile_origin(&first);
                tairix_geometry::Rect::new(x, y, crate::tiles::TILE, crate::tiles::TILE)
                    .intersection(&window.view.layout.canvas)
            };
            assert!(
                damage
                    .rects()
                    .iter()
                    .all(|rect| place.intersection(rect) == *rect),
                "only where the tile lands: {:?} for {place:?}",
                damage.rects()
            );
            // The rest of the job could not be drawn: asked for again.
            let _ = keys;
        }
    }
}

/// Memory pressure that lets go of what stands in for tiles not yet drawn
/// repaints every pixel that changes with it.
#[test]
fn pressure_letting_the_stand_in_go_repaints_the_canvas() {
    let mut window = Window::new();
    let red = tairix_raster::Color::rgb(220, 20, 30);
    while let Some(job) = window.view.wanted_render() {
        let tiles = job.keys.iter().map(|key| {
            let mut tile = tairix_raster::Surface::new(crate::tiles::TILE, crate::tiles::TILE)
                .expect("a tile");
            tile.fill(red);
            (*key, tile)
        });
        window
            .view
            .rendered(job.scene, (&job.keys, tiles), &mut Region::new());
    }
    let canvas = window.view.layout.canvas;
    window.view.viewport.zoom_at(
        2.0,
        Point::new(
            f64::from(canvas.width) / 2.0,
            f64::from(canvas.height) / 2.0,
        ),
    );
    let asked = window.view.wanted_render().expect("the new zoom's tiles");
    window.view.render_refused(&asked.keys);
    let before = window.rendered();
    let mut damage = Region::new();
    window
        .view
        .adopt_pressure(tairix_reclaim::PressureBand::Severe, &mut damage);
    let after = window.rendered();
    assert_ne!(before.pixels(), after.pixels(), "the stand-in went");
    assert_repainted(&before, &after, &damage, &"severe pressure");
}

/// A gradient drag the desktop takes, or Escape ends, takes back the gradient
/// it was laying live rather than leaving it to land with whatever comes next.
#[test]
fn a_gradient_drag_cancelled_lays_nothing() {
    let escape = InputEvent::KeyPressed {
        key: Key::Named(NamedKey::Escape),
        modifiers: Modifiers::default(),
    };
    for end in [InputEvent::PointerCancelled, escape] {
        let mut window = Window::new();
        window.rectangle((100.0, 100.0), (180.0, 160.0));
        let depth = window.view.document().depth();
        window.view.choose_tool(Tool::Gradient);
        window.hold_drag((110.0, 130.0), (170.0, 130.0));
        assert!(window.view.live.is_some(), "laid live while held");
        window.feed(end);
        assert!(window.view.live.is_none(), "{end:?} took it back");
        window.view.choose_tool(Tool::Select);
        window.drag((400.0, 400.0), (420.0, 420.0));
        assert_eq!(
            window.view.document().depth(),
            depth,
            "{end:?} left no step"
        );
    }
}
