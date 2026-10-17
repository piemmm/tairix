use core::sync::atomic::{AtomicUsize, Ordering};

use tairix_log::{Event, Sink};
use tairix_reclaim::{PressureBand, ReportedPressure};

use super::{ChunkCache, GroundCache};
use crate::chunk::ChunkBuild;
use crate::land::{Block, Keeping, RealmLand, Store, Unkept};
use crate::params::{RealmParams, RealmSpec};
use crate::realm::RealmField;
use tairix_parallel::SERIAL;
use tairix_terrain::droplet::PASSES;
use tairix_wintersun_net::value::ChunkCoord;

/// A sink that only counts, so a test can tell a silent cache from a
/// refused one without a journal.
struct CountingSink(AtomicUsize);

impl Sink for CountingSink {
    fn write_event(&self, _: &Event<'_>) {
        self.0.fetch_add(1, Ordering::Relaxed);
    }
}

static SINK: CountingSink = CountingSink(AtomicUsize::new(0));
/// The band every test that does not move it reads, so a test that tightens
/// its own cannot refuse another's entries while they run side by side.
static PRESSURE: ReportedPressure = ReportedPressure::unknown();
static SQUEEZED_CHUNKS: ReportedPressure = ReportedPressure::unknown();
static SQUEEZED_GROUND: ReportedPressure = ReportedPressure::unknown();

/// A cache with a mebibyte-scale budget, which is enough for a handful of
/// chunks and not for a realm's worth.
fn cache() -> ChunkCache {
    PRESSURE.report(PressureBand::Normal);
    ChunkCache::new("wintersun-test", 64 * 1024 * 1024, &PRESSURE, &SINK)
}

/// A ground cache for `land`'s realm under `pressure`, a budget of some
/// mebibytes.
fn ground(land: &RealmLand, pressure: &'static ReportedPressure) -> GroundCache {
    pressure.report(PressureBand::Normal);
    GroundCache::new(
        ("wintersun-test", land.field().params()),
        64 * 1024 * 1024,
        pressure,
        &SINK,
    )
}

/// The digest of the chunk at `coord`, its ground worn from what `store`
/// keeps.
fn digest_from(land: &RealmLand, coord: ChunkCoord, store: &mut dyn Store) -> u64 {
    let chunk = ChunkBuild::new(coord)
        .and_then(|build| build.finish(land, store, &SERIAL))
        .expect("builds");
    crate::digest::chunk(&chunk)
}

/// Whether `store` keeps the origin chunk's own ground, worn by every pass.
fn holds_the_origin(store: &mut dyn Store) -> bool {
    store.read(
        Block {
            worn: PASSES,
            row: 0,
            column: 0,
        },
        &mut |_| {},
    )
}

fn land() -> RealmLand {
    let params = RealmParams::new(RealmSpec {
        seed: 0xCAC4E,
        extent_chunks: 32,
        coarse_samples: 64,
        ..RealmParams::default_realm(0xCAC4E).spec()
    })
    .expect("legal");
    RealmLand::new(RealmField::generate(params).expect("solves"), &SERIAL).expect("its land")
}

#[test]
fn a_chunk_is_generated_once_and_then_held() {
    let land = land();
    let mut cache = cache();
    let coord = ChunkCoord { x: 0, y: 0 };

    let first = cache
        .get_or_generate((&land, &mut Unkept), coord, &SERIAL)
        .expect("generates");
    let first_digest = crate::digest::chunk(&first);
    drop(first);

    let second = cache
        .get_or_generate((&land, &mut Unkept), coord, &SERIAL)
        .expect("serves");
    assert!(second.is_cached(), "the second ask should be a hit");
    assert_eq!(crate::digest::chunk(&second), first_digest);
}

#[test]
fn the_cache_charges_for_what_it_holds() {
    let land = land();
    let mut cache = cache();
    assert_eq!(cache.charged_bytes(), 0);
    let served = cache
        .get_or_generate((&land, &mut Unkept), ChunkCoord { x: 1, y: 2 }, &SERIAL)
        .expect("generates");
    // A chunk is some seventy kibibytes against a four-mebibyte budget,
    // so a cache this size has no excuse to refuse it.
    assert!(served.is_cached(), "the budget admits a chunk");
    drop(served);
    assert!(cache.charged_bytes() > 0, "a held entry must be charged");
}

#[test]
fn pressure_gives_the_memory_back() {
    let land = land();
    SQUEEZED_CHUNKS.report(PressureBand::Normal);
    let mut cache = ChunkCache::new("wintersun-test", 64 * 1024 * 1024, &SQUEEZED_CHUNKS, &SINK);
    let mut worn = Keeping::within(usize::MAX);
    for x in 0..6 {
        let _ = cache.get_or_generate((&land, &mut worn), ChunkCoord { x, y: 0 }, &SERIAL);
    }
    assert!(cache.charged_bytes() > 0, "the cache holds what it built");
    SQUEEZED_CHUNKS.report(PressureBand::Critical);
    assert!(
        cache.enforce_pressure() > 0,
        "a critical band gives chunks back"
    );
    assert_eq!(
        cache.charged_bytes(),
        0,
        "a critical band holds no runtime cache"
    );
}

#[test]
fn ground_worn_through_the_ground_cache_is_the_ground_worn_alone() {
    let land = land();
    let mut kept = ground(&land, &PRESSURE);
    let coord = ChunkCoord { x: 0, y: 0 };
    let alone = digest_from(&land, coord, &mut Unkept);
    assert!(!holds_the_origin(&mut kept));
    assert_eq!(digest_from(&land, coord, &mut kept), alone);
    assert!(
        holds_the_origin(&mut kept),
        "the blocks it was handed are kept"
    );
    assert_eq!(
        digest_from(&land, coord, &mut kept),
        alone,
        "and read back to the bit"
    );
}

#[test]
fn pressure_gives_the_worn_ground_back() {
    let land = land();
    let mut kept = ground(&land, &SQUEEZED_GROUND);
    let _ = digest_from(&land, ChunkCoord { x: 0, y: 0 }, &mut kept);
    assert!(holds_the_origin(&mut kept));
    SQUEEZED_GROUND.report(PressureBand::Critical);
    assert!(
        kept.enforce_pressure() > 0,
        "a critical band gives blocks back"
    );
    assert!(!holds_the_origin(&mut kept), "and keeps none");
}

#[test]
fn a_parameter_change_invalidates_every_entry() {
    let first = land();
    let mut cache = cache();
    let coord = ChunkCoord { x: 0, y: 0 };
    let before = crate::digest::chunk(
        &cache
            .get_or_generate((&first, &mut Unkept), coord, &SERIAL)
            .expect("generates"),
    );

    let params = RealmParams::new(RealmSpec {
        seed: 0xD1FF,
        ..first.field().params().spec()
    })
    .expect("legal");
    let second =
        RealmLand::new(RealmField::generate(params).expect("solves"), &SERIAL).expect("its land");
    let after = crate::digest::chunk(
        &cache
            .get_or_generate((&second, &mut Unkept), coord, &SERIAL)
            .expect("generates"),
    );

    assert_ne!(before, after, "a stale chunk was served for a new realm");
}

#[test]
fn the_cache_can_wipe_a_chunk_it_releases() {
    // Terrain is public, so the cache's own release path does not call
    // this — which is exactly why it is exercised here rather than left
    // to be discovered wrong the day something sensitive is cached.
    use tairix_reclaim::CachedBytes;

    let land = land();
    let mut chunk = crate::chunk::ChunkBuild::new(ChunkCoord { x: 0, y: 0 })
        .expect("fits")
        .finish(&land, &mut Unkept, &SERIAL)
        .expect("builds");
    let before = crate::digest::chunk(&chunk);
    chunk.wipe();
    assert_ne!(crate::digest::chunk(&chunk), before, "wipe left the bytes");
    assert!(chunk.scatter().is_empty());
}

#[test]
fn an_uncached_result_is_still_a_correct_one() {
    // A budget too small to admit anything must still answer every ask.
    let land = land();
    PRESSURE.report(PressureBand::Normal);
    let mut tiny = ChunkCache::new("wintersun-tiny", 1024, &PRESSURE, &SINK);
    let served = tiny
        .get_or_generate((&land, &mut Unkept), ChunkCoord { x: 0, y: 0 }, &SERIAL)
        .expect("generates");
    assert!(!served.is_cached(), "a kilobyte cannot hold a chunk");
    assert_eq!(served.ground(0, 0).total(), 255);
    assert_eq!(served.biome(0, 0).total(), 255);
}
