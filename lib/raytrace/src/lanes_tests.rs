use super::*;
use crate::sample::{mix32, unit};
use crate::shape::{reciprocal, Aabb};
use crate::vector::Ray;

fn draw(index: u32, salt: u32) -> f64 {
    unit(mix32(mix32(index) ^ salt))
}

/// A box somewhere in a few kilometres, from a sliver to tens of metres
/// across, some flat in one axis.
fn some_box(index: u32) -> Aabb {
    let at = |salt| 4000.0 * (draw(index, salt) - 0.5);
    let size = |salt| {
        let size = 30.0 * draw(index, salt) * draw(index, salt + 7);
        if draw(index, salt + 13) < 0.1 {
            0.0
        } else {
            size
        }
    };
    let min = Vec3::new(at(1), at(2), at(3));
    Aabb {
        min,
        max: min + Vec3::new(size(4), size(5), size(6)),
    }
}

/// Four boxes laid into lanes, the first in lane 0.
fn laned(boxes: &[Aabb; 4]) -> Corners<Lanes> {
    Corners {
        min: core::array::from_fn(|axis| Lanes(boxes.map(|single| single.min.along(axis)))),
        max: core::array::from_fn(|axis| Lanes(boxes.map(|single| single.max.along(axis)))),
    }
}

fn some_ray(index: u32) -> Ray {
    let origin = Vec3::new(
        3000.0 * (draw(index, 21) - 0.5),
        3000.0 * (draw(index, 22) - 0.5),
        3000.0 * (draw(index, 23) - 0.5),
    );
    // A few rays lie along an axis, where the reciprocal is a sliver.
    let axis = |salt| {
        if draw(index, salt + 9) < 0.05 {
            0.0
        } else {
            draw(index, salt) - 0.5
        }
    };
    let dir = Vec3::new(axis(24), axis(25), axis(26));
    let dir = if dir.length() > 0.0 {
        dir.normalized()
    } else {
        Vec3::new(0.0, 1.0, 0.0)
    };
    Ray::new(origin, dir)
}

#[test]
fn each_lane_crosses_its_box_bit_for_bit_as_one_box_alone_does() {
    for index in 0..4000u32 {
        let ray = some_ray(index);
        let inverse = reciprocal(ray.dir);
        let boxes = [0, 1, 2, 3].map(|lane| some_box(4 * index + lane));
        let (Lanes(enter), Lanes(leave)) = laned(&boxes).padded().crossing(ray.origin, inverse);
        for (lane, single) in boxes.iter().enumerate() {
            let (alone_enter, alone_leave) = single.padded().crossing(&ray, inverse);
            assert_eq!(
                (enter[lane].to_bits(), leave[lane].to_bits()),
                (alone_enter.to_bits(), alone_leave.to_bits()),
                "ray {index}, lane {lane}"
            );
        }
    }
}

#[test]
fn a_ray_through_a_box_crosses_it_and_one_wide_of_it_does_not() {
    let starts = [0.0, 5.0, 10.0, 15.0];
    let lanes = laned(&starts.map(|x| Aabb {
        min: Vec3::new(x, -1.0, -1.0),
        max: Vec3::new(x + 2.0, 1.0, 1.0),
    }));
    let along = Ray::new(Vec3::new(-3.0, 0.0, 0.0), Vec3::new(1.0, 0.0, 0.0));
    let (Lanes(enter), Lanes(leave)) = lanes.padded().crossing(along.origin, reciprocal(along.dir));
    for (lane, x) in starts.into_iter().enumerate() {
        assert!(enter[lane] <= leave[lane], "lane {lane}");
        assert!(
            (enter[lane] - (x + 3.0)).abs() < 1e-6,
            "lane {lane} enters at {}",
            enter[lane]
        );
        assert!(
            (leave[lane] - (x + 5.0)).abs() < 1e-6,
            "lane {lane} leaves at {}",
            leave[lane]
        );
    }
    let wide = Ray::new(Vec3::new(-3.0, 3.0, 0.0), Vec3::new(1.0, 0.0, 0.0));
    let (Lanes(enter), Lanes(leave)) = lanes.padded().crossing(wide.origin, reciprocal(wide.dir));
    assert!((0..4).all(|lane| enter[lane] > leave[lane]));
}
