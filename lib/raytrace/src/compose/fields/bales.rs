//! What a farmed land's cut fields are left lying in: each field's one kind
//! of bale or stack, of straw on a cereal's stubble, of hay on a meadow made
//! for hay, and wrapped for silage on a cut ley. Small oblong bales lie where
//! the baler dropped them along its rows; big round bales lie scattered
//! where it rolled them out, their axes across its way; big square bales lie
//! here and there, a few stacked two high; and sheaves stand stooked in rows
//! to dry, a meadow's hay heaped in cocks.
//!
//! Every field draws from its own key, so how far a detail lays them never
//! changes what the rest of a scene draws.

use core::f64::consts::{FRAC_PI_2, PI, TAU};

use tairix_countryside::field::FieldId;
use tairix_countryside::layout::{Layout, Parcel};
use tairix_countryside::usage::{Bale, Crop};
use tairix_countryside::Point;
use tairix_util::mathf;

use super::super::{rgb, Dice, Stage};
use super::{across_view, field_key, Built};
use crate::farmed::{Grown, Stage as Growth};
use crate::heightfield::Heightfield;
use crate::land::Land;
use crate::material::{Finish, Material, Relief};
use crate::pigment::Pigment;
use crate::prototype::{Assembly, Part, Tube};
use crate::shape::Shape;
use crate::solid::{Form, Solid, Wear};
use crate::straw::{self, Bound, Straw};
use crate::vector::{share, Frame, Pose, Vec3};

/// How far from the eye bales are laid, past which a round bale spans too
/// few pixels to show; and the places of a field's lattice one unit looks
/// at.
const REACH: f64 = 700.0;
const PLACES_A_UNIT: usize = 3000;
/// How near the eye bales are laid all round it, out of the view as in it:
/// near enough to shadow it or show in what reflects.
const BESIDE: f64 = 25.0;

/// What a field's bales are of.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Baled {
    Straw,
    Hay,
    /// Silage, wrapped in film.
    Wrapped,
}

/// A farmed land's cut fields being laid with their bales, field by field
/// nearest the eye's way first, a bounded unit at a time, into one
/// structure raised once all are laid.
#[derive(Debug)]
pub(super) struct Baling {
    assembly: Assembly,
    /// The materials each kind of bale of each thing is made in, the loose
    /// stalks of each thing, and twine, made as first wanted.
    materials: [Option<u16>; 12],
    loosened: [Option<u16>; 3],
    twine: Option<u16>,
    eye: Point,
    heading: f64,
    /// The parcels to look at, by index among the layout's, and the next.
    parcels: alloc::vec::Vec<usize>,
    next: usize,
    lot: Option<Lot>,
    seed: u64,
}

/// The field in hand: which it is, what its bales are and are of, the unit
/// ways along and across its rows, the lattice its bales are dropped on and
/// how far along it the laying has come, and its draws.
#[derive(Debug)]
struct Lot {
    field: FieldId,
    bale: Bale,
    baled: Baled,
    along: Point,
    across: Point,
    middle: Point,
    /// How far apart its rows run and its bales lie along them, and the
    /// lattice's reach either way along and across its middle.
    apart: (f64, f64),
    reach: (f64, f64),
    at: (f64, f64),
    draws: Dice,
}

impl Baling {
    /// The laying of `layout`'s bales about the eye at `eye`, looking along
    /// `heading`, its fields keyed under `seed`; `None` when the heap will not
    /// hold it.
    pub(super) fn new(layout: &Layout, (eye, heading): (Point, f64), seed: u64) -> Option<Self> {
        let mut parcels = alloc::vec::Vec::new();
        for (index, parcel) in layout.parcels().iter().enumerate() {
            if parcel.usage.bale.is_some()
                && (parcel.field.middle - eye).length()
                    < REACH + 0.5 * mathf::sqrt(parcel.field.area)
            {
                parcels.try_reserve(1).ok()?;
                parcels.push(index);
            }
        }
        Some(Self {
            assembly: Assembly::with_room(parcels.len() * 64, 0)?,
            materials: [None; 12],
            loosened: [None; 3],
            twine: None,
            eye,
            heading,
            parcels,
            next: 0,
            lot: None,
            seed,
        })
    }

    /// How far the laying has come, as a share of its fields.
    pub(super) fn done(&self) -> f64 {
        share(self.next, self.parcels.len())
    }

    /// A unit of the laying on `land`; whether every field is laid.
    pub(super) fn step(
        &mut self,
        stage: &mut Stage,
        (land, layout): (&Land, &Layout),
    ) -> Option<bool> {
        let mut looked = 0;
        while looked < PLACES_A_UNIT {
            let Some(lot) = self.lot.as_mut() else {
                let Some(&index) = self.parcels.get(self.next) else {
                    return Some(true);
                };
                self.next += 1;
                self.lot = layout
                    .parcels()
                    .get(index)
                    .and_then(|parcel| Lot::new(parcel, self.seed));
                continue;
            };
            let Some(at) = lot.next_place() else {
                self.lot = None;
                continue;
            };
            looked += 1;
            let (field, bale, baled) = (lot.field, lot.bale, lot.baled);
            let way = lot.way();
            let off = (at - self.eye).length();
            if off > REACH || (off > BESIDE && !across_view((self.eye, self.heading), at)) {
                continue;
            }
            // Only where the field lies cut: not its margin, nor a field
            // whose crop still stands.
            let lies_cut = |fields: &[Heightfield], at: Point| {
                let lie = land.grids.lie(fields, at.x, at.y);
                cut(Grown::of(lie.grown))
                    && lie.upright >= 0.85
                    && layout
                        .parcel_at(at, &Built { land, fields })
                        .is_some_and(|here| here.field.id == field)
            };
            if !lies_cut(&stage.fields, at) {
                continue;
            }
            let near = if off < LOOSE_REACH {
                Some(Near {
                    stalks: self.loose(stage, baled)?,
                    twine: self.twine(stage)?,
                    footprint: off * stage.pixel,
                })
            } else {
                None
            };
            let material = self.material(stage, (bale, baled))?;
            let fields = &stage.fields;
            let lay = Laying {
                ground: Vec3::new(at.x, land.grids.height(fields, at.x, at.y), at.y),
                normal: land.grids.normal(fields, at.x, at.y),
                way,
                material,
                near,
                cut: &|at| lies_cut(fields, at),
            };
            let lot = self.lot.as_mut()?;
            lay.bale(&mut self.assembly, &mut lot.draws, (bale, baled))?;
        }
        Some(false)
    }

    /// Raise the bales laid, if any were; `None` when the stage will not
    /// hold them.
    pub(super) fn raise(self, stage: &mut Stage, key: u32) -> Option<()> {
        if self.assembly.parts() == 0 {
            return Some(());
        }
        let material = usize::from(self.materials.iter().flatten().next().copied()?);
        let prototype = stage.assemble(self.assembly.finish()?)?;
        let pose = Pose::new(Vec3::ZERO, Frame::WORLD);
        stage.add(
            Shape::Instance {
                prototype,
                pose,
                scale: 1.0,
                key,
            },
            material,
            pose,
            false,
        )?;
        Some(())
    }

    /// The material loose stalks of `baled` are in, made the first time:
    /// waxy, catching the light along them.
    fn loose(&mut self, stage: &mut Stage, baled: Baled) -> Option<u16> {
        let slot = baled as usize;
        if let Some(made) = self.loosened.get(slot).copied().flatten() {
            return Some(made);
        }
        let [light, dark] = stalks_of(baled).map(rgb);
        let made = stage.material(Material::new(
            Pigment::Solid(light.lerp(dark, 0.3)),
            Finish::Coated {
                roughness: STALK_SHEEN,
            },
        ))?;
        let made = u16::try_from(made).ok()?;
        *self.loosened.get_mut(slot)? = Some(made);
        Some(made)
    }

    /// The material baler twine is, made the first time: orange, its
    /// polypropylene's sheen.
    fn twine(&mut self, stage: &mut Stage) -> Option<u16> {
        if let Some(made) = self.twine {
            return Some(made);
        }
        let made = stage.material(Material::new(
            Pigment::Solid(rgb(TWINE)),
            Finish::Coated { roughness: 0.45 },
        ))?;
        let made = u16::try_from(made).ok()?;
        self.twine = Some(made);
        Some(made)
    }

    /// The material `bale`s of `baled` are made in, made the first time.
    fn material(&mut self, stage: &mut Stage, (bale, baled): (Bale, Baled)) -> Option<u16> {
        let slot = bale as usize * 3 + baled as usize;
        if let Some(made) = self.materials.get(slot).copied().flatten() {
            return Some(made);
        }
        let mut dice = Dice::keyed(self.seed, slot + 1);
        let made = u16::try_from(stage.material(made_in(&mut dice, (bale, baled)))?).ok()?;
        *self.materials.get_mut(slot)? = Some(made);
        Some(made)
    }
}

/// Whether a field grown as `grown` lies cut: a cereal's stubble, a ley cut
/// for silage, or a meadow made for hay.
fn cut(grown: Grown) -> bool {
    matches!(
        grown,
        Grown::Hayed
            | Grown::Sown(
                Crop::Wheat | Crop::Barley | Crop::Oats | Crop::Ley,
                Growth::Stubble
            )
    )
}

/// Straw's stalks and hay's, as sRGB; a sheaf's ears; baler twine's orange
/// and a round bale's net's white; and silage film, black or green.
const STRAW: [u32; 2] = [0xD8_AE_5A, 0xB4_88_3E];
const HAY: [u32; 2] = [0xA8_9E_58, 0x84_7E_42];
const EARS: u32 = 0xC8_A4_5A;
const TWINE: u32 = 0xE0_62_1C;
const NET: u32 = 0xE8_E6_DE;
const FILM: [u32; 2] = [0x18_1A_1C, 0x3A_5C_3A];

/// How rough a stalk's waxy skin is: enough to glint where it turns to the
/// sun, too rough to mirror the sky.
const STALK_SHEEN: f64 = 0.7;

/// The two shades of the stalks a bale of `baled` is made of, as sRGB.
const fn stalks_of(baled: Baled) -> [u32; 2] {
    match baled {
        Baled::Straw => STRAW,
        Baled::Hay | Baled::Wrapped => HAY,
    }
}

/// Whether a `bale` of `baled` is wrapped in film: a big bale of silage.
fn filmed(bale: Bale, baled: Baled) -> bool {
    baled == Baled::Wrapped && matches!(bale, Bale::Round | Bale::Square)
}

/// The material a `bale` of `baled` is made in.
fn made_in(dice: &mut Dice, (bale, baled): (Bale, Baled)) -> Material {
    if filmed(bale, baled) {
        // Film stretched tight over the bale, creased where it overlaps.
        let film = rgb(FILM[dice.count(0, 1) as usize]);
        return Material::new(Pigment::Solid(film), Finish::Coated { roughness: 0.32 })
            .with_relief(Relief::grain(0.025, 18.0, dice.seed()));
    }
    let stalks = stalks_of(baled).map(rgb);
    let (bound, half) = match (bale, baled) {
        (Bale::Small, _) => (
            Bound::Oblong {
                twines: SMALL_TWINES,
            },
            SMALL,
        ),
        (Bale::Square, _) => (
            Bound::Oblong {
                twines: SQUARE_TWINES,
            },
            SQUARE,
        ),
        (Bale::Round, _) => (Bound::Rolled, ROUND),
        (Bale::Stook, Baled::Straw) => (Bound::Sheaf { ears: rgb(EARS) }, SHEAF),
        (Bale::Stook, _) => (Bound::Cock, COCK),
    };
    let binding = rgb(if bound == Bound::Rolled { NET } else { TWINE });
    let straw = Straw {
        stalks,
        binding,
        bound,
        half,
        seed: dice.seed(),
    };
    Material::new(
        Pigment::Straw(straw),
        Finish::Coated {
            roughness: STALK_SHEEN,
        },
    )
    .with_relief(Relief::grain(0.22, 30.0, dice.seed()))
}

/// The half extents of each kind of bale: a small one's along its length,
/// up and across; a big square one's; a round one's radius, half width and
/// radius again; a sheaf's; and a hay cock's foot, half height and head.
const SMALL: Vec3 = Vec3::new(0.45, 0.18, 0.23);
const SQUARE: Vec3 = Vec3::new(1.2, 0.45, 0.6);
/// How many strings tie a small oblong bale, and a big one.
const SMALL_TWINES: u8 = 2;
const SQUARE_TWINES: u8 = 5;
const ROUND: Vec3 = Vec3::new(0.68, 0.6, 0.68);
const SHEAF: Vec3 = Vec3::new(0.11, 0.5, 0.11);
const COCK: Vec3 = Vec3::new(0.75, 0.6, 0.08);

impl Lot {
    /// The field `parcel` holds as its bales are dropped over it, keyed under
    /// `seed`; `None` where it is baled in nothing.
    fn new(parcel: &Parcel, seed: u64) -> Option<Self> {
        let bale = parcel.usage.bale?;
        let field = &parcel.field;
        let id = field.id;
        let mut draws = Dice::keyed(seed ^ field_key(id), 3);
        let baled = match parcel.usage.used {
            tairix_countryside::usage::Use::Arable(Crop::Ley) => Baled::Wrapped,
            tairix_countryside::usage::Use::Arable(_) => Baled::Straw,
            _ => Baled::Hay,
        };
        let apart = match bale {
            Bale::Small => (draws.range(6.0, 8.0), draws.range(7.0, 11.0)),
            Bale::Round => (draws.range(20.0, 28.0), draws.range(18.0, 26.0)),
            Bale::Square => (draws.range(24.0, 32.0), draws.range(16.0, 24.0)),
            Bale::Stook => (draws.range(5.5, 7.5), draws.range(4.5, 6.5)),
        };
        let along = field.along;
        let across = Point::new(-along.y, along.x);
        let (mut long, mut wide) = (0.0f64, 0.0f64);
        for &corner in &field.cell.corners {
            let off = corner - field.middle;
            long = long.max(off.dot(along).abs());
            wide = wide.max(off.dot(across).abs());
        }
        Some(Self {
            field: id,
            bale,
            baled,
            along,
            across,
            middle: field.middle,
            apart,
            reach: (long, wide),
            at: (-long, -wide + 0.5 * apart.0),
            draws,
        })
    }

    /// The next place on the field's lattice, each jittered its own way off
    /// its row; `None` once the lattice is walked.
    fn next_place(&mut self) -> Option<Point> {
        let (rows, along) = self.apart;
        loop {
            if self.at.1 > self.reach.1 {
                return None;
            }
            if self.at.0 > self.reach.0 {
                self.at = (-self.reach.0 + along * self.draws.unit(), self.at.1 + rows);
                continue;
            }
            let (u, v) = (
                self.at.0 + along * self.draws.range(-0.3, 0.3),
                self.at.1 + rows * self.draws.range(-0.08, 0.08),
            );
            self.at.0 += along;
            return Some(self.middle + self.along * u + self.across * v);
        }
    }

    /// The way the baler went along this row, a little off the field's own.
    fn way(&mut self) -> Point {
        let turned = self.draws.range(-0.25, 0.25);
        let (sin, cos) = (mathf::sin(turned), mathf::cos(turned));
        Point::new(
            self.along.x * cos - self.along.y * sin,
            self.along.x * sin + self.along.y * cos,
        )
    }
}

/// Where one bale is laid: on the ground at `ground` facing `normal`, the
/// baler going `way`, in `material`; what it holds near the eye; and
/// whether a place about it lies cut, as a bale laid beside it must.
struct Laying<'a> {
    ground: Vec3,
    normal: Vec3,
    way: Point,
    material: u16,
    near: Option<Near>,
    cut: &'a dyn Fn(Point) -> bool,
}

/// What a bale near enough the eye for a stalk or a string to span a pixel
/// holds beyond its body: the material its stalk ends are in, and its
/// twine's; and how broad a pixel is where it lies.
#[derive(Copy, Clone, Debug)]
struct Near {
    stalks: u16,
    twine: u16,
    footprint: f64,
}

impl Laying<'_> {
    /// Whether `at` lies on cut ground.
    fn on_cut(&self, at: Vec3) -> bool {
        (self.cut)(Point::new(at.x, at.z))
    }

    /// Lay one `bale` of `baled` into `assembly` from `draws`.
    fn bale(
        &self,
        assembly: &mut Assembly,
        draws: &mut Dice,
        (bale, baled): (Bale, Baled),
    ) -> Option<()> {
        // Its length along the baler's way, on the ground's slope.
        let yaw = mathf::atan2(self.way.x, self.way.y) - FRAC_PI_2;
        let level = Frame::turned(yaw, 0.0).aligning(Vec3::UP, self.normal);
        // Straw bristles; film is stretched smooth over it.
        let bristle = if filmed(bale, baled) {
            0.0
        } else {
            straw::BRISTLE
        };
        let soft = |arris: f64, lumps: f64| Wear {
            arris,
            lumps,
            bristle,
            ..Wear::default()
        };
        match (bale, baled) {
            (Bale::Small, _) => {
                let half = SMALL * draws.range(0.92, 1.08);
                let centre = self.ground + self.normal * (half.y - 0.02);
                let worn = soft(0.05, 0.005);
                self.solid(
                    assembly,
                    (centre, level, half),
                    (Form::Block { fan: 0 }, worn),
                    draws,
                )?;
                self.tie(
                    assembly,
                    (Pose::new(centre, level), half),
                    (SMALL_TWINES, worn.arris),
                )
            }
            (Bale::Square, _) => {
                let half = SQUARE * draws.range(0.97, 1.03);
                let layers = if draws.chance(0.3) { 2 } else { 1 };
                let side = if draws.chance(0.4) { 2 } else { 1 };
                let worn = soft(0.06, 0.008);
                let over =
                    |beside: u32| (f64::from(beside) - 0.5 * f64::from(side - 1)) * 2.02 * half.z;
                for layer in 0..layers {
                    for beside in (0..side)
                        .filter(|&beside| self.on_cut(self.ground + level.z * over(beside)))
                    {
                        let up = f64::from(layer) * 2.0 * half.y + half.y - 0.02;
                        let centre = self.ground + self.normal * up + level.z * over(beside);
                        self.solid(
                            assembly,
                            (centre, level, half),
                            (Form::Block { fan: 0 }, worn),
                            draws,
                        )?;
                        self.tie(
                            assembly,
                            (Pose::new(centre, level), half),
                            (SQUARE_TWINES, worn.arris),
                        )?;
                    }
                }
                Some(())
            }
            (Bale::Round, _) => {
                // Rolled out with its axis across the baler's way.
                let half = ROUND * draws.range(0.92, 1.08);
                let rolled = Frame {
                    x: level.x,
                    y: level.z,
                    z: -level.y,
                };
                let centre = self.ground + self.normal * (half.x - 0.03);
                let drum = Form::Drum {
                    taper: 0,
                    swell: 30,
                    flutes: 0,
                };
                self.solid(
                    assembly,
                    (centre, rolled, half),
                    (drum, soft(0.12, 0.008)),
                    draws,
                )
            }
            (Bale::Stook, Baled::Straw) => self.stook(assembly, (level, soft), draws),
            (Bale::Stook, _) => {
                let half = COCK * draws.range(0.85, 1.15);
                let centre = self.ground + self.normal * (half.y - 0.05);
                let heaped = Form::Turned { bow: 3, bulge: 25 };
                self.solid(
                    assembly,
                    (centre, level, half),
                    (heaped, soft(0.0, 0.05)),
                    draws,
                )
            }
        }
    }

    /// A stook: sheaves in two rows down its line, leaning their heads
    /// together against one another to dry.
    fn stook(
        &self,
        assembly: &mut Assembly,
        (level, soft): (Frame, impl Fn(f64, f64) -> Wear),
        draws: &mut Dice,
    ) -> Option<()> {
        let pairs = draws.count(3, 5);
        let down = |pair: u32| (f64::from(pair) - 0.5 * f64::from(pairs - 1)) * STOOK_PAIRED;
        let foot =
            |down: f64, side: f64| self.ground + level.x * down + level.z * (side * STOOK_ROWS);
        // A stook stands whole on cut ground or not at all, each sheaf's foot
        // read for itself.
        if !(0..pairs).all(|pair| {
            [-1.0, 1.0]
                .into_iter()
                .all(|side| self.on_cut(foot(down(pair), side)))
        }) {
            return Some(());
        }
        for pair in 0..pairs {
            let down = down(pair);
            for side in [-1.0, 1.0] {
                let half = SHEAF * draws.range(0.9, 1.1);
                let lean = draws.range(0.18, 0.3);
                let up = (self.normal * mathf::cos(lean) - level.z * (side * mathf::sin(lean)))
                    .normalized();
                let frame = Frame {
                    x: level.x,
                    y: up,
                    z: level.x.cross(up),
                };
                let centre = foot(down, side) + up * (half.y - 0.03);
                let sheaf = Form::Drum {
                    taper: 30,
                    swell: 0,
                    flutes: 0,
                };
                self.solid(
                    assembly,
                    (centre, frame, half),
                    (sheaf, soft(0.02, 0.01)),
                    draws,
                )?;
            }
        }
        Some(())
    }

    /// One solid of `form` `half` its size each way about `centre`, turned to
    /// `frame`, worn as `wear` has it, keyed from `draws`; and the loose
    /// stalks a unit of straw sheds, where it lies near the eye.
    fn solid(
        &self,
        assembly: &mut Assembly,
        (centre, frame, half): (Vec3, Frame, Vec3),
        (form, wear): (Form, Wear),
        draws: &mut Dice,
    ) -> Option<()> {
        let key = draws.seed();
        assembly.push(Part::Solid(Solid::new(
            (Pose::new(centre, frame), half),
            (form, &wear),
            (self.material, None, 0.0),
            key,
        )))?;
        match self.near.filter(|_| wear.bristle > 0.0) {
            Some(near) => shed(
                assembly,
                (Pose::new(centre, frame), half),
                (form, near.stalks),
                (key, near.footprint),
            ),
            None => Some(()),
        }
    }

    /// Tie the oblong bale `half` its size each way about `pose`, its
    /// arrises worn `arris` round, where it lies near the eye: `twines`
    /// strings each looping lengthwise over its top and down its ends,
    /// pulled tight round its arrises and into its straw, at least as thick
    /// as shows.
    fn tie(
        &self,
        assembly: &mut Assembly,
        (pose, half): (Pose, Vec3),
        (twines, arris): (u8, f64),
    ) -> Option<()> {
        let Some(near) = self.near else {
            return Some(());
        };
        let radius = TWINE_RADIUS.max(0.35 * near.footprint);
        let round = arris.min(0.45 * half.x.min(half.y));
        // Riding on the straw it presses down.
        let out = round + TWINE_LIFT + radius;
        let corner = |(x, y): (f64, f64), turn: f64| {
            (x + out * mathf::cos(turn), y + out * mathf::sin(turn))
        };
        let (ends, top) = (half.x - round, half.y - round);
        let mut run = [(0.0, 0.0); 2 * (CORNER_STEPS + 1) + 2];
        let mut at = 0;
        let mut put = |point: (f64, f64)| {
            if let Some(slot) = run.get_mut(at) {
                *slot = point;
                at += 1;
            }
        };
        put((ends + out, -half.y + 0.02));
        for step in 0..=CORNER_STEPS {
            put(corner((ends, top), FRAC_PI_2 * share(step, CORNER_STEPS)));
        }
        for step in 0..=CORNER_STEPS {
            put(corner(
                (-ends, top),
                FRAC_PI_2 * (1.0 + share(step, CORNER_STEPS)),
            ));
        }
        put((-(ends + out), -half.y + 0.02));
        for index in 0..twines {
            let z = half.z * straw::tied_at(index, twines);
            let local = |(x, y): (f64, f64)| pose.point_to_world(Vec3::new(x, y, z));
            for pair in run.windows(2) {
                let [from, to] = pair else {
                    continue;
                };
                let (a, b) = (local(*from), local(*to));
                let along = (b - a).normalized();
                let string = Tube::new(
                    (a, b),
                    ((radius, radius), (0.0, 0.0)),
                    (near.twine, 0),
                    Frame::around(along).x,
                );
                assembly.push(Part::Tube(string))?;
            }
        }
        Some(())
    }
}

/// How far apart a stook's pairs of sheaves stand down its line, and how far
/// either side of it each row's feet.
const STOOK_PAIRED: f64 = 0.26;
const STOOK_ROWS: f64 = 0.2;

/// How thick baler twine runs, as broad as its straw is painted bound by it;
/// how far out of a bale's pressed body it rides on the straw; and how many
/// pieces it is pulled round an arris in.
const TWINE_RADIUS: f64 = 0.5 * straw::TWINE;
const TWINE_LIFT: f64 = 0.004;
const CORNER_STEPS: usize = 3;

/// How many stalk ends stand out of a square metre of a unit of straw, a
/// mat of them over every face; how long and how thick one is; and the most
/// one unit sheds.
const LOOSE: f64 = 2200.0;
const LOOSE_LENGTH: (f64, f64) = (0.008, 0.06);
const LOOSE_RADIUS: (f64, f64) = (0.0012, 0.002);
const MOST_LOOSE: f64 = 40_000.0;
/// How near the eye a bale lies for its stalk ends to stand as stalks, and
/// the fewest pixels one spans across: past where a stalk alone spans that,
/// fewer and thicker stalks stand for as much of them.
const LOOSE_REACH: f64 = 45.0;
const LOOSE_SPAN: f64 = 1.2;

/// Shed into `assembly` the stalk ends standing out of every face of the
/// unit of straw of `form` `half` its size each way about `pose`, keyed
/// `key`, in `material`, a pixel there `footprint` metres across: each
/// sprung from its face, a few more about its arrises, lying along it as
/// often as standing off it.
fn shed(
    assembly: &mut Assembly,
    (pose, half): (Pose, Vec3),
    (form, material): (Form, u16),
    (key, footprint): (u32, f64),
) -> Option<()> {
    let mut draws = Dice::keyed(u64::from(key), 7);
    let round = !matches!(form, Form::Block { .. });
    let area = if round {
        TAU * half.x * (half.x + 2.0 * half.y)
    } else {
        8.0 * (half.x * half.y + half.y * half.z + half.z * half.x)
    };
    // How many fine stalk ends each stands for, kept as broad and as much
    // longer as covers as much of the face.
    let merged = (0.5 * LOOSE_SPAN * footprint / LOOSE_RADIUS.1).max(1.0);
    let count = (LOOSE * area / (merged * mathf::sqrt(merged))).min(MOST_LOOSE);
    let mut shed = 0.0;
    while shed < count {
        shed += 1.0;
        let (at, out, wound) = sprung(&mut draws, half, round);
        // Where its layers were wound, a stalk lies round with them.
        let (side, turned) = match wound {
            Some(round) => (
                round,
                draws.range(-WINDING, WINDING) + if draws.chance(0.5) { 0.0 } else { PI },
            ),
            None => (Frame::around(out).x, draws.range(0.0, TAU)),
        };
        let lying = side * mathf::cos(turned) + out.cross(side) * mathf::sin(turned);
        // Most lie low along the face, a few stand off it.
        let rising = draws.unit();
        let standing = 0.05 + 0.75 * rising * rising;
        let way = (out * standing + lying * (1.0 - standing)).normalized();
        let short = draws.unit();
        let length = (LOOSE_LENGTH.0 + (LOOSE_LENGTH.1 - LOOSE_LENGTH.0) * short * short)
            * mathf::sqrt(merged);
        let radius = draws.range(LOOSE_RADIUS.0, LOOSE_RADIUS.1) * merged;
        // Rooted within the face, springing from among the stalks standing
        // proud on it.
        let (root, from) = (at - out * 0.01, at + out * (0.25 * straw::BRISTLE));
        let tip = from + way * length;
        let (root, tip) = (pose.point_to_world(root), pose.point_to_world(tip));
        let along = (tip - root).normalized();
        let stalk = Tube::new(
            (root, tip),
            ((radius, 0.7 * radius), (0.0, length)),
            (material, draws.seed()),
            Frame::around(along).x,
        );
        assembly.push(Part::Tube(stalk))?;
    }
    Some(())
}

/// How far off the way round its winding, in radians, a stalk lies on a
/// round bale's end.
const WINDING: f64 = 0.35;

/// A place on the face of a unit `half` its size each way, round about its
/// `y` where it is `round`, the unit way out of the face there, and on a
/// round unit's end the way round its axis its layers were wound: more
/// often about its arrises, where the straw is least held.
fn sprung(draws: &mut Dice, half: Vec3, round: bool) -> (Vec3, Vec3, Option<Vec3>) {
    let edged = |draws: &mut Dice| {
        let share = draws.range(-1.0, 1.0);
        if draws.chance(0.2) {
            share.signum() * draws.range(0.85, 1.0)
        } else {
            share
        }
    };
    if round {
        let ends = half.x / (half.x + 2.0 * half.y);
        if draws.chance(ends) {
            let (radius, turn) = (half.x * mathf::sqrt(draws.unit()), draws.range(0.0, TAU));
            let up = draws.sign();
            let (sin, cos) = (mathf::sin(turn), mathf::cos(turn));
            let at = Vec3::new(radius * cos, up * half.y, radius * sin);
            return (at, Vec3::new(0.0, up, 0.0), Some(Vec3::new(-sin, 0.0, cos)));
        }
        let turn = draws.range(0.0, TAU);
        let out = Vec3::new(mathf::cos(turn), 0.0, mathf::sin(turn));
        return (
            out * half.x + Vec3::new(0.0, edged(draws) * half.y, 0.0),
            out,
            None,
        );
    }
    // A face as likely as its share of the unit's area.
    let (ends, beds, sides) = (half.y * half.z, half.x * half.z, half.x * half.y);
    let pick = draws.range(0.0, ends + beds + sides);
    let axis = if pick < ends {
        0
    } else if pick < ends + beds {
        1
    } else {
        2
    };
    let sign = draws.sign();
    let mut along = |index: usize, extent: f64| {
        if index == axis {
            sign * extent
        } else {
            edged(draws) * extent
        }
    };
    let at = Vec3::new(along(0, half.x), along(1, half.y), along(2, half.z));
    let out = |index: usize| if index == axis { sign } else { 0.0 };
    (at, Vec3::new(out(0), out(1), out(2)), None)
}

#[cfg(test)]
#[path = "bales_tests.rs"]
mod tests;
