use tairix_parallel::SERIAL;
use tairix_wintersun_net::value::ChunkCoord;

use super::*;
use crate::chunk::ChunkBuild;
use crate::digest::{chunk, reference_params};

fn reference() -> RealmLand {
    let field = RealmField::generate(reference_params()).expect("solves");
    RealmLand::new(field, &SERIAL).expect("its land")
}

/// Level 0 lies over the whole realm a fixed few times finer than its field,
/// and level 1 on the cells, at every realm a field can describe.
#[test]
fn the_levels_reach_the_cells_from_a_bounded_level_zero() {
    for (side, step) in [
        (32, 64),
        (64, 32),
        (256, 64),
        (512, 8192),
        (256, 1),
        (512, 4),
    ] {
        let levels = levels(side, step);
        let (far, cells) = (
            levels[0].expect("level 0").ratio,
            levels[CELLS].expect("level 1").ratio,
        );
        assert_eq!(far * cells, step, "{side} samples {step} apart");
        assert!(
            side * far <= LEVEL_ZERO_CELLS.max(side),
            "{side}, {step}: {far}"
        );
    }
}

/// A chunk's ground is the land's to the bit however it was reached: alone,
/// or worn from blocks a store kept of chunks solved before it.
#[test]
fn a_chunks_ground_is_the_same_from_any_store() {
    let land = reference();
    let coord = ChunkCoord { x: 2, y: -1 };
    let build = |store: &mut dyn Store| {
        ChunkBuild::new(coord)
            .and_then(|build| build.finish(&land, store, &SERIAL))
            .expect("builds")
    };
    let alone = chunk(&build(&mut Unkept));
    let mut kept = Keeping::within(usize::MAX);
    for x in 1..=3 {
        let _ = ChunkBuild::new(ChunkCoord { x, y: 0 })
            .and_then(|build| build.finish(&land, &mut kept, &SERIAL))
            .expect("builds");
    }
    assert_eq!(chunk(&build(&mut kept)), alone);
}

/// Ground off the realm's land is refused rather than made up.
#[test]
fn ground_off_the_land_is_refused() {
    let land = reference();
    let far = Placed::square((1 << 20, 0), 80);
    assert_eq!(
        land.ground(far, &mut Unkept, &SERIAL).err(),
        Some(WorldError::OutOfRange)
    );
}

/// A store bound to so many bytes never holds more, however its blocks are
/// kept again larger: a block too large to keep in its place goes, never
/// left standing for ground it no longer is.
#[test]
fn a_keeping_never_holds_more_than_its_bound() {
    let block = Block {
        worn: 1,
        row: 0,
        column: 0,
    };
    let kept = |samples: usize| Kept {
        ground: alloc::vec![0.5; samples],
        track: Vec::new(),
    };
    let mut keeping = Keeping::within(64);
    keeping.keep(block, kept(8));
    assert_eq!(keeping.bytes, 32);
    keeping.keep(block, kept(16));
    assert_eq!(keeping.bytes, 64);
    keeping.keep(block, kept(24));
    assert_eq!(keeping.bytes, 0);
    assert!(!keeping.read(block, &mut |_| {}));
    keeping.keep(Block { column: 1, ..block }, kept(32));
    assert_eq!(keeping.bytes, 0, "too large to keep at all");
}
