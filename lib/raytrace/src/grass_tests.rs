use alloc::vec::Vec;

use super::*;
use crate::heightfield::Heightfield;
use crate::scene::Grid;
use crate::shade::{Crown, Shade};
use crate::shape::Shape;
use crate::vector::PACKET;

/// A level field at height nought over `-4..4` each way, its every vertex
/// carrying `attributes`.
fn level(attributes: Option<[u8; 4]>) -> Heightfield {
    let mut field = Heightfield::new(16, (-4.0, -4.0), 0.5, false).expect("a grid");
    let side = field.rows();
    for (_, band) in field.bands(0..side, side) {
        band.fill(0.0);
    }
    if let Some(attributes) = attributes {
        assert!(field.carry_attributes());
        field.rows_mut(0..side).1.fill(attributes);
    }
    field.seal();
    field
}

const MEADOW: GrassKind = GrassKind {
    height: (0.1, 0.3),
    width: 0.01,
    lean: 0.45,
    droop: 0.3,
    thickness: 1.0,
    tufted: 0.5,
    stems: 0.15,
    head: Head::Plume,
    habit: Habit::Open,
    share: 1.0,
};

fn grass(flowers: f64) -> Grass {
    Grass {
        kinds: [Some(MEADOW), None, None, None],
        shoots: 700.0,
        flowers,
    }
}

/// Seen by an eye whose pixels are too fine to merge a leaf, never fading.
const SEEN: Seen = Seen {
    eye: (0.0, 0.0),
    pixel: 0.0,
    fade: (f64::INFINITY, f64::INFINITY),
};

fn cover(cover: &Cover, cell: f64) -> Lawn {
    Lawn {
        field: 0,
        from: (-1.0, -1.0),
        to: (1.0, 1.0),
        hole: None,
        floor: -0.05,
        ceiling: 0.05,
        cell,
        cover: *cover,
        shade: None,
        sward: 5,
        seed: 3,
        seen: SEEN,
        tops: None,
    }
}

fn lawn(flowers: f64) -> Lawn {
    cover(&Cover::Grass(grass(flowers)), 0.15)
}

/// The shade of a closed wood over the whole of a test lawn.
fn closed_wood() -> Shade {
    let crowns: Vec<Crown> = (-8..=8)
        .flat_map(|row| {
            (-8..=8).map(move |column| ((f64::from(column) * 3.0, f64::from(row) * 3.0), 3.0))
        })
        .collect();
    Shade::of(&crowns, ((-10.0, -10.0), (10.0, 10.0)), 0.25, 4.0).expect("a closed wood")
}

/// A ray from `height` above a random place over the lawn, looking down
/// across it at anything from steeply to a few degrees.
fn looking_down(index: u32, height: f64) -> Ray {
    let draw = |salt: u32| unit(mix32(mix32(index ^ 0xabc) ^ salt));
    let origin = Vec3::new(2.0 * draw(1) - 1.0, height, 2.0 * draw(2) - 1.0);
    let heading = TAU * draw(3);
    let dip = 0.1 + 1.4 * draw(4);
    let dir = Vec3::new(
        mathf::cos(dip) * mathf::cos(heading),
        -mathf::sin(dip),
        mathf::cos(dip) * mathf::sin(heading),
    );
    Ray::new(origin, dir)
}

/// What `lawn` over `field` shows `rays` rays looking down from `height`.
fn met(lawn: &Lawn, field: Heightfield, (rays, height): (u32, f64)) -> Vec<(Ray, Hit)> {
    let fields = [field];
    let geometry = Geometry {
        faces: &[],
        fields: &fields,
        prototypes: &[],
        lawns: &[],
    };
    (0..rays)
        .filter_map(|index| {
            let ray = looking_down(index, height);
            lawn.intersect(&ray, 1e-9, f64::INFINITY, geometry)
                .map(|hit| (ray, hit))
        })
        .collect()
}

/// How the cell about `at` of `lawn` over `field` grows.
fn stand_at(lawn: &Lawn, field: &Heightfield, at: (f64, f64)) -> Option<Stand> {
    let Cover::Grass(grass) = lawn.cover else {
        return None;
    };
    lawn.stand(&grass, (field, None), lawn.cell_of(at))
}

#[test]
fn shoots_root_within_their_cells_and_never_cross_the_walls() {
    let lawn = lawn(1.0);
    let field = level(None);
    let (mut edge, mut total, mut flowered, mut headed) = (0u32, 0u32, 0u32, 0u32);
    for (index, merged) in (0..400u32).zip([1.0, 1.0, 2.5, 3.75].into_iter().cycle()) {
        let Some(stand) = stand_at(&lawn, &field, (-0.9 + 0.0045 * f64::from(index), 0.3)) else {
            continue;
        };
        let stand = Stand { merged, ..stand };
        let cell_key = mix32(index);
        let mut toward = (1.0, 0.0);
        for shoot in 0..24 {
            // Bowing as far as the rim of a tussock ever has it.
            let sprout = lawn.sprout(&stand, (cell_key, shoot));
            let placed = lawn.place(&sprout, (&stand, 1.0), (toward, 1.4));
            let root = sprout.root;
            toward = turn_golden(toward);
            let tip = (
                root.0 + placed.reach * placed.toward.0,
                root.1 + placed.reach * placed.toward.1,
            );
            for at in [root.0, root.1, tip.0, tip.1] {
                assert!(
                    at >= placed.breadth - 1e-12 && at <= lawn.cell - placed.breadth + 1e-12,
                    "{placed:?}"
                );
            }
            assert!(placed.width <= WIDEST * lawn.cell + 1e-12, "{placed:?}");
            if let Some(radius) = placed.flower {
                flowered += 1;
                for at in [tip.0, tip.1] {
                    assert!(at - radius >= 0.0 && at + radius <= lawn.cell, "{placed:?}");
                }
            }
            headed += u32::from(placed.head.is_some());
            assert_eq!(placed.key & !KEY, 0);
            let near_wall = |at: f64| at < 0.2 * lawn.cell || at > 0.8 * lawn.cell;
            if near_wall(root.0) || near_wall(root.1) {
                edge += 1;
            }
            total += 1;
        }
    }
    // Rooted evenly, near half fall within a fifth of the walls: none do if
    // the shoots crowd the middle, and the lawn shows its grid.
    assert!(
        total > 2000 && edge * 5 > total * 2,
        "{edge} of {total} near the walls"
    );
    assert!(
        flowered > 0 && headed > 0,
        "{flowered} flowers, {headed} heads"
    );
}

#[test]
fn shoots_stand_on_the_ground_within_the_lawn() {
    let hits = met(&lawn(0.05), level(None), (3000, 0.6));
    let tallest = MEADOW.height.1 * RANKEST * STEM_RISE * BOW;
    for (ray, hit) in &hits {
        let at = ray.at(hit.t);
        assert!(hit.t > 0.0);
        assert!(
            (-1.0..=1.0).contains(&at.x) && (-1.0..=1.0).contains(&at.z),
            "{at:?}"
        );
        assert!(at.y >= -0.011 && at.y <= tallest + FLOWER_ROOM, "{at:?}");
        assert!((hit.normal.length() - 1.0).abs() < 1e-9);
        assert!((0.0..=1.0).contains(&hit.along));
    }
    assert!(hits.len() > 150, "{} of 3000 met the grass", hits.len());
}

#[test]
fn what_a_cover_holds_is_met_nearest_first() {
    let covers = [
        lawn(0.1),
        cover(
            &Cover::Weeds(Weeds {
                share: 1.0,
                leaves: (5, 9),
            }),
            0.32,
        ),
        cover(
            &Cover::Litter(Litter {
                most: 6,
                length: (0.05, 0.12),
                outline: Outline::Lobed { lobes: 4 },
                age: 0.0,
            }),
            0.32,
        ),
    ];
    let fields = [level(None)];
    let geometry = Geometry {
        faces: &[],
        fields: &fields,
        prototypes: &[],
        lawns: &[],
    };
    for lawn in &covers {
        for index in 0..1500 {
            let ray = looking_down(index, 0.6);
            let Some(hit) = lawn.intersect(&ray, 1e-9, f64::INFINITY, geometry) else {
                continue;
            };
            assert!(
                lawn.intersect(&ray, 1e-9, hit.t * (1.0 - 1e-9), geometry)
                    .is_none(),
                "{:?}, ray {index}: something nearer than {}",
                lawn.cover,
                hit.t
            );
            let again = lawn
                .intersect(&ray, 1e-9, f64::INFINITY, geometry)
                .expect("met again");
            assert_eq!((again.t, again.mark), (hit.t, hit.mark));
        }
    }
}

/// A packet's rays crossing a lawn together, each with its own reach and one
/// of them not asked at all, meet just what each meets alone: as near
/// parallel as a pixel's samples, or spread wide enough to part.
#[test]
fn rays_crossing_a_lawn_together_meet_what_each_meets_alone() {
    let covers = [
        lawn(1.0),
        cover(
            &Cover::Weeds(Weeds {
                share: 1.0,
                leaves: (5, 9),
            }),
            0.32,
        ),
        cover(
            &Cover::Litter(Litter {
                most: 6,
                length: (0.05, 0.12),
                outline: Outline::Lobed { lobes: 4 },
                age: 0.0,
            }),
            0.32,
        ),
    ];
    let fields = [level(None)];
    let geometry = Geometry {
        faces: &[],
        fields: &fields,
        prototypes: &[],
        lawns: &[],
    };
    let asked = (0..PACKET)
        .filter(|&lane| lane != 5)
        .fold(Members::NONE, Members::with);
    let mut met = 0;
    for lawn in &covers {
        for packet in 0..1200u32 {
            let base = looking_down(packet, 0.6);
            let spread = if packet % 9 == 0 { 0.2 } else { 0.003 };
            let rays: [Ray; PACKET] = core::array::from_fn(|lane| {
                let lane = u32::try_from(lane).expect("a packet is small");
                let jitter =
                    |salt: u32| spread * (unit(mix32(mix32(packet ^ lane << 20) ^ salt)) - 0.5);
                let dir = base.dir + Vec3::new(jitter(1), jitter(2), jitter(3));
                Ray::new(base.origin, dir.normalized())
            });
            let fars: [f64; PACKET] =
                core::array::from_fn(|lane| if lane == 3 { 0.5 } else { f64::INFINITY });
            let mut together = [None; PACKET];
            lawn.intersect_rays(&rays, asked, (1e-9, &fars), geometry, &mut together);
            for (lane, ray) in rays.iter().enumerate() {
                let alone = (lane != 5)
                    .then(|| lawn.intersect(ray, 1e-9, fars[lane], geometry))
                    .flatten();
                assert_eq!(
                    alloc::format!("{:?}", together[lane]),
                    alloc::format!("{alone:?}"),
                    "{:?}, packet {packet}, ray {lane}",
                    lawn.cover
                );
                met += usize::from(alone.is_some());
            }
        }
    }
    assert!(met > 2000, "only {met} rays met the covers");
}

#[test]
fn flowers_and_seed_heads_top_their_shoots() {
    let hits = met(&lawn(1.0), level(None), (4000, 0.6));
    let flowers: Vec<_> = hits
        .iter()
        .filter(|(_, hit)| hit.mark & FLOWER != 0)
        .collect();
    for (ray, hit) in &flowers {
        assert!(ray.at(hit.t).y >= 0.04, "a flower heads its leaf");
        assert!((hit.along - 1.0).abs() < 1e-12);
    }
    assert!(flowers.len() > 50, "{} flowers met", flowers.len());
    let mean_height = |heads: bool| {
        let heights: Vec<f64> = hits
            .iter()
            .filter(|(_, hit)| hit.mark & FLOWER == 0 && (hit.mark & HEAD != 0) == heads)
            .map(|(ray, hit)| ray.at(hit.t).y)
            .collect();
        assert!(!heights.is_empty(), "heads {heads}: none met");
        heights.iter().sum::<f64>() / crate::vector::real(heights.len())
    };
    let (heads, leaves) = (mean_height(true), mean_height(false));
    assert!(
        heads > leaves,
        "seed heads stand over the leaves: {heads} against {leaves}"
    );
}

#[test]
fn weeds_lie_low_and_fallen_leaves_flat_on_the_ground() {
    let weeds = cover(
        &Cover::Weeds(Weeds {
            share: 1.0,
            leaves: (5, 9),
        }),
        0.32,
    );
    let hits = met(&weeds, level(None), (3000, 0.6));
    assert!(hits.len() > 100, "{} of 3000 met a weed", hits.len());
    for (ray, hit) in &hits {
        assert_ne!(hit.mark & WEED, 0);
        let at = ray.at(hit.t);
        assert!(
            (0.0..=0.2 * 0.32).contains(&at.y),
            "a rosette lies low: {at:?}"
        );
    }
    // Under trees, where fallen leaves gather.
    let litter = Lawn {
        shade: Some(closed_wood()),
        ..cover(
            &Cover::Litter(Litter {
                most: 6,
                length: (0.05, 0.12),
                outline: Outline::Ovate { teeth: 6 },
                age: 0.5,
            }),
            0.32,
        )
    };
    let hits = met(&litter, level(None), (3000, 0.6));
    assert!(hits.len() > 50, "{} of 3000 met a fallen leaf", hits.len());
    for (ray, hit) in &hits {
        assert_ne!(hit.mark & LITTER, 0);
        assert!(
            (0.0..=0.05).contains(&ray.at(hit.t).y),
            "flat on the ground"
        );
        assert!((0.5..=1.0).contains(&hit.along), "no fresher than its age");
    }
}

#[test]
fn nothing_grows_on_a_road_and_less_on_a_path() {
    let open = met(&lawn(0.0), level(Some([0, 128, 0, 255])), (3000, 0.6)).len();
    let road = met(&lawn(0.0), level(Some([0, 128, 255, 255])), (3000, 0.6)).len();
    let path = met(&lawn(0.0), level(Some([0, 128, 120, 255])), (3000, 0.6)).len();
    assert_eq!(road, 0, "no grass on a road");
    assert!(
        path < open && path > 0,
        "{path} on a path, {open} in the open"
    );
}

#[test]
fn grass_thins_and_leaves_gather_under_the_trees() {
    let shade = closed_wood();
    let grass_open = met(&lawn(0.0), level(None), (3000, 0.6)).len();
    let grass_shaded = met(
        &Lawn {
            shade: Some(shade.clone()),
            ..lawn(0.0)
        },
        level(None),
        (3000, 0.6),
    )
    .len();
    assert!(
        grass_shaded < grass_open,
        "{grass_shaded} under trees, {grass_open} in the open"
    );
    let litter = |shade: Option<Shade>| Lawn {
        shade,
        ..cover(
            &Cover::Litter(Litter {
                most: 6,
                length: (0.05, 0.12),
                outline: Outline::Ovate { teeth: 6 },
                age: 0.0,
            }),
            0.32,
        )
    };
    let fallen_open = met(&litter(None), level(None), (3000, 0.6)).len();
    let fallen_shaded = met(&litter(Some(shade)), level(None), (3000, 0.6)).len();
    assert!(
        fallen_shaded > fallen_open,
        "{fallen_shaded} under trees, {fallen_open} in the open"
    );
}

#[test]
fn nothing_is_met_above_the_shoots_or_beside_the_lawn() {
    let fields = [level(None)];
    let geometry = Geometry {
        faces: &[],
        fields: &fields,
        prototypes: &[],
        lawns: &[],
    };
    let lawn = lawn(0.1);
    let over = Ray::new(Vec3::new(-3.0, 1.2, 0.0), Vec3::new(1.0, 0.0, 0.0));
    assert!(lawn
        .intersect(&over, 1e-9, f64::INFINITY, geometry)
        .is_none());
    let beside = Ray::new(Vec3::new(2.5, 1.0, 0.0), -Vec3::UP);
    assert!(lawn
        .intersect(&beside, 1e-9, f64::INFINITY, geometry)
        .is_none());
    assert!(!Shape::Lawn { lawn: 0 }.casts_shadow());
}

#[test]
fn a_finer_lawn_keeps_the_ground_it_covers_to_itself() {
    let field = level(None);
    let holed = Lawn {
        hole: Some(((-0.5, -0.5), (0.5, 0.5))),
        ..lawn(0.0)
    };
    let fields = [level(None)];
    let geometry = Geometry {
        faces: &[],
        fields: &fields,
        prototypes: &[],
        lawns: &[],
    };
    for index in 0..600u32 {
        let x = -0.45 + 0.9 * unit(mix32(index));
        let z = -0.45 + 0.9 * unit(mix32(index ^ 0x77));
        let down = Ray::new(Vec3::new(x, 1.0, z), -Vec3::UP);
        assert!(
            holed
                .intersect(&down, 1e-9, f64::INFINITY, geometry)
                .is_none(),
            "{x} {z}"
        );
        assert!(holed
            .canopy(Vec3::new(x, 0.0, z), core::slice::from_ref(&field))
            .is_none());
    }
    assert!(
        met(&holed, level(None), (3000, 0.6)).len() > 50,
        "it still grows about the hole"
    );
}

#[test]
fn a_sward_grows_in_tussocks_with_thin_ground_between() {
    let lawn = lawn(0.0);
    let (mut thin, mut thick, mut samples) = (0u32, 0u32, 0u32);
    let mut mean = 0.0;
    for i in 0..300u32 {
        for j in 0..300u32 {
            let sward = lawn.sward((0.037 * f64::from(i), 0.041 * f64::from(j)));
            let (thickness, stature) = sward.growth(1.0);
            assert!(thickness > 0.0 && stature > 0.0 && stature <= RANKEST);
            assert!((mathf::hypot(sward.out.0, sward.out.1) - 1.0).abs() < 1e-9);
            thin += u32::from(thickness < 0.35);
            thick += u32::from(thickness > 1.6);
            mean += thickness;
            samples += 1;
        }
    }
    let mean = mean / f64::from(samples);
    // Clumps and gaps both, about as much grass as an even sod on the whole.
    assert!(
        thin * 10 > samples && thick * 10 > samples,
        "{thin} thin, {thick} thick of {samples}"
    );
    assert!((0.7..1.3).contains(&mean), "{mean}");
    // An even sod varies only with its swathes.
    let (least, most) = (0..400u32)
        .map(|i| lawn.sward((0.021 * f64::from(i), 0.0)).growth(0.0).0)
        .fold((f64::INFINITY, 0.0f64), |(low, high), value| {
            (low.min(value), high.max(value))
        });
    assert!(
        least >= 0.45 - 1e-9 && most <= 1.55 + 1e-9,
        "{least} to {most}"
    );
}

#[test]
fn each_kind_of_grass_takes_to_the_ground_that_suits_it() {
    let rush = GrassKind {
        habit: Habit::Wet,
        share: 0.6,
        ..MEADOW
    };
    let grass = Grass {
        kinds: [Some(MEADOW), Some(rush), None, None],
        ..grass(0.0)
    };
    let lawn = cover(&Cover::Grass(grass), 0.15);
    let count = |wet: f64| {
        (0..2000u32)
            .filter(|&index| {
                let at = (0.37 * f64::from(index % 50), 0.41 * f64::from(index / 50));
                lawn.kind(&grass, (at, mix32(index)), (wet, 0.0))
                    .map(|(kind, _)| kind)
                    == Some(1)
            })
            .count()
    };
    let (dry, wet) = (count(0.0), count(1.0));
    assert_eq!(dry, 0, "rushes stay out of the dry");
    assert!(wet > 1200, "and crowd the wet: {wet} of 2000");
}

#[test]
fn far_leaves_merge_into_fewer_broader_ones_covering_as_much() {
    let lawn = Lawn {
        seen: Seen {
            pixel: 0.001,
            ..SEEN
        },
        ..lawn(0.0)
    };
    let width = MEADOW.width;
    let at = |distance: f64| lawn.merged((distance, 0.0), width);
    assert!((at(1.0) - 1.0).abs() < 1e-12, "near, as they are");
    assert!(
        at(20.0) > 1.0 && at(30.0) > at(20.0),
        "{} {}",
        at(20.0),
        at(30.0)
    );
    // Merged to a little under a pixel's width where they stand.
    assert!((at(30.0) - MERGED * 30.0 * 0.001 / width).abs() < 1e-9);
    // Never broader than the cell allows.
    assert!((at(1e6) * width - WIDEST * lawn.cell).abs() < 1e-12);
}

#[test]
fn a_sward_fades_into_the_ground_far_from_the_eye() {
    let field = level(None);
    let fading = Lawn {
        seen: Seen {
            fade: (0.2, 0.9),
            ..SEEN
        },
        ..lawn(0.0)
    };
    assert!(fading.thrives(&field, (0.05, 0.0)) > 0.9);
    assert!(fading.thrives(&field, (0.6, 0.0)) < fading.thrives(&field, (0.3, 0.0)));
    assert!(fading.thrives(&field, (0.95, 0.0)).abs() < 1e-12);
    assert!(fading
        .canopy(Vec3::new(0.95, 0.0, 0.0), core::slice::from_ref(&field))
        .is_none());
}

#[test]
fn the_blades_above_thin_the_light_below() {
    let field = level(None);
    let lawn = lawn(0.0);
    let at = (0.1, 0.2);
    let fields = core::slice::from_ref(&field);
    let ground = lawn
        .canopy(Vec3::new(at.0, 0.0, at.1), fields)
        .expect("under the sward");
    let higher = lawn
        .canopy(Vec3::new(at.0, 0.4 * ground.up, at.1), fields)
        .expect("within the sward");
    let sun = |rise: f64| Vec3::new(mathf::sqrt(1.0 - rise * rise), rise, 0.0);
    let (overhead, low) = (ground.through(sun(1.0)), ground.through(sun(0.2)));
    assert!(overhead < 1.0 && low < overhead, "{overhead} then {low}");
    assert!(
        higher.through(sun(0.5)) > ground.through(sun(0.5)),
        "less above, more light"
    );
    assert!(
        ground.through(-Vec3::UP) > 0.99,
        "nothing between the ground and itself"
    );
    let diffuse = ground.diffuse();
    assert!(diffuse > 0.0 && diffuse < overhead, "{diffuse}");
    assert!(
        lawn.canopy(Vec3::new(at.0, 2.0, at.1), fields).is_none(),
        "above it, nothing"
    );
    let open = Canopy {
        density: 0.0,
        up: 0.3,
        down: 0.0,
    };
    assert!((open.diffuse() - 1.0).abs() < 1e-9, "{}", open.diffuse());
}

#[test]
fn a_mark_carries_a_shoots_kind_and_vigour_clear_of_its_key() {
    for kind in 0..u32::try_from(GRASS_KINDS).expect("a handful of kinds") {
        for step in 0..=VIGOUR_STEPS {
            let mark = (0x3a_5c77 & KEY) | marks(kind, step) | HEAD;
            assert_eq!(grass_kind(mark), kind as usize);
            assert!((vigour(mark) - f64::from(step) / f64::from(VIGOUR_STEPS)).abs() < 1e-12);
            assert_ne!(mark & HEAD, 0);
            assert_eq!(mark & (FLOWER | WEED | LITTER), 0);
        }
    }
}

#[test]
fn the_golden_turn_is_the_golden_angle() {
    let angle = crate::sample::GOLDEN_ANGLE;
    assert!((GOLDEN.0 - mathf::cos(angle)).abs() < 1e-15);
    assert!((GOLDEN.1 - mathf::sin(angle)).abs() < 1e-15);
}

#[test]
fn a_golden_turn_keeps_a_heading_unit_and_spreads_a_cell_evenly() {
    let mut toward = (1.0, 0.0);
    let mut headings = Vec::new();
    for _ in 0..8 {
        headings.push(mathf::atan2(toward.1, toward.0));
        toward = turn_golden(toward);
        assert!((toward.0 * toward.0 + toward.1 * toward.1 - 1.0).abs() < 1e-12);
    }
    headings.sort_by(f64::total_cmp);
    // Eight headings, none crowding another: every gap within twice the
    // even share of the circle.
    let gaps = headings.windows(2).map(|pair| pair[1] - pair[0]);
    let wrap = TAU - (headings[7] - headings[0]);
    for gap in gaps.chain([wrap]) {
        assert!(gap < 2.0 * TAU / 8.0, "{headings:?}");
    }
}
