//! Host tests of the water's edge's plants: each patch stands within its
//! square and its plants' reach, made in its own materials, floating flat
//! or rising as its plant does, and drawn from its seed alone.

extern crate std;

use alloc::vec::Vec;

use super::*;
use crate::prototype::{point, Prototype};

const MARSH: Marsh = Marsh {
    stems: 1,
    leaves: 2,
    heads: 3,
    hearts: 4,
};

const MARGINS: [Margin; 4] = [
    Margin::Reed,
    Margin::Reedmace,
    Margin::Lily,
    Margin::Pondweed,
];

const SEASONS: [Season; 4] = [
    Season::Spring,
    Season::Summer,
    Season::Autumn { fallen: 80 },
    Season::Winter,
];

fn grown(margin: Margin, (side, count): (f64, u16), season: Season, seed: u64) -> Prototype {
    grown_at(margin, (side, count, 1.0), season, seed)
}

fn grown_at(
    margin: Margin,
    (side, count, stature): (f64, u16, f64),
    season: Season,
    seed: u64,
) -> Prototype {
    patch(margin, (side, count, stature), MARSH, season, seed)
        .expect("a patch")
        .whole()
}

/// How far beyond its square a plant of `margin` reaches, and how high it
/// stands at most and lies at least.
fn reach(margin: Margin) -> (f64, (f64, f64)) {
    match margin {
        Margin::Reed => (0.85, (-0.01, 3.2)),
        Margin::Reedmace => (1.5, (-0.01, 2.5)),
        // A pad's box is the square about its circle, turned.
        Margin::Lily => (0.23, (-0.006, 0.1)),
        Margin::Pondweed => (0.15, (0.0, 0.009)),
    }
}

#[test]
fn every_patch_stands_within_its_square_and_its_plants_reach() {
    for margin in MARGINS {
        let (beyond, (lowest, highest)) = reach(margin);
        for season in SEASONS {
            for (side, count) in [(0.75, 30), (3.75, 120)] {
                for seed in 0..3 {
                    let bounds = grown(margin, (side, count), season, seed).bounds();
                    let out = 0.5 * side + beyond;
                    assert!(
                        bounds.min.x > -out
                            && bounds.max.x < out
                            && bounds.min.z > -out
                            && bounds.max.z < out,
                        "{margin:?} {season:?} {side}: {bounds:?}"
                    );
                    assert!(
                        bounds.min.y > lowest && bounds.max.y < highest,
                        "{margin:?} {season:?} {side}: {bounds:?}"
                    );
                }
            }
        }
    }
    // Reeds and reedmace stand head-high; the floating plants lie on the
    // water.
    let tall = |margin| grown(margin, (0.75, 30), Season::Summer, 1).bounds().max.y;
    assert!(tall(Margin::Reed) > 1.6 && tall(Margin::Reedmace) > 1.3);
}

#[test]
fn a_patch_fits_the_room_it_takes_for_its_parts() {
    for margin in MARGINS {
        for season in SEASONS {
            for count in [1, 7, 8, 9, 64] {
                let parts = grown(margin, (0.75, count), season, 2).parts().len();
                assert!(
                    parts <= most_parts(margin, count),
                    "{margin:?} {season:?} {count}: {parts}"
                );
                assert!(
                    parts >= usize::from(count),
                    "{margin:?}: every plant has a part"
                );
            }
        }
    }
}

#[test]
fn a_patch_is_made_in_its_marshs_materials_alone() {
    let made = |margin, season| -> Vec<u16> {
        let mut materials: Vec<u16> = grown(margin, (3.75, 200), season, 4)
            .parts()
            .iter()
            .map(|part| match part {
                Part::Tube(tube) => tube.material,
                Part::Leaf(blade) => blade.material,
                Part::Facet(facet) => facet.material,
            })
            .collect();
        materials.sort_unstable();
        materials.dedup();
        materials
    };
    assert_eq!(made(Margin::Reed, Season::Summer), [1, 2, 3]);
    assert_eq!(made(Margin::Reedmace, Season::Summer), [1, 2, 3]);
    assert_eq!(
        made(Margin::Lily, Season::Summer),
        [2, 3, 4],
        "pads, petals and hearts"
    );
    assert_eq!(
        made(Margin::Lily, Season::Autumn { fallen: 80 }),
        [2],
        "only its pads"
    );
    assert_eq!(made(Margin::Pondweed, Season::Summer), [2]);
}

#[test]
fn every_part_of_a_plant_takes_the_plants_colour_but_a_key_of_its_own() {
    for key in [0u32, 1, 2, 3, 0xdead_beef, u32::MAX] {
        let keys: Vec<u32> = (0..80).map(|index| part(key, index)).collect();
        assert!(keys.iter().all(|&own| own & 3 == key & 3), "{key:#x}");
        let mut distinct = keys.clone();
        distinct.sort_unstable();
        distinct.dedup();
        assert_eq!(distinct.len(), keys.len(), "{key:#x}");
    }
}

#[test]
fn a_lily_pad_is_round_and_floats_flat_just_above_the_water() {
    let pads = grown(Margin::Lily, (3.75, 120), Season::Summer, 5);
    let mut heights = Vec::new();
    for part in pads.parts() {
        let Part::Leaf(blade) = part else {
            continue;
        };
        if blade.outline != Outline::Pad {
            continue;
        }
        assert!(
            (2.0 * blade.width - blade.length).abs() < 1e-6,
            "round: {blade:?}"
        );
        assert!(point(blade.normal).y > 0.995, "flat: {blade:?}");
        let middle = point(blade.base) + point(blade.axis) * f64::from(0.5 * blade.length);
        assert!((0.0019..0.0061).contains(&middle.y), "{}", middle.y);
        heights.push(middle.y);
    }
    assert_eq!(heights.len(), 120);
    heights.sort_by(f64::total_cmp);
    heights.dedup();
    assert!(
        heights.len() > 110,
        "pads lie apart in height: {}",
        heights.len()
    );
    for part in grown(Margin::Pondweed, (3.75, 60), Season::Summer, 5).parts() {
        let Part::Leaf(blade) = part else {
            panic!("pondweed is all leaves");
        };
        assert!(point(blade.normal).y > 0.9999);
        assert!((0.0019..0.0061).contains(&f64::from(blade.base[1])));
    }
}

#[test]
fn a_lily_flowers_one_in_eight_in_summer_and_a_reed_plumes_as_its_season_has_it() {
    let flowers = |season| {
        grown(Margin::Lily, (3.75, 80), season, 6)
            .parts()
            .iter()
            .filter(|part| matches!(part, Part::Tube(_)))
            .count()
    };
    assert_eq!(flowers(Season::Summer), 10, "a heart to each flower");
    for season in [
        Season::Spring,
        Season::Autumn { fallen: 80 },
        Season::Winter,
    ] {
        assert_eq!(flowers(season), 0, "{season:?}");
    }
    let plumes = |season, seed| {
        grown(Margin::Reed, (3.75, 200), season, seed)
            .parts()
            .iter()
            .filter(|part| {
                matches!(
                    part,
                    Part::Leaf(Blade {
                        outline: Outline::Fascicle { .. },
                        ..
                    })
                )
            })
            .count()
    };
    let every = 200 * usize::from(PLUME);
    for season in [Season::Autumn { fallen: 80 }, Season::Winter] {
        assert_eq!(plumes(season, 7), every, "{season:?}");
    }
    // Last year's on some in spring, and this year's opening on most over
    // the summer.
    let (spring, summer) = (plumes(Season::Spring, 7), plumes(Season::Summer, 7));
    assert!(
        (every / 4..every * 11 / 20).contains(&spring),
        "{spring} of {every}"
    );
    assert!(
        (every * 9 / 20..every * 3 / 4).contains(&summer),
        "{summer} of {every}"
    );
}

#[test]
fn a_patch_stands_as_tall_as_its_stature() {
    for margin in [Margin::Reed, Margin::Reedmace] {
        let tall = |stature| {
            grown_at(
                margin,
                (0.75, 30, stature),
                Season::Autumn { fallen: 80 },
                3,
            )
            .bounds()
            .max
            .y
        };
        let (short, full) = (tall(0.55), tall(1.1));
        assert!(short < 0.6 * full, "{margin:?}: {short} beside {full}");
    }
    let broad = |stature| {
        grown_at(
            Margin::Lily,
            (3.75, 60, stature),
            Season::Autumn { fallen: 80 },
            3,
        )
        .parts()
        .iter()
        .map(|part| match part {
            Part::Leaf(blade) => f64::from(blade.length),
            _ => 0.0,
        })
        .sum::<f64>()
    };
    assert!(broad(0.55) < 0.55 * broad(1.0) && broad(1.1) > 1.05 * broad(1.0));
}

#[test]
fn a_patch_is_drawn_from_its_seed_alone() {
    let parts = |seed| {
        alloc::format!(
            "{:?}",
            grown(
                Margin::Reedmace,
                (0.75, 12),
                Season::Autumn { fallen: 80 },
                seed
            )
            .parts()
        )
    };
    assert_eq!(parts(9), parts(9));
    assert_ne!(parts(9), parts(10));
}
