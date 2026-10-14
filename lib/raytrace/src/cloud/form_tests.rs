//! Host tests of what a deck's cloud is made of: its noises tile, its
//! weather covers its share of the sky, its cloud keeps within the bands
//! its cells hold, and its water grows denser up from its base.

use super::*;
use crate::cloud::{Deck, Matter};
use crate::sample::{mix32, unit};

const HEAPS: Deck = Deck {
    base: 1200.0,
    base_spread: 150.0,
    depth: (600.0, 1800.0),
    cover: 0.35,
    heap: 1.0,
    scale: 2500.0,
    stretch: 1.2,
    heading: 0.4,
    thickness: 0.12,
    billow: 400.0,
    fibre: 1.0,
    matter: Matter::Water,
    seed: 17,
};

const SHEET: Deck = Deck {
    cover: 0.9,
    heap: 0.0,
    ..HEAPS
};

/// The noises a bank's clouds are cut from, filled whole.
fn textures() -> Textures {
    let mut textures = Textures::new().expect("fits");
    textures.fill_shape(0..SHAPE_SIDE, &tairix_parallel::SERIAL, 5);
    textures.fill_detail(0..DETAIL_SIDE, &tairix_parallel::SERIAL, 6);
    textures
}

/// Draw `salt` of point `index`'s draws, `0.0..1.0`.
fn draw(index: u32, salt: u32) -> f64 {
    unit(mix32(index.wrapping_mul(0x9e37_79b9) ^ salt))
}

#[test]
fn the_noises_repeat_across_their_period() {
    for step in 0..48u32 {
        let p = Vec3::new(
            f64::from(step) * 0.37,
            f64::from(step) * 0.91,
            f64::from(step) * 1.53,
        );
        let worley = Vec3::new(4.0, 8.0, 12.0);
        assert!((worley_tiled(p, 4.0, 3) - worley_tiled(p + worley, 4.0, 3)).abs() < 1e-9);
        let side = real(SHAPE_SIDE);
        let texel = p * 9.0;
        assert!(
            (bubbles(texel, 7) - bubbles(texel + Vec3::new(side, -side, 2.0 * side), 7)).abs()
                < 1e-9
        );
        let detail = real(DETAIL_SIDE);
        assert!(
            (wisps(texel, 7) - wisps(texel + Vec3::new(-detail, detail, detail), 7)).abs() < 1e-9
        );
    }
}

/// Over a broad stretch of sky, the share of columns a deck's footprints
/// cover is the deck's cover, however much or little that is.
#[test]
fn a_decks_footprints_cover_its_share_of_the_sky() {
    for cover in [0.15, 0.35, 0.6, 0.9] {
        let deck = Deck { cover, ..HEAPS };
        let samples = 6000u32;
        let covered = (0..samples)
            .filter(|&index| {
                let (x, z) = (400_000.0 * draw(index, 1), 400_000.0 * draw(index, 2));
                f64::from(weather(&deck, (x, z), 50.0).cover) > FOOTPRINT
            })
            .count();
        let share = real(covered) / f64::from(samples);
        assert!((share - cover).abs() < 0.07, "{cover} covers {share}");
    }
}

/// A column's cloud stands no shallower than the deck's least depth and no
/// deeper than its greatest; and a heap in a broad field of cloud stands
/// deeper than one in a sparse field.
#[test]
fn a_heap_grows_deeper_the_broader_the_cloud_it_stands_in() {
    let (mut broad, mut sparse) = ((0.0, 0u32), (0.0, 0u32));
    for index in 0..8000u32 {
        let (x, z) = (300_000.0 * draw(index, 3), 300_000.0 * draw(index, 4));
        let column = weather(&HEAPS, (x, z), 50.0);
        let depth = f64::from(column.top - column.base);
        assert!(
            depth >= HEAPS.depth.0 - 1e-3 && depth <= HEAPS.depth.1 + 1e-3,
            "{depth} deep"
        );
        if f64::from(column.cover) <= FOOTPRINT {
            continue;
        }
        let around = (0..8u32)
            .filter(|&ring| {
                let angle = f64::from(ring) * core::f64::consts::FRAC_PI_4;
                let at = (
                    x + 1500.0 * mathf::cos(angle),
                    z + 1500.0 * mathf::sin(angle),
                );
                f64::from(weather(&HEAPS, at, 50.0).cover) > FOOTPRINT
            })
            .count();
        let tally = if around >= 6 {
            &mut broad
        } else if around <= 2 {
            &mut sparse
        } else {
            continue;
        };
        tally.0 += depth;
        tally.1 += 1;
    }
    assert!(
        broad.1 > 30 && sparse.1 > 30,
        "{} broad, {} sparse",
        broad.1,
        sparse.1
    );
    let (broad, sparse) = (broad.0 / f64::from(broad.1), sparse.0 / f64::from(sparse.1));
    assert!(
        broad > 1.15 * sparse,
        "{broad} broad against {sparse} sparse"
    );
}

/// Nothing stands outside the band a weather cell's corners give it, read
/// however finely, nor where a bounding read says none can.
#[test]
fn no_cloud_stands_outside_its_cells_band_or_its_bounding_read() {
    let textures = textures();
    let (mut cloudy, spacing) = (0u32, 130.0);
    for deck in [HEAPS, SHEET] {
        for cell in 0..400u32 {
            let (x0, z0) = (50_000.0 * draw(cell, 5), 50_000.0 * draw(cell, 6));
            let corners = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)]
                .map(|(i, j)| weather(&deck, (x0 + i * spacing, z0 + j * spacing), spacing));
            let (low, high) = band_over(&deck, corners, textures.reach(&deck));
            for sample in 0..40u32 {
                let key = cell * 64 + sample;
                let across = (draw(key, 7), draw(key, 8));
                let altitude = deck.base - 400.0 + 3000.0 * draw(key, 9);
                let grained = Vec3::new(x0 + across.0 * spacing, altitude, z0 + across.1 * spacing);
                let column = Blend { corners, across };
                let bounding = cloud(
                    &deck,
                    &textures,
                    (&column, altitude),
                    grained,
                    Edges::Bounding(1.0),
                );
                for footprint in [1.0, 16.0, 120.0] {
                    let Some(held) = cloud(
                        &deck,
                        &textures,
                        (&column, altitude),
                        grained,
                        Edges::Resolved(footprint),
                    ) else {
                        continue;
                    };
                    cloudy += 1;
                    assert!(held.extinction > 0.0 && held.extinction.is_finite());
                    assert!(
                        (f64::from(low)..=f64::from(high)).contains(&altitude),
                        "{altitude} outside {low}..{high}"
                    );
                    if footprint <= 1.0 {
                        assert!(
                            bounding.is_some(),
                            "a bounding read misses cloud at {grained:?}"
                        );
                    }
                }
            }
        }
    }
    assert!(cloudy > 1000, "{cloudy} reads met cloud");
}

#[test]
fn water_grows_denser_up_from_its_base_as_its_liquid_water_does_and_ice_does_not() {
    assert!((extinction(&HEAPS, ADIABATIC) - HEAPS.thickness).abs() < 1e-12);
    let ratio = extinction(&HEAPS, 8.0 * ADIABATIC) / extinction(&HEAPS, ADIABATIC);
    assert!(
        (ratio - 4.0).abs() < 1e-9,
        "eight times the height holds {ratio}"
    );
    let mut last = 0.0;
    for metre in 0..3000u32 {
        let here = extinction(&HEAPS, f64::from(metre));
        assert!(here >= last && here > 0.0, "{metre}: {here} after {last}");
        last = here;
    }
    let ice = Deck {
        matter: Matter::Ice,
        ..HEAPS
    };
    assert!((extinction(&ice, 10.0) - extinction(&ice, 2000.0)).abs() < 1e-15);
}

/// An envelope stands at the base on a footprint's edge, falls below it
/// beyond and rises within; a sheet stands near its full depth soon after
/// its edge, a heap only toward its middle.
#[test]
fn an_envelope_rises_from_its_footprints_edge() {
    assert!(envelope(&HEAPS, FOOTPRINT, 1000.0).abs() < 1e-12);
    assert!(envelope(&HEAPS, FOOTPRINT - 0.05, 1000.0) < 0.0);
    let mut last = f64::NEG_INFINITY;
    for step in 0..=50u32 {
        let cover = f64::from(step) / 50.0;
        let here = envelope(&HEAPS, cover, 1000.0);
        assert!(here >= last, "{cover}: {here} after {last}");
        assert!(here <= 1000.0 + 1e-9);
        last = here;
    }
    let early = FOOTPRINT + 0.05;
    assert!(envelope(&SHEET, early, 1000.0) > 1.5 * envelope(&HEAPS, early, 1000.0));
}

#[test]
fn the_normal_quantile_splits_its_share_and_mirrors_about_the_middle() {
    assert!(normal_quantile(0.5).abs() < 1e-12);
    let mut last = f64::NEG_INFINITY;
    for step in 1..100u32 {
        let share = f64::from(step) / 100.0;
        let quantile = normal_quantile(share);
        assert!(quantile > last);
        assert!((quantile + normal_quantile(1.0 - share)).abs() < 1e-9);
        last = quantile;
    }
    // Within a percent of the normal's own: its upper decile lies at 1.2816.
    assert!((normal_quantile(0.9) - 1.2816).abs() < 0.02);
}

/// Read resolving every finer octave, the wisps are cut finer about their
/// texture's own mean, which they keep; a footprint too broad for any of
/// them reads the texture alone.
#[test]
fn the_finer_wisps_cut_about_the_textures_mean() {
    let textures = textures();
    let side = textures.detail.side;
    let (mut own, mut sharp, mut moved) = (0.0, 0.0, 0.0);
    let count = side * side * side;
    for index in 0..count {
        let texel = Vec3::new(
            real(index % side) + 0.5,
            real(index / side % side) + 0.5,
            real(index / (side * side)) + 0.5,
        );
        let (alone, cut) = (
            textures.wisp_at(texel, [0.0; FINER.len()]),
            textures.wisp_at(texel, [1.0; FINER.len()]),
        );
        assert!((alone - textures.detail.at(texel)).abs() < 1e-12);
        own += alone;
        sharp += cut;
        moved += (cut - alone).abs();
    }
    let (own, sharp, moved) = (own / real(count), sharp / real(count), moved / real(count));
    assert!(
        (own - textures.wisp_mean).abs() < 2e-3,
        "{own} about {}",
        textures.wisp_mean
    );
    assert!(
        (sharp - textures.wisp_mean).abs() < 0.02,
        "{sharp} about {}",
        textures.wisp_mean
    );
    assert!(
        moved > 0.02,
        "the finer reads cut the wisps by {moved} on average"
    );
}

/// A cloud's edge is never sharper than its droplets make it, and widens
/// with a coarser footprint only as far as a billow.
#[test]
fn a_clouds_edge_widens_with_its_footprint_up_to_a_billow() {
    assert!((skin(&HEAPS, 0.0) - SKIN).abs() < 1e-12);
    assert!((skin(&HEAPS, 100.0) - 100.0).abs() < 1e-12);
    assert!((skin(&HEAPS, 1e9) - HEAPS.billow).abs() < 1e-12);
    assert!((edges_footprint(&HEAPS, Edges::Averaged) - AVERAGED * HEAPS.billow).abs() < 1e-12);
}
