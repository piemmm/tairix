//! Wood torn across its grain: the break a trunk, a limb or a root leaves
//! where it snapped. Bent until it gave, the wood failed in tension on the
//! side it was bent away from, its fibres pulling out there in long laths,
//! and crushed on the other, so the break climbs across the wood to a jagged
//! crest of slabs split along the grain, standing from a face itself split
//! into fibres. Its bark tears back from the rim in a dark edge and hangs in
//! tatters. A break that has lain long has lost its finer splinters, greyed,
//! and its heart has rotted hollow.

use core::f64::consts::{PI, TAU};

use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::mathf;

use crate::noise::{noise3, smoothstep};
use crate::prototype::Mesh;
use crate::vector::{real, wrapped, Frame, Vec3};

/// The materials a break is made in: the torn wood, the rotten wood of a
/// hollow heart, and the bark's own torn edge.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Grain {
    pub(crate) wood: u16,
    pub(crate) rot: u16,
    pub(crate) edge: u16,
}

/// A break to tear: where its middle lies; the frame whose `y` the wood runs
/// out of it along; its radius at each angle round it, from the frame's `x`
/// toward its `z`; the way round it the wood was bent away from, where its
/// fibres pulled out; how long it has lain, from fresh at nought to all but
/// rotted away at one; and whether bark wraps it, as it does all but wood
/// torn within a cut.
pub(crate) struct Break<'a> {
    pub(crate) centre: Vec3,
    pub(crate) frame: Frame,
    pub(crate) outline: &'a dyn Fn(f64) -> f64,
    pub(crate) tension: f64,
    pub(crate) age: f64,
    pub(crate) barked: bool,
}

/// The rings a break's face is torn in, as shares of its radius, the last
/// band the bark's thickness.
const RINGS: [f64; 10] = [0.0, 0.14, 0.28, 0.42, 0.56, 0.7, 0.82, 0.92, 0.97, 1.0];

/// How far apart round its rim a break's spokes stand at most, in metres,
/// and how few and how many it has.
const SPOKE_SPACING: f64 = 0.012;
const SPOKES: (u32, u32) = (10, 56);

/// How far apart a break's fibres stand, in metres.
const FIBRE: f64 = 0.011;

/// How long a break has lain before its heart rots hollow, as its age.
pub(crate) const HOLLOWS: f64 = 0.55;

/// A right-handed frame whose `y` runs along the unit `way`: a break's, the
/// wood running out of it.
pub(crate) fn running(way: Vec3) -> Frame {
    let frame = Frame::around(way);
    Frame {
        x: frame.y,
        y: frame.z,
        z: frame.x,
    }
}

/// How far past `from` `age` stands, as a share of what is left of the way
/// to one: nought before it, one at its end.
pub(crate) fn past(age: f64, from: f64) -> f64 {
    ((age - from) / (1.0 - from).max(1e-9)).clamp(0.0, 1.0)
}

fn range(dice: &mut NonCryptoRng, (low, high): (f64, f64)) -> f64 {
    low + (high - low) * dice.next_f64()
}

impl Mesh {
    /// The index the next point pushed will take.
    fn next(&self) -> Option<u32> {
        u32::try_from(self.points.len()).ok()
    }

    /// The flat quad of `corners`, wound about its outward face, in
    /// `material`: its own four points, so its edges stay sharp.
    fn quad(&mut self, corners: [Vec3; 4], material: u16) -> Option<()> {
        let first = self.next()?;
        self.points.try_reserve(4).ok()?;
        self.faces.try_reserve(2).ok()?;
        self.points.extend(corners);
        self.faces.push(([first, first + 1, first + 2], material));
        self.faces.push(([first, first + 2, first + 3], material));
        Some(())
    }

    /// The flat triangle of `corners`, wound about its outward face, in
    /// `material`.
    fn triangle(&mut self, corners: [Vec3; 3], material: u16) -> Option<()> {
        let first = self.next()?;
        self.points.try_reserve(3).ok()?;
        self.faces.try_reserve(1).ok()?;
        self.points.extend(corners);
        self.faces.push(([first, first + 1, first + 2], material));
        Some(())
    }
}

/// The break where a limb running along the unit `way` snapped off at `end`,
/// `radius` thick there, in `grain`, `age` as long as it has lain: round,
/// barked, pulled out any way, drawn from `dice`; `None` when the heap will
/// not hold it.
pub(crate) fn snapped(
    (end, way, radius): (Vec3, Vec3, f64),
    (grain, age): (Grain, f64),
    dice: &mut NonCryptoRng,
) -> Option<Mesh> {
    let outline = |_: f64| radius;
    let brk = Break {
        centre: end,
        frame: running(way),
        outline: &outline,
        tension: TAU * dice.next_f64(),
        age,
        barked: true,
    };
    tear(&brk, grain, dice)
}

/// Tear `brk` in `grain`, drawn from `dice`; `None` when the heap will not
/// hold it.
pub(crate) fn tear(brk: &Break<'_>, grain: Grain, dice: &mut NonCryptoRng) -> Option<Mesh> {
    let (narrowest, broadest) = extent(brk);
    let spokes = u32::try_from(mathf::round_i32(TAU * broadest / SPOKE_SPACING).max(0))
        .unwrap_or(0)
        .clamp(SPOKES.0, SPOKES.1);
    let rotten = past(brk.age, HOLLOWS);
    // However broad a break, it is torn as deep as its wood was bent across:
    // a rail broken under its load splinters along its depth.
    let bent = f64::midpoint((brk.outline)(brk.tension), (brk.outline)(brk.tension + PI));
    let face = Face {
        brk,
        // Fresh, the tension side stands far above the crushed side; long
        // weathered, its crest has broken down.
        climb: bent * range(dice, (0.3, 1.0)) * (1.0 - 0.5 * brk.age),
        fibres: bent * range(dice, (0.12, 0.3)) * (1.0 - 0.6 * brk.age),
        hollow: (
            (0.3 + 0.45 * rotten) * f64::from(u8::from(rotten > 0.0)),
            narrowest * range(dice, (0.4, 1.2)) * rotten,
        ),
        salt: dice.next_u32(),
        size: narrowest,
        bent,
    };
    let mut torn = Mesh::default();
    let rings = u32::try_from(RINGS.len()).ok()?;
    torn.points
        .try_reserve(RINGS.len() * spokes as usize + 2 * spokes as usize)
        .ok()?;
    torn.faces
        .try_reserve(2 * RINGS.len() * spokes as usize)
        .ok()?;
    // The face: one point at its middle, then ring by ring out to its rim.
    for (ring, &share) in RINGS.iter().enumerate() {
        let count = if ring == 0 { 1 } else { spokes };
        for spoke in 0..count {
            let angle = TAU * f64::from(spoke) / f64::from(spokes);
            torn.points.push(face.point(share, angle));
        }
    }
    let at = |ring: u32, spoke: u32| {
        if ring == 0 {
            0
        } else {
            1 + (ring - 1) * spokes + spoke % spokes
        }
    };
    let edge = if brk.barked { grain.edge } else { grain.wood };
    for ring in 0..rings - 1 {
        let inner = RINGS.get(ring as usize).copied().unwrap_or(0.0);
        let material = if ring + 2 >= rings {
            edge
        } else if inner < face.hollow.0 {
            grain.rot
        } else {
            grain.wood
        };
        for spoke in 0..spokes {
            let (a, b) = (at(ring, spoke), at(ring, spoke + 1));
            let (c, d) = (at(ring + 1, spoke), at(ring + 1, spoke + 1));
            if ring == 0 {
                torn.faces.push(([a, d, c], material));
            } else {
                torn.faces.push(([a, d, c], material));
                torn.faces.push(([a, b, d], material));
            }
        }
    }
    // The bark's torn edge, from where the limb below ends up to the face's
    // rim.
    let base = torn.next()?;
    for spoke in 0..spokes {
        let angle = TAU * f64::from(spoke) / f64::from(spokes);
        torn.points.push(face.rim(angle, 0.0));
    }
    for spoke in 0..spokes {
        let (low, next_low) = (base + spoke, base + (spoke + 1) % spokes);
        let (high, next_high) = (at(rings - 1, spoke), at(rings - 1, spoke + 1));
        torn.faces.push(([low, high, next_high], edge));
        torn.faces.push(([low, next_high, next_low], edge));
    }
    let laths = mathf::round_i32((broadest / 0.018).clamp(2.0, 22.0) * (1.0 - 0.75 * rotten));
    for _ in 0..laths {
        lath(&mut torn, &face, (grain.wood, edge), dice)?;
    }
    let tatters = if brk.barked { dice.next_u32() % 3 } else { 0 };
    for _ in 0..tatters {
        tatter(&mut torn, &face, grain.edge, dice)?;
    }
    Some(torn)
}

/// The face a break tears across: how far its tension side climbs above its
/// crushed side, how far its fibres stand proud of one another, how far out
/// its heart has rotted as a share of its radius and how deep, the salt its
/// fibres are drawn under, how thick its wood is across its narrowest, and
/// how far it reaches from its middle the way it was bent.
struct Face<'a> {
    brk: &'a Break<'a>,
    climb: f64,
    fibres: f64,
    hollow: (f64, f64),
    salt: u32,
    size: f64,
    bent: f64,
}

/// How far across `brk` is at its narrowest and at its broadest.
fn extent(brk: &Break<'_>) -> (f64, f64) {
    let (least, most) = (0..16u32)
        .map(|step| (brk.outline)(TAU * f64::from(step) / 16.0))
        .fold((f64::INFINITY, 0.0f64), |(least, most), reach| {
            (least.min(reach), most.max(reach))
        });
    (least.max(1e-4), most.max(1e-4))
}

impl Face<'_> {
    /// The way out from the break's middle at `angle`.
    fn way(&self, angle: f64) -> Vec3 {
        self.brk.frame.x * mathf::cos(angle) + self.brk.frame.z * mathf::sin(angle)
    }

    /// The face's height `share` of the way out to its rim at `angle`.
    fn height(&self, share: f64, angle: f64) -> f64 {
        let toward = mathf::cos(wrapped(angle - self.brk.tension));
        // Climbing toward where the fibres pulled out, the crushed side low.
        let climb = self.climb * smoothstep(-0.7, 1.0, toward * share.max(0.25));
        let reach = share * (self.brk.outline)(angle);
        let place = self.way(angle) * reach;
        let fibre = noise3(
            Vec3::new(place.x / FIBRE, place.z / FIBRE, place.y / FIBRE),
            self.salt,
        );
        // The pulled-out side is the more ragged.
        let ragged = self.fibres * (0.5 + 0.5 * smoothstep(-0.5, 1.0, toward)) * (0.5 + fibre);
        let (rot_reach, rot_depth) = self.hollow;
        let hollowed = if share < rot_reach {
            let inner = share / rot_reach;
            rot_depth * (1.0 - inner * inner)
        } else {
            0.0
        };
        // The bark tears about level, a little ragged; the wood within climbs
        // from it.
        let rim = smoothstep(0.8, 0.97, share);
        let fringe = 0.06 * self.size * (0.5 + fibre);
        (climb + ragged).max(0.0) * (1.0 - rim) + fringe * rim - hollowed
    }

    /// The face's point `share` of the way out at `angle`.
    fn point(&self, share: f64, angle: f64) -> Vec3 {
        let reach = share * (self.brk.outline)(angle);
        self.brk.centre + self.way(angle) * reach + self.brk.frame.y * self.height(share, angle)
    }

    /// The point on the rim at `angle`, `up` above where the limb below
    /// ends.
    fn rim(&self, angle: f64, up: f64) -> Vec3 {
        self.brk.centre + self.way(angle) * (self.brk.outline)(angle) + self.brk.frame.y * up
    }
}

/// A lath of `wood` pulled out of `face`'s wood: a slab split along the
/// grain, standing from the face where its fibres pulled out, leaning out a
/// little, narrowing and splitting at its tip; one torn from the rim keeps
/// the bark's `edge` on its outer face.
fn lath(
    torn: &mut Mesh,
    face: &Face<'_>,
    (wood, edge): (u16, u16),
    dice: &mut NonCryptoRng,
) -> Option<()> {
    let brk = face.brk;
    let thickness = face.size;
    // Most stand where the wood was pulled, and toward the rim, where it was
    // pulled hardest; fewer round its flanks.
    let off = range(dice, (-1.0, 1.0));
    let angle = brk.tension + 1.6 * off * off.abs();
    let share = 0.5 + 0.48 * mathf::sqrt(dice.next_f64());
    let outer = if share > 0.85 { edge } else { wood };
    let root = face.point(share, angle);
    let out = face.way(angle);
    let round = brk.frame.y.cross(out);
    let lean = range(dice, (0.04, 0.35));
    let twist = range(dice, (-0.15, 0.15));
    let up = (brk.frame.y * mathf::cos(lean) + out * mathf::sin(lean) + round * twist).normalized();
    let length = face.bent * range(dice, (0.25, 1.3)) * (1.0 - 0.6 * brk.age);
    let width = (thickness * range(dice, (0.05, 0.14))).max(0.003);
    let thick = width * range(dice, (0.25, 0.5));
    // Its wide faces lie along the wood's rays, its thin ones round it.
    let across = out - up * out.dot(up);
    let across = if across.length() > 1e-9 {
        across.normalized()
    } else {
        round
    };
    let side = up.cross(across);
    let sunk = root - up * (0.15 * length);
    let section = |at: Vec3, (w, t): (f64, f64)| {
        [
            at + across * w + side * t,
            at - across * w + side * t,
            at - across * w - side * t,
            at + across * w - side * t,
        ]
    };
    let foot = section(sunk, (0.5 * width, 0.5 * thick));
    let bend = range(dice, (-0.1, 0.1));
    let middle_at = sunk + up * (0.6 * length) + across * (bend * length);
    let middle = section(middle_at, (0.38 * width, 0.4 * thick));
    // Split at its tip into two ragged points, one longer.
    let tips = [
        middle_at + up * (0.4 * length) + across * (0.25 * width),
        middle_at + up * (0.4 * length * range(dice, (0.55, 0.9))) - across * (0.25 * width),
    ];
    for side_of in 0..4usize {
        let next = (side_of + 1) % 4;
        let (f0, f1) = (*foot.get(side_of)?, *foot.get(next)?);
        let (m0, m1) = (*middle.get(side_of)?, *middle.get(next)?);
        // The last side faces out along the wood's rays, toward the bark.
        let material = if side_of == 3 { outer } else { wood };
        torn.quad([f0, f1, m1, m0], material)?;
        let tip = |corner: Vec3| {
            let toward = (corner - middle_at).dot(across);
            if toward > 0.0 {
                tips[0]
            } else {
                tips[1]
            }
        };
        let (t0, t1) = (tip(m0), tip(m1));
        if (t0 - t1).length() < 1e-12 {
            torn.triangle([m0, m1, t0], material)?;
        } else {
            torn.quad([m0, m1, t1, t0], material)?;
        }
    }
    Some(())
}

/// A tatter of bark in `material` hanging from `face`'s rim: a strip peeled
/// back from the wood, curling out and down.
fn tatter(torn: &mut Mesh, face: &Face<'_>, material: u16, dice: &mut NonCryptoRng) -> Option<()> {
    let brk = face.brk;
    let angle = range(dice, (0.0, TAU));
    let reach = (brk.outline)(angle);
    let out = face.way(angle);
    let round = brk.frame.y.cross(out);
    let width = reach * range(dice, (0.06, 0.15));
    let length = reach * range(dice, (0.15, 0.4));
    let thick = (0.03 * reach).max(0.002);
    let top = face.rim(angle, 0.5 * face.height(0.98, angle).max(0.0));
    let mut sections = [[Vec3::ZERO; 2]; 4];
    for (step, section) in sections.iter_mut().enumerate() {
        let t = real(step) / 3.0;
        // Peeling away from the wood as it hangs.
        let at = top + out * (0.35 * length * t * t + 0.25 * thick) - brk.frame.y * (length * t);
        let narrow = width * (1.0 - 0.45 * t);
        *section = [at + round * (0.5 * narrow), at - round * (0.5 * narrow)];
    }
    for pair in sections.windows(2) {
        let [[a, b], [c, d]] = [*pair.first()?, *pair.get(1)?];
        torn.quad([a, b, d, c], material)?;
        let inward = -out * thick;
        torn.quad([b + inward, a + inward, c + inward, d + inward], material)?;
    }
    Some(())
}

#[cfg(test)]
#[path = "fracture_tests.rs"]
mod tests;
