//! Moss and lichen: where each grows on stone, wall and roof, how thick it
//! stands, and what colour it is.
//!
//! One reading serves a cover's geometry and its colour: the solid a cover
//! grows on reads what grows where a ray met it and hands that to the
//! pigment, so the cushion of moss seen in relief near the eye and the green
//! it settles to far off lie in the same places. Moss spreads out of the
//! joints, where grit and water gather, in mats of packed cushions whose
//! ragged margins break into the cushions they are made of; lichen grows in
//! colonies young and old, each one species spreading from where it took
//! hold.

use tairix_util::mathf;

use crate::noise::{cells3, cells3_among, noise3, smoothstep, Cells, NOISE_SLOPE};
use crate::pigment::Spot;
use crate::sample::{mix32, unit};
use crate::vector::Vec3;

/// Moss in its shade, and where it catches the light: a dark olive, never
/// the yellow-green of new leaves.
pub(crate) const MOSS: [Vec3; 2] = [Vec3::new(0.035, 0.06, 0.014), Vec3::new(0.1, 0.15, 0.032)];

/// Moss's young tips, and a dry stone's hoary cushion: grey-green, its
/// leaves' white hair points catching the light.
const YOUNG_MOSS: Vec3 = Vec3::new(0.13, 0.155, 0.035);
const HOARY_MOSS: Vec3 = Vec3::new(0.13, 0.14, 0.11);

/// Moss dried out in an exposed summer: golden brown.
const DRY_MOSS: Vec3 = Vec3::new(0.11, 0.085, 0.032);

/// A feather moss's golden green, among the darker cushion mosses.
const GOLDEN_MOSS: Vec3 = Vec3::new(0.12, 0.13, 0.028);

/// Crustose lichen on bark: its paler and its greener crust.
pub(crate) const CRUST: [Vec3; 2] = [Vec3::new(0.46, 0.5, 0.42), Vec3::new(0.36, 0.44, 0.24)];

/// The tallest a cushion of moss stands, and the deepest a lichen's crust
/// and a foliose lichen's lobes lie, in metres.
const MOSS_DEPTH: f64 = 0.022;
const CRUST_DEPTH: f64 = 0.0008;
const LOBE_DEPTH: f64 = 0.0022;

/// How deep a moss cushion's shoots stand apart at its surface, in metres,
/// and how many of them span a metre.
const FIBRE: f64 = 0.0012;
const FIBRES: f64 = 650.0;

/// The most a cover stands out from what it grows on, in metres.
pub(crate) const DEEPEST: f64 = MOSS_DEPTH + FIBRE;

/// How many of moss's coarsest patches span a metre, and each finer octave
/// of them: how many times as many span it, and its share of the whole.
const PATCHES: f64 = 1.4;
const OCTAVES: [(f64, f64); 3] = [(1.0, 0.5), (2.9, 0.3), (8.3, 0.2)];

/// How many swirls of the warp that rags the patches' edges span a metre,
/// and the farthest it pushes them, in metres.
const SWIRLS: f64 = 2.2;
const SWIRL: f64 = 0.12;

/// How many of the cushions moss is packed from span a metre, how far a
/// packed cushion's dome rises over a share of its cell, and the least and
/// the most a lone cushion's reach, as shares of its cell.
const CUSHIONS: f64 = 30.0;
const PACKED: f64 = 0.4;
const LONE: (f64, f64) = (0.25, 0.47);

/// How far from a joint moss creeping out of it reaches, in metres, and how
/// far into its patches that pushes a mat.
const JOINT_REACH: f64 = 0.12;
const JOINT_PULL: f64 = 0.45;

/// How far below and above a wall's foot its splash wets it, in metres.
const SPLASH: (f64, f64) = (0.1, 0.35);

/// How far into its patches a mat rises to its full height; the share of
/// that the crevices between its cushions stand at; and the least share
/// that stands at all, where a mat thins at its margin to the cushions it
/// breaks into.
const TAPER: f64 = 0.25;
const CREVICE: f64 = 0.35;
const BREAK: f64 = 0.12;

/// How many young and old colonies' cells and lichen's patches span a
/// metre.
const COLONIES: f64 = 9.0;
const OLD_COLONIES: f64 = 2.0;
const LICHEN_PATCHES: f64 = 0.9;

/// How many of the patches where birds perch, and the bright lichens that
/// feed on what they leave grow, span a metre.
const PERCHES: f64 = 0.5;

/// The least reach of a colony, as a share of its cell, and how much more
/// the widest reach.
const LEAST_REACH: f64 = 0.25;
const MORE_REACH: f64 = 0.45;

/// How far a colony's margin wanders, as shares of its reach, in lobes as
/// many as these span its reach.
const WANDER: [(f64, f64); 2] = [(3.5, 0.14), (11.0, 0.07)];

/// How far inside where a crust meets its neighbour its margin's colour
/// and its rise reach, in metres.
const BORDER: f64 = 0.012;

/// How many swirls of the warp old crusts are found through, so where they
/// meet wanders, span one of their cells, and the farthest it pushes them,
/// as a share of a cell.
const MEETING: f64 = 1.7;
const MEANDER: f64 = 0.35;

/// How many of a crust's areoles, the cracked plates it grows in, span a
/// metre, and how many of the mottles of a colony's thallus.
const AREOLES: f64 = 250.0;
const MOTTLES: f64 = 45.0;

/// What a cover's stone is to the lichens on it: lime-rich, as limestone,
/// marble, mortar and brick are, or acid, as granite, sandstone and slate.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Substrate {
    Calcareous,
    Siliceous,
}

/// How a structure's stone is covered.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Cover {
    /// How much of what suits it moss takes, `0.0..=1.0`.
    pub(crate) moss: f64,
    /// How thickly lichen colonises its faces, `0.0..=1.0`.
    pub(crate) lichen: f64,
    /// How damp the place is, `0.0..=1.0`: moss climbs upright faces and
    /// dries out less the damper.
    pub(crate) damp: f64,
    /// How dry the season is, `0.0..=1.0`: exposed moss browns.
    pub(crate) drought: f64,
    /// The height, in the structure's own frame, below which splash and
    /// damp let moss take any face.
    pub(crate) foot: f64,
    /// The way the structure's shaded side faces, level and unit, in its own
    /// frame: the side moss climbs; nought where no side stays shaded.
    pub(crate) shade: Vec3,
    pub(crate) substrate: Substrate,
    pub(crate) seed: u32,
}

/// What grows at a point.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) enum Growth {
    Bare,
    /// Moss, and how far up its cushion's dome, `0.0` at its rim and `1.0`
    /// at its crown.
    Moss(f64),
    /// A lichen colony: its species and how far in from its margin, `0.0`
    /// at the edge and `1.0` at its heart.
    Lichen(Lichen, f64),
}

impl Growth {
    /// What grows, as a surface's coordinates carry it from the solid it was
    /// found on to the pigment it is shaded in: how far into its moss or its
    /// colony, and which it is.
    pub(crate) fn carried(self) -> (f64, f64) {
        match self {
            Self::Bare => (0.0, -1.0),
            Self::Moss(dome) => (dome, 0.0),
            Self::Lichen(lichen, inside) => (inside, f64::from(lichen.index() + 1)),
        }
    }

    /// What grows where a surface's coordinates `uv` carried it.
    fn of_carried((within, which): (f64, f64)) -> Self {
        match which {
            w if w < -0.5 => Self::Bare,
            w if w < 0.5 => Self::Moss(within),
            w => usize::try_from(mathf::round_i32(w - 1.0))
                .ok()
                .and_then(|index| Lichen::ALL.get(index))
                .map_or(Self::Bare, |&lichen| Self::Lichen(lichen, within)),
        }
    }
}

/// The lichens of stone.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Lichen {
    /// *Xanthoria parietina*: orange leafy rosettes where birds perch.
    Xanthoria,
    /// *Caloplaca*: an orange crust on lime-rich stone.
    Caloplaca,
    /// *Lecanora muralis*: pale green-grey rosettes on walls.
    Lecanora,
    /// *Rhizocarpon geographicum*: the yellow-green map lichen, its areoles
    /// edged in black, on acid rock.
    Rhizocarpon,
    /// A grey crust.
    Grey,
    /// *Ochrolechia*: a thick white crust.
    White,
    /// *Verrucaria nigrescens*: a black crust on limestone.
    Verrucaria,
}

impl Lichen {
    const ALL: [Self; 7] = [
        Self::Xanthoria,
        Self::Caloplaca,
        Self::Lecanora,
        Self::Rhizocarpon,
        Self::Grey,
        Self::White,
        Self::Verrucaria,
    ];

    fn index(self) -> u8 {
        match self {
            Self::Xanthoria => 0,
            Self::Caloplaca => 1,
            Self::Lecanora => 2,
            Self::Rhizocarpon => 3,
            Self::Grey => 4,
            Self::White => 5,
            Self::Verrucaria => 6,
        }
    }

    /// The species a colony keyed `key` is, on `substrate`, where `bright`
    /// of the colonies are the orange lichens that feed on what birds leave;
    /// an `old` colony, grown wide, a crust.
    fn of(key: u32, substrate: Substrate, bright: f64, old: bool) -> Self {
        let pick = unit(mix32(key ^ 0x51c4));
        if old {
            return match (substrate, pick) {
                (Substrate::Calcareous, p) if p < 0.4 => Self::Lecanora,
                (Substrate::Calcareous, p) if p < 0.93 => Self::Grey,
                (Substrate::Calcareous, _) => Self::Verrucaria,
                (Substrate::Siliceous, p) if p < 0.3 => Self::Rhizocarpon,
                (Substrate::Siliceous, p) if p < 0.55 => Self::Lecanora,
                (Substrate::Siliceous, p) if p < 0.85 => Self::Grey,
                (Substrate::Siliceous, _) => Self::White,
            };
        }
        match substrate {
            Substrate::Calcareous => match pick {
                p if p < 0.5 * bright => Self::Caloplaca,
                p if p < bright => Self::Xanthoria,
                p if p < bright + 0.38 => Self::Lecanora,
                p if p < bright + 0.95 => Self::Grey,
                _ => Self::Verrucaria,
            },
            Substrate::Siliceous => match pick {
                p if p < 0.5 * bright => Self::Xanthoria,
                p if p < 0.12 + 0.5 * bright => Self::Rhizocarpon,
                p if p < 0.4 + 0.5 * bright => Self::Lecanora,
                p if p < 0.75 => Self::Grey,
                _ => Self::White,
            },
        }
    }

    /// Whether it grows in leafy lobes rather than a crust.
    const fn foliose(self) -> bool {
        matches!(self, Self::Xanthoria)
    }

    /// Its thallus's colour.
    const fn colour(self) -> Vec3 {
        match self {
            Self::Xanthoria => Vec3::new(0.58, 0.3, 0.05),
            Self::Caloplaca => Vec3::new(0.52, 0.2, 0.05),
            Self::Lecanora => Vec3::new(0.42, 0.44, 0.34),
            Self::Rhizocarpon => Vec3::new(0.46, 0.48, 0.14),
            Self::Grey => Vec3::new(0.4, 0.4, 0.36),
            Self::White => Vec3::new(0.5, 0.5, 0.46),
            Self::Verrucaria => Vec3::new(0.1, 0.095, 0.085),
        }
    }

    /// The colour of its fruiting discs.
    const fn fruit(self) -> Vec3 {
        match self {
            Self::Xanthoria | Self::Caloplaca => Vec3::new(0.55, 0.12, 0.02),
            Self::Lecanora | Self::Grey => Vec3::new(0.3, 0.22, 0.1),
            Self::Rhizocarpon | Self::Verrucaria => Vec3::new(0.012, 0.012, 0.012),
            Self::White => Vec3::new(0.62, 0.55, 0.45),
        }
    }
}

/// The black of a map lichen's prothallus, between its areoles and about
/// its margin.
const PROTHALLUS: Vec3 = Vec3::new(0.014, 0.014, 0.013);

/// How much of a cover's relief shows: its cushions, its shoots, and its
/// crusts, each `0.0..=1.0`.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Shown {
    pub(crate) cushions: f64,
    pub(crate) shoots: f64,
    pub(crate) crusts: f64,
}

impl Shown {
    /// How much shows where a pixel covers `footprint` metres: each part as
    /// it spans enough pixels, `spans` saying how many it must span to show
    /// at all and to show in full.
    pub(crate) fn at(footprint: f64, spans: (f64, f64)) -> Self {
        let shows = |depth: f64| smoothstep(spans.0, spans.1, depth / footprint.max(1e-12));
        Self {
            cushions: shows(MOSS_DEPTH),
            shoots: shows(FIBRE),
            crusts: shows(CRUST_DEPTH),
        }
    }

    pub(crate) fn any(self) -> bool {
        self.cushions > 0.0 || self.shoots > 0.0 || self.crusts > 0.0
    }
}

/// Where on a unit a cover grows: the point, in its structure's own frame
/// and metres; the way the unit's dressed form faces there; how readily moss
/// lodges on the unit, a joint's mortar most and a dressed face least; and
/// how far the point lies from the unit's nearest joint, in metres.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Lodging {
    pub(crate) p: Vec3,
    pub(crate) normal: Vec3,
    pub(crate) affinity: f64,
    pub(crate) joint: f64,
}

impl Cover {
    /// What grows `at`.
    pub(crate) fn growth(&self, at: &Lodging) -> Growth {
        if let Some(height) = self.moss_at(at) {
            return Growth::Moss(height);
        }
        match self.colony(at) {
            Some((lichen, inside)) => Growth::Lichen(lichen, inside),
            None => Growth::Bare,
        }
    }

    /// How far the cover `at` stands out from the face it grows on, in
    /// metres, as much of its relief as `shown` showing.
    pub(crate) fn depth(&self, at: &Lodging, shown: Shown) -> f64 {
        if let Some(height) = self.moss_at(at) {
            // Its shoots stand from its cushions, never from bare stone.
            let tips = if shown.shoots > 0.0 {
                shown.shoots * FIBRE * shoots(at.p, self.seed) * smoothstep(0.0, 0.1, height)
            } else {
                0.0
            };
            return shown.cushions * MOSS_DEPTH * height + tips;
        }
        // A crust shows only nearer than a cushion does, so no colony need
        // be found where none would show.
        if shown.crusts <= 0.0 {
            return 0.0;
        }
        match self.colony(at) {
            Some((lichen, inside)) => {
                let deep = if lichen.foliose() {
                    LOBE_DEPTH
                } else {
                    CRUST_DEPTH
                };
                shown.crusts * deep * smoothstep(0.0, 0.25, inside)
            }
            None => 0.0,
        }
    }

    /// How steeply its depth can rise along a ray, a metre to a metre, as
    /// much of its relief as `shown` showing: what a march must step within
    /// it by.
    pub(crate) fn steepest(&self, shown: Shown) -> f64 {
        let mossed = if self.moss > 0.0 {
            MOSS_DEPTH * moss_slope() * shown.cushions + 3.75 * FIBRES * FIBRE * shown.shoots
        } else {
            0.0
        };
        let colonised = if self.lichen > 0.0 {
            // A colony rises across a quarter of its share inside the
            // smallest one's margin, as fast as that margin wanders; a crust
            // as fast inside where it meets its neighbour.
            let wander: f64 = WANDER
                .iter()
                .map(|&(lobes, share)| NOISE_SLOPE * lobes * share)
                .sum();
            let lobed = LOBE_DEPTH * (1.0 + wander) * COLONIES / LEAST_REACH;
            // Where crusts meet, as fast as the warp they are found through
            // swings it.
            let met = CRUST_DEPTH / BORDER * (1.0 + NOISE_SLOPE * MEETING * MEANDER);
            1.5 / 0.25 * lobed.max(met) * shown.crusts
        } else {
            0.0
        };
        mossed + colonised
    }

    /// How much moss stands `at`, if any grows there: its height as a share
    /// of the tallest a cushion stands. Moss mantles what faces the sky in
    /// mats spreading from the joints, climbs a damp wall's shaded side and
    /// its splashed foot, and lodges a lone cushion here and there beyond.
    fn moss_at(&self, at: &Lodging) -> Option<f64> {
        // Nothing clings to a face worn smooth.
        if self.moss <= 0.0 || at.affinity <= 0.0 {
            return None;
        }
        let normal = at.normal;
        let under = smoothstep(-0.4, -0.1, normal.y);
        if under <= 0.0 {
            return None;
        }
        let up = smoothstep(0.3, 0.75, normal.y);
        let level = Vec3::new(normal.x, 0.0, normal.z);
        let shaded = smoothstep(0.0, 0.8, level.dot(self.shade));
        let foot = 1.0 - smoothstep(self.foot - SPLASH.0, self.foot + SPLASH.1, at.p.y);
        let joint = 1.0 - smoothstep(0.0, JOINT_REACH, at.joint);
        let wet = up + (1.0 - up) * under * self.damp * (0.35 * shaded + 0.65 * foot);
        let cells = at.p * CUSHIONS;
        let size = |id: u32| 0.6 + 0.4 * unit(mix32(id ^ 0x3e));
        let reach = 1.2 * self.moss * wet * (0.3 + at.affinity) - 0.55;
        let margin = self.patches(at.p) + JOINT_PULL * joint * at.affinity + reach;
        let mat = if margin > 0.0 {
            let cushion = cells3(cells, self.seed ^ 0x3d, 0.85);
            let packed = smoothstep(0.0, PACKED, cushion.wall());
            let height = smoothstep(0.0, TAPER, margin)
                * (CREVICE + (1.0 - CREVICE) * packed * size(cushion.id));
            (height - BREAK) / (1.0 - BREAK)
        } else {
            0.0
        };
        // Beyond its mats moss lodges where grit gathers: a joint, the
        // splashed foot of a wall, its damp shaded side.
        let density = self.moss
            * at.affinity
            * under
            * (0.6 * joint + 0.04 * up + (1.0 - up) * self.damp * (0.1 * shaded + 0.4 * foot));
        let lone = if density > 0.0 {
            let lodged = cells3_among(cells, self.seed ^ 0x3d, 0.85, |id| unit(id) < density);
            let reach = LONE.0 + (LONE.1 - LONE.0) * unit(mix32(lodged.id ^ 0x3f));
            let r = lodged.nearest / reach;
            (1.0 - r * r) * size(lodged.id)
        } else {
            0.0
        };
        let height = mat.max(lone);
        (height > 0.0).then_some(height.min(1.0))
    }

    /// How far into moss's patches `p` lies: below nought outside them,
    /// ragged at every scale by the warp swirling them.
    fn patches(&self, p: Vec3) -> f64 {
        let swirl = Vec3::new(
            noise3(p * SWIRLS, self.seed ^ 0x6a),
            0.0,
            noise3(p * SWIRLS, self.seed ^ 0x6b),
        ) * SWIRL;
        let q = (p + swirl) * PATCHES;
        OCTAVES
            .iter()
            .zip(0u32..)
            .map(|(&(scale, share), salt)| share * noise3(q * scale, self.seed ^ (0x6d + salt)))
            .sum()
    }

    /// The colony of lichen `at`, if one grows there: its species and how
    /// far within its margin. Young colonies stand among the wide crusts of
    /// old ones, the bright lichens where birds perch on what faces the sky.
    fn colony(&self, at: &Lodging) -> Option<(Lichen, f64)> {
        if self.lichen <= 0.0 {
            return None;
        }
        let (p, normal) = (at.p, at.normal);
        // Lichens take the faces rain wets and dries, and least the
        // undersides it never reaches.
        let suits =
            smoothstep(-0.5, -0.05, normal.y) * (0.55 + 0.45 * smoothstep(0.0, 0.8, normal.y));
        // Lichen keeps to where water runs and lingers: in patches, not
        // evenly over a face.
        let patch = smoothstep(-0.25, 0.45, noise3(p * LICHEN_PATCHES, self.seed ^ 0x1c4d));
        let thick = self.lichen * suits * (0.25 + 1.5 * patch);
        let up = smoothstep(0.3, 0.8, normal.y);
        let perch = smoothstep(0.15, 0.6, noise3(p * PERCHES, self.seed ^ 0x1c50));
        let bright = 0.01 + 0.25 * up * perch;
        // Young colonies stand alone, most of them small, here and there on
        // the old.
        let young = cells3_among(p * COLONIES, self.seed ^ 0x1c4e, 0.9, |id| {
            unit(id) < 0.5 * thick
        });
        if young.nearest.is_finite() {
            let inside = rounded(p, &young, COLONIES, 0.0);
            if inside > 0.0 {
                return Some((
                    Lichen::of(young.id, self.substrate, bright, false),
                    inside.min(1.0),
                ));
            }
        }
        // Old crusts, where lichen grows thick, have spread until they meet,
        // a mosaic whose joins wander.
        let cell = p * OLD_COLONIES;
        let meander = Vec3::new(
            noise3(cell * MEETING, self.seed ^ 0x1c51),
            noise3(cell * MEETING, self.seed ^ 0x1c52),
            noise3(cell * MEETING, self.seed ^ 0x1c53),
        ) * MEANDER;
        let old = cells3(cell + meander, self.seed ^ 0x1c4f, 0.9);
        if unit(old.id) >= thick {
            return None;
        }
        let spread = smoothstep(0.3, 1.0, thick);
        let inside = rounded(p, &old, OLD_COLONIES, spread).min(old.wall() / OLD_COLONIES / BORDER);
        (inside > 0.0).then(|| {
            (
                Lichen::of(old.id, self.substrate, bright, true),
                inside.min(1.0),
            )
        })
    }

    /// The colour of the cover at `spot`, which its solid found growing there
    /// and carried in the spot's coordinates.
    pub(crate) fn colour(&self, spot: &Spot) -> Vec3 {
        match Growth::of_carried(spot.uv) {
            Growth::Moss(height) => self.moss_colour(spot, height),
            Growth::Lichen(lichen, inside) => lichen_colour(spot, lichen, inside),
            // Nothing grows: the deep shade between moss shoots, where a ray
            // can only have found the cover's own underside.
            Growth::Bare => MOSS[0],
        }
    }

    /// Moss's colour `height` up its cushion: dark in its crevices and
    /// between its tufts, its crown and young tips paler, each mat one kind
    /// or another, a dry exposed cushion hoary, and browning in a dry
    /// season.
    fn moss_colour(&self, spot: &Spot, height: f64) -> Vec3 {
        let shows = 1.0 - smoothstep(0.25, 1.5, spot.width * CUSHIONS);
        let tufts = 0.5 + 0.5 * shows * noise3(spot.p * (1.7 * CUSHIONS), self.seed ^ 0x51);
        let lit = tufts * (0.35 + 0.65 * height);
        let green = MOSS[0].lerp(MOSS[1], lit).lerp(YOUNG_MOSS, 0.4 * lit * lit);
        let kind = smoothstep(
            0.05,
            0.45,
            noise3(spot.p * (2.0 * PATCHES), self.seed ^ 0x53),
        );
        let mixed = green.lerp(GOLDEN_MOSS * (0.55 + 0.6 * lit), 0.7 * kind);
        let exposed = smoothstep(0.4, 0.9, spot.normal.y) * (1.0 - self.damp);
        let hoary = mixed.lerp(HOARY_MOSS, 0.7 * exposed);
        let browned = self.drought
            * exposed
            * smoothstep(
                -0.3,
                0.4,
                noise3(spot.p * (4.0 * PATCHES), self.seed ^ 0x52),
            );
        hoary.lerp(DRY_MOSS, 0.8 * browned)
    }
}

/// How far inside its margin `p` lies in the colony `found` about it among
/// cells `scale` to a metre: `0.0` on its margin, and up to `1.0` at its
/// heart. A colony reaches a share of its cell, most of them little, and
/// `spread` of a cell more where lichen grows thick; its margin wanders in
/// lobes.
fn rounded(p: Vec3, found: &Cells, scale: f64, spread: f64) -> f64 {
    let draw = unit(mix32(found.id ^ 0x77));
    let reach = (LEAST_REACH + MORE_REACH * draw * draw + spread) / scale;
    let wander: f64 = WANDER
        .iter()
        .zip(0u32..)
        .map(|(&(lobes, share), salt)| share * noise3(p * (lobes / reach), found.id ^ salt))
        .sum();
    1.0 - found.nearest / scale / (reach * (1.0 + wander))
}

/// How steeply moss's height share can rise, a metre to a metre: a mat's
/// taper across its swirled patches, its joints and its splashed foot, and
/// the packed cushions it rises in; or a lone cushion's dome.
fn moss_slope() -> f64 {
    let octaves: f64 = OCTAVES.iter().map(|&(scale, share)| scale * share).sum();
    let patches = NOISE_SLOPE * PATCHES * octaves * (1.0 + NOISE_SLOPE * SWIRLS * SWIRL);
    // How a mat's reach climbs a wall with its splash, at the most.
    let splash = 1.2 * 1.3 * 0.65 * 1.5 / (SPLASH.0 + SPLASH.1);
    let margin = patches + JOINT_PULL * 1.5 / JOINT_REACH + splash;
    let mat = (1.5 / TAPER * margin + (1.0 - CREVICE) * 1.5 / PACKED * CUSHIONS) / (1.0 - BREAK);
    let lone = 2.0 / LONE.0 * CUSHIONS;
    mat.max(lone)
}

/// A moss's shoots standing at `p`: `1.0` on a shoot's tip, nought between.
fn shoots(p: Vec3, seed: u32) -> f64 {
    smoothstep(0.55, 0.15, cells3(p * FIBRES, seed ^ 0x3f, 1.0).nearest)
}

/// A lichen's colour `inside` its colony: its thallus, a map lichen's areoles
/// edged in its prothallus and its margin black, a crust's fruiting discs,
/// each settling to its mean where `spot` is too wide to show it.
fn lichen_colour(spot: &Spot, lichen: Lichen, inside: f64) -> Vec3 {
    let seed = mix32(u32::from(lichen.index()) ^ 0x7a1);
    let shows = |count: f64| 1.0 - smoothstep(0.3, 1.5, spot.width * count);
    let thallus =
        lichen.colour() * (1.0 + 0.24 * shows(MOTTLES) * noise3(spot.p * MOTTLES, seed ^ 0x3));
    let fine = shows(600.0);
    let colour = match lichen {
        Lichen::Rhizocarpon => {
            let lines = if fine > 0.0 {
                1.0 - smoothstep(0.03, 0.09, cells3(spot.p * 600.0, seed, 0.9).wall())
            } else {
                0.0
            };
            let rim = 1.0 - smoothstep(0.04, 0.12, inside);
            thallus.lerp(PROTHALLUS, (fine * lines).max(rim).max(0.18 * (1.0 - fine)))
        }
        Lichen::Verrucaria | Lichen::Grey | Lichen::White | Lichen::Lecanora => {
            // A crust grows in cracked plates, and where it meets another its
            // dark prothallus lines the join.
            let areoles = shows(AREOLES);
            let cracks = if areoles > 0.0 {
                areoles
                    * (1.0
                        - smoothstep(0.03, 0.08, cells3(spot.p * AREOLES, seed ^ 0x7, 0.9).wall()))
            } else {
                0.0
            };
            let rim = 1.0 - smoothstep(0.02, 0.1, inside);
            thallus
                .lerp(thallus * 0.55, 0.7 * cracks)
                .lerp(PROTHALLUS, 0.65 * rim)
        }
        _ => {
            let fruit = if fine > 0.0 {
                let discs = cells3(spot.p * 380.0, seed ^ 0x9, 0.8);
                (1.0 - smoothstep(0.18, 0.26, discs.nearest))
                    * smoothstep(0.25, 0.6, inside)
                    * f64::from(u8::from(unit(discs.id) < 0.5))
            } else {
                0.0
            };
            thallus.lerp(lichen.fruit(), fine * fruit + 0.06 * (1.0 - fine))
        }
    };
    // A colony is palest at its growing margin and darkest where oldest.
    colour * (0.86 + 0.18 * smoothstep(0.0, 0.5, 1.0 - inside))
}

#[cfg(test)]
#[path = "cover_tests.rs"]
mod tests;
