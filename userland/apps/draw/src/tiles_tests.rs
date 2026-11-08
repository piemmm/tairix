//! Unit tests for the tile cache.

use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;

use tairix_bezier::{Bounds, Point};
use tairix_drawing::shape::RectShape;
use tairix_drawing::{Children, Drawing, Grid, Ids, Kind, Node, Page, Shape, Style, Unit};
use tairix_raster::Surface;

use super::{changed_area, covering, TileKey, Tiles, TILE};
use tairix_reclaim::PressureBand;

fn rect(ids: &mut Ids, x: f64) -> Arc<Node> {
    Arc::new(
        Node::new(
            ids.issue(),
            Kind::Shape {
                shape: Shape::Rect(RectShape {
                    x,
                    y: 0.0,
                    width: 10.0,
                    height: 10.0,
                    rx: 0.0,
                    ry: 0.0,
                }),
                style: Arc::new(Style::default()),
            },
        )
        .expect("a rect"),
    )
}

fn drawing(children: Vec<Arc<Node>>, ids: &mut Ids) -> Drawing {
    let layer = Node::new(
        ids.issue(),
        Kind::Group {
            children: Children::from_nodes(children).expect("held"),
            clip: None,
        },
    )
    .expect("a layer");
    Drawing::new(
        Page {
            width: 2000.0,
            height: 1000.0,
            unit: Unit::Px,
            background: None,
        },
        Children::from_nodes(vec![Arc::new(layer)]).expect("held"),
        tairix_drawing::SharedSlice::new(),
        Grid::default(),
    )
    .expect("a drawing")
}

fn key(column: i64, row: i64) -> TileKey {
    TileKey {
        pixels: 1.0_f64.to_bits(),
        column,
        row,
        outline: false,
    }
}

fn tile() -> Surface {
    Surface::new(TILE, TILE).expect("a tile")
}

#[test]
fn the_tiles_covering_a_canvas_start_from_its_centre() {
    let keys = covering(
        Bounds::new(Point::ZERO, Point::new(600.0, 300.0)),
        Point::new(10.0, 10.0),
        1.0,
        false,
    );
    assert_eq!(keys.len(), 4 * 3, "columns -1 to 2, rows -1 to 1");
    assert!(keys
        .iter()
        .all(|key| (-1..=2).contains(&key.column) && (-1..=1).contains(&key.row)));
    let first = keys[0];
    assert!(matches!((first.column, first.row), (0 | 1, 0)), "{first:?}");
}

#[test]
fn tiles_far_from_the_origin_are_counted_and_the_count_ends() {
    let area = Bounds::new(Point::ZERO, Point::new(1024.0, 512.0));
    let near = covering(area, Point::new(-1e17, 3e16), 2.0, false);
    assert_eq!(near.len(), 8, "four across, two down");
    // Past the integers a float steps through one at a time, stepping a
    // float index would never end.
    let far = covering(area, Point::new(-1e19, -1e19), 2.0, false);
    assert!(far.len() <= 8);
}

#[test]
fn only_what_an_edit_changes_is_its_changed_area() {
    let mut ids = Ids::new();
    let kept = rect(&mut ids, 0.0);
    let moved = rect(&mut ids, 1000.0);
    let before = drawing(vec![kept.clone(), moved.clone()], &mut ids);
    let layer = before.layers().get(0).expect("layer");
    let elsewhere = Arc::new(
        moved
            .with_transform(tairix_bezier::Affine::translate(0.0, 500.0))
            .expect("moved"),
    );
    let rebuilt = layer
        .with_kind(Kind::Group {
            children: Children::from_nodes(vec![kept, elsewhere]).expect("held"),
            clip: None,
        })
        .expect("rebuilt");
    let after = before
        .with_node_at(&[0], Arc::new(rebuilt))
        .expect("edited");
    let area = changed_area(&before, &after);
    assert!(area.min.x >= 999.0 && area.max.y >= 510.0, "{area:?}");
    assert!(changed_area(&before, &before).is_empty());
}

#[test]
fn a_change_to_the_page_alone_stales_no_tile() {
    let mut ids = Ids::new();
    let before = drawing(vec![rect(&mut ids, 0.0)], &mut ids);
    let page = *before.page();
    let after = before
        .with_page(tairix_drawing::Page {
            width: page.width * 2.0,
            background: Some(tairix_colour::Rgba::new(0, 0, 255, 255)),
            ..page
        })
        .expect("a new page");
    assert!(changed_area(&before, &after).is_empty());
}

#[test]
fn a_tile_is_asked_for_once_kept_when_it_lands_and_asked_again_once_stale() {
    let mut ids = Ids::new();
    let before = drawing(vec![rect(&mut ids, 0.0), rect(&mut ids, 1000.0)], &mut ids);
    let mut tiles = Tiles::default();
    let visible = [key(0, 0), key(3, 0)];
    assert_eq!(tiles.wanted(&visible), visible.to_vec());
    assert!(tiles.wanted(&visible).is_empty(), "asked once");
    for at in visible {
        tiles.landed(0, at, tile(), &visible);
    }
    assert!(tiles.wanted(&visible).is_empty(), "both fresh");
    // Move the far rectangle within its tile: only that tile goes stale.
    let mut nodes = before
        .layers()
        .get(0)
        .and_then(|layer| layer.children())
        .expect("children")
        .to_vec()
        .expect("held");
    nodes[1] = Arc::new(
        nodes[1]
            .with_transform(tairix_bezier::Affine::translate(5.0, 0.0))
            .expect("moved"),
    );
    let after = drawing(nodes, &mut ids);
    tiles.changed(&before, &after, 1);
    assert_eq!(tiles.wanted(&visible), vec![key(3, 0)]);
    assert!(
        tiles.tile(&key(3, 0)).is_some(),
        "a stale tile is painted on until replaced"
    );
}

#[test]
fn a_window_has_one_job_out_whatever_changes_while_it_is() {
    let mut ids = Ids::new();
    let before = drawing(vec![rect(&mut ids, 0.0)], &mut ids);
    let after = drawing(vec![rect(&mut ids, 1.0)], &mut ids);
    let mut tiles = Tiles::default();
    let visible = [key(0, 0), key(1, 0)];
    let out = tiles.wanted(&visible);
    assert_eq!(out, visible.to_vec());
    tiles.changed(&before, &after, 1);
    assert!(
        tiles.wanted(&visible).is_empty(),
        "the job out is still out"
    );
    tiles.invalidate(2);
    assert!(
        tiles.wanted(&visible).is_empty(),
        "even past a change of mode"
    );
    for at in out {
        tiles.landed(0, at, tile(), &visible);
    }
    assert_eq!(
        tiles.wanted(&visible),
        visible.to_vec(),
        "what it drew is stale: asked again once it is in"
    );
}

#[test]
fn a_job_never_taken_is_asked_for_again_and_one_never_drawn_is_not() {
    let mut tiles = Tiles::default();
    let visible = [key(0, 0), key(1, 0)];
    let out = tiles.wanted(&visible);
    tiles.not_taken(&out);
    assert_eq!(tiles.wanted(&visible), out, "the queue was full: ask again");
    tiles.refused(&out);
    assert!(
        tiles.wanted(&visible).is_empty(),
        "a render that could not draw them is not asked in a loop"
    );
}

#[test]
fn a_tile_from_an_earlier_scene_is_taken_only_if_nothing_since_touched_it() {
    let mut ids = Ids::new();
    let before = drawing(vec![rect(&mut ids, 0.0)], &mut ids);
    let mut nodes = before
        .layers()
        .get(0)
        .and_then(|layer| layer.children())
        .expect("children")
        .to_vec()
        .expect("held");
    nodes[0] = Arc::new(
        nodes[0]
            .with_transform(tairix_bezier::Affine::translate(1.0, 0.0))
            .expect("moved"),
    );
    let after = drawing(nodes, &mut ids);
    let mut tiles = Tiles::default();
    let visible = [key(0, 0), key(5, 0)];
    let _ = tiles.wanted(&visible);
    tiles.changed(&before, &after, 1);
    tiles.landed(0, key(5, 0), tile(), &visible);
    tiles.landed(0, key(0, 0), tile(), &visible);
    assert!(tiles.tile(&key(5, 0)).is_some(), "untouched since: taken");
    assert!(tiles.tile(&key(0, 0)).is_none(), "touched since: refused");
}

#[test]
fn the_cache_holds_the_view_twice_over_beyond_it_and_less_under_pressure() {
    let mut tiles = Tiles::default();
    let visible = [key(0, 0), key(1, 0)];
    for column in 0..10 {
        tiles.landed(0, key(column, 0), tile(), &visible);
    }
    let held = |tiles: &Tiles| {
        (0..10)
            .filter(|&column| tiles.tile(&key(column, 0)).is_some())
            .count()
    };
    assert_eq!(held(&tiles), 6, "the two in view and twice as many again");
    assert!(tiles.tile(&key(0, 0)).is_some() && tiles.tile(&key(5, 0)).is_some());
    assert!(
        tiles.tile(&key(6, 0)).is_none(),
        "the furthest let go first"
    );
    tiles.adopt_pressure(PressureBand::Moderate, &visible);
    assert_eq!(held(&tiles), 4);
    tiles.adopt_pressure(PressureBand::Severe, &visible);
    assert_eq!(held(&tiles), 2, "nothing but the view");
    assert!(tiles.tile(&key(0, 0)).is_some() && tiles.tile(&key(1, 0)).is_some());
}

fn at_zoom(pixels: f64, columns: core::ops::Range<i64>, outline: bool) -> Vec<TileKey> {
    columns
        .map(|column| TileKey {
            pixels: pixels.to_bits(),
            column,
            row: 0,
            outline,
        })
        .collect()
}

fn land_all(tiles: &mut Tiles, visible: &[TileKey]) {
    let keys = tiles.wanted(visible);
    for key in keys {
        tiles.landed(0, key, tile(), visible);
    }
}

#[test]
fn a_zoom_from_a_view_shown_whole_stands_it_in_until_the_new_view_lands() {
    let mut tiles = Tiles::default();
    let before = at_zoom(1.0, 0..2, false);
    land_all(&mut tiles, &before);
    assert!(tiles.stand_in(false).is_none(), "nothing to stand in for");
    let after = at_zoom(2.0, 0..4, false);
    let asked = tiles.wanted(&after);
    let stand_in = tiles.stand_in(false).expect("the view just left");
    assert_eq!(stand_in.pixels.to_bits(), 1.0_f64.to_bits());
    assert_eq!(stand_in.origin, Point::ZERO);
    assert_eq!(
        tairix_raster::Picture::size(&stand_in.picture),
        (2 * TILE, TILE)
    );
    assert_eq!(
        stand_in.onto(2.0).map(Point::new(10.0, 20.0)),
        Point::new(20.0, 40.0),
        "its pixels at the new zoom"
    );
    assert!(
        tiles.stand_in(true).is_none(),
        "and never for the other mode"
    );
    for key in asked {
        tiles.landed(0, key, tile(), &after);
    }
    let _ = tiles.wanted(&after);
    assert!(
        tiles.stand_in(false).is_none(),
        "let go once the view is whole"
    );
}

#[test]
fn a_view_not_shown_whole_never_displaces_one_that_was() {
    let mut tiles = Tiles::default();
    land_all(&mut tiles, &at_zoom(1.0, 0..2, false));
    let halfway = at_zoom(2.0, 0..4, false);
    let asked = tiles.wanted(&halfway);
    tiles.landed(0, asked[0], tile(), &halfway);
    tiles.not_taken(&asked);
    let _ = tiles.wanted(&at_zoom(3.0, 0..6, false));
    let stand_in = tiles.stand_in(false).expect("still the whole view");
    assert_eq!(stand_in.pixels.to_bits(), 1.0_f64.to_bits());

    // With none kept, part of a view is better than none.
    let mut fresh = Tiles::default();
    let first = at_zoom(1.0, -1..2, false);
    let asked = fresh.wanted(&first);
    fresh.landed(0, asked[0], tile(), &first);
    let _ = fresh.wanted(&at_zoom(0.5, -1..1, false));
    let stand_in = fresh.stand_in(false).expect("the part that landed");
    assert_eq!(stand_in.origin, Point::new(-f64::from(TILE), 0.0));
    let (width, _) = tairix_raster::Picture::size(&stand_in.picture);
    assert_eq!(width, 3 * TILE);
}

#[test]
fn a_change_of_mode_or_severe_pressure_lets_the_backdrop_go() {
    let mut tiles = Tiles::default();
    land_all(&mut tiles, &at_zoom(1.0, 0..2, false));
    let _ = tiles.wanted(&at_zoom(2.0, 0..4, false));
    assert!(tiles.stand_in(false).is_some());
    let _ = tiles.wanted(&at_zoom(2.0, 0..4, true));
    assert!(tiles.stand_in(false).is_none(), "the mode changed");

    let mut pressed = Tiles::default();
    let before = at_zoom(1.0, 0..2, false);
    land_all(&mut pressed, &before);
    let _ = pressed.wanted(&at_zoom(2.0, 0..4, false));
    pressed.adopt_pressure(PressureBand::Severe, &at_zoom(2.0, 0..4, false));
    assert!(pressed.stand_in(false).is_none(), "severe pressure");
    let _ = pressed.wanted(&at_zoom(4.0, 0..8, false));
    assert!(
        pressed.stand_in(false).is_none(),
        "and none is kept under it"
    );
}
