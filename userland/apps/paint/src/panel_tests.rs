use alloc::string::String;
use alloc::vec;

use tairix_controls::{Button, Keystroke};
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};
use tairix_raster::{Color, Surface};
use tairix_theme::{Theme, ThemeRegistry};

use super::{switch, Height, Panel, PanelArt, PanelEvent, PanelOutcome, Part, Placed};
use crate::filter::Parameter;
use crate::layout::Faces;
use crate::track::Track;

const BOUNDS: Rect = Rect::new(10, 10, 216, 600);
const VIEWPORT: Rect = Rect::new(0, 0, 900, 640);

const LEVEL: Parameter = Parameter {
    label: "Level",
    least: 0,
    most: 255,
};

struct Context {
    registry: ThemeRegistry,
}

impl Context {
    fn new() -> Self {
        Self {
            registry: ThemeRegistry::with_builtins(),
        }
    }

    fn theme(&self) -> &Theme {
        self.registry.active()
    }

    fn faces(&self) -> Faces {
        Faces::of(self.theme(), Scale::ONE)
    }

    fn placed(&self, panel: &Panel, part: usize, bounds: Rect) -> Placed {
        panel
            .place_of(part, bounds, self.faces(), Scale::ONE, self.theme())
            .expect("laid out")
    }

    fn pointer(&self, panel: &mut Panel, event: InputEvent, bounds: Rect) -> PanelOutcome {
        panel.on_pointer(
            &event,
            (bounds, VIEWPORT),
            (self.faces(), Scale::ONE, self.theme()),
            &mut Region::new(),
        )
    }

    fn click(&self, panel: &mut Panel, at: Point, bounds: Rect) -> PanelOutcome {
        self.pointer(panel, InputEvent::PointerMoved { to: at }, bounds);
        let pressed = self.pointer(
            panel,
            InputEvent::PointerPressed {
                button: PointerButton::Primary,
            },
            bounds,
        );
        let released = self.pointer(
            panel,
            InputEvent::PointerReleased {
                button: PointerButton::Primary,
            },
            bounds,
        );
        if matches!(released, PanelOutcome::Event(_)) {
            released
        } else {
            pressed
        }
    }

    fn select_all(&self, panel: &mut Panel) -> PanelOutcome {
        panel.on_key(
            Keystroke {
                key: Key::Char('a'),
                modifiers: Modifiers {
                    ctrl: true,
                    ..Modifiers::default()
                },
                at_ns: 0,
            },
            (BOUNDS, VIEWPORT),
            (self.faces(), Scale::ONE, self.theme()),
            &mut Region::new(),
        )
    }

    fn key(&self, panel: &mut Panel, key: Key, shift: bool) -> PanelOutcome {
        panel.on_key(
            Keystroke {
                key,
                modifiers: Modifiers {
                    shift,
                    ..Modifiers::default()
                },
                at_ns: 0,
            },
            (BOUNDS, VIEWPORT),
            (self.faces(), Scale::ONE, self.theme()),
            &mut Region::new(),
        )
    }
}

fn panel() -> Panel {
    Panel::new(vec![
        Part::number(LEVEL, 128),
        Part::Buttons(vec![Button::labelled("Reset"), Button::labelled("Apply")]),
        switch("Preview", true),
    ])
}

#[test]
fn parts_stack_down_the_bounds_a_gap_apart_and_measure_what_they_take() {
    let context = Context::new();
    let panel = panel();
    let laid: alloc::vec::Vec<Placed> = panel
        .placed(BOUNDS, context.faces(), Scale::ONE, context.theme())
        .map(|(_, _, placed)| placed)
        .collect();
    assert_eq!(laid.len(), 3);
    assert_eq!(laid[0].rect.top(), BOUNDS.top());
    assert!(laid
        .windows(2)
        .all(|pair| pair[0].rect.bottom() < pair[1].rect.top()));
    let height = panel.measured_height(BOUNDS.width, context.faces(), Scale::ONE, context.theme());
    assert_eq!(
        laid[2].rect.bottom() - BOUNDS.top(),
        tairix_geometry::to_i32(height)
    );
    let short = Rect {
        height: height - 1,
        ..BOUNDS
    };
    assert_eq!(
        panel
            .placed(short, context.faces(), Scale::ONE, context.theme())
            .count(),
        2,
        "a part past the foot is left out"
    );
}

#[test]
fn a_graph_gives_up_its_height_before_a_control_is_lost() {
    let context = Context::new();
    let panel = Panel::new(vec![
        Part::Custom {
            height: Height::Ratio(1, 1),
            focusable: false,
        },
        Part::Buttons(vec![Button::labelled("Apply")]),
    ]);
    let height = panel.measured_height(BOUNDS.width, context.faces(), Scale::ONE, context.theme());
    let short = Rect {
        height: height - 100,
        ..BOUNDS
    };
    let laid: alloc::vec::Vec<Placed> = panel
        .placed(short, context.faces(), Scale::ONE, context.theme())
        .map(|(_, _, placed)| placed)
        .collect();
    assert_eq!(laid.len(), 2, "the buttons still stand");
    assert!(laid[0].rect.height <= BOUNDS.width - 100);
    assert!(laid[1].rect.bottom() <= short.bottom());
}

#[test]
fn a_number_follows_its_slider_and_its_slider_its_typing() {
    let context = Context::new();
    let mut panel = panel();
    let placed = context.placed(&panel, 0, BOUNDS);
    let slider = placed.controls[1];
    let outcome = context.click(
        &mut panel,
        Point::new(slider.right() - 2, slider.center().y),
        BOUNDS,
    );
    let PanelOutcome::Event(PanelEvent::Number {
        part: 0,
        value,
        settled: true,
        ..
    }) = outcome
    else {
        panic!("a settled number, not {outcome:?}");
    };
    assert!(value > 240, "{value}");
    let Part::Number { field, .. } = &panel.parts()[0] else {
        panic!("a number part");
    };
    assert_eq!(field.value(), value, "the field follows the slider");
    context.click(&mut panel, placed.controls[0].center(), BOUNDS);
    context.select_all(&mut panel);
    let typed = context.key(&mut panel, Key::Char('9'), false);
    assert_eq!(
        typed,
        PanelOutcome::Event(PanelEvent::Number {
            part: 0,
            cell: 0,
            value: 9,
            settled: false
        })
    );
}

#[test]
fn tab_walks_the_stops_settles_a_field_and_walks_off_either_end() {
    let context = Context::new();
    let mut panel = panel();
    assert!(panel.enter_focus(true, BOUNDS, &mut Region::new()));
    assert_eq!(panel.focus(), Some((0, 0)));
    context.key(&mut panel, Key::Named(NamedKey::Backspace), false);
    let settled = context.key(&mut panel, Key::Named(NamedKey::Tab), false);
    assert!(
        matches!(
            settled,
            PanelOutcome::Event(PanelEvent::Number { settled: true, .. })
        ),
        "{settled:?}"
    );
    assert_eq!(panel.focus(), Some((0, 1)), "on to the slider");
    for stop in [(1, 0), (1, 1), (2, 0)] {
        context.key(&mut panel, Key::Named(NamedKey::Tab), false);
        assert_eq!(panel.focus(), Some(stop));
    }
    assert_eq!(
        context.key(&mut panel, Key::Named(NamedKey::Tab), false),
        PanelOutcome::Left { forward: true }
    );
    assert_eq!(panel.focus(), None);
    assert!(panel.enter_focus(false, BOUNDS, &mut Region::new()));
    assert_eq!(panel.focus(), Some((2, 0)), "the last stop going back");
}

#[test]
fn buttons_and_switches_answer_their_presses_and_a_withheld_panel_nothing() {
    let context = Context::new();
    let mut panel = panel();
    let buttons = context.placed(&panel, 1, BOUNDS);
    assert_eq!(
        context.click(&mut panel, buttons.controls[1].center(), BOUNDS),
        PanelOutcome::Event(PanelEvent::Pressed { part: 1, button: 1 })
    );
    let preview = context.placed(&panel, 2, BOUNDS);
    assert_eq!(
        context.click(&mut panel, preview.controls[0].center(), BOUNDS),
        PanelOutcome::Event(PanelEvent::Switched { part: 2, on: false })
    );
    panel.set_withheld(true);
    assert_eq!(
        context.click(&mut panel, buttons.controls[0].center(), BOUNDS),
        PanelOutcome::Ignored
    );
    assert!(!panel.enter_focus(true, BOUNDS, &mut Region::new()));
}

#[test]
fn a_list_opens_owns_the_pointer_and_answers_its_choice() {
    let context = Context::new();
    let mut panel = Panel::new(vec![Part::choice("Channel", &["RGB", "Red", "Green"], 0)]);
    let field = context.placed(&panel, 0, BOUNDS).controls[0];
    context.click(&mut panel, field.center(), BOUNDS);
    assert!(panel.listing());
    let popup = panel.popup_rect(
        BOUNDS,
        VIEWPORT,
        context.faces(),
        Scale::ONE,
        context.theme(),
    );
    assert!(!popup.is_empty());
    let row = Point::new(popup.center().x, popup.bottom() - 4);
    let chosen = context.click(&mut panel, row, BOUNDS);
    assert_eq!(
        chosen,
        PanelOutcome::Event(PanelEvent::Chosen { part: 0, index: 2 })
    );
    assert!(!panel.listing());
}

#[test]
fn a_track_and_an_owner_drawn_part_take_their_presses() {
    let context = Context::new();
    let mut panel = Panel::new(vec![
        Part::Track(Track::new(0, 255, &[(0, Color::rgba(0, 0, 0, 255))])),
        Part::Custom {
            height: Height::Fixed(40),
            focusable: true,
        },
    ]);
    let track = context.placed(&panel, 0, BOUNDS).controls[0];
    let moved = context.click(
        &mut panel,
        Point::new(track.center().x, track.top() + 10),
        BOUNDS,
    );
    assert!(
        matches!(
            moved,
            PanelOutcome::Event(PanelEvent::Moved {
                part: 0,
                handle: 0,
                settled: true,
                ..
            })
        ),
        "{moved:?}"
    );
    let custom = context.placed(&panel, 1, BOUNDS).controls[0];
    context.pointer(
        &mut panel,
        InputEvent::PointerMoved {
            to: custom.center(),
        },
        BOUNDS,
    );
    let pressed = context.pointer(
        &mut panel,
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
        BOUNDS,
    );
    assert_eq!(
        pressed,
        PanelOutcome::Custom {
            part: 1,
            rect: custom
        }
    );
    assert_eq!(panel.focus(), Some((1, 0)), "it takes the keyboard");
    let away = context.pointer(
        &mut panel,
        InputEvent::PointerMoved {
            to: Point::new(500, 500),
        },
        BOUNDS,
    );
    assert_eq!(
        away,
        PanelOutcome::Custom {
            part: 1,
            rect: custom
        },
        "the press holds it"
    );
}

struct Plain;

impl PanelArt for Plain {
    fn draw(&self, surface: &mut Surface, _: usize, rect: Rect) {
        tairix_controls::fill_area(surface, rect, Color::rgba(255, 0, 0, 255));
    }

    fn sweeps(&self, _: usize) -> bool {
        false
    }

    fn sweep(&self, _: usize, _: u32) -> Color {
        Color::rgba(0, 0, 0, 255)
    }
}

#[test]
fn the_owner_draws_its_own_parts() {
    let context = Context::new();
    let panel = Panel::new(vec![
        Part::Note(String::from("A note")),
        Part::Custom {
            height: Height::Fixed(20),
            focusable: false,
        },
    ]);
    let mut surface = Surface::new(300, 200).expect("room");
    panel.render(
        &mut surface,
        Rect::new(10, 10, 216, 180),
        (context.faces(), Scale::ONE, context.theme()),
        &Plain,
    );
    let custom = context
        .placed(&panel, 1, Rect::new(10, 10, 216, 180))
        .controls[0];
    let at = custom.center();
    let pixel = surface
        .get(
            u32::try_from(at.x).expect("on"),
            u32::try_from(at.y).expect("on"),
        )
        .expect("drawn");
    let mut expected = Surface::new(1, 1).expect("room");
    tairix_controls::fill_area(
        &mut expected,
        Rect::new(0, 0, 1, 1),
        Color::rgba(255, 0, 0, 255),
    );
    assert_eq!(Some(pixel), expected.get(0, 0));
}
