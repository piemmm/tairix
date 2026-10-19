//! Which ground a view wants and in what order, what the held ground says
//! the surface is, and that a road reaches the ground drawn.
#![allow(
    clippy::float_arithmetic,
    reason = "a camera is placed through the perspective, which takes reals"
)]

use tairix_inline::ArrayVec;
use tairix_raster::view::Perspective;
use tairix_wintersun_art::decal::Fray;
use tairix_wintersun_art::splat::Warp;
use tairix_wintersun_net::value::{ChunkCoord, WorldPoint};
use tairix_wintersun_world::chunk::Chunk;
use tairix_wintersun_world::geom::{CELL_SUB_UNITS, CHUNK_CELLS};
use tairix_wintersun_world::ground::Ground;

use super::*;
use crate::mesh::{TerrainMesh, WORLD_PER_ELEVATION};
use crate::testing::{chunk, land_of, realm, worn};

/// The middle of chunk `coord`.
fn middle(coord: ChunkCoord) -> WorldPoint {
    WorldPoint {
        x: coord.x * CHUNK_SUB_UNITS + CHUNK_SUB_UNITS / 2,
        y: coord.y * CHUNK_SUB_UNITS + CHUNK_SUB_UNITS / 2,
    }
}

/// A camera above `at`, looking north and down.
fn looking_north(at: WorldPoint) -> Perspective {
    Perspective::new(
        [f64::from(at.x), f64::from(at.y) + 30_000.0, 40_000.0],
        [0.0, -0.6, -0.8],
        [1.0, 0.0, 0.0],
        [0.0, 0.8, -0.6],
        200.0,
        (320, 180),
        256.0,
    )
    .expect("a camera")
}

#[test]
fn a_point_lies_in_the_chunk_whose_square_holds_it() {
    assert_eq!(
        chunk_at(WorldPoint { x: 0, y: 0 }),
        ChunkCoord { x: 0, y: 0 }
    );
    assert_eq!(
        chunk_at(WorldPoint { x: -1, y: -1 }),
        ChunkCoord { x: -1, y: -1 }
    );
    assert_eq!(
        chunk_at(WorldPoint {
            x: CHUNK_SUB_UNITS,
            y: CHUNK_SUB_UNITS - 1
        }),
        ChunkCoord { x: 1, y: 0 }
    );
    let coord = ChunkCoord { x: 2, y: -3 };
    assert_eq!(distance_squared(coord, middle(coord)), 0);
    let beside = WorldPoint {
        x: 3 * CHUNK_SUB_UNITS + 10,
        y: middle(coord).y,
    };
    assert_eq!(distance_squared(coord, beside), 100);
}

#[test]
fn held_ground_is_ordered_once_and_given_back_beyond_its_reach() {
    let land = land_of(realm());
    let mut kept = worn();
    let coords = [
        ChunkCoord { x: 1, y: 0 },
        ChunkCoord { x: -1, y: 0 },
        ChunkCoord { x: 0, y: 0 },
    ];
    let mut ground = HeldGround::new();
    for coord in coords {
        ground.adopt(chunk(&land, &mut kept, coord)).expect("room");
        ground
            .adopt_far(land.far(coord).expect("a patch"))
            .expect("room");
    }
    ground
        .adopt(chunk(&land, &mut kept, coords[0]))
        .expect("room");
    let held: alloc::vec::Vec<ChunkCoord> = ground.solved().iter().map(Chunk::coord).collect();
    let mut sorted = coords.to_vec();
    sorted.sort_unstable();
    assert_eq!(held, sorted, "held in coordinate order, each once");
    assert_eq!(ground.far().len(), 3);

    // Focused on the west chunk with a radius of nought, the east one is two
    // chunks away and given back; the patches are all in view range.
    let west = middle(coords[1]);
    assert!(
        ground.release_distant(west, 0, west),
        "the east chunk was given back"
    );
    assert!(!ground.holds(coords[0]));
    assert!(ground.holds(coords[1]) && ground.holds(coords[2]));
    assert!(coords.iter().all(|coord| ground.holds_far(*coord)));
    // An eye far beyond the view range gives every patch back.
    let away = WorldPoint {
        x: west.x + 20 * CHUNK_SUB_UNITS * 4,
        y: west.y,
    };
    assert!(ground.release_distant(away, 0, away));
    assert!(ground.far().is_empty());
    assert!(
        !ground.release_distant(away, 0, away),
        "nothing left to give back"
    );
}

#[test]
fn the_nearest_unsolved_chunk_about_the_focus_is_asked_for_first() {
    let params = realm();
    let land = land_of(params);
    let focus = middle(ChunkCoord { x: 0, y: 0 });
    let mut ground = HeldGround::new();
    assert_eq!(
        ground.next_solve(params, focus, 1, &[]),
        Some(ChunkCoord { x: 0, y: 0 }),
        "the focus's own chunk first"
    );
    ground
        .adopt(chunk(&land, &mut worn(), ChunkCoord { x: 0, y: 0 }))
        .expect("room");
    let next = ground
        .next_solve(params, focus, 1, &[])
        .expect("more to solve");
    assert_eq!(
        (next.x.abs() + next.y.abs()),
        1,
        "an edge neighbour before a corner"
    );
    let refused = [next];
    assert_ne!(ground.next_solve(params, focus, 1, &refused), Some(next));
    assert_eq!(
        ground.next_solve(params, focus, 0, &[]),
        None,
        "nothing left at radius nought"
    );
}

#[test]
fn far_ground_is_asked_for_only_where_the_camera_sees_and_nothing_covers_it() {
    let params = realm();
    let land = land_of(params);
    let at = middle(ChunkCoord { x: 0, y: 1 });
    let camera = looking_north(at);
    let mut ground = HeldGround::new();
    let first = ground.next_far(params, &camera, &[]);
    assert!(!first.is_empty() && first.len() <= FAR_BATCH);
    let eye = WorldPoint {
        x: at.x,
        y: at.y + 30_000,
    };
    let distances: alloc::vec::Vec<i64> = first
        .iter()
        .map(|coord| distance_squared(*coord, eye))
        .collect();
    assert!(
        distances.windows(2).all(|pair| pair[0] <= pair[1]),
        "nearest first"
    );
    for coord in &first {
        assert!(params.holds_chunk(coord.x, coord.y));
        assert!(
            coord.y <= at.y.div_euclid(CHUNK_SUB_UNITS) + 1,
            "behind the eye"
        );
    }
    // Far ground held is not asked for again; solved ground still is, so its
    // far reading is held when it is given back.
    ground
        .adopt(chunk(&land, &mut worn(), first[0]))
        .expect("room");
    ground
        .adopt_far(land.far(first[1]).expect("a patch"))
        .expect("room");
    let refused: ArrayVec<ChunkCoord, 1> = {
        let mut list = ArrayVec::new();
        let _ = list.try_push(first[2]);
        list
    };
    let second = ground.next_far(params, &camera, refused.as_slice());
    assert!(
        second.contains(&first[0]),
        "solved ground's far reading was not asked for"
    );
    assert!(!second.contains(&first[1]));
    assert!(
        !second.contains(&first[2]),
        "a refused chunk was asked for again"
    );
}

#[test]
fn a_chunks_box_holds_every_vertex_its_meshes_draw() {
    let land = land_of(realm());
    let coord = ChunkCoord { x: 1, y: -1 };
    let solved = crate::testing::mesh(coord, 4, Ground::Meadow, |_, _| 0);
    let patch = land.far(coord).expect("a patch");
    let far = TerrainMesh::far(&patch, patch.step(), &[], &Fray::new(1), &Warp::new(1))
        .expect("a far mesh");
    let (low, high) = chunk_box(coord);
    for mesh in [&solved, &far] {
        for j in 0..mesh.side() {
            for i in 0..mesh.side() {
                let at = mesh.place(i, j);
                let (x, y) = (f64::from(at.x), f64::from(at.y));
                assert!(
                    (low[0]..=high[0]).contains(&x) && (low[1]..=high[1]).contains(&y),
                    "{at:?} lies outside its chunk's box"
                );
            }
        }
    }
}

/// Above `at`, looking straight up: a view that sees no ground at all.
fn skyward(at: WorldPoint) -> Perspective {
    Perspective::new(
        [f64::from(at.x), f64::from(at.y), 1_000_000.0],
        [0.0, 0.0, 1.0],
        [1.0, 0.0, 0.0],
        [0.0, 1.0, 0.0],
        500.0,
        (640, 360),
        1.0,
    )
    .expect("a view")
}

#[test]
fn ground_with_no_room_is_asked_for_again_once_relieved_and_never_refused() {
    let params = realm();
    let at = middle(ChunkCoord { x: 0, y: 1 });
    let camera = looking_north(at);
    let mut holding = Holding::new();
    let Some(Ask::Far(first)) = holding.next_ask(params, &camera, at) else {
        panic!("far ground is asked for first");
    };
    let short_far = Made::Far {
        patches: Vec::new(),
        refused: Vec::new(),
        short: true,
    };
    assert!(holding.take(short_far).is_some(), "the shortfall is said");
    assert!(
        matches!(holding.next_ask(params, &camera, at), Some(Ask::Solve(_))),
        "chunks are still solved while far ground has no room"
    );
    assert!(holding.take(Made::Short).is_some());
    assert_eq!(
        holding.next_ask(params, &camera, at),
        None,
        "nothing is asked with no room"
    );
    assert!(holding.take(Made::Short).is_none(), "said once");
    holding.relieve();
    assert_eq!(
        holding.next_ask(params, &camera, at),
        Some(Ask::Far(first)),
        "a shortfall refused nothing"
    );
}

#[test]
fn a_chunk_the_generator_refused_is_never_asked_for_again() {
    let params = realm();
    let at = middle(ChunkCoord { x: 0, y: 1 });
    let camera = looking_north(at);
    let short_far = || Made::Far {
        patches: Vec::new(),
        refused: Vec::new(),
        short: true,
    };
    let mut holding = Holding::new();
    let _ = holding.take(short_far());
    let Some(Ask::Solve(first)) = holding.next_ask(params, &camera, at) else {
        panic!("a chunk is asked for");
    };
    assert!(holding.take(Made::Refused(first)).is_some());
    assert!(holding.take(Made::Refused(first)).is_none(), "said once");
    holding.relieve();
    let _ = holding.take(short_far());
    assert_ne!(
        holding.next_ask(params, &camera, at),
        Some(Ask::Solve(first))
    );
}

#[test]
fn ground_off_the_realm_is_refused_rather_than_short() {
    let land = land_of(realm());
    let off = ChunkCoord {
        x: i32::MAX / 4,
        y: 0,
    };
    assert!(matches!(
        Made::solved(off, &land, &mut worn()),
        Made::Refused(coord) if coord == off
    ));
    let Made::Far {
        patches,
        refused,
        short,
    } = Made::far(&[off], &land)
    else {
        panic!("far ground answers as far ground");
    };
    assert!(patches.is_empty() && refused == [off] && !short);
}

#[test]
fn a_scan_that_found_nothing_is_made_again_once_the_view_moves() {
    let params = realm();
    let away = WorldPoint {
        x: i32::MAX / 2,
        y: i32::MAX / 2,
    };
    let mut holding = Holding::new();
    assert_eq!(holding.next_ask(params, &skyward(away), away), None);
    let at = middle(ChunkCoord { x: 0, y: 1 });
    assert!(matches!(
        holding.next_ask(params, &looking_north(at), away),
        Some(Ask::Far(_))
    ));
}

#[test]
fn held_ground_says_the_surface_is_where_its_cells_put_it() {
    let land = land_of(realm());
    let coord = ChunkCoord { x: 0, y: 0 };
    let solved = chunk(&land, &mut worn(), coord);
    let mut ground = HeldGround::new();
    assert_eq!(ground.height_at(middle(coord)), None, "nothing held");
    ground
        .adopt_far(land.far(coord).expect("a patch"))
        .expect("room");
    let far = ground
        .height_at(middle(coord))
        .expect("the patch covers it");
    let mesh = TerrainMesh::far(
        &ground.far()[0],
        ground.far()[0].step(),
        &[],
        &Fray::new(1),
        &Warp::new(1),
    )
    .expect("it builds");
    assert_eq!(
        Some(far),
        mesh.height_at(middle(coord)),
        "the patch and its mesh agree"
    );

    ground.adopt(solved).expect("room");
    let half = CELL_SUB_UNITS / 2;
    let last = i32::try_from(CHUNK_CELLS).expect("a chunk's width") - 1;
    for (dx, dy) in [(0, 0), (5, 9), (last, 3)] {
        let at = WorldPoint {
            x: dx * CELL_SUB_UNITS + half,
            y: dy * CELL_SUB_UNITS + half,
        };
        let water = ground.solved()[0]
            .water_beside(dx, dy)
            .expect("in the chunk");
        assert_eq!(
            ground.height_at(at),
            Some(i32::from(water.0) * WORLD_PER_ELEVATION)
        );
    }
}

#[test]
fn a_distant_mesh_is_drawn_coarser_and_never_coarser_than_a_chunk() {
    let focal = 770;
    assert_eq!(stride_for(0, focal), 1);
    assert_eq!(stride_for(i64::from(24 * CELL_SUB_UNITS), focal), 1);
    let mut last = 1;
    for metres in [100, 300, 1_000, 3_000, 100_000] {
        let stride = stride_for(i64::from(metres) * i64::from(CELL_SUB_UNITS), focal);
        assert!(stride >= last && stride.is_power_of_two() && stride <= CHUNK_CELLS);
        last = stride;
    }
    assert_eq!(last, CHUNK_CELLS);
    // A wider screen sees finer detail at the same distance.
    let far = i64::from(1_000 * CELL_SUB_UNITS);
    assert!(stride_for(far, focal * 4) < stride_for(far, focal));
}

#[test]
fn a_road_reaches_the_ground_drawn() {
    // The reference realm, because a realm as small as the others here places
    // no settlements to route a road between.
    let params = crate::reference::params().expect("the reference realm is in range");
    let land = land_of(params);
    let field = land.field();
    let roads = RoadDecals::from_realm(field).expect("the roads fit");
    assert!(!roads.is_empty(), "the reference realm routes a road");
    let decals = roads.decals().expect("the decals fit");
    let fray = Fray::new(field.params().seed());
    let on_road = field
        .roads()
        .first()
        .and_then(|road| road.path.first())
        .and_then(|cell| cell.centre())
        .expect("a road has a first cell");
    let solved = chunk(&land, &mut worn(), chunk_at(on_road));
    let bare = TerrainMesh::solved(&solved, 1, &[], &fray, &Warp::new(1)).expect("it builds");
    let paved = TerrainMesh::solved(&solved, 1, &decals, &fray, &Warp::new(1)).expect("it builds");
    let quad = |mesh: &TerrainMesh| {
        let origin = mesh.origin();
        let (i, j) = (
            u32::try_from((on_road.x - origin.x).div_euclid(mesh.spacing())).expect("inside"),
            u32::try_from((on_road.y - origin.y).div_euclid(mesh.spacing())).expect("inside"),
        );
        mesh.plan(i.min(mesh.side() - 2), j.min(mesh.side() - 2))
            .expect("a quad")
            .grounds()
            .any(|ground| ground == Ground::Gravel)
    };
    assert!(
        !quad(&bare),
        "the road starts on gravel already, so it would prove nothing"
    );
    assert!(quad(&paved), "the road did not reach its quad");
}

#[test]
fn an_ask_in_flight_reaches_the_loop_however_often_the_loop_asks() {
    let mut desk = ChunkDesk::new();
    let wanted = Ask::Solve(ChunkCoord { x: 3, y: -2 });
    assert!(desk.ask(wanted.clone()), "an idle worker is woken");
    assert_eq!(desk.next_job(), Some(wanted.clone()));
    for _ in 0..3 {
        assert!(
            !desk.ask(wanted.clone()),
            "no ask is taken while one is made"
        );
    }
    assert!(!desk.ask(Ask::Far(ArrayVec::new())));
    assert!(desk.deliver(1u8), "the answer is kept");
    assert_eq!(desk.collect(), Some(1));
    assert_eq!(
        desk.next_job(),
        None,
        "a declined ask leaves nothing queued"
    );
}

#[test]
fn an_answer_waiting_is_collected_before_another_ask_is_taken() {
    let mut desk = ChunkDesk::<u8>::new();
    let (first, second) = (
        Ask::Solve(ChunkCoord { x: 0, y: 0 }),
        Ask::Solve(ChunkCoord { x: 1, y: 0 }),
    );
    assert!(desk.ask(first.clone()));
    assert_eq!(desk.next_job(), Some(first));
    assert!(desk.deliver(1));
    assert!(
        !desk.ask(second.clone()),
        "a second answer would replace the first before it is collected"
    );
    assert_eq!(desk.next_job(), None);
    assert_eq!(desk.collect(), Some(1));
    assert!(desk.ask(second.clone()));
    assert_eq!(desk.next_job(), Some(second));
}

#[test]
fn an_ask_not_yet_taken_gives_way_to_the_next() {
    let mut desk = ChunkDesk::<u8>::new();
    let far = Ask::Solve(ChunkCoord { x: 9, y: 9 });
    let near = Ask::Solve(ChunkCoord { x: 0, y: 1 });
    assert!(desk.ask(far));
    assert!(desk.ask(near.clone()));
    assert_eq!(desk.next_job(), Some(near));
    assert_eq!(desk.next_job(), None);
}

#[test]
fn a_stopped_desk_hands_out_nothing() {
    let mut desk = ChunkDesk::<u8>::new();
    desk.stop();
    assert!(desk.stopping());
    assert!(!desk.ask(Ask::Solve(ChunkCoord { x: 0, y: 0 })));
    assert_eq!(desk.next_job(), None);
}

#[test]
fn ground_past_any_distance_is_drawn_at_the_coarsest_stride() {
    assert_eq!(stride_for(i64::MAX, 1 << 20), CHUNK_CELLS);
    assert_eq!(stride_for(0, 1), 1, "underfoot is the finest");
}
