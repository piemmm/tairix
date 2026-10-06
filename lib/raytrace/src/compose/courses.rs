//! Masonry laid as masons lay it: stones in courses and bricks in their
//! bonds, mortar bedded between them, arches of voussoirs, columns of drums
//! and rings of stones about a round building, every unit a solid of its own
//! drawn from its own key and worn as long as the structure has stood.
//!
//! A [`Mason`] lays a structure into one assembly in the structure's own
//! frame, its `y` up; the stage builds its hierarchy as it grows the rest of
//! the scene's prototypes.

use core::f64::consts::{PI, TAU};

use tairix_util::mathf;

use super::{rgb, Dice, Stage};
use crate::cover::{Cover, Substrate};
use crate::masonry::{Masonry, Unit};
use crate::material::{Finish, Material, Relief};
use crate::pigment::Pigment;
use crate::prototype::{Assembly, Building, Part};
use crate::sample::{mix32, unit};
use crate::shape::Shape;
use crate::solid::{Form, Solid, Wear};
use crate::timber::Timber;
use crate::vector::{Frame, Pose, Vec3};

/// A stone a structure is built in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Quarry {
    Marble,
    Limestone,
    Sandstone,
    RedSandstone,
    Granite,
    /// Split thin for a roof: blue-grey, fine-grained, barely weathering.
    Slate,
}

impl Quarry {
    pub(super) const ALL: [Self; 5] = [
        Self::Marble,
        Self::Limestone,
        Self::Sandstone,
        Self::RedSandstone,
        Self::Granite,
    ];

    /// Its two shades, its flecks, how many grains span a metre and how far
    /// its blocks' shades wander, as sRGB.
    const fn palette(self) -> ([u32; 2], [u32; 2], f64, f64) {
        match self {
            Self::Marble => (
                [0xE0_DA_CE, 0xCC_C2_B0],
                [0xC0_BC_B4, 0xEA_E6_DE],
                500.0,
                0.06,
            ),
            Self::Limestone => (
                [0xC4_B4_92, 0xA8_96_72],
                [0x9A_8C_70, 0xD2_C6_AA],
                380.0,
                0.13,
            ),
            Self::Sandstone => (
                [0xB8_96_66, 0x9A_74_48],
                [0x8A_68_42, 0xC8_AE_84],
                900.0,
                0.15,
            ),
            Self::RedSandstone => (
                [0xA6_62_4A, 0x8A_4C_3A],
                [0x7A_42_30, 0xB8_7A_62],
                900.0,
                0.14,
            ),
            Self::Granite => (
                [0xA6_A2_9A, 0x88_84_7E],
                [0x2A_2A_2C, 0xE0_DC_D6],
                220.0,
                0.08,
            ),
            Self::Slate => (
                [0x4A_50_58, 0x3A_3E_46],
                [0x2E_32_38, 0x62_68_70],
                1400.0,
                0.08,
            ),
        }
    }

    /// How fast it weathers against limestone's: granite barely, marble
    /// sugaring and sandstone crumbling.
    const fn softness(self) -> f64 {
        match self {
            Self::Marble => 0.8,
            Self::Limestone => 0.7,
            Self::Sandstone | Self::RedSandstone => 0.85,
            Self::Granite | Self::Slate => 0.15,
        }
    }

    const fn substrate(self) -> Substrate {
        match self {
            Self::Marble | Self::Limestone => Substrate::Calcareous,
            Self::Sandstone | Self::RedSandstone | Self::Granite | Self::Slate => Substrate::Siliceous,
        }
    }

    /// How rough its dressed face is, as a clear coat's roughness: polished
    /// marble the smoothest.
    const fn roughness(self) -> f64 {
        match self {
            Self::Marble => 0.45,
            Self::Granite | Self::Slate => 0.6,
            Self::Limestone | Self::Sandstone | Self::RedSandstone => 0.85,
        }
    }
}

/// A clay bricks are fired from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Clay {
    /// A common red.
    Red,
    /// A London stock's yellow.
    Stock,
    /// A blue engineering brick's purple-grey.
    Blue,
}

impl Clay {
    pub(super) const ALL: [Self; 3] = [Self::Red, Self::Stock, Self::Blue];

    const fn palette(self) -> ([u32; 2], [u32; 2]) {
        match self {
            Self::Red => ([0xA4_52_3A, 0x86_42_30], [0x5E_2A_1C, 0xC2_80_62]),
            Self::Stock => ([0xC8_A2_72, 0xA8_86_5C], [0x6A_4E_34, 0xDA_C2_9A]),
            Self::Blue => ([0x56_4C_54, 0x40_3A_42], [0x2A_26_2A, 0x7A_70_74]),
        }
    }
}

/// Where a structure stands, as weathering it: how damp, how dry the
/// season, and the height its foot stands at.
#[derive(Copy, Clone, Debug)]
pub(super) struct Weathering {
    pub(super) damp: f64,
    pub(super) drought: f64,
    pub(super) foot: f64,
}

/// What a structure is built in, and how long it has stood.
#[derive(Copy, Clone, Debug)]
pub(super) struct Stonework {
    pub(super) stone: u16,
    pub(super) mortar: u16,
    pub(super) cover: Option<u16>,
    /// How weathered, from new at nought to ancient at one.
    pub(super) age: f64,
    /// How fast its stone weathers.
    pub(super) softness: f64,
}

/// Lime mortar's two shades and its sand's flecks, as sRGB.
const MORTAR: ([u32; 2], [u32; 2]) = ([0xC6_BE_AE, 0xB0_A8_98], [0x86_7E_6E, 0xE2_DC_D0]);

impl Stage {
    /// The materials a structure of `quarry` `age` old stands in where
    /// `exposure` has it, and the moss and lichen it carries.
    pub(super) fn stonework(
        &mut self,
        dice: &mut Dice,
        quarry: Quarry,
        (age, exposure): (f64, Weathering),
    ) -> Option<Stonework> {
        self.rockwork(dice, (quarry, Unit::Stone), (age, exposure))
    }

    /// The materials a wall of field stones of `quarry`'s rock, gathered off
    /// the land, `age` old stands in where `exposure` has it, and the moss
    /// and lichen it carries.
    pub(super) fn fieldwork(
        &mut self,
        dice: &mut Dice,
        quarry: Quarry,
        (age, exposure): (f64, Weathering),
    ) -> Option<Stonework> {
        self.rockwork(dice, (quarry, Unit::Field), (age, exposure))
    }

    /// The materials a structure of `quarry`'s rock laid as `unit` `age` old
    /// stands in where `exposure` has it.
    fn rockwork(
        &mut self,
        dice: &mut Dice,
        (quarry, unit): (Quarry, Unit),
        (age, exposure): (f64, Weathering),
    ) -> Option<Stonework> {
        let (bases, flecks, grain, shade) = quarry.palette();
        let stone = self.masonry(
            dice,
            (bases, flecks, grain, shade),
            (age, exposure, unit),
            quarry.roughness(),
        )?;
        self.work(
            dice,
            stone,
            (age, exposure),
            (quarry.substrate(), quarry.softness()),
        )
    }

    /// The materials a structure of bricks of `clay` `age` old stands in
    /// where `exposure` has it.
    pub(super) fn brickwork(
        &mut self,
        dice: &mut Dice,
        clay: Clay,
        (age, exposure): (f64, Weathering),
    ) -> Option<Stonework> {
        let (bases, flecks) = clay.palette();
        let unit = Unit::Brick {
            burnt: dice.range(0.05, 0.3),
            reclaimed: if dice.chance(0.3) {
                dice.range(0.1, 0.5)
            } else {
                0.0
            },
        };
        let stone = self.masonry(
            dice,
            (bases, flecks, 500.0, 0.18),
            (age, exposure, unit),
            0.85,
        )?;
        self.work(dice, stone, (age, exposure), (Substrate::Calcareous, 0.6))
    }

    /// The materials a structure of sawn timber `age` old stands in where
    /// `exposure` has it, painted where `paint` has it: its wood, which is
    /// also what it is fixed with, and the moss and lichen grown on it.
    pub(super) fn timberwork(
        &mut self,
        dice: &mut Dice,
        (age, exposure): (f64, Weathering),
        paint: Option<(Vec3, f64)>,
    ) -> Option<Stonework> {
        let timber = Timber {
            bases: [rgb(0x7A_5C_3E), rgb(0x9C_7C_58)],
            weathering: age,
            damp: exposure.damp,
            foot: exposure.foot,
            paint,
            seed: dice.seed(),
        };
        let material = Material::new(
            Pigment::Timber(timber),
            Finish::Coated {
                roughness: 0.72 + 0.22 * age,
            },
        )
        .with_relief(Relief::grain(0.04, 90.0, dice.seed()));
        let wood = u16::try_from(self.material(material)?).ok()?;
        let cover = match grown(dice, (age, exposure), Substrate::Siliceous) {
            Some(cover) => Some(u16::try_from(self.material(Material::new(Pigment::Cover(cover), Finish::Matte))?).ok()?),
            None => None,
        };
        Some(Stonework {
            stone: wood,
            mortar: wood,
            cover,
            age,
            softness: 0.9,
        })
    }

    /// The work a structure is laid in: its `stone`, the mortar it is bedded
    /// in, and the moss and lichen grown on it, as `age` and `exposure` have
    /// weathered stone of `substrate` that weathers as fast as `softness`.
    fn work(
        &mut self,
        dice: &mut Dice,
        stone: u16,
        (age, exposure): (f64, Weathering),
        (substrate, softness): (Substrate, f64),
    ) -> Option<Stonework> {
        let mortar = self.mortar(dice, (age, exposure))?;
        let cover = match grown(dice, (age, exposure), substrate) {
            Some(cover) => {
                let material = Material::new(Pigment::Cover(cover), Finish::Matte);
                Some(u16::try_from(self.material(material)?).ok()?)
            }
            None => None,
        };
        Some(Stonework {
            stone,
            mortar,
            cover,
            age,
            softness,
        })
    }

    fn masonry(
        &mut self,
        dice: &mut Dice,
        (bases, flecks, grain, shade): ([u32; 2], [u32; 2], f64, f64),
        (age, exposure, unit): (f64, Weathering, Unit),
        roughness: f64,
    ) -> Option<u16> {
        let masonry = Masonry {
            bases: bases.map(rgb),
            flecks: flecks.map(rgb),
            grain,
            shade,
            weathering: age,
            damp: exposure.damp,
            foot: exposure.foot,
            unit,
            seed: dice.seed(),
        };
        let material = Material::new(
            Pigment::Masonry(masonry),
            Finish::Coated {
                roughness: roughness + (0.95 - roughness) * age,
            },
        )
        .with_relief(Relief::grain(
            0.02 + 0.05 * roughness,
            grain.min(200.0),
            dice.seed(),
        ));
        u16::try_from(self.material(material)?).ok()
    }

    fn mortar(&mut self, dice: &mut Dice, (age, exposure): (f64, Weathering)) -> Option<u16> {
        self.masonry(
            dice,
            (MORTAR.0, MORTAR.1, 900.0, 0.04),
            (age, exposure, Unit::Mortar),
            0.95,
        )
    }
}

/// The cover a structure `age` old has grown where `exposure` has it, on
/// stone of `substrate`, if anything has grown on it yet.
fn grown(
    dice: &mut Dice,
    (age, exposure): (f64, Weathering),
    substrate: Substrate,
) -> Option<Cover> {
    let moss = age * exposure.damp * dice.range(0.5, 1.0);
    // Lichen takes decades to colonise stone, so a kept monument carries
    // little and a ruin a mosaic of it.
    let lichen = smooth_age(age) * smooth_age(age) * dice.range(0.3, 0.8);
    if moss <= 0.02 && lichen <= 0.02 {
        return None;
    }
    let heading = dice.range(0.0, TAU);
    Some(Cover {
        moss,
        lichen,
        damp: exposure.damp,
        drought: exposure.drought,
        foot: exposure.foot,
        shade: Vec3::new(mathf::sin(heading), 0.0, mathf::cos(heading)),
        substrate,
        seed: dice.seed(),
    })
}

/// How far lichen has spread over stone `age` old: it takes decades to
/// colonise and then keeps on.
fn smooth_age(age: f64) -> f64 {
    let a = age.clamp(0.0, 1.0);
    a * a * (3.0 - 2.0 * a)
}

impl Stage {
    /// Raise the structure `mason` laid, its own frame at `pose`, keyed
    /// `key`: its prototype to grow with the scene's, and its placing.
    pub(super) fn raise(&mut self, mason: Mason, pose: Pose, key: u32) -> Option<usize> {
        let stone = usize::from(mason.stone());
        let prototype = self.assemble(mason.finish()?)?;
        self.add(
            Shape::Instance {
                prototype,
                pose,
                scale: 1.0,
                key,
            },
            stone,
            pose,
            true,
        )
    }
}

/// How a unit was dressed.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Dressing {
    /// Sawn and tooled true, finely jointed.
    Ashlar,
    /// Squared roughly with a hammer.
    Squared,
    /// Split from its bed as it came.
    Rubble,
    /// Gathered off the fields as it lay, never dressed at all.
    Field,
    /// Laid as a floor: worn smooth by feet, which keep it clear of moss
    /// and lichen but in its joints.
    Flag,
    /// Fired in a mould.
    Brick,
    /// The mortar units are bedded in.
    Mortar,
    /// Sawn timber: its arrises rounded and its grain checked by the weather.
    Timber,
}

impl Dressing {
    /// How readily moss lodges on a unit so dressed: in a joint's mortar
    /// most, on a field stone's or a split stone's rough faces next, on a
    /// dressed face least.
    const fn affinity(self) -> f64 {
        match self {
            Self::Mortar => 1.0,
            Self::Field => 0.6,
            Self::Rubble => 0.65,
            Self::Timber => 0.55,
            Self::Squared => 0.45,
            Self::Brick => 0.35,
            Self::Ashlar => 0.25,
            Self::Flag => 0.0,
        }
    }
}

/// A column's order.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Order {
    /// Fluted, straight from the floor to a cushion and a square slab.
    Doric,
    /// Fluted, on a moulded base, under a scrolled capital.
    Ionic,
    /// Plain, on a plinth and a torus, under a plain block.
    Tuscan,
}

impl Order {
    pub(super) const ALL: [Self; 3] = [Self::Doric, Self::Ionic, Self::Tuscan];

    /// How much its shaft narrows to its neck, how many flutes it is cut
    /// in, and how much it swells by its entasis, as shares of its radius.
    const fn shaft(self) -> (f64, u8, f64) {
        match self {
            Self::Doric => (0.22, 20, 0.018),
            Self::Ionic => (0.14, 24, 0.012),
            Self::Tuscan => (0.18, 0, 0.012),
        }
    }
}

/// A wall laid in courses.
#[derive(Copy, Clone, Debug)]
pub(super) struct Wall<'a> {
    /// The middle of its foot: `x` along it, `y` up, `z` out of its front.
    pub(super) pose: Pose,
    pub(super) length: f64,
    pub(super) height: f64,
    pub(super) thickness: f64,
    pub(super) dressing: Dressing,
    /// The least and the most its courses rise, in metres, and its stones'
    /// lengths as multiples of their course's rise.
    pub(super) rise: (f64, f64),
    pub(super) long: (f64, f64),
    /// The width of its joints.
    pub(super) joint: f64,
    /// The arches it opens under.
    pub(super) openings: &'a [Opening],
    /// Whether its back shows, laid as its front is.
    pub(super) back: bool,
}

/// An arched opening through a wall: where its middle stands along the wall,
/// its span, how far its arch rises over it — half its span for a round
/// arch, less for a segmental one — the height it springs from, and the
/// depth of its ring.
#[derive(Copy, Clone, Debug)]
pub(super) struct Opening {
    pub(super) at: f64,
    pub(super) span: f64,
    pub(super) rise: f64,
    pub(super) springing: f64,
    pub(super) ring: f64,
}

/// An arch's intrados `span` across rising `rise`: the radius of the circle
/// it lies on, and how far below its springing that circle's middle stands.
fn arc(span: f64, rise: f64) -> (f64, f64) {
    let rise = rise.clamp(1e-3, 0.5 * span);
    let radius = (0.25 * span * span + rise * rise) / (2.0 * rise);
    (radius, radius - rise)
}

impl Opening {
    /// The circle its ring's back lies on: its radius, and its middle's
    /// height.
    fn extrados(&self) -> (f64, f64) {
        let (radius, below) = arc(self.span, self.rise);
        (radius + self.ring, self.springing - below)
    }

    /// How far either side of its middle the opening and its ring reach at
    /// height `y`.
    fn half_width(&self, y: f64) -> f64 {
        let (outer, middle) = self.extrados();
        if y < self.springing {
            0.5 * self.span
        } else if y < middle + outer {
            mathf::sqrt(outer * outer - (y - middle) * (y - middle))
        } else {
            0.0
        }
    }

    /// The lowest the back of its ring stands over `from..to` along the
    /// wall: where a unit cut to sit on the ring leaves a wedge to fill.
    fn ring_back(&self, (from, to): (f64, f64)) -> f64 {
        let (outer, middle) = self.extrados();
        let farthest = (from - self.at).abs().max((to - self.at).abs());
        if farthest >= outer {
            self.springing
        } else {
            (middle + mathf::sqrt(outer * outer - farthest * farthest)).max(self.springing)
        }
    }

    /// The height a unit spanning `from..to` along the wall must stand on to
    /// clear the opening, beneath which only its piers stand: `None` where
    /// the unit lies wholly within the opening below its springing.
    fn clears(&self, (from, to): (f64, f64), (bottom, top): (f64, f64)) -> Option<f64> {
        let (outer, middle) = self.extrados();
        let nearest = if from <= self.at && self.at <= to {
            0.0
        } else {
            (from - self.at).abs().min((to - self.at).abs())
        };
        if nearest >= outer
            || middle + mathf::sqrt(outer * outer - nearest * nearest) <= self.springing
        {
            return Some(bottom);
        }
        let ring_top = middle + mathf::sqrt(outer * outer - nearest * nearest);
        if nearest >= 0.5 * self.span && top <= self.springing {
            // Beside the opening and below its haunch: the pier.
            return Some(bottom);
        }
        (ring_top < top).then_some(bottom.max(ring_top))
    }
}

/// A ring of voussoirs springing from either side of an opening.
#[derive(Copy, Clone, Debug)]
pub(super) struct Ring {
    /// The middle of its springing line, in the wall's frame, and the wall's
    /// pose.
    pub(super) centre: Vec3,
    pub(super) wall: Pose,
    /// Its intrados's span and rise, and its ring's depth.
    pub(super) span: f64,
    pub(super) rise: f64,
    pub(super) depth: f64,
    /// How far through the wall it runs, either side of its middle.
    pub(super) through: (f64, f64),
    /// How many voussoirs round it.
    pub(super) count: u32,
    pub(super) dressing: Dressing,
    pub(super) joint: f64,
}

/// A column standing in its structure.
#[derive(Copy, Clone, Debug)]
pub(super) struct Column {
    pub(super) foot: Vec3,
    pub(super) radius: f64,
    pub(super) height: f64,
    pub(super) order: Order,
    pub(super) yaw: f64,
    /// Where it broke, as a share of its shaft, if it did.
    pub(super) broken: Option<f64>,
}

impl Column {
    /// The point on its axis at height `level`.
    fn at(&self, level: f64) -> Vec3 {
        Vec3::new(self.foot.x, level, self.foot.z)
    }

    /// The frame its square members are set square in.
    fn frame(&self) -> Frame {
        Frame::turned(self.yaw, 0.0)
    }
}

/// A ring of stones about a round building: a step, a course of an
/// entablature.
#[derive(Copy, Clone, Debug)]
pub(super) struct Annulus {
    pub(super) centre: Vec3,
    pub(super) inner: f64,
    pub(super) outer: f64,
    pub(super) height: f64,
    pub(super) stones: u32,
    pub(super) turn: f64,
    pub(super) dressing: Dressing,
    pub(super) joint: f64,
}

/// How bricks are laid in a wall.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum Bond {
    /// Stretchers only, each course a half brick along.
    Stretcher,
    /// A course of stretchers, then one of headers.
    English,
    /// Stretcher and header by turns in every course.
    Flemish,
    /// Three courses of stretchers to one of headers.
    Garden,
}

impl Bond {
    pub(super) const ALL: [Self; 4] = [Self::Stretcher, Self::English, Self::Flemish, Self::Garden];
}

/// A brick's length, depth and height, and the joint it is laid with.
const BRICK: (f64, f64, f64, f64) = (0.215, 0.1025, 0.065, 0.01);

/// `height` brought to the nearest top of a course of bricks.
pub(super) fn brick_courses(height: f64) -> f64 {
    let pitch = BRICK.2 + BRICK.3;
    mathf::round(height / pitch).max(1.0) * pitch
}

/// How deep a wall's pointing lies behind its faces, and how far one bed of
/// mortar runs into the next.
const RECESS: f64 = 0.006;
const OVERLAP: f64 = 0.0005;

/// Lays a structure unit by unit.
#[derive(Debug)]
pub(super) struct Mason {
    assembly: Assembly,
    work: Stonework,
    seed: u32,
    laid: u32,
}

impl Mason {
    /// A mason laying a structure in `work`, its units keyed under `seed`.
    pub(super) fn new(work: Stonework, seed: u32) -> Option<Self> {
        Some(Self {
            assembly: Assembly::with_room(1024, 0)?,
            work,
            seed,
            laid: 0,
        })
    }

    /// The structure laid, its hierarchy to build.
    pub(super) fn finish(self) -> Option<Building> {
        self.assembly.finish()
    }

    /// The material its stones are made in.
    pub(super) const fn stone(&self) -> u16 {
        self.work.stone
    }

    /// Whether it has laid nothing yet.
    pub(super) const fn is_empty(&self) -> bool {
        self.laid == 0
    }

    fn key(&mut self) -> u32 {
        self.laid = self.laid.wrapping_add(1);
        mix32(self.seed ^ mix32(self.laid))
    }

    /// Lay one unit dressed as `dressing` to `form`, `half` its size each
    /// way from its middle at `pose`.
    pub(super) fn unit(
        &mut self,
        (pose, half): (Pose, Vec3),
        form: Form,
        dressing: Dressing,
    ) -> Option<()> {
        let key = self.key();
        let wear = self.worn(dressing, half, key);
        let material = if dressing == Dressing::Mortar {
            self.work.mortar
        } else {
            self.work.stone
        };
        let cover = self.work.cover.filter(|_| dressing != Dressing::Flag);
        self.assembly.push(Part::Solid(Solid::new(
            (pose, half),
            (form, &wear),
            (material, cover, dressing.affinity()),
            key,
        )))
    }

    /// Bed mortar filling the box `half` each way about `pose`'s origin.
    fn bed(&mut self, pose: Pose, half: Vec3) -> Option<()> {
        if half.x <= 0.0 || half.y <= 0.0 || half.z <= 0.0 {
            return Some(());
        }
        self.unit((pose, half), Form::Block { fan: 0 }, Dressing::Mortar)
    }

    /// How a unit dressed as `dressing`, `half` its size, keyed `key`, has
    /// worn as long as the structure has stood.
    fn worn(&self, dressing: Dressing, half: Vec3, key: u32) -> Wear {
        let (age, soft) = (self.work.age, self.work.softness);
        let draw = |salt: u32| unit(mix32(key ^ salt));
        let least = half.x.min(half.y).min(half.z);
        let weathered = age * soft;
        let count = |most: f64| u8::try_from(mathf::round_i32(draw(3) * most).max(0)).unwrap_or(0);
        let cracked = |chance: f64, widest: f64| {
            if draw(5) < chance {
                0.0004 + widest * draw(6)
            } else {
                0.0
            }
        };
        match dressing {
            Dressing::Ashlar | Dressing::Flag => Wear {
                arris: 0.0015 + 0.02 * weathered * (0.5 + draw(1)),
                chips: count(1.0 + 7.0 * age),
                lumps: 0.0,
                pits: 0.004 * weathered * draw(4),
                crack: cracked(0.03 + 0.2 * age, 0.002 * (0.3 + age)),
            },
            Dressing::Squared => Wear {
                arris: 0.006 + 0.02 * weathered * (0.5 + draw(1)),
                chips: count(2.0 + 4.0 * age),
                lumps: least * (0.04 + 0.04 * draw(2)),
                pits: 0.003 * weathered * draw(4),
                crack: cracked(0.02 + 0.1 * age, 0.002),
            },
            Dressing::Rubble => Wear {
                arris: least * (0.18 + 0.12 * draw(1)),
                chips: count(2.0),
                lumps: least * (0.08 + 0.08 * draw(2)),
                pits: 0.004 * weathered * draw(4),
                crack: 0.0,
            },
            // Its form already wears its arrises round, so its wear is its
            // broken faces' wandering, the scars where frost spalled them and
            // its weathered skin.
            Dressing::Field => Wear {
                arris: 0.0,
                chips: count(5.0),
                lumps: least * (0.14 + 0.1 * draw(2)),
                pits: 0.0008 + 0.0025 * weathered * draw(4),
                crack: 0.0,
            },
            Dressing::Brick => Wear {
                arris: 0.0015 + 0.004 * age * draw(1),
                chips: count(0.5 + 3.0 * age),
                lumps: 0.0006 * draw(2),
                pits: 0.0015 * age * draw(4),
                crack: cracked(0.02 + 0.06 * age, 0.0008),
            },
            Dressing::Mortar => Wear {
                arris: 0.0,
                chips: 0,
                lumps: 0.0,
                pits: 0.002 + 0.004 * age,
                crack: 0.0,
            },
            Dressing::Timber => Wear {
                arris: 0.002 + 0.008 * age * (0.5 + draw(1)),
                chips: count(1.0 + 2.0 * age),
                lumps: 0.0,
                pits: 0.0008 * age * draw(4),
                crack: cracked(0.15 + 0.5 * age, 0.004 * (0.3 + age)),
            },
        }
    }

    /// Lay `wall` in courses, opening it under its arches, its stones bedded
    /// in mortar recessed behind its faces; each end of a stretch its
    /// openings leave — the wall's own ends, a pier's jamb — faced with
    /// quoins laid through it, long and short by turns.
    pub(super) fn wall(&mut self, wall: &Wall<'_>) -> Option<()> {
        let (rise, long) = (wall.rise, wall.long);
        let mut bottom = 0.0;
        let mut course = 0u32;
        while bottom < wall.height - 0.02 {
            let key = mix32(self.seed ^ mix32(course ^ 0xc0));
            let draw = |salt: u32| unit(mix32(key ^ salt));
            let mut top = (bottom + rise.0 + (rise.1 - rise.0) * draw(1)).min(wall.height);
            if wall.height - top < 0.6 * rise.0 {
                top = wall.height;
            }
            // An arch springs from a course's top: the impost.
            for opening in wall.openings {
                if opening.springing > bottom + 0.05 && opening.springing < top - 1e-6 {
                    top = top.min(opening.springing);
                }
            }
            let height = top - bottom;
            let quoin = |end: u32| {
                height
                    * if (course + end).is_multiple_of(2) {
                        long.1.min(1.6)
                    } else {
                        long.0.max(0.7)
                    }
            };
            for run in runs(wall, (bottom, top))? {
                let (from, to) =
                    self.quoined(wall, &run, (bottom, top), (quoin(0), quoin(1)), None)?;
                for face in faces(wall.back) {
                    let stagger = if face > 0.0 { draw(2) } else { draw(3) };
                    let mut at = from - stagger * height * long.0;
                    let mut stone = 0u32;
                    while at < to {
                        let along =
                            height * (long.0 + (long.1 - long.0) * unit(mix32(key ^ stone ^ 0x51)));
                        let mut end = at + along;
                        if to - end < 0.4 * height {
                            end = to;
                        }
                        let span = (at.max(from), end.min(to));
                        let bed = height * (0.9 + 0.8 * unit(mix32(key ^ stone ^ 0x52)));
                        if span.1 - span.0 > 0.05 {
                            self.course_unit(
                                wall,
                                (span, (bottom, top)),
                                face,
                                bed.min(0.5 * wall.thickness),
                            )?;
                        }
                        at = end;
                        stone += 1;
                    }
                }
            }
            bottom = top;
            course += 1;
        }
        Some(())
    }

    /// Face the ends of `run`, a stretch of `wall`'s course `bottom..top`,
    /// that show with units laid through the wall, `lengths` along it at its
    /// start and its end — bricks of `brick` where given, headers turned
    /// through — and answer the stretch left between them; the run's mortar
    /// core is bedded behind them too, its pointing recessed from the ends
    /// they face.
    fn quoined(
        &mut self,
        wall: &Wall<'_>,
        run: &Run,
        (bottom, top): (f64, f64),
        (first, last): (f64, f64),
        brick: Option<f64>,
    ) -> Option<(f64, f64)> {
        let room = run.to - run.from;
        let (mut from, mut to) = (run.from, run.to);
        let (first, last) = if first + last > room - 0.1 {
            (0.5 * room, 0.5 * room)
        } else {
            (first, last)
        };
        if run.faced.0 {
            self.through(wall, (from, from + first), (bottom, top), brick)?;
            from += first;
        }
        if run.faced.1 && to - last > from - 1e-9 {
            self.through(wall, (to - last, to), (bottom, top), brick)?;
            to -= last;
        }
        let recess = |faced: bool| if faced { RECESS } else { 0.0 };
        self.core(
            wall,
            (run.from + recess(run.faced.0), run.to - recess(run.faced.1)),
            (bottom, top),
        )?;
        Some((from, to.max(from)))
    }

    /// A unit laid through `wall` from face to face over `from..to` along it
    /// in its course `bottom..top`: a quoin, or where `brick` gives a brick's
    /// length, the bricks of a header turned through it.
    fn through(
        &mut self,
        wall: &Wall<'_>,
        (from, to): (f64, f64),
        (bottom, top): (f64, f64),
        brick: Option<f64>,
    ) -> Option<()> {
        let joint = brick.map_or(wall.joint, |_| BRICK.3);
        match brick {
            None => {
                let middle = Vec3::new(f64::midpoint(from, to), f64::midpoint(bottom, top), 0.0);
                let half = Vec3::new(
                    0.5 * (to - from - joint),
                    0.5 * (top - bottom - joint),
                    0.5 * wall.thickness,
                );
                let pose = Pose::new(wall.pose.point_to_world(middle), wall.pose.frame);
                self.unit((pose, half), Form::Block { fan: 0 }, wall.dressing)
            }
            Some(long) => {
                let count = mathf::ceil(wall.thickness / (long + joint)).max(1.0);
                let each = wall.thickness / count;
                let frame = Frame {
                    x: wall.pose.frame.z,
                    y: wall.pose.frame.y,
                    z: -wall.pose.frame.x,
                };
                for index in 0..u32::try_from(mathf::round_i32(count)).ok()? {
                    let z = -0.5 * wall.thickness + each * (f64::from(index) + 0.5);
                    let middle = Vec3::new(f64::midpoint(from, to), f64::midpoint(bottom, top), z);
                    let half = Vec3::new(
                        0.5 * (each - joint),
                        0.5 * (top - bottom - joint),
                        0.5 * (to - from - joint),
                    );
                    let pose = Pose::new(wall.pose.point_to_world(middle), frame);
                    self.unit((pose, half), Form::Block { fan: 0 }, Dressing::Brick)?;
                }
                Some(())
            }
        }
    }

    /// Bed the mortar core of `wall` over `from..to` along it in its course
    /// `bottom..top`, behind its faces.
    fn core(
        &mut self,
        wall: &Wall<'_>,
        (from, to): (f64, f64),
        (bottom, top): (f64, f64),
    ) -> Option<()> {
        let middle = Vec3::new(f64::midpoint(from, to), f64::midpoint(bottom, top), 0.0);
        let pose = Pose::new(wall.pose.point_to_world(middle), wall.pose.frame);
        let depth = 0.5 * wall.thickness - RECESS;
        // Each course's core overlaps the next a sliver, so no ray finds the
        // plane between them.
        self.bed(
            pose,
            Vec3::new(0.5 * (to - from), 0.5 * (top - bottom) + OVERLAP, depth),
        )
    }

    /// Lay a unit of `wall` spanning `from..to` along it and `bottom..top`
    /// up it on its `face` (`1.0` its front, `-1.0` its back), `bed` deep
    /// into it, clear of its openings: cut into narrower pieces where it
    /// meets an arch's ring, each standing on the ring.
    fn course_unit(
        &mut self,
        wall: &Wall<'_>,
        ((from, to), (bottom, top)): ((f64, f64), (f64, f64)),
        face: f64,
        bed: f64,
    ) -> Option<()> {
        let height = top - bottom;
        let pieces = if wall
            .openings
            .iter()
            .any(|opening| opening.clears((from, to), (bottom, top)) != Some(bottom))
        {
            let narrow = (0.8 * height).max(0.12);
            u32::try_from(mathf::round_i32(mathf::ceil((to - from) / narrow)).max(1)).ok()?
        } else {
            1
        };
        let width = (to - from) / f64::from(pieces);
        for piece in 0..pieces {
            let start = from + width * f64::from(piece);
            let span = (start, start + width);
            let mut floor = bottom;
            let mut lowest = bottom;
            let mut blocked = false;
            for opening in wall.openings {
                match opening.clears(span, (bottom, top)) {
                    Some(at) => {
                        floor = floor.max(at);
                        lowest = lowest.max(opening.ring_back(span).max(bottom).min(at));
                    }
                    None => blocked = true,
                }
            }
            if blocked {
                continue;
            }
            // A stone set on an arch's ring stands on its highest point over
            // the stone; mortar fills the wedge beneath down to the ring.
            let laid = top - floor >= 0.35 * height;
            let filled = if laid { floor } else { top };
            if filled > lowest {
                let middle = Vec3::new(
                    f64::midpoint(span.0, span.1),
                    f64::midpoint(lowest, filled),
                    face * 0.25 * wall.thickness,
                );
                let pose = Pose::new(wall.pose.point_to_world(middle), wall.pose.frame);
                self.bed(
                    pose,
                    Vec3::new(
                        0.5 * width + OVERLAP,
                        0.5 * (filled - lowest) + OVERLAP,
                        0.25 * wall.thickness - 0.5 * RECESS,
                    ),
                )?;
            }
            if !laid {
                continue;
            }
            let half = Vec3::new(
                0.5 * (width - wall.joint),
                0.5 * (top - floor - wall.joint),
                0.5 * bed,
            );
            let middle = Vec3::new(
                f64::midpoint(span.0, span.1),
                f64::midpoint(floor, top),
                face * (0.5 * wall.thickness - 0.5 * bed),
            );
            let pose = Pose::new(wall.pose.point_to_world(middle), wall.pose.frame);
            self.unit((pose, half), Form::Block { fan: 0 }, wall.dressing)?;
        }
        Some(())
    }

    /// Lay `ring`'s voussoirs, each bedded in mortar behind its faces, its
    /// keystone a little proud.
    pub(super) fn arch(&mut self, ring: &Ring) -> Option<()> {
        let count = ring.count.max(3) | 1;
        let (radius, below) = arc(ring.span, ring.rise);
        let sweep = mathf::asin((0.5 * ring.span / radius).min(1.0));
        let half_angle = sweep / f64::from(count);
        let middle = radius + 0.5 * ring.depth;
        let centre = ring.centre - Vec3::UP * below;
        let (near, far) = ring.through;
        for index in 0..count {
            // Each voussoir's stones break joint with its neighbours'.
            let lengths = segments(
                far - near,
                (0.5, 1.1),
                mix32(self.seed ^ mix32(index ^ 0xa4c)),
            );
            let angle =
                0.5 * PI - sweep + 2.0 * sweep * (f64::from(index) + 0.5) / f64::from(count);
            let key = index == count / 2;
            let depth = if key { 1.15 * ring.depth } else { ring.depth };
            let radial = Vec3::new(mathf::cos(angle), mathf::sin(angle), 0.0);
            let along = Vec3::new(mathf::sin(angle), -mathf::cos(angle), 0.0);
            let frame = Frame {
                x: ring.wall.frame.to_world(along),
                y: ring.wall.frame.to_world(radial),
                z: ring.wall.frame.z,
            };
            let half_x = (middle * half_angle - 0.5 * ring.joint).max(0.01);
            let half_y = 0.5 * depth;
            let fan = mathf::round_i32(half_angle * half_y / half_x * 100.0).clamp(-127, 127);
            let fan = i8::try_from(fan).unwrap_or(0);
            let at_ring = centre + radial * (radius + half_y);
            let mut start = near;
            for &length in lengths.iter().flatten() {
                let proud = if key { 0.03 } else { 0.0 };
                let at = at_ring + Vec3::new(0.0, 0.0, start + 0.5 * length);
                let half = Vec3::new(half_x, half_y, 0.5 * (length - ring.joint) + proud);
                let pose = Pose::new(ring.wall.point_to_world(at), frame);
                self.unit((pose, half), Form::Block { fan }, ring.dressing)?;
                start += length;
            }
            let bedded = Vec3::new(
                middle * half_angle,
                0.5 * ring.depth,
                0.5 * (far - near) - RECESS,
            );
            let at = centre + radial * middle + Vec3::new(0.0, 0.0, f64::midpoint(near, far));
            self.bed(Pose::new(ring.wall.point_to_world(at), frame), bedded)?;
        }
        Some(())
    }

    /// Wall up `opening` of `wall` in rubble from `from`, a height in the
    /// wall's frame, to its crown, the rubble's face set `back` behind the
    /// wall's: each course as wide as the intrados at its top, the mortar the
    /// stones are bedded in filling out to the arch.
    pub(super) fn infill(
        &mut self,
        wall: &Wall<'_>,
        opening: &Opening,
        (from, back): (f64, f64),
    ) -> Option<()> {
        let (radius, below) = arc(opening.span, opening.rise);
        let middle = opening.springing - below;
        let crown = opening.springing + opening.rise;
        let within = |y: f64| {
            if y <= opening.springing {
                0.5 * opening.span
            } else {
                mathf::sqrt((radius * radius - (y - middle) * (y - middle)).max(0.0))
            }
        };
        let face = 0.5 * wall.thickness - back;
        let depth = (wall.thickness - back).min(0.5);
        let joint = 0.025;
        let mut bottom = from;
        let mut course = 0u32;
        while bottom < crown - 0.06 {
            let key = mix32(self.seed ^ mix32(course ^ 0x1f));
            let draw = |salt: u32| unit(mix32(key ^ salt));
            let top = (bottom + 0.14 + 0.14 * draw(1)).min(crown);
            let reach = within(top);
            if reach < 0.06 {
                break;
            }
            let height = top - bottom;
            let (left, right) = (opening.at - reach, opening.at + reach);
            let mut at = left - draw(2) * height;
            let mut stone = 0u32;
            while at < right {
                let end = (at + height * (1.0 + 1.2 * unit(mix32(key ^ stone ^ 0x2f)))).min(right);
                let start = at.max(left);
                if end - start > 0.05 {
                    let centre = Vec3::new(
                        f64::midpoint(start, end),
                        f64::midpoint(bottom, top),
                        face - 0.5 * depth,
                    );
                    let half = Vec3::new(
                        0.5 * (end - start - joint),
                        0.5 * (height - joint),
                        0.5 * depth,
                    );
                    let pose = Pose::new(wall.pose.point_to_world(centre), wall.pose.frame);
                    self.unit((pose, half), Form::Block { fan: 0 }, Dressing::Rubble)?;
                }
                at = end;
                stone += 1;
            }
            bottom = top;
            course += 1;
        }
        // Its corners run on into the ring and the wall about it, out of
        // sight.
        let centre = Vec3::new(
            opening.at,
            f64::midpoint(from, crown),
            face - RECESS - 0.5 * depth,
        );
        let pose = Pose::new(wall.pose.point_to_world(centre), wall.pose.frame);
        self.bed(
            pose,
            Vec3::new(0.5 * opening.span, 0.5 * (crown - from), 0.5 * depth),
        )
    }

    /// Lay `ring` in bricks: rows of rowlocks, each brick on edge with its
    /// length to the arch's middle, the rows breaking joint along the barrel
    /// and their wedging joints bedded in mortar behind the faces.
    pub(super) fn rowlocks(&mut self, ring: &Ring) -> Option<()> {
        let (long, deep, high, joint) = BRICK;
        let (radius, below) = arc(ring.span, ring.rise);
        let sweep = mathf::asin((0.5 * ring.span / radius).min(1.0));
        let centre = ring.centre - Vec3::UP * below;
        let (near, far) = ring.through;
        let rows = u32::try_from(mathf::round_i32(ring.depth / (deep + joint)).max(1)).ok()?;
        for row in 0..rows {
            let inner = radius + f64::from(row) * (deep + joint);
            let count = u32::try_from(mathf::round_i32(mathf::floor(
                2.0 * sweep * inner / (high + 0.006),
            )))
            .ok()?;
            let slices =
                u32::try_from(mathf::round_i32(mathf::ceil((far - near) / (long + joint)))).ok()?;
            for index in 0..count.max(3) {
                let angle = 0.5 * PI - sweep
                    + 2.0 * sweep * (f64::from(index) + 0.5) / f64::from(count.max(3));
                let radial = Vec3::new(mathf::cos(angle), mathf::sin(angle), 0.0);
                let along =
                    ring.wall
                        .frame
                        .to_world(Vec3::new(mathf::sin(angle), -mathf::cos(angle), 0.0));
                let ring_frame = Frame {
                    x: along,
                    y: ring.wall.frame.to_world(radial),
                    z: ring.wall.frame.z,
                };
                let middle = centre + radial * (inner + 0.5 * deep);
                let stagger = if (row + index) % 2 == 0 { 0.0 } else { 0.5 };
                // A rowlock's length runs along the barrel, its end on the face.
                let lying = Frame {
                    x: ring_frame.z,
                    y: ring_frame.y,
                    z: -ring_frame.x,
                };
                for slice in 0..=slices {
                    let from = near + (f64::from(slice) - stagger) * (long + joint);
                    let (start, end) = (from.max(near), (from + long + joint).min(far));
                    if end - start < 0.4 * deep {
                        continue;
                    }
                    let at = middle + Vec3::new(0.0, 0.0, f64::midpoint(start, end));
                    let half = Vec3::new(0.5 * (end - start - joint), 0.5 * deep, 0.5 * high);
                    self.unit(
                        (Pose::new(ring.wall.point_to_world(at), lying), half),
                        Form::Block { fan: 0 },
                        Dressing::Brick,
                    )?;
                }
                // A bed of mortar behind each four bricks, filling the joints
                // that widen toward the ring's back.
                if index % 4 == 0 {
                    let step = 2.0 * sweep / f64::from(count.max(3));
                    let start = 0.5 * PI - sweep + step * f64::from(index);
                    let end = (start + 4.0 * step).min(0.5 * PI + sweep);
                    let mid = f64::midpoint(start, end);
                    let out = Vec3::new(mathf::cos(mid), mathf::sin(mid), 0.0);
                    let frame = Frame {
                        x: ring.wall.frame.to_world(Vec3::new(
                            mathf::sin(mid),
                            -mathf::cos(mid),
                            0.0,
                        )),
                        y: ring.wall.frame.to_world(out),
                        z: ring.wall.frame.z,
                    };
                    let at = centre
                        + out * (inner + 0.5 * deep)
                        + Vec3::new(0.0, 0.0, f64::midpoint(near, far));
                    // A straight bed reaches its group's ends out to the ring's
                    // back, where the arc is longest.
                    let bed = Vec3::new(
                        0.5 * (end - start) * (inner + deep + joint) + OVERLAP,
                        f64::midpoint(deep, joint) + OVERLAP,
                        0.5 * (far - near) - RECESS,
                    );
                    self.bed(Pose::new(ring.wall.point_to_world(at), frame), bed)?;
                }
            }
        }
        Some(())
    }

    /// Lay `column`: its base, its shaft in drums each a little out of true
    /// on the one below, and its capital; or, where it broke, its shaft only
    /// as far as it stands, its last drum split across. The height of its
    /// top.
    pub(super) fn column(&mut self, column: &Column) -> Option<f64> {
        let level = self.column_base(column)?;
        let level = self.shaft(column, level)?;
        if column.broken.is_some() {
            return Some(level);
        }
        self.capital(column, level)
    }

    /// `column`'s base as its order has it: the height of its top.
    fn column_base(&mut self, column: &Column) -> Option<f64> {
        let (r, frame) = (column.radius, column.frame());
        let mut level = column.foot.y;
        match column.order {
            Order::Doric => {}
            Order::Ionic => {
                let plinth = Vec3::new(1.4 * r, 0.16 * r, 1.4 * r);
                self.unit(
                    (Pose::new(column.at(level + plinth.y), frame), plinth),
                    Form::Block { fan: 0 },
                    Dressing::Ashlar,
                )?;
                level += 2.0 * plinth.y;
                level = self.moulding(
                    column.at(level),
                    (1.3 * r, 1.25 * r, 0.12 * r),
                    (1, 55),
                    frame,
                )?;
                level = self.moulding(
                    column.at(level),
                    (1.12 * r, 1.08 * r, 0.1 * r),
                    (1, -45),
                    frame,
                )?;
                level = self.moulding(
                    column.at(level),
                    (1.15 * r, 1.04 * r, 0.1 * r),
                    (1, 50),
                    frame,
                )?;
            }
            Order::Tuscan => {
                let plinth = Vec3::new(1.3 * r, 0.22 * r, 1.3 * r);
                self.unit(
                    (Pose::new(column.at(level + plinth.y), frame), plinth),
                    Form::Block { fan: 0 },
                    Dressing::Ashlar,
                )?;
                level += 2.0 * plinth.y;
                level = self.moulding(
                    column.at(level),
                    (1.18 * r, 1.05 * r, 0.12 * r),
                    (1, 60),
                    frame,
                )?;
            }
        }
        Some(level)
    }

    /// `column`'s shaft in drums from `level`, as far as it stands: the
    /// height of its top.
    fn shaft(&mut self, column: &Column, level: f64) -> Option<f64> {
        let r = column.radius;
        let (narrowing, flutes, swell) = column.order.shaft();
        let standing = column
            .broken
            .map_or(column.height, |share| column.height * share);
        let drums = u32::try_from(mathf::round_i32(column.height / (1.6 * r)).max(2)).ok()?;
        let tall = column.height / f64::from(drums);
        let radius_at = |y: f64| {
            let t = (y / column.height).clamp(0.0, 1.0);
            r * (1.0 - narrowing * t + swell * 4.0 * t * (1.0 - t))
        };
        let mut shaft = 0.0;
        let key = self.key();
        for drum in 0..drums {
            if shaft >= standing - 1e-6 {
                break;
            }
            let top = (shaft + tall).min(standing);
            let (foot_r, head_r) = (radius_at(shaft), radius_at(top));
            let taper =
                u8::try_from(mathf::round_i32((1.0 - head_r / foot_r) * 255.0).clamp(0, 255))
                    .unwrap_or(0);
            let draw = |salt: u32| unit(mix32(key ^ mix32(drum ^ salt)));
            // Flutes keep in line from drum to drum; each drum is set down a
            // hair off its neighbour's line.
            let off = Vec3::new(0.004 * (draw(1) - 0.5), 0.0, 0.004 * (draw(2) - 0.5));
            let turn = 0.004 * (draw(3) - 0.5);
            let pose = Pose::new(
                column.at(level + f64::midpoint(shaft, top)) + off,
                Frame::turned(column.yaw + turn, 0.0),
            );
            // Drums were fitted close: the joint shows where their arrises
            // wore round, not as a gap.
            let half = Vec3::new(foot_r, 0.5 * (top - shaft) - 0.0003, foot_r);
            let form = Form::Drum {
                taper,
                swell: 0,
                flutes,
            };
            if column.broken.is_some() && top >= standing - 1e-6 {
                self.broken_drum((pose, half), form)?;
            } else {
                self.unit((pose, half), form, Dressing::Ashlar)?;
            }
            shaft = top;
        }
        Some(level + standing)
    }

    /// `column`'s capital on its shaft's top at `level`, as its order has
    /// it: the height of its top.
    fn capital(&mut self, column: &Column, level: f64) -> Option<f64> {
        let (r, frame) = (column.radius, column.frame());
        let (narrowing, _, _) = column.order.shaft();
        let neck = r * (1.0 - narrowing);
        let block = |mason: &mut Self, half: Vec3, top: f64| {
            mason.unit(
                (Pose::new(column.at(top - half.y), frame), half),
                Form::Block { fan: 0 },
                Dressing::Ashlar,
            )
        };
        match column.order {
            Order::Doric => {
                let level =
                    self.moulding(column.at(level), (neck, 1.18 * r, 0.22 * r), (2, 0), frame)?;
                let abacus = Vec3::new(1.25 * r, 0.16 * r, 1.25 * r);
                block(self, abacus, level + 2.0 * abacus.y)?;
                Some(level + 2.0 * abacus.y)
            }
            Order::Ionic => {
                let level =
                    self.moulding(column.at(level), (neck, 1.05 * r, 0.12 * r), (2, 0), frame)?;
                self.volutes(column.at(level), r, column.yaw)?;
                let top = level + 0.42 * r;
                block(self, Vec3::new(1.2 * r, 0.1 * r, 1.0 * r), top)?;
                Some(top)
            }
            Order::Tuscan => {
                let level =
                    self.moulding(column.at(level), (neck, 1.08 * r, 0.16 * r), (2, 25), frame)?;
                let abacus = Vec3::new(1.2 * r, 0.18 * r, 1.2 * r);
                block(self, abacus, level + 2.0 * abacus.y)?;
                Some(level + 2.0 * abacus.y)
            }
        }
    }

    /// A moulding turned from `foot` in radius to `head` over `height`,
    /// standing on `at`, its curve's bow and bulge: the height of its top.
    fn moulding(
        &mut self,
        at: Vec3,
        (foot, head, height): (f64, f64, f64),
        (bow, bulge): (u8, i8),
        frame: Frame,
    ) -> Option<f64> {
        let half = Vec3::new(foot, 0.5 * height, head);
        let pose = Pose::new(at + Vec3::UP * (0.5 * height), frame);
        self.unit((pose, half), Form::Turned { bow, bulge }, Dressing::Ashlar)?;
        Some(at.y + height)
    }

    /// The drum a column broke at: split across where it gave, its upper
    /// face rough as the stone fractured, bitten where its edge spalled off.
    fn broken_drum(&mut self, (pose, half): (Pose, Vec3), form: Form) -> Option<()> {
        let key = self.key();
        let mut wear = self.worn(Dressing::Ashlar, half, key);
        wear.lumps = wear.lumps.max(0.25 * half.y.min(half.x));
        wear.chips = wear.chips.max(6);
        wear.arris = wear.arris.max(0.02);
        self.assembly.push(Part::Solid(Solid::new(
            (pose, half),
            (form, &wear),
            (
                self.work.stone,
                self.work.cover,
                Dressing::Rubble.affinity(),
            ),
            key,
        )))
    }

    /// An Ionic capital's scrolls over the neck at `at`: a bolster along
    /// each side, waisted, ending at either face in a volute.
    fn volutes(&mut self, at: Vec3, r: f64, yaw: f64) -> Option<()> {
        let across = Frame::turned(yaw, 0.0);
        // A bolster's own `y` runs along it, front to back.
        let lying = Frame {
            x: across.x,
            y: across.z,
            z: -Vec3::UP,
        };
        let (length, volute) = (0.82 * r, 0.38 * r);
        for side in [-1.0, 1.0] {
            let eye = at + Vec3::UP * (0.12 * r) + across.x * (side * 0.95 * r);
            let half = Vec3::new(0.22 * r, length, 0.22 * r);
            self.unit(
                (Pose::new(eye, lying), half),
                Form::Turned { bow: 1, bulge: -18 },
                Dressing::Ashlar,
            )?;
            for face in [-1.0, 1.0] {
                let disc = eye + across.z * (face * (length - 0.035 * r));
                let half = Vec3::new(volute, 0.07 * r, volute);
                self.unit(
                    (Pose::new(disc, lying), half),
                    Form::Scroll { turns: 3 },
                    Dressing::Ashlar,
                )?;
            }
        }
        let cushion = Vec3::new(0.75 * r, 0.12 * r, 0.8 * r);
        self.unit(
            (Pose::new(at + Vec3::UP * (0.24 * r), across), cushion),
            Form::Block { fan: 0 },
            Dressing::Ashlar,
        )
    }

    /// Lay `annulus`: a ring of stones about its middle, each cut to its
    /// share of the ring, jointed radially.
    pub(super) fn annulus(&mut self, annulus: &Annulus) -> Option<()> {
        let count = annulus.stones.max(6);
        let half_angle = PI / f64::from(count);
        let middle = f64::midpoint(annulus.inner, annulus.outer);
        let half_y = 0.5 * (annulus.outer - annulus.inner);
        let half_x = (middle * half_angle - 0.5 * annulus.joint).max(0.01);
        let fan =
            i8::try_from(mathf::round_i32(half_angle * half_y / half_x * 100.0).clamp(-127, 127))
                .unwrap_or(0);
        for index in 0..count {
            let angle = annulus.turn + TAU * f64::from(index) / f64::from(count);
            let radial = Vec3::new(mathf::sin(angle), 0.0, mathf::cos(angle));
            let frame = Frame {
                x: Vec3::new(-mathf::cos(angle), 0.0, mathf::sin(angle)),
                y: radial,
                z: Vec3::UP,
            };
            let centre = annulus.centre + radial * middle + Vec3::UP * (0.5 * annulus.height);
            let half = Vec3::new(half_x, half_y, 0.5 * annulus.height - 0.5 * annulus.joint);
            self.unit(
                (Pose::new(centre, frame), half),
                Form::Block { fan },
                annulus.dressing,
            )?;
            let bed = Vec3::new(
                middle * half_angle,
                half_y - RECESS,
                0.5 * annulus.height - RECESS,
            );
            self.bed(Pose::new(centre, frame), bed)?;
        }
        Some(())
    }

    /// Lay `wall` in bricks of `bond`, each course bedded in its mortar, the
    /// ends of each stretch its openings leave closed by headers turned
    /// through it, a few bricks lost from an old wall.
    pub(super) fn bricks(&mut self, wall: &Wall<'_>, bond: Bond) -> Option<()> {
        let (long, deep, high, joint) = BRICK;
        let pitch = high + joint;
        let courses =
            u32::try_from(mathf::round_i32(mathf::floor(wall.height / pitch)).max(1)).ok()?;
        let through = wall.thickness <= 2.0 * deep + joint + 0.005;
        let lost = 0.002 + 0.012 * self.work.age;
        for course in 0..courses {
            let bottom = f64::from(course) * pitch;
            let top = bottom + pitch;
            let headers_course = match bond {
                Bond::Stretcher | Bond::Flemish => false,
                Bond::English => course % 2 == 1,
                Bond::Garden => course % 4 == 3,
            };
            // The closers at a stretch's ends alternate a header's width and
            // three quarters of a brick, so the bond breaks joint round them.
            let closer = if course % 2 == 0 {
                deep + joint
            } else {
                0.75 * long + joint
            };
            for run in runs(wall, (bottom, top))? {
                let (from, to) =
                    self.quoined(wall, &run, (bottom, top), (closer, closer), Some(long))?;
                for face in faces(wall.back) {
                    if face < 0.0 && through && headers_course {
                        continue;
                    }
                    let mut at = from
                        - match bond {
                            Bond::Stretcher | Bond::Garden if course % 2 == 1 => {
                                f64::midpoint(long, joint)
                            }
                            Bond::Flemish if course % 2 == 1 => f64::midpoint(long, deep) + joint,
                            _ => 0.0,
                        };
                    let mut index = 0u32;
                    while at < to {
                        let header = headers_course || (bond == Bond::Flemish && index % 2 == 1);
                        let length = if header { deep } else { long };
                        let end = at + length + joint;
                        let span = (at.max(from), end.min(to));
                        let key =
                            mix32(self.seed ^ mix32(course ^ mix32(index ^ u32::from(face > 0.0))));
                        if unit(key) >= lost && span.1 - span.0 > 0.4 * deep {
                            let into = match (header, through) {
                                (true, true) => wall.thickness,
                                (true, false) => long,
                                (false, _) => deep,
                            };
                            self.brick(wall, (span, (bottom, top)), face, (into, header, through))?;
                        }
                        at = end;
                        index += 1;
                    }
                }
            }
        }
        Some(())
    }

    /// One brick of `wall` spanning `from..to` along it in the course
    /// `bottom..top`, on its `face`, `into` deep: a header shows its end,
    /// and through a one-brick wall it shows at both faces.
    fn brick(
        &mut self,
        wall: &Wall<'_>,
        ((from, to), (bottom, top)): ((f64, f64), (f64, f64)),
        face: f64,
        (into, header, through): (f64, bool, bool),
    ) -> Option<()> {
        for opening in wall.openings {
            if opening.clears((from, to), (bottom, top)) != Some(bottom) {
                return Some(());
            }
        }
        let joint = BRICK.3;
        // A brick is laid with its length along its own `x`; a header's
        // length runs into the wall.
        let (frame, half) = if header {
            (
                Frame {
                    x: wall.pose.frame.z,
                    y: wall.pose.frame.y,
                    z: -wall.pose.frame.x,
                },
                Vec3::new(
                    0.5 * into - 0.5 * joint,
                    0.5 * (top - bottom - joint),
                    0.5 * (to - from - joint),
                ),
            )
        } else {
            (
                wall.pose.frame,
                Vec3::new(
                    0.5 * (to - from - joint),
                    0.5 * (top - bottom - joint),
                    0.5 * into,
                ),
            )
        };
        let z = if header && through {
            0.0
        } else {
            face * (0.5 * wall.thickness - 0.5 * into)
        };
        let middle = Vec3::new(f64::midpoint(from, to), f64::midpoint(bottom, top), z);
        self.unit(
            (Pose::new(wall.pose.point_to_world(middle), frame), half),
            Form::Block { fan: 0 },
            Dressing::Brick,
        )
    }
}

/// A stretch of a wall's course its openings leave standing: from and to
/// along the wall, and whether each end shows — the wall's own end, or a
/// pier's jamb below the arch springing from it.
#[derive(Copy, Clone, Debug)]
struct Run {
    from: f64,
    to: f64,
    faced: (bool, bool),
}

/// The stretches of `wall`'s course `bottom..top` its openings leave
/// standing, in order along it.
fn runs(wall: &Wall<'_>, (bottom, top): (f64, f64)) -> Option<alloc::vec::Vec<Run>> {
    // Over an arch the ring narrows the opening as it rises: a course spans
    // all but what the ring holds at its top, its units cut to sit on the
    // ring below that.
    let level = top - 1e-6;
    let mut gaps = alloc::vec::Vec::new();
    gaps.try_reserve_exact(wall.openings.len()).ok()?;
    for opening in wall.openings {
        let reach = opening.half_width(level);
        if reach > 0.0 {
            // Below its springing a pier's jamb shows; above it the arch's
            // ring covers the course's end.
            gaps.push((
                opening.at - reach,
                opening.at + reach,
                bottom < opening.springing - 1e-6,
            ));
        }
    }
    gaps.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut runs = alloc::vec::Vec::new();
    runs.try_reserve_exact(gaps.len() + 1).ok()?;
    let (mut from, mut faced) = (-0.5 * wall.length, true);
    for &(start, end, jamb) in &gaps {
        if start > from + 0.02 {
            runs.push(Run {
                from,
                to: start.min(0.5 * wall.length),
                faced: (faced, jamb),
            });
        }
        if end > from {
            from = end;
            faced = jamb;
        }
    }
    if from < 0.5 * wall.length - 0.02 {
        runs.push(Run {
            from,
            to: 0.5 * wall.length,
            faced: (faced, true),
        });
    }
    Some(runs)
}

/// The faces of a wall laid: its front, and its back where it shows.
fn faces(back: bool) -> impl Iterator<Item = f64> {
    [1.0, -1.0].into_iter().take(if back { 2 } else { 1 })
}

/// Lengths between `least` and `most` metres, drawn under `seed`, filling
/// `length`: the stones of a voussoir's course through a wall. At most eight;
/// past that, the last runs on to fill it.
fn segments(length: f64, (least, most): (f64, f64), seed: u32) -> [Option<f64>; 8] {
    let mut out = [None; 8];
    let mut left = length;
    for (index, slot) in (0u32..).zip(out.iter_mut()) {
        if left <= 1e-6 {
            break;
        }
        let draw = least + (most - least) * unit(mix32(seed ^ index));
        let take = if left - draw < 0.5 * least || index == 7 {
            left
        } else {
            draw
        };
        *slot = Some(take);
        left -= take;
    }
    out
}

#[cfg(test)]
#[path = "courses_tests.rs"]
mod tests;
