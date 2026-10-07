//! Trees, shrubs and grass: the kinds a scene grows, their bark and their
//! leaves through the year, and lawns of blades over the ground.
//!
//! Each kind of tree is grown a few times over from seeds of its own, each a
//! prototype the scene places as often as it likes, turned and sized, so a
//! wood of hundreds of trees holds a handful of trees' worth of limbs and
//! leaves.

use alloc::vec::Vec;
use core::f64::consts::TAU;

use tairix_parallel::JobRunner;
use tairix_util::{fallible, mathf};

use super::crops::Sowing;
use super::{rgb, Dice, Recipe, Stage};
use crate::bark::{Bark, BarkKind};
use crate::cactus::{Flesh, Ribs, FELT, RIB_DEPTH};
use crate::deadwood::{Decay, Fungus, Habit as Shelving, Sprouting, Top, Woods};
use crate::fracture::Grain;
use crate::grass::{
    Cover, Grass, GrassKind, Habit as Tufting, Head, Lawn, Litter, Seen, Sown, Weeds, GRASS_KINDS,
    WILD_KINDS,
};
use crate::leaf::Outline;
use crate::material::{Finish, Material, Relief};
use crate::noise::smoothstep;
use crate::pigment::{Blades, Crowd, Foliage, Pigment};
use crate::shade::{Rect, Sampling, Shade, Shades};
use crate::shape::Shape;
use crate::tree::{Envelope, Fruit, Leafing, Level, Season, Species, Stock};
use crate::vector::{real, share, Frame, Pose, Vec3};
use crate::wood::{Affinity, Habit, KINDS, VARIANTS};

/// The kinds of tree and shrub a scene can grow.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Kind {
    Oak,
    Maple,
    Birch,
    Beech,
    Willow,
    Poplar,
    Pine,
    Spruce,
    Olive,
    Cherry,
    /// An orchard's apple: low and spreading, pruned open, pink-white with
    /// blossom in spring and hung with apples by autumn.
    Apple,
    Palm,
    Saguaro,
    Hazel,
    /// The hedge's thorn: dense and twiggy, white with blossom in May, red
    /// with haws in autumn.
    Hawthorn,
    Box,
    Heather,
    Gorse,
    Fern,
}

/// How a tree grew up: alone in the open, spreading as wide as it likes;
/// close among others, reaching up for the light and shedding the limbs it
/// shades; or young, in the shade of the grown.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Stand {
    Open,
    Close,
    Young,
    /// Dead where it stands: its trunk snapped short of its top, its limbs
    /// broken back to their stubs.
    Dead,
}

impl Kind {
    /// Whether this kind is a shrub, which grows as it does however it
    /// stands.
    pub(super) const fn shrub(self) -> bool {
        matches!(
            self,
            Self::Hazel | Self::Hawthorn | Self::Box | Self::Heather | Self::Gorse | Self::Fern
        )
    }

    /// How far a grown tree of this kind spreads its crown from its trunk,
    /// as a share of its height, as it stands.
    pub(super) const fn crown(self, stand: Stand) -> f64 {
        // In the open, and dead: a snag keeps only its limbs' broken stubs,
        // drooping on a willow and held close about a poplar's trunk.
        let (open, dead) = match self {
            Self::Fern => (0.95, 0.95),
            Self::Heather | Self::Gorse => (0.5, 0.5),
            Self::Oak => (0.5, 0.15),
            Self::Hazel => (0.52, 0.52),
            Self::Hawthorn => (0.48, 0.48),
            Self::Apple => (0.55, 0.2),
            Self::Box => (0.47, 0.47),
            Self::Olive => (0.43, 0.135),
            Self::Beech => (0.42, 0.13),
            Self::Willow => (0.42, 0.11),
            Self::Maple | Self::Cherry => (0.41, 0.126),
            Self::Palm => (0.35, 0.112),
            Self::Pine => (0.33, 0.12),
            Self::Spruce => (0.33, 0.115),
            Self::Birch => (0.25, 0.1),
            Self::Saguaro => (0.12, 0.045),
            Self::Poplar => (0.09, 0.042),
        };
        match stand {
            _ if self.shrub() => open,
            Stand::Open => open,
            Stand::Close => 0.78 * open,
            Stand::Young => 0.95 * open,
            Stand::Dead => dead,
        }
    }
}

/// The share of its kind's least grown height the shortest grown tree of a
/// kind stands.
const YOUNGEST: f64 = 0.5;

/// A kind's grown trees.
#[derive(Copy, Clone, Debug)]
pub(super) struct Grown {
    pub(super) kind: Kind,
    pub(super) habit: Habit,
}

/// The kinds a scene grows, grown.
#[derive(Clone, Debug)]
pub(super) struct Grove {
    grown: [Option<Grown>; KINDS],
}

impl Grove {
    /// The first [`KINDS`] of `kinds`, each grown a few times for `season`
    /// as `stand` has them; its trees to be grown before the scene is
    /// traced. `None` when the heap will not hold them.
    pub(super) fn new(
        stage: &mut Stage,
        dice: &mut Dice,
        (kinds, season): (&[Kind], Season),
        stand: Stand,
    ) -> Option<Self> {
        let mut grown = [None; KINDS];
        for (slot, &kind) in grown.iter_mut().zip(kinds) {
            *slot = Some(grow(stage, dice, (kind, stand), season)?);
        }
        Some(Self { grown })
    }

    /// The grove's kinds, grown.
    pub(super) fn kinds(&self) -> impl Iterator<Item = &Grown> + '_ {
        self.grown.iter().flatten()
    }

    /// The grove's kinds as a wood reads them, in order.
    pub(super) fn habits(&self) -> [Option<Habit>; KINDS] {
        self.grown.map(|grown| grown.map(|grown| grown.habit))
    }

    /// The first of the grove's kinds.
    pub(super) fn first(&self) -> Option<Grown> {
        self.kinds().next().copied()
    }

    /// The grove's kind `kind`, if it grows one.
    pub(super) fn of(&self, kind: Kind) -> Option<Grown> {
        self.kinds().find(|grown| grown.kind == kind).copied()
    }

    /// This grove's kinds and then `other`'s, as many as a grove holds.
    pub(super) fn and(mut self, other: &Self) -> Self {
        let mut more = other.kinds().copied();
        for slot in self.grown.iter_mut().filter(|slot| slot.is_none()) {
            *slot = more.next();
        }
        self
    }
}

/// How many fallen trunks, and stumps, of each kind a scene grows to choose
/// among.
pub(super) const DEAD_VARIANTS: usize = 3;

/// What a kind leaves dead: its fallen trunks, each its length and how thick
/// it is at its foot; its stumps, each how tall and thick; and its standing
/// dead. They have lain as long as the wood's dead of their kind have, their
/// bark weathered, mossed and sloughing, their hearts rotting, rot fruiting
/// on them in brackets, and a broadleaf's stumps sending up shoots.
#[derive(Copy, Clone, Debug)]
pub(super) struct Dead {
    pub(super) logs: [(u32, f64, f64); DEAD_VARIANTS],
    pub(super) stumps: [(u32, f64, f64); DEAD_VARIANTS],
    pub(super) snags: Grown,
    bark: usize,
}

impl Dead {
    /// `kind`'s dead in `season`, to be grown before the scene is traced;
    /// `None` when the heap will not hold them.
    pub(super) fn new(
        stage: &mut Stage,
        dice: &mut Dice,
        (kind, season): (Kind, Season),
    ) -> Option<Self> {
        let age = dice.range(0.15, 0.95);
        let rotting = Rotting::new(stage, dice, (kind, season), age)?;
        let species = species(kind);
        let typical = f64::midpoint(species.height.0, species.height.1);
        let girth = typical * species.girth;
        let mut logs = [(0, 0.0, 0.0); DEAD_VARIANTS];
        for (variant, log) in logs.iter_mut().enumerate() {
            let grown = (real(variant) + dice.range(0.0, 1.0)) / real(DEAD_VARIANTS);
            let (length, radius) = (
                typical * (0.35 + 0.55 * grown),
                girth * (0.55 + 0.6 * grown),
            );
            let recipe = Recipe::Log {
                length,
                radius,
                woods: rotting.torn,
                thrown: dice.chance(0.5),
                decay: rotting.decay(dice),
                seed: dice.wide(),
            };
            *log = (stage.plan(&recipe)?, length, radius);
        }
        let mut stumps = [(0, 0.0, 0.0); DEAD_VARIANTS];
        for stump in &mut stumps {
            let (height, radius) = (dice.range(0.25, 1.1), girth * dice.range(0.7, 1.2));
            let top = if dice.chance(0.5) {
                Top::Snapped
            } else {
                Top::Sawn
            };
            let decay = rotting.decay(dice);
            // A cut stump sprouts more readily than a snapped one, and a
            // rotting one not at all.
            let sprouts = match top {
                Top::Sawn => 0.5,
                Top::Snapped => 0.3,
            } * (1.0 - smoothstep(0.5, 0.75, decay.age));
            let sprouting = rotting.sprouting.filter(|_| dice.chance(sprouts));
            let recipe = Recipe::Stump {
                height,
                radius,
                top,
                woods: rotting.sawn,
                decay,
                sprouting,
                seed: dice.wide(),
            };
            *stump = (stage.plan(&recipe)?, height, radius);
        }
        Some(Self {
            logs,
            stumps,
            snags: grow(stage, dice, (kind, Stand::Dead), season)?,
            bark: rotting.bark,
        })
    }

    /// Lay fallen trunk or stump `prototype`, `scale` times its size, posed
    /// at `pose`, set apart from the rest by `key`.
    pub(super) fn lay(
        &self,
        stage: &mut Stage,
        (prototype, scale): (u32, f64),
        pose: Pose,
        key: u32,
    ) -> Option<()> {
        lay_dead(stage, self.bark, (prototype, scale), (pose, key))
    }
}

/// How a kind's dead rot in a wood: the bark they wear, the materials their
/// torn and their sawn wood is made in, how long they have lain, the fungus
/// fruiting on them, and the shoots a broadleaf's stumps send up.
#[derive(Copy, Clone, Debug)]
struct Rotting {
    bark: usize,
    torn: Woods,
    sawn: Woods,
    age: f64,
    fungus: Fungus,
    sprouting: Option<Sprouting>,
}

impl Rotting {
    /// How `kind`'s dead rot in `season`, having lain about `age`, in
    /// materials of `stage`'s; `None` when the stage will not hold them.
    fn new(
        stage: &mut Stage,
        dice: &mut Dice,
        (kind, season): (Kind, Season),
        age: f64,
    ) -> Option<Self> {
        let bark = dead_bark_material(stage, dice, (kind, season), age)?;
        let stock = u16::try_from(bark).ok()?;
        let rot = u16::try_from(rot_material(stage, dice)?).ok()?;
        // The soil a thrown trunk tore up.
        let edge = u16::try_from(edge_material(stage, dice)?).ok()?;
        // Dark topsoil flecked with the paler subsoil and grit it held.
        let soil = u16::try_from(
            stage.material(
                Material::new(
                    Pigment::Speckle {
                        base: rgb(0x34_28_1E),
                        flecks: [rgb(0x5A_48_36), rgb(0x22_1A_14)],
                        scale: 40.0,
                        seed: dice.seed(),
                    },
                    Finish::Matte,
                )
                .with_relief(Relief::grain(0.45, 30.0, dice.seed())),
            )?,
        )
        .ok()?;
        let torn = Woods {
            bark: stock,
            wood: u16::try_from(dead_wood_material(stage, age)?).ok()?,
            rot,
            edge,
            soil,
        };
        // A sawn face shows the tree's rings about its own axis, tan where it
        // is fresh, weathering grey-brown and darker.
        let weathered = smoothstep(0.0, 0.6, age);
        let rings = stage.material(
            Material::new(
                Pigment::Wood {
                    light: rgb(0x9A_86_6A).lerp(rgb(0x6A_62_58), weathered),
                    dark: rgb(0x6E_5A_44).lerp(rgb(0x4A_44_3C), weathered),
                    scale: dice.range(150.0, 300.0),
                    seed: dice.seed(),
                },
                Finish::Matte,
            )
            .with_relief(Relief::grain(0.05, 90.0, dice.seed())),
        )?;
        let sawn = Woods {
            wood: u16::try_from(rings).ok()?,
            ..torn
        };
        Some(Self {
            bark,
            torn,
            sawn,
            age,
            fungus: fungus(stage, dice, kind)?,
            sprouting: if coppices(kind) {
                Some(sprouts(stage, dice, (kind, season))?)
            } else {
                None
            },
        })
    }

    /// One piece's decay, drawn about the wood's: older or younger by a few
    /// seasons, and fruiting the likelier the longer it has lain.
    fn decay(&self, dice: &mut Dice) -> Decay {
        let age = (self.age + dice.range(-0.2, 0.2)).clamp(0.0, 1.0);
        Decay {
            age,
            fungus: dice.chance(0.15 + 0.6 * age).then_some(self.fungus),
        }
    }
}

/// The bracket fungus rot fruits in on `kind`'s dead, as materials of
/// `stage`'s: a birch's own polypore, pale and thick, or elsewhere as often
/// turkey tail's thin banded tiers as an artist's bracket's woody shelves;
/// `None` when the stage will not hold them.
fn fungus(stage: &mut Stage, dice: &mut Dice, kind: Kind) -> Option<Fungus> {
    let (habit, [first, second], margin, pores) = if kind == Kind::Birch {
        (
            Shelving::Thick,
            [0xB8_A0_80, 0xC8_B4_98],
            0xD8_CC_B4,
            0xF2_EE_E4,
        )
    } else if dice.chance(0.6) {
        let zones = dice.pick(&[
            [0x6A_5A_4A, 0x9A_8E_7A],
            [0x5A_4A_3A, 0xA8_8A_5A],
            [0x4E_56_5E, 0x8A_8E_88],
        ])?;
        (Shelving::Thin, zones, 0xE8_E0_CC, 0xD8_D0_BC)
    } else {
        (
            Shelving::Thick,
            [0x006A_4A32, 0x8A_6A_4A],
            0xE0_D8_C8,
            0xF0_EC_E0,
        )
    };
    let made = |colour: u32, stage: &mut Stage| {
        u16::try_from(stage.material(Material::new(Pigment::Solid(rgb(colour)), Finish::Matte))?)
            .ok()
    };
    Some(Fungus {
        habit,
        zones: [made(first, stage)?, made(second, stage)?],
        margin: made(margin, stage)?,
        pores: made(pores, stage)?,
    })
}

/// Whether `kind`'s stumps send up shoots from their foot, as a broadleaf's
/// do.
fn coppices(kind: Kind) -> bool {
    !matches!(
        kind,
        Kind::Pine | Kind::Spruce | Kind::Palm | Kind::Saguaro | Kind::Fern
    )
}

/// The young shoots `kind`'s stumps send up from their foot in `season`, in
/// their own bark, leafy but in winter; `None` when the stage will not hold
/// their materials.
fn sprouts(
    stage: &mut Stage,
    dice: &mut Dice,
    (kind, season): (Kind, Season),
) -> Option<Sprouting> {
    let young = Bark {
        kind: BarkKind::Smooth,
        light: rgb(0x6A_5A_3A),
        dark: rgb(0x3E_34_22),
        accent: rgb(0x5A_6A_3A),
        rise: 0.0,
        snow: if season == Season::Winter { 0.6 } else { 0.0 },
        moss: 0.0,
        bare: 0.0,
        seed: dice.seed(),
    };
    let bark = stage.material(Material::new(
        Pigment::Bark(young),
        Finish::Coated { roughness: 0.7 },
    ))?;
    let leaves = if season == Season::Winter || species(kind).leafing.per_twig == 0 {
        None
    } else {
        Some(
            u16::try_from(stage.material(Material::new(
                Pigment::Foliage(foliage(kind, season)),
                Finish::Leaf {
                    translucency: translucency(kind),
                },
            ))?)
            .ok()?,
        )
    };
    Some(Sprouting {
        bark: u16::try_from(bark).ok()?,
        leaves,
        leafing: species(kind).leafing,
    })
}

/// How many branches, and trunks, of a kind a stream's floods leave in it to
/// choose among.
const DRIFT_BRANCHES: usize = 4;
const DRIFT_TRUNKS: usize = 2;

/// What a stream's floods leave of a kind: the branches they broke off and
/// carried and a trunk or two undercut from the bank, their bark weathered
/// and mossed.
#[derive(Copy, Clone, Debug)]
pub(super) struct Drift {
    branches: [Piece; DRIFT_BRANCHES],
    trunks: [Piece; DRIFT_TRUNKS],
    bark: usize,
}

/// A piece of drift: its prototype, its length, and how thick at its foot.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(super) struct Piece {
    pub(super) prototype: u32,
    pub(super) length: f64,
    pub(super) radius: f64,
}

impl Drift {
    /// `kind`'s drift in `season`, to be grown before the scene is traced;
    /// `None` when the heap will not hold it.
    pub(super) fn new(
        stage: &mut Stage,
        dice: &mut Dice,
        (kind, season): (Kind, Season),
    ) -> Option<Self> {
        // What the floods carried was broken from the living or lately
        // dead: weathered, but not yet rotten.
        let age = dice.range(0.1, 0.4);
        let rotting = Rotting::new(stage, dice, (kind, season), age)?;
        let species = species(kind);
        let typical = f64::midpoint(species.height.0, species.height.1);
        let girth = typical * species.girth;
        let bark = rotting.bark;
        let mut log = |stage: &mut Stage, (length, radius): (f64, f64)| {
            let recipe = Recipe::Log {
                length,
                radius,
                woods: rotting.torn,
                thrown: false,
                decay: Decay {
                    age: rotting.age,
                    fungus: None,
                },
                seed: dice.wide(),
            };
            Some(Piece {
                prototype: stage.plan(&recipe)?,
                length,
                radius,
            })
        };
        let unset = Piece {
            prototype: 0,
            length: 0.0,
            radius: 0.0,
        };
        let mut branches = [unset; DRIFT_BRANCHES];
        for (variant, branch) in branches.iter_mut().enumerate() {
            let grown = share(variant, DRIFT_BRANCHES - 1);
            *branch = log(stage, (0.8 + 2.4 * grown, 0.025 + 0.065 * grown))?;
        }
        let mut trunks = [unset; DRIFT_TRUNKS];
        for (variant, trunk) in trunks.iter_mut().enumerate() {
            let grown = share(variant, DRIFT_TRUNKS - 1);
            *trunk = log(
                stage,
                (
                    typical * (0.35 + 0.2 * grown),
                    girth * (0.45 + 0.25 * grown),
                ),
            )?;
        }
        Some(Self {
            branches,
            trunks,
            bark,
        })
    }

    /// A trunk if `trunk`, else a branch, drawn from `dice`.
    pub(super) fn piece(&self, dice: &mut Dice, trunk: bool) -> Option<Piece> {
        let pieces: &[Piece] = if trunk { &self.trunks } else { &self.branches };
        let last = u32::try_from(pieces.len().checked_sub(1)?).ok()?;
        pieces
            .get(usize::try_from(dice.count(0, last)).ok()?)
            .copied()
    }

    /// Lay branch or trunk `prototype`, `scale` times its size, posed at
    /// `pose`, set apart from the rest by `key`.
    pub(super) fn lay(
        &self,
        stage: &mut Stage,
        (prototype, scale): (u32, f64),
        (pose, key): (Pose, u32),
    ) -> Option<()> {
        lay_dead(stage, self.bark, (prototype, scale), (pose, key))
    }
}

/// The weathered, mossed bark `kind`'s dead wear in `season`, having lain
/// `age`, as a material of `stage`'s; `None` when the stage will not hold
/// it.
fn dead_bark_material(
    stage: &mut Stage,
    dice: &mut Dice,
    (kind, season): (Kind, Season),
    age: f64,
) -> Option<usize> {
    // Bark loosens once the wood beneath has begun to rot, and sloughs away;
    // moss takes it the longer it lies.
    let pattern = Bark {
        bare: sloughs(kind) * smoothstep(0.2, 0.9, age),
        moss: 0.5 * smoothstep(0.3, 0.95, age),
        ..dead_bark(kind, dice.seed())
    };
    let snow = snowed(season);
    stage.material(
        Material::new(
            Pigment::Bark(Bark {
                snow,
                ..pattern.clone()
            }),
            Finish::Coated { roughness: 0.95 },
        )
        .with_relief(Relief::Bark {
            bark: pattern,
            depth: bark_depth(kind),
        }),
    )
}

/// The rotten wood a dead heart crumbles to, as a material of `stage`'s;
/// `None` when the stage will not hold it.
fn rot_material(stage: &mut Stage, dice: &mut Dice) -> Option<usize> {
    stage.material(
        Material::new(Pigment::Solid(rgb(0x3A_28_1A)), Finish::Matte).with_relief(Relief::grain(
            0.12,
            60.0,
            dice.seed(),
        )),
    )
}

/// The bark's own edge where it was torn or cut through, darker than the
/// bark's face or the wood within, as a material of `stage`'s; `None` when
/// the stage will not hold it.
fn edge_material(stage: &mut Stage, dice: &mut Dice) -> Option<usize> {
    stage.material(
        Material::new(Pigment::Solid(rgb(0x2A_20_18)), Finish::Matte).with_relief(Relief::grain(
            0.2,
            70.0,
            dice.seed(),
        )),
    )
}

/// The wood where dead wood broke, `age` as long as it has lain: tan where
/// it is fresh, weathering grey-brown; as a material of `stage`'s, `None`
/// when the stage will not hold it.
fn dead_wood_material(stage: &mut Stage, age: f64) -> Option<usize> {
    let colour = rgb(0x8A_74_58).lerp(rgb(0x5E_56_4C), smoothstep(0.0, 0.6, age));
    stage.material(
        Material::new(Pigment::Solid(colour), Finish::Matte)
            .with_relief(Relief::grain(0.15, 120.0, 0x3d)),
    )
}

/// Lay dead wood `prototype`, `scale` times its size, in `bark`, posed at
/// `pose` and set apart from the rest by `key`.
fn lay_dead(
    stage: &mut Stage,
    bark: usize,
    (prototype, scale): (u32, f64),
    (pose, key): (Pose, u32),
) -> Option<()> {
    stage
        .add(
            Shape::Instance {
                prototype,
                pose,
                scale,
                key,
            },
            bark,
            pose,
            false,
        )
        .map(|_| ())
}

/// The colours of what `kind` sheds on a wood's floor, `age` from this
/// autumn's fall to last year's, freshest and most rotted; the humus it rots
/// to; and how much of the floor beneath it moss carpets: its leaves as they
/// turned, browning as they lie, or its needles over a carpet of moss.
pub(super) fn litter(kind: Kind, age: f64) -> ([Vec3; 2], Vec3, f64) {
    let (humus, rotted) = (rgb(0x2E_22_18), rgb(0x4E_3A_2A));
    let (fresh, moss) = match kind {
        Kind::Pine | Kind::Spruce => (rgb(0x74_58_40), 0.4),
        _ => {
            let [a, b, c, d] = palette(kind, Season::Autumn { fallen: 100 }).map(rgb);
            ((a + b + c + d) * 0.25, 0.15)
        }
    };
    let lying = fresh.lerp(rotted, 0.35 + 0.6 * age.clamp(0.0, 1.0));
    ([lying, lying.lerp(rotted, 0.6)], humus, moss)
}

/// Plant a tree of `habit` at `base`, about `height` tall, turned any way.
pub(super) fn plant(
    stage: &mut Stage,
    dice: &mut Dice,
    habit: &Habit,
    base: Vec3,
    height: f64,
) -> Option<()> {
    let variant = habit.nearest(height, dice.unit());
    let natural = *habit.heights.get(variant)?;
    let pose = Pose::new(base, Frame::turned(dice.range(0.0, TAU), 0.0));
    place(
        stage,
        habit,
        (variant, habit.sized(height, natural)),
        (pose, dice.seed()),
    )
    .map(|_| ())
}

/// The least and most a grown plant is scaled by: never so far its limbs and
/// leaves stop looking the size they are, and a saguaro, whose areoles and
/// spines grow with it, by less.
const SCALED: (f64, f64) = (0.75, 1.35);
const SAGUARO_SCALED: (f64, f64) = (0.88, 1.14);

/// How readily `kind` takes to ground against the other kinds of its wood:
/// willows and poplars to the wet, pines to the dry and poor, beeches to deep
/// and well-drained soil, birches wherever others give way.
const fn affinity(kind: Kind) -> Affinity {
    let (base, wet, rich, least) = match kind {
        Kind::Willow | Kind::Poplar => (0.25, 1.6, 0.0, 0.0),
        Kind::Pine => (1.7, -0.6, -0.4, 0.0),
        Kind::Spruce => (0.8, 0.5, 0.0, 0.0),
        Kind::Beech => (1.0, -1.2, 0.5, 0.1),
        Kind::Oak => (1.0, -0.6, 0.3, 0.1),
        Kind::Maple => (0.9, 0.0, 0.4, 0.0),
        Kind::Birch => (1.3, 0.0, -0.5, 0.0),
        Kind::Fern => (0.7, 0.8, 0.0, 0.0),
        _ => return Affinity::EVEN,
    };
    Affinity {
        base,
        wet,
        rich,
        least,
    }
}

/// Stand grown tree `variant` of `habit`, `scale` times its size, at `pose`,
/// set apart from the rest by `key`, its crown recorded on the stage: how far
/// the crown reaches.
pub(super) fn place(
    stage: &mut Stage,
    habit: &Habit,
    (variant, scale): (usize, f64),
    (pose, key): (Pose, u32),
) -> Option<f64> {
    let (prototype, natural) = (
        *habit.prototypes.get(variant)?,
        *habit.heights.get(variant)?,
    );
    stage.add(
        Shape::Instance {
            prototype,
            pose,
            scale,
            key,
        },
        habit.bark,
        pose,
        false,
    )?;
    let reach = habit.crown * natural * scale;
    stage.canopy((pose.at.x, pose.at.z), reach)?;
    Some(reach)
}

/// Grow `kind` for `season` as `stand` has it, in materials of its own: its
/// trees queued for growing.
fn grow(
    stage: &mut Stage,
    dice: &mut Dice,
    (kind, stand): (Kind, Stand),
    season: Season,
) -> Option<Grown> {
    if kind == Kind::Saguaro {
        return grow_saguaro(stage, dice, season);
    }
    let snow = snowed(season);
    let pattern = bark(kind, dice.seed());
    let bark = stage.material(
        Material::new(
            Pigment::Bark(Bark {
                snow,
                ..pattern.clone()
            }),
            Finish::Coated { roughness: 0.9 },
        )
        .with_relief(Relief::Bark {
            bark: pattern,
            depth: bark_depth(kind),
        }),
    )?;
    // The dead bear no leaves, whatever the season.
    let leafing = if stand == Stand::Dead {
        Season::Winter
    } else {
        season
    };
    let leaves = stage.material(Material::new(
        Pigment::Foliage(foliage(kind, leafing)),
        Finish::Leaf {
            translucency: translucency(kind),
        },
    ))?;
    // Where its limbs snapped, the wood within and the bark's torn edge.
    let grain = Grain {
        wood: u16::try_from(dead_wood_material(stage, 0.4)?).ok()?,
        rot: u16::try_from(rot_material(stage, dice)?).ok()?,
        edge: u16::try_from(edge_material(stage, dice)?).ok()?,
    };
    // An apple bears small green fruit through the summer, swelling red by
    // autumn.
    let fruit = match (kind, leafing) {
        (Kind::Apple, Season::Summer) => Some((0x86_A2_40, 0.022, 0.008)),
        (Kind::Apple, Season::Autumn { .. }) => Some((0xA0_24_1A, 0.034, 0.012)),
        _ => None,
    };
    let fruit = match fruit {
        Some((colour, radius, share)) => Some(Fruit {
            material: u16::try_from(stage.material(Material::new(
                Pigment::Solid(rgb(colour)),
                Finish::Coated { roughness: 0.3 },
            ))?)
            .ok()?,
            radius,
            share,
        }),
        None => None,
    };
    let stock = Stock {
        bark: u16::try_from(bark).ok()?,
        leaves: u16::try_from(leaves).ok()?,
        grain,
        fruit,
    };
    let roots = if kind == Kind::Palm {
        u16::try_from(palm_roots(stage, dice)?).ok()?
    } else {
        stock.bark
    };
    let (prototypes, heights) = variants(stage, dice, (kind, stand, leafing), (stock, roots))?;
    Some(Grown {
        kind,
        habit: Habit {
            affinity: affinity(kind),
            prototypes,
            heights,
            bark,
            crown: kind.crown(stand),
            scaled: SCALED,
        },
    })
}

/// Plan the variants of `kind` standing as `stand` in the `leafing` season,
/// of `stock` and rooted in `roots`: each one's prototype and its height.
fn variants(
    stage: &mut Stage,
    dice: &mut Dice,
    (kind, stand, leafing): (Kind, Stand, Season),
    (stock, roots): (Stock, u16),
) -> Option<([u32; VARIANTS], [f64; VARIANTS])> {
    let mut prototypes = [0u32; VARIANTS];
    let mut heights = [0.0f64; VARIANTS];
    for (variant, (prototype, height)) in prototypes.iter_mut().zip(heights.iter_mut()).enumerate()
    {
        let seed = dice.wide();
        let recipe = if kind == Kind::Palm {
            *height = dice.range(9.0, 16.0);
            Recipe::Palm {
                height: *height,
                stock,
                roots,
                fronds: u16::try_from(dice.count(13, 18)).ok()?,
                seed,
            }
        } else if kind == Kind::Fern {
            *height = dice.range(0.4, 1.1);
            Recipe::Fern {
                height: *height,
                stock,
                fronds: u16::try_from(dice.count(9, 16)).ok()?,
                seed,
            }
        } else {
            let species = stood(kind, stand);
            let (least, most) = (YOUNGEST * species.height.0, species.height.1);
            let share = (real(variant) + dice.range(-0.2, 0.2)) / real(VARIANTS - 1);
            *height = least + (most - least) * share.clamp(0.0, 1.0);
            Recipe::Tree {
                species,
                height: *height,
                season: leafing,
                stock,
                seed,
            }
        };
        *prototype = stage.plan(&recipe)?;
    }
    Some((prototypes, heights))
}

/// The material a palm's mat of roots is made in: smooth, tan where it is
/// fresh and greyed where the sun has had it, ringed faintly where its
/// rootlets grew.
fn palm_roots(stage: &mut Stage, dice: &mut Dice) -> Option<usize> {
    let skin = Bark {
        kind: BarkKind::Taproot,
        light: rgb(0x6E_5A_46),
        dark: rgb(0x36_2C_22),
        accent: rgb(0x6A_64_5C),
        rise: 0.0,
        snow: 0.0,
        moss: 0.0,
        bare: 0.0,
        seed: dice.seed(),
    };
    stage.material(
        Material::new(
            Pigment::Bark(skin.clone()),
            Finish::Coated { roughness: 0.75 },
        )
        .with_relief(Relief::Bark {
            bark: skin,
            depth: 0.0008,
        }),
    )
}

/// A saguaro's skin: dull grey-green on its crests and darker down its
/// grooves, its young areoles' felt a greyish brown.
const FLESH: (u32, u32, u32) = (0x7A_8A_62, 0x42_4E_38, 0x6E_60_4E);

/// A saguaro's girth for its height: thickening as it grows taller, to about
/// a third of a metre in radius on the tallest.
fn saguaro_girth(height: f64) -> f64 {
    (0.1 + 0.024 * height).min(0.34)
}

/// How many ribs run round a saguaro's stem `girth` in radius, `stray` more
/// or fewer than most such stems: a dozen on a slender stem, two dozen on the
/// stoutest.
fn ribs_round(girth: f64, stray: f64) -> u8 {
    let count = mathf::round_i32(girth * 72.0 + stray).clamp(11, 26);
    u8::try_from(count).unwrap_or(18)
}

/// A saguaro's skin folded into `ribs`.
fn flesh(ribs: Ribs) -> Bark {
    let (light, dark, accent) = FLESH;
    Bark {
        kind: BarkKind::Ribbed { ribs: ribs.count },
        light: rgb(light),
        dark: rgb(dark),
        accent: rgb(accent),
        rise: 0.0,
        snow: 0.0,
        moss: 0.0,
        bare: 0.0,
        seed: ribs.seed,
    }
}

/// The material of a saguaro's stem `girth` in radius folded into `ribs`,
/// snow lying on it as `snow` has it: its ribs cut as deep as its girth asks.
fn flesh_material(stage: &mut Stage, ribs: Ribs, (girth, snow): (f64, f64)) -> Option<u16> {
    let skin = flesh(ribs);
    let made = stage.material(
        Material::new(
            Pigment::Bark(Bark {
                snow,
                ..skin.clone()
            }),
            Finish::Coated { roughness: 0.78 },
        )
        .with_relief(Relief::Bark {
            bark: skin,
            depth: RIB_DEPTH * girth / (1.0 - FELT),
        }),
    )?;
    u16::try_from(made).ok()
}

/// Saguaros for `season`, each grown in materials of its own: its trunk's
/// and its arms' ribs as many as their girths ask, and its spines coloured by
/// their age.
fn grow_saguaro(stage: &mut Stage, dice: &mut Dice, season: Season) -> Option<Grown> {
    let snow = snowed(season);
    let spines = stage.material(Material::new(
        Pigment::Spines(palette(Kind::Saguaro, season).map(rgb)),
        Finish::Leaf {
            translucency: translucency(Kind::Saguaro),
        },
    ))?;
    let spines = u16::try_from(spines).ok()?;
    let mut prototypes = [0u32; VARIANTS];
    let mut heights = [0.0f64; VARIANTS];
    let mut skin = None;
    for (prototype, height) in prototypes.iter_mut().zip(heights.iter_mut()) {
        *height = dice.range(3.0, 8.0);
        let girth = saguaro_girth(*height) * dice.range(0.88, 1.12);
        let arms = dice.range(0.62, 0.82);
        let trunk = Ribs {
            count: ribs_round(girth, dice.range(-1.5, 1.5)),
            seed: dice.seed(),
        };
        let limbs = Ribs {
            count: ribs_round(girth * arms, dice.range(-1.5, 1.5)).min(trunk.count),
            seed: dice.seed(),
        };
        let flesh = Flesh {
            trunk: (flesh_material(stage, trunk, (girth, snow))?, trunk),
            arms: (flesh_material(stage, limbs, (girth * arms, snow))?, limbs),
            spines,
        };
        skin.get_or_insert(flesh.trunk.0);
        *prototype = stage.plan(&Recipe::Saguaro {
            height: *height,
            girth: (girth, arms),
            flesh,
            spines: stage.densities.spines,
            seed: dice.wide(),
        })?;
    }
    Some(Grown {
        kind: Kind::Saguaro,
        habit: Habit {
            affinity: affinity(Kind::Saguaro),
            prototypes,
            heights,
            bark: usize::from(skin?),
            crown: Kind::Saguaro.crown(Stand::Open),
            scaled: SAGUARO_SCALED,
        },
    })
}

/// How `kind` grows standing as `stand` has it: close among others a tree
/// sheds the limbs its neighbours shade and reaches up narrow for the light;
/// young, it is short and has branched only so far. A shrub grows as it does
/// however it stands.
fn stood(kind: Kind, stand: Stand) -> Species {
    let mut species = species(kind);
    match stand {
        _ if kind.shrub() => {}
        Stand::Open => {}
        Stand::Close => {
            species.base += 0.3 * (0.85 - species.base);
            species.attraction.0 += 0.1;
            // Shaded from below, a close-grown tree sheds its lower limbs and
            // keeps their stubs.
            species.stubs += 0.8;
            if let Some(limbs) = species.levels.get_mut(1) {
                limbs.length.0 *= 0.8;
            }
        }
        Stand::Young => {
            species.height = (1.4, 4.5);
            species.base = 0.12;
            species.depth = species.depth.saturating_sub(1).max(2);
            if let Some(limbs) = species.levels.get_mut(1) {
                limbs.branches *= 0.6;
            }
        }
        Stand::Dead => {
            species.height = (0.45 * species.height.0, 0.8 * species.height.1);
            species.base += 0.3 * (0.85 - species.base);
            species.stubs *= 1.5;
            species.depth = 2;
            species.evergreen = false;
            species.snapped = true;
            if let Some(trunk) = species.levels.get_mut(0) {
                trunk.taper *= 0.5;
            }
            // Its limbs broken back to a third of their length, as thick
            // where they broke as a third of the way out along a whole one.
            if let Some(limbs) = species.levels.get_mut(1) {
                limbs.length.0 *= 0.35;
                limbs.taper *= 0.35;
                limbs.branches *= 0.5;
            }
        }
    }
    species
}

#[allow(
    clippy::too_many_arguments,
    reason = "a level of branching is Weber and Penn's eight numbers, written as their table reads"
)]
const fn level(
    branches: f64,
    length: (f64, f64),
    taper: f64,
    down: (f64, f64),
    rotate: (f64, f64),
    curve: (f64, f64, f64),
    segments: u32,
    fork: (f64, f64),
) -> Level {
    Level {
        branches,
        length,
        taper,
        form: 1.0,
        down,
        rotate,
        curve,
        segments,
        fork,
    }
}

/// How far a trunk narrows up to its tip.
const TRUNK_TAPER: f64 = 0.97;

/// A trunk: one stem, bending a little, holding its girth up its bole by
/// the form `profile` before it narrows into the crown.
const fn trunk(profile: f64, stray: f64, segments: u32, fork: (f64, f64)) -> Level {
    Level {
        form: profile,
        ..level(
            0.0,
            (1.0, 0.0),
            TRUNK_TAPER,
            (0.0, 0.0),
            (0.0, 0.0),
            (0.0, 0.0, stray),
            segments,
            fork,
        )
    }
}

/// How `kind` grows.
#[allow(clippy::too_many_lines, reason = "a table of species, one arm each")]
fn species(kind: Kind) -> Species {
    match kind {
        Kind::Oak => Species {
            envelope: Envelope::Spherical,
            height: (13.0, 21.0),
            base: 0.22,
            girth: 0.03,
            flare: 0.8,
            stubs: 0.5,
            ratio_power: 1.4,
            trunks: 1,
            levels: [
                trunk(0.55, 22.0, 8, (0.25, 32.0)),
                level(
                    14.0,
                    (0.55, 0.12),
                    0.9,
                    (62.0, 18.0),
                    (140.0, 30.0),
                    (-30.0, 30.0, 70.0),
                    8,
                    (0.15, 26.0),
                ),
                level(
                    16.0,
                    (0.45, 0.1),
                    0.9,
                    (45.0, 15.0),
                    (140.0, 40.0),
                    (-20.0, 0.0, 55.0),
                    5,
                    (0.0, 0.0),
                ),
                level(
                    30.0,
                    (0.36, 0.1),
                    1.0,
                    (45.0, 20.0),
                    (140.0, 40.0),
                    (0.0, 0.0, 45.0),
                    2,
                    (0.0, 0.0),
                ),
            ],
            depth: 4,
            attraction: (0.0, -0.1),
            leafing: Leafing {
                outline: Outline::Lobed { lobes: 4 },
                per_twig: 22,
                length: 0.11,
                breadth: 0.5,
                fold: 0.25,
                angle: 50.0,
                toward_light: 0.6,
            },
            evergreen: false,
            snapped: false,
        },
        Kind::Maple | Kind::Cherry => Species {
            envelope: Envelope::Spherical,
            height: if kind == Kind::Maple {
                (12.0, 19.0)
            } else {
                (6.0, 10.0)
            },
            base: 0.2,
            girth: 0.025,
            flare: 0.55,
            stubs: 0.4,
            ratio_power: 1.35,
            trunks: 1,
            levels: [
                trunk(0.6, 14.0, 7, (0.12, 24.0)),
                level(
                    12.0,
                    (0.52, 0.1),
                    0.9,
                    (50.0, 15.0),
                    (140.0, 20.0),
                    (-10.0, 10.0, 32.0),
                    7,
                    (0.1, 20.0),
                ),
                level(
                    16.0,
                    (0.5, 0.1),
                    0.9,
                    (40.0, 15.0),
                    (140.0, 30.0),
                    (0.0, 0.0, 30.0),
                    5,
                    (0.0, 0.0),
                ),
                level(
                    30.0,
                    (0.4, 0.1),
                    1.0,
                    (45.0, 15.0),
                    (140.0, 30.0),
                    (0.0, 0.0, 30.0),
                    2,
                    (0.0, 0.0),
                ),
            ],
            depth: 4,
            attraction: (0.15, 0.0),
            leafing: Leafing {
                outline: if kind == Kind::Maple {
                    Outline::Palmate { lobes: 5 }
                } else {
                    Outline::Ovate { teeth: 8 }
                },
                per_twig: 20,
                length: if kind == Kind::Maple { 0.13 } else { 0.08 },
                breadth: if kind == Kind::Maple { 0.95 } else { 0.55 },
                fold: 0.2,
                angle: 50.0,
                toward_light: 0.7,
            },
            evergreen: false,
            snapped: false,
        },
        // Pruned to a short bole and an open, spreading crown its limbs
        // reach out level from.
        Kind::Apple => Species {
            envelope: Envelope::Spherical,
            height: (3.5, 5.5),
            base: 0.28,
            girth: 0.034,
            flare: 0.5,
            stubs: 0.6,
            ratio_power: 1.3,
            trunks: 1,
            levels: [
                trunk(0.55, 18.0, 6, (0.16, 28.0)),
                level(
                    10.0,
                    (0.62, 0.12),
                    0.85,
                    (62.0, 14.0),
                    (140.0, 25.0),
                    (-18.0, 12.0, 36.0),
                    7,
                    (0.12, 22.0),
                ),
                level(
                    16.0,
                    (0.48, 0.1),
                    0.9,
                    (45.0, 15.0),
                    (140.0, 30.0),
                    (-6.0, 6.0, 34.0),
                    5,
                    (0.0, 0.0),
                ),
                level(
                    30.0,
                    (0.38, 0.1),
                    1.0,
                    (45.0, 15.0),
                    (140.0, 30.0),
                    (0.0, 0.0, 30.0),
                    2,
                    (0.0, 0.0),
                ),
            ],
            depth: 4,
            attraction: (0.04, 0.0),
            leafing: Leafing {
                outline: Outline::Ovate { teeth: 10 },
                per_twig: 18,
                length: 0.075,
                breadth: 0.55,
                fold: 0.25,
                angle: 50.0,
                toward_light: 0.65,
            },
            evergreen: false,
            snapped: false,
        },
        Kind::Birch => Species {
            envelope: Envelope::TendFlame,
            height: (13.0, 20.0),
            base: 0.3,
            girth: 0.012,
            flare: 0.35,
            stubs: 0.8,
            ratio_power: 1.5,
            trunks: 1,
            levels: [
                trunk(0.6, 45.0, 10, (0.35, 18.0)),
                level(
                    26.0,
                    (0.33, 0.08),
                    0.95,
                    (45.0, 15.0),
                    (140.0, 30.0),
                    (20.0, -20.0, 30.0),
                    6,
                    (0.0, 0.0),
                ),
                level(
                    10.0,
                    (0.5, 0.1),
                    0.95,
                    (40.0, 15.0),
                    (140.0, 30.0),
                    (-10.0, 0.0, 30.0),
                    4,
                    (0.0, 0.0),
                ),
                level(
                    24.0,
                    (0.55, 0.15),
                    1.0,
                    (30.0, 15.0),
                    (140.0, 30.0),
                    (10.0, 0.0, 20.0),
                    2,
                    (0.0, 0.0),
                ),
            ],
            depth: 4,
            attraction: (0.25, -0.9),
            leafing: Leafing {
                outline: Outline::Ovate { teeth: 9 },
                per_twig: 22,
                length: 0.06,
                breadth: 0.62,
                fold: 0.2,
                angle: 45.0,
                toward_light: 0.5,
            },
            evergreen: false,
            snapped: false,
        },
        Kind::Beech => Species {
            envelope: Envelope::Spherical,
            height: (16.0, 24.0),
            base: 0.25,
            girth: 0.025,
            flare: 0.7,
            stubs: 0.3,
            ratio_power: 1.4,
            trunks: 1,
            levels: [
                trunk(0.5, 10.0, 8, (0.15, 24.0)),
                level(
                    16.0,
                    (0.5, 0.1),
                    0.9,
                    (55.0, 15.0),
                    (140.0, 30.0),
                    (-15.0, 20.0, 30.0),
                    7,
                    (0.15, 25.0),
                ),
                level(
                    16.0,
                    (0.45, 0.1),
                    0.9,
                    (50.0, 15.0),
                    (140.0, 30.0),
                    (0.0, 0.0, 25.0),
                    5,
                    (0.0, 0.0),
                ),
                level(
                    30.0,
                    (0.35, 0.1),
                    1.0,
                    (60.0, 15.0),
                    (140.0, 30.0),
                    (0.0, 0.0, 25.0),
                    2,
                    (0.0, 0.0),
                ),
            ],
            depth: 4,
            attraction: (0.05, 0.0),
            leafing: Leafing {
                outline: Outline::Ovate { teeth: 0 },
                per_twig: 20,
                length: 0.09,
                breadth: 0.58,
                fold: 0.15,
                angle: 60.0,
                toward_light: 0.85,
            },
            evergreen: false,
            snapped: false,
        },
        Kind::Willow => Species {
            envelope: Envelope::Hemispherical,
            height: (10.0, 15.0),
            base: 0.2,
            girth: 0.04,
            flare: 0.7,
            stubs: 0.3,
            ratio_power: 1.35,
            trunks: 1,
            levels: [
                trunk(0.5, 20.0, 6, (0.35, 40.0)),
                level(
                    10.0,
                    (0.5, 0.1),
                    0.9,
                    (40.0, 15.0),
                    (140.0, 30.0),
                    (20.0, -30.0, 30.0),
                    7,
                    (0.0, 0.0),
                ),
                level(
                    10.0,
                    (0.5, 0.1),
                    0.9,
                    (30.0, 10.0),
                    (140.0, 30.0),
                    (30.0, 0.0, 20.0),
                    5,
                    (0.0, 0.0),
                ),
                level(
                    18.0,
                    (1.5, 0.3),
                    1.0,
                    (20.0, 10.0),
                    (140.0, 30.0),
                    (0.0, 0.0, 10.0),
                    6,
                    (0.0, 0.0),
                ),
            ],
            depth: 4,
            attraction: (0.0, -2.6),
            leafing: Leafing {
                outline: Outline::Lanceolate,
                per_twig: 48,
                length: 0.1,
                breadth: 0.14,
                fold: 0.3,
                angle: 30.0,
                toward_light: 0.2,
            },
            evergreen: false,
            snapped: false,
        },
        Kind::Poplar => Species {
            envelope: Envelope::TaperedCylindrical,
            height: (18.0, 27.0),
            base: 0.05,
            girth: 0.018,
            flare: 0.5,
            stubs: 0.6,
            ratio_power: 1.4,
            trunks: 1,
            levels: [
                trunk(0.62, 6.0, 10, (0.0, 0.0)),
                level(
                    70.0,
                    (0.2, 0.05),
                    0.95,
                    (25.0, 10.0),
                    (140.0, 30.0),
                    (-10.0, 0.0, 20.0),
                    5,
                    (0.0, 0.0),
                ),
                level(
                    8.0,
                    (0.5, 0.1),
                    0.95,
                    (30.0, 10.0),
                    (140.0, 30.0),
                    (0.0, 0.0, 20.0),
                    3,
                    (0.0, 0.0),
                ),
                level(
                    22.0,
                    (0.4, 0.1),
                    1.0,
                    (35.0, 10.0),
                    (140.0, 30.0),
                    (0.0, 0.0, 20.0),
                    2,
                    (0.0, 0.0),
                ),
            ],
            depth: 4,
            attraction: (0.9, 0.3),
            leafing: Leafing {
                outline: Outline::Ovate { teeth: 5 },
                per_twig: 18,
                length: 0.08,
                breadth: 0.8,
                fold: 0.15,
                angle: 45.0,
                toward_light: 0.5,
            },
            evergreen: false,
            snapped: false,
        },
        Kind::Pine => Species {
            envelope: Envelope::TendFlame,
            height: (15.0, 24.0),
            base: 0.55,
            girth: 0.02,
            flare: 0.5,
            stubs: 1.6,
            ratio_power: 1.3,
            trunks: 1,
            levels: [
                trunk(0.62, 25.0, 10, (0.08, 20.0)),
                level(
                    22.0,
                    (0.35, 0.1),
                    0.95,
                    (80.0, 15.0),
                    (140.0, 40.0),
                    (-40.0, 30.0, 40.0),
                    6,
                    (0.0, 0.0),
                ),
                level(
                    8.0,
                    (0.5, 0.1),
                    0.95,
                    (50.0, 15.0),
                    (140.0, 40.0),
                    (0.0, 0.0, 40.0),
                    4,
                    (0.0, 0.0),
                ),
                level(
                    24.0,
                    (0.35, 0.1),
                    1.0,
                    (40.0, 15.0),
                    (140.0, 40.0),
                    (0.0, 0.0, 30.0),
                    2,
                    (0.0, 0.0),
                ),
            ],
            depth: 4,
            attraction: (0.35, 0.2),
            leafing: Leafing {
                outline: Outline::Fascicle { count: 11 },
                per_twig: 16,
                length: 0.22,
                breadth: 0.9,
                fold: 0.0,
                angle: 60.0,
                toward_light: 0.5,
            },
            evergreen: true,
            snapped: false,
        },
        Kind::Spruce => Species {
            envelope: Envelope::Conical,
            height: (16.0, 28.0),
            base: 0.05,
            girth: 0.018,
            flare: 0.6,
            stubs: 2.5,
            ratio_power: 1.3,
            trunks: 1,
            levels: [
                trunk(0.8, 5.0, 14, (0.0, 0.0)),
                level(
                    70.0,
                    (0.3, 0.05),
                    0.95,
                    (98.0, 8.0),
                    (137.5, 10.0),
                    (-20.0, 40.0, 12.0),
                    6,
                    (0.0, 0.0),
                ),
                level(
                    34.0,
                    (0.42, 0.1),
                    1.0,
                    (62.0, 15.0),
                    (137.5, 30.0),
                    (0.0, 0.0, 15.0),
                    3,
                    (0.0, 0.0),
                ),
                level(
                    0.0,
                    (0.0, 0.0),
                    1.0,
                    (0.0, 0.0),
                    (0.0, 0.0),
                    (0.0, 0.0, 0.0),
                    1,
                    (0.0, 0.0),
                ),
            ],
            depth: 3,
            attraction: (0.0, -0.35),
            leafing: Leafing {
                outline: Outline::Shoot { count: 16 },
                per_twig: 26,
                length: 0.26,
                breadth: 0.4,
                fold: 0.1,
                angle: 30.0,
                toward_light: 0.3,
            },
            evergreen: true,
            snapped: false,
        },
        Kind::Olive => Species {
            envelope: Envelope::Hemispherical,
            height: (5.0, 9.0),
            base: 0.25,
            girth: 0.05,
            flare: 0.8,
            stubs: 0.2,
            ratio_power: 1.35,
            trunks: 1,
            levels: [
                trunk(0.45, 70.0, 8, (0.4, 40.0)),
                level(
                    9.0,
                    (0.55, 0.1),
                    0.9,
                    (50.0, 20.0),
                    (140.0, 40.0),
                    (-20.0, 20.0, 80.0),
                    6,
                    (0.2, 30.0),
                ),
                level(
                    9.0,
                    (0.5, 0.1),
                    0.9,
                    (45.0, 20.0),
                    (140.0, 40.0),
                    (0.0, 0.0, 60.0),
                    4,
                    (0.0, 0.0),
                ),
                level(
                    26.0,
                    (0.35, 0.1),
                    1.0,
                    (40.0, 20.0),
                    (140.0, 40.0),
                    (0.0, 0.0, 40.0),
                    2,
                    (0.0, 0.0),
                ),
            ],
            depth: 4,
            attraction: (0.1, -0.1),
            leafing: Leafing {
                outline: Outline::Lanceolate,
                per_twig: 22,
                length: 0.07,
                breadth: 0.16,
                fold: 0.2,
                angle: 40.0,
                toward_light: 0.5,
            },
            evergreen: true,
            snapped: false,
        },
        Kind::Hazel
        | Kind::Hawthorn
        | Kind::Box
        | Kind::Heather
        | Kind::Gorse
        | Kind::Fern
        | Kind::Palm
        | Kind::Saguaro => shrub(kind),
    }
}

/// How a shrub grows: several stems rising from the woody stool they share,
/// each swelling a little where it leaves it, and no trunk to speak of.
fn shrub(kind: Kind) -> Species {
    let (height, trunks, leaf, per_twig, outline) = match kind {
        Kind::Hazel => ((3.0, 5.0), 7, 0.1, 16, Outline::Ovate { teeth: 12 }),
        Kind::Hawthorn => ((2.0, 4.0), 6, 0.035, 34, Outline::Ovate { teeth: 7 }),
        Kind::Box => ((0.8, 1.6), 6, 0.025, 30, Outline::Ovate { teeth: 0 }),
        Kind::Heather => ((0.3, 0.6), 8, 0.01, 36, Outline::Lanceolate),
        Kind::Fern => ((0.4, 1.1), 1, 0.12, 12, Outline::Shoot { count: 12 }),
        _ => ((0.8, 1.8), 7, 0.016, 34, Outline::Lanceolate),
    };
    Species {
        envelope: Envelope::Spherical,
        height,
        base: 0.1,
        girth: 0.012,
        flare: 0.8,
        stubs: 0.0,
        ratio_power: 1.3,
        trunks,
        levels: [
            trunk(1.0, 25.0, 5, (0.2, 30.0)),
            level(
                8.0,
                (0.5, 0.1),
                0.9,
                (40.0, 15.0),
                (140.0, 30.0),
                (-10.0, 10.0, 40.0),
                4,
                (0.0, 0.0),
            ),
            level(
                8.0,
                (0.45, 0.1),
                1.0,
                (45.0, 15.0),
                (140.0, 30.0),
                (0.0, 0.0, 40.0),
                2,
                (0.0, 0.0),
            ),
            level(
                0.0,
                (0.0, 0.0),
                1.0,
                (0.0, 0.0),
                (0.0, 0.0),
                (0.0, 0.0, 0.0),
                1,
                (0.0, 0.0),
            ),
        ],
        depth: 3,
        attraction: (0.3, 0.0),
        leafing: Leafing {
            outline,
            per_twig,
            length: leaf,
            breadth: if matches!(outline, Outline::Lanceolate) {
                0.2
            } else {
                0.7
            },
            fold: 0.2,
            angle: 50.0,
            toward_light: 0.6,
        },
        evergreen: !matches!(kind, Kind::Hazel | Kind::Hawthorn),
        snapped: false,
    }
}

/// `kind`'s bark: a saguaro's skin as a stem of middling girth wears it.
pub(super) fn bark(kind: Kind, seed: u32) -> Bark {
    let (bark_kind, light, dark, accent, rise) = match kind {
        Kind::Saguaro => {
            return flesh(Ribs {
                count: ribs_round(saguaro_girth(5.5), 0.0),
                seed,
            });
        }
        Kind::Oak => (BarkKind::Furrowed, 0x7A_6A_58, 0x2A_22_1C, 0x8C_94_70, 0.0),
        Kind::Maple => (BarkKind::Furrowed, 0x6E_62_56, 0x32_2A_24, 0x88_8E_74, 0.0),
        // Smooth and coppery, banded with pale lenticels, peeling thinly.
        Kind::Hazel => (BarkKind::Banded, 0x7E_60_4C, 0x3E_2E_24, 0xA8_9A_86, 0.0),
        // Grey-brown, fissured into small plates as it ages.
        Kind::Hawthorn => (BarkKind::Furrowed, 0x76_6C_62, 0x34_2E_2A, 0x8E_92_78, 0.0),
        // Glossy mahogany, darker than it looks in the sun, peeling coppery.
        Kind::Cherry => (BarkKind::Banded, 0x4E_2C_26, 0x1C_11_0F, 0x74_5E_52, 0.0),
        // Grey-brown, flaking in small scales on an old tree's trunk.
        Kind::Apple => (BarkKind::Scaly, 0x7A_6C_5C, 0x36_2C_24, 0x8A_92_72, 0.0),
        Kind::Birch => (BarkKind::Papery, 0xC8_C4_BC, 0x22_20_1E, 0xBC_AC_9C, 0.0),
        Kind::Beech => (BarkKind::Smooth, 0x9A_98_92, 0x6A_68_62, 0x84_90_76, 0.0),
        Kind::Willow | Kind::Olive => (BarkKind::Furrowed, 0x7A_70_62, 0x3A_32_2A, 0x92_96_7A, 0.0),
        Kind::Poplar => (BarkKind::Furrowed, 0x8A_86_78, 0x3E_3A_33, 0x94_98_80, 0.0),
        Kind::Pine => (BarkKind::Plated, 0x6C_56_48, 0x1A_14_11, 0xC4_6A_3A, 8.0),
        Kind::Spruce | Kind::Box | Kind::Heather | Kind::Gorse => {
            (BarkKind::Scaly, 0x6A_58_4A, 0x30_26_20, 0x7A_7A_68, 0.0)
        }
        Kind::Palm => (BarkKind::Ringed, 0x8C_7C_66, 0x4A_3E_33, 0x9A_8A_70, 0.0),
        Kind::Fern => (BarkKind::Smooth, 0x5E_6A_33, 0x3A_42_20, 0x6E_7A_3A, 0.0),
    };
    Bark {
        kind: bark_kind,
        light: rgb(light),
        dark: rgb(dark),
        accent: rgb(accent),
        rise,
        snow: 0.0,
        moss: 0.0,
        bare: 0.0,
        seed,
    }
}

/// `kind`'s bark on its dead: greyed a little by the weather and gone dark in
/// its cracks.
fn dead_bark(kind: Kind, seed: u32) -> Bark {
    let live = bark(kind, seed);
    let weathered = rgb(0x7E_7A_70);
    Bark {
        light: live.light.lerp(weathered, 0.25),
        dark: live.dark * 0.8,
        accent: live.accent.lerp(rgb(0x8A_94_6A), 0.5),
        ..live
    }
}

/// How much of `kind`'s bark has sloughed from its dead by the time they
/// have all but rotted: a thick, corky bark stays on in its plates for years,
/// a birch's outlasts the wood it wraps, a thin one falls away in sheets.
fn sloughs(kind: Kind) -> f64 {
    match kind {
        Kind::Birch => 0.1,
        Kind::Oak | Kind::Olive => 0.25,
        Kind::Palm | Kind::Saguaro | Kind::Fern | Kind::Box | Kind::Heather | Kind::Gorse => 0.3,
        Kind::Cherry => 0.4,
        Kind::Maple | Kind::Hazel | Kind::Hawthorn | Kind::Apple => 0.45,
        Kind::Pine | Kind::Willow | Kind::Poplar => 0.5,
        Kind::Spruce => 0.65,
        Kind::Beech => 0.75,
    }
}

/// How deep `kind`'s bark is cut, in metres.
fn bark_depth(kind: Kind) -> f64 {
    match kind {
        Kind::Oak | Kind::Pine => 0.024,
        Kind::Poplar | Kind::Willow | Kind::Olive => 0.012,
        Kind::Saguaro => RIB_DEPTH * saguaro_girth(5.5) / (1.0 - FELT),
        Kind::Maple | Kind::Hazel | Kind::Hawthorn | Kind::Apple => 0.008,
        Kind::Palm => 0.006,
        Kind::Birch => 0.005,
        Kind::Spruce | Kind::Box | Kind::Heather | Kind::Gorse => 0.004,
        Kind::Beech | Kind::Cherry => 0.002,
        Kind::Fern => 0.001,
    }
}

/// How much of what faces the sky the snow lying in `season` covers on
/// what stands out in it: none but in winter.
pub(super) fn snowed(season: Season) -> f64 {
    if season == Season::Winter {
        0.85
    } else {
        0.0
    }
}

/// How much of the light on a leaf's back `kind`'s leaves let through.
pub(super) fn translucency(kind: Kind) -> f64 {
    match kind {
        Kind::Pine | Kind::Spruce | Kind::Box | Kind::Heather | Kind::Gorse | Kind::Olive => 0.2,
        Kind::Palm | Kind::Fern => 0.3,
        // A saguaro's spines, which a low sun behind one lights in a halo.
        Kind::Saguaro => 0.4,
        _ => 0.45,
    }
}

/// `kind`'s leaves in `season`.
fn foliage(kind: Kind, season: Season) -> Foliage {
    let autumn = matches!(season, Season::Autumn { .. });
    let colours = palette(kind, season);
    Foliage {
        colours: colours.map(rgb),
        underside: if matches!(kind, Kind::Olive | Kind::Willow | Kind::Poplar) {
            0.35
        } else {
            0.15
        },
        veins: if matches!(kind, Kind::Pine | Kind::Spruce | Kind::Heather) {
            0.0
        } else {
            0.4
        },
        edge: rgb(0x5A_3A_1C),
        browning: if autumn { 0.7 } else { 0.08 },
        spots: if autumn { 0.6 } else { 0.05 },
        snow: if season == Season::Winter { 0.9 } else { 0.0 },
        outline: species(kind).leafing.outline,
    }
}

/// The four colours `kind`'s leaves are in `season`.
pub(super) fn palette(kind: Kind, season: Season) -> [u32; 4] {
    let autumn = matches!(season, Season::Autumn { .. });
    let spring = season == Season::Spring;
    match kind {
        Kind::Oak if autumn => [0x8A_5A_20, 0xA0_70_2A, 0x6A_4A_1C, 0xB0_8A_3A],
        Kind::Oak if spring => [0x7A_A0_40, 0x8F_B8_4A, 0x6A_90_34, 0x9C_C0_56],
        Kind::Oak => [0x3A_5A_22, 0x4A_6A_2A, 0x2E_4A_1C, 0x55_75_2F],
        Kind::Maple if autumn => [0xC0_30_1A, 0xE0_6A_1A, 0xE8_A0_20, 0xA0_20_18],
        Kind::Maple => [0x4A_7A_2A, 0x5A_8A_33, 0x3E_6A_24, 0x68_94_3A],
        Kind::Cherry if spring => [0xF4_C0_D4, 0xFF_E4_EE, 0xE8_A0_BC, 0xFF_F6_F8],
        Kind::Cherry if autumn => [0xC8_50_28, 0xD8_7A_30, 0xA8_3A_20, 0xE0_A0_40],
        // In blossom among its first leaves; turning late and yellowing.
        Kind::Apple if spring => [0xF6_E2_E6, 0xEC_C2_CC, 0x6E_96_3E, 0xFF_F4_F4],
        Kind::Apple if autumn => [0x7A_8E_36, 0x9C_9A_3A, 0x6A_80_30, 0xB4_A0_40],
        Kind::Apple => [0x3E_68_28, 0x4A_76_2E, 0x36_5E_22, 0x56_80_34],
        Kind::Cherry => [0x3E_6A_26, 0x4C_78_2E, 0x34_5C_20, 0x5A_84_34],
        Kind::Birch | Kind::Poplar if autumn => [0xE0_B8_30, 0xF0_CC_48, 0xC8_9C_24, 0xD8_A8_38],
        Kind::Birch => [0x6A_94_34, 0x7C_A4_3C, 0x58_84_2C, 0x94_B4_4C],
        Kind::Beech if autumn => [0xB0_60_1C, 0xC8_7A_28, 0x8E_4A_18, 0xD0_90_40],
        Kind::Beech => [0x4E_7A_26, 0x5E_8A_2E, 0x40_6A_20, 0x6C_96_36],
        Kind::Willow if autumn => [0xC8_C0_40, 0xD8_CC_50, 0xB0_A8_38, 0xE0_D8_60],
        Kind::Willow => [0x8A_AA_50, 0x9C_B8_60, 0x7A_9A_44, 0xA8_C0_6A],
        Kind::Poplar => [0x4A_7A_2C, 0x5A_88_34, 0x3E_6C_24, 0x66_92_3C],
        Kind::Pine => [0x2E_4A_24, 0x3A_5A_2C, 0x26_40_1E, 0x44_62_33],
        Kind::Spruce => [0x1E_36_1C, 0x26_42_22, 0x18_2E_18, 0x2E_4C_28],
        Kind::Olive => [0x7A_8A_5A, 0x8A_9A_6A, 0x6A_7A_4E, 0x98_A6_7A],
        Kind::Palm => [0x5A_8A_2E, 0x6A_9A_3A, 0x4E_7A_28, 0x8A_7A_4A],
        // A saguaro's spines by their age: red-brown and young, tan, grey,
        // and weathered pale.
        Kind::Saguaro => [0x6A_32_24, 0xB0_96_6E, 0x8E_8A_80, 0xC4_C0_B6],
        Kind::Hazel if autumn => [0xC8_B0_40, 0xB0_98_34, 0xD8_C0_50, 0x9A_84_2C],
        Kind::Hazel => [0x4E_7C_2A, 0x5C_8A_33, 0x44_6E_24, 0x68_94_3A],
        Kind::Hawthorn if spring => [0xF2_EE_E2, 0x5A_84_30, 0xFA_F6_EE, 0x4C_76_2A],
        Kind::Hawthorn if autumn => [0x6A_5A_2A, 0xA0_28_1E, 0x5A_4A_24, 0xB8_34_22],
        Kind::Hawthorn => [0x3C_60_26, 0x4A_70_2E, 0x32_52_1E, 0x56_7C_34],
        Kind::Box => [0x2E_50_1E, 0x38_5C_24, 0x28_46_1A, 0x42_66_2A],
        Kind::Heather => [0x7A_4A_8A, 0x8C_5A_9A, 0x5E_3A_6C, 0x3E_5A_2A],
        Kind::Gorse => [0x3A_5A_24, 0xE8_C8_20, 0x32_50_1E, 0xF0_D0_30],
        Kind::Fern if autumn => [0xA0_70_2A, 0xB8_84_33, 0x8A_5A_22, 0xC0_90_40],
        Kind::Fern if spring => [0x7A_AA_3A, 0x8C_BA_44, 0x6A_9A_30, 0x9C_C8_52],
        Kind::Fern => [0x4A_7A_28, 0x5A_8A_30, 0x3E_6A_22, 0x6A_9A_3A],
    }
}

/// The grass a land grows: up to four kinds of it and their colours; what a
/// farmed land's fields grow in it instead, and the seed its pastures' pats
/// are scattered under; how many shoots stand to a square metre where it
/// grows best; the share of them ending in a wildflower, and the
/// wildflowers' colours; the share of the ground weeds take, and their
/// colours; and the leaves fallen on it.
#[derive(Copy, Clone, Debug)]
pub(super) struct Grassland {
    pub(super) kinds: [Option<(GrassKind, Blades)>; WILD_KINDS],
    pub(super) sowing: Sowing,
    pub(super) grazing: Option<u32>,
    pub(super) shoots: f64,
    pub(super) flowers: f64,
    pub(super) blossoms: [u32; 4],
    pub(super) weeds: f64,
    pub(super) weed_colours: [u32; 2],
    pub(super) fallen: Option<Fallen>,
}

/// What sort of grassland a land grows.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Character {
    /// A lowland meadow: rye or timothy, fine fescue on its banks, hair-grass
    /// tussocks in its wet ground, bent where it is trodden.
    Meadow,
    /// Parkland about buildings: a close lawn, rougher grass at its edges.
    Park,
    /// Upland pasture: bent and fescue, hair-grass and rushes in its wet.
    Upland,
    /// Coastal turf: fescue, marram on the dry ground behind the shore.
    Coast,
}

/// A kind of grass, and its colours in summer as `0xRRGGBB`: its leaves'
/// two greens, the straw their tips dry to, and its seed heads'.
struct Sort {
    kind: GrassKind,
    leaves: [u32; 2],
    tip: u32,
    head: u32,
}

/// The kinds of grass a grassland is made of.
const RYE: Sort = Sort {
    kind: GrassKind {
        height: (0.18, 0.5),
        width: 0.005,
        lean: 0.4,
        droop: 0.3,
        thickness: 1.0,
        tufted: 0.45,
        stems: 0.1,
        head: Some(Head::Spike),
        nod: 0.0,
        habit: Tufting::Open,
        share: 1.0,
        rows: 0.0,
        stood: None,
    },
    leaves: [0x3E_6E_26, 0x4E_7E_2E],
    tip: 0xA8_B0_58,
    head: 0x9C_A0_5A,
};
const TIMOTHY: Sort = Sort {
    kind: GrassKind {
        height: (0.22, 0.6),
        width: 0.006,
        lean: 0.35,
        droop: 0.25,
        stems: 0.14,
        ..RYE.kind
    },
    leaves: [0x4A_74_2C, 0x5A_80_34],
    tip: 0xB8_B4_68,
    head: 0x8A_86_58,
};
const FESCUE: Sort = Sort {
    kind: GrassKind {
        height: (0.05, 0.16),
        width: 0.0028,
        lean: 0.35,
        droop: 0.2,
        thickness: 1.7,
        tufted: 0.2,
        stems: 0.04,
        head: Some(Head::Plume),
        nod: 0.0,
        habit: Tufting::Dry,
        share: 0.5,
        rows: 0.0,
        stood: None,
    },
    leaves: [0x4A_6A_3A, 0x56_76_42],
    tip: 0xB0_A8_70,
    head: 0xA8_8C_70,
};
const BENT: Sort = Sort {
    kind: GrassKind {
        height: (0.1, 0.3),
        width: 0.003,
        lean: 0.35,
        droop: 0.2,
        thickness: 1.3,
        tufted: 0.3,
        stems: 0.16,
        head: Some(Head::Plume),
        nod: 0.0,
        habit: Tufting::Trodden,
        share: 0.35,
        rows: 0.0,
        stood: None,
    },
    leaves: [0x56_78_34, 0x66_84_3C],
    tip: 0xC0_B0_70,
    head: 0x9A_70_60,
};
const HAIR_GRASS: Sort = Sort {
    kind: GrassKind {
        height: (0.35, 0.8),
        width: 0.0035,
        lean: 0.5,
        droop: 0.4,
        thickness: 1.3,
        tufted: 1.0,
        stems: 0.08,
        head: Some(Head::Plume),
        nod: 0.0,
        habit: Tufting::Wet,
        share: 0.6,
        rows: 0.0,
        stood: None,
    },
    leaves: [0x3A_58_26, 0x46_62_2C],
    tip: 0xB8_A8_68,
    head: 0xB0_A0_80,
};
const RUSH: Sort = Sort {
    kind: GrassKind {
        height: (0.45, 0.9),
        width: 0.003,
        lean: 0.12,
        droop: 0.05,
        thickness: 1.1,
        tufted: 0.9,
        stems: 0.05,
        head: Some(Head::Spike),
        nod: 0.0,
        habit: Tufting::Wet,
        share: 0.45,
        rows: 0.0,
        stood: None,
    },
    leaves: [0x30_50_20, 0x3A_5A_24],
    tip: 0x6A_6A_3A,
    head: 0x7A_5A_3A,
};
const MARRAM: Sort = Sort {
    kind: GrassKind {
        height: (0.4, 0.9),
        width: 0.004,
        lean: 0.35,
        droop: 0.2,
        thickness: 0.8,
        tufted: 0.7,
        stems: 0.1,
        head: Some(Head::Spike),
        nod: 0.0,
        habit: Tufting::Dry,
        share: 0.7,
        rows: 0.0,
        stood: None,
    },
    leaves: [0x7A_8A_60, 0x8A_96_6C],
    tip: 0xC8_C0_90,
    head: 0xC0_B0_88,
};
const MOWN: Sort = Sort {
    kind: GrassKind {
        height: (0.03, 0.07),
        width: 0.003,
        lean: 0.3,
        droop: 0.1,
        thickness: 2.0,
        tufted: 0.05,
        stems: 0.0,
        head: Some(Head::Spike),
        nod: 0.0,
        habit: Tufting::Open,
        share: 1.0,
        rows: 0.0,
        stood: None,
    },
    leaves: [0x3A_6E_24, 0x46_7A_2C],
    tip: 0x5E_84_34,
    head: 0x8A_8A_50,
};

/// What a season makes of grass: the colours its leaves, their tips and its
/// seed heads turn toward, and how far, in linear light.
struct Turning {
    leaves: (u32, f64),
    tip: (u32, f64),
    head: (u32, f64),
}

const fn turning(season: Season) -> Turning {
    match season {
        Season::Spring => Turning {
            leaves: (0x6A_A0_38, 0.35),
            tip: (0x7A_A0_40, 0.6),
            head: (0x8A_A0_50, 0.5),
        },
        Season::Summer => Turning {
            leaves: (0, 0.0),
            tip: (0, 0.0),
            head: (0, 0.0),
        },
        Season::Autumn { .. } => Turning {
            leaves: (0x7A_6A_38, 0.4),
            tip: (0xB0_8A_50, 0.5),
            head: (0x8A_6A_48, 0.6),
        },
        Season::Winter => Turning {
            leaves: (0x7A_70_50, 0.6),
            tip: (0xB8_A8_80, 0.6),
            head: (0x8A_7A_60, 0.6),
        },
    }
}

/// The straw a cured meadow's leaves dry to, and their tips.
const CURED: (u32, u32) = (0xB8_A4_65, 0xE0_D0_98);

impl Sort {
    /// This kind of grass as it grows in `season`, `cured` of the way to hay,
    /// its heights `rank` times its own, holding `share` of its sward.
    fn grown(&self, season: Season, cured: f64, rank: f64, share: f64) -> (GrassKind, Blades) {
        let turning = turning(season);
        let turn = |colour: u32, (toward, by): (u32, f64)| rgb(colour).lerp(rgb(toward), by);
        let leaves = self
            .leaves
            .map(|leaf| turn(leaf, turning.leaves).lerp(rgb(CURED.0), 0.55 * cured));
        let kind = GrassKind {
            height: (self.kind.height.0 * rank, self.kind.height.1 * rank),
            share,
            ..self.kind
        };
        let blades = Blades {
            leaves,
            tip: turn(self.tip, turning.tip).lerp(rgb(CURED.1), 0.6 * cured),
            head: turn(self.head, turning.head).lerp(rgb(CURED.1), 0.4 * cured),
        };
        (kind, blades)
    }
}

/// The grassland of `character` in `season`.
pub(super) fn grassland(dice: &mut Dice, character: Character, season: Season) -> Grassland {
    let rank = dice.range(0.75, 1.25);
    let cured = match (character, season) {
        (Character::Meadow, Season::Summer) => dice.range(0.0, 0.7),
        (Character::Upland | Character::Coast, Season::Summer) => dice.range(0.0, 0.35),
        _ => 0.0,
    };
    let mixed: [Option<(&Sort, f64)>; WILD_KINDS] = match character {
        Character::Meadow => [
            Some((if dice.chance(0.5) { &RYE } else { &TIMOTHY }, 1.0)),
            Some((&FESCUE, 0.5)),
            Some((&HAIR_GRASS, 0.6)),
            Some((&BENT, 0.35)),
        ],
        Character::Park => [
            Some((&MOWN, 1.0)),
            Some((&RYE, 0.3)),
            Some((&FESCUE, 0.3)),
            Some((&BENT, 0.25)),
        ],
        Character::Upland => [
            Some((&BENT, 1.0)),
            Some((&FESCUE, 0.8)),
            Some((&HAIR_GRASS, 0.7)),
            Some((&RUSH, 0.5)),
        ],
        Character::Coast => [
            Some((&FESCUE, 1.0)),
            Some((&MARRAM, 0.7)),
            Some((&BENT, 0.4)),
            None,
        ],
    };
    let (shoots, flowers, weeds) = match character {
        Character::Meadow => (1100.0, dice.range(0.0, 0.05), 0.28),
        Character::Park => (1600.0, 0.01, 0.15),
        Character::Upland => (1000.0, dice.range(0.0, 0.03), 0.25),
        Character::Coast => (900.0, dice.range(0.0, 0.02), 0.2),
    };
    let (blossoms, weed_colours) = match (character, season) {
        (Character::Upland, _) => (
            [0x9A_5A_AA, 0xB0_70_C0, 0xF4_E8_F0, 0xE8_C8_40],
            [0x4A_5A_2C, 0x5C_64_30],
        ),
        (_, Season::Spring) => (
            [0xF8_F4_F0, 0xF4_D0_30, 0xC8_3A_3A, 0x8A_6A_D8],
            [0x3E_6A_24, 0x4A_76_2A],
        ),
        (_, Season::Autumn { .. } | Season::Winter) => (
            [0xF0_E8_D0, 0xE8_C0_40, 0xD0_80_40, 0xF8_F0_E8],
            [0x5A_6A_30, 0x6A_74_38],
        ),
        (_, Season::Summer) => (
            [0xF8_F4_F0, 0xE8_60_30, 0xF0_C0_30, 0xC8_60_B0],
            [0x40_62_24, 0x52_70_2A],
        ),
    };
    Grassland {
        kinds: mixed.map(|sort| sort.map(|(sort, share)| sort.grown(season, cured, rank, share))),
        sowing: Sowing::NONE,
        grazing: None,
        shoots: shoots * dice.range(0.85, 1.15),
        flowers,
        blossoms,
        weeds,
        weed_colours,
        fallen: None,
    }
}

impl Grassland {
    /// What the grassland looks like from too far off to make out a leaf, as
    /// the ground's own grass and the straw of its parched patches: its kinds'
    /// greens, weighed by how much of it each holds, and a little of their
    /// dry tips, which a low view sees most of.
    pub(super) fn afar(&self) -> (Vec3, Vec3) {
        let (mut green, mut straw, mut total) = (Vec3::ZERO, Vec3::ZERO, 0.0);
        for (kind, blades) in self.kinds.iter().flatten() {
            let leaf = (blades.leaves[0] + blades.leaves[1]) * 0.5;
            green += (leaf * 0.8 + blades.tip * 0.2) * kind.share;
            straw += blades.tip * kind.share;
            total += kind.share;
        }
        let total = total.max(1e-9);
        (green * (1.0 / total), straw * (1.0 / total))
    }

    fn crowd(&self, fallen: [u32; 4]) -> Pigment {
        let none = Blades {
            leaves: [Vec3::ZERO; 2],
            tip: Vec3::ZERO,
            head: Vec3::ZERO,
        };
        let mut grasses = [none; GRASS_KINDS];
        let kinds = self.kinds.iter().chain(self.sowing.kinds.iter());
        for (slot, kind) in grasses.iter_mut().zip(kinds) {
            *slot = kind.map_or(none, |(_, blades)| blades);
        }
        Pigment::Crowd(Crowd {
            grasses,
            blossoms: self.blossoms.map(rgb),
            weeds: self.weed_colours.map(rgb),
            fallen: fallen.map(rgb),
        })
    }

    /// The sward it grows over the land: its wild grasses, and what its
    /// fields grow in their place.
    fn grass(&self) -> Grass {
        Grass {
            kinds: self.kinds.map(|kind| kind.map(|(kind, _)| kind)),
            sown: Sown {
                kinds: self.sowing.kinds.map(|kind| kind.map(|(kind, _)| kind)),
                by_growth: self.sowing.by_growth,
            },
            grazing: self.grazing,
            shoots: self.shoots,
            flowers: self.flowers,
        }
    }
}

/// Leaves fallen from a kind of tree: how thickly they lie where most have
/// fallen, as leaves to a cell of ground, and how long ago the most of them
/// fell, from `0.0` for this autumn's to `1.0` for last year's.
#[derive(Copy, Clone, Debug)]
pub(super) struct Fallen {
    pub(super) kind: Kind,
    pub(super) most: u32,
    pub(super) age: f64,
}

impl Fallen {
    /// What has fallen from `grown` by `season`: a fresh fall in autumn,
    /// last year's leaves rotting in spring and summer, and the last of them
    /// under snow in winter.
    pub(super) fn from(grown: &Grown, season: Season, thickness: f64) -> Option<Self> {
        if matches!(
            grown.kind,
            Kind::Palm | Kind::Saguaro | Kind::Box | Kind::Heather | Kind::Gorse | Kind::Fern
        ) {
            return None;
        }
        let (most, age) = match season {
            Season::Autumn { fallen } => (1.0 + f64::from(fallen) / 8.0, 0.0),
            Season::Winter => (2.0, 0.8),
            Season::Spring | Season::Summer => (2.5, 0.65),
        };
        let most = u32::try_from(mathf::round_i32(most * thickness)).ok()?;
        (most > 0).then_some(Self {
            kind: grown.kind,
            most,
            age,
        })
    }
}

/// How far apart the samples of a cover's shade lie, at least, and in cells
/// of the cover.
const SHADE_STEP: f64 = 0.75;
const SHADE_CELLS: f64 = 2.0;

/// The side of the cells weeds and fallen leaves are laid out in: broad
/// enough to hold a rosette, or a leaf lying flat.
const LEAF_CELL: f64 = 0.32;

/// One lawn of a sward: the scene's built height grid it lies on, the
/// rectangle it covers and the finer lawn's rectangle within that, and the
/// side of its cells.
#[derive(Copy, Clone, Debug)]
pub(super) struct Tier {
    pub(super) field: u32,
    pub(super) from: (f64, f64),
    pub(super) to: (f64, f64),
    pub(super) hole: Option<((f64, f64), (f64, f64))>,
    pub(super) cell: f64,
}

/// A sward of a grassland being laid a lawn at a time: its grass over each
/// of its tiers, finest first, drawn under one pattern so the tiers meet
/// unseen and seen as `seen` has it; then weeds and fallen leaves over the
/// finest grid's rectangle `ground`, seen as `near` has it. Each lawn's shade
/// is sampled a bounded unit at a time before the lawn is laid.
#[derive(Debug)]
pub(super) struct Laying {
    material: usize,
    pattern: u32,
    lawns: Vec<Planned>,
    next: usize,
    /// The shade over the next lawn, being sampled.
    sampling: Option<Sampling>,
}

/// One lawn of a sward, planned: the grid it lies on, the rectangle it
/// covers and the finer lawn's within it, what it is of, the side of its
/// cells, and how it is seen.
#[derive(Copy, Clone, Debug)]
struct Planned {
    field: u32,
    rect: Rect,
    hole: Option<Rect>,
    cover: Cover,
    cell: f64,
    seen: Seen,
}

impl Laying {
    /// `grassland` to be laid over `tiers` and `ground`; `None` when the
    /// stage will not hold its material.
    pub(super) fn new(
        stage: &mut Stage,
        dice: &mut Dice,
        grassland: &Grassland,
        (tiers, seen): (Vec<Tier>, Seen),
        (ground, near): ((u32, Rect), Seen),
    ) -> Option<Self> {
        let fallen_colours = grassland.fallen.map_or([0; 4], |fallen| {
            palette(fallen.kind, Season::Autumn { fallen: 0 })
        });
        let material = stage.material(Material::new(
            grassland.crowd(fallen_colours),
            Finish::Leaf { translucency: 0.3 },
        ))?;
        let grass = Cover::Grass(grassland.grass());
        let weeds = (grassland.weeds > 0.0).then_some(Cover::Weeds(Weeds {
            share: grassland.weeds,
            leaves: (5, 9),
        }));
        let litter = grassland.fallen.map(|fallen| {
            let leafing = species(fallen.kind).leafing;
            Cover::Litter(Litter {
                most: fallen.most,
                length: (0.6 * leafing.length, 1.1 * leafing.length),
                outline: leafing.outline,
                age: fallen.age,
            })
        });
        let swards = tiers.iter().map(|tier| Planned {
            field: tier.field,
            rect: (tier.from, tier.to),
            hole: tier.hole,
            cover: grass,
            cell: tier.cell,
            seen,
        });
        let (field, rect) = ground;
        let floor = [weeds, litter].into_iter().flatten().map(|cover| Planned {
            field,
            rect,
            hole: None,
            cover,
            cell: LEAF_CELL,
            seen: near,
        });
        Some(Self {
            material,
            pattern: dice.seed(),
            lawns: fallible::collected(tiers.len() + 2, swards.chain(floor))?,
            next: 0,
            sampling: None,
        })
    }

    /// The share of the lawns laid so far.
    pub(super) fn done(&self) -> f64 {
        let within = self.sampling.as_ref().map_or(0.0, Sampling::done);
        (real(self.next) + within) / real(self.lawns.len().max(1))
    }

    /// The next unit of the laying across `runner`, in the shade `shades`
    /// casts: a unit of the next lawn's shade sampled, or the lawn laid once
    /// it is; whether every lawn is laid, or `None` when the stage will not
    /// hold one.
    pub(super) fn step(
        &mut self,
        stage: &mut Stage,
        dice: &mut Dice,
        (shades, runner): (&Shades, &dyn JobRunner),
    ) -> Option<bool> {
        let Some(&lawn) = self.lawns.get(self.next) else {
            return Some(true);
        };
        let sampling = match self.sampling.as_mut() {
            Some(sampling) => sampling,
            None => self.sampling.insert(Sampling::new(
                lawn.rect,
                SHADE_STEP.max(SHADE_CELLS * lawn.cell),
            )?),
        };
        if !sampling.step(shades, runner) {
            return Some(false);
        }
        let shade = self.sampling.take()?.finish();
        self.next += 1;
        let covered = Lawn {
            hole: lawn.hole,
            sward: self.pattern,
            seed: dice.seed(),
            seen: lawn.seen,
            ..covering(stage, (!shade.is_open()).then_some(shade), &lawn)?
        };
        if matches!(lawn.cover, Cover::Grass(_)) {
            // Laid only where grass grows: a lawn over the sea or bare rock
            // would hold a canopy grid of nothing.
            if covered.grows(stage.fields.get(lawn.field as usize)?) {
                let tops = stage.tops(&covered)?;
                stage.lawn(
                    Lawn {
                        tops: Some(tops),
                        ..covered
                    },
                    self.material,
                )?;
            }
        } else {
            stage.lawn(covered, self.material)?;
        }
        Some(self.next >= self.lawns.len())
    }
}

/// A lawn as `lawn` plans it, bounded by the lowest and highest the ground
/// lies under it and in `shade`; its pattern, seed and sight still to set.
fn covering(stage: &Stage, shade: Option<Shade>, lawn: &Planned) -> Option<Lawn> {
    let ground = stage.fields.get(lawn.field as usize)?;
    let (from, to) = lawn.rect;
    Some(Lawn {
        field: lawn.field,
        from,
        to,
        hole: None,
        floor: ground.lowest_over(from, to) - 0.02,
        ceiling: ground.highest_over(from, to),
        cell: lawn.cell,
        cover: lawn.cover,
        shade,
        sward: 0,
        seed: 0,
        seen: Seen::from((0.0, 0.0), (f64::INFINITY, f64::INFINITY)),
        tops: None,
    })
}

#[cfg(test)]
#[path = "plants_tests.rs"]
mod tests;
