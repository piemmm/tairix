use super::*;
use core::net::{IpAddr, Ipv4Addr};

use tairix_geometry::Point;

use crate::layout;
use crate::model::{Map, NodeKey, NodeKind};

fn close(a: i32, b: i32, tolerance: i32) -> bool {
    (a - b).abs() <= tolerance
}

#[test]
fn window_and_graph_round_trip_at_unit_zoom() {
    let vp = Viewport::new(800, 600);
    let graph = Point::new(123, -45);
    let back = vp.to_graph(vp.to_window(graph));
    assert!(close(back.x, graph.x, 1));
    assert!(close(back.y, graph.y, 1));
}

#[test]
fn zoom_about_holds_the_anchor_point_fixed() {
    let mut vp = Viewport::new(800, 600);
    let anchor = Point::new(620, 180);
    let graph_under_anchor = vp.to_graph(anchor);
    vp.set_zoom_about(anchor, 2500);
    let moved = vp.to_window(graph_under_anchor);
    assert!(close(moved.x, anchor.x, 2));
    assert!(close(moved.y, anchor.y, 2));
    assert_eq!(vp.zoom(), 2500);
}

#[test]
fn zoom_is_clamped_to_its_range() {
    let mut vp = Viewport::new(800, 600);
    vp.set_zoom_about(Point::new(400, 300), u32::MAX);
    assert_eq!(vp.zoom(), MAX_ZOOM_PER_MILLE);
    vp.set_zoom_about(Point::new(400, 300), 1);
    assert_eq!(vp.zoom(), MIN_ZOOM_PER_MILLE);
}

#[test]
fn pan_moves_the_centred_graph_point() {
    let mut vp = Viewport::new(800, 600);
    let before = vp.center();
    vp.pan_by(100, 0);
    assert_eq!(vp.center().x, before.x - 100);
    assert_eq!(vp.center().y, before.y);
}

fn sample_map() -> Map {
    let mut map = Map::new();
    let local = map
        .intern(NodeKey::LocalHost, NodeKind::LocalHost, 0)
        .unwrap();
    let peer = map
        .intern(
            NodeKey::Neighbour(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 5))),
            NodeKind::Neighbour,
            0,
        )
        .unwrap();
    map.link(local, peer, crate::model::EdgeKind::OnLink);
    layout(&mut map);
    map
}

#[test]
fn clamp_keeps_the_centre_within_the_extent() {
    let map = sample_map();
    let mut vp = Viewport::new(800, 600);
    vp.pan_by(-1_000_000, -1_000_000);
    vp.clamp_pan(&map);
    let (mut min, mut max) = (
        Point::new(i32::MAX, i32::MAX),
        Point::new(i32::MIN, i32::MIN),
    );
    for node in map.nodes() {
        min = Point::new(min.x.min(node.pos.x), min.y.min(node.pos.y));
        max = Point::new(max.x.max(node.pos.x), max.y.max(node.pos.y));
    }
    assert!(vp.center().x >= min.x - 400 && vp.center().x <= max.x + 400);
    assert!(vp.center().y >= min.y - 400 && vp.center().y <= max.y + 400);
}

#[test]
fn fit_shows_the_whole_extent_inside_the_margin_and_never_magnifies() {
    let map = sample_map();
    let mut view = Viewport::new(120, 90);
    view.fit(&map, 20);
    for node in map.nodes() {
        let at = view.to_window(node.pos);
        assert!(
            (20..=100).contains(&at.x) && (20..=70).contains(&at.y),
            "{at:?}"
        );
    }
    assert!(view.zoom() < DEFAULT_ZOOM_PER_MILLE, "zoomed out to fit");

    let mut roomy = Viewport::new(100_000, 100_000);
    roomy.fit(&map, 20);
    assert_eq!(
        roomy.zoom(),
        DEFAULT_ZOOM_PER_MILLE,
        "a small map keeps its own size"
    );

    let mut untouched = Viewport::new(400, 300);
    untouched.fit(&Map::new(), 20);
    assert_eq!(
        (untouched.zoom(), untouched.center()),
        (DEFAULT_ZOOM_PER_MILLE, Point::ORIGIN)
    );
}
