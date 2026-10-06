//! Host tests of masonry as laid: a wall's faces covered and its openings
//! open, an arch's ring over its opening, its jambs quoined, bricks in their
//! bond, columns in drums, and cover coming with age.

use alloc::vec::Vec;

use tairix_rng::NonCryptoRng;

use super::*;
use crate::detail::Detail;
use crate::prototype::{Building, Prototype};
use crate::vector::Ray;

fn stage() -> Stage {
    Stage::new(Detail::Simple.densities()).expect("a stage")
}

fn dice() -> Dice {
    Dice(NonCryptoRng::seed_from_u64(17))
}

fn weathering() -> Weathering {
    Weathering {
        damp: 0.5,
        drought: 0.3,
        foot: 0.3,
    }
}

fn stone(stage: &mut Stage, age: f64) -> Stonework {
    stage
        .stonework(&mut dice(), Quarry::Sandstone, (age, weathering()))
        .expect("stone")
}

fn built(mason: Mason) -> Prototype {
    Building::whole(mason.finish().expect("a structure"))
}

const OPENING: Opening = Opening {
    at: 0.0,
    span: 2.0,
    rise: 1.0,
    springing: 1.5,
    ring: 0.35,
};

fn arcade(stage: &mut Stage, brick: Option<Bond>) -> (Prototype, Stonework) {
    let work = match brick {
        Some(_) => stage
            .brickwork(&mut dice(), Clay::Red, (0.5, weathering()))
            .expect("bricks"),
        None => stone(stage, 0.5),
    };
    let mut mason = Mason::new(work, 3).expect("a mason");
    let openings = [OPENING];
    let wall = Wall {
        pose: Pose::new(Vec3::ZERO, Frame::WORLD),
        length: 5.0,
        height: 3.4,
        thickness: 0.6,
        dressing: Dressing::Ashlar,
        rise: (0.3, 0.4),
        long: (1.2, 2.4),
        joint: 0.006,
        openings: &openings,
        back: true,
    };
    let ring = Ring {
        centre: Vec3::new(0.0, OPENING.springing, 0.0),
        wall: wall.pose,
        span: OPENING.span,
        rise: OPENING.rise,
        depth: OPENING.ring,
        through: (-0.3, 0.3),
        count: 15,
        dressing: Dressing::Ashlar,
        joint: 0.006,
    };
    if let Some(bond) = brick {
        mason.bricks(&wall, bond).expect("bricks");
        mason.rowlocks(&ring).expect("rowlocks");
    } else {
        mason.wall(&wall).expect("a wall");
        mason.arch(&ring).expect("an arch");
    }
    (built(mason), work)
}

fn first(structure: &Prototype, from: Vec3, way: Vec3) -> Option<crate::shape::Hit> {
    structure.intersect(&Ray::new(from, way), (0.0, 10.0), None)
}

#[test]
fn a_walls_faces_are_covered_and_its_opening_open() {
    for brick in [None, Some(Bond::Flemish)] {
        let mut stage = stage();
        let (structure, _) = arcade(&mut stage, brick);
        for index in 0..400u32 {
            let x = -2.4 + 4.8 * crate::sample::unit(crate::sample::mix32(index));
            let y = 0.05 + 3.3 * crate::sample::unit(crate::sample::mix32(index ^ 9));
            let hit = first(&structure, Vec3::new(x, y, 2.0), Vec3::new(0.0, 0.0, -1.0));
            let (outer, middle) = OPENING.extrados();
            let inner = outer - OPENING.ring;
            let from_arch = mathf::sqrt(x * x + (y - middle) * (y - middle));
            let inside = (y < OPENING.springing && x.abs() < 0.5 * OPENING.span)
                || (y >= OPENING.springing && from_arch < inner);
            // Within a joint's breadth of the opening's edge either may hold.
            let edge = (y < OPENING.springing && (x.abs() - 0.5 * OPENING.span).abs() < 0.02)
                || (y >= OPENING.springing && (from_arch - inner).abs() < 0.02);
            if edge {
                continue;
            }
            match hit {
                Some(_) => assert!(!inside, "{brick:?}: met at {x}, {y}, within the opening"),
                None => assert!(inside, "{brick:?}: a ray through the wall at {x}, {y}"),
            }
            // No deeper than a chip bites or a joint is pointed.
            if let Some(hit) = hit {
                assert!(
                    hit.t < 2.0 - 0.3 + 0.06,
                    "{brick:?}: a face sunk at {x}, {y}"
                );
            }
        }
    }
}

#[test]
fn an_arch_rings_its_opening_and_its_jambs_are_quoined() {
    for brick in [None, Some(Bond::English)] {
        let mut stage = stage();
        let (structure, work) = arcade(&mut stage, brick);
        // Up through the opening, its ring's soffit at the crown, a joint's
        // pointing no deeper than its recess.
        let crown = first(&structure, Vec3::new(0.0, 0.5, 0.1), Vec3::UP).expect("the soffit");
        let soffit = 0.5 + crown.t;
        assert!(
            (soffit - (OPENING.springing + OPENING.rise)).abs() < 0.04,
            "{brick:?}: the soffit at {soffit}"
        );
        // Across the opening into a jamb, below the springing: the reveal's
        // own units, its joints between them, never the core laid bare.
        let (mut faced, mut looked) = (0, 0);
        let mut misses = Vec::new();
        for index in 0..40u32 {
            let y = 0.05 + 1.4 * f64::from(index) / 40.0;
            let Some(jamb) = first(
                &structure,
                Vec3::new(0.0, y, 0.05),
                Vec3::new(1.0, 0.0, 0.0),
            ) else {
                panic!("{brick:?}: no jamb at {y}");
            };
            assert!(
                jamb.t < 0.5 * OPENING.span + 0.05,
                "{brick:?}: the reveal sunk at {y}"
            );
            looked += 1;
            if jamb.material == Some(u32::from(work.stone)) {
                faced += 1;
            } else {
                misses.push((y, jamb.t));
            }
        }
        assert!(
            faced * 4 > looked * 3,
            "{brick:?}: {faced} of {looked} of the reveal faced; mortar at {misses:?}"
        );
    }
}

#[test]
fn bricks_keep_their_bond_and_the_lost_are_few() {
    let mut stage = stage();
    let work = stage
        .brickwork(&mut dice(), Clay::Stock, (0.1, weathering()))
        .expect("bricks");
    let mut mason = Mason::new(work, 8).expect("a mason");
    let wall = Wall {
        pose: Pose::new(Vec3::ZERO, Frame::WORLD),
        length: 4.0,
        height: 1.5,
        thickness: 0.215,
        dressing: Dressing::Brick,
        rise: (0.075, 0.075),
        long: (1.0, 1.0),
        joint: 0.01,
        openings: &[],
        back: true,
    };
    mason.bricks(&wall, Bond::English).expect("bricks");
    let structure = built(mason);
    let pitch = BRICK.2 + BRICK.3;
    let mut courses = [(0u32, 0u32); 24];
    for part in structure.parts() {
        let crate::prototype::Part::Solid(solid) = part else {
            continue;
        };
        if solid.material() != work.stone {
            continue;
        }
        let course = mathf::round_i32(mathf::floor(solid.centre().y / pitch));
        if let Some(count) = usize::try_from(course)
            .ok()
            .and_then(|course| courses.get_mut(course))
        {
            // A header's length runs through the wall.
            if solid.frame().x.z.abs() > 0.9 {
                count.0 += 1;
            } else {
                count.1 += 1;
            }
        }
    }
    for (index, &(headers, stretchers)) in courses.iter().enumerate().take(20) {
        // Headers close each course's ends; between them its bond rules.
        if index % 2 == 1 {
            assert!(
                headers > 2 * stretchers,
                "course {index}: {headers} headers, {stretchers} stretchers"
            );
        } else {
            assert!(
                stretchers > headers,
                "course {index}: {headers} headers, {stretchers} stretchers"
            );
        }
    }
}

#[test]
fn a_column_rises_in_drums_to_its_shaft_or_breaks_off() {
    for order in Order::ALL {
        let mut stage = stage();
        let mut mason = Mason::new(stone(&mut stage, 0.4), 2).expect("a mason");
        let column = Column {
            foot: Vec3::ZERO,
            radius: 0.3,
            height: 4.0,
            order,
            yaw: 0.0,
            broken: None,
        };
        let top = mason.column(&column).expect("a column");
        assert!(top > 4.0 && top < 4.8, "{order:?}: its top at {top}");
        let broken = mason
            .column(&Column {
                broken: Some(0.4),
                foot: Vec3::new(3.0, 0.0, 0.0),
                ..column
            })
            .expect("a broken column");
        let standing = match order {
            Order::Doric => 0.0,
            Order::Ionic => 2.0 * 0.16 * 0.3 + 0.12 * 0.3 + 0.1 * 0.3 + 0.1 * 0.3,
            Order::Tuscan => 2.0 * 0.22 * 0.3 + 0.12 * 0.3,
        };
        assert!(
            (broken - (standing + 1.6)).abs() < 1e-6,
            "{order:?}: broken off at {broken}"
        );
        let structure = built(mason);
        let (_, flutes, _) = order.shaft();
        let drums: Vec<_> = structure
            .parts()
            .iter()
            .filter_map(|part| match part {
                crate::prototype::Part::Solid(solid) => match solid.form() {
                    Form::Drum { flutes: cut, .. } => Some(cut),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert!(
            drums.len() >= 4 && drums.iter().all(|&cut| cut == flutes),
            "{order:?}: {drums:?}"
        );
    }
}

#[test]
fn moss_and_lichen_come_with_age() {
    let amounts = |age: f64| {
        let mut stage = stage();
        let work = stone(&mut stage, age);
        work.cover
            .and_then(|cover| stage.materials.get(usize::from(cover)).cloned())
            .map_or((0.0, 0.0), |material| match material.pigment {
                Pigment::Cover(cover) => (cover.moss, cover.lichen),
                _ => (0.0, 0.0),
            })
    };
    let (young, old) = (amounts(0.15), amounts(0.95));
    assert!(
        old.0 > young.0 && old.1 > 3.0 * young.1,
        "{young:?} then {old:?}"
    );
}
