//! Straw and hay as a farm binds it: an oblong bale's stalks pressed along
//! its length in flakes, its cut ends, and the twine about it; a round
//! bale's layers rolled about its axis and the net wrapped round its side; a
//! sheaf's stalks standing up to its ears, tied at its waist; and a hay
//! cock's tangle.
//!
//! The stalks are the roughest face a farm leaves, so they stand in true
//! relief on the solid they bind ([`crate::solid`]): each stalk crested where
//! it lies, bundled and parted in shadowed gaps, its cut ends bristling out
//! of an end — and the same stalks colour it, each its own shade. A stalk
//! fades from the relief as it comes to span too few pixels, and from the
//! colour to the stalks' mean, so far off a bale is its pressed form.

use core::f64::consts::TAU;

use tairix_util::mathf;

use crate::ground::{coverage, fade};
use crate::noise::{cell, noise3, smoothstep, NOISE_SLOPE};
use crate::pigment::Spot;
use crate::sample::{mix32, unit};
use crate::vector::Vec3;

/// Straw or hay bound one way: its stalks' two shades, the twine or net
/// that binds it, how it is bound, the half extents of the solids it binds,
/// and its seed.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Straw {
    pub(crate) stalks: [Vec3; 2],
    pub(crate) binding: Vec3,
    pub(crate) bound: Bound,
    pub(crate) half: Vec3,
    pub(crate) seed: u32,
}

/// How straw is bound.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Bound {
    /// An oblong bale, its stalks along its `x`, `twines` strings round it.
    Oblong { twines: u8 },
    /// A round bale, its layers rolled about its `y` and a net round them.
    Rolled,
    /// A sheaf standing up its `y`, its ears at its head this colour.
    Sheaf { ears: Vec3 },
    /// A hay cock heaped about its `y`.
    Cock,
}

/// How far a straw unit's stalks stand proud of its pressed body at most,
/// and its gaps sink into it.
pub(crate) const BRISTLE: f64 = 0.045;

/// How broad one stalk lies and how far one runs before another takes over;
/// how broad a bundle of them lies and how long, and a clump of bundles;
/// how broad and long the gaps between bundles part; and how far the stalks
/// wander off their line, over how far.
pub(crate) const STALK: f64 = 0.005;
const STALK_RUN: f64 = 0.12;
pub(crate) const BROAD: f64 = 0.012;
const LONG: f64 = 0.03;
const CLUMP: (f64, f64) = (0.05, 0.12);
const GAP_BROAD: f64 = 0.016;
const GAP_LONG: f64 = 0.028;
const WARP: (f64, f64) = (0.012, 0.04);
/// The window of the noise a gap opens across: low, so gaps open over much
/// of every face.
const GAPPING: (f64, f64) = (-0.1, 0.7);
/// How much rougher its stalks lie about an arris, where they are least
/// held.
const FRAYED: f64 = 0.5;
/// How thick each turn of a round bale's rolled strip lies; how broad the
/// seam between two turns; how far, as a share of a turn, its edge wanders,
/// over how far round; and how deep, as a share of [`BRISTLE`], each seam
/// sinks.
const LAYER: f64 = 0.05;
const LAYER_SEAM: f64 = 0.3 * LAYER;
const RAGGED: f64 = 0.3;
const LAYER_WANDER: f64 = 0.08;
const ROLLED_SEAM: f64 = 0.9;
/// How far out from a round bale's axis its turns first part: within, it
/// was rolled too tight for a seam to open.
const CORE: f64 = 1.5 * LAYER;
/// How thick a flake a baler presses, and how broad the seam between two;
/// how far, as a share of a flake, its thickness wanders; how broad a bale's
/// twine runs; and how far apart its net's strands, and how broad each.
const FLAKE: f64 = 0.09;
const SEAM: f64 = 0.006;
const FLAKE_WANDER: f64 = 0.45;
pub(crate) const TWINE: f64 = 0.006;
/// How broad a trough a bale's twine presses into its straw.
const PRESSED: f64 = 2.5 * TWINE;
const MESH: f64 = 0.06;
const STRAND: f64 = 0.0025;
/// The mean shade of pressed stalks; and, as their noise falls, where a
/// stalk's crest stands on the whole, the higher of two crossing, and how far
/// a gap parts the bundles, which their shades are centred on so a bale far
/// off is as light as near.
const STALKS_MEAN: f64 = 1.03;
const CREST_MEAN: f64 = 0.55;
const CROSSED_MEAN: f64 = 0.77;
const GAP_MEAN: f64 = 0.17;
/// How deep a round bale's end lies in its seams on the whole: a groove
/// spans a breadth and a half of its floor either side of its line, and
/// falls to its lips as a parabola does, two thirds of the way.
const SEAM_MEAN: f64 = 2.0 * 1.5 * LAYER_SEAM * 2.0 / 3.0 / LAYER;
const _: () = assert!(
    1.5 * LAYER_SEAM <= 0.5 * LAYER,
    "a seam's groove reaches the next turn's"
);
/// The power a unit's faces are weighed at by how near each a place lies:
/// high, so a face's relief holds until near its arris, where it turns to
/// the next's; and the share of the nearest face's weight below which a face
/// has no say.
const SHARPNESS: f64 = 8.0;
const UNHEARD: f64 = 0.01;

/// How much of a straw unit's relief a pixel resolves, each `0.0..=1.0`: its
/// single stalks, the bundles they lie in, and the clumps of bundles.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Shows {
    pub(crate) stalks: f64,
    pub(crate) bundles: f64,
    pub(crate) clumps: f64,
}

impl Shows {
    pub(crate) const ALL: Self = Self {
        stalks: 1.0,
        bundles: 1.0,
        clumps: 1.0,
    };
    pub(crate) const NONE: Self = Self {
        stalks: 0.0,
        bundles: 0.0,
        clumps: 0.0,
    };

    /// How much shows where a pixel spans `footprint` metres, each feature
    /// showing from the first of `spans` pixels across and in full from the
    /// second.
    pub(crate) fn at(footprint: f64, spans: (f64, f64)) -> Self {
        let shows = |size: f64| smoothstep(spans.0, spans.1, size / footprint.max(1e-12));
        Self {
            stalks: shows(STALK),
            bundles: shows(BROAD),
            clumps: shows(CLUMP.0),
        }
    }

    /// Whether any of it shows.
    pub(crate) fn any(&self) -> bool {
        self.stalks > 0.0 || self.bundles > 0.0 || self.clumps > 0.0
    }
}

/// What the stalks are like where they lie: each stalk's crest, `-1.0` in
/// the groove between two to `1.0` along its top; how its bundle and the
/// clump of bundles it lies in swell, each roughly `-1.0..=1.0`; how far a
/// gap parts the bundles, `0.0..=1.0`; and where the crest and the gap stand
/// on the whole, lying as they do.
struct Fibres {
    crest: f64,
    bundle: f64,
    clump: f64,
    gap: f64,
    means: (f64, f64),
}

/// What `look` finds where `weight` lets it show; nought, unlooked for,
/// where it does not.
fn shown(weight: f64, look: impl FnOnce() -> f64) -> f64 {
    if weight > 0.0 {
        look()
    } else {
        0.0
    }
}

/// How far stalks stand proud of a unit's body, as shares of [`BRISTLE`]
/// for each stalk's crest, its bundle, its clump and a gap, and the share
/// they stand out on the whole; at most [`BRISTLE`] once frayed about an
/// arris.
const CRESTS: f64 = 0.16;
const BUNDLES: f64 = 0.2;
const CLUMPS: f64 = 0.17;
const GAPS: f64 = 0.5;
const STANDING: f64 = 0.1;
/// How deep, as a share of [`BRISTLE`], its twine presses its trough, and
/// how far its flakes' seams lie open at most.
const GROOVED: f64 = 0.4;
const SEAMED: f64 = 0.15;
/// The turn askew of the rest the stalks laid across them lie at: the cosine
/// and sine of half a radian.
const ASKEW: (f64, f64) = (0.877_582_561_890_372_8, 0.479_425_538_604_203);

const _: () = assert!((CRESTS + BUNDLES + CLUMPS + STANDING) * (1.0 + FRAYED) <= 1.0);

/// Where on a straw unit a place lies, in its stalks' own terms, in metres.
#[derive(Copy, Clone, Debug)]
enum Lying {
    /// Among stalks laid one way: how far across them and along them, each
    /// a line `(at, 0.0)` or a place `(x, z)` on the circle its stalks wind
    /// round; and whether both are lines, where some stalks lie askew across
    /// the rest.
    Laid {
        across: (f64, f64),
        along: (f64, f64),
        straight: bool,
    },
    /// Among cut stalk ends standing out of a face, at `(x, y)` on it.
    Ends(f64, f64),
    /// Among a sheaf's ears, or a cock's tangle, at this point of it.
    Heaped(Vec3),
}

impl Straw {
    /// The extents its faces are measured against on a unit `half` its size
    /// each way: an oblong bale's own, a round unit's radius about its `y`
    /// either way across, its `z` naming a turned unit's head.
    fn spans(&self, half: Vec3) -> Vec3 {
        match self.bound {
            Bound::Oblong { .. } => half,
            Bound::Rolled | Bound::Sheaf { .. } | Bound::Cock => Vec3::new(half.x, half.y, half.x),
        }
    }

    /// Where on a unit it binds, `half` its size each way, its local point
    /// `q` lies: which face — an oblong bale's `0.0` across its `x`, `1.0` its
    /// `y` and `2.0` its `z`; a round unit's `1.0` an end and `2.0` its side —
    /// where on it, and its girth there. On a face its place is a share of
    /// its extents and it has no girth; round a side, as round a limb, its
    /// place is how far up the axis and its angle about it, and its girth its
    /// distance out from the axis.
    pub(crate) fn surface(&self, q: Vec3, half: Vec3) -> (f64, (f64, f64), f64) {
        let half = self.spans(half);
        let (x, y, z) = (q.x / half.x, q.y / half.y, q.z / half.z);
        match self.bound {
            Bound::Oblong { .. } => {
                let (face, uv) = if x.abs() >= y.abs() && x.abs() >= z.abs() {
                    (0.0, (z, y))
                } else if y.abs() >= z.abs() {
                    (1.0, (x, z))
                } else {
                    (2.0, (x, y))
                };
                (face, uv, 0.0)
            }
            Bound::Rolled | Bound::Sheaf { .. } | Bound::Cock => {
                if y.abs() >= mathf::hypot(x, z) {
                    (1.0, (x, z), 0.0)
                } else {
                    (2.0, (q.y, mathf::atan2(q.z, q.x)), mathf::hypot(q.x, q.z))
                }
            }
        }
    }

    /// Its colour at `spot`, at the place on its unit its `along`, `uv` and
    /// `girth` give as [`Self::surface`] has them.
    pub(crate) fn colour(&self, spot: &Spot) -> Vec3 {
        let (face, uv) = (spot.along, spot.uv);
        let width = spot.width.max(1e-5);
        let shade = 0.88 + 0.24 * unit(mix32(spot.mark ^ self.seed));
        let base = self.stalks[0].lerp(self.stalks[1], 0.5) * shade;
        let lying = self.lying(face, uv, spot.girth);
        let shading = self.shade(lying, width);
        let stalks = base * shading;
        let half = self.half;
        match self.bound {
            // Its strings loop round it lengthwise, over its top and its
            // ends, and never cross its sides.
            Bound::Oblong { twines } => {
                let (u, v) = uv;
                let flaked = if face < 0.5 {
                    stalks
                } else {
                    stalks * self.flaked(u * half.x, width)
                };
                let across = if face < 0.5 { u * half.z } else { v * half.z };
                let tied = if face < 1.5 {
                    self.twined(across, twines, width)
                } else {
                    0.0
                };
                flaked.lerp(self.binding, tied)
            }
            Bound::Rolled if face < 1.5 => stalks * self.layered(lying, width),
            Bound::Rolled => stalks.lerp(self.binding, self.netted(uv, width)),
            Bound::Sheaf { ears } => {
                if face < 1.5 {
                    return ears * shading;
                }
                let up = uv.0;
                let headed = stalks.lerp(ears * shade, smoothstep(0.55, 0.75, up / half.y));
                let waist = coverage((up - 0.05 * half.y).abs(), TWINE, width);
                headed.lerp(self.binding, waist)
            }
            Bound::Cock => stalks,
        }
    }

    /// How far the stalks stand proud of the pressed body of a unit `half`
    /// its size each way at its local point `q`, as much of its single
    /// stalks and of its bundles as `shows` has: crested where each stalk
    /// lies and parted in gaps between bundles, its twine pressing grooves
    /// and its flakes' seams lying open, each face's own and turning from
    /// one face's to the next across its arrises.
    pub(crate) fn proud(&self, q: Vec3, half: Vec3, shows: Shows) -> f64 {
        let half = self.spans(half);
        let reach = |at: f64, extent: f64| sharpened((at / extent).abs());
        // Each face's place on it as [`Self::surface`] reads it, and how much
        // it weighs here.
        let faces: [(f64, (f64, f64), f64, f64); 3] = match self.bound {
            Bound::Oblong { .. } => [
                (0.0, (q.z / half.z, q.y / half.y), 0.0, reach(q.x, half.x)),
                (1.0, (q.x / half.x, q.z / half.z), 0.0, reach(q.y, half.y)),
                (2.0, (q.x / half.x, q.y / half.y), 0.0, reach(q.z, half.z)),
            ],
            Bound::Rolled | Bound::Sheaf { .. } | Bound::Cock => {
                let girth = mathf::hypot(q.x, q.z);
                [
                    (1.0, (q.x / half.x, q.z / half.z), 0.0, reach(q.y, half.y)),
                    (
                        2.0,
                        (q.y, mathf::atan2(q.z, q.x)),
                        girth,
                        reach(girth, half.x),
                    ),
                    (2.0, (0.0, 0.0), 0.0, 0.0),
                ]
            }
        };
        let most = faces.iter().map(|&(.., weight)| weight).fold(0.0, f64::max);
        let (mut raised, mut weighed, mut every) = (0.0, 0.0, 0.0);
        for &(face, uv, girth, weight) in &faces {
            every += weight;
            // A face weighed far below the nearest has no say worth looking
            // up, its say fading out rather than cut off so the relief stays
            // continuous.
            let say = weight - UNHEARD * most;
            if say > 0.0 {
                raised += say * self.raised((face, uv, girth), shows);
                weighed += say;
            }
        }
        if weighed <= 0.0 {
            return 0.0;
        }
        // Where two faces share the say, the place lies about an arris.
        let frayed = 1.0 + FRAYED * (2.0 * (1.0 - most / every)).min(1.0);
        frayed * raised / weighed
    }

    /// The steepest [`Self::proud`] rises, a metre to a metre, as much
    /// showing as `shows` has.
    pub(crate) fn steepest(&self, shows: Shows) -> f64 {
        let warped = 1.0 + WARP.0 * NOISE_SLOPE / WARP.1;
        let crest = CRESTS * BRISTLE * 2.0 * NOISE_SLOPE / STALK * shows.stalks;
        let bundle = BUNDLES * BRISTLE * NOISE_SLOPE / BROAD;
        let clump = CLUMPS * BRISTLE * NOISE_SLOPE / CLUMP.0 * shows.clumps;
        // A smoothstep rises at most half as steeply again as a straight
        // line across its window.
        let gap = GAPS * BRISTLE * 1.5 / (GAPPING.1 - GAPPING.0) * NOISE_SLOPE / GAP_BROAD;
        // A face's relief turns to the next's, and frays, within an arris's
        // breadth.
        let spans = self.spans(self.half);
        let least = spans.x.min(spans.y).min(spans.z);
        let turning = (2.0 + FRAYED) * BRISTLE * SHARPNESS / least;
        let most = shows.stalks.max(shows.bundles).max(shows.clumps);
        // Only the stalks and their bundles wander off their line.
        let laid = warped * (crest + bundle * shows.bundles) + gap * shows.bundles + clump;
        (1.0 + FRAYED) * (laid + self.grooves(shows)) + turning * most
    }

    /// The steepest its binding presses grooves into it, or its rolling
    /// parts it in seams, a metre to a metre, as much showing as `shows` has.
    fn grooves(&self, shows: Shows) -> f64 {
        // A groove's sides rise twice its depth across a breadth and a half
        // of its floor.
        let sides = |depth: f64, broad: f64| 2.0 * depth * BRISTLE / (1.5 * broad);
        match self.bound {
            // Its twine's trough may cross a flake's seam, which wanders as
            // its flakes' thickness does.
            Bound::Oblong { .. } => {
                (sides(GROOVED, PRESSED) + sides(SEAMED, SEAM) * (1.0 + FLAKE_WANDER * NOISE_SLOPE))
                    * shows.bundles
            }
            // A seam winds round tighter toward the core, fading out before
            // it would wind too tight, and its edge wanders.
            Bound::Rolled => {
                let winding = mathf::hypot(1.0, LAYER / (TAU * 0.5 * CORE))
                    + RAGGED * LAYER * NOISE_SLOPE / LAYER_WANDER;
                let fading = ROLLED_SEAM * BRISTLE * 1.5 / (0.5 * CORE);
                (sides(ROLLED_SEAM, LAYER_SEAM) * winding + fading) * shows.clumps
            }
            Bound::Sheaf { .. } | Bound::Cock => 0.0,
        }
    }

    /// How far stalks stand proud at `uv` on face `face`, `girth` out from a
    /// round unit's axis, as much shown as `shows` has.
    fn raised(&self, (face, uv, girth): (f64, (f64, f64), f64), shows: Shows) -> f64 {
        let lying = self.lying(face, uv, girth);
        let fibres = self.fibres(lying, shows);
        let bundles = shows.bundles;
        let swelling = (BUNDLES * fibres.bundle - GAPS * fibres.gap) * bundles
            + (CLUMPS * fibres.clump + STANDING) * shows.clumps;
        let mut proud = BRISTLE * (CRESTS * fibres.crest * shows.stalks + swelling);
        let half = self.half;
        match self.bound {
            Bound::Oblong { twines } => {
                let (u, v) = uv;
                if face > 0.5 {
                    let (seam, open) = self.flake_seam(u * half.x);
                    proud -= SEAMED * BRISTLE * open * groove(seam, SEAM) * bundles;
                }
                if face < 1.5 {
                    // Its twine presses the straw down in a broad trough.
                    let across = if face < 0.5 { u * half.z } else { v * half.z };
                    proud -= GROOVED
                        * BRISTLE
                        * groove(self.twine_off(across, twines), PRESSED)
                        * bundles;
                }
            }
            // Each turn of its rolled strip parts from the next in a deep seam.
            Bound::Rolled if face < 1.5 => {
                let (seam, _) = self.rolled(lying);
                proud -= ROLLED_SEAM * BRISTLE * seam * shows.clumps;
            }
            Bound::Rolled | Bound::Sheaf { .. } | Bound::Cock => {}
        }
        proud
    }

    /// Where on face `face`, at `uv` on it and `girth` out from a round
    /// unit's axis as [`Self::surface`] has them, the stalks lie.
    fn lying(&self, face: f64, (u, v): (f64, f64), girth: f64) -> Lying {
        let half = self.spans(self.half);
        // Round a side, as round a limb: how far up the axis, and the angle
        // about it.
        let round = || (girth * mathf::cos(v), girth * mathf::sin(v));
        match self.bound {
            Bound::Oblong { .. } if face < 0.5 => Lying::Ends(u * half.z, v * half.y),
            Bound::Oblong { .. } => Lying::Laid {
                across: (v * if face < 1.5 { half.z } else { half.y }, 0.0),
                along: (u * half.x, 0.0),
                straight: true,
            },
            Bound::Rolled if face < 1.5 => {
                let (x, z) = (u * half.x, v * half.z);
                Lying::Laid {
                    across: (mathf::hypot(x, z), 0.0),
                    along: (x, z),
                    straight: false,
                }
            }
            Bound::Rolled => Lying::Laid {
                across: (u, 0.0),
                along: round(),
                straight: false,
            },
            Bound::Sheaf { .. } if face < 1.5 => {
                Lying::Heaped(Vec3::new(u * half.x, 0.0, v * half.z))
            }
            Bound::Sheaf { .. } => Lying::Laid {
                across: round(),
                along: (u, 0.0),
                straight: false,
            },
            Bound::Cock if face < 1.5 => Lying::Heaped(Vec3::new(u * half.x, half.y, v * half.z)),
            Bound::Cock => {
                let (x, z) = round();
                Lying::Heaped(Vec3::new(x, u, z))
            }
        }
    }

    /// What the stalks are like where `lying` has them, each feature looked
    /// up only where `shows` lets it show, and nought where it does not: its
    /// crest by the stalks, its bundle and gap by the bundles, its clump by
    /// the clumps.
    fn fibres(&self, lying: Lying, shows: Shows) -> Fibres {
        let seed = self.seed;
        match lying {
            Lying::Laid {
                across,
                along,
                straight,
            } => {
                // Across them and along them, each a line or a circle, at the
                // scales given: never more than three ways at once.
                let at = |across: (f64, f64), (by_across, by_along): (f64, f64)| {
                    Vec3::new(
                        across.0 / by_across,
                        across.1 / by_across + along.1 / by_along,
                        along.0 / by_along,
                    )
                };
                // Stalks wander off their line, and where laid straight some
                // lie askew across the rest.
                let wandered = if shows.stalks > 0.0 || shows.bundles > 0.0 {
                    let wander = WARP.0 * noise3(at(across, (WARP.1, 1.6 * WARP.1)), seed ^ 0x51);
                    (across.0 + wander, across.1)
                } else {
                    across
                };
                let crested = |across: (f64, f64), along: (f64, f64)| {
                    let at = Vec3::new(
                        across.0 / STALK,
                        across.1 / STALK + along.1 / STALK_RUN,
                        along.0 / STALK_RUN,
                    );
                    1.0 - 2.0 * noise3(at, seed ^ 0x3a).abs()
                };
                let crest = shown(shows.stalks, || {
                    let crossed = if straight {
                        let (cos, sin) = ASKEW;
                        let turned = (
                            wandered.0 * cos - along.0 * sin,
                            wandered.0 * sin + along.0 * cos,
                        );
                        crested((turned.0, 0.0), (turned.1, 0.0))
                    } else {
                        -1.0
                    };
                    crested(wandered, along).max(crossed)
                });
                Fibres {
                    crest,
                    bundle: shown(shows.bundles, || {
                        noise3(at(wandered, (BROAD, LONG)), seed ^ 0x3e)
                    }),
                    clump: shown(shows.clumps, || noise3(at(across, CLUMP), seed ^ 0x3b)),
                    gap: shown(shows.bundles, || {
                        smoothstep(
                            GAPPING.0,
                            GAPPING.1,
                            noise3(at(across, (GAP_BROAD, GAP_LONG)), seed ^ 0x3f),
                        )
                    }),
                    means: (if straight { CROSSED_MEAN } else { CREST_MEAN }, GAP_MEAN),
                }
            }
            Lying::Ends(x, y) => Fibres {
                crest: shown(shows.stalks, || {
                    1.0 - 2.0 * noise3(Vec3::new(x / STALK, y / STALK, 0.5), seed ^ 0x3c).abs()
                }),
                bundle: shown(shows.bundles, || {
                    noise3(Vec3::new(x / BROAD, y / BROAD, 0.5), seed ^ 0x3d)
                }),
                clump: shown(shows.clumps, || {
                    noise3(Vec3::new(x / CLUMP.0, y / CLUMP.0, 0.5), seed ^ 0x3b)
                }),
                gap: 0.0,
                means: (CREST_MEAN, 0.0),
            },
            Lying::Heaped(p) => Fibres {
                crest: shown(shows.stalks, || {
                    1.0 - 2.0 * noise3(p * (1.0 / (1.6 * STALK)), seed ^ 0x4b).abs()
                }),
                bundle: shown(shows.bundles, || {
                    noise3(p * (1.0 / (2.0 * BROAD)), seed ^ 0x4c)
                }),
                clump: shown(shows.clumps, || noise3(p * (1.0 / CLUMP.0), seed ^ 0x4e)),
                gap: shown(shows.bundles, || {
                    smoothstep(
                        GAPPING.0,
                        GAPPING.1,
                        noise3(p * (1.0 / GAP_BROAD), seed ^ 0x4d),
                    )
                }),
                means: (CREST_MEAN, GAP_MEAN),
            },
        }
    }

    /// The shade of stalks lying as `lying` has them, a footprint `width`
    /// across: each stalk its own, each bundle and clump its own, the gaps
    /// between them in shadow, settling to their mean as a pixel comes to
    /// span them.
    fn shade(&self, lying: Lying, width: f64) -> f64 {
        let faded = |size: f64| fade(1.0, width / size);
        let shows = Shows {
            stalks: faded(STALK),
            bundles: faded(BROAD),
            clumps: faded(CLUMP.0),
        };
        let fibres = self.fibres(lying, shows);
        let (crest, gap) = fibres.means;
        let stalks = 0.22 * (fibres.crest - crest) * shows.stalks;
        let bundles = (0.3 * fibres.bundle - 0.45 * (fibres.gap - gap)) * shows.bundles;
        let clumps = 0.18 * fibres.clump * shows.clumps;
        STALKS_MEAN * (1.0 + stalks + bundles + clumps)
    }

    /// Where a place on a round bale's end lies in the spiral it was rolled
    /// in: how far into the seam between two turns of it, `0.0..=1.0`, and
    /// how far along the rolled strip, in turns, so the strip is shaded as it
    /// runs and never where the angle round it wraps.
    fn rolled(&self, lying: Lying) -> (f64, f64) {
        let Lying::Laid { across, along, .. } = lying else {
            return (0.0, 0.0);
        };
        let turn = mathf::atan2(along.1, along.0) / TAU;
        // Each turn laid on unevenly, its edge wandering in and out.
        let ragged = RAGGED
            * noise3(
                Vec3::new(along.0 / LAYER_WANDER, along.1 / LAYER_WANDER, 0.5),
                self.seed ^ 0x61,
            );
        let wound = across.0 / LAYER + turn + ragged;
        let parted = smoothstep(0.5 * CORE, CORE, across.0);
        let seam = groove(LAYER * (wound - mathf::round(wound)), LAYER_SEAM) * parted;
        (seam, turn - mathf::floor(wound))
    }

    /// The shade of a round bale's end from its rolling: each turn of its
    /// strip its own shade as it runs, darkening deep into the seams between
    /// turns, a footprint `width` across, as dark on the whole far off.
    fn layered(&self, lying: Lying, width: f64) -> f64 {
        let (seam, strip) = self.rolled(lying);
        let own = fade(
            0.18 * noise3(Vec3::new(strip * 1.5, 0.5, 0.5), self.seed ^ 0x62),
            width / LAYER,
        );
        let seamed = SEAM_MEAN + fade(seam - SEAM_MEAN, width / LAYER_SEAM);
        (1.0 + own) * (1.0 - 0.75 * seamed)
    }

    /// How far `along` a bale's length lies from the nearest seam between
    /// the flakes its baler pressed it in, each flake its own thickness, and
    /// how far open that seam lies, `0.0..=1.0`.
    fn flake_seam(&self, along: f64) -> (f64, f64) {
        let flake = along / FLAKE
            + FLAKE_WANDER * noise3(Vec3::new(along / FLAKE, 0.5, 0.5), self.seed ^ 0x3d);
        let nearest = mathf::round(flake);
        // Most seams lie shut, a few open.
        let draw = unit(mix32(cell(nearest + 0.5).0 ^ self.seed ^ 0x3e));
        (FLAKE * (flake - nearest).abs(), draw * draw * draw)
    }

    /// The shade `along` a bale's length of its flakes and the seams between
    /// them, a footprint `width` across.
    fn flaked(&self, along: f64, width: f64) -> f64 {
        let (off, open) = self.flake_seam(along);
        let seam = coverage(off, 0.5 * SEAM, width) * open;
        let own = fade(
            0.1 * (unit(mix32(cell(along / FLAKE).0 ^ self.seed)) - 0.5),
            width / FLAKE,
        );
        (1.0 + own) * (1.0 - 0.45 * seam)
    }

    /// How far `across` an oblong bale's width from its middle lies from the
    /// nearest of its `twines` strings.
    fn twine_off(&self, across: f64, twines: u8) -> f64 {
        (0..twines).fold(f64::INFINITY, |nearest: f64, index| {
            nearest.min((across - self.half.z * tied_at(index, twines)).abs())
        })
    }

    /// How much of a footprint `width` across, `across` an oblong bale's
    /// width from its middle, its `twines` strings cover.
    fn twined(&self, across: f64, twines: u8, width: f64) -> f64 {
        coverage(self.twine_off(across, twines), 0.5 * TWINE, width)
    }

    /// How much of a footprint `width` across, `up` its axis and `angle`
    /// round it on a round bale's side, its net covers: two sets of strands
    /// crossing.
    fn netted(&self, (up, angle): (f64, f64), width: f64) -> f64 {
        // As many whole meshes round it as come nearest, so the net meets
        // itself where it was joined.
        let girth = TAU * self.half.x;
        let mesh = girth / mathf::round(girth / MESH).max(1.0);
        let round = angle * self.half.x;
        let strand = |at: f64| {
            let nearest = mathf::round(at / mesh) * mesh;
            coverage((at - nearest).abs(), 0.5 * STRAND, width)
        };
        strand(up + round).max(strand(up - round))
    }
}

/// Where the `index`th of an oblong bale's `twines` strings runs round it,
/// across its width as a share of its half width from its middle.
pub(crate) fn tied_at(index: u8, twines: u8) -> f64 {
    ((f64::from(index) + 0.5) / f64::from(twines.max(1)) * 2.0 - 1.0) * 0.8
}

/// `share` raised to [`SHARPNESS`].
fn sharpened(share: f64) -> f64 {
    let squared = share * share;
    let fourth = squared * squared;
    fourth * fourth
}

/// How deep a groove `off` from its line lies, `0.0..=1.0`, its floor
/// `broad` across, rounding up to its lips.
fn groove(off: f64, broad: f64) -> f64 {
    let within = off / (1.5 * broad);
    (1.0 - within * within).max(0.0)
}

#[cfg(test)]
#[path = "straw_tests.rs"]
mod tests;
