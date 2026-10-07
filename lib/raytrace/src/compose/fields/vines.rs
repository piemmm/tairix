//! A farmed land's vineyards as they stand about the eye: vines trained on
//! wires along each field's rows, a post holding the wires every few vines,
//! the vines built once as prototypes at the season's stage and stood as a
//! stand on the rows ([`crate::stand`]), the wires strung along each row from
//! its first post to its last.
//!
//! A vine is a gnarled trunk to the fruiting wire and two cordon arms along
//! it, knobbed with the spurs its shoots rise from between the catch wires.
//! In spring the shoots are short and their leaves small; in summer they
//! stand to the top wire and over in a hedge of palmate leaves, small green
//! grapes beneath; in autumn the leaves turn and the grapes hang ripe; in
//! winter only the pruned canes are left on the wire.

use core::f64::consts::{PI, TAU};

use tairix_countryside::layout::Layout;
use tairix_countryside::usage::Use;
use tairix_countryside::Point;
use tairix_util::mathf;

use super::super::{rgb, Dice, Stage};
use super::field_key;
use super::growing::Growing;
use crate::farmed::{Drill, Grown};
use crate::land::Land;
use crate::leaf::Outline;
use crate::material::{Finish, Material, Relief};
use crate::pigment::{Foliage, Pigment};
use crate::prototype::{Assembly, Blade, Building, Part, Tube};
use crate::sample::mix32;
use crate::shape::Shape;
use crate::stand::{Plant, Planting, Sowing, Stand, PLANTS};
use crate::tree::Season;
use crate::vector::{cell_of, share, single, singles, Frame, Pose, Vec3};

/// How many vines of a season are built for its stands to draw from.
const VARIANTS: usize = 6;

/// How far apart a vineyard's rows run and its vines along them, and how
/// many vines a post stands in place of.
const ROWS: f64 = 2.2;
const APART: f64 = 1.1;
const POSTED: u32 = 5;

/// How high its fruiting wire runs, and the pairs of catch wires above it,
/// with how far either side of the row each pair runs.
const FRUITING: f64 = 0.8;
const CATCH: [(f64, f64); 2] = [(1.15, 0.07), (1.5, 0.08)];

/// How a vineyard's vines are set out: true on their rows, turned to them
/// and upright, nearly every one standing, never with a tramline.
const TRELLISED: Planting = Planting {
    jitter: (0.04, 0.0),
    lean: 0.0,
    trained: Some(0.05),
    come_up: 0.97,
    tramlines: false,
};

/// The share of the reach a vineyard stands as vines out to at which they
/// begin to thin away into the land's own colour of it.
const THINNING: f64 = 0.85;

/// How far a vine's growth has come in a season.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Leafing {
    /// Pruned back to a cane or two tied on the fruiting wire.
    Bare,
    /// Its shoots short and their leaves small.
    Budding,
    /// In full leaf to the top wire, its grapes small and green.
    Leafy,
    /// Its leaves turning, its grapes ripe.
    Turning,
}

const fn leafing(season: Season) -> Leafing {
    match season {
        Season::Winter => Leafing::Bare,
        Season::Spring => Leafing::Budding,
        Season::Summer => Leafing::Leafy,
        Season::Autumn { .. } => Leafing::Turning,
    }
}

/// The materials a vineyard's vines and trellis are made in.
#[derive(Copy, Clone, Debug)]
struct Materials {
    wood: u16,
    shoot: u16,
    leaf: u16,
    grape: u16,
    post: u16,
    wire: u16,
}

impl Materials {
    /// The materials a vineyard is made in `leafing`, its grapes black or
    /// white as `dice` draws, as the stage's.
    fn of(stage: &mut Stage, dice: &mut Dice, leafing: Leafing) -> Option<Self> {
        let leaves: [u32; 4] = match leafing {
            Leafing::Turning => [0xC8_9A_2A, 0xB8_5A_22, 0xD8_B8_3E, 0x9A_34_22],
            Leafing::Budding => [0x8A_B0_44, 0x9C_BE_52, 0x7A_A0_3A, 0xAC_C8_60],
            Leafing::Leafy | Leafing::Bare => [0x4A_76_2A, 0x58_84_32_u32, 0x3E_68_24, 0x64_8E_3A],
        };
        let foliage = Foliage {
            colours: leaves.map(rgb),
            underside: 0.08,
            veins: 0.5,
            edge: rgb(0x6A_44_1E),
            browning: if leafing == Leafing::Turning {
                0.6
            } else {
                0.06
            },
            spots: if leafing == Leafing::Turning {
                0.5
            } else {
                0.04
            },
            snow: 0.0,
            outline: Outline::Palmate { lobes: 5 },
        };
        let black = dice.chance(0.6);
        let grape = match (leafing, black) {
            (Leafing::Turning, true) => 0x2C_1A_34,
            (Leafing::Turning, false) => 0xB8_B2_5A,
            _ => 0x7E_9C_48,
        };
        let mut made = |material: Material| u16::try_from(stage.material(material)?).ok();
        Some(Self {
            wood: made(
                Material::new(
                    Pigment::Solid(rgb(0x5E_4A_38)),
                    Finish::Coated { roughness: 0.9 },
                )
                .with_relief(Relief::grain(0.3, 90.0, dice.seed())),
            )?,
            shoot: made(Material::new(
                Pigment::Solid(rgb(if leafing == Leafing::Bare {
                    0x8A_62_3E
                } else {
                    0x6E_86_3C
                })),
                Finish::Coated { roughness: 0.6 },
            ))?,
            leaf: made(Material::new(
                Pigment::Foliage(foliage),
                Finish::Leaf { translucency: 0.3 },
            ))?,
            grape: made(Material::new(
                Pigment::Solid(rgb(grape)),
                Finish::Coated { roughness: 0.35 },
            ))?,
            post: made(
                Material::new(
                    Pigment::Solid(rgb(0x7A_70_62)),
                    Finish::Coated { roughness: 0.95 },
                )
                .with_relief(Relief::grain(0.25, 40.0, dice.seed())),
            )?,
            wire: made(Material::new(
                Pigment::Solid(rgb(0x9A_9C_9E)),
                Finish::Metal { roughness: 0.4 },
            ))?,
        })
    }
}

/// A palmate leaf `size` across on `growing`, on a stalk from `at` heading
/// out `out`, its face turned to `facing`.
fn leaf(
    growing: &mut Growing,
    (at, out, facing): (Vec3, Vec3, Vec3),
    size: f64,
    (materials, key): (&Materials, u32),
) -> Option<()> {
    let blade = at + out * (0.6 * size);
    growing.limb(&[at, blade], (0.0016, 0.0012), (materials.shoot, key))?;
    let normal = (facing - out * facing.dot(out)).normalized();
    // Hanging from its stalk, a little aside.
    let axis = (out * 0.4 - Vec3::UP * 0.9 + facing.cross(Vec3::UP) * 0.2).normalized();
    let axis = (axis - normal * axis.dot(normal)).normalized();
    growing.reaching(blade + axis * size, 0.5 * size);
    growing.assembly.push(Part::Leaf(Blade {
        base: singles(blade),
        normal: singles(normal),
        axis: singles(axis),
        length: single(size),
        width: single(0.5 * size),
        outline: Outline::Palmate { lobes: 5 },
        fold: 0.25,
        material: materials.leaf,
        key,
    }))
}

/// A bunch of grapes on `growing`, hanging from `at`, `length` long, its
/// berries `berry` in radius: conical, broadest near its shoulder.
fn bunch(
    growing: &mut Growing,
    at: Vec3,
    (length, berry): (f64, f64),
    (materials, key): (&Materials, u32),
    dice: &mut Dice,
) -> Option<()> {
    let tip =
        at - Vec3::UP * length + Vec3::new(dice.range(-0.02, 0.02), 0.0, dice.range(-0.02, 0.02));
    growing.limb(&[at, tip], (0.0018, 0.001), (materials.shoot, key))?;
    let rings = 6;
    for ring in 0..rings {
        let down = f64::from(ring) / f64::from(rings - 1);
        let middle = at.lerp(tip, 0.12 + 0.85 * down);
        let wide = 0.5 * length * (0.55 - 0.42 * down);
        let count = mathf::round_i32(10.0 - 6.0 * down).max(3);
        let turn = dice.range(0.0, TAU);
        for index in 0..count {
            let around = turn + TAU * f64::from(index) / f64::from(count);
            let out = Vec3::new(
                mathf::cos(around),
                dice.range(-0.3, 0.3),
                mathf::sin(around),
            );
            let centre = middle + out * (wide * dice.range(0.6, 1.0));
            let radius = berry * dice.range(0.85, 1.12);
            growing.reaching(centre, radius);
            let grape = Tube::new(
                (centre, centre),
                ((radius, radius), (0.0, 0.0)),
                (materials.grape, key),
                Vec3::new(1.0, 0.0, 0.0),
            );
            growing.assembly.push(Part::Tube(grape))?;
        }
    }
    Some(())
}

/// One vine at `leafing` in `materials`, drawn from `dice`, its arms along
/// its local x: what it is built of, and how tall it stands and how far it
/// reaches about its foot.
fn vine(dice: &mut Dice, leafing: Leafing, materials: &Materials) -> Option<(Building, f64, f64)> {
    let mut growing = Growing::with_room(800, 0)?;
    let key = dice.seed();
    // A gnarled trunk, thickest at its foot, to a head a little below the
    // fruiting wire.
    let head = Vec3::new(
        dice.range(-0.03, 0.03),
        FRUITING - dice.range(0.02, 0.06),
        dice.range(-0.02, 0.02),
    );
    let mut trunk = [Vec3::new(0.0, -0.05, 0.0); 5];
    for (index, point) in trunk.iter_mut().enumerate().skip(1) {
        let up = share(index, 4);
        let wobble =
            Vec3::new(dice.range(-0.035, 0.035), 0.0, dice.range(-0.035, 0.035)) * (1.0 - up);
        *point = Vec3::new(0.0, -0.05, 0.0).lerp(head, up) + wobble;
    }
    growing.limb(
        &trunk,
        (dice.range(0.03, 0.042), 0.022),
        (materials.wood, key),
    )?;
    for side in [-1.0, 1.0] {
        // Each arm runs along the wire to meet its neighbour's.
        let reach = 0.5 * APART * dice.range(0.85, 0.98);
        let arm: [Vec3; 4] = core::array::from_fn(|index| {
            let along = share(index, 3);
            head.lerp(Vec3::new(side * reach, FRUITING + 0.012, 0.0), along)
                + Vec3::new(
                    0.0,
                    0.02 * mathf::sin(PI * along),
                    0.015 * mathf::sin(TAU * along),
                )
        });
        growing.limb(&arm, (0.019, 0.011), (materials.wood, key))?;
        let spurs = dice.count(4, 6);
        for spur in 0..spurs {
            let along = (f64::from(spur) + dice.range(0.3, 0.8)) / f64::from(spurs);
            let foot = head.lerp(Vec3::new(side * reach, FRUITING + 0.012, 0.0), along);
            let knob = foot
                + Vec3::new(
                    dice.range(-0.01, 0.01),
                    dice.range(0.025, 0.045),
                    dice.range(-0.01, 0.01),
                );
            growing.limb(&[foot, knob], (0.012, 0.009), (materials.wood, key))?;
            shoot(
                &mut growing,
                (knob, side),
                leafing,
                (materials, part_key(key, spur)),
                dice,
            )?;
        }
    }
    growing.finish()
}

/// A key of `key`'s for the `index`th of its parts.
fn part_key(key: u32, index: u32) -> u32 {
    mix32(key ^ index.wrapping_mul(0x9e37_79b9))
}

/// A shoot from the spur at `knob` on the arm reaching out `side`, as far as
/// `leafing` has it grown: rising between the catch wires, leafing
/// alternately as it goes, grapes hanging from its foot; or a pruned cane
/// tied along the wire.
fn shoot(
    growing: &mut Growing,
    (knob, side): (Vec3, f64),
    leafing: Leafing,
    (materials, key): (&Materials, u32),
    dice: &mut Dice,
) -> Option<()> {
    if leafing == Leafing::Bare {
        // Winter: a cane or a stub, cut back.
        let long = dice.range(0.04, 0.12);
        return growing.limb(
            &[knob, knob + Vec3::new(side * 0.4 * long, long, 0.0)],
            (0.006, 0.005),
            (materials.shoot, key),
        );
    }
    let tall = match leafing {
        Leafing::Budding => dice.range(0.15, 0.42),
        _ => dice.range(0.75, 1.15),
    };
    let lean = Vec3::new(dice.range(-0.12, 0.12), 0.0, dice.range(-0.06, 0.06));
    // The tallest flop over the top wire, to whichever side they lean.
    let over = (knob.y + tall - (CATCH[1].0 + 0.15)).max(0.0);
    let fall = if lean.z < 0.0 { -1.0 } else { 1.0 };
    let points: [Vec3; 5] = core::array::from_fn(|index| {
        let along = share(index, 4);
        let flop = Vec3::new(0.0, -1.0, 1.4 * fall) * (over * along * along);
        knob + (Vec3::UP + lean * along) * (tall * along) + flop
    });
    growing.limb(&points, (0.0055, 0.0028), (materials.shoot, key))?;
    let (breadth, every) = match leafing {
        Leafing::Budding => (dice.range(0.05, 0.08), 0.06),
        _ => (dice.range(0.14, 0.19), 0.08),
    };
    let leaves = mathf::round_i32(tall / every).max(2);
    for index in 0..leaves {
        let along = (f64::from(index) + 0.6) / f64::from(leaves);
        let (piece, within) = cell_of(along * 4.0);
        let (from, to) = (
            points.get(piece).copied()?,
            points.get((piece + 1).min(4)).copied()?,
        );
        let at = from.lerp(to, within);
        // Alternate sides across the row, a little round each time.
        let across = if index % 2 == 0 { 1.0 } else { -1.0 };
        let round = dice.range(-0.6, 0.6);
        let out = Vec3::new(
            mathf::sin(round) * 0.6,
            dice.range(0.05, 0.35),
            across * mathf::cos(round),
        )
        .normalized();
        let facing = Vec3::new(dice.range(-0.3, 0.3), dice.range(0.2, 0.6), across).normalized();
        let grown = breadth * dice.range(0.8, 1.15) * (1.0 - 0.35 * along * along);
        leaf(
            growing,
            (at, out, facing),
            grown,
            (materials, part_key(key, index.cast_unsigned())),
        )?;
    }
    if leafing != Leafing::Budding && dice.chance(0.7) {
        let (length, berry) = match leafing {
            Leafing::Turning => (dice.range(0.12, 0.2), 0.0075),
            _ => (dice.range(0.08, 0.13), 0.0048),
        };
        let at = points.get(1).copied()?.lerp(knob, 0.5) + Vec3::new(0.0, 0.0, dice.sign() * 0.04);
        bunch(
            growing,
            at,
            (length, berry),
            (materials, part_key(key, 0x6b)),
            dice,
        )?;
    }
    Some(())
}

/// A trellis post: a weathered round post standing over its top wire.
fn post(materials: &Materials, key: u32) -> Option<(Building, f64, f64)> {
    let mut growing = Growing::with_room(2, 0)?;
    let top = CATCH[1].0 + 0.35;
    growing.limb(
        &[Vec3::new(0.0, -0.3, 0.0), Vec3::new(0.0, top, 0.0)],
        (0.05, 0.045),
        (materials.post, key),
    )?;
    growing.finish()
}

/// How thick a trellis's wire runs, how many pieces a stretch between two
/// posts is strung in, and how far it sags midway.
const WIRE: f64 = 0.0016;
const SAG_PIECES: usize = 4;
const SAG: f64 = 0.012;

/// A trellis's wires, each how high it runs and how far across the row: the
/// fruiting wire, and each pair of catch wires either side.
const STRANDS: [(f64, f64); 5] = [
    (FRUITING, 0.0),
    (CATCH[0].0, -CATCH[0].1),
    (CATCH[0].0, CATCH[0].1),
    (CATCH[1].0, -CATCH[1].1),
    (CATCH[1].0, CATCH[1].1),
];

/// The wires strung on `assembly` between each two of `posts` standing next
/// to one another along a row, `span` apart, across the rows' `side` way,
/// sagging a little between their posts.
fn wires(
    assembly: &mut Assembly,
    posts: &[(i32, Vec3)],
    (side, span): (Vec3, f64),
    (material, key): (u16, u32),
) -> Option<()> {
    for pair in posts.windows(2) {
        let [(row, from), (next, to)] = pair else {
            continue;
        };
        // Only posts next to one another along the same row hold a wire.
        if row != next || ((*to - *from).length() - span).abs() > 0.25 * span {
            continue;
        }
        for (high, off) in STRANDS {
            let lifted = |at: Vec3| at + Vec3::UP * high + side * off;
            let mut points = [lifted(*from); SAG_PIECES + 1];
            for (index, point) in points.iter_mut().enumerate() {
                let along = share(index, SAG_PIECES);
                *point =
                    lifted(from.lerp(*to, along)) - Vec3::UP * (4.0 * SAG * along * (1.0 - along));
            }
            for piece in points.windows(2) {
                let [a, b] = piece else {
                    continue;
                };
                let way = (*b - *a).cross(Vec3::UP);
                let way = if way.length() > 1e-9 {
                    way.normalized()
                } else {
                    side
                };
                assembly.push(Part::Tube(Tube::new(
                    (*a, *b),
                    ((WIRE, WIRE), (0.0, 0.0)),
                    (material, key),
                    way,
                )))?;
            }
        }
    }
    Some(())
}

/// What a vineyard's stands draw from in a season: its vines and its post,
/// and the materials they and its wires are made in.
struct Made {
    vines: [Plant; PLANTS],
    post: Plant,
    materials: Materials,
}

impl Made {
    /// The vines and the post for `leafing`, planned on `stage`, drawn from
    /// `seed`; `None` when the stage will not hold them.
    fn new(stage: &mut Stage, leafing: Leafing, seed: u64) -> Option<Self> {
        let materials = Materials::of(stage, &mut Dice::keyed(seed, 0), leafing)?;
        let mut vines = [Plant {
            prototype: 0,
            material: 0,
            height: 0.0,
            reach: 0.0,
        }; PLANTS];
        for (index, slot) in vines.iter_mut().take(VARIANTS).enumerate() {
            let (building, height, reach) =
                vine(&mut Dice::keyed(seed, index + 1), leafing, &materials)?;
            *slot = Plant {
                prototype: stage.assemble(building)?,
                material: u32::from(materials.wood),
                height,
                reach,
            };
        }
        let (building, height, reach) = post(&materials, Dice::keyed(seed, VARIANTS + 1).seed())?;
        let post = Plant {
            prototype: stage.assemble(building)?,
            material: u32::from(materials.post),
            height,
            reach,
        };
        Some(Self {
            vines,
            post,
            materials,
        })
    }
}

/// Stand `layout`'s vineyards lying within `reach` of the eye at `eye` on
/// `land` in `season`, their vines built as first wanted from `seed`, and
/// string their trellises' wires; `None` when the stage will not hold them.
pub(super) fn stand(
    stage: &mut Stage,
    (land, layout): (&Land, &Layout),
    (eye, season): (Point, Season),
    (reach, seed): (f64, u64),
) -> Option<()> {
    let leafing = leafing(season);
    let mut made: Option<Made> = None;
    for parcel in layout.parcels() {
        let field = &parcel.field;
        if parcel.usage.used != Use::Vineyard || field.cell.distance(eye) > reach {
            continue;
        }
        let (code, rows) = land
            .grids
            .grows(&stage.fields, field.middle.x, field.middle.y);
        if Grown::of(code) != Grown::Vineyard {
            continue;
        }
        if made.is_none() {
            made = Some(Made::new(stage, leafing, seed)?);
        }
        let Made {
            vines,
            post,
            materials,
        } = made.as_ref()?;
        let drill = Drill::of(ROWS, (code, rows));
        let sowing = Sowing {
            grows: (code, rows),
            drill,
            apart: APART,
            planting: TRELLISED,
            plants: *vines,
            kinds: VARIANTS,
            sizes: (0.92, 1.06),
            posts: Some((POSTED, *post)),
            eye: (eye.x, eye.y),
            thinning: (THINNING * reach, reach),
            seed: Dice::keyed(seed ^ field_key(field.id), 1).seed(),
        };
        let Some(stood) = Stand::new((land.grids, &stage.fields), &field.cell, &sowing) else {
            continue;
        };
        let posts = stood.posts(&stage.fields)?;
        let (wire, wood) = (materials.wire, usize::from(materials.wood));
        let side = drill.place((1.0, 0.0));
        let mut strung = Assembly::with_room(posts.len() * STRANDS.len() * SAG_PIECES, 0)?;
        let key = Dice::keyed(seed ^ field_key(field.id), 2).seed();
        wires(
            &mut strung,
            &posts,
            (Vec3::new(side.0, 0.0, side.1), f64::from(POSTED) * APART),
            (wire, key),
        )?;
        stage.stand(stood, wood)?;
        if strung.parts() > 0 {
            let prototype = stage.assemble(strung.finish()?)?;
            let still = Pose::new(Vec3::ZERO, Frame::WORLD);
            let instance = Shape::Instance {
                prototype,
                pose: still,
                scale: 1.0,
                key: 0,
            };
            stage.add(instance, usize::from(wire), still, false)?;
        }
    }
    Some(())
}

#[cfg(test)]
#[path = "vines_tests.rs"]
mod tests;
