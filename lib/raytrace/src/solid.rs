//! Masonry and timber in true form: a stone, a brick, a column's drum, a
//! beam, each met as the solid its dressing and its weathering make it.
//!
//! A [`Solid`] is a part of a prototype, a unit of a structure laid at its
//! real size in its own frame: a block, its two ends leaning as a voussoir's
//! do; a drum, tapering and swelling as a column's shaft, perhaps fluted; or
//! a moulding turned to a profile. Its arrises are worn round, chips struck
//! from them, a split stone's faces wander, its faces pit as they erode, a
//! crack may run into it, and moss and lichen grow on it ([`Cover`]).
//!
//! Every one of these is geometry, met by sphere tracing (Hart, "Sphere
//! Tracing", 1996), and each fades out as it comes to span too few pixels
//! from the eye, so far off a solid is its dressed form; what covers it is
//! still coloured where it grows, read from the same point either way.

use tairix_util::mathf;

use crate::cover::{Cover, Growth, Lodging, Shown, DEEPEST};
use crate::cut::{gradient, Cutting, Seeking};
use crate::material::Material;
use crate::noise::{cells3, noise3, smoothstep, NOISE_SLOPE};
use crate::pigment::Pigment;
use crate::prototype::point;
use crate::sample::{mix32, unit};
use crate::shape::{reciprocal, Aabb, Hit};
use crate::vector::{singles, Frame, Pose, Ray, Vec3};

/// The form a solid is dressed to.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Form {
    /// A box, its two `x` faces leaning so its half length grows by `fan`
    /// hundredths of itself from its middle to its head: a voussoir's.
    Block { fan: i8 },
    /// A drum about its `y` axis, `half.x` in radius at its foot and
    /// narrower by `taper` 255ths at its head, swelling by `swell`
    /// thousandths of its radius halfway up, cut in `flutes` flutes.
    Drum { taper: u8, swell: u8, flutes: u8 },
    /// A moulding turned about its `y` axis from `half.x` in radius at its
    /// foot to `half.z` at its head, along a curve rising `bow` times as
    /// fast at its foot as a straight line would, bulging by `bulge`
    /// hundredths of its half height.
    Turned { bow: u8, bulge: i8 },
    /// A volute: a disc about its `y` axis, `half.x` in radius, its faces
    /// carved in a spiral channel winding `turns` times in from its rim to
    /// the raised eye at its middle.
    Scroll { turns: u8 },
}

/// How a solid has worn.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub(crate) struct Wear {
    /// The radius its arrises are worn to, in metres.
    pub(crate) arris: f64,
    /// How many chips are struck from its arrises.
    pub(crate) chips: u8,
    /// How far a split or rough-dressed stone's faces wander from its form,
    /// in metres.
    pub(crate) lumps: f64,
    /// How deep its faces are pitted as they erode, in metres.
    pub(crate) pits: f64,
    /// How wide a crack running into it gapes at its face, in metres; nought
    /// for none.
    pub(crate) crack: f64,
}

/// A unit of a structure: a stone, a brick, a drum, a timber.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Solid {
    centre: [f32; 3],
    /// Its frame as a unit quaternion, each part in 32 767ths.
    turn: [i16; 4],
    half: [f32; 3],
    material: u16,
    /// The material what covers it is made in; [`BARE`] where nothing does.
    cover: u16,
    key: u32,
    form: Form,
    /// Its arris and its lumps in half millimetres, its chips, its pits in
    /// 40ths of a millimetre, its crack in 50ths.
    worn: [u8; 5],
    /// How readily moss lodges on it, in 255ths: a joint's mortar most, a
    /// dressed face least.
    affinity: u8,
}

/// The cover a bare solid has.
const BARE: u16 = u16::MAX;

/// The most chips a solid's arrises lose.
const MOST_CHIPS: usize = 12;

/// How many pixels a feature must span to show at all, and from how many it
/// shows in full: as a limb's cut bark is faded.
const SPANS: (f64, f64) = (1.5, 3.0);

/// The shortest step a march takes, as a share of the pixel where it stands
/// and in metres at the least; the most steps over its stretch; and how
/// closely a crossing is found, as a share of the shortest step.
const LEAST_STEP: (f64, f64) = (0.75, 4e-4);
const MOST_STEPS: f64 = 192.0;
const FOUND: f64 = 0.01;

/// The most guesses a surface's crossing is narrowed by: as many as halving
/// alone would take to reach its tolerance, so the guesses never cost more.
const MOST_FOUND: u32 = 8;

/// The longest a march's shortest step grows, as a share of a solid's least
/// half extent.
const SMALLEST: f64 = 0.1;

/// How much faster than its own terms' sum a solid's surface may rise along
/// a ray, for the rounding of its arrises and its leaning faces.
const MARGIN: f64 = 1.25;

/// How many lumps, pits and lumps of a crack's jag span a solid's least
/// half extent, a metre, and a metre.
const LUMPS: f64 = 1.1;
const PITS: f64 = 85.0;
const JAGS: f64 = 38.0;

impl Solid {
    /// A solid of `form` `half` its size each way from its middle, at
    /// `pose`, worn as `wear` has it, made in `material` and covered in
    /// `cover`, keyed `key`.
    pub(crate) fn new(
        (pose, half): (Pose, Vec3),
        (form, wear): (Form, &Wear),
        (material, cover, affinity): (u16, Option<u16>, f64),
        key: u32,
    ) -> Self {
        let quantised = |value: f64, unit: f64| {
            u8::try_from(mathf::round_i32((value / unit).clamp(0.0, 255.0))).unwrap_or(u8::MAX)
        };
        Self {
            centre: singles(pose.at),
            turn: turn_of(pose.frame),
            half: singles(half.max(Vec3::splat(1e-4))),
            material,
            cover: cover.unwrap_or(BARE),
            key,
            form,
            worn: [
                quantised(wear.arris, 5e-4),
                u8::try_from(usize::from(wear.chips).min(MOST_CHIPS)).unwrap_or(0),
                quantised(wear.lumps, 5e-4),
                quantised(wear.pits, 2.5e-5),
                quantised(wear.crack, 2e-5),
            ],
            affinity: quantised(affinity, 1.0 / 255.0),
        }
    }

    /// Its middle, in its prototype's frame.
    pub(crate) fn centre(&self) -> Vec3 {
        point(self.centre)
    }

    /// Its frame, in its prototype's.
    pub(crate) fn frame(&self) -> Frame {
        frame_of(self.turn)
    }

    /// How far it reaches each way from its middle, in its own frame, before
    /// it wears.
    pub(crate) fn half(&self) -> Vec3 {
        point(self.half)
    }

    /// The material it is made in.
    #[cfg(test)]
    pub(crate) const fn material(&self) -> u16 {
        self.material
    }

    /// The form it is dressed to.
    #[cfg(test)]
    pub(crate) const fn form(&self) -> Form {
        self.form
    }

    #[cfg(test)]
    pub(crate) const fn key(&self) -> u32 {
        self.key
    }

    /// How it has worn, as it holds it.
    pub(crate) fn wear(&self) -> Wear {
        let [arris, chips, lumps, pits, crack] = self.worn;
        Wear {
            arris: f64::from(arris) * 5e-4,
            chips,
            lumps: f64::from(lumps) * 5e-4,
            pits: f64::from(pits) * 2.5e-5,
            crack: f64::from(crack) * 2e-5,
        }
    }

    /// How far its dressed form reaches each way from its middle in its own
    /// frame: its widest, its lumps and whatever covers it included.
    fn reach(&self) -> Vec3 {
        let half = self.half();
        let widest = match self.form {
            Form::Block { fan } => Vec3::new(
                half.x * (1.0 + f64::from(fan).abs() / 100.0),
                half.y,
                half.z,
            ),
            Form::Drum { swell, .. } => {
                let radius = half.x * (1.0 + f64::from(swell) / 1000.0);
                Vec3::new(radius, half.y, radius)
            }
            Form::Turned { bulge, .. } => {
                let radius = half.x.max(half.z) + f64::from(bulge).max(0.0) / 100.0 * half.y;
                Vec3::new(radius, half.y, radius)
            }
            Form::Scroll { .. } => Vec3::new(half.x, half.y, half.x),
        };
        let cover = if self.cover == BARE { 0.0 } else { DEEPEST };
        widest + Vec3::splat(self.wear().lumps + cover + 1e-4)
    }

    /// The box it lies in, in its prototype's frame.
    pub(crate) fn bounds(&self) -> Aabb {
        let (reach, frame, centre) = (self.reach(), self.frame(), self.centre());
        let mut bounds = Aabb::EMPTY;
        for corner in 0..8u8 {
            let local = Vec3::new(
                if corner & 1 == 0 { -reach.x } else { reach.x },
                if corner & 2 == 0 { -reach.y } else { reach.y },
                if corner & 4 == 0 { -reach.z } else { reach.z },
            );
            bounds = bounds.including(centre + frame.to_world(local));
        }
        bounds.padded()
    }

    /// Where `ray`, in its prototype's frame, meets it within `(near, far)`,
    /// seen as `cutting` places its prototype; for a shadow, which asks only
    /// whether it does, the place it was found crossing in.
    pub(crate) fn meet(
        &self,
        ray: &Ray,
        ((near, far), seeking): ((f64, f64), Seeking),
        cutting: Option<&Cutting<'_>>,
    ) -> Option<Hit> {
        let shaped = Shaped::new(self, cutting);
        let local = Ray::new(
            shaped.frame.to_local(ray.origin - shaped.centre),
            shaped.frame.to_local(ray.dir),
        );
        let reach = self.reach();
        let (enter, leave) = Aabb {
            min: -reach,
            max: reach,
        }
        .crossing(&local, reciprocal(local.dir));
        let (enter, leave) = (enter.max(near), leave.min(far));
        if enter >= leave {
            return None;
        }
        shaped.march(&local, ((enter, leave), seeking))
    }
}

/// A unit quaternion `(w, x, y, z)` carrying the world's axes onto
/// `frame`'s, each part in 32 767ths.
fn turn_of(frame: Frame) -> [i16; 4] {
    let (x, y, z) = (frame.x, frame.y, frame.z);
    let trace = x.x + y.y + z.z;
    let raw = if trace > 0.0 {
        let s = 2.0 * mathf::sqrt(1.0 + trace);
        [0.25 * s, (y.z - z.y) / s, (z.x - x.z) / s, (x.y - y.x) / s]
    } else if x.x > y.y && x.x > z.z {
        let s = 2.0 * mathf::sqrt((1.0 + x.x - y.y - z.z).max(1e-12));
        [(y.z - z.y) / s, 0.25 * s, (y.x + x.y) / s, (z.x + x.z) / s]
    } else if y.y > z.z {
        let s = 2.0 * mathf::sqrt((1.0 + y.y - x.x - z.z).max(1e-12));
        [(z.x - x.z) / s, (y.x + x.y) / s, 0.25 * s, (z.y + y.z) / s]
    } else {
        let s = 2.0 * mathf::sqrt((1.0 + z.z - x.x - y.y).max(1e-12));
        [(x.y - y.x) / s, (z.x + x.z) / s, (z.y + y.z) / s, 0.25 * s]
    };
    let norm = mathf::sqrt(raw.iter().map(|part| part * part).sum::<f64>()).max(1e-12);
    raw.map(|part| {
        i16::try_from(mathf::round_i32(
            (part / norm * 32_767.0).clamp(-32_767.0, 32_767.0),
        ))
        .unwrap_or(0)
    })
}

/// The frame the quaternion `turn` carries the world's axes onto, made
/// orthonormal again from its rounded parts.
fn frame_of(turn: [i16; 4]) -> Frame {
    let [w, i, j, k] = turn.map(f64::from);
    let norm = mathf::sqrt(w * w + i * i + j * j + k * k);
    if norm <= 0.0 {
        return Frame::WORLD;
    }
    let [w, i, j, k] = [w / norm, i / norm, j / norm, k / norm];
    Frame {
        x: Vec3::new(
            1.0 - 2.0 * (j * j + k * k),
            2.0 * (i * j + w * k),
            2.0 * (i * k - w * j),
        ),
        y: Vec3::new(
            2.0 * (i * j - w * k),
            1.0 - 2.0 * (i * i + k * k),
            2.0 * (j * k + w * i),
        ),
        z: Vec3::new(
            2.0 * (i * k + w * j),
            2.0 * (j * k - w * i),
            1.0 - 2.0 * (i * i + j * j),
        ),
    }
}

/// A chip struck from an arris: the ball it took away, the way out from
/// the arris it was struck from, and how deep it bit.
#[derive(Copy, Clone, Debug)]
struct Chip {
    centre: Vec3,
    out: Vec3,
    radius: f64,
    bite: f64,
}

/// A crack running into a solid from one of its faces: the plane it runs
/// in, the outward way of the face it opened from and the point on that
/// face, how far it runs along the face, how deep it runs in from it, and
/// how wide it gapes there.
#[derive(Copy, Clone, Debug)]
struct Crack {
    normal: Vec3,
    face: Vec3,
    from: Vec3,
    length: f64,
    depth: f64,
    width: f64,
}

/// How much of a solid's wear shows at a point: its lumps, its chips, its
/// pits and its crack, each `0.0..=1.0`, and its cover's relief.
#[derive(Copy, Clone, Debug)]
struct Showing {
    lumps: f64,
    chips: f64,
    pits: f64,
    crack: f64,
    cover: Shown,
}

impl Showing {
    const ALL: Self = Self {
        lumps: 1.0,
        chips: 1.0,
        pits: 1.0,
        crack: 1.0,
        cover: Shown {
            cushions: 1.0,
            shoots: 1.0,
            crusts: 1.0,
        },
    };

    /// How much shows of features `wear` sizes where a pixel covers
    /// `footprint` of the prototype's units, `scale` metres each.
    fn at((footprint, scale): (f64, f64), wear: &Wear, chips: f64) -> Self {
        let shows = |size: f64| smoothstep(SPANS.0, SPANS.1, size / footprint.max(1e-12));
        Self {
            lumps: shows(wear.lumps),
            chips: shows(chips),
            pits: shows(wear.pits),
            crack: shows(wear.crack),
            cover: Shown::at(footprint * scale, SPANS),
        }
    }

    /// Whether any wear shows as relief, so the surface's own normal is its
    /// relief's.
    fn relieved(&self) -> bool {
        self.pits > 0.0 || self.crack > 0.0 || self.cover.any()
    }
}

/// A solid laid out for its march, in its own frame and metres.
struct Shaped<'a> {
    solid: &'a Solid,
    centre: Vec3,
    frame: Frame,
    half: Vec3,
    wear: Wear,
    /// How much its leaning faces or its profile steepen its distance.
    lean: f64,
    chips: [Chip; MOST_CHIPS],
    chipped: usize,
    /// The largest chip's bite, which how much of the chips shows is judged
    /// by.
    deepest_chip: f64,
    crack: Option<Crack>,
    cover: Option<&'a Cover>,
    view: Option<View>,
}

/// Where a solid is seen from, in its own frame: the eye, the angle a pixel
/// spans, and the prototype's scale, which its covers are grown at.
#[derive(Copy, Clone, Debug)]
struct View {
    eye: Vec3,
    pixel: f64,
    scale: f64,
}

impl<'a> Shaped<'a> {
    fn new(solid: &'a Solid, cutting: Option<&Cutting<'a>>) -> Self {
        let (centre, frame, half) = (solid.centre(), solid.frame(), solid.half());
        let wear = solid.wear();
        let least = half.x.min(half.y).min(half.z);
        let lean = match solid.form {
            Form::Block { fan } => half.x * f64::from(fan).abs() / 100.0 / half.y,
            Form::Drum { taper, swell, .. } => {
                (half.x * f64::from(taper) / 255.0 + 4.0 * half.x * f64::from(swell) / 1000.0)
                    / (2.0 * half.y)
            }
            Form::Turned { bow, bulge } => {
                (half.z - half.x).abs() * f64::from(bow.max(1)) / (2.0 * half.y)
                    + f64::from(bulge).abs() / 100.0 * core::f64::consts::FRAC_PI_2
            }
            // The channel's floor rises to its fillet over the inner part of
            // half its pitch, and winds in toward the eye round the face.
            Form::Scroll { .. } => 2.0 * SCROLL_DEPTH * 1.5 / (0.5 - SCROLL_FILLET),
        };
        let cover = cutting
            .filter(|_| solid.cover != BARE)
            .and_then(|cutting| cutting.materials.get(usize::from(solid.cover)))
            .and_then(covering);
        let view = cutting.map(|cutting| View {
            eye: frame.to_local(cutting.eye - centre),
            pixel: cutting.pixel,
            scale: cutting.scale,
        });
        let mut shaped = Self {
            solid,
            centre,
            frame,
            half,
            wear,
            lean,
            chips: [Chip {
                centre: Vec3::ZERO,
                out: Vec3::UP,
                radius: 0.0,
                bite: 0.0,
            }; MOST_CHIPS],
            chipped: 0,
            deepest_chip: 0.0,
            crack: None,
            cover,
            view,
        };
        shaped.strike(least);
        shaped.crack = shaped.cracked(least);
        shaped
    }

    /// Strike the solid's chips: each from an arris its key picks, a ball
    /// biting into it there.
    fn strike(&mut self, least: f64) {
        let (half, key, head) = (self.half, self.solid.key, self.head());
        let count = usize::from(self.wear.chips).min(MOST_CHIPS);
        let form = self.solid.form;
        for index in 0..count {
            let salted = u32::try_from(index).unwrap_or(0);
            let draw = |salt: u32| unit(mix32(key ^ mix32(salted.wrapping_mul(0x9e37) ^ salt)));
            let size = least.min(0.12);
            // A chip is a shallow scallop, far broader than it is deep.
            let radius = size * (0.3 + 0.6 * draw(1));
            let bite = radius * (0.12 + 0.22 * draw(2));
            let along = 1.7 * draw(3) - 0.85;
            let sign = |salt: u32| if draw(salt) < 0.5 { -1.0 } else { 1.0 };
            let (edge, out) = match form {
                Form::Block { .. } => match mix32(key ^ salted) % 3 {
                    0 => (
                        Vec3::new(along * half.x, sign(4) * half.y, sign(5) * half.z),
                        Vec3::new(0.0, sign(4), sign(5)),
                    ),
                    1 => (
                        Vec3::new(sign(4) * half.x, along * half.y, sign(5) * half.z),
                        Vec3::new(sign(4), 0.0, sign(5)),
                    ),
                    _ => (
                        Vec3::new(sign(4) * half.x, sign(5) * half.y, along * half.z),
                        Vec3::new(sign(4), sign(5), 0.0),
                    ),
                },
                // A round form's chips come off its rims.
                Form::Drum { .. } | Form::Turned { .. } | Form::Scroll { .. } => {
                    let angle = core::f64::consts::TAU * draw(3);
                    let (c, s) = (mathf::cos(angle), mathf::sin(angle));
                    let rim = if sign(4) < 0.0 { half.x } else { head };
                    (
                        Vec3::new(rim * c, sign(4) * half.y, rim * s),
                        Vec3::new(c, sign(4), s),
                    )
                }
            };
            let out = out.normalized();
            if let Some(slot) = self.chips.get_mut(index) {
                *slot = Chip {
                    centre: edge + out * (radius - bite),
                    out,
                    radius,
                    bite,
                };
                self.deepest_chip = self.deepest_chip.max(bite);
                self.chipped = index + 1;
            }
        }
    }

    /// A round form's radius at its head.
    fn head(&self) -> f64 {
        let half = self.half;
        match self.solid.form {
            Form::Turned { .. } => half.z,
            Form::Drum { taper, .. } => half.x * (1.0 - f64::from(taper) / 255.0),
            Form::Block { .. } | Form::Scroll { .. } => half.x,
        }
    }

    /// How far apart a volute `radius` across winding `turns` times lays its
    /// spiral's turns.
    fn spiral_pitch(radius: f64, turns: u8) -> f64 {
        radius * (1.0 - SCROLL_EYE) / f64::from(turns.max(1))
    }

    /// The crack its key runs into it, if it has one.
    fn cracked(&self, least: f64) -> Option<Crack> {
        if self.wear.crack <= 0.0 {
            return None;
        }
        let key = mix32(self.solid.key ^ 0xc4ac);
        let draw = |salt: u32| unit(mix32(key ^ salt));
        let half = self.half;
        // Across the solid's length, leaning a little, opened from its top
        // or from one of its faces.
        let lean = 0.6 * (draw(1) - 0.5);
        let normal =
            Vec3::new(mathf::cos(lean), 0.25 * (draw(2) - 0.5), mathf::sin(lean)).normalized();
        let along = half.x * (1.4 * draw(3) - 0.7);
        let (face, from) = if draw(4) < 0.6 {
            (
                Vec3::UP,
                Vec3::new(along, half.y, half.z * (2.0 * draw(6) - 1.0)),
            )
        } else {
            let side = if draw(5) < 0.5 { 1.0 } else { -1.0 };
            (
                Vec3::new(0.0, 0.0, side),
                Vec3::new(along, half.y * (2.0 * draw(6) - 1.0), side * half.z),
            )
        };
        Some(Crack {
            normal,
            face,
            from,
            length: (2.0 * half.y).max(half.z) * (0.6 + 0.9 * draw(7)),
            depth: least * (0.4 + 0.8 * draw(8)),
            width: self.wear.crack,
        })
    }

    /// How much of the wear shows at local point `q`.
    fn showing(&self, q: Vec3) -> Showing {
        match self.view {
            Some(view) => {
                let footprint = (q - view.eye).length() * view.pixel;
                Showing::at((footprint, view.scale), &self.wear, self.deepest_chip)
            }
            None => Showing::ALL,
        }
    }

    /// How far local point `q` stands outside the dressed form, arrises worn
    /// round: negative within it.
    fn dressed(&self, q: Vec3) -> f64 {
        let half = self.half;
        let least = half.x.min(half.y).min(half.z);
        let round = self.wear.arris.min(0.45 * least);
        let lean = 1.0 / mathf::sqrt(1.0 + self.lean * self.lean);
        let rounded = |(across, up): (f64, f64)| {
            let outside = mathf::hypot(across.max(0.0), up.max(0.0));
            outside + across.max(up).min(0.0) - round
        };
        match self.solid.form {
            Form::Block { fan } => {
                let grows = f64::from(fan) / 100.0;
                let reach = half.x * (1.0 + grows * (q.y / half.y).clamp(-1.0, 1.0));
                let e = Vec3::new(
                    (q.x.abs() - reach) * lean + round,
                    q.y.abs() - half.y + round,
                    q.z.abs() - half.z + round,
                );
                let outside = e.max(Vec3::ZERO).length();
                outside + e.x.max(e.y).max(e.z).min(0.0) - round
            }
            Form::Drum {
                taper,
                swell,
                flutes,
            } => {
                let t = f64::midpoint(q.y / half.y, 1.0).clamp(0.0, 1.0);
                let head = half.x * (1.0 - f64::from(taper) / 255.0);
                let mut radius = half.x
                    + (head - half.x) * t
                    + half.x * f64::from(swell) / 1000.0 * 4.0 * t * (1.0 - t);
                if flutes > 0 {
                    let around =
                        mathf::atan2(q.z, q.x) / core::f64::consts::TAU * f64::from(flutes);
                    let within = around - mathf::floor(around);
                    let groove = 1.0 - (2.0 * within - 1.0) * (2.0 * within - 1.0);
                    radius -= FLUTE_DEPTH * half.x * groove;
                }
                let rho = mathf::hypot(q.x, q.z);
                rounded(((rho - radius) * lean + round, q.y.abs() - half.y + round))
            }
            Form::Turned { bow, bulge } => {
                let t = f64::midpoint(q.y / half.y, 1.0).clamp(0.0, 1.0);
                let mut fall = 1.0;
                for _ in 0..bow.max(1) {
                    fall *= 1.0 - t;
                }
                let radius = half.x
                    + (half.z - half.x) * (1.0 - fall)
                    + f64::from(bulge) / 100.0 * half.y * mathf::sin(core::f64::consts::PI * t);
                let rho = mathf::hypot(q.x, q.z);
                rounded(((rho - radius) * lean + round, q.y.abs() - half.y + round))
            }
            Form::Scroll { turns } => {
                let rho = mathf::hypot(q.x, q.z);
                let face = half.y - self.channel((rho, mathf::atan2(q.z, q.x)), turns);
                rounded((rho - half.x + round, q.y.abs() - face + round))
            }
        }
    }

    /// The way the dressed form faces at local point `q`: a block's read
    /// off the rounded box [`Self::dressed`] measures, through its fan and
    /// its leaning faces; a turned form's found as its distance's gradient.
    fn dressed_normal(&self, q: Vec3) -> Vec3 {
        let Form::Block { fan } = self.solid.form else {
            return gradient(q, 1e-4, |at| self.dressed(at));
        };
        let half = self.half;
        let least = half.x.min(half.y).min(half.z);
        let round = self.wear.arris.min(0.45 * least);
        let lean = 1.0 / mathf::sqrt(1.0 + self.lean * self.lean);
        let grows = f64::from(fan) / 100.0;
        let height = q.y / half.y;
        let e = Vec3::new(
            (q.x.abs() - half.x * (1.0 + grows * height.clamp(-1.0, 1.0))) * lean + round,
            q.y.abs() - half.y + round,
            q.z.abs() - half.z + round,
        );
        let outside = e.max(Vec3::ZERO);
        let length = outside.length();
        // How the distance rises with each part of `e`: away from the inner
        // box where it lies outside it, else along the part nearest its face.
        let rises = if length > 0.0 {
            outside * (1.0 / length)
        } else if e.x >= e.y && e.x >= e.z {
            Vec3::new(1.0, 0.0, 0.0)
        } else if e.y >= e.z {
            Vec3::UP
        } else {
            Vec3::new(0.0, 0.0, 1.0)
        };
        let sign = |value: f64| if value < 0.0 { -1.0 } else { 1.0 };
        // A voussoir's ends lean in as it rises.
        let slant = if height.abs() < 1.0 {
            half.x * grows / half.y
        } else {
            0.0
        };
        Vec3::new(
            rises.x * lean * sign(q.x),
            rises.y * sign(q.y) - rises.x * lean * slant,
            rises.z * sign(q.z),
        )
        .normalized()
    }

    /// How deep a volute's channel is cut at `rho` from its middle and
    /// `angle` round it: nought on its fillet, where the spiral runs, and
    /// on the eye within it; deepest between the turns.
    fn channel(&self, (rho, angle): (f64, f64), turns: u8) -> f64 {
        let radius = self.half.x;
        let eye = SCROLL_EYE * radius;
        if rho <= eye || rho >= radius {
            return 0.0;
        }
        let pitch = Self::spiral_pitch(radius, turns);
        let around = angle / core::f64::consts::TAU;
        let around = around - mathf::floor(around);
        // The spiral passes `rho` once a turn, each pass a pitch further in.
        let mut nearest = f64::INFINITY;
        for turn in 0..=turns {
            let wound = (around + f64::from(turn)) / f64::from(turns.max(1));
            if wound > 1.0 {
                break;
            }
            nearest = nearest.min((rho - (radius - (radius - eye) * wound)).abs());
        }
        let share = (nearest / pitch).min(0.5);
        SCROLL_DEPTH * pitch * smoothstep(SCROLL_FILLET, 0.5, share)
    }

    /// How far local point `q` stands outside the worn solid: its dressed
    /// form, its faces wandering and pitted, its chips taken and its crack
    /// opened, each as much as `showing` has it.
    fn worn(&self, q: Vec3, showing: &Showing) -> f64 {
        let key = self.solid.key;
        let least = self.half.x.min(self.half.y).min(self.half.z);
        let dressed = self.dressed(q);
        let mut d = dressed;
        let lumps = self.wear.lumps * showing.lumps;
        if lumps > 0.0 {
            let scale = LUMPS / least;
            d += lumps
                * (0.65 * noise3(q * scale, key ^ 0x1a)
                    + 0.35 * noise3(q * (2.7 * scale), key ^ 0x1b));
        }
        for chip in self.chips.iter().take(self.chipped) {
            let bite = chip.bite * showing.chips;
            if bite <= 0.0 {
                continue;
            }
            // A chip fading out bites less deep, its ball drawn back out.
            let centre = chip.centre + chip.out * (chip.bite - bite);
            d = d.max(chip.radius - (q - centre).length());
        }
        let pits = self.wear.pits * showing.pits;
        if pits > 0.0 && d.abs() < 2.0 * pits {
            let found = cells3(q * PITS, key ^ 0x2a, 0.9);
            // Erosion only takes stone away: its grain roughens the face
            // inward, never out.
            let dimple = 1.0 - smoothstep(0.0, 0.5, found.nearest);
            let grain = 0.5 + 0.5 * noise3(q * (3.1 * PITS), key ^ 0x2b);
            d += pits * (dimple + 0.35 * grain);
        }
        if let Some(crack) = self.crack.filter(|_| showing.crack > 0.0) {
            let offset = q - crack.from;
            let s = crack.normal.dot(offset) + 0.0015 * noise3(q * JAGS, key ^ 0x3a);
            let along = crack.face.cross(crack.normal).dot(offset).abs();
            let deep = (-crack.face.dot(offset)).max(0.0);
            let gape = crack.width
                * showing.crack
                * smoothstep(crack.length, 0.55 * crack.length, along)
                * smoothstep(crack.depth, 0.0, deep);
            if gape > 0.0 {
                d = d.max(gape - s.abs());
            }
        }
        d
    }

    /// How far local point `q` stands outside the solid and what covers it,
    /// as much of each as `showing` has it.
    fn standing(&self, q: Vec3, showing: &Showing) -> f64 {
        let worn = self.worn(q, showing);
        match (self.cover, self.view) {
            (Some(cover), Some(view)) if showing.cover.any() => {
                // Beyond what its cover could reach, the cover cannot change
                // which side of the surface a point stands.
                let deepest = DEEPEST / view.scale;
                if worn > deepest {
                    return worn - deepest;
                }
                worn - cover.depth(&self.lodging(q), showing.cover) / view.scale
            }
            _ => worn,
        }
    }

    /// Where local point `q` lies as its cover grows there: in its
    /// structure's own frame and metres, the way its dressed form faces, and
    /// how far it lies from the nearest of the unit's edges, where it meets
    /// the next unit at a joint.
    fn lodging(&self, q: Vec3) -> Lodging {
        let scale = self.view.map_or(1.0, |view| view.scale);
        let normal = self.frame.to_world(self.dressed_normal(q));
        // On a face the nearest of its bounds is the face itself; the next
        // nearest is the edge it runs to.
        let (a, b, c) = (
            self.half.x - q.x.abs(),
            self.half.y - q.y.abs(),
            self.half.z - q.z.abs(),
        );
        let edge = a.min(b).max(a.max(b).min(c));
        Lodging {
            p: (self.centre + self.frame.to_world(q)) * scale,
            normal,
            affinity: self.affinity(),
            joint: edge.max(0.0) * scale,
        }
    }

    /// How steeply the surface can rise along a ray where as much shows as
    /// `showing` has it, a metre to a metre.
    fn steepest(&self, showing: &Showing) -> f64 {
        let least = self.half.x.min(self.half.y).min(self.half.z);
        let flutes = match self.solid.form {
            Form::Drum { flutes, .. } => {
                4.0 * FLUTE_DEPTH * f64::from(flutes) / core::f64::consts::TAU
            }
            Form::Block { .. } | Form::Turned { .. } | Form::Scroll { .. } => 0.0,
        };
        let lumps =
            self.wear.lumps * showing.lumps * NOISE_SLOPE * LUMPS / least * (0.65 + 0.35 * 2.7);
        let pits = self.wear.pits * showing.pits * PITS * (3.0 + 0.35 * NOISE_SLOPE * 3.1);
        let crack = if self.crack.is_some() && showing.crack > 0.0 {
            0.0015 * JAGS * NOISE_SLOPE
        } else {
            0.0
        };
        let cover = match (self.cover, self.view) {
            (Some(cover), Some(_)) => cover.steepest(showing.cover),
            _ => 0.0,
        };
        MARGIN * (1.0 + self.lean + flutes + lumps + pits + crack + cover)
    }

    /// The surface `ray`, in the solid's own frame, first meets within
    /// `(enter, leave)`.
    fn march(&self, ray: &Ray, ((enter, leave), seeking): ((f64, f64), Seeking)) -> Option<Hit> {
        // Detail shows most where the solid stands nearest the eye, so the
        // march steps as finely as it needs to there throughout.
        let nearest = self.showing(self.nearest_to_eye());
        let steepest = self.steepest(&nearest);
        // A step never outgrows a share of the solid itself, however far off
        // it stands, or it would step clean over it.
        let finest = SMALLEST * self.half.x.min(self.half.y).min(self.half.z);
        let shortest = |t: f64| {
            let least = match self.view {
                Some(view) => {
                    let pixel = (ray.at(t) - view.eye).length() * view.pixel;
                    (LEAST_STEP.0 * pixel).max(LEAST_STEP.1 / view.scale)
                }
                None => LEAST_STEP.1,
            };
            least.max((leave - enter) / MOST_STEPS).min(finest)
        };
        let stand = |t: f64| {
            let q = ray.at(t);
            self.standing(q, &self.showing(q))
        };
        let (mut before, mut t) = (enter, enter);
        let mut above = stand(t);
        let mut was = above;
        // A ray leaving the solid from within, as one bouncing off a hollow
        // its chips left does, is let out before it looks for the surface.
        let mut inside = above < 0.0;
        while t < leave {
            if above < 0.0 && !inside {
                return Some(self.found(ray, ((before, was), (t, above)), seeking, shortest(t)));
            }
            if above >= 0.0 {
                inside = false;
            }
            (before, was) = (t, above);
            t = (t + (above.abs() / steepest).max(shortest(t))).min(leave);
            above = stand(t);
            if t >= leave {
                break;
            }
        }
        (above < 0.0 && !inside)
            .then(|| self.found(ray, ((before, was), (t, above)), seeking, shortest(t)))
    }

    /// The local point of the solid's reach nearest the eye.
    fn nearest_to_eye(&self) -> Vec3 {
        let reach = self.solid.reach();
        self.view.map_or(Vec3::ZERO, |view| {
            Vec3::new(
                view.eye.x.clamp(-reach.x, reach.x),
                view.eye.y.clamp(-reach.y, reach.y),
                view.eye.z.clamp(-reach.z, reach.z),
            )
        })
    }

    /// The hit where `ray` crosses into the surface between `outside` and
    /// `within`, each with how far it stands outside the surface, found to
    /// within a share of `step`.
    fn found(
        &self,
        ray: &Ray,
        ((mut outside, mut out), (mut within, mut inn)): ((f64, f64), (f64, f64)),
        seeking: Seeking,
        step: f64,
    ) -> Hit {
        if seeking == Seeking::Any {
            return Hit::plain(within, Vec3::UP);
        }
        let tolerance = FOUND * step;
        // Regula falsi, the end that keeps its place halved each time it
        // does so twice running (the Illinois rule), so neither end sticks;
        // a guess off the bracket falls back to its middle.
        let mut kept = 0i8;
        for _ in 0..MOST_FOUND {
            if within - outside <= tolerance {
                break;
            }
            let guess = within - inn * (within - outside) / (inn - out);
            let middle = if guess > outside && guess < within {
                guess
            } else {
                outside.midpoint(within)
            };
            let q = ray.at(middle);
            let standing = self.standing(q, &self.showing(q));
            if standing < 0.0 {
                (within, inn) = (middle, standing);
                if kept < 0 {
                    out *= 0.5;
                }
                kept = -1;
            } else {
                (outside, out) = (middle, standing);
                if kept > 0 {
                    inn *= 0.5;
                }
                kept = 1;
            }
        }
        let t = outside.midpoint(within);
        let q = ray.at(t);
        let showing = self.showing(q);
        let spacing = 4.0 * tolerance;
        let normal = gradient(q, spacing, |at| self.standing(at, &showing));
        let normal = if normal.dot(normal) > 0.0 {
            normal
        } else {
            gradient(q, 1e-4, |at| self.dressed(at))
        };
        let (face, on_face) = self.face(q);
        let (material, uv) = match self.cover {
            Some(cover) => match cover.growth(&self.lodging(q)) {
                Growth::Bare => (self.solid.material, on_face),
                growth => (self.solid.cover, growth.carried()),
            },
            None => (self.solid.material, on_face),
        };
        let world = self.frame.to_world(normal);
        Hit {
            t,
            normal: world,
            shading: world,
            mark: self.solid.key,
            along: face,
            uv,
            girth: 0.0,
            material: Some(u32::from(material)),
            tangent: self.frame.x,
            relieved: showing.relieved(),
            member: None,
            cover: None,
        }
    }

    /// How readily moss lodges on the solid.
    fn affinity(&self) -> f64 {
        f64::from(self.solid.affinity) / 255.0
    }

    /// Which of its faces local point `q` lies on — `0.0` an end, across its
    /// own `x`; `1.0` a bed, across `y`; `2.0` a face, across `z` — and where
    /// on that face, as shares of the face's two half extents: what its
    /// pigment reads it by.
    fn face(&self, q: Vec3) -> (f64, (f64, f64)) {
        let (x, y, z) = (q.x / self.half.x, q.y / self.half.y, q.z / self.half.z);
        if x.abs() >= y.abs() && x.abs() >= z.abs() {
            (0.0, (z, y))
        } else if y.abs() >= z.abs() {
            (1.0, (x, z))
        } else {
            (2.0, (x, y))
        }
    }
}

/// A cover's growth as its material's pigment holds it, if it is a cover's.
fn covering(material: &Material) -> Option<&Cover> {
    match &material.pigment {
        Pigment::Cover(cover) => Some(cover),
        _ => None,
    }
}

/// How deep a fluted drum's flutes are cut, as a share of its radius.
const FLUTE_DEPTH: f64 = 0.035;

/// A volute's eye, as a share of its radius; how deep its channel is cut,
/// and how broad its fillet stands, as shares of its turns' pitch.
const SCROLL_EYE: f64 = 0.16;
const SCROLL_DEPTH: f64 = 0.32;
const SCROLL_FILLET: f64 = 0.1;

#[cfg(test)]
#[path = "solid_tests.rs"]
mod tests;
