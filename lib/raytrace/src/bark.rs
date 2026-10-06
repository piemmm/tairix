//! Bark: the skin of a tree's limbs in relief and in colour — plates and
//! the fissures between them, ridges and furrows, lenticels and scars, the
//! lichen and moss that live on it, and the soil splashed up its foot.
//!
//! A pattern is laid on the limb itself, along its stem and round its girth
//! at their real size in metres. The circle round the limb is carried onto a
//! circle through the pattern's space, so the pattern closes on itself with
//! no seam, and each tree is moved to its own part of that space by the key
//! it was placed under, so no two trees of a kind wear the same bark.

use core::f64::consts::TAU;

use tairix_util::mathf;

use crate::cactus::{
    self, cushion, profile, Ribs, CORK_ROUGH, CROWN, CUSHION, CUTICLE, FELT, MEAN_RIB, WOOL,
};
use crate::cover::{CRUST, MOSS};
use crate::noise::{cells3, hash2, noise3, smoothstep, NOISE_SLOPE};
use crate::pigment::{lying, Spot, SNOW};
use crate::sample::{mix32, unit};
use crate::vector::Vec3;

/// The pattern a bark is cut in.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum BarkKind {
    /// Deep furrows parting and joining up the trunk, their ridges broken
    /// across into blocks: oak, maple, poplar, willow, olive.
    Furrowed,
    /// White, marked across with lenticels and scarred black below its
    /// limbs, going over to black fissured bark about its foot: birch.
    Papery,
    /// Grey-brown plates between deep fissures, thinning up the trunk to
    /// orange flakes: pine.
    Plated,
    /// Smooth and grey, faintly mottled, an eye where each limb fell: beech.
    Smooth,
    /// Glossy, banded across with rows of lenticels: cherry.
    Banded,
    /// Small close scales: spruce, fir.
    Scaly,
    /// Rings where old fronds fell: a palm.
    Ringed,
    /// A cactus's skin, folded into `ribs` ribs round its stem however thick
    /// it is, felted areoles along their crests: laid in its plant's own
    /// measure, its stem reckoned down from its apex.
    Ribbed { ribs: u8 },
    /// Shallow rings across it where its fine roots grew: a carrot.
    Taproot,
}

/// A bark: its pattern; the colours of its ridges and of its hollows; the
/// colour lichen, or on a plated bark the upper trunk, takes it toward, and
/// from what height up the trunk that upper colour shows; how much snow lies
/// on its limbs; how much of it moss takes, at most; and how much of it has
/// sloughed away from dead wood.
#[derive(Clone, Debug)]
pub(crate) struct Bark {
    pub(crate) kind: BarkKind,
    pub(crate) light: Vec3,
    pub(crate) dark: Vec3,
    pub(crate) accent: Vec3,
    pub(crate) rise: f64,
    pub(crate) snow: f64,
    pub(crate) moss: f64,
    pub(crate) bare: f64,
    pub(crate) seed: u32,
}

/// Where a point of bark lies on its limb, and how finely it is seen.
#[derive(Copy, Clone, Debug)]
pub(crate) struct OnLimb {
    /// How far along the tree's path from the ground, in metres.
    along: f64,
    /// Its angle round the limb, and that angle's cosine and sine.
    angle: f64,
    round: (f64, f64),
    /// The limb's radius there, in metres.
    girth: f64,
    /// Where the tree's own bark lies in the pattern's space.
    offset: Vec3,
    /// How wide a patch of the bark one pixel covers, in metres.
    width: f64,
}

impl OnLimb {
    /// `along` metres up its stem and `angle` radians round a limb `girth`
    /// in radius, on the tree placed under `key`, seen a pixel `width` wide.
    pub(crate) fn new(along: f64, angle: f64, girth: f64, (key, width): (u32, f64)) -> Self {
        let key = mix32(key ^ 0xba12_c0de);
        let offset = Vec3::new(unit(key), unit(mix32(key)), unit(mix32(key ^ 0x51))) * 4096.0;
        Self {
            along,
            angle,
            round: (mathf::cos(angle), mathf::sin(angle)),
            girth: girth.max(1e-4),
            offset,
            width,
        }
    }

    /// The point of bark `spot` names.
    pub(crate) fn of(spot: &Spot) -> Self {
        Self::new(
            spot.uv.0,
            spot.uv.1,
            spot.girth,
            (spot.instance, spot.width),
        )
    }

    /// This point moved `along` metres along the limb and `round` metres
    /// round it, the way its angle grows.
    pub(crate) fn moved(&self, along: f64, round: f64) -> Self {
        let angle = self.angle + round / self.girth;
        Self {
            along: self.along + along,
            angle,
            round: (mathf::cos(angle), mathf::sin(angle)),
            ..*self
        }
    }

    /// The point in a pattern's space laid out `across` features to a metre
    /// round the limb and `up` to a metre along it.
    fn at(&self, (across, up): (f64, f64)) -> Vec3 {
        let radius = self.girth * across;
        Vec3::new(
            radius * self.round.0,
            self.along * up,
            radius * self.round.1,
        ) + self.offset
    }

    /// How much of a feature `size` metres across a pixel still shows.
    fn shows(&self, size: f64) -> f64 {
        1.0 - smoothstep(0.4 * size, 1.6 * size, self.width)
    }
}

/// How far a moss cushion on bark stands proud at its crown, in metres, and
/// how many of its mounds span a metre.
const MOSS_RISE: f64 = 0.009;
const MOUNDS: f64 = 60.0;

/// Soil splashed up a trunk's foot.
const SOIL: Vec3 = Vec3::new(0.12, 0.095, 0.07);

/// The bare wood a stripped scar shows, and a birch's twigs, too young to
/// have whitened.
const HEARTWOOD: Vec3 = Vec3::new(0.34, 0.26, 0.18);
const BIRCH_TWIG: Vec3 = Vec3::new(0.2, 0.12, 0.1);

/// Metres between the rows a trunk's scars are strewn in.
const SCAR_ROW: f64 = 0.8;

/// A cactus's skin as it ages: grey-olive where the years have weathered its
/// wax; cork in its lighter and darker tones where it has barked over; and
/// the grey its felt fades to.
const AGED_SKIN: Vec3 = Vec3::new(0.156, 0.168, 0.098);
const CORK: [Vec3; 2] = [Vec3::new(0.195, 0.133, 0.08), Vec3::new(0.069, 0.045, 0.03)];
const OLD_FELT: Vec3 = Vec3::new(0.102, 0.093, 0.08);

/// The wood where bark sloughed from dead wood: tan sapwood where it lately
/// fell, grey-brown where the weather has had it long; how high it lies, as
/// the bark's own height, sunk below all but the floors of its fissures; and
/// how widely the sheets' noise spreads about nought.
const SAPWOOD: Vec3 = Vec3::new(0.16, 0.11, 0.065);
const WEATHERED_WOOD: Vec3 = Vec3::new(0.1, 0.09, 0.078);
const BARED: f64 = 0.12;
const SLOUGH_SPREAD: f64 = 0.4;

/// A net of fissures running up a limb: `round` and `up` of its meshes to a
/// metre round and along it, how far from a fissure in its noise a ridge's
/// middle lies, how jagged its fissures' edges are, and the breaks crossing
/// its ridges — how much of the bark they cross, and `every` to a metre up it.
#[derive(Copy, Clone, Debug)]
struct Net {
    round: f64,
    up: f64,
    crest: f64,
    jagged: f64,
    breaks: f64,
    every: f64,
}

/// A pine's plates, an oak's furrows and the crust about a birch's foot.
const PLATES: Net = Net {
    round: 9.0,
    up: 0.7,
    crest: 0.45,
    jagged: 0.0,
    breaks: 0.9,
    every: 4.9,
};
const FURROWS: Net = Net {
    round: 17.0,
    up: 3.0,
    crest: 0.5,
    jagged: 0.14,
    breaks: 0.85,
    every: 7.0,
};
const BIRCH_FOOT: Net = Net {
    round: 16.0,
    up: 2.0,
    crest: 0.45,
    jagged: 0.0,
    breaks: 0.6,
    every: 14.0,
};

impl Bark {
    /// How far the bark stands out at `at`, from `0.0` in its deepest cracks
    /// to `1.0` on its plates and ridges.
    pub(crate) fn height(&self, at: &OnLimb) -> f64 {
        self.surface(at, false).0
    }

    /// The bark's height at `at`, and how fast it rises along the limb and
    /// round it, a metre each way, read a millimetre apart.
    pub(crate) fn sloped(&self, at: &OnLimb) -> (f64, f64, f64) {
        const STEP: f64 = 1e-3;
        let here = self.height(at);
        (
            here,
            (self.height(&at.moved(STEP, 0.0)) - here) / STEP,
            (self.height(&at.moved(0.0, STEP)) - here) / STEP,
        )
    }

    /// The most the bark's height rises a metre along or round a limb no
    /// thinner than `girth`, however fine its detail: twice the steepest found
    /// over a dense sampling of each pattern, and a cactus's ribs as their
    /// profile bounds them. A palm's rings step, so theirs bounds only the
    /// smooth stretches between the steps.
    pub(crate) fn steepest(&self, girth: f64) -> f64 {
        match self.kind {
            BarkKind::Furrowed => 1_800.0,
            BarkKind::Plated | BarkKind::Scaly => 720.0,
            BarkKind::Papery => 460.0,
            BarkKind::Smooth | BarkKind::Banded => 330.0,
            BarkKind::Ribbed { ribs } => cactus::steepest(ribs, girth),
            BarkKind::Taproot => 2_200.0,
            BarkKind::Ringed => 1_000.0,
        }
    }

    /// What takes a limb placed `scale` times its prototype's size from the
    /// prototype's units to the bark's: a bark is laid at its real size, but
    /// a cactus's in its plant's own, since its areoles carry the spines the
    /// plant is built with.
    pub(crate) const fn measure(&self, scale: f64) -> f64 {
        match self.kind {
            BarkKind::Ribbed { .. } => 1.0,
            _ => scale,
        }
    }

    /// The colour of the bark at `spot`, with the moss and snow on it.
    pub(crate) fn colour(&self, spot: &Spot) -> Vec3 {
        let at = OnLimb::of(spot);
        let (height, colour) = self.surface(&at, true);
        // Where a sheet sloughed away the wood lies open, no fissure's floor.
        let hollow = (1.0 - height) * (1.0 - height) * (1.0 - self.gone(&at));
        let colour = colour * (1.0 - self.cavity() * hollow);
        let mossed = spot
            .cover
            .unwrap_or_else(|| self.mossed(&at, spot.normal, height));
        colour
            .lerp(self.moss_colour(&at, spot.normal), mossed)
            .lerp(SNOW, lying(self.snow, spot.normal))
    }

    /// How much less light the deepest of the bark's hollows gets than its
    /// ridges, from the walls about them: much in a deep fissure, little in
    /// a smooth bark's shallow folds.
    fn cavity(&self) -> f64 {
        match self.kind {
            BarkKind::Plated | BarkKind::Furrowed => 0.7,
            BarkKind::Papery => 0.55,
            BarkKind::Scaly => 0.45,
            BarkKind::Ringed | BarkKind::Ribbed { .. } => 0.3,
            BarkKind::Taproot => 0.25,
            BarkKind::Banded => 0.2,
            BarkKind::Smooth => 0.15,
        }
    }

    /// The bark's height at `at` and, when `paint` asks, its own colour
    /// there, before moss and snow.
    fn surface(&self, at: &OnLimb, paint: bool) -> (f64, Vec3) {
        let (height, colour) = match self.kind {
            BarkKind::Plated => self.plated(at, paint),
            BarkKind::Furrowed => self.furrowed(at, paint),
            BarkKind::Papery => self.papery(at, paint),
            BarkKind::Smooth => self.smooth(at, paint),
            BarkKind::Banded => self.banded(at, paint),
            BarkKind::Scaly => self.scaly(at, paint),
            BarkKind::Ringed => return self.ringed(at, paint),
            BarkKind::Ribbed { ribs } => return self.ribbed(ribs, at, paint),
            BarkKind::Taproot => return self.taproot(at, paint),
        };
        let (height, colour) = self.scarred(at, (height, colour), paint);
        let (height, colour) = if paint {
            (height, self.weathered(at, colour, height))
        } else {
            (height, colour)
        };
        self.sloughed(at, (height, colour), paint)
    }

    /// How much of the bark at `at` has sloughed away, in sheets with sharp
    /// edges where each broke from the wood.
    fn gone(&self, at: &OnLimb) -> f64 {
        if self.bare <= 0.0 {
            return 0.0;
        }
        let seed = self.seed;
        let sheets = noise3(at.at((2.2, 0.9)), seed ^ 0x5f)
            + 0.35 * noise3(at.at((7.0, 2.5)), seed ^ 0x60) * at.shows(0.05);
        // Noise lies about nought, as narrowly as this: the share of it above
        // the threshold is about the share asked.
        let threshold = SLOUGH_SPREAD * (1.0 - 2.0 * self.bare.min(1.0));
        smoothstep(threshold - 0.03, threshold + 0.03, sheets) * at.shows(0.03)
    }

    /// `surface` where its bark has sloughed off dead wood in sheets: the
    /// wood beneath, sunk below the bark it fell from, its grain standing in
    /// fine ridges where the weather wore the softer wood between, split in
    /// checks along it and engraved by the galleries of the beetles that fed
    /// beneath the bark; tan where it lately fell, weathering grey-brown.
    fn sloughed(&self, at: &OnLimb, (height, colour): (f64, Vec3), paint: bool) -> (f64, Vec3) {
        let gone = self.gone(at);
        if gone <= 0.0 {
            return (height, colour);
        }
        let seed = self.seed;
        let grain = noise3(at.at((160.0, 3.0)), seed ^ 0x61) * at.shows(0.003);
        let checked = smoothstep(0.0, 0.3, noise3(at.at((6.0, 1.0)), seed ^ 0x63));
        let check = (1.0 - smoothstep(0.0, 0.04, noise3(at.at((22.0, 1.2)), seed ^ 0x62).abs()))
            * checked
            * at.shows(0.006);
        // Beetles fed in a few places, never all over.
        let fed = smoothstep(0.35, 0.6, noise3(at.at((3.0, 1.5)), seed ^ 0x65));
        let gallery = (1.0
            - smoothstep(0.0, 0.025, noise3(at.at((30.0, 18.0)), seed ^ 0x64).abs()))
            * fed
            * at.shows(0.004);
        let wood = (BARED + 0.04 * grain - 0.12 * check.max(0.6 * gallery)).max(0.0);
        let height = height + (wood - height) * gone;
        if !paint {
            return (height, colour);
        }
        // Most bared wood has lain long enough to grey.
        let weathered = 0.45 + 0.55 * smoothstep(-0.3, 0.5, noise3(at.at((2.0, 0.6)), seed ^ 0x66));
        let tone = SAPWOOD.lerp(WEATHERED_WOOD, weathered) * (0.92 + 0.1 * grain);
        let bared = tone.lerp(tone * 0.35, check).lerp(tone * 0.6, gallery);
        (height, colour.lerp(bared, gone))
    }

    /// A pine's bark. Low on the trunk, long rough plates split up its length
    /// by wide fissures that part and join, broken across here and there and
    /// layered in thin flaking sheets; higher, from where each tree and each
    /// side of it turns about `rise`, thin orange bark peeling in papery
    /// flakes and cracked up its length.
    fn plated(&self, at: &OnLimb, paint: bool) -> (f64, Vec3) {
        let seed = self.seed;
        let turn = self.rise * (1.0 + 0.35 * noise3(at.at((0.05, 0.01)), seed ^ 0x71))
            + 1.6 * noise3(at.at((2.0, 0.3)), seed ^ 0x6d);
        let upper = smoothstep(turn - 1.2, turn + 2.2, at.along);
        let (ridge, cut) = self.fissures(at, &PLATES);
        let plate = smoothstep(0.22, 0.42, ridge) * (0.8 + 0.2 * ridge) * (1.0 - 0.55 * cut);
        let (sheets, sheet_edge) =
            terraced(0.5 + 0.5 * noise3(at.at((10.0, 22.0)), seed ^ 0x2a), 2.0);
        let fibre = noise3(at.at((110.0, 5.0)), seed ^ 0x3d) * at.shows(0.006);
        let rough = self.rough(at, (38.0, 11.0));
        let layered = at.shows(0.04);
        let low = mix(
            0.6,
            plate * (0.74 + 0.1 * sheets * layered + 0.1 * rough + 0.05 * fibre),
            at.shows(0.1),
        );
        let flakes = noise3(at.at((16.0, 30.0)), seed ^ 0x3b);
        let lifted = smoothstep(0.15, 0.4, flakes) * at.shows(0.025);
        let rim = (1.0 - smoothstep(0.0, 0.08, (flakes - 0.15).abs())) * at.shows(0.02);
        let cracked = smoothstep(0.02, 0.12, noise3(at.at((13.0, 1.3)), seed ^ 0x3c).abs());
        let high = (0.82 + 0.1 * lifted) * mix(1.0, 0.7 + 0.3 * cracked, at.shows(0.02));
        let height = mix(low, high, upper).min(1.0);
        if !paint {
            return (height, Vec3::ZERO);
        }
        // Grey-brown plates, redder in places and weathered grey on their
        // highest faces; red-brown down the fissures' walls, dark at their
        // floors.
        let tone = smoothstep(-0.5, 0.6, noise3(at.at((3.0, 0.5)), seed ^ 0x77));
        let weathered = smoothstep(0.75, 0.92, low)
            * smoothstep(-0.2, 0.5, noise3(at.at((6.0, 1.5)), seed ^ 0x78));
        let face = self
            .light
            .lerp(self.light * Vec3::new(1.2, 0.86, 0.72), tone)
            .lerp(
                Vec3::splat(self.light.max_element() * 0.95),
                0.45 * weathered,
            )
            .lerp(self.light * 1.2, 0.25 * sheet_edge * layered)
            * (0.92 + 0.08 * fibre);
        let wall = self.dark.lerp(
            self.dark.lerp(self.accent, 0.3),
            smoothstep(0.15, 0.4, ridge),
        );
        let low = wall.lerp(face, smoothstep(0.3, 0.55, ridge) * (1.0 - 0.5 * cut));
        let paper = self
            .accent
            .lerp(self.accent * Vec3::new(1.12, 1.06, 0.98), tone)
            .lerp(Vec3::new(0.72, 0.6, 0.5), 0.35 * lifted)
            .lerp(self.accent * 0.55, 0.6 * rim)
            * (0.8 + 0.2 * cracked);
        (height, low.lerp(paper, upper))
    }

    /// Where `at` lies in `net`: how far toward the middle of the ridge
    /// between two fissures it lies, from `0.0` in a fissure's floor to
    /// `1.0`; and how deep in one of the broad, shallow breaks crossing the
    /// ridges.
    fn fissures(&self, at: &OnLimb, net: &Net) -> (f64, f64) {
        let seed = self.seed;
        let (round, up) = (net.round, net.up);
        let warp = 0.45 * noise3(at.at((0.45 * round, 0.8 * up)), seed ^ 0x11);
        let fine = noise3(at.at((2.7 * round, 2.9 * up)), seed ^ 0x12) * at.shows(0.35 / round);
        let jag = if net.jagged > 0.0 {
            net.jagged
                * noise3(at.at((7.0 * round, 6.5 * up)), seed ^ 0x13)
                * at.shows(0.12 / round)
        } else {
            0.0
        };
        let value =
            noise3(at.at((round, up)) + Vec3::new(warp, 0.0, -warp), seed) + 0.25 * fine + jag;
        let ridge = (value.abs() / net.crest).min(1.0);
        let across = noise3(
            at.at((0.55 * round, net.every)) + Vec3::new(0.0, warp, 0.0),
            seed ^ 0x1b,
        );
        let patches = smoothstep(
            0.15,
            0.5,
            noise3(at.at((0.7 * round, 2.2 * up)), seed ^ 0x1c),
        );
        let cut = (1.0 - smoothstep(0.0, 0.2, across.abs()))
            * patches
            * net.breaks
            * at.shows(0.5 / round);
        (ridge, cut)
    }

    /// The knobbly roughness of a ridge's or a plate's face, `round` and `up`
    /// of its knobs to a metre, with the small cracks between them: from
    /// about `-1.0` to `1.0`.
    fn rough(&self, at: &OnLimb, (round, up): (f64, f64)) -> f64 {
        let seed = self.seed;
        let knobs = noise3(at.at((round, up)), seed ^ 0x4b)
            + 0.5 * noise3(at.at((2.3 * round, 2.1 * up)), seed ^ 0x4c) * at.shows(0.5 / round);
        let cracks = 1.0
            - smoothstep(
                0.0,
                0.06,
                noise3(at.at((0.8 * round, 2.5 * up)), seed ^ 0x4d).abs(),
            );
        (knobs - 0.8 * cracks * at.shows(0.6 / round)) * at.shows(1.0 / round)
    }

    /// Furrows parting and joining up the trunk in a long net, sharp at their
    /// floors and red-brown down their walls, between narrow ridges broken
    /// across into blocks, their crests crumbling into corky scales, cracked
    /// along the grain and knobbly.
    fn furrowed(&self, at: &OnLimb, paint: bool) -> (f64, Vec3) {
        let seed = self.seed;
        let (ridge, cut) = self.fissures(at, &FURROWS);
        // A furrow's walls climb from a sharp floor to a narrow crest.
        let profile = ridge * (2.0 - ridge);
        let near = at.shows(0.012);
        let (scale, seam) = if near > 0.0 {
            let scales = cells3(at.at((70.0, 30.0)), seed ^ 0x54, 0.9);
            let seam = 1.0 - smoothstep(0.0, 0.12, scales.wall());
            // Each scale stands at its own height, easing down into its seam
            // so its neighbours meet there.
            (unit(scales.id) * (1.0 - seam), seam * near)
        } else {
            (0.5, 0.0)
        };
        let (knobs, crack) = self.cork(at);
        let skin = 0.74 + 0.2 * near * scale + 0.08 * knobs - 0.3 * seam - 0.25 * crack;
        let height = mix(0.5, profile * (1.0 - 0.6 * cut) * skin, at.shows(0.06)).clamp(0.0, 1.0);
        if !paint {
            return (height, Vec3::ZERO);
        }
        let tone = smoothstep(-0.5, 0.6, noise3(at.at((3.0, 0.8)), seed ^ 0x77));
        let crest = smoothstep(0.55, 0.9, profile);
        let face = self
            .light
            .lerp(self.light * Vec3::new(0.86, 0.9, 0.95), tone)
            .lerp(
                Vec3::splat(self.light.max_element() * 0.92),
                0.4 * crest * (0.4 + 0.6 * scale),
            )
            * (0.9 + 0.1 * knobs);
        let inner = self
            .dark
            .lerp(self.light * Vec3::new(1.05, 0.76, 0.58), 0.5);
        let walls = self.dark.lerp(inner, smoothstep(0.05, 0.45, ridge));
        let colour = walls
            .lerp(face, smoothstep(0.45, 0.85, ridge) * (1.0 - 0.5 * cut))
            .lerp(self.dark, 0.75 * seam.max(crack));
        (height, colour)
    }

    /// The corky skin of a furrowed bark's ridges at `at`: how knobbly it
    /// stands there, from about `-1.0` to `1.0`, and how deep in one of the
    /// cracks running along its grain, `0.0` to `1.0`.
    fn cork(&self, at: &OnLimb) -> (f64, f64) {
        let seed = self.seed;
        let knobs = (noise3(at.at((42.0, 26.0)), seed ^ 0x4e)
            + 0.5 * noise3(at.at((97.0, 61.0)), seed ^ 0x4f) * at.shows(0.004))
            * at.shows(0.012);
        let along = noise3(at.at((64.0, 6.0)), seed ^ 0x50).abs();
        let patchy = smoothstep(-0.1, 0.3, noise3(at.at((12.0, 3.0)), seed ^ 0x52));
        let crack = (1.0 - smoothstep(0.0, 0.05, along)) * patchy * at.shows(0.004);
        (knobs, crack)
    }

    /// A birch's bark: white, with its dark lenticels running across it and
    /// thin papery strips peeling up it; about its foot, rising further up
    /// some sides than others, black rough bark, fissured, with the white
    /// showing through in islands. Its twigs are brown.
    fn papery(&self, at: &OnLimb, paint: bool) -> (f64, Vec3) {
        let seed = self.seed;
        let foot =
            1.0 + 1.4 * unit(mix32(seed ^ 0x19)) + 0.7 * noise3(at.at((3.0, 0.6)), seed ^ 0x5);
        let black = 1.0 - smoothstep(foot - 0.5, foot + 0.2, at.along);
        let lenticels = cells3(at.at((42.0, 120.0)), seed ^ 0x41, 0.85);
        let dash = (1.0 - smoothstep(0.25, 0.42, lenticels.nearest))
            * f64::from(u8::from(unit(lenticels.id) < 0.55))
            * at.shows(0.012);
        let (strip, strip_edge) =
            terraced(0.5 + 0.5 * noise3(at.at((5.0, 40.0)), seed ^ 0x62), 2.0);
        let peeling = at.shows(0.02);
        let white = 0.9 + 0.06 * strip * peeling + 0.04 * (1.0 - dash);
        let (ridge, cut) = self.fissures(at, &BIRCH_FOOT);
        let crust = (1.0 - (1.0 - ridge) * (1.0 - ridge)) * (1.0 - 0.5 * cut);
        let height = mix(white, mix(0.62, crust, at.shows(0.06)), black);
        if !paint {
            return (height, Vec3::ZERO);
        }
        // Chalky white, greyed in dusky patches and banded across here and
        // there by dark, roughened rings: the marks that tell a birch from far
        // off, where its lenticels are long lost.
        let dusky = smoothstep(0.1, 0.7, noise3(at.at((2.5, 1.0)), seed ^ 0x9));
        let ring = noise3(at.at((0.9, 3.4)), seed ^ 0x81);
        let banded = (1.0 - smoothstep(0.02, 0.14, ring.abs()))
            * smoothstep(-0.25, 0.35, noise3(at.at((2.2, 0.7)), seed ^ 0x82))
            * at.shows(0.06);
        let paper = self
            .light
            .lerp(self.accent, 0.55 * strip_edge * peeling)
            .lerp(self.light * Vec3::new(0.7, 0.68, 0.66), 0.45 * dusky);
        let marked = paper
            .lerp(self.dark * 1.6, 0.85 * dash)
            .lerp(self.dark * 2.5, 0.8 * banded);
        let islands = smoothstep(0.35, 0.65, noise3(at.at((9.0, 3.0)), seed ^ 0x6a))
            * smoothstep(0.7, 0.95, crust);
        let rough = self
            .dark
            .lerp(self.dark * 3.0, 0.5 * crust)
            .lerp(paper * 0.8, islands);
        let bark = marked.lerp(rough, black);
        let twig = 1.0 - smoothstep(0.012, 0.035, at.girth);
        (height, bark.lerp(BIRCH_TWIG, twig))
    }

    /// Smooth grey bark, mottled in patches of every size.
    fn smooth(&self, at: &OnLimb, paint: bool) -> (f64, Vec3) {
        let seed = self.seed;
        let mottle = noise3(at.at((1.5, 0.8)), seed)
            + 0.5 * noise3(at.at((6.0, 3.5)), seed ^ 0x3)
            + 0.25 * noise3(at.at((24.0, 14.0)), seed ^ 0x4) * at.shows(0.02);
        let height = 0.86 + 0.05 * mottle * at.shows(0.04);
        if !paint {
            return (height, Vec3::ZERO);
        }
        (
            height,
            self.dark.lerp(self.light, smoothstep(-1.1, 1.0, mottle)),
        )
    }

    /// A cherry's bark: glossy, banded across with rows of raised lenticels
    /// and peeling in thin rings between them.
    fn banded(&self, at: &OnLimb, paint: bool) -> (f64, Vec3) {
        let seed = self.seed;
        let lenticels = cells3(at.at((22.0, 55.0)), seed ^ 0x2d, 0.8);
        let band = smoothstep(0.35, 0.7, noise3(at.at((1.5, 6.0)), seed ^ 0x4e));
        let dash =
            (1.0 - smoothstep(0.2, 0.4, lenticels.nearest)) * (0.3 + 0.7 * band) * at.shows(0.015);
        let peel = smoothstep(0.55, 0.8, noise3(at.at((3.0, 25.0)), seed ^ 0x6)) * at.shows(0.02);
        let height = 0.8 + 0.12 * dash - 0.1 * peel;
        if !paint {
            return (height, Vec3::ZERO);
        }
        let colour = self
            .light
            .lerp(self.accent, 0.6 * peel)
            .lerp(self.dark, 0.8 * dash);
        (height, colour)
    }

    /// Thin bark flaking in small rounded scales, each lifting at its lower
    /// edge.
    fn scaly(&self, at: &OnLimb, paint: bool) -> (f64, Vec3) {
        let seed = self.seed;
        let scales = cells3(at.at((34.0, 40.0)), seed, 0.9);
        let dome = 1.0 - smoothstep(0.0, 0.95, scales.nearest);
        let lip = smoothstep(0.0, 0.5, scales.toward.y) * dome;
        let flaking = at.shows(0.025);
        let height = mix(0.7, 0.4 + 0.45 * dome + 0.12 * lip, flaking);
        if !paint {
            return (height, Vec3::ZERO);
        }
        let tone = smoothstep(-0.5, 0.6, noise3(at.at((4.0, 1.0)), seed ^ 0x77));
        let scale = self
            .light
            .lerp(self.light * Vec3::new(1.15, 0.9, 0.8), tone)
            .lerp(Vec3::splat(self.light.max_element()), 0.3 * lip * flaking);
        (height, self.dark.lerp(scale, smoothstep(0.3, 0.75, height)))
    }

    /// A palm's trunk: the narrow scar each old frond left as it fell, one
    /// above another and never quite level, its upper lip a little proud;
    /// between them a face of fibres running up the trunk, split here and
    /// there along them and weathered grey where they stand out.
    fn ringed(&self, at: &OnLimb, paint: bool) -> (f64, Vec3) {
        let seed = self.seed;
        let ring = at.along * 6.5 + 0.2 * noise3(at.at((5.0, 1.0)), seed);
        let within = ring - mathf::floor(ring);
        let scar = 1.0 - smoothstep(0.0, 0.12, within.min(1.0 - within));
        let lip = smoothstep(0.1, 0.2, within) * (1.0 - smoothstep(0.2, 0.42, within));
        let fibres = noise3(at.at((150.0, 5.0)), seed ^ 5) * at.shows(0.006);
        let split = (1.0 - smoothstep(0.0, 0.06, noise3(at.at((40.0, 1.5)), seed ^ 6).abs()))
            * at.shows(0.01);
        let scars = at.shows(0.04);
        let height = (0.82 - 0.5 * scar * scars + 0.1 * lip * scars + 0.05 * fibres - 0.18 * split)
            .clamp(0.0, 1.0);
        if !paint {
            return (height, Vec3::ZERO);
        }
        let weathered = smoothstep(0.0, 0.6, fibres) * 0.25 + 0.15 * lip;
        let face = self.light.lerp(self.accent, weathered);
        (
            height,
            face.lerp(self.dark, (0.85 * scar * scars).max(0.6 * split)),
        )
    }

    /// A cactus's skin folded into `ribs` ribs: rounded crests between V
    /// grooves, a felted cushion every couple of centimetres along each crest
    /// and a crown of felt over the apex; waxy green where it is young,
    /// greying as it ages and corked brown over the oldest. Its folds settle
    /// to their mean where a pixel spans more than the gap between crests.
    fn ribbed(&self, ribs: u8, at: &OnLimb, paint: bool) -> (f64, Vec3) {
        let seed = self.seed;
        let layout = Ribs { count: ribs, seed };
        let stem = at.along;
        let lie = layout.lie(at.angle, stem);
        let pitch = layout.pitch() * at.girth;
        let corked = layout.corked(at.angle, stem);
        let rough =
            noise3(at.at((CORK_ROUGH.0, CORK_ROUGH.0)), seed ^ 0xc0) * at.shows(1.0 / CORK_ROUGH.0);
        let fold = mix(MEAN_RIB, profile(lie.toward), at.shows(pitch));
        // Cork rounds the ribs over and roughens them.
        let rib = mix(fold, 0.3 + 0.5 * fold + CORK_ROUGH.1 * rough, corked);
        let (along, _) = layout.nearest_areole(lie.rib, stem);
        let wool = noise3(at.at((WOOL.0, WOOL.0)), seed ^ 0xf1) * at.shows(1.0 / WOOL.0);
        let felt =
            cushion(along, lie.off * pitch, wool) * at.shows(2.0 * CUSHION.1) * (1.0 - corked);
        let crown = 1.0 - smoothstep(0.5 * CROWN, CROWN, stem);
        let grain = noise3(at.at((CUTICLE.0, CUTICLE.0)), seed ^ 0x9c) * at.shows(1.0 / CUTICLE.0);
        let height =
            ((1.0 - FELT) * rib + FELT * felt.max(0.7 * crown) + CUTICLE.1 * grain).clamp(0.0, 1.0);
        if !paint {
            return (height, Vec3::ZERO);
        }
        let young = 1.0 - smoothstep(0.6, 3.5, stem);
        let mottle = smoothstep(-0.6, 0.6, noise3(at.at((3.0, 1.2)), seed ^ 0x77));
        let crest = smoothstep(0.05, 0.85, fold);
        let skin = self.dark.lerp(self.light, crest) * (0.9 + 0.14 * mottle);
        // Waxy and green while young, its crests weathering greyer with age.
        let skin = skin.lerp(
            AGED_SKIN.lerp(skin, 0.4),
            (1.0 - young) * (0.2 + 0.2 * crest),
        );
        let skin = skin.lerp(CORK[0].lerp(CORK[1], smoothstep(-0.4, 0.5, rough)), corked);
        let tufts = 0.8 + 0.25 * wool;
        let felted = self.accent.lerp(OLD_FELT, smoothstep(0.15, 1.6, stem)) * tufts;
        (
            height,
            skin.lerp(felted, smoothstep(0.05, 0.35, felt).max(crown)),
        )
    }

    /// A taproot's skin: shallow rings across it where its fine roots grew,
    /// a few millimetres apart and broken round it, and faint streaks along
    /// it.
    fn taproot(&self, at: &OnLimb, paint: bool) -> (f64, Vec3) {
        let seed = self.seed;
        let ring = at.along * 180.0 + 1.5 * noise3(at.at((30.0, 6.0)), seed);
        let within = ring - mathf::floor(ring);
        let broken = smoothstep(-0.3, 0.3, noise3(at.at((60.0, 40.0)), seed ^ 0x2e));
        let groove =
            (1.0 - smoothstep(0.0, 0.15, within.min(1.0 - within))) * broken * at.shows(0.004);
        let streak = noise3(at.at((90.0, 3.0)), seed ^ 0x4f) * at.shows(0.003);
        let height = (0.9 - 0.55 * groove + 0.05 * streak).clamp(0.0, 1.0);
        if !paint {
            return (height, Vec3::ZERO);
        }
        let tone = smoothstep(-0.5, 0.6, noise3(at.at((8.0, 2.0)), seed ^ 0x77));
        let skin = self.light.lerp(self.accent, 0.35 * tone);
        (height, skin.lerp(self.dark, 0.7 * groove))
    }

    /// `surface` marked where limbs once grew: an eye with a ridge about it,
    /// and on a birch the black chevron below it, on a beech the brows above.
    fn scarred(&self, at: &OnLimb, (height, colour): (f64, Vec3), paint: bool) -> (f64, Vec3) {
        if at.girth < 0.04 {
            return (height, colour);
        }
        let Some((across, up, key)) = scar(at, self.seed, self.scars()) else {
            return (height, colour);
        };
        let size = at.girth * (0.18 + 0.22 * unit(key));
        let (x, y) = (across / size, up / (size * (0.7 + 0.5 * unit(mix32(key)))));
        let reach = mathf::sqrt(x * x + y * y);
        let shows = at.shows(size);
        let eye = (1.0 - smoothstep(0.55, 0.8, reach)) * shows;
        let rim = (smoothstep(0.6, 0.85, reach) - smoothstep(0.9, 1.25, reach)) * shows;
        let (brow, below) = match self.kind {
            BarkKind::Papery => (0.0, chevron(x, y + 1.6, 3.2) * shows),
            BarkKind::Smooth => (chevron(x, y - 1.3, 2.4) * shows, 0.0),
            _ => (0.0, 0.0),
        };
        let height = (height - 0.35 * eye + 0.12 * rim).clamp(0.0, 1.0);
        if !paint {
            return (height, colour);
        }
        let core = colour.lerp(HEARTWOOD * 0.6, 0.35).lerp(self.dark, 0.5);
        let marked = colour
            .lerp(core, eye)
            .lerp(self.dark, 0.9 * below)
            .lerp(self.dark * 1.3, 0.6 * brow);
        (height, marked)
    }

    /// The chance each place up a trunk holds a scar: a birch is marked black
    /// below nearly every limb it shed, a beech's eyes are many.
    fn scars(&self) -> f64 {
        match self.kind {
            BarkKind::Papery => 0.7,
            BarkKind::Smooth => 0.45,
            _ => 0.3,
        }
    }

    /// `colour` weathered: crusted with lichen in patches on what stands
    /// out, streaked where rain runs down, soiled about the foot.
    fn weathered(&self, at: &OnLimb, colour: Vec3, height: f64) -> Vec3 {
        let seed = self.seed;
        let lichen = match self.kind {
            BarkKind::Ringed | BarkKind::Ribbed { .. } | BarkKind::Banded => 0.0,
            BarkKind::Plated => 0.35,
            _ => 0.7,
        };
        let patch = smoothstep(
            0.25,
            0.5,
            noise3(at.at((4.0, 1.6)), seed ^ 0x1c) + 0.35 * noise3(at.at((22.0, 9.0)), seed ^ 0x2c),
        ) * lichen
            * smoothstep(0.3, 0.8, height);
        let crust = CRUST[0].lerp(
            CRUST[1],
            smoothstep(-0.4, 0.6, noise3(at.at((9.0, 4.0)), seed ^ 0x3c)),
        );
        let streak = smoothstep(0.35, 0.75, noise3(at.at((12.0, 0.35)), seed ^ 0x5c));
        let soiled = 1.0
            - smoothstep(
                0.04,
                0.3 + 0.15 * noise3(at.at((6.0, 3.0)), seed ^ 0x7c),
                at.along,
            );
        colour
            .lerp(crust, 0.8 * patch)
            .lerp(colour * 0.72, 0.5 * streak)
            .lerp(SOIL, 0.75 * soiled)
    }

    /// How much of the bark at `at`, facing `normal` and standing `height`
    /// out of its fissures, moss covers: in patches, over what faces the sky
    /// and about the foot of a trunk, the fissures taken before the crests.
    fn mossed(&self, at: &OnLimb, normal: Vec3, height: f64) -> f64 {
        if self.moss <= 0.0 {
            return 0.0;
        }
        // Moss lies in cushions, crisp at their edges: the more of it, the
        // more of the bark it takes, never a green wash over all of it.
        let facing = smoothstep(0.05, 0.65, normal.y);
        let foot = 0.8 * (1.0 - smoothstep(0.3, 1.6, at.along));
        let spread = 0.25 + 0.3 * height - 0.5 * self.moss * facing.max(foot);
        let cushions = noise3(at.at((7.0, 3.5)), self.seed ^ 0x3d)
            + 0.3 * noise3(at.at((18.0, 10.0)), self.seed ^ 0x3e);
        smoothstep(spread - 0.06, spread + 0.06, cushions) * f64::from(u8::from(self.moss > 0.0))
    }

    /// How much of the bark at `at`, facing `out` and standing `height` out
    /// of its fissures, moss covers, and how far its cushion stands proud of
    /// the bark there, in metres: what a limb cut in true relief is raised
    /// by near the eye.
    pub(crate) fn moss_cushion(&self, at: &OnLimb, out: Vec3, height: f64) -> (f64, f64) {
        let share = self.mossed(at, out, height);
        if share <= 0.0 {
            return (0.0, 0.0);
        }
        let mound = 0.55 + 0.45 * noise3(at.at((MOUNDS, MOUNDS)), self.seed ^ 0x3f);
        (share, MOSS_RISE * smoothstep(0.3, 0.9, share) * mound)
    }

    /// The most moss stands proud of the bark, in metres: nought on a bark
    /// moss never takes.
    pub(crate) fn moss_reach(&self) -> f64 {
        if self.moss > 0.0 {
            MOSS_RISE
        } else {
            0.0
        }
    }

    /// How steeply moss's cushions can rise round and along a limb, a metre
    /// to a metre: their crisp edges, over the patches' finest noise, and
    /// their mounds.
    pub(crate) fn moss_steepest(&self) -> f64 {
        if self.moss > 0.0 {
            MOSS_RISE * (NOISE_SLOPE * 18.0 / 0.12 * 2.5 + NOISE_SLOPE * MOUNDS * 0.45)
        } else {
            0.0
        }
    }

    /// Moss's own colour at `at`: its tufts, lit where they face the sky.
    fn moss_colour(&self, at: &OnLimb, normal: Vec3) -> Vec3 {
        let tufts = smoothstep(-0.3, 0.6, noise3(at.at((14.0, 9.0)), self.seed ^ 0x51));
        MOSS[0].lerp(
            MOSS[1],
            tufts * (0.5 + 0.5 * smoothstep(0.2, 0.9, normal.y)),
        )
    }
}

/// `from` blended toward `to` by `share`.
fn mix(from: f64, to: f64, share: f64) -> f64 {
    from + (to - from) * share
}

/// `value` climbing through `steps` flat terraces a step apart, each rising
/// sharply at its edge — the layered sheets of flaking bark — and how near a
/// terrace's edge it lies, `1.0` on it.
fn terraced(value: f64, steps: f64) -> (f64, f64) {
    let scaled = value * steps;
    let floor = mathf::floor(scaled);
    let rise = smoothstep(0.0, 0.3, scaled - floor);
    ((floor + rise) / steps, 1.0 - rise)
}

/// The nearest scar to `at` among those strewn in rows up the trunk under
/// `seed`, each place holding one by `chance`: how far round the limb and
/// how far along it the point lies from its middle, in metres, and its key;
/// `None` with none in reach.
///
/// Each row holds two places a scar may stand, at angles its key draws, and
/// the distance round is taken the short way: the scars wrap round the limb
/// with no seam, however thick it is.
fn scar(at: &OnLimb, seed: u32, chance: f64) -> Option<(f64, f64, u32)> {
    let row = mathf::floor(at.along / SCAR_ROW);
    let mut nearest: Option<(f64, f64, u32)> = None;
    for step in [-1.0, 0.0, 1.0] {
        let whole = row + step;
        let index = mathf::round_i32(whole).cast_unsigned();
        for place in 0..2u32 {
            let key = hash2(index, place, seed ^ 0x5ca7);
            if unit(key) > chance {
                continue;
            }
            let middle = (whole + unit(mix32(key))) * SCAR_ROW;
            let turned = at.angle - TAU * unit(mix32(key ^ 0x9));
            let turned = crate::vector::wrapped(turned);
            let (across, up) = (turned * at.girth, at.along - middle);
            let farther =
                nearest.is_some_and(|(x, y, _)| x * x + y * y <= across * across + up * up);
            if !farther {
                nearest = Some((across, up, key));
            }
        }
    }
    nearest
}

/// How far `(x, y)` lies within a chevron opening downward from its apex at
/// the origin, `span` wide: `1.0` along its arms, fading off them.
fn chevron(x: f64, y: f64, span: f64) -> f64 {
    let arm = (y + 0.55 * x.abs()).abs();
    (1.0 - smoothstep(0.12, 0.3, arm)) * (1.0 - smoothstep(0.35 * span, 0.5 * span, x.abs()))
}

#[cfg(test)]
#[path = "bark_tests.rs"]
mod tests;
