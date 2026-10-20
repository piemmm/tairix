//! Painter tests.
//!
//! A frame is pixels, so these hold the painter to what a test can see: every
//! game draws a whole table, a clipped repaint writes nothing outside its
//! clip, the things a player must see actually change pixels, and a frame
//! drawn mid-change under reduced motion is the frame of the table at rest.

use alloc::vec::Vec;

use tairix_geometry::{Rect, Region, Scale};
use tairix_raster::Pixel;
use tairix_theme::Theme;

use super::*;
use crate::card::SpiderSuits;
use crate::deal::DealNumber;
use crate::layout::Placement;
use crate::motion::{Cascade, Timing};
use crate::prefs::Preferences;
use crate::variant::{Draw, Rules};

const CLIENT: Rect = Rect::new(0, 0, 900, 680);
const STATUS: Status<'static> = Status {
    game: "Klondike \u{b7} deal 7",
    message: "",
    readings: "0 moves   0:00",
};

struct Scene {
    layout: Layout,
    table: Table,
    motion: Motion,
    art: CardArt,
    placements: Vec<Placement>,
}

impl Scene {
    fn new(variant: Variant, reduced: bool) -> Self {
        let layout = Layout::resolve(CLIENT, variant, Scale::ONE);
        let table = Table::deal(
            Rules::of(variant),
            DealNumber::new(variant, 7).expect("a deal"),
        )
        .0;
        let mut placements = Vec::new();
        layout.placements(&table, &mut placements);
        let mut motion = Motion::new(variant.deck().count(), layout.card_size(), reduced);
        motion.snap(&placements, layout.card_size());
        let palette = Palette::resolve(&Theme::dark(), &Preferences::default());
        let art = CardArt::new(
            layout.card_width,
            layout.card_height,
            layout.radius,
            layout.hairline,
            palette,
        );
        Self {
            layout,
            table,
            motion,
            art,
            placements,
        }
    }

    fn draw(&mut self, surface: &mut Surface, extra: Extra<'_>, now_ns: u64) {
        let view = View {
            layout: &self.layout,
            table: &self.table,
            motion: &self.motion,
            held: extra.held,
            cursor: extra.cursor,
            target: extra.target,
            status: STATUS,
            font: BitmapFont::monospace(14),
            now_ns,
        };
        frame(surface, &view, &mut self.art);
    }

    fn pixels(&mut self, extra: Extra<'_>, now_ns: u64) -> Vec<Pixel> {
        let mut surface = canvas();
        self.draw(&mut surface, extra, now_ns);
        surface.pixels().to_vec()
    }
}

#[derive(Copy, Clone, Default)]
struct Extra<'a> {
    held: &'a [(CardId, Point)],
    cursor: Option<Cursor>,
    target: Option<PileId>,
}

fn canvas() -> Surface {
    Surface::new(CLIENT.width, CLIENT.height).expect("a surface")
}

fn column(variant: Variant, index: usize) -> PileId {
    variant.shape().tableau.nth(index).expect("a column")
}

#[test]
fn every_game_draws_a_whole_table() {
    for variant in Variant::ALL {
        let mut scene = Scene::new(variant, false);
        let pixels = scene.pixels(Extra::default(), 0);
        assert!(
            pixels.iter().all(|pixel| pixel.a == 255),
            "{variant:?} leaves nothing undrawn"
        );
        let mut bare = canvas();
        waiting(
            &mut bare,
            &scene.layout,
            STATUS,
            BitmapFont::monospace(14),
            &mut scene.art,
        );
        assert_ne!(
            bare.pixels(),
            pixels.as_slice(),
            "{variant:?} draws its cards"
        );
    }
}

#[test]
fn the_empty_table_draws_the_baize_and_every_place() {
    let mut scene = Scene::new(Variant::FreeCell, false);
    let mut surface = canvas();
    waiting(
        &mut surface,
        &scene.layout,
        STATUS,
        BitmapFont::monospace(14),
        &mut scene.art,
    );
    assert!(surface.pixels().iter().all(|pixel| pixel.a == 255));
    let place = scene
        .layout
        .card_at(scene.layout.origin(column(Variant::FreeCell, 0)));
    let centre = place.center();
    let baize = Point::new(scene.layout.felt.left() + 2, scene.layout.felt.bottom() - 2);
    let at = |point: Point| {
        surface.get(
            u32::try_from(point.x).expect("on"),
            u32::try_from(point.y).expect("on"),
        )
    };
    assert_ne!(at(centre), at(baize), "a place is a recess in the baize");
}

#[test]
fn a_clipped_repaint_writes_only_inside_its_clip() {
    let variant = Variant::Klondike(Draw::Three);
    let mut scene = Scene::new(variant, false);
    let mut surface = canvas();
    scene.draw(&mut surface, Extra::default(), 0);
    let before = surface.pixels().to_vec();
    let pile = column(variant, 3);
    let cursor = Some(Cursor {
        pile,
        depth: Some(scene.table.pile(pile).len() - 1),
    });
    let clip = scene.layout.pile_area(&scene.table, pile);
    let (x, y) = clip.surface_origin().expect("on the surface");
    surface.with_clip(x, y, clip.width, clip.height, |surface| {
        scene.draw(
            surface,
            Extra {
                cursor,
                ..Extra::default()
            },
            0,
        );
    });
    let after = surface.pixels();
    let width = usize::try_from(CLIENT.width).expect("a width");
    let mut changed = false;
    for (index, (was, is)) in before.iter().zip(after).enumerate() {
        let point = Point::new(
            i32::try_from(index % width).expect("x"),
            i32::try_from(index / width).expect("y"),
        );
        if was != is {
            changed = true;
            assert!(clip.contains(point), "{point:?} written outside the clip");
        }
    }
    assert!(changed, "the cursor's ring is drawn inside the clip");
}

#[test]
fn the_cursor_and_a_landing_place_are_drawn() {
    let variant = Variant::FreeCell;
    let mut scene = Scene::new(variant, false);
    let plain = scene.pixels(Extra::default(), 0);
    let pile = column(variant, 2);
    let cursor = Some(Cursor {
        pile,
        depth: Some(0),
    });
    assert_ne!(
        scene.pixels(
            Extra {
                cursor,
                ..Extra::default()
            },
            0
        ),
        plain
    );
    let target = Some(column(variant, 5));
    assert_ne!(
        scene.pixels(
            Extra {
                target,
                ..Extra::default()
            },
            0
        ),
        plain
    );
}

#[test]
fn a_held_card_is_drawn_where_the_pointer_holds_it() {
    let variant = Variant::Klondike(Draw::One);
    let mut scene = Scene::new(variant, false);
    let pile = column(variant, 6);
    let card = scene.table.top(pile).expect("a top card").card();
    let rest = scene.placements[card.index()].at;
    let away = Point::new(rest.x - 300, rest.y + 200);
    let held = [(card, away)];
    let plain = scene.pixels(Extra::default(), 0);
    let holding = scene.pixels(
        Extra {
            held: &held,
            ..Extra::default()
        },
        0,
    );
    let width = usize::try_from(CLIENT.width).expect("a width");
    let index_of = |point: Point| {
        usize::try_from(point.y).expect("y") * width + usize::try_from(point.x).expect("x")
    };
    let size = scene.layout.card_size();
    let middle = |at: Point| Point::new(at.x + to_i32(size.0 / 2), at.y + to_i32(size.1 / 2));
    assert_ne!(
        plain[index_of(middle(away))],
        holding[index_of(middle(away))]
    );
    assert_ne!(
        plain[index_of(middle(rest))],
        holding[index_of(middle(rest))],
        "its place shows what it covered"
    );
}

#[test]
fn reduced_motion_draws_a_change_as_the_table_at_rest() {
    let variant = Variant::Spider(SpiderSuits::One);
    let mut moving = Scene::new(variant, true);
    let from = column(variant, 0);
    let to = column(variant, 1);
    let top = moving.table.top(from).expect("a top card").card();
    let mut placements = moving.placements.clone();
    let landing = moving.layout.landing(&moving.table, to);
    placements[top.index()].at = Point::new(landing.left(), landing.top());
    moving
        .motion
        .retarget(0, &placements, &[], |_| Timing::SLIDE, &mut Region::new());
    let mut still = Scene::new(variant, true);
    still.motion.snap(&placements, still.layout.card_size());
    assert_eq!(
        moving.pixels(Extra::default(), 1),
        still.pixels(Extra::default(), 1)
    );
}

#[test]
fn a_card_in_flight_is_drawn_between_its_places() {
    let variant = Variant::FreeCell;
    let mut scene = Scene::new(variant, false);
    let at_rest = scene.pixels(Extra::default(), 0);
    let top = scene
        .table
        .top(column(variant, 0))
        .expect("a top card")
        .card();
    let mut placements = scene.placements.clone();
    placements[top.index()].at = Point::new(placements[top.index()].at.x + 400, 40);
    scene
        .motion
        .retarget(0, &placements, &[], |_| Timing::SLIDE, &mut Region::new());
    let midway = scene.pixels(Extra::default(), Timing::SLIDE.span_ns / 2);
    let landed = scene.pixels(Extra::default(), Timing::SLIDE.span_ns);
    assert_ne!(midway, at_rest);
    assert_ne!(midway, landed);
}

#[test]
fn the_victory_draws_its_bouncing_cards_over_what_is_there() {
    let variant = Variant::Klondike(Draw::One);
    let mut scene = Scene::new(variant, false);
    let top = scene
        .table
        .top(column(variant, 6))
        .expect("a top card")
        .card();
    let home = alloc::vec![(top, scene.placements[top.index()].at)];
    let felt = scene.layout.felt;
    let size = scene.layout.card_size();
    scene.motion.celebrate(Cascade::new(
        home,
        (felt.left(), felt.right(), felt.bottom()),
        size,
        0,
        9,
    ));
    scene
        .motion
        .advance(crate::motion::FRAME_NS, &mut Region::new());
    let cascade = scene.motion.cascade().expect("a victory");
    assert!(cascade.begun());
    let (_, at) = cascade.bouncing().next().expect("a card thrown");
    let fill = Pixel {
        r: 1,
        g: 2,
        b: 3,
        a: 255,
    };
    let mut surface = Surface::filled(CLIENT.width, CLIENT.height, fill).expect("a surface");
    scene.draw(&mut surface, Extra::default(), crate::motion::FRAME_NS);
    let card = scene.layout.card_at(at);
    let width = usize::try_from(CLIENT.width).expect("a width");
    for (index, pixel) in surface.pixels().iter().enumerate() {
        let point = Point::new(
            i32::try_from(index % width).expect("x"),
            i32::try_from(index / width).expect("y"),
        );
        let reach = to_i32(size.1 / 10);
        let near = Rect::new(
            card.left() - reach,
            card.top() - reach,
            card.width + size.1 / 5,
            card.height + size.1 / 5,
        );
        if !near.contains(point) && !scene.layout.status.contains(point) {
            assert_eq!(
                *pixel, fill,
                "{point:?}: the trail is what the frame already held"
            );
        }
    }
    let middle = card.center();
    let drawn = surface.get(
        u32::try_from(middle.x).expect("x"),
        u32::try_from(middle.y).expect("y"),
    );
    assert_ne!(drawn, Some(fill));
}

#[test]
fn the_kept_baize_draws_what_shading_it_afresh_would() {
    let mut scene = Scene::new(Variant::Klondike(Draw::One), false);
    let palette = *scene.art.palette();
    let mut kept = canvas();
    felt(&mut kept, scene.layout.felt, &mut scene.art);
    let mut fresh = canvas();
    shade_felt(&mut fresh, scene.layout.felt, &palette);
    assert_eq!(kept.pixels(), fresh.pixels());
    // A repaint clipped to a corner copies only that corner.
    let mut clipped = canvas();
    clipped.with_clip(10, 10, 40, 30, |surface| {
        felt(surface, scene.layout.felt, &mut scene.art);
    });
    let width = usize::try_from(CLIENT.width).expect("a width");
    for (index, pixel) in clipped.pixels().iter().enumerate() {
        let (x, y) = (index % width, index / width);
        let inside = (10..50).contains(&x) && (10..40).contains(&y);
        let wanted = if inside {
            fresh.pixels()[index]
        } else {
            Pixel::TRANSPARENT
        };
        assert_eq!(*pixel, wanted, "({x}, {y})");
    }
}
