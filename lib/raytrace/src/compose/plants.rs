//! Trees, bushes and grass: a trunk and its limbs as tapering posts, each
//! limb ending in a crown of leaves; and lawns of blades over the ground.

use core::f64::consts::TAU;

use super::{direction, rgb, Dice, Stage};
use crate::foliage::Crown;
use crate::grass::Lawn;
use crate::material::{Finish, Material, Relief};
use crate::pigment::Pigment;
use crate::vector::Vec3;

/// The shapes a tree grows in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Habit {
    /// A broad, rounded crown on a few spreading limbs.
    Broad,
    /// A tall cone of tiers.
    Conifer,
    /// A slender trunk and a narrow, open crown.
    Slender,
    /// A bush: its crowns on the ground, and no trunk to see.
    Bush,
}

/// A kind of tree: its habit, its bark, and its leaves.
#[derive(Copy, Clone, Debug)]
pub(super) struct Species {
    pub(super) habit: Habit,
    pub(super) bark: (u32, u32),
    pub(super) leaves: [u32; 4],
    /// The colour the outermost leaves catch the light in.
    pub(super) tip: u32,
}

pub(super) const OAK: Species = Species {
    habit: Habit::Broad,
    bark: (0x6A_58_46, 0x2E_24_1C),
    leaves: [0x3A_62_22, 0x4A_72_2A, 0x2C_52_1A, 0x5A_80_30],
    tip: 0xA8_C0_50,
};
pub(super) const MAPLE: Species = Species {
    habit: Habit::Broad,
    bark: (0x5A_4A_3E, 0x28_20_1A),
    leaves: [0xC0_4A_1E, 0xE0_8C_28, 0xA8_2C_18, 0xD8_B0_30],
    tip: 0xF4_D0_60,
};
pub(super) const CHERRY: Species = Species {
    habit: Habit::Broad,
    bark: (0x4A_36_30, 0x22_18_14),
    leaves: [0xF4_C0_D4, 0xFF_E4_EE, 0xE8_A0_BC, 0xFF_F6_F8],
    tip: 0xFF_FF_FF,
};
pub(super) const OLIVE: Species = Species {
    habit: Habit::Broad,
    bark: (0x8A_80_74, 0x46_3E_36),
    leaves: [0x68_76_48, 0x7A_86_56, 0x56_64_3C, 0x8C_96_68],
    tip: 0xB8_C0_94,
};
pub(super) const FIR: Species = Species {
    habit: Habit::Conifer,
    bark: (0x5A_4232, 0x26_1A_12),
    leaves: [0x1E_3A_1C, 0x26_46_22, 0x18_32_18, 0x2E_50_28],
    tip: 0x4E_70_38,
};
pub(super) const PINE: Species = Species {
    habit: Habit::Conifer,
    bark: (0x9A_5E_3C, 0x46_28_18),
    leaves: [0x2A_48_24, 0x34_56_2A, 0x22_3E_1E, 0x40_62_30],
    tip: 0x6A_8A_44,
};
/// Snow lies on a fir's outer needles, so its tips are white.
pub(super) const SNOWY_FIR: Species = Species {
    habit: Habit::Conifer,
    bark: (0x5A_4232, 0x26_1A_12),
    leaves: [0x1C_34_1C, 0x24_40_22, 0x18_2E_18, 0x2A_48_28],
    tip: 0xF4_F8_FF,
};
pub(super) const BIRCH: Species = Species {
    habit: Habit::Slender,
    bark: (0xEC_E8_E0, 0x2A_26_22),
    leaves: [0x6A_94_34, 0x7C_A4_3C, 0x58_84_2C, 0x94_B4_4C],
    tip: 0xC8_DC_70,
};
pub(super) const GOLDEN_BIRCH: Species = Species {
    habit: Habit::Slender,
    bark: (0xEC_E8_E0, 0x2A_26_22),
    leaves: [0xE0_B8_30, 0xF0_CC_48, 0xC8_9C_24, 0xF8_DC_60],
    tip: 0xFF_F0_90,
};
pub(super) const POPLAR: Species = Species {
    habit: Habit::Slender,
    bark: (0x8A_84_76, 0x3A_36_30),
    leaves: [0x3E_6A_26, 0x4C_78_2E, 0x34_5C_20, 0x5C_86_34],
    tip: 0x9C_BC_4C,
};
pub(super) const BOX: Species = Species {
    habit: Habit::Bush,
    bark: (0x5A_4A_3E, 0x28_20_1A),
    leaves: [0x34_5A_22, 0x42_68_28, 0x2A_4C_1C, 0x50_74_2E],
    tip: 0x8C_AC_48,
};
pub(super) const HEATHER: Species = Species {
    habit: Habit::Bush,
    bark: (0x5A_4A_3E, 0x28_20_1A),
    leaves: [0x7A_4A_8A, 0x8C_5A_9A, 0x5E_3A_6C, 0x3E_5A_2A],
    tip: 0xB0_80_C0,
};

/// A species' bark and leaves, once made.
#[derive(Copy, Clone, Debug)]
pub(super) struct Grown {
    habit: Habit,
    bark: usize,
    leaves: usize,
}

impl Species {
    /// The species' materials, made in `stage`.
    pub(super) fn grow(&self, stage: &mut Stage, dice: &mut Dice) -> Option<Grown> {
        let bark = stage.material(
            Material::new(
                Pigment::Bark {
                    light: rgb(self.bark.0),
                    dark: rgb(self.bark.1),
                    scale: dice.range(3.0, 6.0),
                    seed: dice.seed(),
                },
                Finish::Coated { roughness: 0.85 },
            )
            .with_relief(Relief::Grain {
                depth: 0.3,
                scale: 5.0,
                seed: dice.seed(),
            }),
        )?;
        let leaves = stage.material(Material::new(
            crowd(self.leaves, self.tip, [self.tip; 4]),
            Finish::Leaf { translucency: 0.4 },
        ))?;
        Some(Grown {
            habit: self.habit,
            bark,
            leaves,
        })
    }
}

/// A crowd of `colours`, lightening toward `tip`, any flowers among them in
/// `blossoms`.
fn crowd(colours: [u32; 4], tip: u32, blossoms: [u32; 4]) -> Pigment {
    Pigment::Crowd {
        colours: colours.map(rgb),
        tip: rgb(tip),
        blossoms: blossoms.map(rgb),
    }
}

/// A tree of `grown` standing at `base`, about `height` tall.
pub(super) fn tree(
    stage: &mut Stage,
    dice: &mut Dice,
    base: Vec3,
    height: f64,
    grown: Grown,
) -> Option<()> {
    match grown.habit {
        Habit::Broad => broad(stage, dice, base, height, grown),
        Habit::Conifer => conifer(stage, dice, base, height, grown),
        Habit::Slender => slender(stage, dice, base, height, grown),
        Habit::Bush => bush(stage, dice, base, height, grown),
    }
}

/// How far a trunk is sunk below where it stands, so it meets sloping
/// ground without a gap.
const ROOTED: f64 = 0.3;

fn broad(stage: &mut Stage, dice: &mut Dice, base: Vec3, height: f64, grown: Grown) -> Option<()> {
    let girth = height * dice.range(0.028, 0.04);
    let bole = height * dice.range(0.25, 0.45);
    let lean = Vec3::new(dice.range(-0.1, 0.1), 1.0, dice.range(-0.1, 0.1)).normalized();
    let fork = base + lean * bole;
    stage.limb(
        base - Vec3::UP * ROOTED,
        fork,
        (girth, girth * 0.72),
        grown.bark,
    )?;
    let spread = height * dice.range(0.24, 0.38);
    let limbs = dice.count(3, 6);
    let turn = dice.range(0.0, TAU);
    for limb in 0..limbs {
        let heading = turn + TAU * f64::from(limb) / f64::from(limbs) + dice.range(-0.5, 0.5);
        let rise = dice.angle(25.0, 70.0);
        let end = fork + direction(heading, 0.0, rise) * (height * dice.range(0.15, 0.32));
        stage.limb(fork, end, (girth * 0.55, girth * 0.25), grown.bark)?;
        let radius = spread * dice.range(0.45, 0.75);
        let radii = Vec3::new(
            radius * dice.range(0.85, 1.15),
            radius * dice.range(0.6, 0.9),
            radius * dice.range(0.85, 1.15),
        );
        crown(
            stage,
            dice,
            (end + Vec3::UP * (0.3 * radius), radii),
            grown.leaves,
            BROADLEAF,
        )?;
    }
    // The crown's top, as often off the trunk's line as on it.
    let top = fork + lean * ((height - bole) * dice.range(0.35, 0.6));
    let drift = Vec3::new(dice.range(-0.25, 0.25), 0.0, dice.range(-0.25, 0.25)) * spread;
    let radii = Vec3::new(
        spread * dice.range(0.8, 1.0),
        spread * dice.range(0.65, 0.85),
        spread * dice.range(0.8, 1.0),
    );
    crown(stage, dice, (top + drift, radii), grown.leaves, BROADLEAF).map(|_| ())
}

fn conifer(
    stage: &mut Stage,
    dice: &mut Dice,
    base: Vec3,
    height: f64,
    grown: Grown,
) -> Option<()> {
    let girth = height * dice.range(0.018, 0.026);
    stage.limb(
        base - Vec3::UP * ROOTED,
        base + Vec3::UP * (0.92 * height),
        (girth, girth * 0.3),
        grown.bark,
    )?;
    // Tiers overlapping enough that together they draw one cone.
    let tiers = dice.count(7, 11);
    let skirt = height * dice.range(0.19, 0.26);
    let low = height * dice.range(0.08, 0.2);
    let spacing = (0.93 * height - low) / f64::from(tiers - 1);
    for tier in 0..tiers {
        let t = f64::from(tier) / f64::from(tiers - 1);
        let radius = skirt * (1.0 - 0.9 * t) * dice.range(0.92, 1.08);
        let radii = Vec3::new(radius, (0.5 * radius).max(1.1 * spacing), radius);
        let centre = base + Vec3::UP * (low + spacing * f64::from(tier));
        crown(stage, dice, (centre, radii), grown.leaves, NEEDLES)?;
    }
    Some(())
}

fn slender(
    stage: &mut Stage,
    dice: &mut Dice,
    base: Vec3,
    height: f64,
    grown: Grown,
) -> Option<()> {
    let girth = height * dice.range(0.015, 0.02);
    let lean = Vec3::new(dice.range(-0.05, 0.05), 1.0, dice.range(-0.05, 0.05)).normalized();
    stage.limb(
        base - Vec3::UP * ROOTED,
        base + lean * (0.8 * height),
        (girth, girth * 0.45),
        grown.bark,
    )?;
    let clumps = dice.count(2, 3);
    for clump in 0..clumps {
        let rise = 0.5 + 0.3 * f64::from(clump) / f64::from(clumps);
        let radius = height * dice.range(0.11, 0.16);
        let drift = Vec3::new(dice.range(-0.4, 0.4), 0.0, dice.range(-0.4, 0.4)) * radius;
        let centre = base + lean * (rise * height) + drift;
        crown(
            stage,
            dice,
            (centre, Vec3::new(radius, 1.6 * radius, radius)),
            grown.leaves,
            OPEN,
        )?;
    }
    Some(())
}

fn bush(stage: &mut Stage, dice: &mut Dice, base: Vec3, height: f64, grown: Grown) -> Option<()> {
    for _ in 0..dice.count(1, 3) {
        let radius = 0.5 * height * dice.range(0.7, 1.0);
        let offset = Vec3::new(dice.range(-0.5, 0.5), 0.0, dice.range(-0.5, 0.5)) * height;
        let centre = base + offset + Vec3::UP * (0.7 * radius);
        crown(
            stage,
            dice,
            (centre, Vec3::new(radius, 0.8 * radius, radius)),
            grown.leaves,
            SCRUB,
        )?;
    }
    Some(())
}

/// How a crown is leaved: the share of its cells that hold a leaf at its
/// surface and at its heart, each drawn from a range; a leaf's breadth over
/// its length; and how far the leaves turn to face up and out.
#[derive(Copy, Clone, Debug)]
struct Leafage {
    surface: (f64, f64),
    heart: (f64, f64),
    breadth: f64,
    lift: f64,
}

/// A broadleaf's crown: dense outside, open within, so light comes through.
const BROADLEAF: Leafage = Leafage {
    surface: (0.55, 0.75),
    heart: (0.08, 0.18),
    breadth: 0.55,
    lift: 0.45,
};
/// A conifer's tiers: needles packed close all through.
const NEEDLES: Leafage = Leafage {
    surface: (0.85, 0.95),
    heart: (0.35, 0.5),
    breadth: 0.45,
    lift: 0.6,
};
/// A slender tree's airy clumps.
const OPEN: Leafage = Leafage {
    surface: (0.5, 0.7),
    heart: (0.08, 0.15),
    breadth: 0.6,
    lift: 0.25,
};
/// A bush, thick with leaves.
const SCRUB: Leafage = Leafage {
    surface: (0.7, 0.85),
    heart: (0.2, 0.35),
    breadth: 0.6,
    lift: 0.35,
};

/// A crown of `radii` about `centre` in `leaves`, leaved as `leafage` has it.
fn crown(
    stage: &mut Stage,
    dice: &mut Dice,
    (centre, radii): (Vec3, Vec3),
    leaves: usize,
    leafage: Leafage,
) -> Option<usize> {
    let least = radii.x.min(radii.y).min(radii.z);
    let cell = (least / 16.0).clamp(0.05, 0.3);
    stage.crown(
        Crown {
            centre,
            radii,
            cell,
            leaf: cell * dice.range(0.36, 0.44),
            breadth: leafage.breadth,
            surface: dice.range(leafage.surface.0, leafage.surface.1),
            heart: dice.range(leafage.heart.0, leafage.heart.1),
            lift: leafage.lift,
            seed: dice.seed(),
        },
        leaves,
    )
}

/// A kind of grass: its blades, the colour their tips fade to, and its
/// flowers.
#[derive(Copy, Clone, Debug)]
pub(super) struct Sward {
    pub(super) blades: [u32; 4],
    pub(super) tip: u32,
    pub(super) blossoms: [u32; 4],
}

pub(super) const SPRING: Sward = Sward {
    blades: [0x4A_7A_2A, 0x5A_8A_30, 0x3C_6A_22, 0x6A_94_38],
    tip: 0xB0_C8_60,
    blossoms: [0xF8_F4_F0, 0xF4_D0_30, 0xC8_3A_3A, 0x8A_6A_D8],
};
pub(super) const SUMMER: Sward = Sward {
    blades: [0x5A_7A_2C, 0x6E_88_34, 0x4C_6C_26, 0x84_94_40],
    tip: 0xC8_C4_70,
    blossoms: [0xF8_F4_F0, 0xE8_60_30, 0xF0_C0_30, 0xC8_60_B0],
};
pub(super) const HAY: Sward = Sward {
    blades: [0xA8_94_58, 0xB8_A464, 0x8C_7C_48, 0xC4_B0_70],
    tip: 0xE0_D0_98,
    blossoms: [0xF0_E8_D0, 0xE8_C0_40, 0xD0_80_40, 0xF8_F0_E8],
};
pub(super) const MOOR: Sward = Sward {
    blades: [0x6A_6A_34, 0x7C_78_3C, 0x58_5A_2C, 0x8C_84_48],
    tip: 0xB8_AC_70,
    blossoms: [0x9A_5A_AA, 0xB0_70_C0, 0xF4_E8_F0, 0xE8_C8_40],
};

/// How a lawn grows: its blades' least and greatest height, how far they
/// lean, and the share that flower.
#[derive(Copy, Clone, Debug)]
pub(super) struct Growth {
    pub(super) height: (f64, f64),
    pub(super) lean: f64,
    pub(super) flowers: f64,
}

/// A lawn of `sward` growing as `growth` has it over the rectangle `from`–
/// `to` of the scene's height grid `field`, whose height `height_at` gives
/// before the grid is filled.
pub(super) fn lawn(
    stage: &mut Stage,
    dice: &mut Dice,
    (field, height_at): (u32, &dyn Fn(f64, f64) -> f64),
    (from, to): ((f64, f64), (f64, f64)),
    sward: &Sward,
    growth: Growth,
) -> Option<usize> {
    let material = stage.material(Material::new(
        crowd(sward.blades, sward.tip, sward.blossoms),
        Finish::Leaf { translucency: 0.3 },
    ))?;
    let (mut floor, mut ceiling) = (f64::INFINITY, f64::NEG_INFINITY);
    let steps = 32u32;
    for i in 0..=steps {
        for j in 0..=steps {
            let x = from.0 + (to.0 - from.0) * f64::from(i) / f64::from(steps);
            let z = from.1 + (to.1 - from.1) * f64::from(j) / f64::from(steps);
            let at = height_at(x, z);
            floor = floor.min(at);
            ceiling = ceiling.max(at);
        }
    }
    // The samples can miss a crest or a hollow between them.
    let slack = 0.1 + 0.05 * (ceiling - floor);
    let tallest = growth.height.1;
    let cell = (1.1 * tallest).clamp(0.08, 0.2);
    stage.lawn(
        Lawn {
            field,
            from,
            to,
            floor: floor - slack,
            ceiling: ceiling + slack,
            cell,
            blades: dice.count(5, 8),
            height: growth.height,
            width: 0.006 + 0.008 * (tallest / 0.5).min(1.5),
            lean: growth.lean,
            flowers: growth.flowers,
            seed: dice.seed(),
        },
        material,
    )
}
