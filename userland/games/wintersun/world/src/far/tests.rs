use alloc::collections::BTreeSet;

use tairix_parallel::SERIAL;
use tairix_wintersun_net::value::ChunkCoord;

use crate::blend::{Kind, WEIGHT_TOTAL};
use crate::chunk::{Chunk, ChunkBuild};
use crate::geom::{signed, CHUNK_CELLS};
use crate::ground::Ground;
use crate::land::{Keeping, RealmLand};
use crate::params::{RealmParams, RealmSpec};
use crate::realm::RealmField;

fn land(seed: u64) -> RealmLand {
    let params = RealmParams::new(RealmSpec {
        seed,
        extent_chunks: 32,
        coarse_samples: 64,
        ..RealmParams::default_realm(seed).spec()
    })
    .expect("legal");
    RealmLand::new(RealmField::generate(params).expect("solves"), &SERIAL).expect("its land")
}

fn solved(land: &RealmLand, coord: ChunkCoord, kept: &mut Keeping) -> Chunk {
    ChunkBuild::new(coord)
        .expect("fits")
        .finish(land, kept, &SERIAL)
        .expect("builds")
}

#[test]
fn a_far_patch_is_pure_and_agrees_with_its_neighbours_along_a_seam() {
    let land = land(0xFA4_0001);
    let coord = ChunkCoord { x: -3, y: 5 };
    let patch = land.far(coord).expect("a patch");
    let again = land.far(coord).expect("a patch");
    let side = patch.side();
    assert_eq!(side, CHUNK_CELLS / land.far_step() + 1);
    for j in 0..side {
        for i in 0..side {
            assert_eq!(patch.surface(i, j), again.surface(i, j));
            assert_eq!(patch.ground(i, j), again.ground(i, j));
        }
    }
    let east = land
        .far(ChunkCoord {
            x: coord.x + 1,
            y: coord.y,
        })
        .expect("a patch");
    let south = land
        .far(ChunkCoord {
            x: coord.x,
            y: coord.y + 1,
        })
        .expect("a patch");
    for k in 0..side {
        assert_eq!(
            patch.surface(side - 1, k),
            east.surface(0, k),
            "east seam at {k}"
        );
        assert_eq!(
            patch.ground(side - 1, k),
            east.ground(0, k),
            "east seam at {k}"
        );
        assert_eq!(
            patch.surface(k, side - 1),
            south.surface(k, 0),
            "south seam at {k}"
        );
        assert_eq!(
            patch.ground(k, side - 1),
            south.ground(k, 0),
            "south seam at {k}"
        );
    }
    assert!(patch.surface(side, 0).is_none(), "nothing past the patch");
}

#[test]
fn a_far_patch_is_the_solved_ground_seen_coarsely() {
    let land = land(0xFA4_0002);
    let mut kept = Keeping::within(64 << 20);
    let (mut agreeing, mut compared) = (0usize, 0usize);
    let (mut seen_wet, mut sea) = (0usize, 0usize);
    let (mut apart, mut dry) = (0.0f64, 0usize);
    for coord in [
        ChunkCoord { x: 0, y: 0 },
        ChunkCoord { x: 3, y: -2 },
        ChunkCoord { x: -5, y: 4 },
    ] {
        let patch = land.far(coord).expect("a patch");
        let chunk = solved(&land, coord, &mut kept);
        let step = land.far_step();
        for j in 0..patch.side() {
            for i in 0..patch.side() {
                let blend = patch.ground(i, j).expect("inside");
                assert_eq!(blend.total(), WEIGHT_TOTAL, "a normalised blend");
                let (cx, cy) = (i * step, j * step);
                if cx >= CHUNK_CELLS || cy >= CHUNK_CELLS {
                    continue;
                }
                let wet = chunk.water(cx, cy) > chunk.elevation(cx, cy);
                let far_wet = blend.dominant() == Ground::Water;
                // Level 0 does not hold the fine relief, so a coast lies a
                // little differently from afar; the sea as a whole agrees.
                if chunk.surface(cx, cy).is_sea() {
                    sea += 1;
                    seen_wet += usize::from(far_wet);
                }
                if wet || far_wet {
                    continue;
                }
                // Dry land lies where level 0 put it, give or take the relief
                // below level 0's step; the sea floor shelves deeper at each
                // level, which the water hides.
                let height = patch.surface(i, j).expect("inside").units();
                let solved_height = chunk.elevation(cx, cy).units();
                if solved_height > 0.0 {
                    apart += (height - solved_height).abs();
                    dry += 1;
                }
                // The dominant ground from afar is one the solved ground near
                // it shows.
                let near: BTreeSet<u8> = (-2i32..=2)
                    .flat_map(|dy| (-2i32..=2).map(move |dx| (dx, dy)))
                    .filter_map(|(dx, dy)| chunk.ground_beside(signed(cx) + dx, signed(cy) + dy))
                    .flat_map(|blend| {
                        blend
                            .slots()
                            .map(|(kind, _)| kind.id())
                            .collect::<alloc::vec::Vec<_>>()
                    })
                    .collect();
                compared += 1;
                if near.contains(&blend.dominant().id()) {
                    agreeing += 1;
                }
            }
        }
    }
    assert!(compared > 40, "the probes held dry ground");
    // Level 0 holds neither the relief below its step nor the wear and the
    // channels the cells add, so it stands a few units proud of the solved
    // ground; a patch read one step off the lattice stands several times
    // further from it.
    let mean = apart / f64::from(u32::try_from(dry).expect("a count"));
    assert!(
        mean < 20.0,
        "dry land lies {mean} from where the solved chunk has it, on average"
    );
    assert!(
        seen_wet * 10 >= sea * 8,
        "{seen_wet} of {sea} sea vertices are water from afar"
    );
    assert!(
        agreeing * 10 >= compared * 6,
        "{agreeing} of {compared} far vertices show a ground the solved chunk has there"
    );
}

#[test]
fn a_far_patch_off_the_realm_is_refused() {
    let land = land(0xFA4_0003);
    let far = land.field().params().max_chunk() + 1;
    assert!(land.far(ChunkCoord { x: far, y: 0 }).is_err());
}

/// A patch's halo holds what its neighbours hold one vertex in, so a slope
/// read at its edge is the slope read from the other side.
#[test]
fn a_far_patchs_halo_holds_its_neighbours_heights() {
    let land = land(0xFA4_0002);
    let coord = ChunkCoord { x: 2, y: -4 };
    let patch = land.far(coord).expect("a patch");
    let east = land
        .far(ChunkCoord {
            x: coord.x + 1,
            y: coord.y,
        })
        .expect("a patch");
    let side = patch.side();
    let edge = i64::from(side);
    for j in 0..side {
        assert_eq!(patch.surface_beside(edge, i64::from(j)), east.surface(1, j));
        assert_eq!(
            east.surface_beside(-1, i64::from(j)),
            patch.surface(side - 2, j)
        );
    }
    assert_eq!(patch.surface_beside(edge + 1, 0), None);
    assert_eq!(patch.surface_beside(-2, 0), None);
}
