//! A water lily as it grows and ages: the outline each of its pads, sepals
//! and petals is cut to, drawn from the part's own key, and the colours its
//! parts wear through their lives. The water-crowfoot's small flower is made
//! of the same parts, its petals broad.
//!
//! A pad is reckoned in flat coordinates of its own radius: its stalk at the
//! origin, the slit of its sinus opening along `-x`. A sepal or a petal is
//! reckoned in its own length, `u` along it from its base and `v` across it,
//! so a nick in its edge is round. A part's key carries its plant's colour in
//! its two low bits and how far through its life it is in the four above;
//! the rest draws its blemishes, so the outline a ray is cut to and the
//! colours it is shaded in agree.

use core::f64::consts::{FRAC_PI_2, PI, TAU};

use tairix_util::mathf;

use crate::noise::{cell, cells2, noise2, smoothstep};
use crate::pigment::Spot;
use crate::sample::{mix32, unit};
use crate::vector::{cell_of, power, Vec3};

/// The bits of a part's key, above its plant's colour, that keep how far
/// through its life it is.
const AGE_SHIFT: u32 = 2;
const AGE_LEVELS: u32 = 16;
const AGE_MASK: u32 = (AGE_LEVELS - 1) << AGE_SHIFT;

/// `key`, its part `age` of the way through its life: `0.0` just unrolled
/// or opened, `1.0` dying.
pub(crate) fn aged(key: u32, age: f64) -> u32 {
    let level = mathf::round_i32(age.clamp(0.0, 1.0) * f64::from(AGE_LEVELS - 1));
    (key & !AGE_MASK) | (u32::try_from(level).unwrap_or(0) << AGE_SHIFT)
}

/// How far through its life the part keyed `key` is.
pub(crate) fn age(key: u32) -> f64 {
    f64::from((key & AGE_MASK) >> AGE_SHIFT) / f64::from(AGE_LEVELS - 1)
}

/// The outline a lily's mesh is cut to where its coordinates fall.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Trim {
    Pad,
    Sheet(Sheet),
}

/// The sheets a flower is made of.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Sheet {
    Sepal,
    Petal,
    /// A stamen: a narrow strap, its anther along its upper half.
    Stamen,
    /// A broad petal widest toward its round tip: a crowfoot's.
    Broad,
}

impl Trim {
    /// Whether the part keyed `key` covers `at` of its flat coordinates.
    pub(crate) fn keeps(self, at: (f64, f64), key: u32) -> bool {
        match self {
            Self::Pad => Pad::of(key).edges(at).kept(),
            Self::Sheet(sheet) => Petal::of(key, sheet).edges(at).kept(),
        }
    }
}

/// The draws a part's key makes: the same each time it is read.
struct Draws {
    key: u32,
    drawn: u32,
}

impl Draws {
    fn of(key: u32) -> Self {
        Self {
            key: mix32(key ^ 0x5ca1_ab1e),
            drawn: 0,
        }
    }

    fn unit(&mut self) -> f64 {
        self.drawn = self.drawn.wrapping_add(1);
        unit(mix32(self.key ^ self.drawn.wrapping_mul(0x9e37_79b9)))
    }

    fn range(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }

    /// How many of a blemish a part bears: `mean` of them on average, most
    /// often few, and `most` at most.
    fn count(&mut self, mean: f64, most: usize) -> usize {
        cell_of(-mathf::ln(1.0 - self.unit()) * mean).0.min(most)
    }
}

/// How far inside a part's outline a point lies, in the part's size: from
/// its margin, and from the nearest wound a grazer, a tear or rot made in
/// it. Negative off it.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Edges {
    pub(crate) margin: f64,
    pub(crate) wound: f64,
}

impl Edges {
    fn kept(self) -> bool {
        self.margin > 0.0 && self.wound > 0.0
    }
}

/// How ragged a bite's edge is chewed, and how many of its tooth marks span
/// its part; how unevenly a hole rots round, and how many of its wobbles do.
const CHEWED: f64 = 0.14;
const CHEW: f64 = 38.0;
const ROTTED: f64 = 0.2;
const ROT: f64 = 60.0;

/// A piece bitten from a part: an oval about `centre`, its half-lengths
/// `axes` along and across the unit `way`, its edge chewed ragged.
#[derive(Copy, Clone, Debug, Default)]
struct Bite {
    centre: (f64, f64),
    way: (f64, f64),
    axes: (f64, f64),
}

impl Bite {
    /// A piece bitten from a pad within `rim`: most often from its margin,
    /// as a china-mark moth's larva cuts its case, else from within.
    fn on_pad(draws: &mut Draws, rim: &Rim) -> Self {
        let angle = draws.range(-(PI - 0.35), PI - 0.35);
        let (cos, sin) = (mathf::cos(angle), mathf::sin(angle));
        if draws.unit() < 0.75 {
            let reach = rim.reach(angle) - draws.range(-0.02, 0.05);
            Self {
                centre: (reach * cos, reach * sin),
                way: (cos, sin),
                axes: (draws.range(0.035, 0.08), draws.range(0.05, 0.13)),
            }
        } else {
            let reach = draws.range(0.3, 0.8);
            let turn = draws.range(0.0, PI);
            Self {
                centre: (reach * cos, reach * sin),
                way: (mathf::cos(turn), mathf::sin(turn)),
                axes: (draws.range(0.025, 0.06), draws.range(0.03, 0.075)),
            }
        }
    }

    /// How far outside it `(x, y)` lies, negative within: its tooth marks,
    /// drawn under `seed`, fade away from its edge.
    fn clear(&self, (x, y): (f64, f64), seed: u32) -> f64 {
        let (dx, dy) = (x - self.centre.0, y - self.centre.1);
        let along = (dx * self.way.0 + dy * self.way.1) / self.axes.0;
        let across = (dy * self.way.0 - dx * self.way.1) / self.axes.1;
        let reach = mathf::hypot(along, across);
        let near = 1.0 - smoothstep(CHEWED, 3.0 * CHEWED, (reach - 1.0).abs());
        let chewed = if near > 0.0 {
            1.0 + CHEWED * near * noise2(x * CHEW, y * CHEW, seed)
        } else {
            1.0
        };
        (reach - chewed) * self.axes.0.min(self.axes.1)
    }
}

/// A split running from `origin` along the unit `way`: closed until `from`
/// along it, gaping wider until it is `gape` wide at `to`, where it meets
/// the part's edge.
#[derive(Copy, Clone, Debug, Default)]
struct Split {
    origin: (f64, f64),
    way: (f64, f64),
    from: f64,
    to: f64,
    gape: f64,
}

impl Split {
    /// How far outside it `(x, y)` lies, negative within.
    fn clear(&self, (x, y): (f64, f64)) -> f64 {
        let (dx, dy) = (x - self.origin.0, y - self.origin.1);
        let along = dx * self.way.0 + dy * self.way.1;
        let across = (dy * self.way.0 - dx * self.way.1).abs();
        if along <= self.from {
            return mathf::hypot(self.from - along, across);
        }
        let opened = ((along - self.from) / (self.to - self.from).max(1e-9)).min(1.0);
        across - 0.5 * self.gape * opened
    }
}

/// A hole rotted or eaten through a part, about `centre`.
#[derive(Copy, Clone, Debug, Default)]
struct Hole {
    centre: (f64, f64),
    radius: f64,
}

impl Hole {
    /// How far outside it `(x, y)` lies, negative within: its rim, drawn
    /// under `seed`, wanders only near its edge.
    fn clear(&self, (x, y): (f64, f64), seed: u32) -> f64 {
        let reach = mathf::hypot(x - self.centre.0, y - self.centre.1);
        let near = 1.0 - smoothstep(ROTTED, 3.0 * ROTTED, (reach / self.radius - 1.0).abs());
        let wobble = if near > 0.0 {
            ROTTED * near * noise2(x * ROT, y * ROT, seed)
        } else {
            0.0
        };
        reach - self.radius * (1.0 + wobble)
    }
}

/// The slit a pad's flat coordinates leave either side of `-x`, in radians:
/// its lobes meet or overlap only as it is laid.
pub(crate) const SINUS: f64 = 0.04;

/// The most bites, splits and holes a pad bears, and nicks a petal does.
const MOST_BITES: usize = 5;
const MOST_SPLITS: usize = 3;
const MOST_HOLES: usize = 8;
const MOST_NICKS: usize = 3;

/// How many of a frayed margin's notches span a pad's radius about its
/// round.
const FRAY: f64 = 9.0;

/// A pad's margin before blemishes: how much longer than broad it is and
/// the way it is longest, and two gentle undulations of it, each a count
/// round it, a depth and where it starts.
#[derive(Copy, Clone, Debug)]
struct Rim {
    oval: (f64, f64),
    waves: [(f64, f64, f64); 2],
}

impl Rim {
    fn reach(&self, angle: f64) -> f64 {
        let mut reach = 1.0 + self.oval.0 * mathf::cos(2.0 * (angle - self.oval.1));
        for (count, depth, start) in self.waves {
            reach += depth * mathf::cos(count * angle + start);
        }
        reach
    }

    fn most(&self) -> f64 {
        1.0 + self.oval.0.abs() + self.waves.iter().map(|wave| wave.1).sum::<f64>()
    }
}

/// A water lily's pad as its key draws it.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Pad {
    /// How far through its life it is.
    pub(crate) age: f64,
    rim: Rim,
    /// How round each lobe's tip is, and how deep its margin has frayed.
    corner: f64,
    fray: f64,
    bites: [Bite; MOST_BITES],
    splits: [Split; MOST_SPLITS],
    holes: [Hole; MOST_HOLES],
    /// How many of its bites, splits and holes it bears.
    marks: (usize, usize, usize),
    /// What its blemishes and its colours are drawn under.
    seed: u32,
}

impl Pad {
    /// The pad keyed `key`: the older, the more it has been bitten, torn and
    /// rotted.
    pub(crate) fn of(key: u32) -> Self {
        let age = age(key);
        let mut draws = Draws::of(key);
        let rim = Rim {
            oval: (draws.range(-0.07, 0.07), draws.range(0.0, PI)),
            waves: [
                (
                    3.0 + mathf::floor(3.0 * draws.unit()),
                    draws.range(0.0, 0.035),
                    draws.range(0.0, TAU),
                ),
                (
                    6.0 + mathf::floor(3.0 * draws.unit()),
                    draws.range(0.0, 0.016),
                    draws.range(0.0, TAU),
                ),
            ],
        };
        let corner = draws.range(0.03, 0.13);
        let fray = draws.range(0.0, 0.08) * smoothstep(0.55, 1.0, age);
        let worn = smoothstep(0.2, 0.95, age);
        let mut bites = [Bite::default(); MOST_BITES];
        let bitten = draws.count(0.35 + 2.4 * worn, MOST_BITES);
        for bite in bites.iter_mut().take(bitten) {
            *bite = Bite::on_pad(&mut draws, &rim);
        }
        let mut splits = [Split::default(); MOST_SPLITS];
        let torn = draws.count(0.15 + 0.9 * worn, MOST_SPLITS);
        for split in splits.iter_mut().take(torn) {
            // Along a vein, from the margin in.
            let angle = draws.range(-(PI - 0.3), PI - 0.3);
            let reach = rim.reach(angle);
            *split = Split {
                origin: (0.0, 0.0),
                way: (mathf::cos(angle), mathf::sin(angle)),
                from: reach - draws.range(0.08, 0.45),
                to: reach,
                gape: draws.range(0.006, 0.03),
            };
        }
        let mut holes = [Hole::default(); MOST_HOLES];
        let pierced = draws.count(0.2 + 3.5 * worn * worn, MOST_HOLES);
        for hole in holes.iter_mut().take(pierced) {
            let (angle, reach) = (draws.range(-PI, PI), draws.range(0.12, 0.88));
            *hole = Hole {
                centre: (reach * mathf::cos(angle), reach * mathf::sin(angle)),
                radius: draws.range(0.008, 0.028) * (1.0 + age),
            };
        }
        Self {
            age,
            rim,
            corner,
            fray,
            bites,
            splits,
            holes,
            marks: (bitten, torn, pierced),
            seed: mix32(key ^ 0x0b5e_55ed),
        }
    }

    /// How far its margin reaches from its stalk at flat `angle`, before it
    /// frays or is bitten.
    pub(crate) fn reach(&self, angle: f64) -> f64 {
        self.rim.reach(angle)
    }

    /// The farthest its margin reaches.
    pub(crate) fn most(&self) -> f64 {
        self.rim.most()
    }

    /// How far inside it `(x, y)` lies: from its margin, its lobes' tips
    /// rounded where they meet its sinus's slit, and from its wounds.
    pub(crate) fn edges(&self, (x, y): (f64, f64)) -> Edges {
        let r = mathf::hypot(x, y);
        let angle = mathf::atan2(y, x);
        let open = PI - SINUS - angle.abs();
        let slit = if open < FRAC_PI_2 {
            r * mathf::sin(open)
        } else {
            r
        };
        let edge = self.rim.reach(angle) - self.frayed(angle) - r;
        let corner = self.corner;
        let margin = if edge < corner && slit < corner {
            corner - mathf::hypot(corner - edge, corner - slit)
        } else {
            edge.min(slit)
        };
        let mut wound = f64::INFINITY;
        let mut salt = self.seed;
        for bite in self.bites.iter().take(self.marks.0) {
            salt = mix32(salt);
            wound = wound.min(bite.clear((x, y), salt));
        }
        for split in self.splits.iter().take(self.marks.1) {
            wound = wound.min(split.clear((x, y)));
        }
        for hole in self.holes.iter().take(self.marks.2) {
            salt = mix32(salt);
            wound = wound.min(hole.clear((x, y), salt));
        }
        Edges { margin, wound }
    }

    /// How far its margin has frayed in at `angle`, in notches as it rots.
    fn frayed(&self, angle: f64) -> f64 {
        if self.fray <= 0.0 {
            return 0.0;
        }
        let (x, y) = (FRAY * mathf::cos(angle), FRAY * mathf::sin(angle));
        let notch = noise2(x, y, self.seed) + 0.5 * noise2(2.0 * x, 2.0 * y, self.seed ^ 0x2f);
        self.fray * power(notch - 0.1, 1.3)
    }
}

/// A water lily's sepal, petal or stamen as its key draws it.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Petal {
    /// How far through its life it is.
    pub(crate) age: f64,
    /// Its greatest half-width, in its length.
    pub(crate) breadth: f64,
    /// How far along it is widest, how blunt its tip, and how much wider
    /// its one side is than its other.
    widest: f64,
    blunt: f64,
    lean: f64,
    split: Split,
    nicks: [Hole; MOST_NICKS],
    /// Whether it is split, and how many nicks it bears.
    marks: (bool, usize),
    seed: u32,
}

impl Petal {
    /// The `sheet` keyed `key`: a petal widest past its middle and
    /// blunt-tipped, a sepal widest below it and drawn to a point, a stamen
    /// a narrow strap, a broad petal round; the older, the more it has split
    /// and been nicked.
    pub(crate) fn of(key: u32, sheet: Sheet) -> Self {
        let age = age(key);
        let mut draws = Draws::of(key);
        let (breadth, widest, blunt) = match sheet {
            Sheet::Sepal => (
                draws.range(0.15, 0.2),
                draws.range(0.3, 0.42),
                draws.range(0.75, 1.0),
            ),
            Sheet::Petal => (
                draws.range(0.16, 0.23),
                draws.range(0.5, 0.66),
                draws.range(0.38, 0.7),
            ),
            Sheet::Stamen => (
                draws.range(0.035, 0.09),
                draws.range(0.5, 0.75),
                draws.range(0.7, 1.0),
            ),
            Sheet::Broad => (
                draws.range(0.36, 0.48),
                draws.range(0.62, 0.8),
                draws.range(0.25, 0.42),
            ),
        };
        let mut petal = Self {
            age,
            breadth,
            widest,
            blunt,
            lean: draws.range(-0.14, 0.14),
            split: Split::default(),
            nicks: [Hole::default(); MOST_NICKS],
            marks: (false, 0),
            seed: mix32(key ^ 0x9e7a_1505),
        };
        let worn = smoothstep(0.35, 1.0, age);
        if draws.unit() < 0.05 + 0.3 * worn {
            // From its tip down along a vein.
            let depth = draws.range(0.08, 0.3);
            petal.split = Split {
                origin: (1.0 - depth, draws.range(-0.4, 0.4) * breadth),
                way: (1.0, 0.0),
                from: 0.0,
                to: depth,
                gape: draws.range(0.01, 0.035),
            };
            petal.marks.0 = true;
        }
        let nicked = draws.count(0.1 + 1.2 * worn, MOST_NICKS);
        for index in 0..nicked {
            let u = draws.range(0.3, 0.95);
            let side = if draws.unit() < 0.5 { -1.0 } else { 1.0 };
            let radius = draws.range(0.15, 0.4) * breadth;
            let edge = petal.half_width(u, side);
            if let Some(nick) = petal.nicks.get_mut(index) {
                *nick = Hole {
                    centre: (u, side * (edge + 0.3 * radius)),
                    radius,
                };
            }
        }
        petal.marks.1 = nicked;
        petal
    }

    /// Its half-width `u` along it on the side `side` of its midrib, `1.0`
    /// or `-1.0`, in its length: narrowing to a claw at its base, and to
    /// nothing at its tip, blunt or pointed.
    pub(crate) fn half_width(&self, u: f64, side: f64) -> f64 {
        let shaped = if u < self.widest {
            0.5 * u / self.widest
        } else {
            0.5 + 0.5 * (u - self.widest) / (1.0 - self.widest)
        };
        let body = power(mathf::sin(PI * shaped.clamp(0.0, 1.0)), self.blunt);
        let claw = 0.3 * (1.0 - smoothstep(0.0, 0.3, u));
        self.breadth * (claw + (1.0 - claw) * body) * (1.0 + self.lean * side * mathf::sin(PI * u))
    }

    /// How far inside it `(u, v)` lies: from its margin, and from its split
    /// and its nicks.
    pub(crate) fn edges(&self, (u, v): (f64, f64)) -> Edges {
        let side = if v < 0.0 { -1.0 } else { 1.0 };
        let margin = (self.half_width(u.clamp(0.0, 1.0), side) - v.abs()).min(1.0 - u);
        let mut wound = if self.marks.0 {
            self.split.clear((u, v))
        } else {
            f64::INFINITY
        };
        let mut salt = self.seed;
        for nick in self.nicks.iter().take(self.marks.1) {
            salt = mix32(salt);
            wound = wound.min(nick.clear((u, v), salt));
        }
        Edges { margin, wound }
    }
}

/// What a water lily's material colours.
#[derive(Clone, Debug)]
pub(crate) enum Lily {
    /// Its pads, in four greens as the season has them.
    Pads([Vec3; 4]),
    /// The stalks of its pads and flowers.
    Stalks,
    /// Its sepals: olive without, pale within.
    Sepals,
    /// Its petals, in four whites.
    Petals([Vec3; 4]),
    /// A crowfoot's broad petals, in four whites.
    Broad([Vec3; 4]),
    /// Its stamens and its stigma, in four yellows.
    Hearts([Vec3; 4]),
}

impl Lily {
    /// The colour at `spot`, the part met keyed as its mark is beneath its
    /// placing's.
    pub(crate) fn colour(&self, spot: &Spot) -> Vec3 {
        let key = spot.mark ^ spot.instance;
        match self {
            Self::Pads(greens) => pad(greens, spot, key),
            Self::Stalks => stalk(spot, key),
            Self::Sepals => sepal(spot, key),
            Self::Petals(whites) => petal(whites, spot, (key, Sheet::Petal)),
            Self::Broad(whites) => petal(whites, spot, (key, Sheet::Broad)),
            Self::Hearts(yellows) => heart(yellows, spot, key),
        }
    }
}

/// A pad just unrolled, bronze; and as it yellows, browns and dies.
const BRONZE: Vec3 = Vec3::new(0.098, 0.025, 0.021);
/// The tints a pad's green wanders between over its blade: bluer, yellower.
const HUES: [Vec3; 2] = [Vec3::new(0.85, 1.0, 1.15), Vec3::new(1.2, 1.05, 0.8)];
const YELLOWED: Vec3 = Vec3::new(0.392, 0.305, 0.045);
const DEAD: Vec3 = Vec3::new(0.107, 0.055, 0.018);
/// The rim a wound or a rotting margin dries to, the windows a beetle's
/// larvae graze, and a leaf-spot fungus's spots.
const NECROTIC: Vec3 = Vec3::new(0.048, 0.023, 0.009);
const GRAZED: Vec3 = Vec3::new(0.305, 0.216, 0.084);
const SPOTTED: Vec3 = Vec3::new(0.069, 0.032, 0.013);
/// A pad's underside: purple-red while it is young, browner as it ages.
const UNDER_YOUNG: Vec3 = Vec3::new(0.102, 0.013, 0.021);
const UNDER_OLD: Vec3 = Vec3::new(0.117, 0.065, 0.025);
const STALK: Vec3 = Vec3::new(0.117, 0.076, 0.027);
/// A sepal's olive outer face, the red-brown flushing its edges, and its
/// pale inner face.
const SEPAL: Vec3 = Vec3::new(0.076, 0.117, 0.027);
const FLUSH: Vec3 = Vec3::new(0.181, 0.048, 0.025);
const SEPAL_INNER: Vec3 = Vec3::new(0.746, 0.776, 0.604);
/// The green-yellow at a lily petal's base and the yellow at a crowfoot's,
/// the green on a petal's back, the cream it ages to and the brown its tip
/// dies to.
const PETAL_BASE: Vec3 = Vec3::new(0.672, 0.731, 0.352);
const CLAW: Vec3 = Vec3::new(0.871, 0.578, 0.045);
const PETAL_BACK: Vec3 = Vec3::new(0.617, 0.672, 0.503);
const CREAM: Vec3 = Vec3::new(0.839, 0.760, 0.552);
const PETAL_BROWN: Vec3 = Vec3::new(0.262, 0.138, 0.051);
/// What a stamen's anther deepens its yellow by, and the orange-brown an old
/// heart dulls to.
const ANTHER: Vec3 = Vec3::new(1.0, 0.78, 0.5);
const HEART_OLD: Vec3 = Vec3::new(0.434, 0.156, 0.021);

/// A pad's veins as they radiate from its stalk, forking twice on their way
/// out: each set's count round it, how far out it begins and how plain it
/// shows; and their half-width, in its radius.
const VEINS: [(f64, f64, f64); 3] = [(18.0, 0.02, 0.8), (36.0, 0.3, 0.55), (72.0, 0.62, 0.35)];
const VEIN: f64 = 0.005;
/// How far in from a growing pad's margin its red-brown flush reaches, and
/// how many cells of its tiny flecks span its radius.
const MARGIN_FLUSH: f64 = 0.018;
const FLECKS: f64 = 34.0;
/// The share of pads a lily beetle's larvae find, and how much of a grazed
/// patch their windows cover.
const GRAZED_SHARE: f64 = 0.4;
const GRAZED_COVER: f64 = 0.15;
/// How many of a leaf-spot fungus's cells span a pad's radius.
const SPOTS: f64 = 6.0;
/// The veins each side of a petal's midrib.
const PETAL_VEINS: f64 = 7.0;

/// Where on its part `spot` lies, in the part's own size, and how much of
/// it a pixel's view spans.
fn reckoned(spot: &Spot) -> ((f64, f64), f64) {
    let size = spot.girth.max(1e-4);
    ((spot.uv.0 / size, spot.uv.1 / size), spot.width / size)
}

fn pad(greens: &[Vec3; 4], spot: &Spot, key: u32) -> Vec3 {
    let pad = Pad::of(key);
    let (at, blur) = reckoned(spot);
    let (x, y) = at;
    let r = mathf::hypot(x, y);
    let (age, seed) = (pad.age, pad.seed);
    let tone = 0.86 + 0.26 * unit(mix32(key ^ 0x70e5));
    // No blade is one even green: it is mottled lighter and darker, yellower
    // and bluer by turns.
    let mottled = 1.0
        + 0.15 * noise2(x * 2.3, y * 2.3, seed ^ 0x7)
        + 0.07 * noise2(x * 9.0, y * 9.0, seed ^ 0x8) * (1.0 - smoothstep(0.02, 0.08, blur));
    let hue = 0.5 + 0.5 * noise2(x * 1.6 + 5.3, y * 1.6, seed ^ 0xa);
    let green = greens[(key & 3) as usize] * HUES[0].lerp(HUES[1], hue);
    let mut colour = green * (tone * mottled);
    colour = colour.lerp(BRONZE * tone, 0.7 * (1.0 - smoothstep(0.0, 0.2, age)));
    let vein = veins(at, blur, seed);
    // It yellows between its veins, its veins keeping their green longest:
    // one pad from its margin in, another in blotches, another all at once;
    // then it browns as it dies.
    let inward = 0.05 + 0.3 * unit(mix32(seed ^ 0xc));
    let blotched = 0.06 + 0.22 * unit(mix32(seed ^ 0xd));
    let late = age
        + inward * (r - 0.55)
        + blotched * noise2(x * 2.6, y * 2.6, seed)
        + 0.05 * noise2(x * 9.0, y * 9.0, seed ^ 0x9)
        - 0.1 * vein;
    colour = colour.lerp(YELLOWED * tone, smoothstep(0.62, 0.9, late));
    let dying = late + 0.08 * noise2(x * 7.0, y * 7.0, seed ^ 0x2);
    colour = colour.lerp(DEAD * tone, smoothstep(0.84, 1.02, dying));
    colour = if spot.front {
        let veined = colour * (1.0 + 0.1 * vein);
        let flecked = flecks(veined, at, blur, (seed, age));
        spotted(
            grazed(flecked, at, blur, (seed, age)),
            at,
            blur,
            (seed, age),
        )
    } else {
        colour.lerp(UNDER_YOUNG.lerp(UNDER_OLD, smoothstep(0.1, 0.6, age)), 0.7)
    };
    // Its margin dries as it ages, and every wound soon after it is made,
    // each yellowing a little way in; the margin runs into its sinus's slit,
    // not round its stalk.
    let edges = pad.edges(at);
    let (rim, scar) = (0.008 + 0.05 * age * age + blur, 0.006 + 0.035 * age + blur);
    let outward = smoothstep(0.04, 0.2, r);
    // A growing pad's margin is flushed red-brown.
    let flushed = (1.0 - smoothstep(0.0, MARGIN_FLUSH + blur, edges.margin))
        * outward
        * (1.0 - smoothstep(0.3, 0.6, age));
    colour = colour.lerp(UNDER_YOUNG * tone, 0.45 * flushed);
    let edge = outward * smoothstep(0.25, 0.7, age);
    let dried = ((1.0 - smoothstep(0.0, rim, edges.margin)) * edge)
        .max(1.0 - smoothstep(0.0, scar, edges.wound));
    let halo = ((1.0 - smoothstep(rim, 2.5 * rim, edges.margin)) * edge)
        .max(1.0 - smoothstep(scar, 2.5 * scar, edges.wound));
    colour
        .lerp(YELLOWED * tone, 0.5 * halo * smoothstep(0.2, 0.7, age))
        .lerp(NECROTIC * tone, dried)
}

/// `colour` flecked with the tiny dark spots a grown pad gathers where
/// insects fed and spores landed: a few on a young pad, more as it ages.
fn flecks(colour: Vec3, (x, y): (f64, f64), blur: f64, (seed, age): (u32, f64)) -> Vec3 {
    let shown = 1.0 - smoothstep(0.004, 0.015, blur);
    if shown <= 0.0 {
        return colour;
    }
    let found = cells2(x * FLECKS, y * FLECKS, seed ^ 0xf1ec, 0.9);
    if unit(found.id) > 0.02 + 0.12 * age {
        return colour;
    }
    let size = 0.06 + 0.1 * unit(mix32(found.id));
    let fleck = 1.0 - smoothstep(0.6 * size, size, found.nearest);
    colour.lerp(SPOTTED, shown * fleck)
}

/// Where a pad's veins run at `(x, y)`: `1.0` on a vein, settling to
/// nothing as a pixel's view spans `blur` of it.
fn veins((x, y): (f64, f64), blur: f64, seed: u32) -> f64 {
    let r = mathf::hypot(x, y);
    // Each vein wanders, more the farther it runs.
    let wander = (0.02 + 0.05 * r) * noise2(x * 4.0, y * 4.0, seed ^ 0x3);
    let turns = (mathf::atan2(y, x) + wander) / TAU;
    let mut vein: f64 = 0.0;
    for (set, (count, from, plain)) in VEINS.into_iter().enumerate() {
        let spacing = TAU * r / count;
        let shown = (1.0 - smoothstep(0.25, 0.7, blur / spacing.max(1e-9)))
            * smoothstep(from, from + 0.08, r);
        if shown <= 0.0 {
            continue;
        }
        // Each finer set runs between the last's, as each vein forks, and no
        // two run evenly spaced.
        let offset = if set == 0 { 0.0 } else { 0.5 };
        let (index, phase) = cell(turns * count + offset);
        let shift = 0.6 * (unit(mix32(index ^ seed ^ (0x51 << set))) - 0.5);
        let across = (phase - 0.5 - shift).abs() * spacing;
        let width = VEIN * (0.7 + 0.6 * unit(mix32(index ^ seed ^ 0x77)));
        vein = vein.max(plain * shown * (1.0 - smoothstep(width, width + blur, across)));
    }
    vein
}

/// `colour` grazed in ragged windows where a lily beetle's larvae ate the
/// upper skin away, on the pads they found and more as a pad ages; seen too
/// far off to make out, the windows settle to the share of a patch they
/// cover.
fn grazed(colour: Vec3, (x, y): (f64, f64), blur: f64, (seed, age): (u32, f64)) -> Vec3 {
    if unit(mix32(seed ^ 0x9a)) > GRAZED_SHARE {
        return colour;
    }
    let patch = smoothstep(0.1, 0.45, noise2(x * 1.7 + 3.1, y * 1.7, seed ^ 0x4))
        * smoothstep(0.15, 0.7, age);
    if patch <= 0.0 {
        return colour;
    }
    let gnawed = 0.65 * noise2(x * 7.0, y * 7.0, seed ^ 0x5)
        + 0.35 * noise2(x * 19.0, y * 19.0, seed ^ 0x55);
    let window = smoothstep(0.28, 0.4, gnawed);
    let window = window + (GRAZED_COVER - window) * smoothstep(0.004, 0.02, blur);
    colour.lerp(GRAZED, patch * window)
}

/// `colour` marked by a leaf-spot fungus's brown spots, each ringed yellow,
/// gathered where the spores took on a pad growing old.
fn spotted(colour: Vec3, (x, y): (f64, f64), blur: f64, (seed, age): (u32, f64)) -> Vec3 {
    let share = smoothstep(0.55, 1.0, age)
        * smoothstep(0.05, 0.4, noise2(x * 1.3 - 2.7, y * 1.3, seed ^ 0xb));
    if share <= 0.0 {
        return colour;
    }
    let found = cells2(x * SPOTS, y * SPOTS, seed ^ 0x6, 0.9);
    if unit(found.id) > 0.45 * share {
        return colour;
    }
    let size = 0.05 + 0.2 * power(unit(mix32(found.id)), 1.5);
    let soft = blur * SPOTS;
    let spot = 1.0 - smoothstep(0.7 * size, size + soft, found.nearest);
    let halo = 1.0 - smoothstep(size, 1.9 * size + soft, found.nearest);
    colour.lerp(YELLOWED, 0.55 * halo).lerp(SPOTTED, spot)
}

fn stalk(spot: &Spot, key: u32) -> Vec3 {
    let tone = 0.8 + 0.35 * unit(mix32(key ^ 0x57a1));
    // Faint streaks running down it.
    let streak = 0.06 * noise2(3.0 * spot.uv.1, 0.5 * spot.uv.0, key);
    STALK * (tone + streak)
}

fn sepal(spot: &Spot, key: u32) -> Vec3 {
    let sepal = Petal::of(key, Sheet::Sepal);
    let ((u, v), _) = reckoned(spot);
    let tone = 0.85 + 0.3 * unit(mix32(key ^ 0x05e9));
    let side = if v < 0.0 { -1.0 } else { 1.0 };
    let width = sepal.half_width(u.clamp(0.0, 1.0), side).max(1e-3);
    let edges = sepal.edges((u, v));
    let outward = 1.0 - (edges.margin / (0.5 * width)).clamp(0.0, 1.0);
    let colour = if spot.front {
        // A red-brown flush toward its edges and its tip.
        let flushed = smoothstep(
            0.3,
            1.0,
            outward + 0.3 * u + 0.25 * noise2(u * 6.0, v * 18.0, sepal.seed),
        );
        (SEPAL * tone).lerp(FLUSH * tone, 0.6 * flushed)
    } else {
        SEPAL_INNER.lerp(
            SEPAL * tone,
            0.35 * outward + 0.3 * (1.0 - smoothstep(0.0, 0.3, u)),
        )
    };
    colour.lerp(
        PETAL_BROWN * 0.6,
        smoothstep(0.6, 1.0, sepal.age + 0.3 * (u - 0.7)),
    )
}

fn petal(whites: &[Vec3; 4], spot: &Spot, (key, sheet): (u32, Sheet)) -> Vec3 {
    let petal = Petal::of(key, sheet);
    let ((u, v), blur) = reckoned(spot);
    let age = petal.age;
    let tone = 0.95 + 0.07 * unit(mix32(key ^ 0x1e));
    let mut colour = whites[(key & 3) as usize] * tone;
    // A lily's petal greens faintly where it springs from the flower; a
    // crowfoot's is yellow there, where its nectar lies.
    let (base, share, reach) = if sheet == Sheet::Broad {
        (CLAW, 0.85, 0.2)
    } else {
        (PETAL_BASE, 0.5, 0.3)
    };
    colour = colour.lerp(base, share * (1.0 - smoothstep(0.0, reach, u)));
    if !spot.front {
        colour = colour.lerp(PETAL_BACK, 0.3 * (1.0 - smoothstep(0.1, 0.8, u)));
    }
    // Fine veins run its length, each keeping its share of its width, none
    // evenly spaced, fading out toward its tip.
    let side = if v < 0.0 { -1.0 } else { 1.0 };
    let width = petal.half_width(u.clamp(0.0, 1.0), side).max(1e-3);
    let spacing = width / PETAL_VEINS;
    let (index, phase) = cell(v / spacing + 0.5 + 0.15 * noise2(u * 4.0, v * 9.0, petal.seed));
    let shift = 0.5 * (unit(mix32(index ^ petal.seed)) - 0.5);
    let vein = (1.0 - smoothstep(0.25, 0.7, blur / spacing))
        * (1.0 - smoothstep(0.08, 0.2, 2.0 * (phase - 0.5 - shift).abs()))
        * (1.0 - smoothstep(0.6, 1.0, u + 0.3 * unit(mix32(index ^ petal.seed ^ 0x3))));
    colour = colour * (1.0 - 0.05 * vein);
    // It creams as it ages, then browns from its tip, its edges and its
    // wounds.
    colour = colour.lerp(CREAM * tone, smoothstep(0.35, 0.8, age));
    let edges = petal.edges((u, v));
    let tip = smoothstep(
        0.62,
        1.0,
        age + 0.4 * (u - 0.75) + 0.15 * noise2(u * 5.0, v * 14.0, petal.seed),
    );
    let rim = 0.01 + 0.05 * smoothstep(0.5, 1.0, age) + blur;
    let edge = (1.0 - smoothstep(0.0, rim, edges.margin)) * smoothstep(0.45, 0.9, age);
    let wound =
        (1.0 - smoothstep(0.0, 0.012 + 0.03 * age + blur, edges.wound)) * smoothstep(0.2, 0.6, age);
    colour.lerp(PETAL_BROWN, tip.max(edge).max(wound))
}

fn heart(yellows: &[Vec3; 4], spot: &Spot, key: u32) -> Vec3 {
    let tone = 0.88 + 0.22 * unit(mix32(key ^ 0x4ea7));
    let colour = yellows[(key & 3) as usize] * tone;
    // A stamen's anther, its upper half, a deeper gold than the filament
    // beneath it, as the stigma's rays are toward its rim.
    let ((out, _), _) = reckoned(spot);
    let colour = colour.lerp(colour * ANTHER, smoothstep(0.45, 0.6, out));
    colour.lerp(HEART_OLD * tone, smoothstep(0.5, 1.0, age(key)))
}

#[cfg(test)]
#[path = "lily_tests.rs"]
mod tests;
