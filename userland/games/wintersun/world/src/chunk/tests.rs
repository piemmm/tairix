use super::{segment_reaches, Chunk, ChunkBuild, Phase, Surface, SHORE_CELLS, WORK_CELLS};
use crate::biome::{self, Biome, Water, SHORE_REACH};
use crate::blend::Blend;
use crate::blend::WEIGHT_TOTAL;
use crate::digest;
use crate::geom::{chunk_origin, signed, CellCoord, Elevation, CHUNK_CELLS};
use crate::ground::Ground;
use crate::params::{RealmParams, RealmSpec};
use crate::realm::RealmField;
use tairix_wintersun_net::value::ChunkCoord;

fn field(seed: u64) -> RealmField {
    let params = RealmParams::new(RealmSpec {
        seed,
        extent_chunks: 32,
        coarse_samples: 64,
        ..RealmParams::default_realm(seed).spec()
    })
    .expect("legal");
    RealmField::generate(params).expect("solves")
}

fn built(field: &RealmField, coord: ChunkCoord) -> Chunk {
    ChunkBuild::new(coord)
        .expect("fits")
        .finish(field)
        .expect("builds")
}

/// A build with every phase before scatter run, so its working grid is
/// complete.
fn through_biome(field: &RealmField, coord: ChunkCoord) -> ChunkBuild {
    let mut build = ChunkBuild::new(coord).expect("fits");
    while build.phase() != Phase::Scatter {
        build.step(field).expect("steps");
    }
    build
}

#[test]
fn a_chunk_too_far_out_for_its_cells_is_refused() {
    // Past the bound a working cell's coordinate would overflow; at it, the
    // farthest working cells still fit.
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
        assert!(far.x > near.x && far.y > near.y);
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
    let field = field(0xC0DE);
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
        assert_eq!(build.step(&field).expect("steps"), want);
    }
    assert_eq!(build.step(&field).expect("steps"), Phase::Done);
}

#[test]
fn a_partial_build_is_readable_at_every_phase() {
    let field = field(0xC0DE);
    let mut build = ChunkBuild::new(ChunkCoord { x: 1, y: 1 }).expect("fits");
    loop {
        // Nothing here panics or reads uninitialised state: a client draws
        // whatever is ready rather than waiting.
        let partial = build.partial();
        assert_eq!(partial.coord(), ChunkCoord { x: 1, y: 1 });
        let _ = partial.elevation(0, 0);
        assert_eq!(partial.ground(31, 31).total(), WEIGHT_TOTAL);
        assert_eq!(partial.biome(31, 31).total(), WEIGHT_TOTAL);
        if build.step(&field).expect("steps") == Phase::Done {
            break;
        }
    }
}

#[test]
fn interrupting_a_build_changes_nothing() {
    let field = field(0xC0DE);
    let coord = ChunkCoord { x: -2, y: 3 };

    let straight_through = built(&field, coord);

    // Stop after every phase, do unrelated work, and resume.
    let mut piecewise = ChunkBuild::new(coord).expect("fits");
    while piecewise.phase() != Phase::Done {
        piecewise.step(&field).expect("steps");
        let _ = built(&field, ChunkCoord { x: 9, y: 9 });
    }
    let resumed = piecewise.finish(&field).expect("finishes");

    assert_eq!(digest::chunk(&straight_through), digest::chunk(&resumed));
}

#[test]
fn a_chunk_is_the_same_alone_as_among_its_neighbours() {
    let field = field(0xB0A7);
    let coord = ChunkCoord { x: 4, y: -5 };
    let alone = digest::chunk(&built(&field, coord));

    for dy in -1..=1 {
        for dx in -1..=1 {
            let _ = built(
                &field,
                ChunkCoord {
                    x: coord.x + dx,
                    y: coord.y + dy,
                },
            );
        }
    }
    assert_eq!(alone, digest::chunk(&built(&field, coord)));
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
    // Scatter reads a scatter step into the halo, because a neighbour's
    // candidate there can exclude one of this chunk's own. Everything the
    // candidate's footing reads must therefore be what the neighbour's own
    // build reads for the same cell — its biomes, its slope, whether it is
    // wet, and whether a road or a settlement cleared it. A realm is
    // searched for chunk pairs a road crosses between, so the cleared flag
    // is compared where it can differ.
    let field = field(0x5EA3);
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
        let west_build = through_biome(&field, coord);
        let east_build = through_biome(&field, east);
        let seam = chunk_origin(east).x;
        for cy in 0..CHUNK_CELLS {
            for depth in 0..SCATTER_REACH {
                let cell = CellCoord::new(seam + signed(depth), chunk_origin(coord).y + signed(cy));
                let from_west = west_build.footing(&field, cell);
                let from_east = east_build.footing(&field, cell);
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
    // A channel bank reaches into the ring around a chunk from a coarse
    // link well outside it, and the finer the coarse step the more links
    // lie between. Each side of a seam must carve the other's edge exactly
    // as the other does, or a footing a scatter step across it reads a
    // different shore, gradient or water.
    for coarse_samples in [256, 128, 64] {
        let params = RealmParams::new(RealmSpec {
            seed: 0x57E9,
            extent_chunks: 4,
            coarse_samples,
            ..RealmParams::default_realm(0).spec()
        })
        .expect("legal");
        let field = RealmField::generate(params).expect("solves");
        let half = params.half_extent_chunks();
        let mut wet = 0_u32;
        for y in -half..half {
            for x in -half..half - 1 {
                let (western, eastern) = (ChunkCoord { x, y }, ChunkCoord { x: x + 1, y });
                let west_build = through_biome(&field, western);
                let east_build = through_biome(&field, eastern);
                let seam = chunk_origin(eastern).x;
                for cy in 0..CHUNK_CELLS {
                    for depth in -signed(SCATTER_REACH)..signed(SCATTER_REACH) {
                        let cell =
                            CellCoord::new(seam + depth, chunk_origin(western).y + signed(cy));
                        let from_west = west_build.footing(&field, cell);
                        let from_east = east_build.footing(&field, cell);
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
    let field = field(0x5EA5);
    let params = field.params();
    let side = signed(params.coarse_samples());
    let mut sea_cells = 0_u32;
    let mut coasts = 0;
    'search: for sy in 1..side - 1 {
        for sx in 1..side - 1 {
            let here = field.sample(sx, sy);
            let east = field.sample(sx + 1, sy);
            if here.is_water() || !east.elevation.is_submerged() {
                continue;
            }
            let chunk = built(&field, params.sample_cell(sx, sy).chunk());
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
    let field = field(0xB0A7);
    let build = through_biome(&field, ChunkCoord { x: 0, y: 0 });
    for cy in 0..CHUNK_CELLS {
        for cx in 0..CHUNK_CELLS {
            let (wx, wy) = (cx + SHORE_CELLS, cy + SHORE_CELLS);
            let Some(mut reading) = build.reading(&field, wx, wy) else {
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

    let field = field(0xD2E5);
    let mut checked = 0_u32;
    for x in -3..3 {
        for y in -3..3 {
            let coord = ChunkCoord { x, y };
            let chunk = built(&field, coord);
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
    let field = field(0xB0A7);
    let chunk = built(&field, ChunkCoord { x: 0, y: 0 });
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
    let field = field(1);
    let chunk = built(&field, ChunkCoord { x: 0, y: 0 });
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
    let field = field(1);
    let chunk = built(&field, ChunkCoord { x: 0, y: 0 });
    let area = (CHUNK_CELLS as usize).pow(2);
    assert!(
        chunk.payload_bytes() >= area * 24,
        "the arrays are accounted for"
    );
}

#[test]
fn scrubbing_leaves_nothing_readable() {
    let field = field(1);
    let mut chunk = built(&field, ChunkCoord { x: 0, y: 0 });
    chunk.scrub();
    assert!(chunk.scatter().is_empty());
    for cy in 0..CHUNK_CELLS {
        for cx in 0..CHUNK_CELLS {
            assert_eq!(chunk.elevation(cx, cy), crate::geom::Elevation::SEA_LEVEL);
            assert_eq!(chunk.surface(cx, cy), Surface::default());
        }
    }
}
