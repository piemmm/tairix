use super::{
    segment_reaches, wetness, Chunk, ChunkBuild, Phase, Surface, APRON_CELLS, GROUND_APRON_CELLS,
    SETTLEMENT_RELIEF_KEPT, SHORE_CELLS, WORK_CELLS,
};
use crate::biome::{self, Biome, Water, SHORE_REACH};
use crate::blend::Blend;
use crate::blend::WEIGHT_TOTAL;
use crate::digest;
use crate::geom::{chunk_origin, signed, CellCoord, Elevation, CHUNK_CELLS};
use crate::ground::Ground;
use crate::land::{Keeping, RealmLand, Store, Unkept};
use crate::params::{RealmParams, RealmSpec};
use crate::realm::RealmField;
use tairix_parallel::SERIAL;
use tairix_wintersun_net::value::ChunkCoord;

/// The land of `params`.
fn land_of(params: RealmParams) -> RealmLand {
    let field = RealmField::generate(params).expect("solves");
    RealmLand::new(field, &SERIAL).expect("its land")
}

fn land(seed: u64) -> RealmLand {
    land_of(
        RealmParams::new(RealmSpec {
            seed,
            extent_chunks: 32,
            coarse_samples: 64,
            ..RealmParams::default_realm(seed).spec()
        })
        .expect("legal"),
    )
}

fn built(land: &RealmLand, coord: ChunkCoord) -> Chunk {
    built_from(land, coord, &mut Unkept)
}

/// The chunk at `coord`, its ground worn from what `store` keeps.
fn built_from(land: &RealmLand, coord: ChunkCoord, store: &mut dyn Store) -> Chunk {
    ChunkBuild::new(coord)
        .expect("fits")
        .finish(land, store, &SERIAL)
        .expect("builds")
}

/// A build with every phase before scatter run, so its working grid is
/// complete, its ground worn from what `store` keeps.
fn through_biome(land: &RealmLand, coord: ChunkCoord, store: &mut dyn Store) -> ChunkBuild {
    let mut build = ChunkBuild::new(coord).expect("fits");
    while build.phase() != Phase::Scatter {
        build.step(land, store, &SERIAL).expect("steps");
    }
    build
}

/// A store keeping every block a test's chunks share.
fn shared() -> Keeping {
    Keeping::within(usize::MAX)
}

#[test]
fn a_chunk_too_far_out_for_its_cells_is_refused() {
    // Past the bound a working cell could not be named by a position; at it,
    // a chunk is named but lies off the realm's land, which is refused.
    use super::MAX_CHUNK_COORD;
    use crate::error::WorldError;
    let bound = i32::try_from(MAX_CHUNK_COORD).expect("fits an i32");
    for coord in [
        ChunkCoord { x: bound + 1, y: 0 },
        ChunkCoord {
            x: 0,
            y: -bound - 1,
        },
        ChunkCoord {
            x: i32::MIN,
            y: i32::MAX,
        },
    ] {
        assert!(matches!(
            ChunkBuild::new(coord),
            Err(WorldError::OutOfRange)
        ));
    }
    let land = land(3);
    for coord in [
        ChunkCoord { x: bound, y: bound },
        ChunkCoord {
            x: -bound,
            y: -bound,
        },
    ] {
        let build = ChunkBuild::new(coord).expect("fits");
        let far = build.work_cell(WORK_CELLS - 1, WORK_CELLS - 1);
        let near = build.work_cell(0, 0);
        assert!(far.centre().is_some() && near.centre().is_some());
        assert!(matches!(
            build.finish(&land, &mut Unkept, &SERIAL),
            Err(WorldError::OutOfRange)
        ));
    }
    // The realm's own edge chunks lie on it, their rings and all.
    let edge = land.field().params().min_chunk();
    let max = land.field().params().max_chunk() - 1;
    for coord in [
        ChunkCoord { x: edge, y: edge },
        ChunkCoord { x: max, y: max },
    ] {
        built(&land, coord);
    }
}

#[test]
fn the_phase_sequence_terminates_at_done() {
    let mut phase = Phase::Relief;
    for _ in 0..16 {
        phase = phase.next();
    }
    assert_eq!(phase, Phase::Done);
    assert_eq!(Phase::Done.next(), Phase::Done);
}

#[test]
fn a_build_walks_every_phase_in_order() {
    let land = land(0xC0DE);
    let mut build = ChunkBuild::new(ChunkCoord { x: 0, y: 0 }).expect("fits");
    let expected = [
        Phase::Water,
        Phase::Structures,
        Phase::Climate,
        Phase::Biome,
        Phase::Scatter,
        Phase::Done,
    ];
    assert_eq!(build.phase(), Phase::Relief);
    for want in expected {
        assert_eq!(
            build.step(&land, &mut Unkept, &SERIAL).expect("steps"),
            want
        );
    }
    assert_eq!(
        build.step(&land, &mut Unkept, &SERIAL).expect("steps"),
        Phase::Done
    );
}

#[test]
fn a_partial_build_is_readable_at_every_phase() {
    let land = land(0xC0DE);
    let mut build = ChunkBuild::new(ChunkCoord { x: 1, y: 1 }).expect("fits");
    loop {
        // Nothing here panics or reads uninitialised state: a client draws
        // whatever is ready rather than waiting.
        let partial = build.partial();
        assert_eq!(partial.coord(), ChunkCoord { x: 1, y: 1 });
        let _ = partial.elevation(0, 0);
        assert_eq!(partial.ground(31, 31).total(), WEIGHT_TOTAL);
        assert_eq!(partial.biome(31, 31).total(), WEIGHT_TOTAL);
        if build.step(&land, &mut Unkept, &SERIAL).expect("steps") == Phase::Done {
            break;
        }
    }
}

#[test]
fn interrupting_a_build_changes_nothing() {
    let land = land(0xC0DE);
    let coord = ChunkCoord { x: -2, y: 3 };

    let straight_through = built(&land, coord);

    // Stop after every phase, do unrelated work, and resume.
    let mut piecewise = ChunkBuild::new(coord).expect("fits");
    let mut elsewhere = shared();
    while piecewise.phase() != Phase::Done {
        piecewise.step(&land, &mut Unkept, &SERIAL).expect("steps");
        let _ = built_from(&land, ChunkCoord { x: 9, y: 9 }, &mut elsewhere);
    }
    let resumed = piecewise
        .finish(&land, &mut Unkept, &SERIAL)
        .expect("finishes");

    assert_eq!(digest::chunk(&straight_through), digest::chunk(&resumed));
}

#[test]
fn a_chunk_is_the_same_alone_as_among_its_neighbours() {
    let land = land(0xB0A7);
    let coord = ChunkCoord { x: 4, y: -5 };
    let alone = digest::chunk(&built(&land, coord));

    // Its neighbours built first, its ground then worn from what they kept.
    let mut kept = shared();
    for dy in -1..=1 {
        for dx in -1..=1 {
            if (dx, dy) != (0, 0) {
                let neighbour = ChunkCoord {
                    x: coord.x + dx,
                    y: coord.y + dy,
                };
                let _ = built_from(&land, neighbour, &mut kept);
            }
        }
    }
    assert_eq!(alone, digest::chunk(&built_from(&land, coord, &mut kept)));
}

#[test]
fn an_apron_holds_what_each_neighbour_holds() {
    let land = land(0xA9E0);
    let mut kept = shared();
    let centre = ChunkCoord { x: 2, y: -3 };
    let chunk = built_from(&land, centre, &mut kept);
    let edge = signed(CHUNK_CELLS);
    for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1), (1, 1), (-1, -1)] {
        let neighbour = built_from(
            &land,
            ChunkCoord {
                x: centre.x + dx,
                y: centre.y + dy,
            },
            &mut kept,
        );
        let (offset_x, offset_y) = (dx * edge, dy * edge);
        let reach = signed(APRON_CELLS);
        for y in -reach..edge + reach {
            for x in -reach..edge + reach {
                let (nx, ny) = (x - offset_x, y - offset_y);
                let inside = |v: i32| (0..edge).contains(&v);
                if !inside(nx) || !inside(ny) {
                    continue;
                }
                let (cx, cy) = (nx.unsigned_abs(), ny.unsigned_abs());
                assert_eq!(
                    chunk.elevation_beside(x, y),
                    Some(neighbour.elevation(cx, cy))
                );
                assert_eq!(chunk.water_beside(x, y), Some(neighbour.water(cx, cy)));
                let ground_reach = signed(GROUND_APRON_CELLS);
                let in_ground_apron = |v: i32| (-ground_reach..edge + ground_reach).contains(&v);
                if in_ground_apron(x) && in_ground_apron(y) {
                    assert_eq!(
                        chunk.ground_beside(x, y),
                        Some(neighbour.ground(cx, cy)),
                        "({x}, {y})"
                    );
                }
            }
        }
    }
    let past = signed(APRON_CELLS) + 1;
    assert_eq!(chunk.elevation_beside(-past, 0), None);
    assert_eq!(
        chunk.ground_beside(edge + signed(GROUND_APRON_CELLS), 0),
        None
    );
}

/// A settlement pulls its ground toward its grade but keeps a share of the
/// relief it was laid on: inside a quarter of its radius, where its pull is
/// near whole, the ground stands between that share and what a quarter's
/// falloff leaves of its departure from the grade.
#[test]
fn a_settlement_keeps_a_share_of_its_natural_relief() {
    let land = land(0x5E77_1E00);
    let field = land.field();
    let mut checked = 0;
    for site in field.sites().iter().filter(|site| site.radius_cells >= 12) {
        let coord = site.at.chunk();
        if !field.params().holds_chunk(coord.x, coord.y) {
            continue;
        }
        let mut build = ChunkBuild::new(coord).expect("fits");
        while build.phase() != Phase::Structures {
            build.step(&land, &mut Unkept, &SERIAL).expect("steps");
        }
        let natural = build.work_ground.clone();
        build.step(&land, &mut Unkept, &SERIAL).expect("levels");
        let (gx, gy) = field.grid_position(site.at);
        let grade = field.coarse_at(gx, gy).elevation_units();
        let inner = f64::from(site.radius_cells) / 4.0;
        let (mut kept, mut was) = (0.0, 0.0);
        for wy in 0..WORK_CELLS {
            for wx in 0..WORK_CELLS {
                let Some((cx, cy)) = ChunkBuild::in_chunk(wx, wy) else {
                    continue;
                };
                let index = ChunkBuild::work_index(wx, wy);
                let cell = build.work_cell(wx, wy);
                let distance = tairix_util::mathf::hypot(
                    f64::from(cell.x - site.at.x),
                    f64::from(cell.y - site.at.y),
                );
                if distance > inner
                    || build.work_wet[index]
                    || build.partial().surface(cx, cy).is_road()
                {
                    continue;
                }
                let (now, before) = (build.work_ground[index] - grade, natural[index] - grade);
                kept += now * now;
                was += before * before;
            }
        }
        if was < 1.0 {
            continue;
        }
        let share = tairix_util::mathf::sqrt(kept / was);
        assert!(
            (SETTLEMENT_RELIEF_KEPT - 0.01..0.38).contains(&share),
            "{share} of its relief kept at {:?}",
            site.at
        );
        checked += 1;
    }
    assert!(checked > 0, "the realm has a settlement on uneven ground");
}

/// Ground the realm holds dry stands above the sea beside it: the relief below
/// the coarse step once sank a dry cell of this chunk almost fifty units
/// beneath sea level, where it would have been drawn as a pit.
#[test]
fn no_dry_cell_lies_below_the_sea() {
    let params = RealmParams::new(RealmSpec {
        seed: 0xFA4_0002,
        extent_chunks: 32,
        coarse_samples: 64,
        ..RealmParams::default_realm(0xFA4_0002).spec()
    })
    .expect("legal");
    let land = land_of(params);
    let mut kept = shared();
    for coord in [ChunkCoord { x: 3, y: -2 }, ChunkCoord { x: 2, y: -2 }] {
        let chunk = built_from(&land, coord, &mut kept);
        for cy in 0..CHUNK_CELLS {
            for cx in 0..CHUNK_CELLS {
                if chunk.water(cx, cy) == chunk.elevation(cx, cy) {
                    assert!(
                        !chunk.elevation(cx, cy).is_submerged(),
                        "{coord:?} ({cx}, {cy}) is dry at {:?}",
                        chunk.elevation(cx, cy)
                    );
                }
            }
        }
    }
}

#[test]
fn the_shore_transform_measures_the_four_neighbour_distance() {
    // The transform is the one quantity a cell's neighbours can change,
    // so it is checked against hand-computed distances rather than
    // against itself. One water cell at the working grid's corner: every
    // other cell's distance is then its Manhattan offset from it, and every
    // cell's nearest water is that one.
    let mut build = ChunkBuild::new(ChunkCoord { x: 0, y: 0 }).expect("fits");
    build.work_shore.fill(u16::MAX);
    build.work_water.fill(Water::Running);
    build.work_shore[ChunkBuild::work_index(0, 0)] = 0;
    build.work_water[ChunkBuild::work_index(0, 0)] = Water::Sea;
    build.measure_shore();

    for wy in 0..WORK_CELLS {
        for wx in 0..WORK_CELLS {
            let index = ChunkBuild::work_index(wx, wy);
            let expected = u16::try_from(wx + wy).expect("inside the grid");
            assert_eq!(build.work_shore[index], expected, "at ({wx}, {wy})");
            assert_eq!(
                build.work_water[index],
                Water::Sea,
                "the sea did not carry to ({wx}, {wy})"
            );
        }
    }

    // With no water at all nothing is reachable, and the transform says
    // so rather than inventing a distance or a shore.
    build.work_shore.fill(u16::MAX);
    build.work_water.fill(Water::Running);
    build.measure_shore();
    assert!(build.work_shore.iter().all(|&d| d == u16::MAX));
    assert!(build
        .work_water
        .iter()
        .all(|&water| water == Water::Running));
}

#[test]
fn a_tie_counts_standing_water_over_a_river_and_the_sea_over_a_lake() {
    // A dry cell equally near two waters faces the more standing of them,
    // whichever pass of the transform reached it first.
    for (first, second) in [
        (Water::Lake, Water::Sea),
        (Water::Sea, Water::Lake),
        (Water::Running, Water::Lake),
        (Water::Lake, Water::Running),
    ] {
        let mut build = ChunkBuild::new(ChunkCoord { x: 0, y: 0 }).expect("fits");
        build.work_shore.fill(u16::MAX);
        build.work_water.fill(Water::Running);
        let (west, east) = (
            ChunkBuild::work_index(10, 20),
            ChunkBuild::work_index(30, 20),
        );
        build.work_shore[west] = 0;
        build.work_shore[east] = 0;
        build.work_water[west] = first;
        build.work_water[east] = second;
        build.measure_shore();
        let between = ChunkBuild::work_index(20, 20);
        assert_eq!(build.work_shore[between], 10);
        assert_eq!(build.work_water[between], first.max(second));
        assert_eq!(
            build.work_water[ChunkBuild::work_index(12, 20)],
            first,
            "nearer the western water"
        );
    }
}

#[test]
fn a_cell_reads_the_same_from_either_side_of_a_seam() {
    // A neighbour's scatter candidate in the halo can exclude one of this
    // chunk's own, so its footing must read what the neighbour's build reads.
    // Pairs a road crosses between are compared, where the cleared flag can
    // differ.
    let land = land(0x5EA3);
    let field = land.field();
    let mut worn = shared();
    let mut compared = 0_u32;
    let mut cleared = 0_u32;
    for road in field.roads() {
        let Some(&from) = road.path.first() else {
            continue;
        };
        let coord = from.chunk();
        let east = ChunkCoord {
            x: coord.x + 1,
            y: coord.y,
        };
        if !field.params().holds_chunk(east.x, east.y) {
            continue;
        }
        let west_build = through_biome(&land, coord, &mut worn);
        let east_build = through_biome(&land, east, &mut worn);
        let seam = chunk_origin(east).x;
        for cy in 0..CHUNK_CELLS {
            for depth in 0..SCATTER_REACH {
                let cell = CellCoord::new(seam + signed(depth), chunk_origin(coord).y + signed(cy));
                let from_west = west_build.footing(field, cell);
                let from_east = east_build.footing(field, cell);
                assert_eq!(
                    from_west.biomes, from_east.biomes,
                    "biomes differ at {cell:?}"
                );
                assert_eq!(
                    from_west.slope.to_bits(),
                    from_east.slope.to_bits(),
                    "slopes differ at {cell:?}"
                );
                assert_eq!(from_west.submerged, from_east.submerged);
                assert_eq!(
                    from_west.cleared, from_east.cleared,
                    "cleared differs at {cell:?}"
                );
                cleared += u32::from(from_east.cleared);
                compared += 1;
            }
        }
        if compared > 20_000 {
            break;
        }
    }
    assert!(compared > 0, "the realm has a road to test beside");
    assert!(cleared > 0, "no road crossed the compared band");
}

/// How far into a neighbour a scatter candidate's footing is read.
const SCATTER_REACH: u32 = crate::scatter::SCATTER_STEP;

#[test]
fn a_seam_reads_the_same_at_every_coarse_step() {
    // A bank reaches into the ring from a link well outside it, more of them
    // the finer the step, and each side must carve the other's edge as it does.
    for coarse_samples in [256, 128, 64] {
        let params = RealmParams::new(RealmSpec {
            seed: 0x57E9,
            extent_chunks: 4,
            coarse_samples,
            ..RealmParams::default_realm(0).spec()
        })
        .expect("legal");
        let land = land_of(params);
        let field = land.field();
        let half = params.half_extent_chunks();
        let mut worn = shared();
        let mut wet = 0_u32;
        for y in -half..half {
            for x in -half..half - 1 {
                let (western, eastern) = (ChunkCoord { x, y }, ChunkCoord { x: x + 1, y });
                let west_build = through_biome(&land, western, &mut worn);
                let east_build = through_biome(&land, eastern, &mut worn);
                let seam = chunk_origin(eastern).x;
                for cy in 0..CHUNK_CELLS {
                    for depth in -signed(SCATTER_REACH)..signed(SCATTER_REACH) {
                        let cell =
                            CellCoord::new(seam + depth, chunk_origin(western).y + signed(cy));
                        let from_west = west_build.footing(field, cell);
                        let from_east = east_build.footing(field, cell);
                        assert_eq!(
                            (from_west.biomes, from_west.submerged, from_west.cleared),
                            (from_east.biomes, from_east.submerged, from_east.cleared),
                            "step {} at {cell:?}",
                            params.cells_per_coarse()
                        );
                        assert_eq!(from_west.slope.to_bits(), from_east.slope.to_bits());
                        wet += u32::from(from_east.submerged);
                    }
                }
            }
        }
        assert!(
            wet > 0,
            "no water along the seams at step {}",
            params.cells_per_coarse()
        );
    }
}

#[test]
fn a_long_road_crossing_a_chunk_is_laid_although_both_its_ends_are_far_away() {
    // A coarse step can be far wider than a chunk, so a road segment can
    // cross one with both ends hundreds of cells outside it. The test is on
    // the segment, never on its endpoints.
    let bounds = ((0.0, 0.0), (79.0, 79.0));
    assert!(segment_reaches((-300.0, 40.0), (212.0, 40.0), 1.6, bounds));
    assert!(segment_reaches(
        (-300.0, -300.0),
        (400.0, 400.0),
        1.6,
        bounds
    ));
    assert!(!segment_reaches((-300.0, 90.0), (212.0, 90.0), 1.6, bounds));
    assert!(segment_reaches((-2.0, -2.0), (-2.0, -2.0), 3.0, bounds));
    assert!(!segment_reaches((-5.0, -5.0), (-5.0, -5.0), 3.0, bounds));
}

#[test]
fn the_sea_is_the_sea_right_up_to_its_shore() {
    // A coast lies between a sample below sea level and one above it, and
    // interpolating the two lifts the water surface above sea level across
    // the band beside the land. That band is still the sea, standing at sea
    // level: it is neither a lake nor a slope of water.
    let land = land(0x5EA5);
    let field = land.field();
    let params = field.params();
    let side = signed(params.coarse_samples());
    let mut worn = shared();
    let mut sea_cells = 0_u32;
    let mut coasts = 0;
    'search: for sy in 1..side - 1 {
        for sx in 1..side - 1 {
            let here = field.sample(sx, sy);
            let east = field.sample(sx + 1, sy);
            if here.is_water() || !east.elevation.is_submerged() {
                continue;
            }
            let chunk = built_from(&land, params.sample_cell(sx, sy).chunk(), &mut worn);
            for cy in 0..CHUNK_CELLS {
                for cx in 0..CHUNK_CELLS {
                    let surface = chunk.surface(cx, cy);
                    if surface.is_lake() {
                        let origin = chunk_origin(chunk.coord());
                        let cell = CellCoord::new(origin.x + signed(cx), origin.y + signed(cy));
                        let (gx, gy) = field.grid_position(cell);
                        assert!(
                            field.coarse_at(gx, gy).sea_share() < 0.5,
                            "sea flagged as lake at {cell:?}"
                        );
                    }
                    if surface.is_sea() && !surface.is_channel() {
                        assert_eq!(
                            chunk.water(cx, cy),
                            Elevation::SEA_LEVEL,
                            "the sea is not flat"
                        );
                        sea_cells += 1;
                    }
                }
            }
            coasts += 1;
            if coasts >= 6 {
                break 'search;
            }
        }
    }
    assert!(coasts > 0 && sea_cells > 0, "the realm has a coast");
}

#[test]
fn the_halo_is_as_wide_as_the_band_it_has_to_resolve() {
    // A shore distance or a gradient the halo cannot measure exactly is one
    // a scatter candidate a step outside the chunk could read differently
    // from the chunk that owns it. So the coast stops making a difference
    // inside the halo's reach, and a cell past it reads exactly as one far
    // from any water.
    assert_eq!(WORK_CELLS, CHUNK_CELLS + 2 * SHORE_CELLS);
    let land = land(0xB0A7);
    let field = land.field();
    let build = through_biome(&land, ChunkCoord { x: 0, y: 0 }, &mut Unkept);
    for cy in 0..CHUNK_CELLS {
        for cx in 0..CHUNK_CELLS {
            let (wx, wy) = (cx + SHORE_CELLS, cy + SHORE_CELLS);
            let Some(mut reading) = build.reading(field, wx, wy) else {
                continue;
            };
            if reading.conditions.shore.0 < SHORE_REACH {
                continue;
            }
            let reached = biome::classify(&reading.conditions);
            reading.conditions.shore = (u16::MAX, Water::Running);
            assert_eq!(
                reached,
                biome::classify(&reading.conditions),
                "a coast beyond its reach"
            );
        }
    }
}

#[test]
fn ground_the_coarse_field_holds_no_water_on_stays_dry() {
    use tairix_util::mathf;

    let land = land(0xD2E5);
    let field = land.field();
    let mut worn = shared();
    let mut checked = 0_u32;
    for x in -3..3 {
        for y in -3..3 {
            let coord = ChunkCoord { x, y };
            let chunk = built_from(&land, coord, &mut worn);
            let origin = chunk_origin(coord);
            for cy in 0..CHUNK_CELLS {
                for cx in 0..CHUNK_CELLS {
                    let cell = CellCoord::new(origin.x + signed(cx), origin.y + signed(cy));
                    let (gx, gy) = field.grid_position(cell);
                    let (sx, sy) = (
                        mathf::round_i32(mathf::floor(gx)),
                        mathf::round_i32(mathf::floor(gy)),
                    );
                    let dry = [(0, 0), (1, 0), (0, 1), (1, 1)]
                        .into_iter()
                        .all(|(dx, dy)| !field.sample(sx + dx, sy + dy).is_water());
                    if !dry {
                        continue;
                    }
                    checked += 1;
                    let surface = chunk.surface(cx, cy);
                    assert!(
                        !surface.is_lake() && !surface.is_sea(),
                        "standing water on ground the coarse field holds dry, at {cell:?}"
                    );
                }
            }
        }
    }
    assert!(checked > 0, "the realm has coarse-dry ground to check");
}

#[test]
fn every_cell_is_coherent() {
    let land = land(0xB0A7);
    let chunk = built(&land, ChunkCoord { x: 0, y: 0 });
    for cy in 0..CHUNK_CELLS {
        for cx in 0..CHUNK_CELLS {
            assert_eq!(chunk.biome(cx, cy).total(), WEIGHT_TOTAL);
            assert_eq!(chunk.ground(cx, cy).total(), WEIGHT_TOTAL);
            let surface = chunk.surface(cx, cy);
            assert!(
                chunk.water(cx, cy) >= chunk.elevation(cx, cy),
                "a water surface below its bed at ({cx}, {cy})"
            );
            if surface.is_water() {
                assert!(!surface.is_cleared(), "a road was laid across a river");
                assert_eq!(chunk.biome(cx, cy), Blend::solid(Biome::OpenWater));
                assert_eq!(chunk.ground(cx, cy), Blend::solid(Ground::Water));
            } else {
                assert_ne!(chunk.biome(cx, cy).dominant(), Biome::OpenWater);
            }
        }
    }
}

#[test]
fn a_cell_index_wraps_onto_the_chunk() {
    let land = land(1);
    let chunk = built(&land, ChunkCoord { x: 0, y: 0 });
    assert_eq!(
        chunk.elevation(0, 0),
        chunk.elevation(CHUNK_CELLS, CHUNK_CELLS)
    );
}

#[test]
fn the_surface_flags_are_independent() {
    let plain = Surface::default();
    assert_eq!(plain.bits(), 0);
    for flag in [
        Surface::ROAD,
        Surface::SETTLEMENT,
        Surface::CHANNEL,
        Surface::LAKE,
        Surface::SEA,
    ] {
        let only = plain.with(flag);
        let set = [
            only.is_road(),
            only.is_settlement(),
            only.is_channel(),
            only.is_lake(),
            only.is_sea(),
        ];
        assert_eq!(
            set.iter().filter(|held| **held).count(),
            1,
            "one flag, one reader"
        );
        assert_eq!(only.is_cleared(), only.is_road() || only.is_settlement());
        assert_eq!(
            only.is_water(),
            only.is_channel() || only.is_lake() || only.is_sea()
        );
    }
    // A road along a lake shore carries both, and each reader sees its own.
    let both = plain.with(Surface::ROAD).with(Surface::LAKE);
    assert!(both.is_road() && both.is_lake() && both.is_cleared() && both.is_water());
    assert!(!both.is_sea() && !both.is_channel() && !both.is_settlement());
}

#[test]
fn a_chunk_reports_the_bytes_it_holds() {
    let land = land(1);
    let chunk = built(&land, ChunkCoord { x: 0, y: 0 });
    let area = (CHUNK_CELLS as usize).pow(2);
    assert!(
        chunk.payload_bytes() >= area * 24,
        "the arrays are accounted for"
    );
}

#[test]
fn scrubbing_leaves_nothing_readable() {
    let land = land(1);
    let mut chunk = built(&land, ChunkCoord { x: 0, y: 0 });
    chunk.scrub();
    assert!(chunk.scatter().is_empty());
    for cy in 0..CHUNK_CELLS {
        for cx in 0..CHUNK_CELLS {
            assert_eq!(chunk.elevation(cx, cy), crate::geom::Elevation::SEA_LEVEL);
            assert_eq!(chunk.surface(cx, cy), Surface::default());
        }
    }
}

#[test]
fn wetness_rises_with_catchment_and_falls_with_gradient() {
    assert_eq!(
        wetness(0.0, 0.2).to_bits(),
        0.0f64.to_bits(),
        "a ridge top is dry"
    );
    assert_eq!(
        wetness(-5.0, 0.2).to_bits(),
        0.0f64.to_bits(),
        "never below dry"
    );
    let (low, high) = (wetness(500.0, 0.1), wetness(50_000.0, 0.1));
    assert!(low < high, "more water gathered is wetter");
    assert!(
        wetness(5_000.0, 0.02) > wetness(5_000.0, 0.4),
        "a gentler slope sheds less"
    );
    for (catchment, gradient) in [(1.0e9, 0.0), (1.0, 50.0), (9_600.0, 1.0e-6)] {
        let wet = wetness(catchment, gradient);
        assert!(
            (0.0..1.0).contains(&wet),
            "{catchment} over {gradient}: {wet}"
        );
    }
}
