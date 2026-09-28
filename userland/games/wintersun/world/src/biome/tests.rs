use super::{
    classify, climate, terrain, Biome, Conditions, Water, BIOME_COUNT, SHORE_REACH,
    SNOWLINE_CELSIUS, TREELINE_CELSIUS,
};
use crate::blend::{Kind, WEIGHT_TOTAL};
use crate::geology::{Lithology, Rock};

/// A temperate, humid, oceanic lowland on shale: broadleaf country.
fn lowland() -> Conditions {
    Conditions {
        celsius: 10.0,
        range: 12.0,
        continentality: 0.1,
        precipitation: 900.0,
        rain_season: 0.0,
        wetness: 0.2,
        elevation_units: 80.0,
        slope: 0.3,
        rift: 0.0,
        lithology: Lithology {
            rock: Rock::Shale,
            volcanism: 0.0,
        },
        shore: (u16::MAX, Water::Running),
    }
}

/// The raw partition, before normalisation: what totality is a claim about.
fn partition(site: &Conditions) -> [f64; BIOME_COUNT] {
    let mut raw = [0.0; BIOME_COUNT];
    climate(site, &mut raw);
    terrain(site, &mut raw);
    raw
}

#[test]
fn the_biome_table_is_in_discriminant_order() {
    for (index, biome) in Biome::ALL.iter().enumerate() {
        assert_eq!(usize::from(biome.id()), index);
    }
}

#[test]
fn the_classification_is_total_over_every_legal_point() {
    // Temperature, precipitation, seasonality and wetness swept across and
    // past everything a realm produces, with the terrain inputs at both ends
    // of theirs: the partition sums to one everywhere, so no climate falls
    // through to the bare kind, and every blend is normalised.
    let rocks = [Rock::Granite, Rock::Shale, Rock::Limestone, Rock::Basalt];
    let mut points = 0_u32;
    for celsius in (-45..=35).step_by(4) {
        for range in [0.0, 6.0, 18.0, 45.0] {
            for precipitation in [0.0, 60.0, 250.0, 700.0, 1800.0, 6000.0] {
                for rain_season in [-1.0, -0.4, 0.0, 0.4, 1.0] {
                    for wetness in [0.0, 0.7, 1.0] {
                        for (index, &rock) in rocks.iter().enumerate() {
                            let flip = index % 2 == 0;
                            let site = Conditions {
                                celsius: f64::from(celsius),
                                range,
                                continentality: if flip { 0.0 } else { 1.0 },
                                precipitation,
                                rain_season,
                                wetness,
                                elevation_units: if flip { 10.0 } else { 2500.0 },
                                slope: if flip { 0.2 } else { 1.8 },
                                rift: if flip { 0.0 } else { 0.4 },
                                lithology: Lithology {
                                    rock,
                                    volcanism: if index == 3 { 0.6 } else { 0.0 },
                                },
                                shore: match index {
                                    0 => (1, Water::Sea),
                                    1 => (2, Water::Lake),
                                    2 => (1, Water::Running),
                                    _ => (u16::MAX, Water::Running),
                                },
                            };
                            let raw = partition(&site);
                            let sum: f64 = raw.iter().sum();
                            assert!((sum - 1.0).abs() < 1.0e-9, "{sum} at {site:?}");
                            assert!(raw.iter().all(|&w| w >= -1.0e-12), "{site:?}");
                            assert_eq!(classify(&site).total(), WEIGHT_TOTAL);
                            points += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(points > 10_000);
}

/// The dominant biome of a site that differs from [`lowland`] as `edit`
/// says.
fn dominant(edit: impl FnOnce(&mut Conditions)) -> Biome {
    let mut site = lowland();
    edit(&mut site);
    classify(&site).dominant()
}

#[test]
fn each_tropical_climate_grows_its_own_biome() {
    // By how much rain falls and when.
    let tropics = |c: &mut Conditions| {
        c.celsius = 25.5;
        c.range = 2.0;
    };
    assert_eq!(
        dominant(|c| {
            tropics(c);
            c.precipitation = 3200.0;
        }),
        Biome::TropicalRainforest
    );
    assert_eq!(
        dominant(|c| {
            tropics(c);
            c.precipitation = 1100.0;
            c.rain_season = 0.8;
        }),
        Biome::Savanna
    );
    // Rain split sixty to forty between the seasons is no dry season: that
    // is rainforest. A dry forest needs most of its rain in one.
    assert_eq!(
        dominant(|c| {
            tropics(c);
            c.precipitation = 1500.0;
            c.rain_season = 0.2;
        }),
        Biome::TropicalRainforest
    );
    assert_eq!(
        dominant(|c| {
            tropics(c);
            c.precipitation = 1500.0;
            c.rain_season = 0.6;
        }),
        Biome::TropicalDryForest
    );
    assert_eq!(
        dominant(|c| {
            tropics(c);
            c.precipitation = 350.0;
            c.rain_season = 0.5;
        }),
        Biome::XericShrubland
    );
    assert_eq!(
        dominant(|c| {
            tropics(c);
            c.precipitation = 60.0;
        }),
        Biome::HotDesert
    );
}

#[test]
fn each_temperate_climate_grows_its_own_biome() {
    assert_eq!(dominant(|_| ()), Biome::TemperateBroadleafForest);
    assert_eq!(
        dominant(|c| {
            c.celsius = 16.0;
            c.precipitation = 600.0;
            c.rain_season = -0.8;
        }),
        Biome::MediterraneanWoodland
    );
    assert_eq!(
        dominant(|c| {
            c.celsius = 7.0;
            c.range = 30.0;
            c.continentality = 0.9;
            c.precipitation = 380.0;
        }),
        Biome::TemperateGrassland
    );
    assert_eq!(
        dominant(|c| {
            c.celsius = 6.0;
            c.range = 30.0;
            c.continentality = 0.9;
            c.precipitation = 90.0;
        }),
        Biome::ColdDesert
    );
    assert_eq!(
        dominant(|c| {
            c.celsius = 10.0;
            c.range = 11.0;
            c.continentality = 0.05;
            c.precipitation = 3000.0;
        }),
        Biome::TemperateRainforest
    );
    assert_eq!(
        dominant(|c| {
            c.celsius = 5.0;
            c.range = 10.0;
            c.continentality = 0.05;
            c.precipitation = 1400.0;
            c.lithology.rock = Rock::Granite;
        }),
        Biome::HeathMoor
    );
    assert_eq!(
        dominant(|c| {
            c.celsius = 6.0;
            c.range = 22.0;
            c.continentality = 0.8;
            c.precipitation = 800.0;
            c.lithology.rock = Rock::Granite;
        }),
        Biome::TemperateConiferForest
    );
}

#[test]
fn each_cold_climate_grows_its_own_biome() {
    assert_eq!(
        dominant(|c| {
            c.celsius = -3.0;
            c.range = 32.0;
            c.continentality = 0.8;
            c.precipitation = 500.0;
        }),
        Biome::BorealForest
    );
    assert_eq!(
        dominant(|c| {
            c.celsius = -9.0;
            c.range = 28.0;
            c.continentality = 0.8;
            c.precipitation = 300.0;
        }),
        Biome::Tundra
    );
    assert_eq!(
        dominant(|c| {
            c.celsius = -18.0;
            c.range = 24.0;
            c.precipitation = 300.0;
        }),
        Biome::IceSheet
    );
    assert_eq!(
        dominant(|c| {
            c.celsius = -14.0;
            c.range = 30.0;
            c.continentality = 0.9;
            c.precipitation = 60.0;
        }),
        Biome::PolarDesert
    );
}

#[test]
fn a_range_climbs_from_forest_through_meadow_to_ice() {
    // One place at sea level and higher, cooled by the lapse rate: the
    // treeline and the snowline are warm-season isotherms, so where they
    // fall is the latitude's to decide.
    let at = |metres: f64| {
        let mut site = lowland();
        site.celsius = 14.0 - metres * 0.0065;
        site.elevation_units = metres;
        site.continentality = 0.8;
        site
    };
    assert!(matches!(
        classify(&at(0.0)).dominant(),
        Biome::TemperateBroadleafForest | Biome::TemperateConiferForest
    ));
    let treeline = at(1900.0);
    assert!(treeline.warm() < TREELINE_CELSIUS);
    assert_eq!(classify(&treeline).dominant(), Biome::AlpineTundra);
    let summit = at(3400.0);
    assert!(summit.warm() < SNOWLINE_CELSIUS);
    assert_eq!(classify(&summit).dominant(), Biome::IceSheet);
}

#[test]
fn a_lowland_beyond_the_treeline_is_tundra_and_a_mountain_alpine() {
    let arctic = dominant(|c| {
        c.celsius = -8.0;
        c.range = 28.0;
        c.continentality = 0.9;
        c.precipitation = 320.0;
        c.elevation_units = 20.0;
    });
    assert_eq!(arctic, Biome::Tundra);
    let alpine = dominant(|c| {
        c.celsius = 1.0;
        c.range = 14.0;
        c.continentality = 0.9;
        c.precipitation = 600.0;
        c.elevation_units = 1800.0;
    });
    assert_eq!(alpine, Biome::AlpineTundra);
}

#[test]
fn a_poorly_drained_flat_is_a_wetland_by_its_warmth_and_its_rock() {
    let wet = |celsius: f64, wetness: f64, rock: Rock| {
        dominant(|c| {
            c.celsius = celsius;
            c.wetness = wetness;
            c.precipitation = 1400.0;
            c.lithology.rock = rock;
        })
    };
    assert_eq!(wet(24.0, 0.97, Rock::Shale), Biome::SwampForest);
    assert_eq!(wet(12.0, 0.97, Rock::Shale), Biome::Marsh);
    // Peat high in a catchment is fed only by the rain, and on acid rock is
    // a bog; on lime, or deep in the catchment where ground water rises
    // into it, it is a fen.
    assert_eq!(wet(2.0, 0.78, Rock::Granite), Biome::Bog);
    assert_eq!(wet(2.0, 0.78, Rock::Limestone), Biome::Fen);
    assert_eq!(wet(2.0, 0.99, Rock::Granite), Biome::Fen);
    // A dry flat is not a wetland however flat it is.
    let playa = dominant(|c| {
        c.celsius = 22.0;
        c.wetness = 0.97;
        c.precipitation = 60.0;
    });
    assert_eq!(playa, Biome::HotDesert);
}

#[test]
fn a_shore_is_its_coast() {
    let shore = |edit: &dyn Fn(&mut Conditions)| {
        dominant(|c| {
            c.shore = (1, Water::Sea);
            edit(c);
        })
    };
    assert_eq!(shore(&|_| ()), Biome::BeachDune);
    assert_eq!(shore(&|c| c.slope = 4.0), Biome::RockyCoast);
    assert_eq!(
        shore(&|c| {
            c.celsius = 26.0;
            c.range = 2.0;
            c.precipitation = 2600.0;
        }),
        Biome::Mangrove
    );
    assert_eq!(shore(&|c| c.wetness = 0.9), Biome::Marsh);
    // Mangrove is a tidal tree: a lake shore in the tropics is a beach.
    let lake = dominant(|c| {
        c.shore = (1, Water::Lake);
        c.celsius = 26.0;
        c.range = 2.0;
        c.precipitation = 2600.0;
    });
    assert_ne!(lake, Biome::Mangrove);
    // Past the reach a caller resolves, a shore is no coast at all.
    let inland = dominant(|c| c.shore = (SHORE_REACH, Water::Sea));
    assert_eq!(inland, Biome::TemperateBroadleafForest);
    // A river's bank is no coast: the land beside it is what the climate
    // grows.
    let bank = dominant(|c| c.shore = (1, Water::Running));
    assert_eq!(bank, Biome::TemperateBroadleafForest);
}

#[test]
fn fresh_lava_torn_ground_and_gullied_clay_take_their_ground() {
    assert_eq!(
        dominant(|c| c.lithology = Lithology {
            rock: Rock::Basalt,
            volcanism: 1.0
        }),
        Biome::VolcanicBarren
    );
    assert_eq!(
        dominant(|c| {
            c.rift = 0.3;
            c.elevation_units = 20.0;
        }),
        Biome::RiftWaste
    );
    assert_eq!(
        dominant(|c| {
            c.celsius = 18.0;
            c.precipitation = 180.0;
            c.slope = 1.0;
        }),
        Biome::Badlands
    );
    // Hard rock does not gully, and a humid climate heals what does.
    assert_ne!(
        dominant(|c| {
            c.celsius = 18.0;
            c.precipitation = 180.0;
            c.slope = 1.0;
            c.lithology.rock = Rock::Granite;
        }),
        Biome::Badlands
    );
    assert_ne!(dominant(|c| c.slope = 1.0), Biome::Badlands);
}

#[test]
fn effective_moisture_is_read_against_the_aridity_threshold() {
    // At 20 °C with rain evenly spread, the threshold is 540 mm: half of it
    // is the desert's edge.
    let mut site = lowland();
    site.celsius = 20.0;
    site.precipitation = 270.0;
    assert!((site.moisture() - 0.5).abs() < 1.0e-9);
    // Summer rain evaporates faster, so the same rain is drier.
    site.rain_season = 1.0;
    assert!(site.moisture() < 0.5);
    // A polar place is not humid on no rain at all.
    site.celsius = -30.0;
    site.rain_season = 0.0;
    site.precipitation = 0.0;
    assert!(site.moisture().abs() < 1.0e-12, "{}", site.moisture());
}

#[test]
fn classification_is_a_pure_function() {
    let site = lowland();
    assert_eq!(classify(&site), classify(&site));
}
