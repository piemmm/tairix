//! Host tests of the ground's own colours: a river's sand only as mottled as
//! its grains lie, its grit the soil's to take, once.

use super::*;

/// A ground of plain shades, under `seed`.
fn ground(seed: u32) -> Ground {
    let shade = |value: f64| Vec3::new(value, 0.9 * value, 0.8 * value);
    Ground {
        palette: Palette {
            grass: shade(0.2),
            dry: shade(0.4),
            moss: shade(0.15),
            earth: shade(0.3),
            silt: shade(0.45),
            rock: shade(0.5),
            strata: shade(0.35),
            lichen: shade(0.6),
            sand: shade(0.7),
            snow: shade(0.95),
        },
        shore: 0.0,
        snow_line: f64::INFINITY,
        cliff: 0.7,
        bedding: 0.3,
        seed,
        ways: None,
        bounds: None,
        tilled: None,
        floor: None,
    }
}

/// However finely it is looked at, a river's sand stands within its mottle
/// of its own shade: the grit of its grains is the soil's, which takes it
/// once over whatever it is made of.
#[test]
fn a_rivers_sand_carries_no_grit_of_its_own() {
    for seed in [1, 7, 0x5eed] {
        let ground = ground(seed);
        let sand = ground.palette.sand.lerp(ground.palette.silt, 0.35);
        for index in 0..4000u32 {
            let p = Vec3::new(
                0.0137 * f64::from(index),
                0.0,
                0.0071 * f64::from(index % 97),
            );
            for width in [1e-4, 1e-3, 1e-2] {
                let ratio = ground.sand_bed(p, width).x / sand.x;
                assert!(
                    (0.84 - 1e-9..=1.0 + 1e-9).contains(&ratio),
                    "{seed}: {ratio} of its shade at {p:?}, {width} across"
                );
            }
        }
    }
}
