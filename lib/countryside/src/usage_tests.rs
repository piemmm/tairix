use super::*;
use crate::holding::HoldingId;
use crate::plane::Convex;

const KEY: Key = Key::new(55);

fn field(index: u32, (slope, fall, wet): (f64, Point, f64)) -> Field {
    Field {
        id: crate::field::FieldId {
            holding: HoldingId::new(2, 3),
            index,
        },
        cell: Convex::default(),
        area: 30_000.0,
        middle: Point::new(100.0, 0.0),
        along: Point::new(1.0, 0.0),
        block: 0,
        slope,
        fall,
        wet,
    }
}

const NONE: Mix = Mix {
    arable: 0.0,
    pasture: 0.0,
    meadow: 0.0,
    orchard: 0.0,
    vineyard: 0.0,
    woodlot: 0.0,
    overgrown: 0.0,
};

#[test]
fn a_region_of_one_use_puts_every_field_to_it() {
    let pasture = Mix { pasture: 1.0, ..NONE };
    let arable = Mix { arable: 1.0, ..NONE };
    for index in 0..200 {
        let level = field(index, (0.01, Point::new(0.0, 1.0), 0.1));
        assert_eq!(usage(KEY, &pasture, &level, (None, Point::new(0.0, -1.0))).used, Use::Pasture);
        assert!(matches!(
            usage(KEY, &arable, &level, (Some(Point::new(0.0, 0.0)), Point::new(0.0, -1.0))).used,
            Use::Arable(_)
        ));
    }
}

#[test]
fn vines_grow_only_on_a_slope_facing_the_sun() {
    let mix = Mix {
        vineyard: 1.0,
        pasture: 0.05,
        ..NONE
    };
    let warm = Point::new(0.0, -1.0);
    let (mut sunny, mut shaded) = (0, 0);
    for index in 0..300 {
        let toward = field(index, (0.15, warm, 0.0));
        let away = field(index, (0.15, -warm, 0.0));
        sunny += usize::from(usage(KEY, &mix, &toward, (None, warm)).used == Use::Vineyard);
        shaded += usize::from(usage(KEY, &mix, &away, (None, warm)).used == Use::Vineyard);
    }
    assert_eq!(shaded, 0);
    assert!(sunny > 250, "{sunny}");
}

#[test]
fn only_a_field_cut_for_hay_or_straw_is_baled() {
    let mix = Mix {
        arable: 1.0,
        pasture: 1.0,
        meadow: 1.0,
        orchard: 1.0,
        vineyard: 1.0,
        woodlot: 1.0,
        overgrown: 1.0,
    };
    for index in 0..400 {
        let parcel = field(index, (0.06, Point::new(0.0, -1.0), 0.3));
        let used = usage(KEY, &mix, &parcel, (Some(Point::new(150.0, 0.0)), Point::new(0.0, -1.0)));
        let cut = matches!(
            used.used,
            Use::Meadow | Use::Arable(Crop::Wheat | Crop::Barley | Crop::Oats | Crop::Ley)
        );
        assert_eq!(used.bale.is_some(), cut, "{used:?}");
    }
}
