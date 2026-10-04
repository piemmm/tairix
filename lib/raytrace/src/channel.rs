//! A river's channel along its course: the pools and riffles its bed runs
//! through, the ledges its rock steps down at, the bars it builds and the
//! banks it cuts, and how its water stands and runs in it.
//!
//! A river's course is counted in units of pool and riffle, five to seven
//! widths apart (Leopold and Wolman, 1957; Keller and Melhorn, 1978): each
//! begins at a crest its bed rises to, falls quickly down the riffle below
//! it and barely over the pool after, so the bed's long profile is a
//! staircase and at low flow the water's follows it, each pool ponded
//! behind the crest below. Where bedded rock holds a reach, the fall of two
//! or three units gathers at one ledge over a plunge pool. In a pool the
//! deepest line swings to one bank — alternately along a straight reach, to
//! the outside of a bend — cutting that bank steep while a bar rises gently
//! on the other, where slack water drops its sand. The water carries as
//! much past every place along its deepest line as over the crest, so it
//! runs fast where shallow and slow where deep, and across the channel it
//! runs as Manning's law has it at each depth.
//!
//! Every place's channel is drawn from where it lies along its course and
//! the land's seed alone, so whatever reads it — the land's grids, its
//! water, a stream's flow — reads the same channel.

use alloc::vec::Vec;
use core::f64::consts::PI;

use tairix_util::{fallible, mathf};

use crate::course::{self, Courses, Mark, Nearest};
use crate::noise::{cell, hash2, noise2, smoothstep};
use crate::sample::{mix32, unit};
use crate::vector::{power, real};

/// Gravity in metres a second squared.
pub(crate) const GRAVITY: f64 = 9.81;

/// Manning's roughness of a bed of cobbles and gravel.
const MANNING: f64 = 0.04;

/// The swiftest a stream runs against the speed of a long wave in it: a
/// stony stream works its bed until it runs just below critical (Grant,
/// 1997).
const MOST_FROUDE: f64 = 0.9;

/// The gentlest a river's water is taken to fall as it runs, so a level
/// reach still runs.
const LEAST_FALL: f64 = 5e-4;

/// How far apart a river's pools lie, in its widths, how far that wanders
/// either way, and the narrowest river whose spacing still narrows with it;
/// and the fall at which its units shorten to half as long, a steep stream
/// stepping down every width or two rather than in long pools.
const SPACING: f64 = 6.0;
const JITTER: f64 = 0.35;
const LEAST_WIDTH: f64 = 2.0;
const STEPPED_FALL: f64 = 0.02;

/// The units of pool and riffle a group spans, where a ledge may gather
/// their fall.
const GROUP: f64 = 3.0;

/// The share of a unit its riffle runs, the least and most; and the share
/// of a unit's fall that comes over its pool rather than its riffle or its
/// ledge.
const RIFFLE: (f64, f64) = (0.25, 0.42);
const SLOW: f64 = 0.12;

/// How far a crest's bed stands above a river's mean bed, and how far below
/// it a typical pool's lies, as shares of its depth; a plunge pool scoured
/// deeper by this share of the ledge's drop.
const CREST: f64 = 0.25;
const POOL: f64 = 0.45;
const PLUNGE: f64 = 0.8;
const MOST_POOL: f64 = 1.5;

/// How long the tongue over a ledge runs, in metres, and the more for each
/// metre of its drop.
const TONGUE: (f64, f64) = (0.35, 0.9);

/// What `fallen - v` averages over a unit, near enough, so the water's mean
/// stands where its depth's share has it.
const MEAN_STEP: f64 = 0.3;

/// The least water the deepest line keeps over it.
const LEAST_DEPTH: f64 = 0.03;

/// How far the deepest line swings to one side across a straight pool and
/// for each unit of a bend's curvature times its width, as shares of half
/// the width, and the most it swings either way.
const ALTERNATE: f64 = 0.35;
const BEND: f64 = 1.6;
const MOST_SWING: f64 = 0.6;

/// How the bed rises to the brim: across a riffle, flat this share of the
/// way and then up its margins; across a pool, as a power of the share of
/// the way, steeper toward a cut bank and gentler up a bar.
const RIFFLE_FLAT: f64 = 0.45;
const POOL_RISE: f64 = 1.8;
const CUT_RISE: f64 = 1.6;
const BAR_RISE: f64 = 0.7;
const LEAST_RISE: f64 = 1.2;

/// A bank's face as broad as this share of the river's width where it is
/// gentlest, wandering by this share either way, and as this many metres,
/// and a twentieth of the width more, where it is cut steep; the least of
/// the width its shelf and the face above it are as broad as, and how much
/// more at the most; how far its top stands above or below the land beyond,
/// as a share of the land's height over the brim; and the least it stands
/// over the brim.
const GENTLE: f64 = 0.6;
const GENTLE_WANDER: f64 = 0.4;
const STEEP: (f64, f64) = (0.22, 0.05);
const SHELF: (f64, f64) = (0.3, 0.5);
const UPPER: (f64, f64) = (0.4, 0.3);
const LIFT: f64 = 0.45;
const LEAST_BANK: f64 = 0.12;

/// The least breadth a channel is drawn at, so one of no width divides by
/// none.
const NARROWEST: f64 = 1e-3;

/// How many halvings find a water's edge.
const EDGE_STEPS: u32 = 40;

/// How far a channel's bed wanders up and down about its smooth form, as a
/// share of its depth below the brim, in lumps as long as these along and
/// across it, each fainter than the last — no more than a quarter, so the
/// bed stays between its deepest and its brim; how far its banks' faces
/// wander, as a share of their own rise; and how far its breadth wanders
/// either way along it.
const RELIEF: f64 = 0.2;
const _: () = assert!(RELIEF <= 0.25);
const LUMPS: [f64; 3] = [2.5, 1.0, 0.4];
const BANK_RELIEF: f64 = 0.15;
const BREADTH: f64 = 0.08;
/// The broadest a channel stands against the breadth its course carries.
pub(crate) const BROADEST: f64 = 1.0 + BREADTH;

/// What a bank's face is laid with, short of where rock outcrops: the
/// earth of its alluvium.
const ALLUVIUM: f64 = 0.2;

/// Keys under which a river's units, banks, breadth and lumps are drawn.
const UNITS: u32 = 0x3a9d;
const WANDER: u32 = 0x51c7;
const SAND: u32 = 0x7e21;
const BROADENED: u32 = 0x47e3;
const LUMPED: u32 = 0x2b5d;

/// A place along a river's course, as its marks have it there.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Station {
    pub(crate) along: f64,
    /// The level of its brim, its water's in spate.
    pub(crate) brim: f64,
    pub(crate) width: f64,
    pub(crate) depth: f64,
    pub(crate) phase: f64,
    pub(crate) turn: f64,
    pub(crate) fall: f64,
}

impl Station {
    /// Where `near` lies along its course.
    pub(crate) fn of(near: &Nearest) -> Self {
        Self {
            along: near.along,
            brim: near.level,
            width: near.width,
            depth: near.depth,
            phase: near.phase,
            turn: near.turn,
            fall: near.fall,
        }
    }

    /// Course `index` of `courses` as it runs `along` its length; `None` for
    /// a course with no marks.
    pub(crate) fn on(courses: &Courses, index: usize, along: f64) -> Option<Self> {
        let mark = courses.at(index, along)?;
        Some(Self {
            along,
            brim: mark.level,
            width: mark.width,
            depth: mark.depth,
            phase: mark.phase,
            turn: mark.turn,
            fall: mark.fall,
        })
    }
}

/// What shapes a river's channel the whole way down it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Form {
    /// The share of its channel's depth its water fills, on average: one in
    /// spate, less in a dry season.
    pub(crate) flowing: f64,
    /// The share of its groups of units whose fall gathers at a ledge of
    /// bedded rock, and of its banks rock outcrops in.
    pub(crate) ledges: f64,
    pub(crate) outcrops: f64,
    pub(crate) seed: u32,
}

/// A river's channel across one place along it, between its brims.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Section {
    pub(crate) along: f64,
    /// Its brim's level, and half its breadth there.
    pub(crate) brim: f64,
    pub(crate) half: f64,
    /// The level its water stands at, and how steeply it falls there.
    pub(crate) water: f64,
    pub(crate) slope: f64,
    /// Where its deepest line runs across it, signed as `Nearest` has it,
    /// and its bed's level there.
    pub(crate) thalweg: f64,
    pub(crate) deepest: f64,
    /// How its pool's bed rises from its deepest line to either brim, the
    /// negative side's first: as this power of the share of the way there.
    pub(crate) rise: [f64; 2],
    /// How fast its water runs along its deepest line.
    pub(crate) running: f64,
    /// How much of a pool it is, `0.0..=1.0`, and of a ledge's bare rock.
    pub(crate) pool: f64,
    pub(crate) ledge: f64,
    pub(crate) seed: u32,
}

/// A river's channel across one place along it and the banks either side
/// of it, the negative side's first.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Banked {
    pub(crate) section: Section,
    pub(crate) banks: [Bank; 2],
}

/// How a river's bank rises from its brim to the land beyond it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Bank {
    /// How broad its face is from the brim; how high up it a bench breaks
    /// it, as a share of its height, how broad that bench is and the face
    /// above it.
    pub(crate) face: f64,
    pub(crate) bench: f64,
    pub(crate) shelf: f64,
    pub(crate) upper: f64,
    /// How far its top stands above the land beyond, as a share of the
    /// land's height over the brim.
    pub(crate) lift: f64,
    /// How much of it is the bank a pool cuts, and how much of its face bare
    /// rock outcrops in.
    pub(crate) cut: f64,
    pub(crate) rock: f64,
}

impl Bank {
    /// How far from the brim its face runs and its whole rise to the land, on
    /// a grid `step` apart, each face at least two steps broad.
    fn spans(&self, step: f64) -> (f64, f64) {
        let face = self.face.max(2.0 * step);
        (face, face + self.shelf + self.upper.max(2.0 * step))
    }
}

/// What a unit of a river's course is: a crest and the riffle below it, or
/// a ledge, each with the pool after.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Kind {
    Riffle,
    Ledge,
}

impl Kind {
    /// How closely the water follows the bed's steps in a river flowing
    /// `flowing` full: hardly at all in spate and almost wholly at low
    /// water, a ledge's drop more wholly than a riffle's.
    fn follows(self, flowing: f64) -> f64 {
        let full = flowing * flowing;
        match self {
            Self::Riffle => 1.0 - full,
            Self::Ledge => 1.0 - full * full,
        }
    }
}

/// A unit of a river's course: what it is, the phase it begins at, how many
/// units of phase it spans, and its key.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Unit {
    kind: Kind,
    start: f64,
    span: f64,
    key: u32,
}

/// A unit's long profile, in shares of its length: how far its quick fall
/// runs, where its pool is deepest, and how far below the river's mean bed
/// that lies, as a share of its depth.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Long {
    quick: f64,
    deepest: f64,
    pool: f64,
}

impl Unit {
    /// The unit `phase` lies in under `form`.
    fn at(phase: f64, form: &Form) -> Self {
        let (group, within) = cell(phase / GROUP);
        let first = phase - within * GROUP;
        let key = hash2(group, UNITS, form.seed);
        let draw = unit(key);
        let into = within * GROUP;
        let (kind, start, span) = if draw < 0.5 * form.ledges {
            (Kind::Ledge, first, GROUP)
        } else if draw < form.ledges && into >= 1.0 {
            (Kind::Ledge, first + 1.0, GROUP - 1.0)
        } else {
            (
                Kind::Riffle,
                first + mathf::floor(into.min(GROUP - 1.0)),
                1.0,
            )
        };
        let (whole, _) = cell(start);
        Self {
            kind,
            start,
            span,
            key: mix32(key ^ whole),
        }
    }

    /// How far into it `phase` lies, `0.0..=1.0`.
    fn within(&self, phase: f64) -> f64 {
        ((phase - self.start) / self.span).clamp(0.0, 1.0)
    }

    /// Its long profile, on a river whose pools lie `spacing` apart and
    /// whose depth is `depth`, when it falls `fall` in all.
    fn long(&self, spacing: f64, (fall, depth): (f64, f64)) -> Long {
        let length = (spacing * self.span).max(1e-3);
        let deeper = 0.5 + unit(mix32(self.key ^ 1));
        match self.kind {
            Kind::Riffle => {
                let quick = RIFFLE.0 + (RIFFLE.1 - RIFFLE.0) * unit(mix32(self.key ^ 2));
                Long {
                    quick,
                    deepest: quick + 0.3 * (1.0 - quick),
                    pool: POOL * deeper,
                }
            }
            Kind::Ledge => {
                let quick = ((TONGUE.0 + TONGUE.1 * fall) / length).min(0.2);
                Long {
                    quick,
                    deepest: (quick + (0.5 + 1.5 * fall) / length).min(0.5),
                    pool: (POOL * deeper + PLUNGE * fall / depth.max(1e-3)).min(MOST_POOL),
                }
            }
        }
    }

    /// How closely its water follows its bed's steps in a river flowing
    /// `flowing` full.
    fn follows(&self, flowing: f64) -> f64 {
        self.kind.follows(flowing)
    }

    /// How much of the place `v` into it is a ledge's bare rock: its lip and
    /// the tongue down from it, and the slab above the lip at the end of the
    /// unit before a ledge.
    fn bare(&self, v: f64, long: &Long, form: &Form) -> f64 {
        let lip = match self.kind {
            Kind::Ledge => 1.0 - smoothstep(long.quick, 1.6 * long.quick + 0.02, v),
            Kind::Riffle => 0.0,
        };
        let next = Self::at(self.start + self.span + 0.5, form);
        let slab = if next.kind == Kind::Ledge {
            smoothstep(0.85, 0.97, v)
        } else {
            0.0
        };
        lip.max(slab)
    }
}

impl Long {
    /// How much of its unit's fall has come `v` into it.
    fn fallen(&self, v: f64) -> f64 {
        (1.0 - SLOW) * smoothstep(0.0, self.quick, v) + SLOW * v
    }

    /// How fast that comes, against the unit's mean.
    fn falling(&self, v: f64) -> f64 {
        let x = v / self.quick;
        let quick = if (0.0..=1.0).contains(&x) {
            6.0 * x * (1.0 - x) / self.quick
        } else {
            0.0
        };
        (1.0 - SLOW) * quick + SLOW
    }

    /// How much of its pool the place `v` into it is: none at the crest or
    /// down the quick fall, all where it is deepest.
    fn hollow(&self, v: f64) -> f64 {
        smoothstep(0.6 * self.quick, self.deepest, v) * (1.0 - smoothstep(self.deepest, 1.0, v))
    }
}

/// How far apart pools lie, in metres, about `along` a river `width` wide
/// whose brim falls `fall` there, under `seed`: five to seven widths on a
/// gentle stream (Leopold and Wolman, 1957), shortening toward the steps of
/// a steep one (Montgomery and Buffington, 1997).
fn spacing(width: f64, (along, fall): (f64, f64), seed: u32) -> f64 {
    let typical = SPACING * width.max(LEAST_WIDTH) / (1.0 + fall.max(0.0) / STEPPED_FALL);
    typical * (1.0 + JITTER * noise2(along / (15.0 * width.max(LEAST_WIDTH)), 0.5, seed ^ UNITS))
}

/// How a channel's bed or bank heaves and hollows at `(along, across)` its
/// course under `seed`: lumps at each of `LUMPS`' lengths, each half as
/// strong as the last, held to `-1.0..=1.0`.
fn lumps((along, across): (f64, f64), seed: u32) -> f64 {
    let (mut total, mut weight, mut strength) = (0.0, 0.0, 1.0);
    for (salt, length) in (0u32..).zip(LUMPS) {
        total += strength * noise2(along / length, across / length, seed ^ LUMPED ^ salt);
        weight += strength;
        strength *= 0.5;
    }
    (total / weight).clamp(-1.0, 1.0)
}

/// How fast water `depth` deep runs where its surface falls `slope`: as
/// Manning's law has it, never past the swiftest a stony stream runs.
pub(crate) fn manning(depth: f64, slope: f64) -> f64 {
    let depth = depth.max(0.0);
    let speed = power(depth, 2.0 / 3.0) * mathf::sqrt(slope.max(0.0)) / MANNING;
    speed.min(swiftest(depth))
}

/// The swiftest water `depth` deep runs.
pub(crate) fn swiftest(depth: f64) -> f64 {
    MOST_FROUDE * mathf::sqrt(GRAVITY * depth.max(0.0))
}

/// A river's channel along its course at one place, everything about it
/// that holds the whole way across.
#[derive(Copy, Clone, Debug, PartialEq)]
struct Profile {
    water: f64,
    slope: f64,
    deepest: f64,
    swing: f64,
    hollow: f64,
    ledge: f64,
    running: f64,
}

impl Profile {
    /// The channel at `station` under `form`.
    fn at(station: &Station, form: &Form) -> Self {
        let depth = station.depth.max(1e-3);
        let flowing = form.flowing.clamp(0.0, 1.0);
        let spacing = spacing(station.width, (station.along, station.fall), form.seed);
        let unit = Unit::at(station.phase, form);
        let v = unit.within(station.phase);
        let fall = station.fall.max(0.0) * spacing * unit.span;
        let long = unit.long(spacing, (fall, depth));
        let stepped = long.fallen(v) - v;
        let hollow = long.hollow(v);
        let follows = unit.follows(flowing);
        // The water stands on its crests as its depth's share has it on
        // average, so its pools stand above that and its riffles below.
        let typical = station.fall.max(0.0) * spacing;
        let riffled = Kind::Riffle.follows(flowing);
        let crest = (depth * (flowing - CREST) + riffled * typical * MEAN_STEP).max(LEAST_DEPTH);
        let water = station.brim - depth * (1.0 - CREST) + crest - follows * fall * stepped;
        let raised = -CREST + (CREST + long.pool) * hollow;
        let deepest =
            (station.brim - depth * (1.0 + raised) - fall * stepped).min(water - LEAST_DEPTH);
        // What runs over a crest runs down the riffle below it as steeply as
        // a typical riffle's water falls there.
        let quickest = 1.5 * (1.0 - SLOW) / f64::midpoint(RIFFLE.0, RIFFLE.1) + SLOW;
        let riffle = station.fall.max(LEAST_FALL) * (1.0 + riffled * (quickest - 1.0));
        let over_crest = manning(crest, riffle) * crest;
        let thalweg = water - deepest;
        let running = (over_crest / thalweg).min(swiftest(thalweg));
        // A bend's own swing takes over from the alternate bars of a
        // straight reach as it tightens.
        let bend = station.turn * station.width;
        let alternate = ALTERNATE * (1.0 - smoothstep(0.05, 0.3, bend.abs()));
        // Alternate bars swing the deep water to one side a unit's length
        // along and to the other the next, ledge or riffle.
        let swing = (alternate * mathf::sin(PI * station.phase) - BEND * bend)
            .clamp(-MOST_SWING, MOST_SWING);
        Self {
            water,
            slope: station.fall.max(0.0) * (1.0 + follows * (long.falling(v) - 1.0)),
            deepest,
            swing,
            hollow,
            ledge: unit.bare(v, &long, form),
            running,
        }
    }
}

/// The level a river's water stands at `station` under `form`.
pub(crate) fn water(station: &Station, form: &Form) -> f64 {
    Profile::at(station, form).water
}

/// The farthest from the middle of a course `width` wide, on a grid `step`
/// apart, its channel has any say over the ground: its broadest brim, the
/// broadest its bank's faces and shelf ever run, and the softening beyond.
pub(crate) fn farthest_say(width: f64, step: f64) -> f64 {
    let width = width.max(NARROWEST);
    let face = (GENTLE * (1.0 + GENTLE_WANDER) * width).max(STEEP.0 + STEEP.1 * width);
    let beyond = (SHELF.0 + SHELF.1 + UPPER.0 + UPPER.1) * width;
    0.5 * BROADEST * width + face + beyond + 6.0 * step
}

impl Banked {
    /// The channel at `station` under `form` and its banks.
    pub(crate) fn new(station: &Station, form: &Form) -> Self {
        let profile = Profile::at(station, form);
        let bank = |side: f64| bank(station, form, &profile, side);
        Self {
            section: Section::of(station, form, &profile),
            banks: [bank(-1.0), bank(1.0)],
        }
    }

    /// The bank on the side `across` lies.
    fn bank(&self, across: f64) -> &Bank {
        &self.banks[usize::from(across >= 0.0)]
    }

    /// The ground `across` the course, where the land beyond its banks
    /// stands at `top`: its bed within its brims, and its bank beyond, each
    /// face at least two of a grid's `step`s broad.
    pub(crate) fn ground(&self, across: f64, top: f64, step: f64) -> f64 {
        let section = &self.section;
        let beyond = across.abs() - section.half;
        if beyond < 0.0 {
            return section.bed(across);
        }
        let bank = self.bank(across);
        let (face, reach) = bank.spans(step);
        let lifted = 1.0 - smoothstep(reach, reach + 2.0 * section.half, beyond);
        let height = top - section.brim;
        // A perched river's bank falls to the land beyond rather than rising.
        let top = if height >= 0.0 {
            section.brim + (height * (1.0 + bank.lift * lifted)).max(LEAST_BANK.min(height))
        } else {
            top
        };
        let low = smoothstep(0.0, face, beyond);
        let high = smoothstep(face + bank.shelf, reach, beyond);
        // Slumped and bulging along its faces, firm at its foot and its top.
        let slumped = 1.0
            + BANK_RELIEF
                * mathf::sin(PI * (beyond / reach).clamp(0.0, 1.0))
                * lumps((section.along, across), section.seed ^ WANDER);
        section.brim
            + (top - section.brim) * (bank.bench * low + (1.0 - bank.bench) * high) * slumped
    }

    /// How far beyond its brim on the side `across` lies its bank's face
    /// runs, a grid's `step` softening it.
    pub(crate) fn bank_reach(&self, across: f64, step: f64) -> f64 {
        self.bank(across).spans(step).1
    }

    /// What the water laid down or wore away `across` the course,
    /// `-1.0..=1.0`: sand up its bars and in the deep of its pools, in
    /// patches; bare rock at a ledge; and the earth of its banks' faces and
    /// of the walls its pools cut down to the deep water.
    pub(crate) fn laid(&self, across: f64) -> f64 {
        let section = &self.section;
        let ledge = section.ledge;
        // Where its rock outcrops in a bank it stands out of the earth as
        // blocks of its own; only a ledge's rock is bare across the bed.
        if across.abs() >= section.half {
            return ALLUVIUM * (1.0 - ledge) - ledge;
        }
        let (side, share) = section.toward_brim(across);
        let swing = section.thalweg / section.half;
        let bar_side = usize::from(swing < 0.0);
        if side != bar_side {
            let wall = smoothstep(0.45, 0.8, share) * self.banks[side].cut;
            if wall > 0.0 && ledge < 1.0 {
                return (ALLUVIUM * wall * (1.0 - ledge) - ledge).clamp(-1.0, 1.0);
            }
        }
        let bar = if side == bar_side {
            smoothstep(0.5, 0.85, share) * smoothstep(0.08, 0.35, swing.abs())
        } else {
            0.0
        };
        let deep = 0.5 * (1.0 - smoothstep(0.1, 0.35, share));
        let patches = smoothstep(
            0.15,
            0.5,
            noise2(section.along / 1.8, across / 1.1, section.seed ^ SAND),
        );
        let sand = section.pool * (0.85 * bar).max(deep) * patches;
        (sand * (1.0 - ledge) - ledge).clamp(-1.0, 1.0)
    }
}

impl Section {
    /// The channel at `station` under `form`.
    pub(crate) fn new(station: &Station, form: &Form) -> Self {
        Self::of(station, form, &Profile::at(station, form))
    }

    /// The channel at `station` under `form`, whose long profile is
    /// `profile` there.
    fn of(station: &Station, form: &Form, profile: &Profile) -> Self {
        let width = station.width.max(NARROWEST);
        let breadth = noise2(station.along / (3.0 * width), 0.71, form.seed ^ BROADENED);
        let half = 0.5 * width * (1.0 + BREADTH * breadth.clamp(-1.0, 1.0));
        let swing = profile.swing;
        let toward = POOL_RISE + CUT_RISE * swing.abs();
        let away = (POOL_RISE - BAR_RISE * swing.abs()).max(LEAST_RISE);
        let rise = if swing >= 0.0 {
            [away, toward]
        } else {
            [toward, away]
        };
        Self {
            along: station.along,
            brim: station.brim,
            half,
            water: profile.water,
            slope: profile.slope,
            thalweg: swing * half,
            deepest: profile.deepest,
            rise,
            running: profile.running,
            pool: profile.hollow,
            ledge: profile.ledge,
            seed: form.seed,
        }
    }

    /// Which way from its deepest line `across` lies, as an index into
    /// `rise`, and how far toward the brim, as a share of the way there.
    fn toward_brim(&self, across: f64) -> (usize, f64) {
        let c = self.thalweg;
        if across >= c {
            (1, (across - c) / (self.half - c).max(1e-9))
        } else {
            (0, (c - across) / (self.half + c).max(1e-9))
        }
    }

    /// The bed's level `across` the course, within its brims: flat across
    /// a riffle but for its margins, rounded across a pool, and between the
    /// two as much of a pool as the place is; lumped by the gravel its floods
    /// heaped and the hollows they scoured, most between its deepest line
    /// and its brims, so its water's edge wanders as a stream's does.
    pub(crate) fn bed(&self, across: f64) -> f64 {
        let (side, share) = self.toward_brim(across);
        let share = share.clamp(0.0, 1.0);
        let riffle = smoothstep(RIFFLE_FLAT, 1.0, share);
        let pool = power(share, self.rise[side]);
        let risen = riffle + (pool - riffle) * self.pool;
        let lumped = 4.0 * RELIEF * risen * (1.0 - risen) * lumps((self.along, across), self.seed);
        self.deepest + (self.brim - self.deepest) * (risen + lumped)
    }

    /// How deep its water stands `across` the course.
    pub(crate) fn depth(&self, across: f64) -> f64 {
        self.water_at(across).0
    }

    /// How deep its water stands `across` the course, and how fast it runs
    /// there: as fast for its depth as Manning's law has it against its
    /// deepest line, never past critical.
    pub(crate) fn water_at(&self, across: f64) -> (f64, f64) {
        if across.abs() >= self.half {
            return (0.0, 0.0);
        }
        let depth = (self.water - self.bed(across)).max(0.0);
        let deepest = self.water - self.deepest;
        if depth <= 0.0 || deepest <= 0.0 {
            return (depth, 0.0);
        }
        let speed = (self.running * power(depth / deepest, 2.0 / 3.0)).min(swiftest(depth));
        (depth, speed)
    }

    /// How far across the course its water's edge lies on `side`, `-1.0` or
    /// `1.0`: where the bed, rising steadily from its deepest line, meets
    /// the water.
    pub(crate) fn edge(&self, side: f64) -> f64 {
        let brim = side.signum() * self.half;
        let (mut wet, mut dry) = (self.thalweg, brim);
        if self.bed(dry) <= self.water {
            return dry;
        }
        for _ in 0..EDGE_STEPS {
            let middle = f64::midpoint(wet, dry);
            if self.bed(middle) <= self.water {
                wet = middle;
            } else {
                dry = middle;
            }
        }
        f64::midpoint(wet, dry)
    }
}

/// The bank on `side` of the channel at `station` under `form`, its deep
/// water swung and its pool and ledge as `profile` has them.
fn bank(station: &Station, form: &Form, profile: &Profile, side: f64) -> Bank {
    let width = station.width.max(NARROWEST);
    let along = station.along;
    let salt = if side >= 0.0 { 0x9e37 } else { 0x79b9 };
    let wander = |scale: f64, key: u32| {
        noise2(
            along / (scale * width),
            0.37,
            form.seed ^ WANDER ^ key ^ salt,
        )
        .clamp(-1.0, 1.0)
    };
    let slumped = 0.7 * smoothstep(0.35, 0.7, wander(2.0, 1));
    let cut =
        (smoothstep(0.1, 0.45, profile.swing * side) * (0.4 + 0.6 * profile.hollow)).max(slumped);
    let gentle = GENTLE * width * (1.0 + GENTLE_WANDER * wander(3.0, 2));
    let steep = STEEP.0 + STEEP.1 * width;
    let bench = 1.0 - (1.0 - cut) * 0.7 * smoothstep(-0.1, 0.5, wander(4.0, 3));
    let outcrop = smoothstep(
        0.7 - 1.4 * form.outcrops,
        0.9 - 1.4 * form.outcrops,
        wander(2.6, 4),
    );
    Bank {
        face: gentle + (steep - gentle) * cut,
        bench,
        shelf: (SHELF.0 + 0.5 * SHELF.1 * (1.0 + wander(3.4, 5))) * width * (1.0 - cut),
        upper: (UPPER.0 + 0.5 * UPPER.1 * (1.0 + wander(2.2, 6))) * width,
        lift: LIFT * wander(3.3, 7),
        cut,
        rock: outcrop * (0.4 + 0.6 * cut),
    }
}

/// Count `course`'s units of pool and riffle from its head under `seed`,
/// and mark at each of its marks how sharply it bends and how steeply its
/// brim falls; `None` when the heap will not hold the work.
pub(crate) fn survey(course: &mut [Mark], seed: u32) -> Option<()> {
    let mut along = Vec::new();
    if !fallible::reserve(&mut along, course.len()) {
        return None;
    }
    along.extend(course::travelled(course));
    let level = |at: f64| course::blended(course, &along, at).map_or(0.0, |mark| mark.level);
    let total = along.last().copied().unwrap_or(0.0);
    let mut falls = fallible::filled(course.len(), 0.0)?;
    for (fall, &at) in falls.iter_mut().zip(&along) {
        let (from, to) = ((at - FALL_REACH).max(0.0), (at + FALL_REACH).min(total));
        if to > from {
            *fall = ((level(from) - level(to)) / (to - from)).max(0.0);
        }
    }
    for (mark, fall) in course.iter_mut().zip(falls) {
        mark.fall = fall;
    }
    let mut phase = GROUP * unit(mix32(seed ^ UNITS));
    let mut last = 0.0;
    for (mark, &at) in course.iter_mut().zip(&along) {
        phase += (at - last) / spacing(mark.width, (at, mark.fall), seed);
        mark.phase = phase;
        last = at;
    }
    let mut turns = fallible::filled(course.len(), 0.0)?;
    for (index, turn) in turns.iter_mut().enumerate() {
        let at = |index: usize| course.get(index).map(|mark| (mark.x, mark.z));
        let (Some(before), Some(here), Some(after)) =
            (index.checked_sub(1).and_then(at), at(index), at(index + 1))
        else {
            continue;
        };
        let (a, b) = (
            (here.0 - before.0, here.1 - before.1),
            (after.0 - here.0, after.1 - here.1),
        );
        // The curvature of the circle through the three marks, signed by
        // which way the course turns.
        let span = mathf::hypot(a.0, a.1)
            * mathf::hypot(b.0, b.1)
            * mathf::hypot(after.0 - before.0, after.1 - before.1);
        if span > 1e-12 {
            *turn = 2.0 * (a.0 * b.1 - a.1 * b.0) / span;
        }
    }
    // Averaged over its neighbours, so a course smoothed from a coarse grid
    // bends evenly between its corners.
    for (index, mark) in course.iter_mut().enumerate() {
        let near = turns
            .get(index.saturating_sub(TURN_MARKS)..(index + TURN_MARKS + 1).min(turns.len()))
            .unwrap_or(&[]);
        mark.turn = near.iter().sum::<f64>() / real(near.len().max(1));
    }
    Some(())
}

/// How many marks either way a mark's bend is averaged over: about a
/// river's width of its course, its marks a few metres apart.
const TURN_MARKS: usize = 2;
/// How far either way along its course a mark's fall is measured over.
const FALL_REACH: f64 = 10.0;

#[cfg(test)]
#[path = "channel_tests.rs"]
mod tests;
