use alloc::vec::Vec;

use super::*;
use crate::bark::{Bark, BarkKind};
use crate::grass::marks;
use crate::ground::{Floor, Palette};

const RED: Vec3 = Vec3::new(0.8, 0.1, 0.1);
const BLUE: Vec3 = Vec3::new(0.1, 0.1, 0.8);
const GREY: Vec3 = Vec3::new(0.3, 0.3, 0.3);
/// A floorboard's seam: its dark colour, darkened as `plank` darkens it.
const SEAM: Vec3 = Vec3::new(0.035, 0.035, 0.28);

/// A look-up at `p` on a level surface, the pixel's patch `width` across,
/// its surface coordinates running with the point.
fn spot(p: Vec3, width: f64) -> Spot {
    Spot {
        p,
        normal: Vec3::UP,
        height: p.y,
        width,
        mark: 0,
        along: 0.0,
        uv: (p.y, p.x),
        girth: 0.0,
        instance: 0,
        front: true,
        ground: [0.0; CHANNELS],
        thatch: 0.0,
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
#[allow(
    clippy::too_many_lines,
    reason = "a table of the patterned pigments, one entry each"
)]
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
            Pigment::Bark(Bark {
                kind: BarkKind::Furrowed,
                light: RED,
                dark: BLUE,
                accent: GREY,
                rise: 0.0,
                snow: 0.0,
                moss: 0.0,
                bare: 0.0,
                seed: 8,
            }),
            &[RED, BLUE, GREY],
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
fn a_crowd_colours_each_member_by_its_kind_and_dries_it_toward_its_tip() {
    let tip = Vec3::new(0.9, 0.9, 0.5);
    let head = Vec3::new(0.6, 0.4, 0.2);
    let grasses = [RED, BLUE, GREY, RED * 0.5].map(|leaf| Blades {
        leaves: [leaf, leaf * 0.9],
        tip,
        head,
    });
    let blossoms = [
        Vec3::ONE,
        Vec3::new(1.0, 1.0, 0.0),
        Vec3::new(1.0, 0.0, 1.0),
        Vec3::new(0.0, 1.0, 1.0),
    ];
    let crowd = Pigment::Crowd(Crowd {
        grasses,
        blossoms,
        weeds: [GREY, BLUE],
        fallen: [RED; 4],
    });
    let member = |mark: u32, along: f64| {
        crowd.colour(&Spot {
            mark,
            along,
            ..spot(Vec3::ZERO, 1e-3)
        })
    };
    let apart = |a: Vec3, b: Vec3| (a - b).length();
    for key in 0..64 {
        // A leaf of the blue kind is blue at its root, whatever its key.
        let root = member(key | marks(1, 15), 0.0);
        assert!(root.z > 2.0 * root.x.max(root.y), "key {key}: {root:?}");
        // And straw at its tip.
        let (low, high) = (
            member(key | marks(1, 15), 0.2),
            member(key | marks(1, 15), 1.0),
        );
        assert!(
            apart(high, tip) < apart(low, tip),
            "key {key}: {low:?} to {high:?}"
        );
        // A thin sward dries further down its leaves than a rank one.
        let (thin, rank) = (
            member(key | marks(1, 0), 0.5),
            member(key | marks(1, 15), 0.5),
        );
        assert!(
            apart(thin, tip) < apart(rank, tip),
            "key {key}: {thin:?} against {rank:?}"
        );
        // A seed head is its kind's head colour, whatever kind it is.
        let seeded = member(key | marks(2, 8) | HEAD, 0.5);
        assert!(
            apart(
                seeded * (1.0 / seeded.max_element()),
                head * (1.0 / head.max_element())
            ) < 1e-9
        );
    }
    for index in 0..4u32 {
        let flower = member(FLOWER | (index << 3), 1.0);
        assert!(near(flower, blossoms[index as usize]), "{flower:?}");
    }
    // A fallen leaf browns to the dark of rot as it decays.
    let (fresh, rotten) = (member(LITTER | 8, 0.0), member(LITTER | 8, 1.0));
    assert!(fresh.x > 2.0 * rotten.x, "{fresh:?} rots to {rotten:?}");
    assert!(
        member(WEED | 2, 0.5).z < member(WEED | 3, 0.5).z,
        "a weed in one of its two colours"
    );
}

/// A ground of plain, flat colours, so what shows where is plain to see.
fn ground() -> Pigment {
    Pigment::Ground(Ground {
        palette: Palette {
            grass: Vec3::new(0.1, 0.5, 0.1),
            dry: Vec3::new(0.1, 0.5, 0.1),
            moss: Vec3::new(0.1, 0.3, 0.1),
            earth: Vec3::new(0.3, 0.2, 0.1),
            silt: Vec3::new(0.5, 0.4, 0.3),
            rock: Vec3::new(0.25, 0.25, 0.25),
            strata: Vec3::new(0.25, 0.25, 0.25),
            lichen: Vec3::new(0.25, 0.25, 0.25),
            sand: Vec3::new(0.9, 0.8, 0.5),
            snow: Vec3::ONE,
        },
        shore: 1.0,
        snow_line: 500.0,
        cliff: 0.7,
        bedding: 3.0,
        seed: 3,
        road: None,
        floor: None,
    })
}

/// The ground at `height` facing `normal`, the land there as `lie` says.
fn ground_at(pigment: &Pigment, height: f64, normal: Vec3, lie: [f64; CHANNELS]) -> Vec3 {
    pigment.colour(&Spot {
        p: Vec3::new(40.0, height, -30.0),
        normal,
        height,
        width: 0.01,
        mark: 0,
        along: 0.0,
        uv: (0.0, 0.0),
        girth: 0.0,
        instance: 0,
        front: true,
        ground: lie,
        thatch: 0.0,
    })
}

#[test]
fn ground_is_sand_by_the_shore_rock_on_cliffs_and_snow_on_high_flat_ground() {
    let pigment = ground();
    let green = [0.0, 0.5, 0.0, 1.0, 0.0];
    let beach = ground_at(&pigment, -2.0, Vec3::UP, green);
    // Its grains lighten or darken it, never change its colour.
    let hue = |colour: Vec3| colour * (1.0 / colour.luminance());
    assert!(
        (hue(beach) - hue(Vec3::new(0.9, 0.8, 0.5))).length() < 0.12,
        "{beach:?}"
    );
    let meadow = ground_at(&pigment, 50.0, Vec3::UP, green);
    assert!(meadow.y > meadow.x && meadow.y > meadow.z, "{meadow:?}");
    let cliff = ground_at(&pigment, 50.0, Vec3::new(0.9, 0.3, 0.0).normalized(), green);
    assert!(
        (cliff.x - cliff.z).abs() < 0.05 && cliff.y < 0.3,
        "{cliff:?}"
    );
    let peak = ground_at(&pigment, 900.0, Vec3::UP, green);
    assert!((peak - Vec3::ONE).length() < 0.1, "{peak:?}");
    let crag = ground_at(
        &pigment,
        900.0,
        Vec3::new(0.95, 0.2, 0.0).normalized(),
        green,
    );
    assert!(
        crag.max_element() < 0.3,
        "snow does not lie on a crag: {crag:?}"
    );
}

#[test]
fn nothing_grows_where_the_land_says_it_cannot_and_a_path_is_trodden_bare() {
    let pigment = ground();
    let barren = ground_at(&pigment, 50.0, Vec3::UP, [0.0, 0.5, 0.0, 0.0, 0.0]);
    assert!(
        barren.x > barren.y * 0.5 && barren.y < 0.3,
        "bare earth: {barren:?}"
    );
    let silted = ground_at(&pigment, 50.0, Vec3::UP, [0.6, 1.0, 0.0, 0.0, 0.0]);
    assert!(
        silted.x > barren.x,
        "fresh silt is paler than earth: {silted:?}"
    );
    let path = ground_at(
        &pigment,
        50.0,
        Vec3::UP,
        [0.0, 0.5, 100.0 / 255.0, 1.0, 0.0],
    );
    let meadow = ground_at(&pigment, 50.0, Vec3::UP, [0.0, 0.5, 0.0, 1.0, 0.0]);
    assert!(
        path.y < meadow.y && path.x > meadow.x,
        "{path:?} beside {meadow:?}"
    );
}

/// Ground carrying no snow shows none, however the broad patches that move
/// a snowy land's bare edges lie across it.
#[test]
fn ground_bare_of_snow_shows_none_wherever_its_patches_lie() {
    let pigment = ground();
    let bare = [0.0, 0.5, 0.0, 1.0, 0.0];
    for step in 0..400u32 {
        let p = Vec3::new(f64::from(step) * 7.3, 50.0, f64::from(step % 37) * 11.9);
        let colour = pigment.colour(&Spot {
            p,
            normal: Vec3::UP,
            height: 50.0,
            width: 0.01,
            mark: 0,
            along: 0.0,
            uv: (0.0, 0.0),
            girth: 0.0,
            instance: 0,
            front: true,
            ground: bare,
            thatch: 0.0,
        });
        assert!(
            colour.luminance() < 0.6,
            "{step}: snow at {p:?}: {colour:?}"
        );
    }
}

/// Snow the wind laid hides the ground beneath it only where it lies deep
/// enough to: below any snow line, a drift is snow and a scoured patch shows
/// the grass the wind bared.
#[test]
fn drifted_snow_hides_the_ground_only_where_it_lies_deep_enough() {
    let pigment = ground();
    let lying = |depth: f64| {
        let kept = f64::from(crate::snow::kept(depth)) / 255.0;
        ground_at(&pigment, 50.0, Vec3::UP, [0.0, 0.5, 0.0, 1.0, kept])
    };
    let (bared, crusted, drifted) = (lying(0.0), lying(0.02), lying(0.4));
    assert!(
        bared.y > bared.x && bared.luminance() < 0.6,
        "grass: {bared:?}"
    );
    assert!(
        crusted.luminance() < 0.6 * drifted.luminance(),
        "a crust shows the ground: {crusted:?}"
    );
    assert!(drifted.luminance() > 0.9, "a drift is snow: {drifted:?}");
}

/// Under a closed wood the ground is the leaves it shed and the moss among
/// them, brown where the open meadow beside it is green.
#[test]
fn a_woods_floor_is_its_fallen_leaves_where_the_open_ground_is_grass() {
    let crowns: Vec<crate::shade::Crown> = (-20..=20)
        .flat_map(|row| {
            (-20..=0).map(move |column| ((f64::from(column) * 5.0, f64::from(row) * 5.0), 4.5))
        })
        .collect();
    let shades = crate::shade::Shades::of(&crowns, ((0.0, 0.0), 300.0), (0.0, 0.0)).expect("held");
    let leaves = [Vec3::new(0.45, 0.25, 0.1), Vec3::new(0.3, 0.18, 0.08)];
    let Pigment::Ground(plain) = ground() else {
        panic!("a ground");
    };
    let floored = Pigment::Ground(Ground {
        floor: Some(Floor {
            shades,
            leaves,
            humus: Vec3::new(0.12, 0.08, 0.05),
            moss: 0.0,
        }),
        ..plain
    });
    let at = |x: f64| {
        floored.colour(&Spot {
            p: Vec3::new(x, 50.0, 0.0),
            normal: Vec3::UP,
            height: 50.0,
            width: 0.01,
            mark: 0,
            along: 0.0,
            uv: (0.0, 0.0),
            girth: 0.0,
            instance: 0,
            front: true,
            ground: [0.0, 0.5, 0.0, 1.0, 0.0],
            thatch: 0.0,
        })
    };
    let (under, open) = (at(-50.0), at(60.0));
    assert!(
        under.x > under.y && under.y > under.z,
        "leaf-brown under the wood: {under:?}"
    );
    assert!(
        open.y > open.x && open.y > open.z,
        "grass in the open: {open:?}"
    );
}

#[test]
fn rock_is_bedded_and_the_same_wherever_it_is_asked() {
    let rock = Rock {
        stone: Vec3::splat(0.4),
        strata: Vec3::splat(0.2),
        lichen: Vec3::new(0.5, 0.5, 0.3),
        bedding: 2.0,
        seed: 9,
    };
    let face = Vec3::new(1.0, 0.0, 0.0);
    let tones: Vec<f64> = (0..40)
        .map(|step| {
            rock.colour(Vec3::new(0.0, f64::from(step) * 0.5, 3.0), face, 0.01)
                .x
        })
        .collect();
    let (least, most) = tones
        .iter()
        .fold((f64::INFINITY, 0.0f64), |(l, m), &t| (l.min(t), m.max(t)));
    assert!(most - least > 0.05, "beds differ: {least} to {most}");
    assert_eq!(
        rock.colour(Vec3::new(1.0, 2.0, 3.0), face, 0.01),
        rock.colour(Vec3::new(1.0, 2.0, 3.0), face, 0.01)
    );
}

/// Ground seen close shows its grain, a shade a grain, and settles to an
/// even shade once a pixel spans its grains: two places a few millimetres
/// apart differ up close and match far off.
#[test]
fn ground_grain_shows_up_close_and_settles_to_its_mean_far_off() {
    let pigment = ground();
    let at = |x: f64, width: f64, (ground, height): ([f64; CHANNELS], f64)| {
        pigment.colour(&Spot {
            p: Vec3::new(x, height, 2.0),
            normal: Vec3::UP,
            height,
            width,
            mark: 0,
            along: 0.0,
            uv: (0.0, 0.0),
            girth: 0.0,
            instance: 0,
            front: true,
            ground,
            thatch: 0.0,
        })
    };
    let places: Vec<f64> = (0..40).map(|step| 1.0 + 0.003 * f64::from(step)).collect();
    // Bare soil, a river's scoured bed of gravel, and the sand of a shore.
    for lie in [
        ([0.3, 0.5, 0.0, 0.0, 0.0], 5.0),
        ([0.9, 0.5, 0.0, 0.0, 0.0], 5.0),
        ([0.3, 0.5, 0.0, 0.0, 0.0], 0.0),
    ] {
        let spread = |width: f64| {
            let shades: Vec<f64> = places
                .iter()
                .map(|&x| at(x, width, lie).luminance())
                .collect();
            let most = shades.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let least = shades.iter().copied().fold(f64::INFINITY, f64::min);
            let mean = shades.iter().sum::<f64>() / f64::from(40u32);
            (most - least) / mean
        };
        assert!(spread(2e-4) > 0.04, "{lie:?} up close: {}", spread(2e-4));
        assert!(spread(0.5) < 5e-3, "{lie:?} far off: {}", spread(0.5));
    }
}

/// A spine is coloured by the age its stem carries: red-brown and darker
/// toward its tip while young, grey once old, whatever key it was placed
/// under.
#[test]
fn a_spine_greys_as_it_ages_and_a_young_one_darkens_toward_its_tip() {
    let ages = [
        Vec3::new(0.14, 0.03, 0.017),
        Vec3::new(0.43, 0.3, 0.16),
        Vec3::new(0.27, 0.25, 0.22),
        Vec3::new(0.55, 0.53, 0.47),
    ];
    let spines = Pigment::Spines(ages);
    let at = |age: f64, along: f64, mark: u32| {
        spines.colour(&Spot {
            uv: (age, 0.0),
            along,
            mark,
            ..spot(Vec3::ZERO, 1e-4)
        })
    };
    for mark in (0..64u32).map(mix32) {
        let (young, old) = (at(0.0, 0.2, mark), at(2.6, 0.2, mark));
        // Red-brown: far more red than blue; grey: nearly as blue as red.
        assert!(young.x > 3.0 * young.z, "{mark}: {young:?}");
        assert!(old.z > 0.6 * old.x, "{mark}: {old:?}");
        assert!(
            at(0.0, 0.95, mark).x < 0.6 * young.x,
            "{mark}: its tip is darker"
        );
        assert!(
            (at(2.6, 0.95, mark) - old).length() < 1e-9,
            "{mark}: an old tip is not"
        );
    }
}
