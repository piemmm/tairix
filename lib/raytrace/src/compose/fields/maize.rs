//! A farmed land's maize as it stands about the eye: plants built once as
//! prototypes at the stage the season has them at, and each field near the
//! eye stood as a stand of them on its drilled rows ([`crate::stand`]); past
//! them the sward carries the crop on.
//!
//! A plant is a stalk jointed at its nodes, closer together low down, each
//! leaf's sheath wrapping it up to the collar the blade springs from. Its
//! leaves alternate up it, arching out and drooping, the long ones about its
//! ear the most, each folded along its midrib with its margins rippling and
//! the lowest drying first. Brace roots splay from its foot, a tassel
//! branches from its top, and an ear or two stand part way up in their
//! husks, silks spilling from their tips. Ripe, the whole plant has gone to
//! straw, its leaves hanging and its ears bowed.

use alloc::vec::Vec;
use core::f64::consts::{PI, TAU};

use tairix_countryside::layout::Layout;
use tairix_countryside::usage::{Crop, Use};
use tairix_countryside::Point;
use tairix_util::mathf;

use super::super::{rgb, Dice, Stage};
use super::field_key;
use super::growing::Growing;
use crate::detail::Bounds;
use crate::farmed::{self, Drill, Grown, Stage as Growth};
use crate::land::Land;
use crate::maize::{drying, Maize};
use crate::material::{Finish, Material};
use crate::noise::smoothstep;
use crate::pigment::Pigment;
use crate::prototype::{Building, Mapping};
use crate::sample::mix32;
use crate::stand::{Plant, Sowing, Stand, DRILLED, PLANTS};
use crate::vector::{power, single, Vec3};

/// How many plants of a stage are built for its stands to draw from.
const VARIANTS: usize = 6;

/// How far apart its plants stand along a row; the least and most of their
/// natural size they stand at; and the share of their reach from the eye at
/// which they begin to thin away into the sward.
const APART: f64 = 0.15;
const SIZES: (f64, f64) = (0.9, 1.08);
const THINNING: f64 = 0.8;

/// How many pieces a leaf is built in along its length.
const PIECES: usize = 16;

/// How far from the eye maize standing as plants begins to give way to the
/// sward, and has wholly.
pub(in crate::compose) fn standing(bounds: &Bounds) -> (f64, f64) {
    (THINNING * bounds.crops, bounds.crops)
}

/// Whether a field grown as `grown` stands as plants about the eye.
pub(in crate::compose) fn stands(grown: Grown) -> bool {
    form(grown).is_some()
}

/// The form a field grown as `grown` stands as plants in, if it does.
const fn form(grown: Grown) -> Option<&'static Form> {
    match grown {
        Grown::Sown(Crop::Maize, Growth::Green) => Some(&GREEN),
        Grown::Sown(Crop::Maize, Growth::Ripe) => Some(&RIPE),
        _ => None,
    }
}

/// A maize plant's form at a stage of its growth: how tall it stands to its
/// topmost leaf's collar; how many leaves it bears; its longest leaf, and
/// how broad a leaf runs as a share of its length; how far its leaves droop;
/// how dry its lowest leaf and its topmost; and whether it is ripe.
#[derive(Copy, Clone, Debug)]
struct Form {
    height: (f64, f64),
    leaves: (u32, u32),
    longest: (f64, f64),
    broadest: f64,
    droop: f64,
    dry: (f64, f64),
    ripe: bool,
}

/// Maize in summer, tasselled, its lowest leaves dying back; and ripe in
/// autumn, gone to straw.
const GREEN: Form = Form {
    height: (1.7, 2.05),
    leaves: (13, 16),
    longest: (0.82, 0.98),
    broadest: 0.12,
    droop: 1.25,
    dry: (0.8, 0.0),
    ripe: false,
};
const RIPE: Form = Form {
    height: (1.65, 1.95),
    leaves: (12, 15),
    longest: (0.76, 0.9),
    broadest: 0.105,
    droop: 1.6,
    dry: (1.0, 0.7),
    ripe: true,
};

/// A plant's colours, as sRGB: its leaves' two greens and their midribs',
/// the straw they dry to and a dead leaf's brown; its stalk's and its
/// sheaths'; its tassel's; its husks', its silks' and its brace roots'. The
/// sward carrying a field's maize on past its plants wears its leaves' and
/// its tassels'.
pub(in crate::compose) struct Colours {
    pub(in crate::compose) greens: [u32; 2],
    midrib: u32,
    straw: u32,
    dead: u32,
    stalk: u32,
    sheath: u32,
    pub(in crate::compose) tassel: u32,
    husk: u32,
    silk: u32,
    root: u32,
}

pub(in crate::compose) const GREEN_COLOURS: Colours = Colours {
    greens: [0x46_78_2A, 0x52_84_32_u32],
    midrib: 0xA2_B8_6E,
    straw: 0xB8_A2_62,
    dead: 0x7A_62_3E,
    stalk: 0x5C_86_36,
    sheath: 0x54_7E_30,
    tassel: 0xB2_A6_5A,
    husk: 0x64_8C_3A,
    silk: 0xAC_8C_4E,
    root: 0x8A_6C_4C,
};
pub(in crate::compose) const RIPE_COLOURS: Colours = Colours {
    greens: [0x9C_8A_5A, 0xA8_96_64_u32],
    midrib: 0xC2_B2_84,
    straw: 0xB4_9C_68,
    dead: 0x7E_66_44,
    stalk: 0xA2_8E_60,
    sheath: 0x9A_86_58,
    tassel: 0x7A_68_4A,
    husk: 0xBA_A6_74,
    silk: 0x5A_42_2C,
    root: 0x7A_62_46,
};

/// The materials a plant is made in.
#[derive(Copy, Clone, Debug)]
struct Materials {
    leaf: u16,
    stalk: u16,
    sheath: u16,
    tassel: u16,
    husk: u16,
    silk: u16,
    root: u16,
}

impl Materials {
    /// The materials a plant of `form` is made in, as the stage's; `None`
    /// when the stage will not hold them.
    fn of(stage: &mut Stage, form: &Form) -> Option<Self> {
        let colours = if form.ripe {
            &RIPE_COLOURS
        } else {
            &GREEN_COLOURS
        };
        let leaves = Maize {
            greens: colours.greens.map(rgb),
            midrib: rgb(colours.midrib),
            straw: rgb(colours.straw),
            dead: rgb(colours.dead),
        };
        // A green leaf lets the sun through; a dry one far less.
        let translucency = if form.ripe { 0.12 } else { 0.32 };
        let mut made = |pigment: Pigment, finish: Finish| {
            u16::try_from(stage.material(Material::new(pigment, finish))?).ok()
        };
        let waxy = || Finish::Coated { roughness: 0.5 };
        let solid = |colour: u32| Pigment::Solid(rgb(colour));
        Some(Self {
            leaf: made(Pigment::Maize(leaves), Finish::Leaf { translucency })?,
            stalk: made(solid(colours.stalk), waxy())?,
            sheath: made(solid(colours.sheath), waxy())?,
            tassel: made(solid(colours.tassel), Finish::Leaf { translucency: 0.2 })?,
            husk: made(solid(colours.husk), Finish::Leaf { translucency: 0.15 })?,
            silk: made(solid(colours.silk), Finish::Matte)?,
            root: made(solid(colours.root), Finish::Matte)?,
        })
    }
}

/// The unit level way at `heading` radians round from x toward z.
fn level(heading: f64) -> Vec3 {
    Vec3::new(mathf::cos(heading), 0.0, mathf::sin(heading))
}

/// How a leaf grows: where its collar stands on its stalk and the level way
/// it heads out; how long it is and how broad at its broadest; how far off
/// upright it springs and how far further it droops by its tip; how far it
/// folds along its midrib, twists and drifts aside; and how dry it is.
#[derive(Copy, Clone, Debug)]
struct Leaf {
    collar: Vec3,
    heading: f64,
    length: f64,
    width: f64,
    rise: f64,
    droop: f64,
    fold: f64,
    twist: f64,
    drift: f64,
    dry: f64,
}

/// How broad a leaf runs `along` its length, as a share of its breadth at
/// its broadest: clasping its stalk at its collar, broadest a quarter of
/// the way out and tapering to its point.
fn breadth(along: f64) -> f64 {
    if along < 0.25 {
        0.5 + 0.5 * mathf::sin(0.5 * PI * along / 0.25)
    } else {
        power(
            mathf::cos(0.5 * PI * ((along - 0.25) / 0.75).min(1.0)),
            0.75,
        )
    }
}

/// The buffers a leaf's mesh is built in, kept from leaf to leaf.
#[derive(Default)]
struct Sheet {
    points: Vec<Vec3>,
    faces: Vec<([u32; 3], u16)>,
    coords: Vec<[f32; 2]>,
}

impl Sheet {
    /// `leaf` as a mesh on `growing`, in `material` and keyed `key`: its
    /// midrib and its two margins in [`PIECES`] pieces, meeting at its tip.
    fn leaf(
        &mut self,
        growing: &mut Growing,
        leaf: &Leaf,
        (material, key): (u16, u32),
        dice: &mut Dice,
    ) -> Option<()> {
        let rings = PIECES;
        self.points.clear();
        self.faces.clear();
        self.coords.clear();
        self.points.try_reserve(3 * rings + 1).ok()?;
        self.faces.try_reserve(4 * rings).ok()?;
        self.coords.try_reserve(3 * rings + 1).ok()?;
        let step = leaf.length / f64::from(u32::try_from(rings).ok()?);
        let ripple = (
            dice.range(0.06, 0.09),
            [dice.range(0.0, TAU), dice.range(0.0, TAU)],
        );
        let mut at = leaf.collar;
        for ring in 0..=rings {
            let s = step * f64::from(u32::try_from(ring).ok()?);
            let along = s / leaf.length;
            // It rises from its collar and arches over toward its tip.
            let way = |along: f64| {
                let bent = leaf.rise + leaf.droop * power(along, 2.2);
                level(leaf.heading + leaf.drift * along) * mathf::sin(bent)
                    + Vec3::UP * mathf::cos(bent)
            };
            if ring == rings {
                self.points.push(at);
                self.coords.push([single(s), 0.0]);
                growing.reaching(at, 0.0);
                break;
            }
            let tangent = way(along);
            let flat = level(leaf.heading + leaf.drift * along + 0.5 * PI);
            let side = (flat - tangent * flat.dot(tangent)).normalized();
            let upper = side.cross(tangent);
            let (sin, cos) = (
                mathf::sin(leaf.twist * along),
                mathf::cos(leaf.twist * along),
            );
            let side = side * cos + upper * sin;
            let upper = side.cross(tangent);
            let half = 0.5 * leaf.width * breadth(along);
            let fold = leaf.fold * (1.0 - 0.6 * along);
            let waving = 0.035
                * leaf.width
                * smoothstep(0.1, 0.4, along)
                * (1.0 - smoothstep(0.85, 1.0, along));
            for (edge, phase) in [(-1.0, ripple.1[0]), (1.0, ripple.1[1])] {
                let rippled = waving * mathf::sin(TAU * s / ripple.0 + phase);
                let point = at
                    + side * (edge * half * mathf::cos(fold))
                    + upper * (half * mathf::sin(fold) + rippled);
                if edge < 0.0 {
                    self.points.push(point);
                    self.coords.push([single(s), -1.0]);
                    self.points.push(at);
                    self.coords.push([single(s), 0.0]);
                } else {
                    self.points.push(point);
                    self.coords.push([single(s), 1.0]);
                }
                growing.reaching(point, 0.0);
            }
            at += way(along + 0.5 / f64::from(u32::try_from(rings).ok()?)) * step;
        }
        let tip = u32::try_from(3 * rings).ok()?;
        for ring in 0..u32::try_from(rings).ok()? {
            let [left, middle, right] = [3 * ring, 3 * ring + 1, 3 * ring + 2];
            if ring + 1 == u32::try_from(rings).ok()? {
                self.faces.push(([left, middle, tip], material));
                self.faces.push(([middle, right, tip], material));
            } else {
                let [onward_left, onward_middle, onward_right] = [left + 3, middle + 3, right + 3];
                self.faces.push(([left, middle, onward_middle], material));
                self.faces
                    .push(([left, onward_middle, onward_left], material));
                self.faces.push(([middle, right, onward_right], material));
                self.faces
                    .push(([middle, onward_right, onward_middle], material));
            }
        }
        growing.assembly.mesh_mapped(
            &self.points,
            &self.faces,
            Mapping {
                coords: &self.coords,
                key: drying(key, leaf.dry),
                size: leaf.length,
                trim: None,
            },
        )
    }
}

/// One maize plant of `form` in `materials`, drawn from `dice`: what it is
/// built of, and how tall it stands and how far it reaches about its foot.
fn plant(dice: &mut Dice, form: &Form, materials: &Materials) -> Option<(Building, f64, f64)> {
    let mut growing = Growing::with_room(1600, 3 * PIECES * 18)?;
    let mut sheet = Sheet::default();
    let height = dice.range(form.height.0, form.height.1);
    let count = dice.count(form.leaves.0, form.leaves.1);
    let longest = dice.range(form.longest.0, form.longest.1);
    let foot = dice.range(0.011, 0.014);
    let radius = |up: f64| foot * (1.0 - 0.55 * (up / height).clamp(0.0, 1.0));
    // Its leaves alternate in one plane, turned a little each node.
    let facing = dice.range(0.0, TAU);
    let side = |node: u32, turn: f64| facing + f64::from(node) * PI + turn;
    let node = |index: u32| height * power(f64::from(index) / f64::from(count), 1.3);
    let key = dice.seed();
    // The stalk zigzags a little at each node, toward that node's leaf.
    let mut turns = [0.0; 20];
    for turn in turns.iter_mut().take(usize::try_from(count).ok()? + 1) {
        *turn = dice.range(-0.3, 0.3);
    }
    let turn = |index: u32| {
        turns
            .get(usize::try_from(index).unwrap_or(0))
            .copied()
            .unwrap_or(0.0)
    };
    let joint = |index: u32| {
        let up = node(index);
        level(side(index, turn(index))) * (0.004 * (1.0 - 0.5 * up / height)) + Vec3::UP * up
    };
    for index in 0..count {
        let (low, high) = (node(index), node(index + 1));
        growing.limb(
            &[joint(index), joint(index + 1)],
            (radius(low), radius(high)),
            (materials.stalk, key),
        )?;
    }
    let ear = mathf::round_i32(0.55 * f64::from(count));
    for index in 0..count {
        let (low, high) = (node(index), node(index + 1));
        let along = (f64::from(index) + 0.5) / f64::from(count);
        // Each leaf's sheath wraps its stalk up to the collar above its node.
        let collar = low + (0.55 * (high - low)).min(0.16);
        let wrapped = radius(low) + 0.0025;
        let at = joint(index);
        growing.limb(
            &[at, at + Vec3::UP * (collar - low)],
            (wrapped, wrapped),
            (materials.sheath, key),
        )?;
        let heading = side(index, turn(index));
        // The leaves about its ear grow longest.
        let off_ear = (along - 0.55) / 0.55;
        let bell = (1.0 - off_ear * off_ear).max(0.0);
        let length = longest * (0.42 + 0.58 * bell) * dice.range(0.92, 1.08);
        let shown = Leaf {
            collar: at + Vec3::UP * (collar - low) + level(heading) * wrapped,
            heading,
            length,
            width: length * form.broadest * dice.range(0.88, 1.12),
            rise: 1.0 - 0.5 * along + dice.range(-0.1, 0.1),
            droop: form.droop * (0.5 + 1.2 * length / longest) * dice.range(0.8, 1.2),
            fold: if form.ripe {
                dice.range(0.35, 0.65)
            } else {
                dice.range(0.12, 0.3)
            },
            twist: dice.range(-0.6, 0.6),
            drift: dice.range(-0.25, 0.25),
            dry: (form.dry.1
                + (form.dry.0 - form.dry.1) * (1.0 - smoothstep(0.0, 0.3, along))
                + dice.range(-0.1, 0.1))
            .clamp(0.0, 1.0),
        };
        sheet.leaf(
            &mut growing,
            &shown,
            (materials.leaf, mix32(key ^ index)),
            dice,
        )?;
        let here = i32::try_from(index).ok()?;
        if here == ear || (here + 1 == ear && dice.chance(0.3)) {
            let scale = if here == ear { 1.0 } else { 0.75 };
            cob(
                &mut growing,
                (at + Vec3::UP * (collar - low), heading, wrapped),
                (scale, form.ripe),
                (materials, key),
                dice,
            )?;
        }
    }
    roots(&mut growing, (foot, height), (materials.root, key), dice)?;
    tassel(
        &mut growing,
        (joint(count), radius(height), form.ripe),
        (materials.tassel, key),
        dice,
    )?;
    growing.finish()
}

/// An ear in its husks from the collar `at` on the stalk `radius` thick, on
/// the side `heading` its leaf springs from, `size` of a full ear's: held
/// up while green, bowed down once `ripe`; silks spilling from its tip.
fn cob(
    growing: &mut Growing,
    (at, heading, radius): (Vec3, f64, f64),
    (size, ripe): (f64, bool),
    (materials, key): (&Materials, u32),
    dice: &mut Dice,
) -> Option<()> {
    let off = if ripe {
        dice.range(1.6, 2.3)
    } else {
        dice.range(0.35, 0.6)
    };
    let way = level(heading) * mathf::sin(off) + Vec3::UP * mathf::cos(off);
    let shank = at + level(heading) * radius + way * 0.05;
    growing.limb(
        &[at + level(heading) * radius, shank],
        (0.008, 0.007),
        (materials.stalk, key),
    )?;
    let length = size * dice.range(0.18, 0.23);
    let thick = size * dice.range(0.024, 0.029);
    let tip = shank + way * length;
    growing.limb(&[shank, tip], (thick, 0.55 * thick), (materials.husk, key))?;
    // The husks close past the ear in a point.
    let flag = tip + way * (0.25 * length);
    growing.limb(
        &[tip, flag],
        (0.55 * thick, 0.12 * thick),
        (materials.husk, key),
    )?;
    let silks = dice.count(7, 12);
    for _ in 0..silks {
        let (spread, round) = (dice.range(0.3, 0.9), dice.range(0.0, TAU));
        let across = way.cross(Vec3::UP);
        let across = if across.length() > 1e-6 {
            across.normalized()
        } else {
            Vec3::new(1.0, 0.0, 0.0)
        };
        let aside = across * mathf::cos(round) + across.cross(way) * mathf::sin(round);
        let long = if ripe {
            dice.range(0.02, 0.04)
        } else {
            dice.range(0.04, 0.09)
        };
        let middle = flag + (way * mathf::cos(spread) + aside * mathf::sin(spread)) * (0.5 * long);
        let end = middle + (aside * 0.6 - Vec3::UP * 0.8).normalized() * (0.5 * long);
        growing.limb(
            &[flag, middle, end],
            (0.0007, 0.0005),
            (materials.silk, key),
        )?;
    }
    Some(())
}

/// The brace roots splaying from the foot of a stalk `foot` thick and
/// `height` tall, arching into the ground.
fn roots(
    growing: &mut Growing,
    (foot, height): (f64, f64),
    (material, key): (u16, u32),
    dice: &mut Dice,
) -> Option<()> {
    let count = dice.count(7, 12);
    for _ in 0..count {
        let round = level(dice.range(0.0, TAU));
        let from = round * (0.8 * foot) + Vec3::UP * dice.range(0.015, 0.035 * height);
        let out = dice.range(0.06, 0.12);
        let middle = from + round * (0.45 * out) + Vec3::UP * 0.008;
        let into = round * (foot + out) - Vec3::UP * 0.03;
        growing.limb(&[from, middle, into], (0.0038, 0.0024), (material, key))?;
    }
    Some(())
}

/// A tassel on top of the stalk ending at `top`, `radius` thick there: a
/// spike, its branches spreading and arching over from its lower part, dull
/// and drooping once `ripe`.
fn tassel(
    growing: &mut Growing,
    (top, radius, ripe): (Vec3, f64, bool),
    (material, key): (u16, u32),
    dice: &mut Dice,
) -> Option<()> {
    let length = dice.range(0.26, 0.36);
    let lean = level(dice.range(0.0, TAU)) * dice.range(0.02, 0.08);
    let spike = |along: f64| top + (Vec3::UP + lean * along) * (length * along);
    growing.limb(
        &[spike(0.0), spike(0.35), spike(0.7), spike(1.0)],
        (radius, 0.0012),
        (material, key),
    )?;
    let branches = dice.count(9, 15);
    let droop = if ripe { 1.0 } else { 0.55 };
    for _ in 0..branches {
        let from = spike(dice.range(0.05, 0.4));
        let out = level(dice.range(0.0, TAU));
        let long = dice.range(0.12, 0.24);
        let splay = dice.range(0.5, 0.9);
        let mut points = [from; 4];
        for (index, point) in points.iter_mut().enumerate().skip(1) {
            let along = f64::from(u32::try_from(index).ok()?) / 3.0;
            let bent = splay + droop * along * along * 1.6;
            *point = from + (out * mathf::sin(bent) + Vec3::UP * mathf::cos(bent)) * (long * along);
        }
        growing.limb(&points, (0.0014, 0.0007), (material, key))?;
    }
    Some(())
}

/// The plants of `form` its stands draw from, each a prototype planned on
/// `stage`, drawn from `seed`; `None` when the stage will not hold them.
fn plants(stage: &mut Stage, form: &Form, seed: u64) -> Option<[Plant; PLANTS]> {
    let materials = Materials::of(stage, form)?;
    let none = Plant {
        prototype: 0,
        material: 0,
        height: 0.0,
        reach: 0.0,
    };
    let mut plants = [none; PLANTS];
    for (index, slot) in plants.iter_mut().take(VARIANTS).enumerate() {
        let mut dice = Dice::keyed(seed, index);
        let (building, height, reach) = plant(&mut dice, form, &materials)?;
        *slot = Plant {
            prototype: stage.assemble(building)?,
            material: u32::from(materials.leaf),
            height,
            reach,
        };
    }
    Some(plants)
}

/// Stand `layout`'s maize fields lying near enough the eye at `eye` on
/// `land`, their plants built as first wanted from `seed`, thinning away over
/// `thinning`; `None` when the stage will not hold them.
pub(super) fn stand(
    stage: &mut Stage,
    (land, layout): (&Land, &Layout),
    eye: Point,
    (thinning, seed): ((f64, f64), u64),
) -> Option<()> {
    let mut built: [Option<[Plant; PLANTS]>; 2] = [None, None];
    for parcel in layout.parcels() {
        let field = &parcel.field;
        if parcel.usage.used != Use::Arable(Crop::Maize) || field.cell.distance(eye) > thinning.1 {
            continue;
        }
        let (code, rows) = land
            .grids
            .grows(&stage.fields, field.middle.x, field.middle.y);
        let Some(form) = form(Grown::of(code)) else {
            continue;
        };
        let slot = built.get_mut(usize::from(form.ripe))?;
        let plants = if let Some(plants) = *slot {
            plants
        } else {
            let made = plants(stage, form, seed ^ u64::from(form.ripe))?;
            *slot = Some(made);
            made
        };
        let sowing = Sowing {
            grows: (code, rows),
            drill: Drill::of(farmed::row_spacing(Crop::Maize), (code, rows)),
            apart: APART,
            planting: DRILLED,
            plants,
            kinds: VARIANTS,
            sizes: SIZES,
            posts: None,
            eye: (eye.x, eye.y),
            thinning,
            seed: Dice::keyed(seed ^ field_key(field.id), 1).seed(),
        };
        let Some(stood) = Stand::new((land.grids, &stage.fields), &field.cell, &sowing) else {
            continue;
        };
        stage.stand(stood, usize::try_from(plants[0].material).ok()?)?;
    }
    Some(())
}

#[cfg(test)]
#[path = "maize_tests.rs"]
mod tests;
