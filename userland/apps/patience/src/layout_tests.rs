//! Layout tests: every card of every game lies on the table at every size a
//! window can take, and the pointer finds what is drawn under it.

use alloc::vec::Vec;

use super::*;
use crate::card::{Slot, SpiderSuits};
use crate::deal::DealNumber;
use crate::variant::{Draw, Rules};

fn scales() -> [Scale; 3] {
    [
        Scale::ONE,
        Scale::from_percent(150).expect("a scale"),
        Scale::from_percent(200).expect("a scale"),
    ]
}

/// The client areas a window can have at `scale`: the floor, the opening
/// size, and a large screen.
fn clients(scale: Scale) -> [Rect; 3] {
    let at = |(width, height): (u32, u32)| {
        Rect::new(0, 0, scale.scale_length(width), scale.scale_length(height))
    };
    [at(MIN_SIZE), at(OPEN_SIZE), at((2560, 1440))]
}

fn dealt(variant: Variant) -> Table {
    Table::deal(
        Rules::of(variant),
        DealNumber::new(variant, 7).expect("a deal"),
    )
    .0
}

/// Every card of `table` in one column, face up: the longest a column grows.
fn one_column(variant: Variant) -> Table {
    let deck = variant.deck();
    let shape = variant.shape();
    let mut piles = alloc::vec![Vec::new(); shape.piles()];
    let column = shape.tableau.nth(0).expect("a column").index();
    piles[column] = deck.cards().map(|card| Slot::new(card, true)).collect();
    Table::laid(Rules::of(variant), piles)
}

fn inside(outer: Rect, inner: Rect) -> bool {
    inner.left() >= outer.left()
        && inner.top() >= outer.top()
        && inner.right() <= outer.right()
        && inner.bottom() <= outer.bottom()
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.left() < b.right() && b.left() < a.right() && a.top() < b.bottom() && b.top() < a.bottom()
}

#[test]
fn every_pile_lies_on_the_table_without_touching_another() {
    for variant in Variant::ALL {
        for scale in scales() {
            for client in clients(scale) {
                let layout = Layout::resolve(client, variant, scale);
                let places: Vec<Rect> = variant
                    .shape()
                    .all()
                    .map(|pile| layout.card_at(layout.origin(pile)))
                    .collect();
                for (index, &place) in places.iter().enumerate() {
                    assert!(inside(layout.felt, place), "{variant:?} at {client:?}");
                    for &other in &places[index + 1..] {
                        assert!(!overlaps(place, other), "{variant:?} at {client:?}");
                    }
                }
            }
        }
    }
}

#[test]
fn the_card_grows_with_the_window_between_its_bounds() {
    let variant = Variant::Klondike(Draw::One);
    let tiny = Layout::resolve(Rect::new(0, 0, 200, 150), variant, Scale::ONE);
    assert_eq!(tiny.card_width, CARD_MIN);
    let huge = Layout::resolve(Rect::new(0, 0, 8000, 6000), variant, Scale::ONE);
    assert_eq!(huge.card_width, CARD_MAX);
    let open = Layout::resolve(clients(Scale::ONE)[1], variant, Scale::ONE);
    assert!(open.card_width > tiny.card_width && open.card_width < huge.card_width);
    assert_eq!(open.card_height, open.card_width * ASPECT / 1000);
    let doubled = Layout::resolve(Rect::new(0, 0, 200, 150), variant, scales()[2]);
    assert_eq!(doubled.card_width, CARD_MIN * 2);
}

#[test]
fn a_long_column_closes_up_to_stay_on_the_table() {
    for variant in Variant::ALL {
        let table = one_column(variant);
        let column = variant.shape().tableau.nth(0).expect("a column");
        for scale in scales() {
            for client in clients(scale) {
                let layout = Layout::resolve(client, variant, scale);
                let fan = layout.fan(&table, column);
                let last = table.pile(column).len() - 1;
                let bottom = layout
                    .card_at(layout.place(&table, column, last, fan))
                    .bottom();
                // The floor binds while the least fan still fits; past that the
                // column runs on at the least fan, which still shows every rank.
                let fits = fan.up > layout.least.up;
                assert!(!fits || bottom <= layout.floor, "{variant:?} at {client:?}");
                assert!(fan.up >= layout.least.up && fan.up <= layout.natural.up);
            }
        }
    }
}

#[test]
fn a_short_column_keeps_its_natural_fan() {
    let variant = Variant::Klondike(Draw::Three);
    let table = dealt(variant);
    let layout = Layout::resolve(clients(Scale::ONE)[1], variant, Scale::ONE);
    for column in variant.shape().tableau.iter() {
        assert_eq!(layout.fan(&table, column), layout.natural);
    }
}

#[test]
fn every_card_rests_on_the_table() {
    for variant in Variant::ALL {
        let table = dealt(variant);
        for scale in scales() {
            for client in clients(scale) {
                let layout = Layout::resolve(client, variant, scale);
                let mut placements = Vec::new();
                layout.placements(&table, &mut placements);
                assert_eq!(placements.len(), table.deck().count());
                for pile in variant.shape().all() {
                    for slot in table.pile(pile) {
                        let placement = placements[slot.card().index()];
                        assert_eq!(placement.up, slot.up());
                        assert!(
                            inside(layout.felt, layout.card_at(placement.at)),
                            "{variant:?}"
                        );
                    }
                }
            }
        }
    }
}

#[test]
fn the_pointer_finds_every_card_where_it_shows() {
    for variant in Variant::ALL {
        let table = dealt(variant);
        let layout = Layout::resolve(clients(Scale::ONE)[1], variant, Scale::ONE);
        for pile in variant.shape().all() {
            let fan = layout.fan(&table, pile);
            let cards = table.pile(pile);
            for depth in 0..cards.len() {
                let rect = layout.card_at(layout.place(&table, pile, depth, fan));
                // A point on the part of the card the one above leaves showing.
                let next =
                    (depth + 1 < cards.len()).then(|| layout.place(&table, pile, depth + 1, fan));
                let point = match next {
                    None => Some(rect.center()),
                    Some(next) if next.y > rect.top() => Some(Point::new(
                        rect.center().x,
                        rect.top() + (next.y - rect.top()) / 2,
                    )),
                    Some(next) if next.x > rect.left() => Some(Point::new(
                        rect.left() + (next.x - rect.left()) / 2,
                        rect.center().y,
                    )),
                    Some(_) => None,
                };
                if let Some(point) = point {
                    assert_eq!(
                        layout.hit(&table, point),
                        Some((pile, Some(depth))),
                        "{variant:?}"
                    );
                }
            }
            if cards.is_empty() {
                let place = layout.card_at(layout.origin(pile));
                assert_eq!(layout.hit(&table, place.center()), Some((pile, None)));
            }
        }
        let corner = Point::new(layout.felt.left(), layout.felt.bottom() - 1);
        assert_eq!(layout.hit(&table, corner), None);
    }
}

#[test]
fn a_deep_klondike_column_closes_up_to_fit_the_opening_window() {
    let variant = Variant::Klondike(Draw::One);
    let deck = variant.deck();
    let shape = variant.shape();
    let column = shape.tableau.nth(6).expect("the last column");
    let mut piles = alloc::vec![Vec::new(); shape.piles()];
    piles[column.index()] = deck
        .cards()
        .take(16)
        .enumerate()
        .map(|(depth, card)| Slot::new(card, depth >= 6))
        .collect();
    let table = Table::laid(Rules::of(variant), piles);
    let layout = Layout::resolve(clients(Scale::ONE)[1], variant, Scale::ONE);
    let fan = layout.fan(&table, column);
    assert!(fan.up < layout.natural.up, "a column this deep closes up");
    let bottom = layout
        .card_at(layout.place(&table, column, 15, fan))
        .bottom();
    assert!(bottom <= layout.floor);
}

#[test]
fn a_landing_follows_the_top_card() {
    let variant = Variant::FreeCell;
    let table = dealt(variant);
    let layout = Layout::resolve(clients(Scale::ONE)[1], variant, Scale::ONE);
    for column in variant.shape().tableau.iter() {
        let fan = layout.fan(&table, column);
        let top = layout.place(&table, column, table.pile(column).len() - 1, fan);
        let landing = layout.landing(&table, column);
        assert_eq!(landing.left(), top.x);
        assert_eq!(landing.top(), top.y + to_i32(fan.up));
    }
    for cell in variant.shape().cells.iter() {
        assert_eq!(
            layout.landing(&table, cell),
            layout.card_at(layout.origin(cell))
        );
    }
}

#[test]
fn a_pile_area_spans_its_place_and_its_last_card() {
    let variant = Variant::Spider(SpiderSuits::Two);
    let table = dealt(variant);
    let layout = Layout::resolve(clients(Scale::ONE)[1], variant, Scale::ONE);
    let stock = variant.shape().stock.expect("a stock");
    let area = layout.pile_area(&table, stock);
    // Five deals wait in the stock, each shown a step past the last.
    assert_eq!(area.width, layout.card_width + 4 * layout.deal_step);
    let column = variant.shape().tableau.nth(0).expect("a column");
    let fan = layout.fan(&table, column);
    let last = layout.card_at(layout.place(&table, column, table.pile(column).len() - 1, fan));
    assert_eq!(layout.pile_area(&table, column).bottom(), last.bottom());
}

#[test]
fn the_window_opens_within_the_screen_and_never_shrinks_below_its_floor() {
    for scale in scales() {
        let roomy = WindowGeometry::resolve(scale, Rect::new(0, 0, 10_000, 10_000));
        assert_eq!(roomy.width, scale.scale_length(OPEN_SIZE.0));
        assert_eq!(roomy.height, scale.scale_length(OPEN_SIZE.1));
        let small = WindowGeometry::resolve(scale, Rect::new(0, 0, 640, 480));
        assert!(small.width <= 640 && small.height <= 480);
        let WindowSizing::Resizable {
            min_width_px,
            min_height_px,
            ..
        } = small.sizing
        else {
            panic!("a resizable window");
        };
        assert!(min_width_px <= small.width && min_height_px <= small.height);
        assert_eq!(small.client(), Rect::new(0, 0, small.width, small.height));
    }
}
