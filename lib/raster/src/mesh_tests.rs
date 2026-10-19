//! The mesh path covers what the centre-sampled converter covers, owns a
//! shared edge once, orders depth, interpolates in perspective, and is total
//! for any vertices.

use alloc::vec;
use alloc::vec::Vec;

use tairix_fuzzseed::Prng;

use super::*;
use crate::scan::{Coverage, FillRule, ScanScratch};

const WIDTH: u32 = 48;
const HEIGHT: u32 = 40;

fn vertex(x: i32, y: i32, depth: u32) -> MeshVertex {
    MeshVertex {
        x,
        y,
        depth: Depth(depth),
    }
}

fn buffer() -> DepthBuffer {
    let mut depth = DepthBuffer::new();
    assert!(depth.reset(WIDTH, HEIGHT));
    depth
}

/// How many times each pixel is handed to a shade, row-major.
fn visits(triangles: &[Triangle], test: DepthTest, rows_per_band: u32) -> Vec<u32> {
    let mut depth = buffer();
    let mut counts = vec![0u32; (WIDTH * HEIGHT) as usize];
    for mut band in depth.bands_mut(rows_per_band) {
        for triangle in triangles {
            triangle.fill(&mut band, test, EdgeRecord::Skip, |fragment| {
                counts[(fragment.y * WIDTH + fragment.x) as usize] += 1;
                true
            });
        }
    }
    counts
}

/// The pixels the centre-sampled converter covers for `polygon`.
fn centre_coverage(polygon: &[(i32, i32)]) -> Vec<u32> {
    let mut scratch = ScanScratch::new();
    let mut covered = vec![0u32; (WIDTH * HEIGHT) as usize];
    let Some(mut rows) = scratch.coverage_rows(
        core::slice::from_ref(&polygon),
        256,
        FillRule::NonZero,
        Coverage::Centre,
    ) else {
        return covered;
    };
    let mut row = vec![0u8; WIDTH as usize];
    for y in 0..HEIGHT {
        rows.row(y, 0, &mut row);
        for (x, &cover) in row.iter().enumerate() {
            covered[(y * WIDTH) as usize + x] = u32::from(cover == u8::MAX);
        }
    }
    covered
}

fn random_vertex(rng: &mut Prng) -> MeshVertex {
    let span = |rng: &mut Prng, pixels: u32| {
        i32::try_from(rng.next_u32() % (pixels * 256 + 1024)).expect("small") - 512
    };
    vertex(
        span(rng, WIDTH),
        span(rng, HEIGHT),
        1 + rng.next_u32() % 1_000_000,
    )
}

#[test]
fn a_triangle_covers_exactly_the_centres_the_converter_covers() {
    let mut rng = Prng::new(0x7121_0001);
    let mut compared = 0;
    for _ in 0..400 {
        let corners = [
            random_vertex(&mut rng),
            random_vertex(&mut rng),
            random_vertex(&mut rng),
        ];
        let Some(triangle) = Triangle::new(corners) else {
            continue;
        };
        let polygon: Vec<(i32, i32)> = corners.iter().map(|v| (v.x, v.y)).collect();
        assert_eq!(
            visits(core::slice::from_ref(&triangle), DepthTest::OPAQUE, HEIGHT),
            centre_coverage(&polygon),
            "{corners:?}"
        );
        compared += 1;
    }
    assert!(compared > 300, "the sweep met real triangles");
}

#[test]
fn triangles_sharing_edges_cover_every_centre_once() {
    const SIDE: u32 = 6;
    let mut rng = Prng::new(0x7121_0002);
    let step = (WIDTH.min(HEIGHT) * 256) / SIDE;
    let mut lattice = Vec::new();
    for row in 0..=SIDE {
        for column in 0..=SIDE {
            let jitter = |rng: &mut Prng, at: u32, on_rim: bool| {
                let base = i32::try_from(at * step).expect("small");
                if on_rim {
                    base
                } else {
                    base + i32::try_from(rng.next_u32() % 400).expect("small") - 200
                }
            };
            let rim = row == 0 || column == 0 || row == SIDE || column == SIDE;
            lattice.push(vertex(
                jitter(&mut rng, column, rim),
                jitter(&mut rng, row, rim),
                1000,
            ));
        }
    }
    let at = |row: u32, column: u32| lattice[(row * (SIDE + 1) + column) as usize];
    let mut triangles = Vec::new();
    for row in 0..SIDE {
        for column in 0..SIDE {
            let (a, b, c, d) = (
                at(row, column),
                at(row, column + 1),
                at(row + 1, column + 1),
                at(row + 1, column),
            );
            triangles.extend(Triangle::new([a, b, c]));
            triangles.extend(Triangle::new([a, c, d]));
        }
    }
    let counts = visits(
        &triangles,
        DepthTest {
            compare: DepthCompare::Always,
            write: false,
        },
        5,
    );
    assert!(counts.iter().all(|&c| c <= 1), "a centre was owned twice");
    let side = i32::try_from(SIDE * step).expect("small");
    let outline = [(0, 0), (side, 0), (side, side), (0, side)];
    assert_eq!(counts, centre_coverage(&outline), "the mesh has a crack");
}

#[test]
fn a_stepped_crossing_is_the_computed_one() {
    let mut rng = Prng::new(0x7121_0003);
    for _ in 0..2000 {
        let pick = |rng: &mut Prng| i64::from(rng.next_u32() % (1 << 24)) - (1 << 23);
        let Some(edge) = edge_between(
            (pick(&mut rng), pick(&mut rng)),
            (pick(&mut rng), pick(&mut rng)),
        ) else {
            continue;
        };
        let first = first_centre_from(edge.top);
        let mut y = first * UNIT + UNIT / 2;
        let mut walk = EdgeWalk::at(&edge, y);
        for _ in 0..64 {
            if y >= edge.bottom {
                break;
            }
            assert_eq!(walk.x(), edge.x_at(y), "{edge:?} at {y}");
            walk.advance();
            y += UNIT;
        }
    }
}

/// Two overlapping triangles, the second nearer, as ids by pixel after
/// drawing in `order`.
fn winners(order: [usize; 2]) -> Vec<u8> {
    let far = Triangle::new([
        vertex(0, 0, 100),
        vertex(40 * 256, 0, 100),
        vertex(0, 30 * 256, 100),
    ])
    .expect("a triangle");
    let near = Triangle::new([
        vertex(10 * 256, 5 * 256, 900),
        vertex(45 * 256, 10 * 256, 900),
        vertex(12 * 256, 38 * 256, 900),
    ])
    .expect("a triangle");
    let shapes = [far, near];
    let mut depth = buffer();
    let mut ids = vec![0u8; (WIDTH * HEIGHT) as usize];
    for mut band in depth.bands_mut(7) {
        for index in order {
            shapes[index].fill(&mut band, DepthTest::OPAQUE, EdgeRecord::Skip, |f| {
                ids[(f.y * WIDTH + f.x) as usize] = u8::try_from(index + 1).expect("small");
                true
            });
        }
    }
    ids
}

#[test]
fn nearer_geometry_wins_whatever_the_order() {
    let forward = winners([0, 1]);
    assert_eq!(forward, winners([1, 0]));
    assert!(forward.contains(&1) && forward.contains(&2));
    // Inside both, the nearer one is what was kept.
    assert_eq!(forward[(20 * WIDTH + 15) as usize], 2);
}

#[test]
fn compare_and_write_do_what_they_say() {
    let triangle = Triangle::new([
        vertex(0, 0, 500),
        vertex(30 * 256, 0, 500),
        vertex(0, 30 * 256, 500),
    ])
    .expect("a triangle");
    let mut depth = buffer();
    let mut passed = 0;
    for mut band in depth.bands_mut(HEIGHT) {
        let read_only = DepthTest {
            compare: DepthCompare::Nearer,
            write: false,
        };
        triangle.fill(&mut band, read_only, EdgeRecord::Skip, |_| true);
        assert!(
            band.row(3).iter().all(|d| *d == Depth::CLEAR),
            "nothing written"
        );
        triangle.fill(&mut band, DepthTest::OPAQUE, EdgeRecord::Skip, |_| true);
        triangle.fill(&mut band, DepthTest::OPAQUE, EdgeRecord::Skip, |_| {
            passed += 1;
            true
        });
        assert_eq!(passed, 0, "an equal depth is not nearer");
        let level = DepthTest {
            compare: DepthCompare::NearerOrEqual,
            write: false,
        };
        triangle.fill(&mut band, level, EdgeRecord::Skip, |_| {
            passed += 1;
            true
        });
        assert!(passed > 0, "an equal depth is level");
        // A fragment its shade discards writes nothing even where it passes.
        let discarded = Triangle::new([
            vertex(0, 0, 4000),
            vertex(30 * 256, 0, 4000),
            vertex(0, 30 * 256, 4000),
        ])
        .expect("a triangle");
        discarded.fill(&mut band, DepthTest::OPAQUE, EdgeRecord::Skip, |_| false);
        assert!(band.row(3).iter().take(10).all(|d| d.0 == 500));
    }
}

#[test]
fn weights_sum_to_one_and_meet_each_vertex() {
    let corners = [
        vertex(2 * 256 + 128, 256 + 128, 9000),
        vertex(40 * 256 + 128, 6 * 256 + 128, 3000),
        vertex(8 * 256 + 128, 36 * 256 + 128, 700),
    ];
    let triangle = Triangle::new(corners).expect("a triangle");
    let mut depth = buffer();
    let mut seen = Vec::new();
    for mut band in depth.bands_mut(HEIGHT) {
        triangle.fill(&mut band, DepthTest::OPAQUE, EdgeRecord::Skip, |f| {
            assert_eq!(f.weights.0.iter().sum::<u32>(), WEIGHT_ONE);
            seen.push(f);
            true
        });
    }
    // A corner is a point and covers no centre of its own, but the covered
    // pixel nearest it is mostly that vertex: perspective makes a far
    // corner's share fall quickly, so "mostly" is the heaviest share.
    for (index, (x, y)) in [(2i64, 1i64), (40, 6), (8, 36)].into_iter().enumerate() {
        let nearest = seen
            .iter()
            .min_by_key(|f| (i64::from(f.x) - x).pow(2) + (i64::from(f.y) - y).pow(2))
            .expect("the triangle covers pixels");
        let share = nearest.weights.0[index];
        assert!(
            share > WEIGHT_ONE / 2 && nearest.weights.0.iter().all(|&w| w <= share),
            "corner {index}: {nearest:?}"
        );
    }
}

#[test]
fn weights_are_perspective_correct_rather_than_affine() {
    // A floor at y = 3 receding from a pinhole of focal length 40 centred on
    // (24, 4): its near edge at z = 2 lies on row 64, its far corner at z = 40
    // on row 7, and every vertex lands on a whole sub-unit.
    let (focal, cx, cy) = (40.0, 24.0, 4.0);
    let reach = 16_777_216.0;
    let project = |(x, z): (f64, f64)| {
        vertex(
            tairix_util::mathf::round_i32((cx + focal * x / z) * 256.0),
            tairix_util::mathf::round_i32((cy + focal * 3.0 / z) * 256.0),
            u32::try_from(tairix_util::mathf::round_i32(reach / z)).expect("positive"),
        )
    };
    let triangle =
        Triangle::new([(-6.0, 2.0), (6.0, 2.0), (0.0, 40.0)].map(project)).expect("a triangle");
    let attribute = [0i32, 0, 1_000_000];
    let (near_row, far_row) = (cy + focal * 3.0 / 2.0, cy + focal * 3.0 / 40.0);
    let mut depth = buffer();
    let (mut worst_true, mut worst_affine) = (0.0f64, 0.0f64);
    for mut band in depth.bands_mut(HEIGHT) {
        triangle.fill(&mut band, DepthTest::OPAQUE, EdgeRecord::Skip, |f| {
            let got = f64::from(f.weights.interpolate(attribute));
            let row = f64::from(f.y) + 0.5;
            let z = 3.0 * focal / (row - cy);
            let truth = (z - 2.0) / 38.0 * 1_000_000.0;
            let affine = (near_row - row) / (near_row - far_row) * 1_000_000.0;
            worst_true =
                tairix_util::mathf::fmax(worst_true, tairix_util::mathf::fabs(got - truth));
            worst_affine =
                tairix_util::mathf::fmax(worst_affine, tairix_util::mathf::fabs(got - affine));
            true
        });
    }
    assert!(worst_true < 2_000.0, "off the true surface by {worst_true}");
    assert!(
        worst_affine > 100_000.0,
        "only affine, {worst_affine} from it"
    );
}

#[test]
fn a_banded_fill_is_the_whole_fill() {
    let mut rng = Prng::new(0x7121_0004);
    let triangles: Vec<Triangle> = (0..60)
        .filter_map(|_| {
            Triangle::new([
                random_vertex(&mut rng),
                random_vertex(&mut rng),
                random_vertex(&mut rng),
            ])
        })
        .collect();
    let draw = |rows: u32| {
        let mut depth = buffer();
        let mut records = vec![(0u32, 0u8, [0u32; 3]); (WIDTH * HEIGHT) as usize];
        for mut band in depth.bands_mut(rows) {
            for triangle in &triangles {
                triangle.fill(&mut band, DepthTest::OPAQUE, EdgeRecord::Keep, |f| {
                    records[(f.y * WIDTH + f.x) as usize] =
                        (f.depth.0, f.edge.to_bits(), f.weights.0);
                    true
                });
            }
        }
        records
    };
    let whole = draw(HEIGHT);
    for rows in [1, 3, 7, 16] {
        assert_eq!(draw(rows), whole, "{rows} rows a band");
    }
}

#[test]
fn an_edge_records_its_side_and_distance() {
    // A vertical left edge at x = 10.25 px and a level bottom at y = 30.
    let triangle = Triangle::new([
        vertex(2624, 512, 100),
        vertex(2624, 7680, 100),
        vertex(10240, 7680, 100),
    ])
    .expect("a triangle");
    let mut depth = buffer();
    let mut edges = vec![EdgeNear::NONE; (WIDTH * HEIGHT) as usize];
    for mut band in depth.bands_mut(HEIGHT) {
        triangle.fill(&mut band, DepthTest::OPAQUE, EdgeRecord::Keep, |f| {
            edges[(f.y * WIDTH + f.x) as usize] = f.edge;
            true
        });
    }
    let at = |x: u32, y: u32| edges[(y * WIDTH + x) as usize].get();
    assert_eq!(
        at(10, 15),
        Some((16, Toward::Left)),
        "a quarter pixel to the left"
    );
    assert_eq!(at(20, 29), Some((32, Toward::Down)), "half a pixel below");
    assert_eq!(at(14, 25), None, "nowhere near an edge");
}

#[test]
fn hostile_vertices_draw_nothing_or_stay_inside() {
    assert!(Triangle::new([vertex(0, 0, 1), vertex(i32::MAX, 0, 1), vertex(0, 9, 1)]).is_none());
    assert!(Triangle::new([vertex(0, 0, 1), vertex(5, 5, 1), vertex(10, 10, 1)]).is_none());
    assert!(Triangle::new([vertex(0, 0, 0), vertex(900, 0, 1), vertex(0, 900, 1)]).is_none());
    let mut rng = Prng::new(0x7121_0005);
    let limit = i32::try_from(COORD_LIMIT).expect("within i32");
    for _ in 0..500 {
        let wild = |rng: &mut Prng| {
            let raw = i32::from_ne_bytes(rng.next_u32().to_ne_bytes());
            raw.clamp(-limit, limit)
        };
        let corners = [0; 3].map(|_| vertex(wild(&mut rng), wild(&mut rng), rng.next_u32().max(1)));
        let Some(triangle) = Triangle::new(corners) else {
            continue;
        };
        let mut depth = buffer();
        for mut band in depth.bands_mut(9) {
            triangle.fill(&mut band, DepthTest::OPAQUE, EdgeRecord::Keep, |f| {
                assert!(f.x < WIDTH && f.y < HEIGHT);
                true
            });
        }
    }
}

#[test]
fn a_depth_plane_answers_its_points_and_occludes() {
    let plane = DepthPlane::through([(0.5, 0.5, 100.0), (10.5, 0.5, 200.0), (0.5, 10.5, 100.0)])
        .expect("a plane");
    assert_eq!(plane.at(0, 0), Depth(100));
    assert_eq!(plane.at(10, 0), Depth(200));
    assert_eq!(plane.at(5, 7), Depth(150));
    assert!(DepthPlane::through([(0.0, 0.0, 1.0), (1.0, 1.0, 1.0), (2.0, 2.0, 1.0)]).is_none());
    let mut depth = buffer();
    for mut band in depth.bands_mut(HEIGHT) {
        Triangle::new([
            vertex(0, 0, 150),
            vertex(20 * 256, 0, 150),
            vertex(0, 20 * 256, 150),
        ])
        .expect("a triangle")
        .fill(&mut band, DepthTest::OPAQUE, EdgeRecord::Skip, |_| true);
    }
    let occlusion = Occlusion {
        depth: &depth,
        plane,
        compare: DepthCompare::Nearer,
    };
    assert!(!occlusion.shows(1, 1), "behind what is drawn");
    assert!(occlusion.shows(9, 1), "in front of it");
    assert!(occlusion.shows(30, 30), "nothing is drawn there");
    assert_eq!(DepthPlane::flat(Depth(77)).at(3, 9), Depth(77));
}

#[test]
fn an_occluded_fill_shows_only_in_front_and_is_the_plain_fill_there() {
    use crate::color::Color;
    use crate::surface::{Canvas, Surface};

    // The left half of the buffer is drawn near, the right half not at all.
    let mut depth = buffer();
    for mut band in depth.bands_mut(HEIGHT) {
        for corners in [
            [
                vertex(0, 0, 900),
                vertex(24 * 256, 0, 900),
                vertex(0, 40 * 256, 900),
            ],
            [
                vertex(24 * 256, 0, 900),
                vertex(24 * 256, 40 * 256, 900),
                vertex(0, 40 * 256, 900),
            ],
        ] {
            Triangle::new(corners).expect("a triangle").fill(
                &mut band,
                DepthTest::OPAQUE,
                EdgeRecord::Skip,
                |_| true,
            );
        }
    }
    let disc: Vec<(i32, i32)> = (0..24)
        .map(|step| {
            let angle = f64::from(step) * core::f64::consts::TAU / 24.0;
            (
                tairix_util::mathf::round_i32((24.0 + 14.0 * tairix_util::mathf::cos(angle)) * 8.0),
                tairix_util::mathf::round_i32((20.0 + 14.0 * tairix_util::mathf::sin(angle)) * 8.0),
            )
        })
        .collect();
    let ink = Color::rgb(200, 30, 40);
    let behind = Occlusion {
        depth: &depth,
        plane: DepthPlane::flat(Depth(500)),
        compare: DepthCompare::Nearer,
    };
    let mut plain = Surface::new(WIDTH, HEIGHT).expect("a surface");
    Canvas::fill_polygon_subpixel(&mut plain, &disc, ink, &mut ScanScratch::new());
    let mut whole = Surface::new(WIDTH, HEIGHT).expect("a surface");
    whole.fill_polygon_subpixel_occluded(&disc, ink, &behind, &mut ScanScratch::new());
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let expected = if x < 24 {
                Some(Color::TRANSPARENT.premultiply())
            } else {
                plain.get(x, y)
            };
            assert_eq!(whole.get(x, y), expected, "({x}, {y})");
        }
    }
    let mut banded = Surface::new(WIDTH, HEIGHT).expect("a surface");
    for mut band in banded.row_bands_mut(0..HEIGHT, 6) {
        band.fill_polygon_subpixel_occluded(&disc, ink, &behind, &mut ScanScratch::new());
    }
    assert_eq!(
        banded.pixels(),
        whole.pixels(),
        "a band shows what the whole does"
    );
}
