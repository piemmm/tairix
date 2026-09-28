use alloc::vec::Vec;

use super::{cover, palette, Ground, GroundSite, GROUND_COUNT, ROCK_SLOPE};
use crate::biome::Biome;
use crate::blend::{Blend, Kind, WEIGHT_TOTAL};
use crate::geology::{soils, Rock, SoilSite, Soils};

fn site(celsius: f64, moisture: f64, rock: Rock) -> GroundSite {
    GroundSite {
        celsius,
        warm: celsius + 6.0,
        moisture,
        wetness: 0.3,
        slope: 0.3,
        rock,
        soils: soils(SoilSite {
            rock,
            celsius,
            moisture,
            alluvial: 0.0,
        }),
        patch: 0.5,
    }
}

#[test]
fn the_ground_table_is_in_discriminant_order() {
    for (index, ground) in Ground::ALL.iter().enumerate() {
        assert_eq!(usize::from(ground.id()), index);
    }
}

#[test]
fn every_rock_class_has_a_face_of_its_own() {
    let mut faces: Vec<Ground> = Rock::ALL.iter().map(|&r| Ground::of_rock(r)).collect();
    faces.sort_unstable();
    faces.dedup();
    assert_eq!(faces.len(), Rock::ALL.len(), "two rocks share a face");
}

#[test]
fn every_palette_carries_weight_wherever_its_biome_grows() {
    for &biome in Biome::ALL {
        for celsius in [-20.0, 0.0, 12.0, 26.0] {
            for moisture in [0.1, 1.0, 3.0] {
                for &rock in &Rock::ALL {
                    let total: f64 = palette(biome, &site(celsius, moisture, rock))
                        .iter()
                        .map(|&(_, share)| share)
                        .sum();
                    assert!(
                        total > 0.0,
                        "{biome:?} grows on nothing at {celsius} °C, {moisture}, on {rock:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn every_cover_is_normalised() {
    for &biome in Biome::ALL {
        for &rock in &Rock::ALL {
            for slope in [0.0, 1.0, ROCK_SLOPE, 10.0] {
                let mut here = site(8.0, 1.2, rock);
                here.slope = slope;
                let blend = cover(&Blend::solid(biome), &here);
                assert_eq!(blend.total(), WEIGHT_TOTAL, "{biome:?} on {rock:?}");
            }
        }
    }
}

#[test]
fn a_face_is_its_own_rock_whatever_grows_around_it() {
    for &rock in &Rock::ALL {
        let mut face = site(10.0, 1.2, rock);
        face.slope = ROCK_SLOPE;
        let blend = cover(&Blend::solid(Biome::TemperateBroadleafForest), &face);
        assert_eq!(blend.dominant(), Ground::of_rock(rock));
        // Below the face, scree gathers and the forest floor shows through.
        face.slope = ROCK_SLOPE * 0.5;
        let foot = cover(&Blend::solid(Biome::TemperateBroadleafForest), &face);
        assert!(foot.weight_of(Ground::Scree) > 0);
    }
}

#[test]
fn a_beach_is_the_colour_of_the_rock_it_was_ground_from() {
    let beach = |celsius: f64, rock: Rock| {
        let blend = cover(&Blend::solid(Biome::BeachDune), &site(celsius, 1.0, rock));
        blend.dominant()
    };
    assert_eq!(beach(22.0, Rock::Limestone), Ground::WhiteSand);
    assert_eq!(beach(22.0, Rock::Chalk), Ground::WhiteSand);
    assert_eq!(beach(22.0, Rock::Sandstone), Ground::RedSand);
    assert_eq!(beach(12.0, Rock::Sandstone), Ground::GoldenSand);
    assert_eq!(beach(22.0, Rock::Shale), Ground::GoldenSand);
    // Cold shores and hard rock are shingle.
    assert_eq!(beach(-2.0, Rock::Shale), Ground::Shingle);
    assert_eq!(beach(22.0, Rock::Basalt), Ground::Shingle);
}

#[test]
fn the_ground_set_is_complete() {
    // Every ground is laid somewhere: a variant nothing could produce would
    // be art drawn for nobody.
    let mut seen = [false; GROUND_COUNT];
    let soil_mixes = [
        Soils::default(),
        Soils {
            alluvium: 1.0,
            ..Soils::default()
        },
        Soils {
            laterite: 1.0,
            ..Soils::default()
        },
        Soils {
            podzol: 1.0,
            ..Soils::default()
        },
        Soils {
            chernozem: 1.0,
            ..Soils::default()
        },
        Soils {
            brown_earth: 1.0,
            ..Soils::default()
        },
        Soils {
            desert_crust: 1.0,
            loess: 0.0,
            ..Soils::default()
        },
    ];
    for &biome in Biome::ALL {
        for celsius in [-12.0, 3.0, 12.0, 24.0] {
            for warm in [-1.0, 4.0, 9.0, 20.0] {
                for wetness in [0.0, 0.9] {
                    for patch in [0.0, 1.0] {
                        for slope in [0.2, ROCK_SLOPE * 0.5, ROCK_SLOPE] {
                            for &rock in &Rock::ALL {
                                for soils in soil_mixes {
                                    let here = GroundSite {
                                        celsius,
                                        warm,
                                        moisture: 1.0,
                                        wetness,
                                        slope,
                                        rock,
                                        soils,
                                        patch,
                                    };
                                    for (ground, _) in cover(&Blend::solid(biome), &here).slots() {
                                        seen[usize::from(ground.id())] = true;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    seen[usize::from(Ground::Water.id())] = true;
    let missing: Vec<Ground> = Ground::ALL
        .iter()
        .copied()
        .filter(|g| !seen[usize::from(g.id())])
        .collect();
    assert!(missing.is_empty(), "never laid: {missing:?}");
}

#[test]
fn a_blend_of_biomes_weighs_each_ones_ground() {
    // Half forest, half heath: both floors show, in proportion.
    let raw = {
        let mut raw = [0.0; crate::biome::BIOME_COUNT];
        raw[Biome::BorealForest as usize] = 1.0;
        raw[Biome::HeathMoor as usize] = 1.0;
        raw
    };
    let biomes = Blend::normalise(&raw, Biome::BorealForest);
    let blend = cover(&biomes, &site(4.0, 1.5, Rock::Granite));
    assert!(blend.weight_of(Ground::NeedleLitter) > 0);
    assert!(blend.weight_of(Ground::Heath) > 0);
}

#[test]
fn cover_is_a_pure_function() {
    let here = site(8.0, 1.0, Rock::Sandstone);
    let biomes = Blend::solid(Biome::TemperateGrassland);
    assert_eq!(cover(&biomes, &here), cover(&biomes, &here));
}
