use alloc::vec::Vec;

use super::{soils, Geology, Rock, SoilSite, BOUNDARY_WANDER, PROVINCE_SPLIT, WANDER_CYCLES};
use crate::noise;
use crate::params::{RealmParams, RealmSpec};
use crate::seed::{SeedKey, Stage};
use crate::uplift::Plates;

fn geology(seed: u64, plates: u32) -> (Geology, Plates) {
    let spec = RealmSpec {
        plates,
        ..RealmParams::default_realm(seed).spec()
    };
    let params = RealmParams::new(spec).expect("legal");
    let field = Plates::new(params);
    (
        Geology::new(SeedKey::new(seed), field).expect("fits"),
        field,
    )
}

#[test]
fn the_rock_classes_are_in_discriminant_order() {
    for (index, rock) in Rock::ALL.iter().enumerate() {
        assert_eq!(*rock as usize, index);
    }
}

#[test]
fn the_province_grid_follows_the_plates_and_never_the_extent() {
    let (small, plates) = geology(3, 9);
    assert_eq!(small.grid(), plates.grid() * PROVINCE_SPLIT);
    let mut spec = RealmParams::default_realm(3).spec();
    spec.plates = 9;
    spec.extent_chunks = 4096;
    spec.coarse_samples = 512;
    let wide = Plates::new(RealmParams::new(spec).expect("legal"));
    assert_eq!(
        Geology::new(SeedKey::new(3), wide).expect("fits").grid(),
        small.grid()
    );
}

#[test]
fn the_rock_at_a_place_is_a_pure_function_of_it() {
    let (first, _) = geology(0x6E0, 12);
    let (second, _) = geology(0x6E0, 12);
    for step in 0..400 {
        let u = f64::from(step) * 0.0025;
        let v = f64::from(step % 37) / 37.0;
        assert_eq!(first.at(u, v), second.at(u, v));
    }
}

#[test]
fn every_rock_class_occurs_across_realms() {
    let mut seen = [false; Rock::ALL.len()];
    for seed in 0..12_u64 {
        let (field, _) = geology(seed.wrapping_mul(0x9E37_79B9), 36);
        for row in 0..64 {
            for column in 0..64 {
                let found = field.at(f64::from(column) / 64.0, f64::from(row) / 64.0);
                seen[found.rock as usize] = true;
            }
        }
    }
    let missing: Vec<Rock> = Rock::ALL
        .iter()
        .copied()
        .filter(|rock| !seen[*rock as usize])
        .collect();
    assert!(missing.is_empty(), "never laid down: {missing:?}");
}

#[test]
fn only_basalt_is_volcanic() {
    for seed in 0..6_u64 {
        let (field, _) = geology(seed, 25);
        for row in 0..48 {
            for column in 0..48 {
                let found = field.at(f64::from(column) / 48.0, f64::from(row) / 48.0);
                assert!((0.0..=1.0).contains(&found.volcanism));
                if found.volcanism > 0.0 {
                    assert_eq!(found.rock, Rock::Basalt, "fresh lava that is not basalt");
                }
            }
        }
    }
}

#[test]
fn a_province_boundary_wanders() {
    // Against the partition the sites alone draw, the wander moves some
    // points into a neighbouring province, and only points near a boundary:
    // a boundary that wanders, not one that is noise.
    let (field, _) = geology(0xB0D, 16);
    let grid = f64::from(field.grid());
    let (mut moved, mut total) = (0_u32, 0_u32);
    for step in 0..4000 {
        let (u, v) = (f64::from(step) / 4000.0, 0.37);
        let province = field.nearest(noise::warp(
            field.key,
            Stage::Wander,
            u * grid,
            v * grid,
            WANDER_CYCLES,
            BOUNDARY_WANDER,
        ));
        if province != field.nearest((u * grid, v * grid)) {
            moved += 1;
        }
        total += 1;
    }
    assert!(moved > 0, "the boundaries are the sites' straight edges");
    assert!(
        moved * 4 < total,
        "{moved} of {total} moved: noise, not a wander"
    );
}

#[test]
fn soils_are_a_partition_of_the_ground() {
    for &rock in &Rock::ALL {
        for celsius_step in -30..35 {
            for moisture_step in 0..=30 {
                for alluvial_step in 0..=4 {
                    let site = SoilSite {
                        rock,
                        celsius: f64::from(celsius_step),
                        moisture: f64::from(moisture_step) / 10.0,
                        alluvial: f64::from(alluvial_step) / 4.0,
                    };
                    let s = soils(site);
                    let shares = [
                        s.alluvium,
                        s.loess,
                        s.laterite,
                        s.podzol,
                        s.chernozem,
                        s.brown_earth,
                        s.desert_crust,
                    ];
                    assert!(
                        shares.iter().all(|&share| share >= -1.0e-12),
                        "a negative share at {site:?}"
                    );
                    let total: f64 = shares.iter().sum();
                    assert!((total - 1.0).abs() < 1.0e-9, "{total} at {site:?}");
                }
            }
        }
    }
}

#[test]
fn each_soil_forms_where_it_should() {
    let at = |rock, celsius, moisture, alluvial| {
        soils(SoilSite {
            rock,
            celsius,
            moisture,
            alluvial,
        })
    };
    assert!(at(Rock::Granite, 24.0, 0.1, 0.0).desert_crust > 0.9);
    assert!(at(Rock::Shale, 26.0, 1.8, 0.0).laterite > 0.6);
    assert!(at(Rock::Granite, 1.0, 1.5, 0.0).podzol > 0.6);
    assert!(at(Rock::Limestone, 1.0, 1.5, 0.0).podzol < at(Rock::Granite, 1.0, 1.5, 0.0).podzol);
    assert!(at(Rock::Shale, 9.0, 1.05, 0.0).chernozem > 0.4);
    assert!(at(Rock::Shale, 9.0, 0.65, 0.0).loess > 0.4);
    assert!(at(Rock::Shale, 11.0, 1.8, 0.0).brown_earth > 0.6);
    assert!(at(Rock::Sandstone, 11.0, 1.0, 1.0).alluvium > 0.99);
}
