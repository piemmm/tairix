use alloc::string::String;
use alloc::vec::Vec;

use tairix_controls::Keystroke;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_theme::{Theme, ThemeRegistry};

use super::{place, LayersOutcome, LayersPane, Placed, RowParts, ADD_PART, REMOVE_PART};
use crate::canvas::{Canvas, Kind, Sample};
use crate::document::Layer;
use crate::layout::{Faces, Fit};

/// `count` clear layers named `a`, `b`, `c` and on, the bottom first, each
/// holding a revision of its own from 1 up, as a document gives them.
fn layers(count: usize) -> Vec<Layer> {
    (0..count)
        .map(|at| {
            let canvas = Canvas::new(4, 4, Kind::Rgba, Sample::Rgba([0; 4])).expect("fits");
            let name = char::from(b'a' + u8::try_from(at).expect("few"));
            let mut layer = Layer::new(canvas, String::from(name));
            layer.revision = u64::try_from(at).expect("few") + 1;
            layer
        })
        .collect()
}

/// `pane` laid out down a 220×`height` rectangle over `count` layers.
fn placed(pane: &LayersPane, theme: &Theme, count: usize, height: u32) -> Placed {
    let faces = Faces::of(theme, Scale::ONE);
    let measures = pane.measures(count, 220, faces, Scale::ONE, theme);
    let metrics = theme.metrics();
    place(
        Rect::new(10, 20, 220, height),
        measures,
        (metrics.control_gap, metrics.scrollbar_breadth),
    )
}

/// A pane over its layers, laid out, with the theme it is drawn in.
struct Fixture {
    pane: LayersPane,
    registry: ThemeRegistry,
    layers: Vec<Layer>,
    active: usize,
    placed: Placed,
}

impl Fixture {
    fn new(count: usize) -> Self {
        let registry = ThemeRegistry::with_builtins();
        let pane = LayersPane::new();
        let placed = placed(&pane, registry.active(), count, 400);
        Self {
            pane,
            registry,
            layers: layers(count),
            active: 0,
            placed,
        }
    }

    fn pointer(&mut self, event: InputEvent) -> LayersOutcome {
        let theme = self.registry.active();
        let look = (Faces::of(theme, Scale::ONE), Scale::ONE, theme);
        self.pane.on_pointer(
            &event,
            &self.placed,
            (&self.layers, self.active),
            look,
            &mut Region::new(),
        )
    }

    /// Press and let go at `at`, answering what asked for more: the press or
    /// the release.
    fn click(&mut self, at: Point) -> LayersOutcome {
        self.pointer(InputEvent::PointerMoved { to: at });
        let pressed = self.pointer(InputEvent::PointerPressed {
            button: PointerButton::Primary,
        });
        let released = self.pointer(InputEvent::PointerReleased {
            button: PointerButton::Primary,
        });
        if matches!(pressed, LayersOutcome::Taken | LayersOutcome::Ignored) {
            released
        } else {
            pressed
        }
    }

    fn chord(&mut self, key: Key, modifiers: Modifiers) -> LayersOutcome {
        let theme = self.registry.active();
        let look = (Faces::of(theme, Scale::ONE), Scale::ONE, theme);
        self.pane.on_key(
            Keystroke {
                key,
                modifiers,
                at_ns: 0,
            },
            &self.placed,
            (&self.layers, self.active),
            look,
            &mut Region::new(),
        )
    }

    fn key(&mut self, key: Key) -> LayersOutcome {
        self.chord(key, Modifiers::default())
    }

    fn parts(&self, layer: usize) -> RowParts {
        let faces = Faces::of(self.registry.active(), Scale::ONE);
        self.pane
            .parts_of(layer, &self.placed, faces, Scale::ONE)
            .expect("laid out")
    }
}

/// The pane's controls stand above its rows, which scroll by a bar of their
/// own where they overflow; the least shows two rows and the most all.
#[test]
fn the_controls_stand_above_rows_that_scroll_where_they_overflow() {
    let registry = ThemeRegistry::with_builtins();
    let theme = registry.active();
    let pane = LayersPane::new();
    let measures = pane.measures(5, 220, Faces::of(theme, Scale::ONE), Scale::ONE, theme);
    let gap = theme.metrics().control_gap;
    let heights = measures.heights(gap);
    assert_eq!(heights.fit, Fit::Own, "the rows scroll themselves");
    assert_eq!(heights.least, measures.panel + gap + 2 * measures.row);
    assert_eq!(heights.natural, measures.panel + gap + 5 * measures.row);
    let roomy = placed(&pane, theme, 5, heights.natural);
    assert!(roomy.bar.is_empty(), "every row shows");
    assert_eq!(roomy.shown(0), 0..5);
    let short = placed(&pane, theme, 5, heights.least);
    assert!(!short.bar.is_empty(), "three rows scroll");
    assert_eq!(
        short.panel.height, measures.panel,
        "the controls stay whole"
    );
    assert_eq!(short.shown(0), 0..2);
    assert_eq!(short.shown(measures.row), 1..3, "a row down");
}

/// The top layer's row comes first; a press on a row's eye shows or hides its
/// layer, and one elsewhere on a row not painted on paints on it.
#[test]
fn rows_run_top_layer_first_and_a_press_picks_or_shows_its_layer() {
    let mut pane = Fixture::new(3);
    let (top, bottom) = (pane.placed.row_rect(0, 0), pane.placed.row_rect(2, 0));
    assert!(top.bottom() <= bottom.top(), "the top row first");
    assert_eq!(
        pane.pane.row_rect(2, &pane.placed),
        top,
        "layer 2 is on top"
    );
    let parts = pane.parts(1);
    assert!(parts.eye.right() < parts.thumb.left());
    assert!(parts.thumb.right() < parts.name.left());
    assert!(parts.name.right() <= parts.opacity.left());
    assert_eq!(pane.click(parts.eye.center()), LayersOutcome::Show(1));
    let name = pane.parts(2).name.center();
    assert_eq!(pane.click(name), LayersOutcome::Select(2));
    assert_eq!(pane.pane.renaming(), None, "picked, not renamed");
}

/// Each of the controls' buttons answers the edit it names.
#[test]
fn each_button_answers_the_edit_it_names() {
    let mut pane = Fixture::new(3);
    pane.active = 1;
    let _ = pane.pane.sync((&pane.layers, 1), (true, true, true));
    let theme = pane.registry.active();
    let faces = Faces::of(theme, Scale::ONE);
    let spots: Vec<(Point, LayersOutcome)> = [
        (ADD_PART, 0, LayersOutcome::New),
        (ADD_PART, 1, LayersOutcome::Duplicate),
        (REMOVE_PART, 0, LayersOutcome::Delete),
        (REMOVE_PART, 1, LayersOutcome::MergeDown),
    ]
    .into_iter()
    .map(|(part, cell, edit)| {
        let place = pane
            .pane
            .panel
            .place_of(part, pane.placed.panel, faces, Scale::ONE, theme)
            .expect("laid out");
        (place.controls[cell].center(), edit)
    })
    .collect();
    for (at, edit) in spots {
        assert_eq!(pane.click(at), edit);
    }
}

/// A row dragged past the slop marks the gap it would land in, and let go
/// there moves its layer; let go in its own gap, nothing moves.
#[test]
fn a_row_dragged_moves_its_layer_to_the_gap_it_is_let_go_at() {
    let mut pane = Fixture::new(4);
    pane.active = 3;
    let from = pane.parts(3).name.center();
    pane.pointer(InputEvent::PointerMoved { to: from });
    pane.pointer(InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    let under = pane.placed.row_rect(2, 0);
    pane.pointer(InputEvent::PointerMoved {
        to: Point::new(from.x, under.bottom() - 2),
    });
    assert!(pane.pane.holding());
    let released = pane.pointer(InputEvent::PointerReleased {
        button: PointerButton::Primary,
    });
    assert_eq!(released, LayersOutcome::Move { from: 3, to: 1 });
    assert_eq!(pane.pane.renaming(), None, "a drag renames nothing");
    pane.pointer(InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    pane.pointer(InputEvent::PointerMoved {
        to: Point::new(from.x + 20, under.bottom() - 2),
    });
    pane.pointer(InputEvent::PointerMoved {
        to: Point::new(from.x + 20, under.top() + 2),
    });
    let back = pane.pointer(InputEvent::PointerReleased {
        button: PointerButton::Primary,
    });
    assert_eq!(
        back,
        LayersOutcome::Taken,
        "layer 1's row let go in its own gap moves nothing"
    );
}

/// A press let go on the name of the layer painted on renames it in place:
/// the name all chosen, typed over, Enter answering it and Escape taking it
/// back; the window's chords are the window's.
#[test]
fn the_name_of_the_layer_painted_on_is_renamed_in_place() {
    let mut pane = Fixture::new(2);
    let name = pane.parts(0).name.center();
    assert_eq!(pane.click(name), LayersOutcome::Taken);
    assert_eq!(pane.pane.renaming(), Some(1), "layer a's revision");
    assert_eq!(pane.pane.typed_name(), Some("a"));
    assert!(pane.pane.has_focus());
    for ch in "Sky".chars() {
        pane.key(Key::Char(ch));
    }
    assert_eq!(pane.pane.typed_name(), Some("Sky"), "typed over");
    let ctrl = Modifiers {
        ctrl: true,
        ..Modifiers::default()
    };
    assert_eq!(pane.chord(Key::Char('s'), ctrl), LayersOutcome::Ignored);
    assert_eq!(
        pane.key(Key::Named(NamedKey::Enter)),
        LayersOutcome::Renamed(1)
    );
    assert_eq!(pane.key(Key::Named(NamedKey::Escape)), LayersOutcome::Taken);
    assert_eq!(pane.pane.renaming(), None, "taken back");
    pane.click(name);
    let beside = pane.parts(1).thumb.center();
    assert_eq!(
        pane.click(beside),
        LayersOutcome::Renamed(1),
        "a press beside settles it"
    );
}

/// The keyboard reaches the rows, where Up and Down paint on the layer above
/// or below, Home on the top, Space shows or hides the layer painted on, and
/// Tab walks off the pane.
#[test]
fn the_keyboard_steps_through_the_layers_on_the_rows() {
    let mut pane = Fixture::new(3);
    pane.active = 1;
    let placed = pane.placed;
    assert!(pane.pane.enter_focus(false, &placed, &mut Region::new()));
    assert_eq!(pane.key(Key::Named(NamedKey::Up)), LayersOutcome::Select(2));
    assert_eq!(
        pane.key(Key::Named(NamedKey::Down)),
        LayersOutcome::Select(0)
    );
    assert_eq!(
        pane.key(Key::Named(NamedKey::Home)),
        LayersOutcome::Select(2)
    );
    assert_eq!(pane.key(Key::Char(' ')), LayersOutcome::Show(1));
    assert_eq!(
        pane.key(Key::Named(NamedKey::Tab)),
        LayersOutcome::Left { forward: true }
    );
    assert!(!pane.pane.has_focus());
}

/// A rename is held by its layer's revision: once no layer holds it the rename
/// is dropped, never moved onto the layer now in its place.
#[test]
fn a_rename_follows_its_layer_by_revision() {
    let mut pane = Fixture::new(3);
    pane.active = 1;
    let name = pane.parts(1).name.center();
    pane.click(name);
    assert_eq!(pane.pane.renaming(), Some(2), "layer b's revision");
    pane.layers.remove(1);
    let _ = pane.pane.sync((&pane.layers, 1), (true, true, true));
    assert_eq!(
        pane.pane.renaming(),
        None,
        "its layer gone, the rename goes"
    );
}

/// No rename begins while a worker has the picture, which it would land on.
#[test]
fn no_rename_begins_while_a_worker_has_the_picture() {
    let mut pane = Fixture::new(2);
    let _ = pane.pane.sync((&pane.layers, 0), (true, true, false));
    let name = pane.parts(0).name.center();
    pane.click(name);
    assert_eq!(pane.pane.renaming(), None);
    let _ = pane.pane.sync((&pane.layers, 0), (true, true, true));
    pane.click(name);
    assert_eq!(pane.pane.renaming(), Some(1), "once it has landed");
}

/// A press the controls take takes the keyboard from the rows, so a digit
/// typed next is the opacity field's it landed on, never the rows'.
#[test]
fn a_press_on_the_controls_takes_the_keyboard_from_the_rows() {
    let mut pane = Fixture::new(2);
    pane.active = 1;
    let placed = pane.placed;
    assert!(pane.pane.enter_focus(false, &placed, &mut Region::new()));
    let theme = pane.registry.active();
    let look = (Faces::of(theme, Scale::ONE), Scale::ONE, theme);
    let field = pane.pane.opacity_field(&placed, look).expect("laid out");
    pane.click(field.center());
    assert_ne!(
        pane.key(Key::Char('5')),
        LayersOutcome::Ignored,
        "the field takes it, not the rows"
    );
    assert_ne!(
        pane.key(Key::Named(NamedKey::Down)),
        LayersOutcome::Select(0),
        "nor do the arrows pick a layer"
    );
}
