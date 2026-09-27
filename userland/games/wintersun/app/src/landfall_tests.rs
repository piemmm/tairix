//! A session starts on dry ground the zone admits a body onto, with room to
//! walk, nearest the centre — and says so when there is none.

use alloc::vec::Vec;

use super::*;
use tairix_wintersun_net::value::EntityKind;
use tairix_wintersun_rules::clock::TickRate;
use tairix_wintersun_rules::entity::SpawnSpec;
use tairix_wintersun_rules::stat::Stats;
use tairix_wintersun_rules::terrain::{cell_at, TerrainCell};
use tairix_wintersun_rules::zone::Zone;
use tairix_wintersun_world::geom::Elevation;
use tairix_wintersun_world::params::{RealmParams, RealmSpec};

/// A body a little wider than the player's.
const RADIUS: u16 = 400;

fn realm(seed: u64, ocean_permille: u16) -> RealmField {
    let params = RealmParams::new(RealmSpec {
        seed,
        extent_chunks: 8,
        coarse_samples: 32,
        plates: 8,
        ocean_permille,
        ..RealmParams::winter_default(seed).spec()
    })
    .expect("the spec is in range");
    RealmField::generate(params).expect("the realm generates")
}

/// Whether a body could stand on the cell at the origin.
fn origin_stands(field: &RealmField) -> bool {
    let chunk = ChunkBuild::new(ChunkCoord { x: 0, y: 0 })
        .and_then(|build| build.finish(field))
        .expect("the origin's chunk generates");
    let window = [&chunk];
    let terrain = ChunkTerrain::new(&window).expect("one chunk is sorted");
    let at = WorldPoint { x: 512, y: 512 };
    footprint_clear(&terrain, cell_at(at), at, RADIUS)
}

/// The cells of the landing chunk a body can reach from the start, found by
/// walking the rules' own step test outward: the claim [`Landfall::room`]
/// makes, checked without the labelling that made it.
fn reachable(landfall: &Landfall) -> usize {
    let window = [&landfall.chunk];
    let terrain = ChunkTerrain::new(&window).expect("one chunk is sorted");
    let grid = Grid {
        origin: chunk_origin(landfall.chunk.coord()),
    };
    let mut seen = alloc::vec![false; CHUNK_AREA];
    let start = cell_at(landfall.at);
    let mut frontier = alloc::vec![start];
    seen[grid.index(start).expect("the start is in its chunk")] = true;
    let mut count = 0;
    while let Some(here) = frontier.pop() {
        count += 1;
        for (dx, dy) in [(1, 0), (-1, 0), (0, 1), (0, -1)] {
            let next = CellCoord::new(here.x + dx, here.y + dy);
            let Some(index) = grid.index(next) else {
                continue;
            };
            let at = next.centre().expect("an in-realm cell has a centre");
            if !seen[index] && footprint_clear(&terrain, here, at, RADIUS) {
                seen[index] = true;
                frontier.push(next);
            }
        }
    }
    count
}

/// Whether the start holds a body with no water anywhere under it.
fn dry_start(landfall: &Landfall) -> bool {
    let window = [&landfall.chunk];
    let terrain = ChunkTerrain::new(&window).expect("one chunk is sorted");
    footprint_clear(&terrain, cell_at(landfall.at), landfall.at, RADIUS)
        && footprint(landfall.at, RADIUS)
            .all(|cell| terrain.cell(cell).is_some_and(|cell| cell.depth() == 0))
}

#[test]
fn a_session_starts_where_the_zone_admits_a_body() {
    let field = realm(0x1A2D_FA11, 380);
    let landfall = landfall(&field, RADIUS).expect("the realm has ground");
    assert_eq!(landfall.chunk.coord(), cell_at(landfall.at).chunk());

    let window = [&landfall.chunk];
    let terrain = ChunkTerrain::new(&window).expect("one chunk is sorted");
    let stats = Stats::new(40, 40, 40, 20, 20).expect("inside the domain");
    let spec = SpawnSpec::new(EntityKind(1), landfall.at, stats, 0, RADIUS).expect("a legal body");
    assert!(
        Zone::new(TickRate::default_rate())
            .spawn(spec, &terrain)
            .is_ok(),
        "the start is ground the zone admits a body onto"
    );
}

#[test]
fn a_realm_with_water_at_its_centre_starts_on_land() {
    let field = (0..512_u64)
        .map(|seed| realm(0x5EA0_0000 + seed, 600))
        .find(|field| !origin_stands(field))
        .expect("some realm has water at its centre");
    let landfall = landfall(&field, RADIUS).expect("the realm has ground");
    assert!(dry_start(&landfall), "the start holds a body, dry");
}

#[test]
fn a_realm_of_open_sea_has_nowhere_to_start() {
    let field = realm(0x0CEA_0000, 1000);
    assert!(matches!(
        landfall(&field, RADIUS),
        Err(ClientError::NoGround)
    ));
}

#[test]
fn the_start_is_a_pure_function_of_the_realm() {
    let field = realm(0x1A2D_FA11, 380);
    let first = landfall(&field, RADIUS).expect("the realm has ground");
    let again = landfall(&field, RADIUS).expect("the realm has ground");
    assert_eq!(first.at, again.at);
    assert_eq!(first.room, again.room);
}

/// The realms the client itself opens, over enough seeds that a river bed,
/// a lake shore or a walled hollow nearest the centre is certain to come up
/// — the search once started nearly half of them in one — and one centred
/// on open sea, whose nearest shore is beach under cliffs.
#[test]
fn every_start_the_client_would_open_on_is_dry_with_room_to_walk() {
    let seeds = (0..16_u64)
        .map(|n| 0x5EED_0000 + n * 7919)
        .chain([0x5EF4_2115]);
    for seed in seeds {
        let field =
            RealmField::generate(RealmParams::winter_default(seed)).expect("the realm generates");
        let landfall = landfall(&field, RADIUS).expect("the realm has ground");
        assert!(dry_start(&landfall), "seed {seed:#x} starts in water");
        assert!(
            landfall.room >= ROOM_CELLS,
            "seed {seed:#x} starts with room for {} cells",
            landfall.room
        );
        assert!(
            reachable(&landfall) >= landfall.room,
            "seed {seed:#x} claims more room than a walk from its start reaches"
        );
    }
}

/// Ground shaped by a rule over the cell, for the patches below.
struct Shaped(fn(CellCoord) -> TerrainCell);

impl Terrain for Shaped {
    fn cell(&self, cell: CellCoord) -> Option<TerrainCell> {
        Some((self.0)(cell))
    }
}

/// The patch the realm's centre is the middle of.
const CENTRED: CellCoord = CellCoord::new(-32, -32);

#[test]
fn a_hollow_a_body_could_drop_into_is_not_where_it_starts() {
    // Two units down, past the one-unit step: easy to fall into, and
    // impossible to climb out of.
    let hollow = Shaped(|cell| {
        let ground = if cell.x.abs() <= 1 && cell.y.abs() <= 1 {
            -16
        } else {
            0
        };
        TerrainCell::dry(Elevation(ground))
    });
    let (at, room) = start_on(&hollow, CENTRED, RADIUS)
        .expect("the patch fits")
        .expect("the patch has ground");
    let cell = cell_at(at);
    assert!(
        cell.x.abs() > 1 || cell.y.abs() > 1,
        "started in the hollow at {cell:?}"
    );
    assert_eq!(room, CHUNK_AREA - 9, "the hollow is its own stretch");
}

#[test]
fn a_river_bed_between_its_banks_is_not_where_it_starts() {
    // A wadeable bed five eighths of a unit under its surface, walled by
    // banks five units above it: a body in it wades and never climbs out.
    let river = Shaped(|cell| {
        if cell.y.abs() <= 3 {
            TerrainCell {
                ground: Elevation(-40),
                water: Elevation(-35),
            }
        } else {
            TerrainCell::dry(Elevation(0))
        }
    });
    let (at, room) = start_on(&river, CENTRED, RADIUS)
        .expect("the patch fits")
        .expect("the patch has ground");
    assert_eq!(
        cell_at(at),
        CellCoord::new(-1, -4),
        "the nearest dry bank to the centre, northern first"
    );
    assert_eq!(room, 29 * 64, "the northern bank, and none of the bed");
}

#[test]
fn a_stretch_joins_only_the_steps_a_body_takes_both_ways() {
    fn columns(rise: i16) -> Stretches {
        let grid = Grid {
            origin: CellCoord::new(0, 0),
        };
        let stair: fn(CellCoord) -> TerrainCell = match rise {
            8 => |cell| TerrainCell::dry(Elevation(i16::try_from(cell.x * 8).unwrap_or(0))),
            _ => |cell| TerrainCell::dry(Elevation(i16::try_from(cell.x * 9).unwrap_or(0))),
        };
        Stretches::label(&Shaped(stair), grid, RADIUS).expect("the patch fits")
    }
    let climbable = columns(8);
    assert_eq!(
        climbable.sizes,
        alloc::vec![CHUNK_AREA],
        "a one-unit stair is one stretch"
    );
    let sheer = columns(9);
    assert_eq!(
        sheer.sizes.len(),
        EDGE,
        "each column past the step stands alone"
    );
    assert!(sheer.sizes.iter().all(|&size| size == EDGE));
}

#[test]
fn ground_all_under_water_has_no_start() {
    let sea = Shaped(|_| TerrainCell {
        ground: Elevation(-80),
        water: Elevation(0),
    });
    assert_eq!(
        start_on(&sea, CENTRED, RADIUS).expect("the patch fits"),
        None
    );
}

#[test]
fn every_candidate_chunk_is_named_once_nearest_first() {
    let field = realm(0x1A2D_FA11, 380);
    let chunks = nearest_land(&field).expect("the scan fits");
    assert!(!chunks.is_empty() && chunks.len() <= TRIES);
    let mut unique: Vec<ChunkCoord> = chunks.clone();
    unique.sort_unstable_by_key(|coord| (coord.y, coord.x));
    unique.dedup();
    assert_eq!(unique.len(), chunks.len(), "a chunk was named twice");
}
