//! Host tests of the water's edge's plants: each patch stands within its
//! square and its plants' reach, made in its own materials, floating flat,
//! streaming or rising as its plant does, and drawn from its seed alone.

extern crate std;

use alloc::vec::Vec;

use super::*;
use crate::prototype::{point, Prototype};

const MARSH: Marsh = Marsh {
    stems: 1,
    leaves: 2,
    sepals: 5,
    heads: 3,
    hearts: 4,
};

const MARGINS: [Margin; 5] = [
    Margin::Reed,
    Margin::Reedmace,
    Margin::Lily,
    Margin::Pondweed,
    Margin::Crowfoot,
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
        // A flower floats beside its pad, shedding its petals about it, and
        // a clump's stalks run down beneath the water.
        Margin::Lily => (0.42, (-0.31, 0.14)),
        Margin::Pondweed => (0.15, (0.0, 0.009)),
        // Its stems stream a metre and more down the current.
        Margin::Crowfoot => (1.3, (-0.07, 0.02)),
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
                    parts <= room(margin, count, FINE).0,
                    "{margin:?} {season:?} {count}: {parts}"
                );
                assert!(
                    parts >= usize::from(count),
                    "{margin:?}: every plant has a part"
                );
            }
        }
    }
    // Every crowfoot plant is made alike and one in eight flowers once, so a
    // patch of a whole number of eights in flower takes all the room.
    let crowfoot = grown(Margin::Crowfoot, (0.75, 64), Season::Summer, 2);
    assert_eq!(crowfoot.parts().len(), room(Margin::Crowfoot, 64, FINE).0);
    // A bed of lilies in flower stays within the room it takes, drawn
    // plainly as it is beyond the near clumps.
    for seed in 0..4 {
        let bed = grown(Margin::Lily, (3.75, 120), Season::Summer, seed);
        assert!(bed.parts().len() <= room(Margin::Lily, 120, COARSE).0);
    }
}

#[test]
fn a_patch_is_made_in_its_marshs_materials_alone() {
    let made = |margin, season| -> Vec<u16> {
        let mut materials: Vec<u16> = grown(margin, (3.75, 200), season, 4)
            .parts()
            .iter()
            .filter_map(|part| match part {
                Part::Tube(tube) => Some(tube.material),
                Part::Leaf(blade) => Some(blade.material),
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
        [2, 3, 4, 5],
        "pads, petals, hearts and sepals"
    );
    assert_eq!(
        made(Margin::Lily, Season::Autumn { fallen: 80 }),
        [2],
        "only its pads"
    );
    assert_eq!(made(Margin::Pondweed, Season::Summer), [2]);
    assert_eq!(
        made(Margin::Crowfoot, Season::Summer),
        [1, 2, 3, 4],
        "stems, threads, petals and hearts"
    );
    assert_eq!(
        made(Margin::Crowfoot, Season::Autumn { fallen: 80 }),
        [1, 2],
        "only its stems and threads"
    );
}

/// A crowfoot's stems stream down the current, its patch's `z`, beneath the
/// water, and its flowers stand on it, each its broad petals about a head of
/// carpels ringed by stamens.
#[test]
fn crowfoot_streams_down_the_current_beneath_the_surface() {
    let patch = grown(Margin::Crowfoot, (0.75, 24), Season::Summer, 3);
    let mut stems = 0;
    for part in patch.parts() {
        match part {
            Part::Tube(tube) if tube.material == MARSH.stems => {
                let (a, b) = (point(tube.a), point(tube.b));
                assert!(b.z > a.z, "streams down the current: {a:?} {b:?}");
                assert!(a.y < 0.0 && b.y < 0.0, "beneath the surface");
                stems += 1;
            }
            Part::Facet(facet) => {
                for &corner in &facet.corners {
                    let corner = patch.vertex(corner).expect("a corner");
                    assert!(corner.y > 0.0, "on the water: {corner:?}");
                }
            }
            _ => {}
        }
    }
    assert_eq!(stems, 24 * usize::from(STREAMERS * STREAMER_PIECES));
    let petals = keyed(&patch, Trim::Sheet(Sheet::Broad)).len();
    let flowers = petals / wide(BLOSSOM_PETALS);
    assert!(
        flowers > 0 && petals.is_multiple_of(wide(BLOSSOM_PETALS)),
        "{petals}"
    );
    assert_eq!(
        keyed(&patch, Trim::Sheet(Sheet::Stamen)).len(),
        flowers * wide(BLOSSOM_STAMENS)
    );
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

/// The pads of a lily patch: each one's key, and the heights of its facets'
/// corners.
fn pads(patch: &Prototype) -> Vec<(u32, Vec<f64>)> {
    let mut pads: Vec<(u32, Vec<f64>)> = Vec::new();
    for part in patch.parts() {
        let Part::Facet(facet) = part else {
            continue;
        };
        if facet.trim != Some(Trim::Pad) {
            continue;
        }
        let heights = facet
            .corners
            .iter()
            .filter_map(|&corner| patch.vertex(corner))
            .map(|corner| corner.y);
        match pads.iter_mut().find(|(key, _)| *key == facet.key) {
            Some((_, held)) => held.extend(heights),
            None => pads.push((facet.key, heights.collect())),
        }
    }
    pads
}

#[test]
fn lily_pads_float_on_the_water_each_its_own_and_some_rolled_or_raised() {
    let patch = grown(Margin::Lily, (3.75, 120), Season::Summer, 5);
    let pads = pads(&patch);
    assert_eq!(pads.len(), 120, "a pad to each plant, each keyed its own");
    let mut floating = 0;
    for (key, heights) in &pads {
        let lowest = heights.iter().copied().fold(f64::INFINITY, f64::min);
        let highest = heights.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        assert!(
            lowest > -0.03 && highest < 0.13,
            "{key:#x}: {lowest}..{highest}"
        );
        // Floating: its stalk's end and most of its blade within a pad's
        // thickness or two of the water, rims and the pads it rests on
        // aside.
        let mut sorted = heights.clone();
        sorted.sort_by(f64::total_cmp);
        let middling = sorted.get(sorted.len() / 2).copied().unwrap_or(1.0);
        floating += usize::from(middling < 0.012);
    }
    assert!(floating > 90, "{floating} of 120 floating");
    assert!(
        floating < 120,
        "crowded or young pads stand up off the water"
    );
    // Pondweed's leaves lie flat on the water.
    for part in grown(Margin::Pondweed, (3.75, 60), Season::Summer, 5).parts() {
        let Part::Leaf(blade) = part else {
            panic!("pondweed is all leaves");
        };
        assert!(point(blade.normal).y > 0.9999);
        assert!((0.0019..0.0061).contains(&f64::from(blade.base[1])));
    }
}

/// The distinct keys of the facets of `patch` cut to `trim`.
fn keyed(patch: &Prototype, trim: Trim) -> Vec<u32> {
    let mut keys: Vec<u32> = patch
        .parts()
        .iter()
        .filter_map(|part| match part {
            Part::Facet(facet) if facet.trim == Some(trim) => Some(facet.key),
            _ => None,
        })
        .collect();
    keys.sort_unstable();
    keys.dedup();
    keys
}

#[test]
fn a_lily_flowers_one_in_eight_in_summer_and_a_reed_plumes_as_its_season_has_it() {
    let flowers = |season| {
        let sepals = keyed(
            &grown(Margin::Lily, (3.75, 80), season, 6),
            Trim::Sheet(Sheet::Sepal),
        );
        assert_eq!(sepals.len() % 4, 0, "four sepals to a flower");
        sepals.len() / 4
    };
    assert_eq!(flowers(Season::Summer), 10);
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
    // Every plume tufted alike in a bed beyond the clumps.
    let every = 200 * wide(COARSE.plume.0 * COARSE.plume.1);
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
fn a_reed_ends_in_its_plume_or_a_spear_and_a_spike_swells_between_blunt_ends() {
    let reeds = grown(Margin::Reed, (0.75, 30), Season::Spring, 4);
    let spears = reeds
        .parts()
        .iter()
        .filter(|part| {
            matches!(part, Part::Tube(tube) if tube.material == MARSH.leaves && tube.radii[1] == 0.0)
        })
        .count();
    let mut plumes: Vec<u32> = reeds
        .parts()
        .iter()
        .filter_map(|part| match part {
            Part::Tube(tube) if tube.material == MARSH.heads => Some(tube.key),
            _ => None,
        })
        .collect();
    plumes.sort_unstable();
    plumes.dedup();
    assert_eq!(
        spears + plumes.len(),
        30,
        "{spears} spears, {} plumes",
        plumes.len()
    );
    assert!(spears > 0 && !plumes.is_empty());
    let mace = grown(Margin::Reedmace, (0.75, 40), Season::Summer, 4);
    let mut spikes: Vec<(u32, Vec<[f32; 2]>)> = Vec::new();
    for part in mace.parts() {
        let Part::Tube(tube) = part else {
            continue;
        };
        if tube.material != MARSH.heads {
            continue;
        }
        match spikes.iter_mut().find(|(key, _)| *key == tube.key) {
            Some((_, radii)) => radii.push(tube.radii),
            None => spikes.push((tube.key, alloc::vec![tube.radii])),
        }
    }
    assert!(spikes.len() > 10, "{}", spikes.len());
    for (key, radii) in spikes {
        assert_eq!(radii.len(), SPIKE, "{key:#x}");
        let fullest = radii.iter().flatten().copied().fold(0.0_f32, f32::max);
        let (Some(first), Some(last)) = (radii.first(), radii.last()) else {
            continue;
        };
        assert!(
            first[0] < 0.8 * fullest && last[1] < 0.8 * fullest,
            "{radii:?}"
        );
        assert!(
            first[0] > 0.5 * fullest && last[1] > 0.5 * fullest,
            "blunt: {radii:?}"
        );
    }
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
    // A lily's pads are as broad as its stature, each as its own size.
    let broad = |stature| {
        let patch = grown_at(
            Margin::Lily,
            (3.75, 60, stature),
            Season::Autumn { fallen: 80 },
            3,
        );
        let mut sizes: Vec<(u32, u32)> = patch
            .parts()
            .iter()
            .filter_map(|part| match part {
                Part::Facet(facet) if facet.trim == Some(Trim::Pad) => {
                    Some((facet.key, facet.size.to_bits()))
                }
                _ => None,
            })
            .collect();
        sizes.sort_unstable();
        sizes.dedup();
        assert_eq!(sizes.len(), 60);
        sizes
            .iter()
            .map(|&(_, size)| f64::from(f32::from_bits(size)))
            .sum::<f64>()
    };
    let full = broad(1.0);
    for stature in [0.55, 1.1] {
        assert!((broad(stature) / full - stature).abs() < 1e-3, "{stature}");
    }
}

#[test]
fn a_summer_beds_flowers_are_at_every_stage_from_bud_to_spent() {
    let patch = grown(Margin::Lily, (3.75, 400), Season::Summer, 8);
    let flowers = keyed(&patch, Trim::Sheet(Sheet::Sepal)).len() / 4;
    // Every flower but a bud shows its stigma, the one mesh cut to no
    // outline in its heart's material.
    let mut stigmas: Vec<u32> = patch
        .parts()
        .iter()
        .filter_map(|part| match part {
            Part::Facet(facet) if facet.trim.is_none() && facet.material == Some(MARSH.hearts) => {
                Some(facet.key)
            }
            _ => None,
        })
        .collect();
    stigmas.sort_unstable();
    stigmas.dedup();
    let buds = flowers - stigmas.len();
    assert_eq!(flowers, 50);
    assert!((4..20).contains(&buds), "{buds} buds of {flowers}");
    // Its petals as old as their flowers: fresh, fading and dead.
    let ages: Vec<f64> = keyed(&patch, Trim::Sheet(Sheet::Petal))
        .into_iter()
        .map(crate::lily::age)
        .collect();
    let share = |range: core::ops::Range<f64>| {
        ages.iter().filter(|age| range.contains(age)).count() * 100 / ages.len().max(1)
    };
    assert!(share(0.0..0.3) > 15 && share(0.55..0.9) > 10 && share(0.9..1.01) > 3);
}

#[test]
fn a_lilys_pads_age_with_the_season() {
    let mean = |season| {
        let pads = pads(&grown(Margin::Lily, (3.75, 200), season, 9));
        pads.iter()
            .map(|&(key, _)| crate::lily::age(key))
            .sum::<f64>()
            / 200.0
    };
    let (spring, summer, autumn) = (
        mean(Season::Spring),
        mean(Season::Summer),
        mean(Season::Autumn { fallen: 80 }),
    );
    assert!(
        spring < summer && summer < autumn,
        "{spring} {summer} {autumn}"
    );
    assert!(spring < 0.2 && autumn > 0.7, "{spring} {autumn}");
}

#[test]
fn a_pad_rests_on_those_floating_beneath_it() {
    let lie = Lie {
        float: 0.002,
        raised: 0.0,
        cup: 0.0,
        rim: (0.0, 1.0, 0.0),
        waves: (0.0, 4.0, 0.0),
        ripples: (0.0, 9.0, 0.0),
        overlap: 0.0,
        sink: (0.0, 0.0),
        roll: 0.0,
    };
    let laid = |centre: (f64, f64), turn: f64, key: u32| Laid {
        centre,
        turn,
        radius: 0.1,
        lie,
        pad: crate::lily::Pad::of(key),
        key,
    };
    // The second's sinus turned away from the first.
    let (under, over) = (laid((0.0, 0.0), 0.0, 11), laid((0.12, 0.0), FRAC_PI_2, 12));
    // Where both reach, the second lies on the first; where neither does,
    // the water is open.
    let both = (0.06, 0.0);
    let first = resting(&[under], both).expect("over the first");
    assert!((first - 0.002).abs() < 1e-9);
    let top = resting(&[under, over], both).expect("over both");
    assert!((top - (first + REST)).abs() < 1e-9, "{top}");
    assert!(resting(&[under, over], (0.5, 0.5)).is_none());
    // The second's own mesh lies so where it reaches over the first, and on
    // the water where it does not.
    let toward = FRAC_PI_2 / over.fold(0.6);
    let lying = over.point((0.6, toward), &[under]);
    assert!(
        (lying.x - 0.06).abs() < 1e-9 && lying.z.abs() < 1e-9,
        "{lying:?}"
    );
    assert!((lying.y - top).abs() < 1e-9, "{lying:?}");
    let away = over.point((0.6, -toward), &[under]);
    assert!((away.y - 0.002).abs() < 1e-9, "{away:?}");
    // Lobes that only meet leave no water between them; lobes held apart
    // leave their sinus open.
    let slit = (-0.05, 0.001);
    assert!(under.height_at(slit).is_some());
    let open = Laid {
        lie: Lie {
            overlap: -0.14,
            ..lie
        },
        ..under
    };
    assert!(open.height_at(slit).is_none(), "its sinus is open water");
    // Lobes carried past each other lie one over the other, the upper lifted
    // clear of the lower.
    let lapped = Laid {
        lie: Lie {
            overlap: 0.25,
            ..lie
        },
        ..under
    };
    let (upper, lower) = (
        lapped.place((0.9, PI - SINUS)),
        lapped.place((0.9, -(PI - SINUS))),
    );
    assert!(
        upper.1 < 0.0 && lower.1 > 0.0,
        "carried past: {upper:?} {lower:?}"
    );
    assert!(
        lapped.height((0.9, PI - SINUS)) > lapped.height((0.9, -(PI - SINUS))) + 0.002,
        "the upper lobe over the lower"
    );
}

#[test]
fn a_patch_is_drawn_from_its_seed_alone() {
    for margin in [Margin::Reedmace, Margin::Lily] {
        let parts = |seed| {
            alloc::format!(
                "{:?}",
                grown(margin, (0.75, 12), Season::Summer, seed).parts()
            )
        };
        assert_eq!(parts(9), parts(9), "{margin:?}");
        assert_ne!(parts(9), parts(10), "{margin:?}");
    }
}
