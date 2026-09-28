use alloc::vec::Vec;

use super::{
    airflows, belt_rain, prevalence, row_latitude, solve, step_of, water_distance, wind_order,
    zonal_celsius, Season, LAPSE_RATE, SEASON_SHIFT,
};
use crate::geom::Elevation;
use crate::hydrology;
use crate::params::{RealmParams, RealmSpec};
use crate::realm::{try_filled, CoarseSample};
use crate::relief;
use crate::seed::SeedKey;
use crate::uplift::Plates;
use tairix_util::mathf;
use tairix_wintersun_net::value::Facing;

fn solved(edit: impl FnOnce(&mut RealmSpec)) -> (RealmParams, Vec<CoarseSample>) {
    let mut spec = RealmSpec {
        extent_chunks: 32,
        coarse_samples: 64,
        ..RealmParams::default_realm(0x5170_2000).spec()
    };
    edit(&mut spec);
    let params = RealmParams::new(spec).expect("legal");
    let side = params.coarse_samples() as usize;
    let mut samples = try_filled(side * side, CoarseSample::default()).expect("fits");
    let key = SeedKey::new(params.seed());
    relief::solve(params, key, Plates::new(params), &mut samples).expect("solves");
    hydrology::solve(params, &mut samples).expect("solves");
    solve(params, key, &mut samples).expect("solves");
    (params, samples)
}

/// The mean of `of` over the land in the rows within `reach` of the row
/// nearest `latitude`.
fn land_mean(
    params: RealmParams,
    samples: &[CoarseSample],
    latitude: f64,
    reach: u32,
    of: impl Fn(&CoarseSample) -> f64,
) -> f64 {
    let side = params.coarse_samples();
    let off = |row: u32| mathf::fabs(row_latitude(params, row) - latitude);
    let nearest = (0..side)
        .min_by(|&a, &b| off(a).total_cmp(&off(b)))
        .expect("a realm has rows");
    let width = side as usize;
    let (sum, count) = (nearest.saturating_sub(reach)..(nearest + reach + 1).min(side))
        .flat_map(|row| samples[row as usize * width..(row as usize + 1) * width].iter())
        .filter(|s| !s.is_water())
        .fold((0.0, 0.0), |(sum, count), s| (sum + of(s), count + 1.0));
    sum / f64::max(count, 1.0)
}

#[allow(
    clippy::float_cmp,
    reason = "the values are exact by construction, which is the property under test"
)]
#[test]
fn the_zonal_profile_is_warmest_at_the_equator_and_symmetric() {
    let mut previous = zonal_celsius(0.0);
    for degrees in 1..=90 {
        let latitude = f64::from(degrees);
        let here = zonal_celsius(latitude);
        assert!(here <= previous, "warmer poleward at {latitude}°");
        assert_eq!(
            here,
            zonal_celsius(-latitude),
            "hemispheres differ at {latitude}°"
        );
        previous = here;
    }
    assert!(zonal_celsius(0.0) > 25.0);
    assert!(zonal_celsius(90.0) < -15.0);
    // Past the pole the profile holds its last value rather than wrapping.
    assert_eq!(zonal_celsius(120.0), zonal_celsius(90.0));
}

#[allow(
    clippy::float_cmp,
    reason = "the values are exact by construction, which is the property under test"
)]
#[test]
fn the_belts_are_wet_at_the_equator_dry_in_the_subtropics_and_wet_again_poleward() {
    let equator = belt_rain(0.0);
    let subtropics = belt_rain(27.0);
    let storm_track = belt_rain(50.0);
    let pole = belt_rain(90.0);
    assert!(equator > storm_track && storm_track > subtropics);
    assert!(storm_track > pole);
    let driest = (0..=90)
        .map(|d| belt_rain(f64::from(d)))
        .fold(f64::MAX, f64::min);
    assert_eq!(driest, subtropics.min(driest));
    assert!(belt_rain(-5.0) == belt_rain(0.0) && belt_rain(200.0) == pole);
}

#[test]
fn the_prevailing_winds_share_the_air_everywhere() {
    for flow in airflows(Facing(0xF800)) {
        assert!((flow.heading.0.hypot(flow.heading.1) - 1.0).abs() < 1.0e-9);
    }
    for tenth in -900..=900 {
        let latitude = f64::from(tenth) / 10.0;
        let total: f64 = airflows(Facing(0xF800))
            .iter()
            .map(|&flow| prevalence(flow, latitude))
            .sum();
        assert!((total - 1.0).abs() < 1.0e-9, "{total} at {latitude}°");
    }
}

#[allow(
    clippy::float_cmp,
    reason = "the values are exact by construction, which is the property under test"
)]
#[test]
fn trades_and_easterlies_blow_back_along_the_westerlies_line() {
    let flows = airflows(Facing(0xF800));
    let north_westerly = flows[0];
    let north_easterly = flows[1];
    let south_westerly = flows[2];
    assert!(north_westerly.westerly && north_westerly.northern);
    assert_eq!(north_easterly.heading.0, -north_westerly.heading.0);
    assert_eq!(north_easterly.heading.1, -north_westerly.heading.1);
    // The south's westerlies blow poleward too: the north's, mirrored.
    assert_eq!(south_westerly.heading.0, north_westerly.heading.0);
    assert_eq!(south_westerly.heading.1, -north_westerly.heading.1);
    // Mid-latitudes are the westerlies' and the tropics the trades'.
    assert!(prevalence(north_westerly, 45.0) > 0.99);
    assert!(prevalence(north_easterly, 15.0) > 0.99);
    assert!(prevalence(north_easterly, 80.0) > 0.99);
    assert!(prevalence(south_westerly, -45.0) > 0.99);
}

#[allow(
    clippy::float_cmp,
    reason = "the values are exact by construction, which is the property under test"
)]
#[test]
fn a_season_is_summer_in_one_hemisphere_and_winter_in_the_other() {
    assert_eq!(Season::June.summer(40.0), 1.0);
    assert_eq!(Season::June.summer(-40.0), -1.0);
    assert_eq!(Season::December.summer(40.0), -1.0);
    assert_eq!(Season::June.summer(0.0), 0.0);
    assert!(Season::June.shift() > 0.0 && Season::December.shift() < 0.0);
    assert_eq!(Season::June.shift(), SEASON_SHIFT);
}

#[test]
fn a_wind_always_has_an_upwind_neighbour() {
    for turn in 0..64_u32 {
        let facing = Facing(u16::try_from(turn * 1024).expect("below 65536"));
        let (wx, wy) = facing.unit_vector();
        let offset = (step_of(wx), step_of(wy));
        assert_ne!(offset, (0, 0), "a unit vector has a dominant axis");
    }
}

#[test]
fn the_wind_order_visits_every_cell_upwind_first() {
    let side = 8_u32;
    let area = (side * side) as usize;
    let (wx, wy) = Facing(0xF800).unit_vector();
    let order = wind_order(area, side, wx, wy).expect("fits");
    assert_eq!(order.len(), area);
    let mut seen = try_filled(area, false).expect("fits");
    let mut previous = f64::MIN;
    for raw in order {
        let index = raw as usize;
        assert!(!seen[index], "a cell was visited twice");
        seen[index] = true;
        let column = u32::try_from(index % side as usize).expect("in grid");
        let row = u32::try_from(index / side as usize).expect("in grid");
        let projection = f64::from(column) * wx + f64::from(row) * wy;
        assert!(projection >= previous - 1.0, "the sweep went backwards");
        previous = projection;
    }
    assert!(seen.iter().all(|&visited| visited));
}

#[test]
fn distance_to_water_is_zero_at_water_and_finite_everywhere_reachable() {
    let side = 4_u32;
    let mut samples = try_filled((side * side) as usize, CoarseSample::default()).expect("fits");
    for sample in &mut samples {
        sample.elevation = Elevation::from_units(10.0);
        sample.water = sample.elevation;
    }
    samples[0].elevation = Elevation::from_units(-1.0);
    samples[0].water = Elevation::SEA_LEVEL;

    let distance = water_distance(&samples, side).expect("fits");
    assert_eq!(distance[0], 0);
    assert_eq!(distance[1], 1);
    assert_eq!(distance[(side + 1) as usize], 2);
    assert!(distance.iter().all(|&d| d < u32::MAX));
}

#[test]
fn a_realm_with_no_water_is_uniformly_continental() {
    let side = 3_u32;
    let mut samples = try_filled((side * side) as usize, CoarseSample::default()).expect("fits");
    for sample in &mut samples {
        sample.elevation = Elevation::from_units(50.0);
        sample.water = sample.elevation;
    }
    let distance = water_distance(&samples, side).expect("fits");
    assert!(
        distance.iter().all(|&d| d == u32::MAX),
        "no source, no flood"
    );
}

#[test]
fn the_north_of_a_realm_spanning_the_planet_is_colder_than_its_equator() {
    let (params, samples) = solved(|spec| {
        spec.north_latitude = 80;
        spec.south_latitude = 0;
    });
    let side = params.coarse_samples() as usize;
    let mean = |rows: core::ops::Range<usize>| {
        let (mut total, mut count) = (0.0, 0.0);
        for row in rows {
            for sample in &samples[row * side..(row + 1) * side] {
                if !sample.is_water() {
                    total += sample.temperature.celsius();
                    count += 1.0;
                }
            }
        }
        total / f64::max(count, 1.0)
    };
    assert!(mean(0..side / 8) + 20.0 < mean(side - side / 8..side));
}

#[test]
fn seasons_swing_most_toward_the_pole_and_deep_inland() {
    let (params, samples) = solved(|spec| {
        spec.north_latitude = 80;
        spec.south_latitude = 0;
    });
    let side = params.coarse_samples() as usize;
    let polar = &samples[..side * 4];
    let equatorial = &samples[side * (side - 4)..];
    let widest = |set: &[CoarseSample]| {
        set.iter()
            .map(|s| s.range.celsius())
            .fold(f64::MIN, f64::max)
    };
    assert!(widest(polar) > widest(equatorial) + 15.0);
    // At one latitude, the interior's seasons are further apart than the
    // coast's.
    for row in (0..side).step_by(8) {
        let band = &samples[row * side..(row + 1) * side];
        let inland = band.iter().max_by_key(|s| s.continentality);
        let coast = band
            .iter()
            .filter(|s| !s.is_water())
            .min_by_key(|s| s.continentality);
        if let (Some(inland), Some(coast)) = (inland, coast) {
            if inland.continentality > coast.continentality + 60 {
                assert!(inland.range > coast.range, "row {row}");
            }
        }
    }
}

#[test]
fn a_mediterranean_coast_is_winter_wet_and_a_savanna_summer_wet() {
    // Across a realm spanning the dry belt, rain falls in winter on its
    // poleward flank and in summer on its equatorward one.
    let (params, samples) = solved(|spec| {
        spec.north_latitude = 50;
        spec.south_latitude = 5;
        spec.ocean_permille = 500;
    });
    let season_at =
        |latitude: f64| land_mean(params, &samples, latitude, 0, |s| s.rain_season.fraction());
    assert!(season_at(38.0) < -0.1, "{}", season_at(38.0));
    assert!(season_at(15.0) > 0.1, "{}", season_at(15.0));
}

#[test]
fn the_subtropics_are_drier_than_the_rain_belt_and_the_storm_tracks() {
    let (params, samples) = solved(|spec| {
        spec.north_latitude = 60;
        spec.south_latitude = -5;
    });
    let rain_at = |latitude: f64| {
        land_mean(params, &samples, latitude, 2, |s| {
            s.precipitation.millimetres()
        })
    };
    let (equator, subtropics, westerlies) = (rain_at(2.0), rain_at(26.0), rain_at(50.0));
    assert!(equator > subtropics * 1.5, "{equator} vs {subtropics}");
    assert!(westerlies > subtropics, "{westerlies} vs {subtropics}");
}

#[test]
fn every_value_is_in_range() {
    let (_, samples) = solved(|_| ());
    for sample in &samples {
        assert!(sample.precipitation.millimetres() >= 0.0);
        assert!((-1.0..=1.0).contains(&sample.rain_season.fraction()));
        assert!(sample.range.celsius() >= 0.0);
    }
}

#[test]
fn altitude_cools_the_air() {
    let (params, samples) = solved(|_| ());
    // Compare samples in the same row, so latitude is held constant.
    let side = params.coarse_samples() as usize;
    for row in 0..side {
        let band = &samples[row * side..(row + 1) * side];
        let Some(low) = band
            .iter()
            .filter(|s| !s.elevation.is_submerged())
            .min_by_key(|s| s.elevation)
        else {
            continue;
        };
        let Some(high) = band.iter().max_by_key(|s| s.elevation) else {
            continue;
        };
        let rise = high.elevation.units() - low.elevation.units();
        if rise < 600.0 {
            continue;
        }
        assert!(
            high.temperature < low.temperature,
            "a peak {rise} units above its valley was not colder"
        );
    }
}

#[test]
fn the_lapse_rate_is_the_environmental_one() {
    // 6.5 K per thousand units, which is 6.5 K/km at a unit to the metre.
    assert!((LAPSE_RATE * 1000.0 - 6.5).abs() < 1.0e-9);
}

#[test]
fn a_range_casts_a_rain_shadow() {
    // Westerlies blowing due east through the storm-track band: the lee of
    // a ridge is its east side.
    let (params, samples) = solved(|spec| {
        spec.north_latitude = 55;
        spec.south_latitude = 42;
        spec.westerlies = Facing(0);
    });
    let side = params.coarse_samples() as usize;
    let mut shadowed = 0;
    let mut compared = 0;
    for row in 8..side - 8 {
        for column in 8..side - 9 {
            let index = row * side + column;
            let crest = samples[index];
            let windward = samples[index - 1];
            let lee = samples[index + 1];
            if crest.is_water() || windward.is_water() || lee.is_water() {
                continue;
            }
            let rise = crest.elevation.units() - windward.elevation.units();
            let fall = crest.elevation.units() - lee.elevation.units();
            if rise < 120.0 || fall < 120.0 {
                continue;
            }
            compared += 1;
            if lee.precipitation < windward.precipitation {
                shadowed += 1;
            }
        }
    }
    assert!(compared > 0, "the realm has ridges across the wind");
    assert!(
        shadowed * 4 >= compared * 3,
        "only {shadowed} of {compared} ridges cast a shadow"
    );
}

#[test]
fn the_solve_is_a_pure_function_of_its_input() {
    let (_, first) = solved(|_| ());
    let (_, second) = solved(|_| ());
    assert_eq!(first, second);
}
