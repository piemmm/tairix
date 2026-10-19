//! Fixtures the crate's own tests share.

use tairix_parallel::SERIAL;
use tairix_wintersun_art::decal::Fray;
use tairix_wintersun_art::splat::Warp;
use tairix_wintersun_art::weight::WeightField;
use tairix_wintersun_net::value::ChunkCoord;
use tairix_wintersun_world::chunk::{Chunk, ChunkBuild};
use tairix_wintersun_world::ground::Ground;
use tairix_wintersun_world::land::{Keeping, RealmLand};
use tairix_wintersun_world::params::{RealmParams, RealmSpec};
use tairix_wintersun_world::realm::RealmField;

use crate::mesh::{MeshKey, Source, TerrainMesh, Vertex};

/// A small realm: quick to generate, with relief and several grounds.
pub(crate) fn realm() -> RealmParams {
    RealmParams::new(RealmSpec {
        extent_chunks: 8,
        coarse_samples: 32,
        plates: 8,
        ..RealmParams::default_realm(0x7E57_5EED).spec()
    })
    .expect("the spec is in range")
}

/// `params`' land.
pub(crate) fn land_of(params: RealmParams) -> RealmLand {
    RealmField::generate(params)
        .and_then(|field| RealmLand::new(field, &SERIAL))
        .expect("the realm generates")
}

/// The ground a test's chunks are worn from: every block they share kept.
pub(crate) fn worn() -> Keeping {
    Keeping::within(usize::MAX)
}

/// The chunk at `coord`, worn from what `worn` keeps.
pub(crate) fn chunk(land: &RealmLand, worn: &mut Keeping, coord: ChunkCoord) -> Chunk {
    ChunkBuild::new(coord)
        .expect("a chunk fits")
        .finish(land, worn, &SERIAL)
        .expect("a chunk generates")
}

/// A mesh over chunk `coord` at `stride`, its heights `height(i, j)` in world
/// sub-units, every vertex facing straight up and every quad `ground`.
pub(crate) fn mesh(
    coord: ChunkCoord,
    stride: u32,
    ground: Ground,
    height: impl Fn(u32, u32) -> i32,
) -> TerrainMesh {
    mesh_of(
        MeshKey {
            coord,
            source: Source::Solved,
            stride,
        },
        ground,
        height,
    )
}

/// A mesh `key` names of one `ground`, standing at `height(i, j)`.
pub(crate) fn mesh_of(
    key: MeshKey,
    ground: Ground,
    height: impl Fn(u32, u32) -> i32,
) -> TerrainMesh {
    let side = tairix_wintersun_world::geom::CHUNK_CELLS / key.stride + 1;
    TerrainMesh::build(
        key,
        side,
        |i, j| Vertex {
            height: height(i, j),
            ..Vertex::default()
        },
        |_, _| WeightField::solid(ground),
        &[],
        &Fray::new(0),
        &Warp::new(0),
    )
    .expect("a mesh fits")
}
