use alloc::vec::Vec;

use tairix_cursor::{CursorImage, Shape, VectorCursor};

use super::{Ghost, Halo, HaloRing, PointerOverlay, MAX_GHOSTS, MAX_HALO_RINGS};
use crate::color::Color;
use crate::geometry::{Point, Rect};

const SCREEN: Rect = Rect::new(0, 0, 400, 400);

fn cursor(side: u32) -> CursorImage {
    let s = i32::try_from(side).expect("small side");
    let shape = Shape::from_points(Color::rgb(250, 250, 250), &[(0, 0), (s, 0), (s, s), (0, s)]);
    VectorCursor::new(side, 0, 0, alloc::vec![shape])
        .rasterise(side)
        .expect("renderable")
}

fn ring(radius: u32, width: u32) -> HaloRing {
    HaloRing {
        radius,
        width,
        color: Color::rgba(230, 90, 20, 230),
    }
}

fn halo(rings: &[HaloRing]) -> Halo {
    let mut halo = Halo::new();
    for ring in rings {
        assert!(halo.push(*ring));
    }
    halo
}

fn owed(overlay: &mut PointerOverlay) -> Vec<Rect> {
    let mut marked = Vec::new();
    overlay.settle(|rect| marked.push(rect));
    marked
}

fn area(rects: &[Rect]) -> u64 {
    rects
        .iter()
        .map(|rect| u64::from(rect.width) * u64::from(rect.height))
        .sum()
}

#[test]
fn every_pixel_a_halo_draws_lies_in_its_footprint_and_its_hole_does_not() {
    for (radius, width) in [(1, 1), (3, 1), (6, 2), (12, 3), (40, 4), (97, 6), (160, 9)] {
        for centre in [Point::new(200, 200), Point::new(3, 7)] {
            let mut overlay = PointerOverlay::new();
            overlay.move_to(centre);
            assert!(overlay.set_halo(&halo(&[ring(radius, width)])));
            let footprint = overlay.footprint().halo;
            let sprites = overlay.sprites();
            let art = sprites.first().expect("the halo is drawn");
            let bounds = art.bounds();
            for ly in 0..bounds.height {
                for lx in 0..bounds.width {
                    if art.sample_local(lx, ly).is_none() {
                        continue;
                    }
                    let at = Point::new(
                        bounds.left() + i32::try_from(lx).expect("small"),
                        bounds.top() + i32::try_from(ly).expect("small"),
                    );
                    assert!(
                        footprint.iter().any(|rect| rect.contains(at)),
                        "radius {radius} width {width}: {at:?} is drawn but not covered"
                    );
                }
            }
            if radius >= 40 {
                assert!(
                    !footprint.iter().any(|rect| rect.contains(centre)),
                    "the hole is left out"
                );
                let square = u64::from(radius * 2).pow(2);
                assert!(
                    area(&footprint) * 2 < square,
                    "radius {radius}: a thin band costs well under its square"
                );
            }
        }
    }
}

#[test]
fn overlapping_rings_share_one_band_and_distant_ones_keep_their_own() {
    let mut overlay = PointerOverlay::new();
    overlay.move_to(Point::new(200, 200));
    overlay.set_halo(&halo(&[ring(60, 6), ring(58, 2)]));
    let shared = overlay.footprint().halo.len();
    overlay.set_halo(&halo(&[ring(60, 6)]));
    assert_eq!(
        overlay.footprint().halo.len(),
        shared,
        "a ring inside another adds nothing"
    );
    overlay.set_halo(&halo(&[ring(90, 4), ring(40, 4)]));
    assert!(
        overlay.footprint().halo.len() > shared,
        "two bands apart are two annuli"
    );
    let footprint = overlay.footprint().halo;
    assert!(
        !footprint
            .iter()
            .any(|rect| rect.contains(Point::new(200 + 64, 200))),
        "the gap between two rings is not recomposed"
    );
}

#[test]
fn a_moving_cursor_owes_only_its_own_rectangles() {
    let mut overlay = PointerOverlay::new();
    overlay.set_cursor(cursor(8), Point::new(10, 10));
    overlay.set_trail(&[Ghost {
        at: Point::new(60, 60),
        opacity: 128,
    }]);
    let _ = owed(&mut overlay);
    // The halo is centred on the pointer, so it would follow the cursor; a
    // trail stays where the pointer was.
    assert!(overlay.move_to(Point::new(14, 10)));
    let marked = owed(&mut overlay);
    assert_eq!(marked, [Rect::new(10, 10, 8, 8), Rect::new(14, 10, 8, 8)]);
    assert!(owed(&mut overlay).is_empty(), "nothing is owed twice");
}

#[test]
fn a_halo_follows_the_pointer_and_owes_where_it_was_and_is() {
    let mut overlay = PointerOverlay::new();
    overlay.move_to(Point::new(100, 100));
    overlay.set_halo(&halo(&[ring(30, 3)]));
    let first = owed(&mut overlay);
    assert!(!first.is_empty());
    overlay.move_to(Point::new(140, 100));
    let marked = owed(&mut overlay);
    assert_eq!(
        marked.len(),
        first.len() * 2,
        "where it was and where it is"
    );
    assert!(overlay.set_halo(&Halo::new()));
    let cleared = owed(&mut overlay);
    assert_eq!(
        cleared.len(),
        first.len(),
        "clearing owes only where it was"
    );
    assert!(overlay.sprites().is_empty());
}

#[test]
fn a_trail_is_its_newest_ghosts_and_draws_none_at_no_opacity() {
    let mut overlay = PointerOverlay::new();
    overlay.set_cursor(cursor(4), Point::ORIGIN);
    let ghosts: Vec<Ghost> = (0..MAX_GHOSTS + 3)
        .map(|index| Ghost {
            at: Point::new(i32::try_from(index).expect("small") * 10, 0),
            opacity: 200,
        })
        .collect();
    assert!(overlay.set_trail(&ghosts));
    assert!(
        !overlay.set_trail(&ghosts[3..]),
        "the same newest ghosts change nothing"
    );
    assert_eq!(overlay.sprites().len(), MAX_GHOSTS + 1);
    assert_eq!(
        overlay
            .sprites()
            .first()
            .map(|sprite| sprite.bounds().left()),
        Some(30),
        "the oldest kept ghost is drawn first"
    );

    let faint = [
        Ghost {
            at: Point::new(40, 40),
            opacity: 0,
        },
        Ghost {
            at: Point::new(50, 40),
            opacity: 90,
        },
    ];
    assert!(overlay.set_trail(&faint));
    assert_eq!(
        overlay.sprites().len(),
        2,
        "a ghost at no opacity is not drawn"
    );
    assert!(
        !overlay.set_trail(&faint),
        "and the same trail again is no change"
    );
}

#[test]
fn sprites_draw_the_trail_then_the_halo_then_the_cursor() {
    let mut overlay = PointerOverlay::new();
    overlay.set_cursor(cursor(6), Point::new(50, 50));
    overlay.set_trail(&[Ghost {
        at: Point::new(20, 20),
        opacity: 100,
    }]);
    overlay.set_halo(&halo(&[ring(20, 2)]));
    let sprites = overlay.sprites();
    let bounds: Vec<Rect> = sprites.iter().map(super::Sprite::bounds).collect();
    assert_eq!(
        bounds,
        [
            Rect::new(20, 20, 6, 6),
            Rect::new(30, 30, 40, 40),
            Rect::new(50, 50, 6, 6),
        ]
    );
    let ghost = sprites.first().expect("a ghost");
    let pixel = ghost.sample_local(0, 0).expect("drawn");
    assert_eq!(pixel.a, 100, "a ghost is drawn at its own opacity");
}

#[test]
fn a_shrinking_halo_is_redrawn_into_the_buffer_it_has() {
    let mut overlay = PointerOverlay::new();
    overlay.set_halo(&halo(&[ring(80, 4)]));
    let held = overlay.art.as_ref().map(|art| art.pixels().as_ptr());
    overlay.set_halo(&halo(&[ring(50, 4)]));
    assert_eq!(overlay.art.as_ref().map(|art| art.pixels().as_ptr()), held);
    assert!(
        !overlay.set_halo(&halo(&[ring(50, 4)])),
        "an unchanged halo is no work"
    );
}

#[test]
fn a_halo_redrawn_in_place_leaves_nothing_of_the_last_one() {
    let mut overlay = PointerOverlay::new();
    for (radius, width) in [(90, 6), (71, 4), (40, 5), (33, 2), (12, 3)] {
        overlay.set_halo(&halo(&[ring(radius, width), ring(radius / 2 + 1, 2)]));
    }
    let mut fresh = PointerOverlay::new();
    fresh.set_halo(&halo(&[ring(90, 1)]));
    fresh.set_halo(&halo(&[ring(12, 3), ring(7, 2)]));
    assert_eq!(
        overlay.art.as_ref().map(|art| art.pixels().to_vec()),
        fresh.art.as_ref().map(|art| art.pixels().to_vec()),
        "only the last halo's rings are on the drawing"
    );
}

#[test]
fn a_hidden_overlay_draws_nothing_and_owes_what_it_had_drawn() {
    let mut overlay = PointerOverlay::new();
    overlay.set_cursor(cursor(8), Point::new(10, 10));
    overlay.set_halo(&halo(&[ring(20, 2)]));
    let shown = owed(&mut overlay);
    assert!(overlay.set_hidden(true));
    assert!(overlay.sprites().is_empty());
    assert_eq!(overlay.cursor_bounds(), None);
    assert_eq!(owed(&mut overlay), shown, "erasing is owed where it was");
    assert_eq!(overlay.drawn().count(), 0);
}

#[test]
fn damage_is_owed_exactly_when_settling_would_mark_the_screen() {
    let mut overlay = PointerOverlay::new();
    assert!(!overlay.has_damage(SCREEN));
    overlay.set_cursor(cursor(8), Point::new(10, 10));
    assert!(overlay.has_damage(SCREEN));
    let _ = owed(&mut overlay);
    assert!(!overlay.has_damage(SCREEN));
    overlay.move_to(Point::new(10, 10));
    assert!(
        !overlay.has_damage(SCREEN),
        "landing where it was is no work"
    );
    overlay.move_to(Point::new(1000, 1000));
    assert!(overlay.has_damage(SCREEN), "it left pixels on screen");
    let _ = owed(&mut overlay);
    overlay.move_to(Point::new(2000, 2000));
    assert!(
        !overlay.has_damage(SCREEN),
        "moving wholly off screen owes nothing"
    );
}

#[test]
fn replacing_the_cursor_hands_back_the_image_it_replaced() {
    let mut overlay = PointerOverlay::new();
    assert_eq!(overlay.set_cursor(cursor(8), Point::ORIGIN), None);
    assert_eq!(
        overlay.set_cursor(cursor(4), Point::ORIGIN),
        Some(cursor(8))
    );
}

#[test]
fn a_halo_holds_its_rings_and_refuses_one_past_its_bound() {
    let mut halo = Halo::new();
    assert!(halo.push(ring(0, 3)), "a ring of no radius draws nothing");
    assert!(halo.rings().is_empty());
    for radius in 1..=u32::try_from(MAX_HALO_RINGS).expect("small") {
        assert!(halo.push(ring(radius * 10, 2)));
    }
    assert!(!halo.push(ring(90, 2)));
    assert_eq!(halo.rings().len(), MAX_HALO_RINGS);
}
