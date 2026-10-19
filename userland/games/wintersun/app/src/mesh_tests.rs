//! A mesh holds the ground it is built from, meets its neighbours exactly,
//! and reports the height of the surface it draws.

use tairix_wintersun_art::decal::Fray;
use tairix_wintersun_art::splat::Warp;
use tairix_wintersun_figure::actor::Relief;
use tairix_wintersun_net::value::{ChunkCoord, WorldPoint};
use tairix_wintersun_world::geom::{CELL_SUB_UNITS, CHUNK_CELLS};
use tairix_wintersun_world::ground::Ground;

use super::{MeshCache, MeshKey, Source, Surfaces, TerrainMesh, WORLD_PER_ELEVATION};
use crate::error::ClientError;
use crate::testing::{chunk, land_of, mesh, mesh_of, realm, worn};

#[test]
fn a_solved_mesh_holds_the_chunks_surface_at_its_vertices() {
    let land = land_of(realm());
    let coord = ChunkCoord { x: 0, y: 0 };
    let solved = chunk(&land, &mut worn(), coord);
    for stride in [1, 4, 64] {
        let mesh = TerrainMesh::solved(&solved, stride, &[], &Fray::new(1), &Warp::new(1))
            .expect("it builds");
        assert_eq!(mesh.side(), CHUNK_CELLS / stride + 1);
        assert_eq!(
            mesh.spacing(),
            CELL_SUB_UNITS * i32::try_from(stride).expect("small")
        );
        for j in 0..mesh.side() {
            for i in 0..mesh.side() {
                let (dx, dy) = (
                    i32::try_from(i * stride).expect("small"),
                    i32::try_from(j * stride).expect("small"),
                );
                let surface = solved.water_beside(dx, dy).expect("within the apron");
                let vertex = mesh.vertex(i, j).expect("on the grid");
                assert_eq!(vertex.height, i32::from(surface.0) * WORLD_PER_ELEVATION);
            }
        }
        assert!(mesh.plan(mesh.side() - 2, mesh.side() - 2).is_some());
        assert!(mesh.plan(mesh.side() - 1, 0).is_none());
        assert!(mesh.grounds().next().is_some());
        let (low, high) = mesh.reach();
        assert!(low[2] <= high[2]);
    }
}

/// Relief reads the same from either side of a seam: two neighbours' shared
/// edge carries the same heights and the same normals.
#[test]
fn neighbouring_meshes_agree_along_their_seam() {
    let land = land_of(realm());
    let mut kept = worn();
    let (west, east) = (ChunkCoord { x: -1, y: 1 }, ChunkCoord { x: 0, y: 1 });
    let fray = Fray::new(1);
    let left = TerrainMesh::solved(&chunk(&land, &mut kept, west), 1, &[], &fray, &Warp::new(1))
        .expect("it builds");
    let right = TerrainMesh::solved(&chunk(&land, &mut kept, east), 1, &[], &fray, &Warp::new(1))
        .expect("it builds");
    let last = left.side() - 1;
    for j in 0..left.side() {
        assert_eq!(left.vertex(last, j), right.vertex(0, j), "row {j}");
        assert_eq!(left.place(last, j), right.place(0, j));
    }

    let near = land.far(west).expect("a patch");
    let far = land.far(east).expect("a patch");
    let step = near.step();
    let left = TerrainMesh::far(&near, step, &[], &fray, &Warp::new(1)).expect("it builds");
    let right = TerrainMesh::far(&far, step, &[], &fray, &Warp::new(1)).expect("it builds");
    let last = left.side() - 1;
    for j in 0..left.side() {
        assert_eq!(left.vertex(last, j), right.vertex(0, j), "row {j}");
    }
}

#[test]
fn the_height_drawn_is_read_off_the_meshes_own_triangles() {
    let coord = ChunkCoord { x: 2, y: -1 };
    // A tilted plane, so every triangle of it gives the plane's own height.
    let plane = |i: u32, j: u32| i32::try_from(i * 300 + j * 70).expect("small") - 5_000;
    let mesh = mesh(coord, 4, Ground::Meadow, plane);
    let spacing = mesh.spacing();
    for (i, j) in [(0, 0), (3, 7), (16, 16), (16, 0)] {
        assert_eq!(mesh.height_at(mesh.place(i, j)), Some(plane(i, j)));
    }
    let origin = mesh.origin();
    let between = WorldPoint {
        x: origin.x + spacing / 2,
        y: origin.y + spacing / 4,
    };
    let expected = -5_000 + 300 / 2 + 70 / 4;
    assert_eq!(mesh.height_at(between), Some(expected));
    let beyond = WorldPoint {
        x: origin.x - 1,
        y: origin.y,
    };
    assert_eq!(mesh.height_at(beyond), None);
}

#[test]
fn a_stride_that_does_not_divide_a_chunk_is_refused() {
    let land = land_of(realm());
    let solved = chunk(&land, &mut worn(), ChunkCoord { x: 0, y: 0 });
    let fray = Fray::new(1);
    for stride in [0, 3, 128] {
        assert_eq!(
            TerrainMesh::solved(&solved, stride, &[], &fray, &Warp::new(1)).err(),
            Some(ClientError::Viewport)
        );
    }
    let patch = land.far(ChunkCoord { x: 0, y: 0 }).expect("a patch");
    if patch.step() > 1 {
        assert_eq!(
            TerrainMesh::far(&patch, patch.step() / 2, &[], &fray, &Warp::new(1)).err(),
            Some(ClientError::Viewport)
        );
    }
}

#[test]
fn the_cache_keeps_what_a_frame_draws_and_stands_one_stride_in_for_another() {
    let coord = ChunkCoord { x: 1, y: 1 };
    let key = |stride| MeshKey {
        coord,
        source: Source::Solved,
        stride,
    };
    let mut cache = MeshCache::new();
    cache.begin();
    assert!(!cache.take(key(4)));
    cache
        .adopt(mesh(coord, 4, Ground::Snow, |_, _| 0))
        .expect("room");
    cache.end();
    assert_eq!(cache.len(), 1);

    cache.begin();
    assert!(!cache.take(key(16)), "a stride not built");
    assert_eq!(cache.stand_in(key(16)), Some(key(4)));
    cache.end();
    assert!(cache.get(key(4)).is_some(), "a stand-in is in use");

    cache.begin();
    cache.end();
    assert!(cache.is_empty(), "a frame that drew nothing keeps nothing");
}

#[test]
fn a_stand_in_is_the_nearest_stride_built_for_its_own_chunk_and_source() {
    let (before, here, after) = (
        ChunkCoord { x: 0, y: 1 },
        ChunkCoord { x: 1, y: 1 },
        ChunkCoord { x: 2, y: 1 },
    );
    let mut cache = MeshCache::new();
    cache.begin();
    for (coord, stride) in [(before, 8), (here, 2), (here, 32), (after, 8)] {
        cache
            .adopt(mesh(coord, stride, Ground::Snow, |_, _| 0))
            .expect("room");
    }
    let key = |source, stride| MeshKey {
        coord: here,
        source,
        stride,
    };
    let solved = |stride| key(Source::Solved, stride);
    assert_eq!(cache.stand_in(solved(4)), Some(solved(2)));
    assert_eq!(
        cache.stand_in(solved(16)),
        Some(solved(32)),
        "nearer in detail, though further in cells"
    );
    assert_eq!(
        cache.stand_in(solved(8)),
        Some(solved(2)),
        "the finer of two as near"
    );
    assert_eq!(
        cache.stand_in(key(Source::Far, 8)),
        Some(solved(2)),
        "a chunk drawn solved stands in for its far reading"
    );
    cache
        .adopt(mesh_of(key(Source::Far, 32), Ground::Snow, |_, _| 0))
        .expect("room");
    assert_eq!(
        cache.stand_in(key(Source::Far, 8)),
        Some(key(Source::Far, 32)),
        "its own source first"
    );
    assert_eq!(
        cache.stand_in(MeshKey {
            coord: ChunkCoord { x: 3, y: 1 },
            ..solved(8)
        }),
        None,
        "a chunk past every one built"
    );
}

#[test]
fn the_surface_a_frame_draws_is_read_off_the_mesh_over_each_point() {
    let (here, there) = (ChunkCoord { x: 0, y: 0 }, ChunkCoord { x: 1, y: 0 });
    let low = mesh(here, 8, Ground::Meadow, |_, _| 100);
    let high = mesh(there, 8, Ground::Meadow, |_, _| 900);
    let surfaces = Surfaces::new(&[&high, &low]).expect("room");
    assert_eq!(surfaces.height(low.place(3, 3)), Some(100));
    assert_eq!(surfaces.height(high.place(3, 3)), Some(900));
    // West of the first cell centre belongs to the chunk before, which is not
    // drawn.
    let origin = low.origin();
    assert_eq!(
        surfaces.height(WorldPoint {
            x: origin.x - 1,
            y: origin.y
        }),
        None
    );
}
