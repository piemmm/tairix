use super::*;
use crate::vector::Vec3;

const SEASONS: [Season; 4] = [
    Season::Spring,
    Season::Summer,
    Season::Autumn { fallen: 10 },
    Season::Winter,
];

/// Where maize standing as plants gives way to the sward in these tests.
const STOOD: (f64, f64) = (40.0, 50.0);

fn sowing(season: Season, shoots: f64) -> Sowing {
    super::sowing(season, shoots, STOOD)
}

/// Standing maize gives way to its plants about the eye; every other growth
/// is only ever a sward.
#[test]
fn only_standing_maize_gives_way_to_its_plants() {
    for season in SEASONS {
        let sowing = sowing(season, 1100.0);
        for grown in farmed::growths(season) {
            let index = usize::from(sowing.by_growth[usize::from(grown.code())]);
            let Some(Some((kind, _))) = index
                .checked_sub(WILD_KINDS)
                .and_then(|slot| sowing.kinds.get(slot))
            else {
                continue;
            };
            let standing = matches!(grown, Grown::Sown(Crop::Maize, Stage::Green | Stage::Ripe));
            assert_eq!(
                kind.stood,
                standing.then_some(STOOD),
                "{season:?} {grown:?}"
            );
        }
    }
}

/// Every growth a season holds that shows above its tilth takes a kind of
/// its own in the sward, and looks as its kind does from afar; one showing
/// nothing takes none.
#[test]
fn every_growth_a_season_holds_grows_in_the_sward() {
    for season in SEASONS {
        let sowing = sowing(season, 1100.0);
        for grown in farmed::growths(season) {
            let code = usize::from(grown.code());
            let index = usize::from(sowing.by_growth[code]);
            if look(grown).is_some() {
                assert!(index >= WILD_KINDS, "{season:?} {grown:?} has no kind");
                assert!(
                    sowing.kinds[index - WILD_KINDS].is_some(),
                    "{season:?} {grown:?}"
                );
                assert!(
                    sowing.far[index - WILD_KINDS].is_some(),
                    "{season:?} {grown:?} has no far look"
                );
            } else {
                assert_eq!(
                    index, 0,
                    "{season:?} {grown:?} shows nothing yet has a kind"
                );
                assert!(grown.tilled(), "{season:?} {grown:?}");
            }
        }
    }
}

/// A crop stands as thickly as its own shoots say, whatever the sward it is
/// sown in grows wild.
#[test]
fn a_crop_stands_its_own_shoots_in_any_sward() {
    let grown = Grown::Sown(Crop::Wheat, Stage::Ripe);
    let index =
        |sowing: &Sowing| usize::from(sowing.by_growth[usize::from(grown.code())]) - WILD_KINDS;
    let (thin, thick) = (
        sowing(Season::Summer, 900.0),
        sowing(Season::Summer, 1600.0),
    );
    let thickness = |sowing: &Sowing, shoots: f64| {
        sowing.kinds[index(sowing)].map_or(0.0, |(kind, _)| kind.thickness * shoots)
    };
    assert!((thickness(&thin, 900.0) - thickness(&thick, 1600.0)).abs() < 1e-9);
}

/// From afar a ripe cereal is gold, rape in flower yellow, and a crop in
/// leaf green.
#[test]
fn each_crop_is_its_own_colour_from_afar() {
    let far = |season: Season, grown: Grown| {
        let sowing = sowing(season, 1100.0);
        let index = usize::from(sowing.by_growth[usize::from(grown.code())]) - WILD_KINDS;
        sowing.far[index].map_or(Vec3::ZERO, |far| far.colour)
    };
    let gold = far(Season::Summer, Grown::Sown(Crop::Wheat, Stage::Ripe));
    assert!(
        gold.x > gold.y && gold.y > gold.z && gold.z > 0.0,
        "{gold:?}"
    );
    let yellow = far(
        Season::Spring,
        Grown::Sown(Crop::Rapeseed, Stage::Flowering),
    );
    assert!(
        yellow.x > 2.5 * yellow.z && yellow.y > 2.5 * yellow.z,
        "{yellow:?}"
    );
    for (season, grown) in [
        (Season::Spring, Grown::Sown(Crop::Wheat, Stage::Green)),
        (Season::Summer, Grown::Sown(Crop::Maize, Stage::Green)),
        (Season::Winter, Grown::Sown(Crop::Ley, Stage::Green)),
    ] {
        let green = far(season, grown);
        assert!(
            green.y > green.x && green.y > green.z,
            "{grown:?}: {green:?}"
        );
    }
}
