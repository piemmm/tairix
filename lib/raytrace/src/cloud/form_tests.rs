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
                f64::from(weather(&deck, (x, z), 50.0).0) > FOOTPRINT
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
        let (cover, column) = weather(&HEAPS, (x, z), 50.0);
        let depth = f64::from(column.top - column.base);
        assert!(
            depth >= HEAPS.depth.0 - 1e-3 && depth <= HEAPS.depth.1 + 1e-3,
            "{depth} deep"
        );
        if f64::from(cover) <= FOOTPRINT {
            continue;
        }
        let around = (0..8u32)
            .filter(|&ring| {
                let angle = f64::from(ring) * core::f64::consts::FRAC_PI_4;
                let at = (
                    x + 1500.0 * mathf::cos(angle),
                    z + 1500.0 * mathf::sin(angle),
                );
                f64::from(weather(&HEAPS, at, 50.0).0) > FOOTPRINT
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
/// however finely, nor where a bounding read says none can: the corners
/// standing as far within a round footprint as a distance has them.
#[test]
fn no_cloud_stands_outside_its_cells_band_or_its_bounding_read() {
    let textures = textures();
    let (mut cloudy, spacing) = (0u32, 130.0);
    for deck in [HEAPS, SHEET] {
        for cell in 0..400u32 {
            let (x0, z0) = (50_000.0 * draw(cell, 5), 50_000.0 * draw(cell, 6));
            let middle = (
                x0 + 4000.0 * (draw(cell, 10) - 0.5),
                z0 + 4000.0 * (draw(cell, 11) - 0.5),
            );
            let radius = 300.0 + 2500.0 * draw(cell, 12);
            let corners = [(0.0, 0.0), (1.0, 0.0), (0.0, 1.0), (1.0, 1.0)].map(|(i, j)| {
                let at = (x0 + i * spacing, z0 + j * spacing);
                let from = mathf::sqrt((at.0 - middle.0).powi(2) + (at.1 - middle.1).powi(2));
                Column {
                    edge: single(radius - from),
                    ..weather(&deck, at, spacing).1
                }
            });
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

/// An envelope stands at its base on its footprint's edge, falls below it
/// beyond at its fall, and rises within to its whole depth a run in: a
/// heap's some billows in however deep it is, a sheet's soon after its edge.
#[test]
fn an_envelope_rises_from_its_footprints_edge_to_its_depth() {
    for deck in [HEAPS, SHEET] {
        let (depth, run) = shape(&deck, 1000.0);
        assert!((depth - 1000.0).abs() < 1e-12);
        assert!(envelope(0.0, (depth, run)).abs() < 1e-12);
        let slope = 2.0 * depth / run;
        let near = 1e-4 * run;
        assert!((envelope(near, (depth, run)) / near - slope).abs() < 1e-3 * slope);
        assert!((envelope(-near, (depth, run)) / -near - FALL).abs() < 1e-9);
        let mut last = f64::NEG_INFINITY;
        for step in -100..=300 {
            let edge = f64::from(step) * run / 100.0;
            let here = envelope(edge, (depth, run));
            assert!(
                here >= last && here <= depth + 1e-9,
                "{edge}: {here} after {last}"
            );
            if edge >= run {
                assert!((here - depth).abs() < 1e-9);
            }
            last = here;
        }
    }
    let (heap, sheet) = (shape(&HEAPS, 1000.0).1, shape(&SHEET, 1000.0).1);
    assert!(
        (heap - HEAP_RUN * HEAPS.billow).abs() < 1e-9 && sheet < 0.1 * heap,
        "{heap} against {sheet}"
    );
    // A heap's walls steepen as it deepens, a small one standing lower than
    // it is broad.
    let (shallow, deep) = (shape(&HEAPS, 600.0), shape(&HEAPS, 2400.0));
    assert!(envelope(200.0, deep) > 3.0 * envelope(200.0, shallow));
    assert!(envelope(150.0, shallow) < 300.0);
}

/// However its depth and how heaped it is, an envelope never stands lower
/// for a deeper column, so a band reckoned from its corners' deepest holds
/// it.
#[test]
fn an_envelope_stands_no_lower_for_a_deeper_column() {
    for heap in [0.0, 0.3, 0.55, 0.8, 1.0] {
        let deck = Deck { heap, ..HEAPS };
        for step in -40..=120 {
            let edge = 25.0 * f64::from(step);
            let mut last = f64::NEG_INFINITY;
            for metres in 1..=60 {
                let here = envelope(edge, shape(&deck, 80.0 * f64::from(metres)));
                assert!(here >= last - 1e-9, "{heap} {edge}: {here} after {last}");
                last = here;
            }
        }
    }
}

#[test]
fn an_envelope_is_reached_where_it_stands_as_high() {
    let shape = shape(&HEAPS, 1200.0);
    for step in 0..100 {
        let rise = 12.0 * f64::from(step);
        let Some(edge) = reached(rise, shape) else {
            panic!("{rise} lies below the top");
        };
        assert!(
            (envelope(edge, shape) - rise).abs() < 1e-6,
            "{rise} at {edge}"
        );
    }
    assert!(reached(1200.0, shape).is_none());
}

/// How far within its envelope a place lies has the envelope's own sign
/// about it, is nought on it, and outside is never nearer than the envelope
/// truly lies, so no turret stands further out than it reaches; beside a
/// wall it is the distance to the wall, as above a top the height over it.
#[test]
fn a_place_lies_within_its_envelope_by_the_chord_beside_it() {
    let shape = shape(&HEAPS, 1500.0);
    let curve: Vec<(f64, f64)> = (-8000..=12_000)
        .map(|step| {
            let edge = 0.25 * f64::from(step);
            (edge, envelope(edge, shape))
        })
        .collect();
    let nearest = |edge: f64, rise: f64| {
        curve
            .iter()
            .map(|&(along, high)| mathf::sqrt((along - edge).powi(2) + (high - rise).powi(2)))
            .fold(f64::INFINITY, f64::min)
    };
    for index in 0..3000u32 {
        let (edge, rise) = (
            -1500.0 + 4000.0 * draw(index, 13),
            1.0 + 2400.0 * draw(index, 14),
        );
        let within = within_envelope(edge, rise, shape);
        let vertical = envelope(edge, shape) - rise;
        assert_eq!(within > 0.0, vertical > 0.0, "{edge} {rise}: {within}");
        assert!(within.abs() <= vertical.abs() + 1e-9);
        if within < 0.0 {
            // The curve is drawn a quarter metre apart.
            assert!(
                -within >= nearest(edge, rise) - 0.2,
                "{edge} {rise}: {within}"
            );
        }
    }
    for step in 1..100 {
        let edge = 15.0 * f64::from(step);
        assert!(within_envelope(edge, envelope(edge, shape), shape).abs() < 1e-6);
    }
    // Halfway up its wall, a step out from the wall is a step from it.
    let wall = reached(750.0, shape).unwrap_or(0.0);
    let out = within_envelope(wall - 20.0, 750.0, shape);
    assert!((out + 20.0).abs() < 4.0, "{out} beside the wall");
    let above = within_envelope(shape.1 + 500.0, 1520.0, shape);
    assert!((above + 20.0).abs() < 1e-6, "{above} above the top");
}

/// Along a line, the squared distance from each vertex to the nearest
/// place its cover crosses a footprint's edge is the least of them all.
#[test]
fn the_nearest_crossing_along_a_line_is_the_nearest_of_them_all() {
    for line in 0..200u32 {
        let length = 2 + usize::try_from(line % 61).unwrap_or(0);
        let spacing = 10.0 + 100.0 * draw(line, 15);
        let covers: Vec<f32> = (0..length)
            .map(|at| single(draw(line * 97 + u32::try_from(at).unwrap_or(0), 16)))
            .collect();
        let mut squares = alloc::vec![0.0f32; length];
        nearest_crossings(|at| covers.get(at).copied(), spacing, &mut squares);
        let crossings: Vec<f64> = covers
            .windows(2)
            .enumerate()
            .filter_map(|(at, pair)| {
                crossed(pair[0], pair[1]).map(|share| (real(at) + share) * spacing)
            })
            .collect();
        for (at, &square) in squares.iter().enumerate() {
            let truth = crossings
                .iter()
                .map(|&place| (place - real(at) * spacing).powi(2))
                .fold(f64::INFINITY, f64::min);
            if truth.is_infinite() {
                assert!(square.is_infinite());
            } else {
                assert!(
                    (f64::from(square) - truth).abs() <= 1e-6 * truth.max(1.0),
                    "{square} against {truth}"
                );
            }
        }
    }
}

/// The lower envelope of a line's parabolas is, at each place, the least
/// over the line of the squared distance to a place and that place's seed.
#[test]
fn the_lower_envelope_is_the_least_of_its_parabolas() {
    for line in 0..300u32 {
        let length = 1 + usize::try_from(line % 70).unwrap_or(0);
        let spacing = 5.0 + 200.0 * draw(line, 17);
        let seeds: Vec<f64> = (0..length)
            .map(|at| {
                let key = line * 131 + u32::try_from(at).unwrap_or(0);
                match draw(key, 18) {
                    chance if chance < 0.4 => f64::INFINITY,
                    chance if chance < 0.5 => 0.0,
                    _ => (3000.0 * draw(key, 19)).powi(2),
                }
            })
            .collect();
        let mut envelope = Envelope::new(length).expect("fits");
        let mut found = alloc::vec![f64::NAN; length];
        envelope.sweep(
            |at| seeds.get(at).copied().unwrap_or(f64::INFINITY),
            spacing,
            |at, squared| found[at] = squared,
        );
        for (at, &squared) in found.iter().enumerate() {
            let truth = seeds
                .iter()
                .enumerate()
                .map(|(other, &seed)| (real(at.abs_diff(other)) * spacing).powi(2) + seed)
                .fold(f64::INFINITY, f64::min);
            if truth.is_infinite() {
                assert!(squared.is_infinite(), "{line} {at}: {squared}");
            } else {
                assert!(
                    (squared - truth).abs() <= 1e-9 * truth.max(1.0),
                    "{line} {at}: {squared} against {truth}"
                );
            }
        }
    }
}

/// A vertex's edge is its distance to the nearest crossing, negative
/// outside its footprint, and no further either way than the farthest.
#[test]
fn an_edge_is_signed_by_the_footprint_and_held_to_the_farthest() {
    assert!((signed_edge(0.9, 400.0, 1000.0) - 20.0).abs() < 1e-6);
    assert!((signed_edge(0.1, 400.0, 1000.0) + 20.0).abs() < 1e-6);
    assert!((signed_edge(0.9, f64::INFINITY, 1000.0) - 1000.0).abs() < 1e-3);
    assert!((signed_edge(0.1, 4e6, 1000.0) + 1000.0).abs() < 1e-3);
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

/// Low in a cloud the wisps cut their cells' middles out of its fringe and
/// leave their walls, and high up cut the walls and leave the middles; what
/// is left never stands denser than the fringe it was cut from, nor thinner
/// further into it, and the cloud within its fringe is never cut at all.
#[test]
fn the_wisps_cut_filaments_low_in_a_cloud_and_puffs_high_up() {
    for step in 0..=20u32 {
        let into = f64::from(step) / 20.0;
        for wisp in [0.0, 0.3, 0.5, 0.6, 0.8, 1.0] {
            for billowing in [0.0, 0.5, 1.0] {
                let cut = eroded(into, wisp, billowing);
                assert!(cut <= into + 1e-12 && cut >= 0.0);
                assert!(eroded((into + 0.05).min(1.0), wisp, billowing) >= cut - 1e-12);
            }
            assert!((eroded(1.0, wisp, 0.0) - 1.0).abs() < 1e-12);
        }
        let (kept, gone) = (CUTTING.0 - 0.01, CUTTING.1 + 0.01);
        assert!((eroded(into, kept, 0.0) - into).abs() < 1e-12);
        assert!((eroded(into, 1.0 - kept, 1.0) - into).abs() < 1e-12);
        if into <= DEEPEST_CUT {
            assert!(eroded(into, gone, 0.0) < 1e-12);
            assert!(eroded(into, 1.0 - gone, 1.0) < 1e-12);
        }
        // Even cut deepest, the fringe thickens into the body unbroken.
        assert!((eroded(1.0, gone, 0.0) - 1.0).abs() < 1e-12);
    }
}

/// A fringe too finely cut for a footprint to see its wisps reads as their
/// mean, the mean of what the wisps leave wherever they are read.
#[test]
fn an_unresolved_fringe_reads_as_the_mean_the_wisps_leave() {
    let textures = textures();
    let side = textures.detail.side;
    let wisps: Vec<f64> = (0..side * side * side)
        .map(|index| {
            let texel = Vec3::new(
                real(index % side) + 0.5,
                real(index / side % side) + 0.5,
                real(index / (side * side)) + 0.5,
            );
            textures.wisp_at(texel, [1.0; FINER.len()])
        })
        .collect();
    for (into, billowing) in [
        (0.25, 0.0),
        (0.5, 0.0),
        (0.5, 0.5),
        (0.75, 0.25),
        (0.9, 0.0),
    ] {
        let mean = wisps
            .iter()
            .map(|&wisp| eroded(into, wisp, billowing))
            .sum::<f64>()
            / real(wisps.len());
        let tabled = textures.fringe_mean_at(into, billowing);
        assert!(
            (tabled - mean).abs() < 0.02,
            "{into} {billowing}: {tabled} against {mean}"
        );
    }
}

/// A heap's fringe reaches out furthest low down, and a bounding read's never
/// stands thinner than a resolved one's, however its wisps fall.
#[test]
fn a_fringe_reaches_out_low_down_and_bounds_every_resolved_read() {
    assert!((fringe(&HEAPS, 1.0) - FRINGE.0 * HEAPS.billow).abs() < 1e-12);
    assert!((fringe(&HEAPS, 0.0) - FRINGE.1 * HEAPS.billow).abs() < 1e-12);
    assert!(FRINGE.0 < FRINGE.1);
    let textures = textures();
    let widths = (SKIN, fringe(&HEAPS, 0.0));
    for step in 0..200u32 {
        let surface = -widths.1 + (widths.1 + SKIN) * f64::from(step) / 200.0;
        let billowing = draw(step, 21);
        let bounding = textures.fringed(
            surface,
            widths,
            billowing,
            &Turrets {
                out: 0.0,
                wisps: None,
            },
        );
        for (wisp, kept) in [(0.0, 1.0), (0.6, 1.0), (1.0, 1.0), (0.6, 0.5), (0.9, 0.0)] {
            let read = Turrets {
                out: 0.0,
                wisps: Some((wisp, kept)),
            };
            assert!(textures.fringed(surface, widths, billowing, &read) <= bounding + 1e-12);
        }
    }
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

/// A place lies no further within its envelope for a column less far within
/// its footprint, a shallower column, or a place higher above its base,
/// however heaped the deck: what lets an envelope at a square's most hold
/// every place over it.
#[test]
fn a_place_lies_further_within_for_a_column_further_in_deeper_or_lower() {
    for heap in [0.0, 0.2, 0.55, 0.8, 1.0] {
        let deck = Deck { heap, ..HEAPS };
        for index in 0..4000u32 {
            let edge = -2000.0 + 6000.0 * draw(index, 31);
            let rise = 3000.0 * draw(index, 32);
            let depth = 50.0 + 3000.0 * draw(index, 33);
            let here = within_envelope(edge, rise, shape(&deck, depth));
            let step = 1.0 + 200.0 * draw(index, 34);
            let further = within_envelope(edge + step, rise, shape(&deck, depth));
            let deeper = within_envelope(edge, rise, shape(&deck, depth + step));
            let higher = within_envelope(edge, rise + step, shape(&deck, depth));
            assert!(
                further >= here - 1e-6,
                "{heap} {edge} {rise} {depth}: {further} < {here}"
            );
            assert!(
                deeper >= here - 1e-6,
                "{heap} {edge} {rise} {depth}: {deeper} < {here}"
            );
            assert!(
                higher <= here + 1e-6,
                "{heap} {edge} {rise} {depth}: {higher} > {here}"
            );
        }
    }
}

/// The highest a place can stand and still lie within a margin of its
/// envelope is found to half a metre: a little below it still reaches, and
/// nowhere above it does.
#[test]
fn the_highest_a_cloud_can_stand_is_found_to_half_a_metre() {
    let mut found = 0u32;
    for index in 0..2000u32 {
        let deck = Deck {
            heap: draw(index, 35),
            ..HEAPS
        };
        let (edge, depth) = (
            -1500.0 + 5000.0 * draw(index, 36),
            50.0 + 3000.0 * draw(index, 37),
        );
        let (margin, shape) = (800.0 * draw(index, 38), shape(&deck, depth));
        let reaches = |rise: f64| within_envelope(edge, rise, shape) + margin > 0.0;
        let Some(highest) = highest_rise(edge, shape, margin) else {
            assert!(!reaches(0.0), "{edge} {depth} {margin}");
            continue;
        };
        found += 1;
        for above in [1e-6, 0.01, 1.0, 50.0, 500.0, 5000.0] {
            assert!(
                !reaches(highest + above),
                "{edge} {depth} {margin}: {highest} + {above}"
            );
        }
        if highest >= 0.5 {
            assert!(
                reaches(highest - 0.55) || highest - 0.55 >= shape.0,
                "{edge} {depth} {margin}: {highest}"
            );
        }
    }
    assert!(found > 500, "{found} reached");
}
