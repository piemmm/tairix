use super::*;

const RED: Vec3 = Vec3::new(0.8, 0.1, 0.1);
const BLUE: Vec3 = Vec3::new(0.1, 0.1, 0.8);
const GREY: Vec3 = Vec3::new(0.3, 0.3, 0.3);
/// A floorboard's seam: its dark colour, darkened as `plank` darkens it.
const SEAM: Vec3 = Vec3::new(0.035, 0.035, 0.28);

/// A look-up at `p` on a level surface, the pixel's patch `width` across.
fn spot(p: Vec3, width: f64) -> Spot {
    Spot {
        p,
        normal: Vec3::UP,
        height: p.y,
        width,
        mark: 0,
        along: 0.0,
    }
}

/// Points spread over a few metres.
fn points(count: u32) -> impl Iterator<Item = Vec3> {
    (0..count).map(|index| {
        let key = mix32(index ^ 0x77);
        let at = |salt: u32| 8.0 * (unit(mix32(key ^ salt)) - 0.5);
        Vec3::new(at(1), at(2), at(3))
    })
}

fn near(a: Vec3, b: Vec3) -> bool {
    (a - b).length() < 1e-9
}

/// Every channel between the least and the greatest of `colours`', give or
/// take `slack` of the brightest of them.
fn within(colour: Vec3, colours: &[Vec3], slack: f64) -> bool {
    let least = colours
        .iter()
        .fold(Vec3::splat(f64::INFINITY), |low, c| low.min(*c));
    let most = colours.iter().fold(Vec3::ZERO, |high, c| high.max(*c));
    let give = most.max_element() * slack + 1e-9;
    colour.is_finite()
        && [colour.x - least.x, colour.y - least.y, colour.z - least.z]
            .iter()
            .all(|d| *d >= -give)
        && [most.x - colour.x, most.y - colour.y, most.z - colour.z]
            .iter()
            .all(|d| *d >= -give)
}

#[test]
fn a_checkerboard_alternates_up_close_and_blends_far_off() {
    let checker = Pigment::Checker {
        a: RED,
        b: BLUE,
        size: 1.0,
    };
    assert!(near(
        checker.colour(&spot(Vec3::new(0.5, 0.0, 0.5), 1e-3)),
        RED
    ));
    assert!(near(
        checker.colour(&spot(Vec3::new(1.5, 0.0, 0.5), 1e-3)),
        BLUE
    ));
    assert!(near(
        checker.colour(&spot(Vec3::new(-0.5, 0.0, 0.5), 1e-3)),
        BLUE
    ));
    let far = checker.colour(&spot(Vec3::new(0.3, 0.0, 0.7), 40.0));
    assert!(near(far, RED.lerp(BLUE, 0.5)), "{far:?}");
}

/// Every patterned pigment, the colours it is made of, and how far above
/// the brightest of them its shading may lift it.
fn patterns() -> [(Pigment, &'static [Vec3], f64); 9] {
    [
        (
            Pigment::Marble {
                base: RED,
                vein: BLUE,
                scale: 3.0,
                seed: 1,
            },
            &[RED, BLUE],
            0.0,
        ),
        (
            Pigment::Wood {
                light: RED,
                dark: BLUE,
                scale: 6.0,
                seed: 2,
            },
            &[RED, BLUE],
            0.0,
        ),
        (
            Pigment::Tiles {
                a: RED,
                b: BLUE,
                grout: GREY,
                size: 0.5,
                gap: 0.02,
                seed: 3,
            },
            &[RED, BLUE, GREY],
            0.3,
        ),
        (
            Pigment::Bricks {
                a: RED,
                b: BLUE,
                mortar: GREY,
                size: (0.4, 0.15),
                seed: 4,
            },
            &[RED, BLUE, GREY],
            0.3,
        ),
        (
            Pigment::Planks {
                light: RED,
                dark: BLUE,
                width: 0.2,
                length: 2.0,
                seed: 5,
            },
            &[RED, BLUE, SEAM],
            0.3,
        ),
        (
            Pigment::Stones {
                a: RED,
                b: BLUE,
                joint: GREY,
                size: 0.8,
                gap: 0.03,
                seed: 6,
            },
            &[RED, BLUE, GREY],
            0.35,
        ),
        (
            Pigment::Speckle {
                base: RED,
                flecks: [BLUE, GREY],
                scale: 30.0,
                seed: 7,
            },
            &[RED, BLUE, GREY],
            0.1,
        ),
        (
            Pigment::Bark {
                light: RED,
                dark: BLUE,
                scale: 4.0,
                seed: 8,
            },
            &[RED, BLUE],
            0.35,
        ),
        (
            Pigment::Stripes {
                a: RED,
                b: BLUE,
                width: 0.5,
            },
            &[RED, BLUE],
            0.0,
        ),
    ]
}

#[test]
fn every_pattern_keeps_to_its_own_colours() {
    let patterns = patterns();
    for (pigment, colours, slack) in &patterns {
        for p in points(400) {
            for width in [1e-4, 0.05, 3.0] {
                let colour = pigment.colour(&spot(p, width));
                assert!(
                    within(colour, colours, *slack),
                    "{pigment:?} at {p:?}: {colour:?}"
                );
            }
        }
    }
}

#[test]
fn tiles_show_their_grout_between_them_and_blend_with_it_far_off() {
    let tiles = Pigment::Tiles {
        a: RED,
        b: RED,
        grout: GREY,
        size: 1.0,
        gap: 0.1,
        seed: 9,
    };
    assert!(near(
        tiles.colour(&spot(Vec3::new(1.0, 0.0, 0.5), 1e-4)),
        GREY
    ));
    let face = tiles.colour(&spot(Vec3::new(0.5, 0.0, 0.5), 1e-4));
    assert!(face.x > 0.5 && face.z < 0.2, "{face:?}");
    let far = tiles.colour(&spot(Vec3::new(0.5, 0.0, 0.5), 10.0));
    assert!(
        far.x < face.x && far.z > face.z,
        "grout shares the far-off colour: {far:?}"
    );
}

#[test]
fn stripes_band_across_the_texture_y_axis() {
    let stripes = Pigment::Stripes {
        a: RED,
        b: BLUE,
        width: 1.0,
    };
    let at = |y: f64| stripes.colour(&spot(Vec3::new(3.0, y, -2.0), 1e-4));
    assert!(near(at(0.5), at(2.5)));
    assert!(!near(at(0.5), at(1.5)));
    assert!(near(
        at(0.5),
        stripes.colour(&spot(Vec3::new(-7.0, 0.5, 9.0), 1e-4))
    ));
}

#[test]
fn a_crowd_colours_each_member_and_lightens_it_toward_its_tip() {
    let colours = [RED, BLUE, GREY, RED * 0.5];
    let blossoms = [
        Vec3::ONE,
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::new(1.0, 0.0, 1.0),
        Vec3::new(0.0, 1.0, 1.0),
    ];
    let crowd = Pigment::Crowd {
        colours,
        tip: Vec3::new(0.9, 0.9, 0.5),
        blossoms,
    };
    let member = |mark: u32, along: f64| {
        crowd.colour(&Spot {
            mark,
            along,
            ..spot(Vec3::ZERO, 1e-3)
        })
    };
    for mark in 0..64 {
        let (root, tip) = (member(mark, 0.0), member(mark, 1.0));
        assert!(
            tip.max_element() > root.max_element(),
            "mark {mark}: {root:?} to {tip:?}"
        );
    }
    for index in 0..4u32 {
        let flower = member(FLOWER | (index << 3), 1.0);
        assert!(near(flower, blossoms[index as usize]), "{flower:?}");
    }
}

#[test]
fn land_is_sand_by_the_shore_rock_on_cliffs_and_snow_on_high_flat_ground() {
    let land = Land {
        grass: Vec3::new(0.1, 0.5, 0.1),
        dry: Vec3::new(0.1, 0.5, 0.1),
        earth: Vec3::new(0.3, 0.2, 0.1),
        rock: Vec3::new(0.25, 0.25, 0.25),
        strata: Vec3::new(0.25, 0.25, 0.25),
        sand: Vec3::new(0.9, 0.8, 0.5),
        snow: Vec3::ONE,
        shore: 1.0,
        snow_line: 500.0,
        cliff: 0.7,
        scale: 1000.0,
        seed: 3,
    };
    let pigment = Pigment::Land(land);
    let at = |height: f64, normal: Vec3| {
        pigment.colour(&Spot {
            p: Vec3::new(40.0, height, -30.0),
            normal,
            height,
            width: 0.01,
            mark: 0,
            along: 0.0,
        })
    };
    let beach = at(-2.0, Vec3::UP);
    assert!(near(beach, Vec3::new(0.9, 0.8, 0.5)), "{beach:?}");
    let meadow = at(50.0, Vec3::UP);
    assert!(meadow.y > meadow.x && meadow.y > meadow.z, "{meadow:?}");
    let cliff = at(50.0, Vec3::new(0.9, 0.3, 0.0).normalized());
    assert!(
        (cliff.x - cliff.z).abs() < 0.05 && cliff.y < 0.3,
        "{cliff:?}"
    );
    let peak = at(900.0, Vec3::UP);
    assert!(near(peak, Vec3::ONE), "{peak:?}");
    let crag = at(900.0, Vec3::new(0.95, 0.2, 0.0).normalized());
    assert!(
        crag.max_element() < 0.3,
        "snow does not lie on a crag: {crag:?}"
    );
}
