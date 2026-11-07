//! The inspector driven by its input: scrolling a tall tab, choosing what
//! the picker edits, committing a typed field, and stepping between fields.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_abi::driver::input::SCROLL_UNITS_PER_DETENT;
use tairix_drawing::{Children, Ids, Kind, Node, Paint, Style, TextAlign};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_theme::Theme;

use super::{Change, Changes, Face, Field, Focus, Inspector, Shown, Tab, Target};
use crate::layout::px;
use crate::layout::Layout;

/// `count` empty layers, each named for where it stands.
fn layers(count: usize) -> Children {
    let mut ids = Ids::new();
    let mut nodes = Vec::new();
    for index in 0..count {
        let group = Kind::Group {
            children: Children::new(),
            clip: None,
        };
        let name: Arc<str> = Arc::from(alloc::format!("Layer {index}").as_str());
        let layer = Node::new(ids.issue(), group)
            .and_then(|node| node.with_name(Some(name)))
            .expect("a layer");
        nodes.push(Arc::new(layer));
    }
    Children::from_nodes(nodes).expect("held")
}

fn shown<'a>(style: &'a Style, layers: &'a Children) -> Shown<'a> {
    Shown {
        style,
        layers,
        layer: 0,
        locked: false,
        polygon: false,
        face: Face {
            family: "sans-serif",
            bold: false,
            italic: false,
            align: TextAlign::Start,
        },
    }
}

fn centre(rect: Rect) -> Point {
    Point::new(
        rect.left() + px(rect.width) / 2,
        rect.top() + px(rect.height) / 2,
    )
}

struct Panel {
    inspector: Inspector,
    layout: Layout,
    theme: Theme,
    style: Style,
    layers: Children,
}

impl Panel {
    /// An inspector in a window `height` pixels tall.
    fn new(height: u32) -> Self {
        let style = Style::default();
        Self {
            inspector: Inspector::new(&style),
            layout: Layout::of(1080, height, Scale::ONE, &Theme::light()),
            theme: Theme::light(),
            style,
            layers: layers(1),
        }
    }

    fn feed(&mut self, event: InputEvent) -> Changes {
        let shown = shown(&self.style, &self.layers);
        let context = (&self.layout, Scale::ONE, &self.theme);
        self.inspector
            .on_pointer(&event, context, &shown, &mut Region::new())
    }

    fn key(&mut self, key: Key, modifiers: Modifiers) -> Changes {
        let shown = shown(&self.style, &self.layers);
        let context = (&self.layout, Scale::ONE, &self.theme);
        self.inspector
            .on_key(key, modifiers, context, &shown, &mut Region::new())
    }

    /// Press and let go at window point `at`, answering what the press changed.
    fn click(&mut self, at: Point) -> Changes {
        self.feed(InputEvent::PointerMoved { to: at });
        let changes = self.feed(InputEvent::PointerPressed {
            button: PointerButton::Primary,
        });
        self.feed(InputEvent::PointerReleased {
            button: PointerButton::Primary,
        });
        changes
    }

    /// Where the window shows what the tab lays at `rect`, if it shows it.
    fn shown_at(&self, rect: Rect) -> Option<Rect> {
        let shown = shown(&self.style, &self.layers);
        let placed = self
            .inspector
            .place(&self.layout, Scale::ONE, &self.theme, &shown);
        placed.view.to_window(rect)
    }

    fn button(&self, label: &str) -> Rect {
        let shown = shown(&self.style, &self.layers);
        let placed = self
            .inspector
            .place(&self.layout, Scale::ONE, &self.theme, &shown);
        let button = placed
            .column
            .buttons
            .iter()
            .find(|button| button.label == label)
            .unwrap_or_else(|| panic!("a {label} button"));
        button.rect
    }

    fn field(&self, field: Field) -> Rect {
        let shown = shown(&self.style, &self.layers);
        let placed = self
            .inspector
            .place(&self.layout, Scale::ONE, &self.theme, &shown);
        placed.field_rect(field).expect("the field is laid out")
    }

    /// Type `text` into the field holding the keyboard in place of what it
    /// held, answering what the last key changed.
    fn retype(&mut self, text: &str) -> Changes {
        self.key(Key::Named(NamedKey::End), Modifiers::default());
        for _ in 0..16 {
            self.key(Key::Named(NamedKey::Backspace), Modifiers::default());
        }
        let mut last = Changes::NONE;
        for character in text.chars() {
            last = self.key(Key::Char(character), Modifiers::default());
        }
        last
    }

    /// Bring the inspector into line with `style`, nothing selected, as the
    /// view does after every change it shows.
    fn follow(&mut self, style: &Style) {
        let drawing = crate::document::blank(&mut Ids::new()).expect("a drawing");
        self.inspector
            .follow(&drawing, &[], style, tairix_bezier::Bounds::EMPTY);
    }

    fn picker(&self) -> Rect {
        let shown = shown(&self.style, &self.layers);
        let placed = self
            .inspector
            .place(&self.layout, Scale::ONE, &self.theme, &shown);
        let picker = placed.column.picker.expect("the picker is laid out");
        placed.view.to_window(picker).expect("and shown")
    }

    fn choose_tab(&mut self, tab: Tab) {
        let (_, rect) = Inspector::tab_rects(&self.layout)
            .into_iter()
            .find(|(each, _)| *each == tab)
            .expect("the tab");
        self.click(centre(rect));
        assert_eq!(self.inspector.tab(), tab);
    }
}

#[test]
fn a_tab_taller_than_its_panel_scrolls_to_its_end_and_no_further() {
    let mut panel = Panel::new(420);
    let (extent, viewport, bar) = {
        let shown = shown(&panel.style, &panel.layers);
        let placed = panel
            .inspector
            .place(&panel.layout, Scale::ONE, &panel.theme, &shown);
        (placed.column.extent(), placed.view.viewport(), placed.bar)
    };
    assert!(bar.is_some(), "the Style tab overflows a short panel");
    let opacity = panel.field(Field::Opacity);
    assert!(
        panel.shown_at(opacity).is_none(),
        "its last field starts below"
    );
    panel.feed(InputEvent::PointerMoved {
        to: centre(viewport),
    });
    let detents = SCROLL_UNITS_PER_DETENT * 100;
    panel.feed(InputEvent::PointerScrolled { dx: 0, dy: detents });
    let offset = panel.inspector.bar.model().offset();
    assert_eq!(
        offset,
        u64::from(extent - viewport.height),
        "held at the end"
    );
    let window = panel.shown_at(opacity).expect("the last field shown");
    panel.click(centre(window));
    assert_eq!(
        panel.inspector.focus,
        Some(Focus::Field(Field::Opacity)),
        "hit where it shows"
    );
    panel.feed(InputEvent::PointerMoved {
        to: centre(viewport),
    });
    panel.feed(InputEvent::PointerScrolled {
        dx: 0,
        dy: -detents,
    });
    assert_eq!(panel.inspector.bar.model().offset(), 0);
    assert!(
        Panel::new(1400).shown_at(opacity).is_some(),
        "a tall panel shows it all"
    );
}

#[test]
fn fill_and_stroke_choose_what_the_picker_edits_and_none_paints_nothing() {
    let mut panel = Panel::new(900);
    let stroke = panel.button("Stroke");
    assert_eq!(
        panel.click(centre(stroke)),
        Changes::NONE,
        "a choice, not a change"
    );
    assert_eq!(panel.inspector.target, Target::Stroke);
    let none = panel.button("None");
    let changes: alloc::vec::Vec<_> = panel.click(centre(none)).into_iter().collect();
    assert_eq!(
        changes,
        vec![Change::Paint {
            target: Target::Stroke,
            paint: Paint::None,
            settled: true,
        }]
    );
}

#[test]
fn what_was_typed_into_a_field_lands_when_the_pointer_presses_elsewhere() {
    let mut panel = Panel::new(900);
    panel.choose_tab(Tab::Shape);
    let points = panel.field(Field::Points);
    panel.click(centre(points));
    assert!(panel.inspector.has_keyboard());
    let typed = panel.key(Key::Char('7'), Modifiers::default());
    assert_eq!(typed, Changes::NONE, "a star's points change once settled");
    let polygon = panel.button("Polygon");
    let changes = panel.click(centre(polygon));
    assert!(
        matches!(changes.released, Some(Change::Points(57 | 75))),
        "{changes:?}"
    );
    assert_eq!(
        changes.change,
        Some(Change::Polygon(true)),
        "and the press too"
    );
    assert!(!panel.inspector.has_keyboard());
}

#[test]
fn tab_steps_through_the_fields_and_shift_tab_steps_back_round() {
    let mut panel = Panel::new(900);
    panel.choose_tab(Tab::Object);
    let first = panel.field(Field::Place(0));
    panel.click(centre(first));
    panel.key(Key::Named(NamedKey::Tab), Modifiers::default());
    assert_eq!(panel.inspector.focus, Some(Focus::Field(Field::Place(1))));
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    panel.key(Key::Named(NamedKey::Tab), shift);
    panel.key(Key::Named(NamedKey::Tab), shift);
    assert_eq!(
        panel.inspector.focus,
        Some(Focus::Field(Field::Place(3))),
        "round to the last"
    );
}

#[test]
fn a_press_on_the_tab_strip_changes_tab_and_starts_it_at_its_top() {
    let mut panel = Panel::new(420);
    let viewport = panel.layout.panel;
    panel.feed(InputEvent::PointerMoved {
        to: centre(viewport),
    });
    let detents = SCROLL_UNITS_PER_DETENT * 3;
    panel.feed(InputEvent::PointerScrolled { dx: 0, dy: detents });
    assert!(panel.inspector.bar.model().offset() > 0);
    panel.choose_tab(Tab::Shape);
    assert_eq!(panel.inspector.bar.model().offset(), 0);
}

#[test]
fn a_thousand_layers_lay_out_only_the_rows_the_panel_shows() {
    let mut panel = Panel::new(720);
    panel.layers = layers(1000);
    panel.choose_tab(Tab::Layers);
    let rows = |panel: &Panel| {
        let shown = shown(&panel.style, &panel.layers);
        let placed = panel
            .inspector
            .place(&panel.layout, Scale::ONE, &panel.theme, &shown);
        let names: Vec<alloc::string::String> = placed
            .column
            .buttons
            .iter()
            .filter(|button| button.label.starts_with("Layer"))
            .map(|button| alloc::string::String::from(button.label))
            .collect();
        (names, placed.column.extent())
    };
    let (top, extent) = rows(&panel);
    assert!(top.len() < 40, "only what shows: {}", top.len());
    assert_eq!(
        top.first().map(alloc::string::String::as_str),
        Some("Layer 999"),
        "the topmost first"
    );
    assert!(extent > 1000 * 26, "the column still spans every row");
    let viewport = panel.layout.panel;
    panel.feed(InputEvent::PointerMoved {
        to: centre(viewport),
    });
    panel.feed(InputEvent::PointerScrolled {
        dx: 0,
        dy: SCROLL_UNITS_PER_DETENT * 100_000,
    });
    let (bottom, _) = rows(&panel);
    assert!(bottom.len() < 40);
    assert_eq!(
        bottom.last().map(alloc::string::String::as_str),
        Some("Layer 0"),
        "scrolled to the bottom"
    );
}

#[test]
fn a_colour_dragged_live_settles_however_often_the_inspector_follows() {
    let mut panel = Panel::new(1400);
    let picker = panel.picker();
    let at = |x: i32, y: i32| Point::new(picker.left() + x, picker.top() + y);
    panel.feed(InputEvent::PointerMoved { to: at(12, 12) });
    let pressed = panel.feed(InputEvent::PointerPressed {
        button: PointerButton::Primary,
    });
    let dragged = panel.feed(InputEvent::PointerMoved { to: at(40, 30) });
    let live = pressed.change.or(dragged.change);
    let Some(Change::Paint {
        paint: Paint::Colour(colour),
        settled: false,
        ..
    }) = live
    else {
        panic!("a live colour: {live:?}");
    };
    // The view shows the live colour and follows it, as it does after every
    // change it shows; the drag must still settle.
    let mut showing = panel.style.clone();
    showing.fill = Paint::Colour(colour);
    panel.follow(&showing);
    let released = panel.feed(InputEvent::PointerReleased {
        button: PointerButton::Primary,
    });
    assert!(
        matches!(released.change, Some(Change::Paint { settled: true, .. })),
        "{released:?}"
    );
    assert!(!panel.inspector.unsettled);
    let ground = tairix_colour::Rgba::new(10, 20, 30, 255);
    let mut back = panel.style.clone();
    back.fill = Paint::Colour(ground);
    panel.follow(&back);
    assert_eq!(
        panel.inspector.picker.colour(),
        ground,
        "settled, the picker follows the selection again"
    );
}

#[test]
fn a_width_typed_live_settles_however_often_the_inspector_follows() {
    let mut panel = Panel::new(1400);
    let width = panel.field(Field::Width);
    panel.click(centre(width));
    let typed = panel.retype("3");
    let Some(Change::Width {
        width: live,
        settled: false,
    }) = typed.change
    else {
        panic!("a live width: {typed:?}");
    };
    let mut showing = panel.style.clone();
    showing.stroke.width = live;
    panel.follow(&showing);
    let elsewhere = panel.button("Round");
    let changes = panel.click(centre(elsewhere));
    assert!(
        matches!(changes.released, Some(Change::Width { settled: true, .. })),
        "{changes:?}"
    );
}

#[test]
fn the_picker_takes_the_keyboard_and_tab_walks_on_to_the_fields() {
    let mut panel = Panel::new(1400);
    let picker = panel.picker();
    panel.click(Point::new(picker.left() + 12, picker.top() + 12));
    assert_eq!(panel.inspector.focus, Some(Focus::Picker));
    for _ in 0..64 {
        if panel.inspector.focus != Some(Focus::Picker) {
            break;
        }
        panel.key(Key::Named(NamedKey::Tab), Modifiers::default());
    }
    assert_eq!(panel.inspector.focus, Some(Focus::Field(Field::Width)));
    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    panel.key(Key::Named(NamedKey::Tab), shift);
    assert_eq!(panel.inspector.focus, Some(Focus::Picker), "and back");
}

#[test]
fn a_place_field_changes_only_its_own_measure() {
    let mut panel = Panel::new(900);
    panel.choose_tab(Tab::Object);
    let top = panel.field(Field::Place(1));
    panel.click(centre(top));
    panel.retype("5");
    let changes = panel.key(Key::Named(NamedKey::Enter), Modifiers::default());
    assert!(
        matches!(changes.change, Some(Change::Place { index: 1, .. })),
        "{changes:?}"
    );
}

#[test]
fn a_colour_typed_into_the_picker_lands_once_and_never_again() {
    let mut panel = Panel::new(1400);
    let picker = panel.picker();
    panel.click(Point::new(picker.left() + 12, picker.top() + 12));
    let mut typed = None;
    for _ in 0..16 {
        panel.key(Key::Named(NamedKey::End), Modifiers::default());
        for _ in 0..12 {
            panel.key(Key::Named(NamedKey::Backspace), Modifiers::default());
        }
        let mut last = Changes::NONE;
        for character in "ff0000".chars() {
            last = panel.key(Key::Char(character), Modifiers::default());
        }
        if matches!(last.change, Some(Change::Paint { settled: false, .. })) {
            typed = last.change;
            break;
        }
        panel.key(Key::Named(NamedKey::Tab), Modifiers::default());
        if panel.inspector.focus != Some(Focus::Picker) {
            break;
        }
    }
    assert!(typed.is_some(), "the picker takes a typed colour");
    // A command lands what is under way: the colour shown as it was typed.
    let layout = panel.layout;
    let settled = panel.inspector.settle_edits(&layout, &mut Region::new());
    assert_eq!(
        settled, None,
        "it was shown live, and lands with what is shown"
    );
    assert!(!panel.inspector.unsettled);
    let elsewhere = panel.button("Round");
    let changes = panel.click(centre(elsewhere));
    assert_eq!(
        changes.released, None,
        "nothing is applied again to whatever is selected next"
    );
}
