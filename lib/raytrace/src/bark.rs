//! Bark: the skin of a tree's limbs in relief and in colour — plates and
//! the fissures between them, ridges and furrows, lenticels and scars, the
//! lichen and moss that live on it, and the soil splashed up its foot.
//!
//! A pattern is laid on the limb itself, along its stem and round its girth
//! at their real size in metres. The circle round the limb is carried onto a
//! circle through the pattern's space, so the pattern closes on itself with
//! no seam, and each tree is moved to its own part of that space by the key
//! it was placed under, so no two trees of a kind wear the same bark.

use core::f64::consts::{PI, TAU};

use tairix_util::mathf;

use crate::noise::{cells3, hash2, noise3, smoothstep};
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
    /// Ribs running its length: a cactus.
    Ribbed,
}

/// A bark: its pattern; the colours of its ridges and of its hollows; the
/// colour lichen, or on a plated bark the upper trunk, takes it toward, and
/// from what height up the trunk that upper colour shows; how much snow lies
/// on its limbs; and how much of it moss takes, at most.
#[derive(Clone, Debug)]
pub(crate) struct Bark {
    pub(crate) kind: BarkKind,
    pub(crate) light: Vec3,
    pub(crate) dark: Vec3,
    pub(crate) accent: Vec3,
    pub(crate) rise: f64,
    pub(crate) snow: f64,
    pub(crate) moss: f64,
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

/// Crustose lichen: its paler and its greener crust.
const LICHEN: [Vec3; 2] = [Vec3::new(0.46, 0.5, 0.42), Vec3::new(0.36, 0.44, 0.24)];

/// Moss in its shade, and where it catches the light.
const MOSS: [Vec3; 2] = [Vec3::new(0.055, 0.1, 0.018), Vec3::new(0.19, 0.27, 0.045)];

/// Soil splashed up a trunk's foot.
const SOIL: Vec3 = Vec3::new(0.12, 0.095, 0.07);

/// The bare wood a stripped scar shows, and a birch's twigs, too young to
/// have whitened.
const HEARTWOOD: Vec3 = Vec3::new(0.34, 0.26, 0.18);
const BIRCH_TWIG: Vec3 = Vec3::new(0.2, 0.12, 0.1);

/// Metres between the rows a trunk's scars are strewn in.
const SCAR_ROW: f64 = 0.8;

impl Bark {
    /// How far the bark stands out at `at`, from `0.0` in its deepest cracks
    /// to `1.0` on its plates and ridges.
    pub(crate) fn height(&self, at: &OnLimb) -> f64 {
        self.surface(at, false).0
    }

    /// The colour of the bark at `spot`, with the moss and snow on it.
    pub(crate) fn colour(&self, spot: &Spot) -> Vec3 {
        let at = OnLimb::of(spot);
        let (height, colour) = self.surface(&at, true);
        let hollow = (1.0 - height) * (1.0 - height);
        let colour = colour * (1.0 - self.cavity() * hollow);
        colour
            .lerp(
                self.moss_colour(&at, spot.normal),
                self.mossed(&at, spot.normal),
            )
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
            BarkKind::Ringed | BarkKind::Ribbed => 0.3,
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
            BarkKind::Ribbed => return self.ribbed(at, paint),
        };
        let (height, colour) = self.scarred(at, (height, colour), paint);
        if !paint {
            return (height, colour);
        }
        (height, self.weathered(at, colour, height))
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
        let (ridge, cut) = self.fissures(at, (9.0, 0.7), 0.9);
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

    /// Where `at` lies in a net of fissures running up the limb, `round` to a
    /// metre round it and `up` to a metre along it, parting and joining as
    /// they climb: how far toward the middle of the ridge between two it
    /// lies, from `0.0` in a fissure's floor to `1.0`; and how deep in one of
    /// the broad, shallow breaks crossing the ridges it lies, `breaks` how
    /// often they come.
    fn fissures(&self, at: &OnLimb, (round, up): (f64, f64), breaks: f64) -> (f64, f64) {
        let seed = self.seed;
        let warp = 0.45 * noise3(at.at((0.45 * round, 0.8 * up)), seed ^ 0x11);
        let fine = noise3(at.at((2.7 * round, 2.9 * up)), seed ^ 0x12) * at.shows(0.35 / round);
        let net = noise3(at.at((round, up)) + Vec3::new(warp, 0.0, -warp), seed) + 0.25 * fine;
        let ridge = (net.abs() / 0.45).min(1.0);
        let across = noise3(
            at.at((0.55 * round, 7.0 * up)) + Vec3::new(0.0, warp, 0.0),
            seed ^ 0x1b,
        );
        let patches = smoothstep(
            0.15,
            0.5,
            noise3(at.at((0.7 * round, 2.2 * up)), seed ^ 0x1c),
        );
        let cut =
            (1.0 - smoothstep(0.0, 0.2, across.abs())) * patches * breaks * at.shows(0.5 / round);
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

    /// Furrows parting and joining up the trunk between long rounded ridges,
    /// broken across here and there, the ridges' faces fibrous.
    fn furrowed(&self, at: &OnLimb, paint: bool) -> (f64, Vec3) {
        let seed = self.seed;
        let (ridge, cut) = self.fissures(at, (14.0, 1.0), 0.7);
        let rounded =
            smoothstep(0.18, 0.5, ridge) * (0.7 + 0.3 * (1.0 - (1.0 - ridge) * (1.0 - ridge)));
        let fibre = noise3(at.at((130.0, 6.0)), seed ^ 0x3d) * at.shows(0.005);
        let rough = self.rough(at, (45.0, 14.0));
        let height = mix(
            0.62,
            rounded * (0.88 - 0.4 * cut + 0.08 * rough + 0.05 * fibre),
            at.shows(0.08),
        )
        .clamp(0.0, 1.0);
        if !paint {
            return (height, Vec3::ZERO);
        }
        let tone = smoothstep(-0.5, 0.6, noise3(at.at((4.0, 0.7)), seed ^ 0x77));
        let ridge_colour = self
            .light
            .lerp(self.light * Vec3::new(0.9, 0.92, 0.95), tone)
            .lerp(
                Vec3::splat(self.light.max_element()),
                0.3 * smoothstep(0.8, 1.0, height),
            )
            * (0.9 + 0.1 * fibre);
        let wall = self
            .dark
            .lerp(self.light * 0.5, smoothstep(0.12, 0.4, ridge));
        (
            height,
            wall.lerp(
                ridge_colour,
                smoothstep(0.3, 0.65, ridge) * (1.0 - 0.4 * cut),
            ),
        )
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
        let (ridge, cut) = self.fissures(at, (16.0, 2.0), 0.6);
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

    /// Rings where a palm's old fronds fell, one above another.
    fn ringed(&self, at: &OnLimb, paint: bool) -> (f64, Vec3) {
        let ring = at.along * 6.5 + 0.2 * noise3(at.at((5.0, 1.0)), self.seed);
        let within = ring - mathf::floor(ring);
        let height = smoothstep(0.0, 0.25, within)
            * (0.8 + 0.2 * noise3(at.at((30.0, 30.0)), self.seed ^ 5) * at.shows(0.03));
        let colour = self.dark.lerp(self.light, height);
        (height, if paint { colour } else { Vec3::ZERO })
    }

    /// Ribs running a cactus's length, as many round it however thick.
    fn ribbed(&self, at: &OnLimb, paint: bool) -> (f64, Vec3) {
        let height = 0.5 + 0.5 * mathf::cos(at.angle * 18.0);
        let colour = self.dark.lerp(self.light, height);
        (height, if paint { colour } else { Vec3::ZERO })
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
            BarkKind::Ringed | BarkKind::Ribbed | BarkKind::Banded => 0.0,
            BarkKind::Plated => 0.35,
            _ => 0.7,
        };
        let patch = smoothstep(
            0.25,
            0.5,
            noise3(at.at((4.0, 1.6)), seed ^ 0x1c) + 0.35 * noise3(at.at((22.0, 9.0)), seed ^ 0x2c),
        ) * lichen
            * smoothstep(0.3, 0.8, height);
        let crust = LICHEN[0].lerp(
            LICHEN[1],
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

    /// How much of the bark at `at`, facing `normal`, moss covers: in
    /// patches, over what faces the sky and about the foot of a trunk.
    fn mossed(&self, at: &OnLimb, normal: Vec3) -> f64 {
        if self.moss <= 0.0 {
            return 0.0;
        }
        let facing = smoothstep(0.05, 0.65, normal.y);
        let foot = 0.8 * (1.0 - smoothstep(0.3, 1.6, at.along));
        let patch = smoothstep(-0.25, 0.35, noise3(at.at((3.5, 1.7)), self.seed ^ 0x3d));
        self.moss * facing.max(foot) * patch
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
            let turned = turned - TAU * mathf::floor((turned + PI) / TAU);
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
