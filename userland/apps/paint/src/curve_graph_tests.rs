use tairix_controls::Keystroke;
use tairix_geometry::{Point, Rect, Region, Scale};
use tairix_input::{InputEvent, Key, Modifiers, NamedKey, PointerButton};

use super::{CurveEdit, CurveGraph};
use crate::tone::Curve;

const BOUNDS: Rect = Rect::new(0, 0, 263, 263);

fn event(graph: &mut CurveGraph, curve: &mut Curve, event: InputEvent) -> Option<CurveEdit> {
    let edit = graph.on_pointer(&event, BOUNDS, curve, Scale::ONE, &mut Region::new());
    if let Some(CurveEdit::Changed { curve: changed, .. }) = edit {
        *curve = changed;
    }
    edit
}

fn move_to(graph: &mut CurveGraph, curve: &mut Curve, to: Point) -> Option<CurveEdit> {
    event(graph, curve, InputEvent::PointerMoved { to })
}

fn press(graph: &mut CurveGraph, curve: &mut Curve) -> Option<CurveEdit> {
    event(
        graph,
        curve,
        InputEvent::PointerPressed {
            button: PointerButton::Primary,
        },
    )
}

fn release(graph: &mut CurveGraph, curve: &mut Curve) -> Option<CurveEdit> {
    event(
        graph,
        curve,
        InputEvent::PointerReleased {
            button: PointerButton::Primary,
        },
    )
}

/// Where level `(input, output)` is drawn in [`BOUNDS`].
fn screen(input: u8, output: u8) -> Point {
    CurveGraph::screen_of(CurveGraph::plot(BOUNDS, Scale::ONE), (input, output))
}

#[test]
fn a_press_adds_a_point_that_follows_the_pointer_until_let_go() {
    let mut graph = CurveGraph::default();
    let mut curve = Curve::IDENTITY;
    move_to(&mut graph, &mut curve, screen(64, 64));
    let added = press(&mut graph, &mut curve);
    assert!(matches!(
        added,
        Some(CurveEdit::Changed { settled: false, .. })
    ));
    assert_eq!(curve.points().len(), 3);
    assert_eq!(graph.selected(), Some(1));
    move_to(&mut graph, &mut curve, screen(64, 128));
    assert_eq!(curve.points()[1], (64, 128));
    assert!(matches!(
        release(&mut graph, &mut curve),
        Some(CurveEdit::Changed { settled: true, .. })
    ));
    assert!(!graph.dragging());
    assert_eq!(curve.table()[64], 128);
}

#[test]
fn a_point_carried_out_of_the_graph_is_taken_away_when_let_go() {
    let mut graph = CurveGraph::default();
    let mut curve = Curve::IDENTITY;
    curve.add((128, 200));
    move_to(&mut graph, &mut curve, screen(128, 200));
    assert_eq!(press(&mut graph, &mut curve), Some(CurveEdit::Chose));
    move_to(&mut graph, &mut curve, Point::new(130, -60));
    release(&mut graph, &mut curve);
    assert_eq!(curve, Curve::IDENTITY);
    assert_eq!(graph.selected(), None);
    move_to(&mut graph, &mut curve, screen(0, 0));
    press(&mut graph, &mut curve);
    move_to(&mut graph, &mut curve, Point::new(-80, 300));
    release(&mut graph, &mut curve);
    assert_eq!(curve.points().len(), 2, "a curve keeps its two");
}

#[test]
fn keys_nudge_choose_and_take_away_points() {
    let mut graph = CurveGraph::default();
    let mut curve = Curve::IDENTITY;
    curve.add((100, 100));
    let key = |key| Keystroke {
        key: Key::Named(key),
        modifiers: Modifiers::default(),
        at_ns: 0,
    };
    let feed = |graph: &mut CurveGraph, curve: &mut Curve, named| {
        let edit = graph.on_key(key(named), BOUNDS, curve, &mut Region::new());
        if let Some(CurveEdit::Changed { curve: changed, .. }) = edit {
            *curve = changed;
        }
        edit
    };
    assert!(
        feed(&mut graph, &mut curve, NamedKey::Up).is_none(),
        "not focused"
    );
    graph.focus(true);
    assert_eq!(
        feed(&mut graph, &mut curve, NamedKey::PageDown),
        Some(CurveEdit::Chose)
    );
    assert_eq!(
        feed(&mut graph, &mut curve, NamedKey::PageDown),
        Some(CurveEdit::Chose)
    );
    assert_eq!(graph.selected(), Some(1));
    feed(&mut graph, &mut curve, NamedKey::Up);
    feed(&mut graph, &mut curve, NamedKey::Left);
    assert_eq!(curve.points()[1], (99, 101));
    feed(&mut graph, &mut curve, NamedKey::Delete);
    assert_eq!(curve, Curve::IDENTITY);
    assert!(
        feed(&mut graph, &mut curve, NamedKey::Delete).is_none(),
        "the last two stay"
    );
}
