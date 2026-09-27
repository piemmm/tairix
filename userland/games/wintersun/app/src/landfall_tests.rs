//! A session starts on ground the zone admits a body onto, nearest the
//! centre, and says so when there is none.

use super::*;
use tairix_wintersun_net::value::{ChunkCoord, EntityKind};
use tairix_wintersun_rules::clock::TickRate;
use tairix_wintersun_rules::entity::SpawnSpec;
use tairix_wintersun_rules::stat::Stats;
use tairix_wintersun_rules::terrain::cell_at;
use tairix_wintersun_rules::zone::Zone;
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

fn stands(landfall: &Landfall) -> bool {
    let window = [&landfall.chunk];
    let terrain = ChunkTerrain::new(&window).expect("one chunk is sorted");
    footprint_clear(&terrain, cell_at(landfall.at), landfall.at, RADIUS)
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
    assert!(stands(&landfall), "the start holds a body");
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
}
