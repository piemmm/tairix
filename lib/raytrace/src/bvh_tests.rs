//! Host tests of the bounding volume hierarchy.

use alloc::vec::Vec;

use super::{Builder, Bvh, Walk, MAX_DEPTH};
use crate::sample::{mix32, unit};
use crate::shape::{reciprocal, Aabb, Geometry, Shape};
use crate::vector::{Ray, Vec3};

/// No hulls' faces and no grids: spheres need neither.
const NOTHING: Geometry<'static> = Geometry {
    faces: &[],
    fields: &[],
    prototypes: &[],
    lawns: &[],
    far_woods: &[],
    stands: &[],
    materials: &[],
    view: None,
};

/// The hierarchy over `bounds`, built a slice at a time as a scene's is.
fn built(bounds: &[(u32, Aabb)]) -> Bvh {
    let mut builder = Builder::new(bounds).expect("room to build");
    while !builder.step(64) {}
    builder.finish()
}

struct Draws(u32);

impl Draws {
    fn next(&mut self) -> f64 {
        self.0 = mix32(self.0.wrapping_add(0x6d2b_79f5));
        unit(self.0)
    }

    fn signed(&mut self) -> f64 {
        self.next() * 2.0 - 1.0
    }
}

/// `count` small spheres scattered through a box `spread` across.
fn spheres(count: u32, spread: f64, seed: u32) -> Vec<Shape> {
    let mut draws = Draws(seed);
    (0..count)
        .map(|_| Shape::Sphere {
            centre: Vec3::new(
                draws.signed() * spread,
                draws.signed() * spread,
                draws.signed() * spread,
            ),
            radius: 0.05 + draws.next() * 0.3,
        })
        .collect()
}

fn boxes(shapes: &[Shape]) -> Vec<(u32, Aabb)> {
    shapes
        .iter()
        .enumerate()
        .map(|(index, shape)| {
            (
                u32::try_from(index).expect("few"),
                shape.bounds(NOTHING).expect("bounded"),
            )
        })
        .collect()
}

fn rays(count: u32, spread: f64, seed: u32) -> Vec<Ray> {
    let mut draws = Draws(seed);
    (0..count)
        .map(|_| {
            let origin = Vec3::new(draws.signed(), draws.signed(), draws.signed()) * (spread * 2.0);
            let target = Vec3::new(draws.signed(), draws.signed(), draws.signed()) * spread;
            Ray::new(origin, (target - origin).normalized())
        })
        .collect()
}

/// The nearest object `ray` meets, found through `bvh`, and how many objects
/// the walk tested.
fn closest(bvh: &Bvh, shapes: &[Shape], ray: &Ray) -> (Option<(usize, f64)>, u32) {
    let mut best = None;
    let mut tested = 0;
    bvh.walk(ray, f64::INFINITY, |object, reach| {
        tested += 1;
        let index = object as usize;
        match shapes[index].intersect(ray, 1e-9, reach, NOTHING) {
            Some(hit) => {
                best = Some((index, hit.t));
                Walk::Within(hit.t)
            }
            None => Walk::Within(reach),
        }
    });
    (best, tested)
}

fn brute_force(shapes: &[Shape], ray: &Ray) -> Option<(usize, f64)> {
    let mut best: Option<(usize, f64)> = None;
    for (index, shape) in shapes.iter().enumerate() {
        let reach = best.map_or(f64::INFINITY, |(_, t)| t);
        if let Some(hit) = shape.intersect(ray, 1e-9, reach, NOTHING) {
            best = Some((index, hit.t));
        }
    }
    best
}

#[test]
fn the_hierarchy_finds_the_same_nearest_object_as_testing_every_one() {
    for (count, seed) in [(1u32, 1u32), (2, 2), (7, 3), (60, 4), (300, 5)] {
        let shapes = spheres(count, 4.0, seed);
        let bvh = built(&boxes(&shapes));
        for ray in rays(500, 4.0, seed ^ 0xabc) {
            let (found, _) = closest(&bvh, &shapes, &ray);
            let expected = brute_force(&shapes, &ray);
            match (found, expected) {
                (Some((a, ta)), Some((b, tb))) => {
                    assert!((ta - tb).abs() < 1e-9, "{count}: {ta} against {tb}");
                    // Two spheres may be met at the same distance only if
                    // they are the same one.
                    assert_eq!(a, b, "{count}");
                }
                (None, None) => {}
                other => panic!("{count}: {other:?}"),
            }
        }
    }
}

#[test]
fn a_walk_that_is_told_to_stop_stops() {
    let shapes = spheres(100, 3.0, 9);
    let bvh = built(&boxes(&shapes));
    for ray in rays(200, 3.0, 10) {
        let mut visits = 0;
        bvh.walk(&ray, f64::INFINITY, |_, _| {
            visits += 1;
            Walk::Stop
        });
        assert!(visits <= 1);
    }
}

/// The hierarchy's whole purpose, measured in the work it saves rather than
/// in seconds: a ray through a crowd of small objects tests a small share of
/// them.
#[test]
fn a_ray_through_many_objects_tests_few_of_them() {
    let shapes = spheres(400, 6.0, 11);
    let bvh = built(&boxes(&shapes));
    let probes = rays(1000, 6.0, 12);
    let tested: u32 = probes.iter().map(|ray| closest(&bvh, &shapes, ray).1).sum();
    let mean = f64::from(tested) / 1000.0;
    assert!(
        mean < 400.0 * 0.12,
        "a ray tested {mean} of 400 objects on average"
    );
}

#[test]
fn an_empty_hierarchy_has_nothing_to_visit() {
    let bvh = built(&[]);
    let mut visits = 0;
    bvh.walk(&rays(1, 1.0, 1)[0], f64::INFINITY, |_, reach| {
        visits += 1;
        Walk::Within(reach)
    });
    assert_eq!(visits, 0);
}

/// Objects all at one place cannot be told apart by any cut, yet every one is
/// still in the tree, in leaves no deeper than the walk's stack allows.
#[test]
fn coincident_objects_are_all_held_and_the_tree_stays_shallow() {
    let shapes: Vec<Shape> = (0..500)
        .map(|_| Shape::Sphere {
            centre: Vec3::new(1.0, 2.0, 3.0),
            radius: 0.5,
        })
        .collect();
    let bvh = built(&boxes(&shapes));
    assert_eq!(bvh.order.len(), 500);
    let mut held: Vec<u32> = bvh.order.clone();
    held.sort_unstable();
    held.dedup();
    assert_eq!(held.len(), 500);
    assert!(depth(&bvh, 0) <= MAX_DEPTH);
    let ray = Ray::new(Vec3::new(1.0, 2.0, -5.0), Vec3::new(0.0, 0.0, 1.0));
    let (found, _) = closest(&bvh, &shapes, &ray);
    assert!(found.is_some_and(|(_, t)| (t - 7.5).abs() < 1e-9));
}

fn depth(bvh: &Bvh, node: usize) -> usize {
    let Some(current) = bvh.nodes.get(node) else {
        return 0;
    };
    if current.count > 0 {
        return 1;
    }
    1 + depth(bvh, node + 1).max(depth(bvh, current.start as usize))
}

/// The walk as one loop over a visitor, run to its end without pausing: what
/// a cursor must hand over, in order, each with the reach it then stands at.
fn walk_unpaused(bvh: &Bvh, ray: &Ray, reach: f64, mut visit: impl FnMut(u32, f64) -> Walk) {
    let inverse = reciprocal(ray.dir);
    let mut reach = reach;
    let entry = |index: usize, reach: f64| {
        bvh.nodes
            .get(index)
            .and_then(|node| node.bounds().entry(ray, inverse, reach))
    };
    if entry(0, reach).is_none() {
        return;
    }
    let mut stack = Vec::new();
    let mut node = 0usize;
    loop {
        let current = bvh.nodes[node];
        let mut next = None;
        if current.count > 0 {
            let start = current.start as usize;
            for &object in &bvh.order[start..start + current.count as usize] {
                match visit(object, reach) {
                    Walk::Within(shorter) => reach = shorter,
                    Walk::Stop => return,
                }
            }
        } else {
            let (first, second) = (node + 1, current.start as usize);
            next = match (entry(first, reach), entry(second, reach)) {
                (Some(a), Some(b)) if b < a => {
                    stack.push((first, a));
                    Some(second)
                }
                (Some(_), Some(b)) => {
                    stack.push((second, b));
                    Some(first)
                }
                (Some(_), None) => Some(first),
                (None, Some(_)) => Some(second),
                (None, None) => None,
            };
        }
        if let Some(next) = next {
            node = next;
            continue;
        }
        loop {
            let Some((deferred, entered)) = stack.pop() else {
                return;
            };
            if entered <= reach {
                node = deferred;
                break;
            }
        }
    }
}

/// Taken an object at a time, the walk hands over what the unpaused loop
/// visits, in its order, each with the same reach: whether a closest query's
/// hits shorten it as they come, or it stays put and every box the ray
/// crosses is visited.
#[test]
fn a_cursor_hands_over_what_an_unpaused_walk_visits() {
    let shapes = spheres(3000, 8.0, 7);
    let bvh = built(&boxes(&shapes));
    let mut visited = 0;
    for shortening in [true, false] {
        for (index, ray) in rays(2000, 8.0, 11).iter().enumerate() {
            let visit = |sink: &mut Vec<(u32, u64)>, object: u32, reach: f64| {
                sink.push((object, reach.to_bits()));
                match shapes[object as usize].intersect(ray, 1e-9, reach, NOTHING) {
                    Some(hit) if shortening => Walk::Within(hit.t),
                    _ => Walk::Within(reach),
                }
            };
            let mut taken = Vec::new();
            bvh.walk(ray, f64::INFINITY, |object, reach| {
                visit(&mut taken, object, reach)
            });
            let mut unpaused = Vec::new();
            walk_unpaused(&bvh, ray, f64::INFINITY, |object, reach| {
                visit(&mut unpaused, object, reach)
            });
            assert_eq!(taken, unpaused, "ray {index}, shortening {shortening}");
            visited += taken.len();
        }
    }
    assert!(visited > 4000, "only {visited} objects visited");
}
