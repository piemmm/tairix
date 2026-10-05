//! Deadwood: the trees a wood has lost. A fallen trunk, sagging where it
//! lies, the stubs of its limbs snapped short; broken from its crown and,
//! where the wind threw it, tearing up the plate of soil its roots held, the
//! roots broken off about its rim, or else broken at its foot too. And a
//! stump, snapped or sawn, its foot still flaring into the roots it stands
//! on. Every break is torn wood, never a rounded cap.
//!
//! Dead wood goes on decaying. A sawn face checks as it dries, cracks
//! running in from its rim, and a felled stump keeps the step between the
//! notch and the back cut and the torn hinge between them. Its bark sloughs
//! away in sheets (its material's), its heart rots out, a hollow deepening in
//! a stump's face and a log's breaks, and rot fruits in brackets shelving out
//! from its sides, zoned in the rings they grew in, their pores beneath. A
//! broadleaf stump sends up shoots from its foot.

use alloc::vec::Vec;
use core::f64::consts::{PI, TAU};

use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::mathf;

use crate::flare::{Flare, Lobe, MOST_LOBES};
use crate::foot::{Foot, FLARE_TOP, ROOTS, SWELL, SWELL_REACH};
use crate::fracture::{past, running, snapped, tear, Break, Grain, HOLLOWS};
use crate::noise::noise3;
use crate::prototype::{point, Assembly, Building, Mesh, Part, Tube};
use crate::sample::mix32;
use crate::tree::{leaf, Leafing};
use crate::vector::{real, single, wrapped, Frame, Vec3};

/// The segments a fallen trunk is laid in.
const LOG_SEGMENTS: u32 = 10;

/// How much a fallen trunk narrows toward where its crown was.
pub(crate) const LOG_TAPER: f64 = 0.45;

/// How far a fallen trunk has sunk into the litter it lies on, as a share of
/// its radius.
pub(crate) const SUNK: f64 = 0.18;

/// How far a dead tree's foot flares, at the least and the most.
const FLARE: (f64, f64) = (0.4, 0.75);

/// The materials dead wood is made in: its bark; the wood a break or a cut
/// shows; the rotten wood its heart crumbles to; the bark's own edge where it
/// was torn or cut through, darker than either; and the soil a thrown
/// trunk's roots tore up.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Woods {
    pub(crate) bark: u16,
    pub(crate) wood: u16,
    pub(crate) rot: u16,
    pub(crate) edge: u16,
    pub(crate) soil: u16,
}

impl Woods {
    const fn grain(self) -> Grain {
        Grain {
            wood: self.wood,
            rot: self.rot,
            edge: self.edge,
        }
    }
}

/// How dead wood shelves: in thin brackets in close tiers, as a turkey tail
/// does, or in thick woody ones, as an artist's bracket or a birch polypore.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Habit {
    Thin,
    Thick,
}

/// The fungus rot fruits in on dead wood: how it shelves, and the materials
/// of its brackets' zones from their root outward, of their margins, and of
/// the pores beneath.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Fungus {
    pub(crate) habit: Habit,
    pub(crate) zones: [u16; 2],
    pub(crate) margin: u16,
    pub(crate) pores: u16,
}

/// How far dead wood has gone: how long it has lain, from freshly fallen or
/// cut at nought to all but rotted away at one, and the fungus fruiting on
/// it, if any is.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Decay {
    pub(crate) age: f64,
    pub(crate) fungus: Option<Fungus>,
}

/// What a broadleaf stump sends up from its foot: the bark of its shoots,
/// their leaves if they are in leaf, and how its kind bears them.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Sprouting {
    pub(crate) bark: u16,
    pub(crate) leaves: Option<u16>,
    pub(crate) leafing: Leafing,
}

/// A piece of deadwood being made: what it is assembled from, its draws,
/// and the key its last part took.
struct Timber {
    assembly: Assembly,
    dice: NonCryptoRng,
    key: u32,
}

impl Timber {
    /// A piece grown from `seed`, room made for `parts` parts and
    /// `vertices` vertices; `None` when the heap will not hold them.
    fn new(seed: u64, (parts, vertices): (usize, usize)) -> Option<Self> {
        Some(Self {
            assembly: Assembly::with_room(parts, vertices)?,
            dice: NonCryptoRng::seed_from_u64(seed),
            key: mix32(u32::try_from(seed & 0xffff_ffff).unwrap_or(0)),
        })
    }

    fn unit(&mut self) -> f64 {
        self.dice.next_f64()
    }

    fn range(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }

    /// A count from `low` to `high`, both taken.
    fn count(&mut self, (low, high): (u32, u32)) -> u32 {
        low + self.dice.next_u32() % (high - low + 1)
    }

    /// A key not yet taken.
    fn next_key(&mut self) -> u32 {
        self.key = mix32(self.key ^ 0x9e37_79b9);
        self.key
    }

    /// A tube from `a` to `b`, `radii` thick at either end, `stem` along its
    /// stem there, in `material` and keyed afresh, its bark begun round it
    /// from its top as a fallen trunk lies, and its ends `open` where it
    /// broke.
    fn tube(
        &mut self,
        (a, b): (Vec3, Vec3),
        radii: (f64, f64),
        (stem, material): ((f64, f64), u16),
        open: [bool; 2],
    ) -> Option<Tube> {
        let key = self.next_key();
        let tube = Tube::new((a, b), (radii, stem), (material, key), Vec3::UP).opened(open);
        self.assembly.push(Part::Tube(tube))?;
        Some(tube)
    }

    /// Tear `brk` in `woods`.
    fn torn(&mut self, brk: &Break<'_>, woods: Woods) -> Option<()> {
        let torn = tear(brk, woods.grain(), &mut self.dice)?;
        self.assembly.mesh(&torn.points, &torn.faces)
    }

    /// The break where wood running along `way` snapped off at `end`,
    /// `radius` thick there, in `woods`, `age` as long as it has lain.
    fn snap(
        &mut self,
        (end, way, radius): (Vec3, Vec3, f64),
        (woods, age): (Woods, f64),
    ) -> Option<()> {
        let torn = snapped((end, way, radius), (woods.grain(), age), &mut self.dice)?;
        self.assembly.mesh(&torn.points, &torn.faces)
    }

    /// Brackets of `fungus` shelving out from the wood at `hosts`, each a
    /// point on its surface and the level way out from it there: a tier of
    /// shelves at each, overlapping up the wood.
    fn brackets(&mut self, hosts: &[(Vec3, Vec3)], fungus: Fungus) -> Option<()> {
        for &(root, out) in hosts {
            let (tiers, reach) = match fungus.habit {
                Habit::Thin => (self.count((3, 7)), self.range(0.025, 0.06)),
                Habit::Thick => (self.count((1, 3)), self.range(0.06, 0.18)),
            };
            let mut at = root;
            for _ in 0..tiers {
                let jut = reach * self.range(0.7, 1.15);
                let turn = self.range(-0.35, 0.35);
                let way = Frame::about(Vec3::UP, turn).to_world(out);
                self.bracket((at, way), jut, fungus)?;
                let climb = match fungus.habit {
                    Habit::Thin => self.range(0.25, 0.45) * jut,
                    Habit::Thick => self.range(0.35, 0.6) * jut,
                };
                let sideways = Vec3::UP.cross(out) * self.range(-0.3, 0.3) * jut;
                at = at + Vec3::UP * climb + sideways;
            }
        }
        Some(())
    }

    /// One bracket shelving `reach` out from `root` along the level `out`: a
    /// shelf broader than it reaches, its margin uneven where it grew, ridged
    /// on top in the zones of the years it grew in, a thick one hoof-shaped
    /// and thickest at its root, a thin one waving, its pores beneath.
    fn bracket(&mut self, (root, out): (Vec3, Vec3), reach: f64, fungus: Fungus) -> Option<()> {
        const RINGS: [f64; 10] = [0.0, 0.14, 0.28, 0.42, 0.55, 0.67, 0.78, 0.87, 0.94, 1.0];
        const SWEEP: u32 = 16;
        let breadth = reach * self.range(1.3, 1.9);
        let (thick, zones) = match fungus.habit {
            Habit::Thin => (reach * self.range(0.06, 0.1), self.range(6.0, 10.0)),
            Habit::Thick => (reach * self.range(0.25, 0.45), self.range(4.0, 7.0)),
        };
        let droop = self.range(-0.08, 0.12) * reach;
        let side = Vec3::UP.cross(out).normalized();
        let ruffle = mix32(self.key ^ 0x6b);
        let mut points = Vec::new();
        points
            .try_reserve_exact(2 * RINGS.len() * (SWEEP as usize + 1))
            .ok()?;
        for surface in [1.0, -1.0] {
            for &ring in &RINGS {
                for step in 0..=SWEEP {
                    let sweep = PI * (f64::from(step) / f64::from(SWEEP) - 0.5);
                    // Its margin grew unevenly, and a thin one's waves.
                    let uneven = 0.12 * noise3(Vec3::new(sweep * 2.0, 0.0, 0.0), ruffle);
                    let wave = match fungus.habit {
                        Habit::Thin => {
                            0.1 * noise3(Vec3::new(sweep * 4.0, ring, 0.0), ruffle ^ 0x1)
                        }
                        Habit::Thick => 0.0,
                    };
                    let flat = out * (reach * ring * mathf::cos(sweep) * (1.0 + uneven))
                        + side * (0.5 * breadth * ring * mathf::sin(sweep) * (1.0 + 0.5 * uneven));
                    let rise = if surface > 0.0 {
                        // Ridged where each year's growth began.
                        let ridged = 1.0 - 0.12 * mathf::cos(TAU * zones * ring).abs();
                        thick
                            * mathf::sqrt((1.0 - ring * ring).max(0.0))
                            * (0.6 + 0.4 * (1.0 - ring))
                            * ridged
                    } else {
                        -0.3 * thick * (1.0 - ring)
                    };
                    let edge = 0.06 * thick * surface;
                    points.push(
                        root + flat + Vec3::UP * (rise + edge + wave * thick - droop * ring * ring),
                    );
                }
            }
        }
        let (width, rings) = (SWEEP + 1, u32::try_from(RINGS.len()).ok()?);
        let at = |surface: u32, ring: u32, step: u32| (surface * rings + ring) * width + step;
        let mut faces = Vec::new();
        faces
            .try_reserve_exact((4 * RINGS.len() + 2) * SWEEP as usize)
            .ok()?;
        let margin = rings - 1;
        for ring in 0..margin {
            let zone = if ring + 1 == margin {
                fungus.margin
            } else {
                *fungus.zones.get((ring % 2) as usize)?
            };
            for step in 0..SWEEP {
                let (a, b) = (at(0, ring, step), at(0, ring, step + 1));
                let (c, d) = (at(0, ring + 1, step), at(0, ring + 1, step + 1));
                faces.push(([a, c, b], zone));
                faces.push(([b, c, d], zone));
                let (a, b) = (at(1, ring, step), at(1, ring, step + 1));
                let (c, d) = (at(1, ring + 1, step), at(1, ring + 1, step + 1));
                faces.push(([a, b, c], fungus.pores));
                faces.push(([b, d, c], fungus.pores));
            }
        }
        // The margin joining the two surfaces round the shelf's edge.
        for step in 0..SWEEP {
            let (a, b) = (at(0, margin, step), at(0, margin, step + 1));
            let (c, d) = (at(1, margin, step), at(1, margin, step + 1));
            faces.push(([a, c, b], fungus.margin));
            faces.push(([b, c, d], fungus.margin));
        }
        self.assembly.mesh(&points, &faces)
    }

    /// Shoots of `sprouting` sent up from about the foot or the cut rim of a
    /// stump standing in `frame`, `height` tall, whose surface lies `girth`
    /// out from its axis at each height up it and way out: arching out and
    /// then up toward the light, leafy along their upper reach.
    fn shoots(
        &mut self,
        (frame, height, girth): (Frame, f64, &dyn Fn(f64, Vec3) -> f64),
        sprouting: Sprouting,
    ) -> Option<()> {
        const SEGMENTS: u32 = 6;
        let count = self.count((4, 11));
        for _ in 0..count {
            let around = TAU * self.unit();
            let radial = frame.x * mathf::cos(around) + frame.z * mathf::sin(around);
            // From just below the cut, or from the root collar.
            let up = if self.unit() < 0.55 {
                height * self.range(0.75, 0.95)
            } else {
                height * self.range(0.05, 0.25)
            };
            let base = frame.y * up + radial * (0.95 * girth(up, radial));
            let length = self.range(0.35, 1.6);
            let thick = self.range(0.003, 0.008) * (0.6 + 0.4 * length);
            let mut heading = (radial * 0.8 + frame.y * 0.6).normalized();
            let mut at = base;
            let step = length / f64::from(SEGMENTS);
            for segment in 0..SEGMENTS {
                // Toward the light as it grows, wandering a little.
                let wander = Vec3::new(self.range(-0.15, 0.15), 0.0, self.range(-0.15, 0.15));
                heading = (heading + Vec3::UP * 0.35 + wander).normalized();
                let next = at + heading * step;
                let (t0, t1) = (
                    f64::from(segment) / f64::from(SEGMENTS),
                    f64::from(segment + 1) / f64::from(SEGMENTS),
                );
                self.tube(
                    (at, next),
                    (thick * (1.0 - 0.7 * t0), thick * (1.0 - 0.7 * t1)),
                    ((t0 * length, t1 * length), sprouting.bark),
                    [false; 2],
                )?;
                if let (Some(leaves), true) = (sprouting.leaves, t0 >= 0.25) {
                    let frame = running(heading);
                    for pair in 0..2u32 {
                        let base = at + (next - at) * f64::midpoint(f64::from(pair), 0.5);
                        // A shoot's leaves come larger than its kind's twigs bear.
                        let leafing = Leafing {
                            length: 1.3 * sprouting.leafing.length,
                            ..sprouting.leafing
                        };
                        if let Some(blade) = leaf(
                            &leafing,
                            (base, frame, length),
                            (leaves, &mut self.key),
                            &mut self.dice,
                        ) {
                            self.assembly.push(Part::Leaf(blade))?;
                        }
                    }
                }
                at = next;
            }
        }
        Some(())
    }
}

/// A fallen trunk `length` long and `radius` thick at its foot, lying along
/// its frame's `z` on the ground its `y` stands up from, in `woods`: thrown
/// by the wind, the plate of soil its roots held torn up with it, if
/// `thrown`, and broken off at its foot otherwise, and its crown broken from
/// it, each break torn, decayed as `decay` has it; grown from `seed`, its
/// hierarchy still to build. `None` when the heap will not hold it.
pub(crate) fn log(
    length: f64,
    radius: f64,
    (woods, thrown): (Woods, bool),
    decay: Decay,
    seed: u64,
) -> Option<Building> {
    let mut timber = Timber::new(seed, (256, 8192))?;
    let (axis, foot) = lie(&mut timber, (length, radius), (woods, thrown))?;
    stubs(&mut timber, &axis, (woods, decay.age))?;
    let (first, second) = (axis.first()?.0, axis.get(1)?.0);
    if let Some(tube) = foot {
        Plate::new(&mut timber, tube, radius)?.lay(&mut timber, (woods, decay.age))?;
    } else {
        timber.snap(
            (first, (first - second).normalized(), radius),
            (woods, decay.age),
        )?;
    }
    let last = LOG_SEGMENTS as usize;
    let (&(tip, thick), before) = (axis.get(last)?, axis.get(last - 1)?.0);
    timber.snap(
        (tip, (tip - before).normalized(), thick),
        (woods, decay.age),
    )?;
    if let Some(fungus) = decay.fungus {
        // Brackets shelve from its flanks, where they can grow out level and
        // shed their spores beneath.
        let mut hosts = [(Vec3::ZERO, Vec3::UP); 3];
        let count = timber.count((1, 3)) as usize;
        for host in hosts.iter_mut().take(count) {
            let along = timber.range(0.15, 0.85);
            let index = usize::try_from(mathf::round_i32(along * real(LOG_SEGMENTS as usize)))
                .unwrap_or(0)
                .min(last);
            let &(centre, thick) = axis.get(index)?;
            let flank = if timber.unit() < 0.5 { -1.0 } else { 1.0 };
            let rise = timber.range(-0.3, 0.6);
            let out = Vec3::new(flank, 0.0, 0.0);
            *host = (centre + (out + Vec3::UP * rise).normalized() * thick, out);
        }
        timber.brackets(hosts.get(..count)?, fungus)?;
    }
    timber.assembly.finish()
}

/// A fallen trunk's axis as it lies: each segment's end and its radius there.
type Lying = [(Vec3, f64); LOG_SEGMENTS as usize + 1];

/// Lay a fallen trunk `length` long and `radius` thick at its foot in `woods`,
/// sagging where it lies and open at both ends for what closes them: its axis,
/// and, if it was `thrown`, its first limb still to lay, for its root plate
/// to flare.
fn lie(
    timber: &mut Timber,
    (length, radius): (f64, f64),
    (woods, thrown): (Woods, bool),
) -> Option<(Lying, Option<Tube>)> {
    let step = length / f64::from(LOG_SEGMENTS);
    let resting = |radius: f64| (1.0 - SUNK) * radius;
    let mut at = Vec3::new(0.0, resting(radius), 0.0);
    let mut heading = 0.0f64;
    let mut axis = [(at, radius); LOG_SEGMENTS as usize + 1];
    let mut foot = None;
    for segment in 0..LOG_SEGMENTS {
        let along = f64::from(segment + 1) / f64::from(LOG_SEGMENTS);
        let thick = radius * (1.0 - LOG_TAPER * along);
        heading += 0.06 * (timber.unit() - 0.5);
        let end = Vec3::new(
            at.x + mathf::sin(heading) * step,
            resting(thick),
            at.z + mathf::cos(heading) * step,
        );
        let (r0, travelled) = (
            axis.get(segment as usize).map_or(radius, |&(_, r)| r),
            f64::from(segment) * step,
        );
        let tube = Tube::new(
            (at, end),
            ((r0, thick), (travelled, travelled + step)),
            (woods.bark, timber.next_key()),
            Vec3::UP,
        )
        .opened([segment == 0, segment + 1 == LOG_SEGMENTS]);
        if segment == 0 && thrown {
            foot = Some(tube);
        } else {
            timber.assembly.push(Part::Tube(tube))?;
        }
        if let Some(slot) = axis.get_mut(segment as usize + 1) {
            *slot = (end, thick);
        }
        at = end;
    }
    Some((axis, foot))
}

/// The stubs of a fallen trunk's limbs along `axis`, in `woods`, `age` as long
/// as it has lain: most toward where its crown was, snapped off torn, shorter
/// the longer it has lain and short where a limb pointed into the ground.
fn stubs(timber: &mut Timber, axis: &Lying, (woods, age): (Woods, f64)) -> Option<()> {
    for _ in 0..timber.count((4, 10)) {
        let along = 0.25 + 0.75 * mathf::sqrt(timber.unit());
        let index = usize::try_from(mathf::round_i32(mathf::floor(
            along * f64::from(LOG_SEGMENTS),
        )))
        .unwrap_or(0);
        let &(centre, thick) = axis.get(index.min(LOG_SEGMENTS as usize))?;
        let around = TAU * timber.unit();
        let (side, up) = (mathf::cos(around), mathf::sin(around));
        let short = if up < -0.3 { 0.3 } else { 1.0 } * (1.0 - 0.6 * age);
        let reach = short * thick * (1.5 + 5.0 * timber.unit());
        let out = Vec3::new(side, up, 0.7).normalized();
        let from = centre + Vec3::new(side, up, 0.0) * (0.7 * thick);
        let stub = thick * (0.22 + 0.2 * timber.unit());
        let (end, tip) = (from + out * reach, 0.75 * stub);
        timber.tube(
            (from, end),
            (stub, tip),
            ((0.0, reach), woods.bark),
            [false, true],
        )?;
        timber.snap((end, out, tip), (woods, age))?;
    }
    Some(())
}

/// How far out a thrown trunk's root plate reaches, in the trunk's radii, at
/// the least and the most, and how thick its soil is at the trunk.
const PLATE_REACH: (f64, f64) = (2.4, 3.6);
const PLATE_THICK: (f64, f64) = (1.6, 2.6);

/// How many rings and spokes a root plate's soil is laid in: finer than its
/// clods, the rings closer toward its middle where the spokes crowd.
const PLATE_RINGS: u32 = 30;
const PLATE_SPOKES: u32 = 144;

/// The plate a thrown trunk tore up at its foot: a slab of lumpy soil bound
/// about its roots, standing on edge, its former underside facing away from
/// the trunk. It holds where the trunk's foot lies and the way its trunk runs
/// from there, the frame whose `y` faces out of its underside, the trunk's
/// radius, how far out it reaches and how thick its soil is at the trunk, the
/// salt its outline and clods are drawn under, the trunk's flared limb, and
/// each of the roots that ran out of the trunk's lobes, its angle round the
/// trunk and its thickness.
struct Plate {
    foot: Vec3,
    axis: Vec3,
    frame: Frame,
    radius: f64,
    reach: f64,
    thick: f64,
    salt: u32,
    tube: Tube,
    roots: [(f64, f64); MOST_LOBES],
    count: usize,
}

impl Plate {
    /// The plate `tube`, a thrown trunk's first limb `radius` thick, tore up:
    /// the limb laid flaring into it toward its roots. `None` when the heap
    /// will not hold it.
    fn new(timber: &mut Timber, tube: Tube, radius: f64) -> Option<Self> {
        let (foot, end) = (point(tube.a), point(tube.b));
        let axis = (end - foot).normalized();
        let flare = timber.range(FLARE.0, FLARE.1);
        let count = timber.count(ROOTS) as usize;
        let turn = TAU * timber.unit();
        let mut lobes = [Lobe::default(); MOST_LOBES];
        let mut roots = [(0.0, 0.0); MOST_LOBES];
        for (index, (lobe, root)) in lobes.iter_mut().zip(&mut roots).take(count).enumerate() {
            let angle = turn + TAU * real(index) / real(count) + timber.range(-0.35, 0.35);
            let thick = radius * timber.range(0.2, 0.32);
            let out = flare * timber.range(0.8, 1.3);
            *lobe = Lobe {
                angle: single(angle),
                out: single(out),
                width: single(
                    (1.5 * thick / (radius * (1.0 + SWELL * flare + out))).clamp(0.3, 0.6),
                ),
                climb: single(radius * timber.range(0.35, 0.6)),
            };
            *root = (angle, thick);
        }
        let flared = Flare::new(
            0.0,
            FLARE_TOP * radius,
            (SWELL * flare, SWELL_REACH * radius),
            lobes.get(..count)?,
        )?;
        let index = timber.assembly.flare(flared)?;
        timber.assembly.push(Part::Tube(tube.flared(index)))?;
        Some(Self {
            foot,
            axis,
            frame: running(-axis),
            radius,
            reach: radius * timber.range(PLATE_REACH.0, PLATE_REACH.1),
            thick: radius * timber.range(PLATE_THICK.0, PLATE_THICK.1),
            salt: timber.dice.next_u32(),
            tube,
            roots,
            count,
        })
    }

    /// Lay the plate in `woods`, `age` as long as it has lain: its soil, and
    /// the roots it holds.
    fn lay(&self, timber: &mut Timber, (woods, age): (Woods, f64)) -> Option<()> {
        self.soil(timber, woods)?;
        let mut travelled = 0.0;
        self.laterals(timber, (woods, age), &mut travelled)?;
        self.sinkers(timber, (woods, age), &mut travelled)?;
        self.fine(timber, woods)
    }

    /// The way out from the plate's middle at `angle`.
    fn way(&self, angle: f64) -> Vec3 {
        self.frame.x * mathf::cos(angle) + self.frame.z * mathf::sin(angle)
    }

    /// How far out its rim lies at `angle`: torn out unevenly.
    fn rim(&self, angle: f64) -> f64 {
        let round = Vec3::new(mathf::cos(angle), mathf::sin(angle), 0.0);
        self.reach
            * (1.0
                + 0.3 * noise3(round * 1.7, self.salt)
                + 0.12 * noise3(round * 5.0, self.salt ^ 0x1))
    }

    /// How its soil stands proud at `p`: crumbling clods, ridged where they
    /// broke from one another.
    fn lump(&self, p: Vec3) -> f64 {
        let scale = 0.25 * self.radius;
        noise3(p * (1.0 / scale), self.salt ^ 0x5)
            + 0.6 * (0.5 - noise3(p * (2.6 / scale), self.salt ^ 0x6).abs())
            + 0.3 * (0.5 - noise3(p * (6.0 / scale), self.salt ^ 0x7).abs())
    }

    /// How deep its soil is `share` of the way out to its rim, as a share of
    /// its depth at the trunk: a bulbous mass, thinning only near its torn
    /// rim.
    fn depth(share: f64) -> f64 {
        let rim = share.clamp(0.0, 1.0);
        1.0 - 0.6 * rim * rim * rim
    }

    /// Its underside `share` of the way out at `angle`, clods aside.
    fn face(&self, (share, angle): (f64, f64)) -> Vec3 {
        self.foot
            + self.way(angle) * (share * self.rim(angle))
            + self.frame.y * (0.75 * self.thick * Self::depth(share))
    }

    /// Where on its underside a root running past `at` lies, its back
    /// standing out of the soil.
    fn underside(&self, at: Vec3) -> Vec3 {
        let flat = at - self.foot - self.frame.y * (at - self.foot).dot(self.frame.y);
        let share = (flat.length() / self.rim(frame_angle(&self.frame, flat)).max(1e-9)).min(1.2);
        at + self.frame.y * (0.75 * self.thick * Self::depth(share))
    }

    /// Its soil in `woods`: its former underside lumpy where clods fell from
    /// it, and its former surface toward the trunk, thinning out to the rim.
    fn soil(&self, timber: &mut Timber, woods: Woods) -> Option<()> {
        let rings = PLATE_RINGS;
        let mut points = Vec::new();
        points
            .try_reserve_exact(2 * (rings * PLATE_SPOKES) as usize)
            .ok()?;
        for (lumps, out) in [(0.12, 0.75), (0.06, -0.25)] {
            for ring in 0..rings {
                let along = f64::from(ring) / f64::from(rings - 1);
                let share = along * mathf::sqrt(along);
                for spoke in 0..PLATE_SPOKES {
                    let angle = TAU * f64::from(spoke) / f64::from(PLATE_SPOKES);
                    let flat = self.way(angle) * (share * self.rim(angle));
                    let rough = lumps * self.thick * self.lump(self.foot + flat);
                    let off = out * self.thick * Self::depth(share) + rough;
                    points.push(self.foot + flat + self.frame.y * off);
                }
            }
        }
        let at = |face: u32, ring: u32, spoke: u32| {
            (face * rings + ring) * PLATE_SPOKES + spoke % PLATE_SPOKES
        };
        let mut faces = Vec::new();
        faces
            .try_reserve_exact(4 * (rings * PLATE_SPOKES) as usize)
            .ok()?;
        for ring in 0..rings - 1 {
            for spoke in 0..PLATE_SPOKES {
                let (a, b) = (at(0, ring, spoke), at(0, ring, spoke + 1));
                let (c, d) = (at(0, ring + 1, spoke), at(0, ring + 1, spoke + 1));
                faces.push(([a, d, c], woods.soil));
                faces.push(([a, b, d], woods.soil));
                let (a, b) = (at(1, ring, spoke), at(1, ring, spoke + 1));
                let (c, d) = (at(1, ring + 1, spoke), at(1, ring + 1, spoke + 1));
                faces.push(([a, c, d], woods.soil));
                faces.push(([a, d, b], woods.soil));
            }
        }
        let edge = rings - 1;
        for spoke in 0..PLATE_SPOKES {
            let (a, b) = (at(0, edge, spoke), at(0, edge, spoke + 1));
            let (c, d) = (at(1, edge, spoke), at(1, edge, spoke + 1));
            faces.push(([a, d, c], woods.soil));
            faces.push(([a, b, d], woods.soil));
        }
        timber.assembly.mesh(&points, &faces)
    }

    /// The roots that ran out through it from the trunk's lobes, along its
    /// underside half bared and broken off past its rim, in `woods`, `age` as
    /// long as it has lain; more, thinner, branching from them on the way.
    fn laterals(
        &self,
        timber: &mut Timber,
        (woods, age): (Woods, f64),
        travelled: &mut f64,
    ) -> Option<()> {
        for &(angle, girth) in self.roots.iter().take(self.count) {
            let out = self.tube.way(angle);
            let start = self.foot + out * (self.radius * 0.8);
            let wander = timber.range(-0.25, 0.25);
            let across = self.axis.cross(out);
            let length = self.rim(frame_angle(&self.frame, out)) - 0.8 * self.radius
                + self.radius * timber.range(0.1, 0.5);
            let run = |t: f64| {
                self.underside(start + out * (length * t) + across * (wander * length * t * t))
            };
            torn_root(timber, &run, girth, (woods, age), travelled)?;
            for _ in 0..timber.count((1, 4)) {
                let from = timber.range(0.25, 0.75);
                let fork = start + out * (from * length) + across * (wander * length * from * from);
                let turn = timber.range(-0.9, 0.9);
                let branch = (out * mathf::cos(turn) + across * mathf::sin(turn)).normalized();
                let reach = self.rim(frame_angle(&self.frame, branch)) * timber.range(0.95, 1.15)
                    - (fork - self.foot).length();
                if reach > 0.1 * self.radius {
                    let thin = girth * (1.0 - 0.6 * from) * timber.range(0.3, 0.5);
                    let run = |t: f64| self.underside(fork + branch * (reach * t));
                    torn_root(timber, &run, thin, (woods, age), travelled)?;
                }
            }
        }
        Some(())
    }

    /// The roots that sank deep and the many it held, broken off short of
    /// where the plate tore, every way, in `woods`, `age` as long as it has
    /// lain.
    fn sinkers(
        &self,
        timber: &mut Timber,
        (woods, age): (Woods, f64),
        travelled: &mut f64,
    ) -> Option<()> {
        for _ in 0..timber.count((18, 32)) {
            let angle = TAU * timber.unit();
            let share = timber.range(0.15, 0.7);
            let at_face = self.face((share, angle));
            let down = (self.frame.y
                + self.way(angle) * timber.range(-0.5, 0.8)
                + Vec3::UP * timber.range(-0.4, 0.3))
            .normalized();
            let girth = self.radius * timber.range(0.025, 0.12) * (1.0 - 0.5 * share);
            let reach = girth * timber.range(2.0, 6.0);
            let (from, end) = (
                at_face - down * (0.5 * reach),
                at_face + down * (0.5 * reach),
            );
            timber.tube(
                (from, end),
                (girth, 0.8 * girth),
                ((*travelled, *travelled + reach), woods.bark),
                [false, true],
            )?;
            *travelled += reach;
            timber.snap((end, down, 0.8 * girth), (woods, age))?;
        }
        Some(())
    }

    /// Its fine roots in `woods`: a mat bristling from its underside, torn
    /// short, and more hanging from its rim.
    fn fine(&self, timber: &mut Timber, woods: Woods) -> Option<()> {
        for _ in 0..timber.count((30, 60)) {
            let angle = TAU * timber.unit();
            let at_face = self.face((mathf::sqrt(timber.unit()), angle));
            let out = (self.frame.y
                + self.way(angle) * timber.range(-0.6, 0.9)
                + Vec3::UP * timber.range(-0.4, 0.2))
            .normalized();
            let girth = timber.range(0.002, 0.008);
            let reach = timber.range(0.03, 0.15);
            let from = at_face - out * (0.3 * reach);
            timber.tube(
                (from, from + out * reach),
                (girth, 0.4 * girth),
                ((0.0, reach), woods.bark),
                [false; 2],
            )?;
        }
        for _ in 0..timber.count((6, 14)) {
            let angle = TAU * timber.unit();
            let from = self.foot
                + self.way(angle) * (0.97 * self.rim(angle))
                + self.frame.y * timber.range(-0.1, 0.4) * self.thick;
            let hang = timber.range(0.15, 0.6) * self.reach;
            let drift = self.way(angle) * (0.15 * hang);
            let girth = timber.range(0.003, 0.009);
            let middle = from + drift - Vec3::UP * (0.5 * hang);
            let end = middle + drift * 0.5 - Vec3::UP * (0.5 * hang);
            timber.tube(
                (from, middle),
                (girth, 0.7 * girth),
                ((0.0, 0.5 * hang), woods.bark),
                [false; 2],
            )?;
            timber.tube(
                (middle, end),
                (0.7 * girth, 0.25 * girth),
                ((0.5 * hang, hang), woods.bark),
                [false; 2],
            )?;
        }
        Some(())
    }
}

/// The angle round `frame`'s `y`, from its `x` toward its `z`, that `way`
/// points along.
fn frame_angle(frame: &Frame, way: Vec3) -> f64 {
    mathf::atan2(way.dot(frame.z), way.dot(frame.x))
}

/// A root torn from the ground with its plate, running along `path` from
/// its start at nought to its end at one, `thick` where it starts and
/// narrowing, and broken off at its end; in `woods`, as long as `age` has
/// let it lie, its bark `travelled` metres along where it starts.
fn torn_root(
    timber: &mut Timber,
    path: &dyn Fn(f64) -> Vec3,
    thick: f64,
    (woods, age): (Woods, f64),
    travelled: &mut f64,
) -> Option<()> {
    const SEGMENTS: u32 = 3;
    let girth = |t: f64| thick * (1.0 - 0.55 * t);
    for segment in 0..SEGMENTS {
        let (t0, t1) = (
            f64::from(segment) / f64::from(SEGMENTS),
            f64::from(segment + 1) / f64::from(SEGMENTS),
        );
        let (a, b) = (path(t0), path(t1));
        let step = (b - a).length();
        let last = segment + 1 == SEGMENTS;
        timber.tube(
            (a, b),
            (girth(t0), girth(t1)),
            ((*travelled, *travelled + step), woods.bark),
            [false, last],
        )?;
        *travelled += step;
    }
    let (end, before) = (path(1.0), path(1.0 - 1.0 / f64::from(SEGMENTS)));
    timber.snap((end, (end - before).normalized(), girth(1.0)), (woods, age))
}

/// How a stump's top was left: snapped off and torn, or sawn.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Top {
    Snapped,
    Sawn,
}

/// The rings a sawn face is cut in, from its middle out, the last band its
/// bark cut through; those from the step of a felled one's notch out; and
/// the spokes, before its checks and its step add their own.
const FACE_RINGS: [f64; 8] = [0.0, 0.12, 0.3, 0.5, 0.7, 0.84, 0.9, 1.0];
const NOTCH_RINGS: [f64; 5] = [0.0, 0.4, 0.75, 0.9, 1.0];
const FACE_SPOKES: u32 = 32;

/// The most checks a sawn face dries into.
const MOST_CHECKS: usize = 7;

/// A stump `height` tall and `radius` across, its foot flaring into the
/// roots it stands on, its top as `top` left it, in `woods`, decayed as
/// `decay` has it, and sending up shoots as `sprouting` has them, if it
/// does; grown from `seed`, its hierarchy still to build. `None` when the
/// heap will not hold it.
pub(crate) fn stump(
    height: f64,
    radius: f64,
    (top, woods): (Top, Woods),
    (decay, sprouting): (Decay, Option<Sprouting>),
    seed: u64,
) -> Option<Building> {
    let mut timber = Timber::new(seed, (256, 8192))?;
    let lean = Frame::turned(TAU * timber.unit(), 0.06 * timber.unit());
    let ground = 0.2 * radius;
    let base = Vec3::new(0.0, -ground, 0.0);
    let rise = height + ground;
    let crown = base + lean.y * rise;
    let bole = Tube::new(
        (base, crown),
        ((radius, 0.92 * radius), (0.0, rise)),
        (woods.bark, timber.next_key()),
        lean.x,
    )
    .opened([false, true]);
    let flare = timber.range(FLARE.0, FLARE.1);
    let roots = timber.count(ROOTS);
    // The flare carries on past a low cut, which shows its lobes.
    let foot = Foot::new(
        &bole,
        (radius, ground, FLARE_TOP * radius),
        (flare, roots),
        &mut timber.dice,
    )?;
    let index = timber.assembly.flare(foot.flare())?;
    timber.assembly.push(Part::Tube(bole.flared(index)))?;
    let key = timber.next_key();
    foot.roots((woods.bark, key), 0.0, &mut |part| {
        timber.assembly.push(part)
    })?;
    let flared = foot.flare();
    let girth = |up: f64, way: Vec3| {
        bole.round_radius(up + ground) * flared.factor(up + ground, bole.angle_of(way))
    };
    let way = |angle: f64| lean.x * mathf::cos(angle) + lean.z * mathf::sin(angle);
    let outline = |angle: f64| girth(height, way(angle));
    match top {
        Top::Snapped => {
            let tension = TAU * timber.unit();
            timber.torn(
                &Break {
                    centre: crown,
                    frame: lean,
                    outline: &outline,
                    tension,
                    age: decay.age,
                    barked: true,
                },
                woods,
            )?;
        }
        Top::Sawn => sawn(&mut timber, (crown, lean, &outline), (woods, decay.age))?,
    }
    if let Some(fungus) = decay.fungus {
        let mut hosts = [(Vec3::ZERO, Vec3::UP); 2];
        let count = timber.count((1, 2)) as usize;
        for host in hosts.iter_mut().take(count) {
            let around = TAU * timber.unit();
            let out = Vec3::new(mathf::cos(around), 0.0, mathf::sin(around));
            let up = height * timber.range(0.2, 0.75);
            *host = (lean.y * up + out * girth(up, out), out);
        }
        timber.brackets(hosts.get(..count)?, fungus)?;
    }
    if let Some(sprouting) = sprouting {
        timber.shoots((lean, height, &girth), sprouting)?;
    }
    timber.assembly.finish()
}

/// How a felled stump's notch was cut: the way round its face the tree fell,
/// how far either side of that way the notch spans, how far from the middle
/// its edge runs square to the fall, and how far below the back cut it lies.
#[derive(Copy, Clone, Debug)]
struct Notch {
    fall: f64,
    span: f64,
    hinge: f64,
    step: f64,
}

impl Notch {
    /// How far the back cut reaches along `angle` of a face `outline` across:
    /// to the rim, or to the notch's edge where the notch lies.
    fn back(&self, angle: f64, outline: &dyn Fn(f64) -> f64) -> f64 {
        if wrapped(angle - self.fall).abs() < self.span - 1e-9 {
            (self.hinge / mathf::cos(wrapped(angle - self.fall))).min(outline(angle))
        } else {
            outline(angle)
        }
    }
}

/// The face a stump in `frame` was sawn to at `centre`, `outline` across at
/// each angle round it, in `woods`, as `age` has left it: if it was felled,
/// the back cut and the notch below it; its bark cut through in a dark ring
/// about its rim; the checks it dried into, running in from its rim, more and
/// wider the older it is; and its heart rotting into a hollow.
fn sawn(
    timber: &mut Timber,
    (centre, frame, outline): (Vec3, Frame, &dyn Fn(f64) -> f64),
    (woods, age): (Woods, f64),
) -> Option<()> {
    let felled = timber.unit() < 0.6;
    let fall = TAU * timber.unit();
    let broadest = (0..16u32)
        .map(|step| outline(TAU * f64::from(step) / 16.0))
        .fold(0.0, f64::max);
    let step = broadest * timber.range(0.12, 0.25);
    let hinge = broadest * timber.range(0.04, 0.08);
    let notch = felled.then(|| Notch {
        fall,
        span: mathf::acos((hinge / outline(fall)).clamp(0.0, 1.0)),
        hinge,
        step,
    });
    let count = usize::try_from(mathf::round_i32(
        1.0 + 6.0 * age + 1.5 * (timber.unit() - 0.5),
    ))
    .unwrap_or(0)
    .min(MOST_CHECKS);
    let mut cracks = [(0.0f64, 0.0f64, 0.0f64, 0.0f64); MOST_CHECKS];
    for crack in cracks.iter_mut().take(count) {
        *crack = (
            TAU * timber.unit(),
            timber.range(0.15, 0.45),
            broadest * timber.range(0.004, 0.012) * (1.0 + age),
            broadest * timber.range(0.06, 0.2) * (0.5 + age),
        );
    }
    let cracks = cracks.get(..count)?;
    let rotten = past(age, HOLLOWS);
    let rot_reach = if rotten > 0.0 {
        0.25 + 0.5 * rotten
    } else {
        0.0
    };
    let face = Face {
        centre,
        frame,
        outline,
        cracks,
        rot: (rot_reach, broadest * 0.6 * rotten),
        salt: mix32(timber.key ^ 0x7a),
    };
    // The spokes: evenly round, either edge and the floor of each check, and
    // where the notch's edge meets the rim.
    let mut angles = Vec::new();
    angles
        .try_reserve_exact(FACE_SPOKES as usize + 3 * count + 2)
        .ok()?;
    for spoke in 0..FACE_SPOKES {
        angles.push(wrapped(TAU * f64::from(spoke) / f64::from(FACE_SPOKES)));
    }
    for &(angle, _, width, _) in cracks {
        let half = 0.5 * width / outline(angle);
        angles.extend([angle - half, angle, angle + half].map(wrapped));
    }
    if let Some(notch) = notch {
        angles.extend([
            wrapped(notch.fall - notch.span),
            wrapped(notch.fall + notch.span),
        ]);
    }
    angles.sort_unstable_by(f64::total_cmp);
    angles.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    let back =
        |angle: f64| notch.map_or_else(|| outline(angle), |notch| notch.back(angle, outline));
    let surface = face.polar((&angles, &FACE_RINGS), (&|_| 0.0, &back), (0.0, woods))?;
    timber.assembly.mesh(&surface.points, &surface.faces)?;
    match notch {
        Some(notch) => felling(timber, &face, (&angles, notch), (woods, age)),
        None => Some(()),
    }
}

/// The notch `notch` cut below `face`, along those of `angles` it spans, in
/// `woods`, as `age` has left it: its own face, the riser up from it to the
/// back cut along its edge, and the hinge torn across along the riser's top
/// where the tree fell, until the hinge rots away with the heart.
fn felling(
    timber: &mut Timber,
    face: &Face<'_>,
    (angles, notch): (&[f64], Notch),
    (woods, age): (Woods, f64),
) -> Option<()> {
    let Notch {
        fall,
        span,
        hinge,
        step,
    } = notch;
    let mut spanned = Vec::new();
    spanned.try_reserve_exact(angles.len()).ok()?;
    let mut from = angles
        .iter()
        .position(|&angle| (wrapped(angle - (fall - span))).abs() < 1e-9)?;
    loop {
        let angle = *angles.get(from)?;
        spanned.push(angle);
        if (wrapped(angle - (fall + span))).abs() < 1e-9 {
            break;
        }
        from = (from + 1) % angles.len();
        if spanned.len() > angles.len() {
            return None;
        }
    }
    let back = |angle: f64| notch.back(angle, face.outline);
    let notched = face.polar_open(
        (&spanned, &NOTCH_RINGS),
        (&back, face.outline),
        (-step, woods),
    )?;
    timber.assembly.mesh(&notched.points, &notched.faces)?;
    let mut riser = Vec::new();
    riser.try_reserve_exact(2 * spanned.len()).ok()?;
    for &angle in &spanned {
        riser.push(face.point(back(angle), angle, 0.0));
        riser.push(face.point(back(angle), angle, -step));
    }
    let mut faces = Vec::new();
    faces.try_reserve_exact(2 * spanned.len()).ok()?;
    for index in 0..u32::try_from(spanned.len().saturating_sub(1)).ok()? {
        let (top, low, next_top, next_low) =
            (2 * index, 2 * index + 1, 2 * index + 2, 2 * index + 3);
        faces.push(([top, next_top, next_low], woods.wood));
        faces.push(([top, next_low, low], woods.wood));
    }
    timber.assembly.mesh(&riser, &faces)?;
    if age >= HOLLOWS {
        return Some(());
    }
    // A strip the width of the face across the fall and the hinge's breadth
    // along it, its laths leaning the way the tree went.
    let frame = face.frame;
    let broadest = (0..16u32)
        .map(|step| (face.outline)(TAU * f64::from(step) / 16.0))
        .fold(0.0, f64::max);
    let toward = frame.x * mathf::cos(fall) + frame.z * mathf::sin(fall);
    let across = frame.y.cross(toward).normalized();
    let half = mathf::sqrt((broadest * broadest - hinge * hinge).max(0.0));
    let laid = running(frame.y);
    let hinged = |angle: f64| {
        let way = laid.x * mathf::cos(angle) + laid.z * mathf::sin(angle);
        let (wide, narrow) = (way.dot(across).abs(), way.dot(toward).abs());
        1.0 / mathf::sqrt(
            (wide * wide / (half * half) + narrow * narrow / (hinge * hinge)).max(1e-12),
        )
    };
    timber.torn(
        &Break {
            centre: face.centre + toward * hinge * 0.5 - frame.y * (0.5 * step),
            frame: laid,
            outline: &hinged,
            tension: frame_angle(&laid, -toward),
            age,
            barked: false,
        },
        woods,
    )
}

/// A sawn face as it stands: where its middle lies and how it is turned, how
/// broad at each angle round it, the checks it dried into — each the angle
/// it runs in at, how far in from the rim it starts as a share of the face's
/// breadth there, how wide it opens at the rim and how deep it cuts there —
/// how far out its heart has rotted and how deep, and the salt its rot's
/// roughness is drawn under.
struct Face<'a> {
    centre: Vec3,
    frame: Frame,
    outline: &'a dyn Fn(f64) -> f64,
    cracks: &'a [(f64, f64, f64, f64)],
    rot: (f64, f64),
    salt: u32,
}

impl Face<'_> {
    /// The way out from the middle along `angle`.
    fn way(&self, angle: f64) -> Vec3 {
        self.frame.x * mathf::cos(angle) + self.frame.z * mathf::sin(angle)
    }

    /// The face's point `reach` out along `angle`, its cut at `level`, sunk
    /// by its checks and its rot.
    fn point(&self, reach: f64, angle: f64, level: f64) -> Vec3 {
        let ring = reach / (self.outline)(angle).max(1e-9);
        let cracked = self
            .cracks
            .iter()
            .map(|&(middle, from, width, deep)| {
                if ring < from {
                    return 0.0;
                }
                let opened = (ring - from) / (1.0 - from);
                let off = wrapped(angle - middle).abs() * reach;
                let half = 0.5 * width * opened;
                if off >= half.max(1e-9) {
                    0.0
                } else {
                    deep * opened * (1.0 - off / half)
                }
            })
            .fold(0.0, f64::max);
        let (reach_rot, depth_rot) = self.rot;
        let rot = if ring < reach_rot {
            let inner = ring / reach_rot;
            let rough = 0.8
                + 0.4
                    * noise3(
                        self.way(angle) * (3.0 * ring) + Vec3::splat(ring),
                        self.salt,
                    );
            depth_rot * (1.0 - inner * inner) * rough
        } else {
            0.0
        };
        self.centre + self.way(angle) * reach + self.frame.y * (level - cracked - rot)
    }

    /// The face between `inner` and `outer` reaches along each of `angles`,
    /// round the whole of it, in `rings` from the one to the other, cut at
    /// `level` in `woods`: its points, and its faces in sound or rotten wood
    /// or, in its outermost band, the bark it cut through.
    fn polar(
        &self,
        (angles, rings): (&[f64], &[f64]),
        (inner, outer): (&dyn Fn(f64) -> f64, &dyn Fn(f64) -> f64),
        (level, woods): (f64, Woods),
    ) -> Option<Mesh> {
        self.grid((angles, rings), (inner, outer), (level, woods), true)
    }

    /// [`Self::polar`] over `angles` from the first to the last only.
    fn polar_open(
        &self,
        (angles, rings): (&[f64], &[f64]),
        (inner, outer): (&dyn Fn(f64) -> f64, &dyn Fn(f64) -> f64),
        (level, woods): (f64, Woods),
    ) -> Option<Mesh> {
        self.grid((angles, rings), (inner, outer), (level, woods), false)
    }

    fn grid(
        &self,
        (angles, rings): (&[f64], &[f64]),
        (inner, outer): (&dyn Fn(f64) -> f64, &dyn Fn(f64) -> f64),
        (level, woods): (f64, Woods),
        round: bool,
    ) -> Option<Mesh> {
        let spokes = u32::try_from(angles.len()).ok()?;
        let mut points = Vec::new();
        points.try_reserve_exact(rings.len() * angles.len()).ok()?;
        let mut shares = Vec::new();
        shares.try_reserve_exact(rings.len() * angles.len()).ok()?;
        for &ring in rings {
            for &angle in angles {
                let (from, to) = (inner(angle), outer(angle));
                let reach = from + (to - from) * ring;
                points.push(self.point(reach, angle, level));
                shares.push(reach / (self.outline)(angle).max(1e-9));
            }
        }
        let at = |ring: u32, spoke: u32| ring * spokes + spoke % spokes;
        let columns = if round {
            spokes
        } else {
            spokes.saturating_sub(1)
        };
        let mut faces = Vec::new();
        faces
            .try_reserve_exact(2 * rings.len() * angles.len())
            .ok()?;
        let share_of = |index: u32| shares.get(index as usize).copied().unwrap_or(0.0);
        let bands = u32::try_from(rings.len().saturating_sub(1)).ok()?;
        for ring in 0..bands {
            for spoke in 0..columns {
                let (a, b) = (at(ring, spoke), at(ring, spoke + 1));
                let (c, d) = (at(ring + 1, spoke), at(ring + 1, spoke + 1));
                let rotten = [a, b, c, d]
                    .into_iter()
                    .all(|corner| share_of(corner) <= self.rot.0);
                let barked = ring + 1 == bands && share_of(c).min(share_of(d)) > 0.95;
                let material = if barked {
                    woods.edge
                } else if rotten {
                    woods.rot
                } else {
                    woods.wood
                };
                faces.push(([a, b, c], material));
                faces.push(([b, d, c], material));
            }
        }
        Some(Mesh { points, faces })
    }
}

#[cfg(test)]
#[path = "deadwood_tests.rs"]
mod tests;
