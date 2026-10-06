//! How a wood stands on a land: where its trees grow and what each one is,
//! read from the place alone, so the trees stood one by one about the eye and
//! those hashed from the cells of the wood far off grow alike.
//!
//! A wood covers the ground in patches, opens in gaps where trees fell, and
//! within them grows in stands of its kinds, each stand as tall as it is old;
//! each kind keeps to the ground it takes to, and the ground's slope, wet and
//! growth, its roads and paths, and the heights it keeps between decide
//! whether a tree roots there at all.

use core::f64::consts::TAU;

use tairix_util::mathf;

use crate::heightfield::Heightfield;
use crate::land::{Grids, Lie};
use crate::noise::{cells2, fbm2, smoothstep};
use crate::sample::{mix32, unit};
use crate::shade::Shade;
use crate::vector::{Frame, Pose, Vec3};

/// How a wood grows over a land.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Woodland {
    /// The share of the ground that suits trees the wood covers, and how
    /// broad its patches, and the open ground between them, are.
    pub(crate) cover: f64,
    pub(crate) patch: f64,
    /// How far apart trunks stand, as a share of their two crowns' reaches
    /// together, where the wood grows thickest and where it is most open.
    pub(crate) closure: (f64, f64),
    /// How much of their kinds' grown height its youngest and oldest stands
    /// reach.
    pub(crate) stature: (f64, f64),
    /// The share of the places a gap may open in the canopy where one has:
    /// where a tree or a stand of them fell, and light reaches the floor.
    pub(crate) gaps: f64,
    /// The most trees it stands one by one.
    pub(crate) most: u32,
    /// How far ahead of the eye, in heights of a tree, and how far either
    /// side of the view in radians it keeps clear, so that no tree walls the
    /// view off.
    pub(crate) open: (f64, f64),
}

/// The ground a wood takes to, beyond the somewhere to root, off roads and
/// paths and out of the water, that every tree needs.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Rooting {
    /// How upright the ground stands where trees begin taking to it, and
    /// where they take to it fully.
    pub(crate) upright: (f64, f64),
    /// How strongly wet ground draws trees to it, out of the wood's patches
    /// as well as in them: willows along a stream across open fields.
    pub(crate) streams: f64,
    /// How readily it roots where nothing grows green: under snow, where the
    /// land's green gives out though the trees stand on.
    pub(crate) bare: f64,
    /// The heights over which it comes to take to the land, going up, and
    /// over which it gives out.
    pub(crate) above: Option<(f64, f64)>,
    pub(crate) below: Option<(f64, f64)>,
    /// A clearing it keeps out of: its middle, and how far it reaches.
    pub(crate) clearing: Option<((f64, f64), f64)>,
}

/// Ground as level as most trees want it, anywhere on the land.
pub(crate) const ANYWHERE: Rooting = Rooting {
    upright: (0.74, 0.88),
    streams: 0.0,
    bare: 0.0,
    above: None,
    below: None,
    clearing: None,
};

impl Rooting {
    /// How well the ground `lie` describes at `at` suits the wood's trees,
    /// `0.0..=1.0`.
    pub(crate) fn suits(&self, lie: &Lie, at: (f64, f64)) -> f64 {
        if let Some((middle, reach)) = self.clearing {
            if mathf::hypot(at.0 - middle.0, at.1 - middle.1) < reach {
                return 0.0;
            }
        }
        let above = self
            .above
            .map_or(1.0, |(low, high)| smoothstep(low, high, lie.height));
        let below = self
            .below
            .map_or(1.0, |(low, high)| 1.0 - smoothstep(low, high, lie.height));
        lie.green.max(self.bare)
            * smoothstep(self.upright.0, self.upright.1, lie.upright)
            * (1.0 - lie.road)
            * (1.0 - 0.95 * lie.path)
            * above
            * below
    }
}

/// How readily a kind takes to ground against the other kinds of its wood:
/// `base` and as much more as the ground is `wet` and as `rich` as it grows,
/// never less than `least`.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Affinity {
    pub(crate) base: f64,
    pub(crate) wet: f64,
    pub(crate) rich: f64,
    pub(crate) least: f64,
}

impl Affinity {
    /// Taking to every ground alike.
    pub(crate) const EVEN: Self = Self {
        base: 1.0,
        wet: 0.0,
        rich: 0.0,
        least: 0.0,
    };

    fn of(&self, lie: &Lie) -> f64 {
        (self.base + self.wet * lie.wet + self.rich * lie.green).max(self.least)
    }
}

/// How many trees of each kind a scene grows to choose among: grown to
/// heights spread from a sapling's past the shortest of the kind grown up to
/// its tallest, so a wood stands young trees beneath its old ones.
pub(crate) const VARIANTS: usize = 4;

/// The most kinds one wood holds.
pub(crate) const KINDS: usize = 6;

/// A kind's grown trees, as a wood reads and stands them: how readily the
/// kind takes to ground, the prototypes it placed, how tall each grew, the
/// bark they are made of, how far their crowns spread for their height, and
/// the least and most a tree of it is scaled by.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Habit {
    pub(crate) affinity: Affinity,
    pub(crate) prototypes: [u32; VARIANTS],
    pub(crate) heights: [f64; VARIANTS],
    pub(crate) bark: usize,
    pub(crate) crown: f64,
    pub(crate) scaled: (f64, f64),
}

impl Habit {
    /// Which of the kind's grown trees stands nearest `height` tall: between
    /// the two nearest, as `draw` falls, so a wood of one height is not one
    /// tree over and over.
    pub(crate) fn nearest(&self, height: f64, draw: f64) -> usize {
        let wanted = height.max(1e-3);
        // How many times the taller of the two is the other's height: ranked
        // as the gap between their logarithms, without taking one.
        let apart = |natural: f64| {
            let natural = natural.max(1e-3);
            (wanted / natural).max(natural / wanted)
        };
        // The nearest two, the earlier first where two lie as near.
        let (mut first, mut second) = ((0, f64::INFINITY), (0, f64::INFINITY));
        for (variant, &natural) in self.heights.iter().enumerate() {
            let apart = apart(natural);
            if apart < first.1 {
                second = first;
                first = (variant, apart);
            } else if apart < second.1 {
                second = (variant, apart);
            }
        }
        // The second nearest only while it is still near.
        if second.1 < NEAR && draw < 0.5 {
            second.0
        } else {
            first.0
        }
    }

    /// How far its tree grown `natural` tall is scaled to stand `height`
    /// tall.
    pub(crate) fn sized(&self, height: f64, natural: f64) -> f64 {
        (height / natural.max(1e-3)).clamp(self.scaled.0, self.scaled.1)
    }

    /// The tallest any tree of the kind stands.
    pub(crate) fn tallest(&self) -> f64 {
        self.scaled.1 * self.heights.iter().copied().fold(0.0, f64::max)
    }

    /// The tallest a tree of the kind wanted no taller than `wanted` stands:
    /// grown from one of the two grown trees nearest its height, so from none
    /// taller than the second above `wanted`, and scaled toward it within
    /// its bounds.
    pub(crate) fn highest(&self, wanted: f64) -> f64 {
        // The two shortest grown taller than `wanted`.
        let (mut first, mut second) = (f64::INFINITY, f64::INFINITY);
        for &natural in self.heights.iter().filter(|&&natural| natural > wanted) {
            if natural < first {
                second = first;
                first = natural;
            } else if natural < second {
                second = natural;
            }
        }
        let grown = if second.is_finite() {
            second
        } else if first.is_finite() {
            first
        } else {
            self.heights.iter().copied().fold(0.0, f64::max)
        };
        (grown * self.scaled.1).min(wanted.max(grown * self.scaled.0))
    }
}

/// What a place alone says of the tree a wood would grow there, before the
/// ground is read: where it stands, its draws, how fully the wood's patches
/// cover the place with its gaps opened, the light reaching it beneath a
/// canopy, and how old its stand is, `0.0..=1.0`.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Sprout {
    pub(crate) at: (f64, f64),
    draw: u32,
    in_patches: f64,
    light: f64,
    age: f64,
}

/// A tree as the ground and the wood would grow it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct Tree {
    /// Which of the wood's kinds, which of that kind's grown trees, and how
    /// much of that tree's size.
    pub(crate) kind: u8,
    pub(crate) variant: u8,
    pub(crate) scale: f64,
    pub(crate) height: f64,
    pub(crate) reach: f64,
    /// How far apart it stands from others, as a share of their crowns'
    /// reaches together.
    pub(crate) apart: f64,
    /// The height its trunk is based at.
    pub(crate) base: f64,
}

impl Tree {
    /// Where the tree read at `at` from a place drawn as `draw` stands, turned
    /// about its trunk, and the key that sets it apart from the rest.
    pub(crate) fn placing(&self, at: (f64, f64), draw: u32) -> (Pose, u32) {
        let turn = TAU * unit(mix32(draw ^ 0x510e_527f));
        (
            Pose::new(Vec3::new(at.0, self.base, at.1), Frame::turned(turn, 0.0)),
            mix32(draw ^ 0x9b05_688c),
        )
    }
}

/// What a wood's trees are read by: how it grows, the ground it takes to,
/// its kinds, and the seeds its patches, and its stands and kinds, are laid
/// out under.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Reader {
    pub(crate) woodland: Woodland,
    pub(crate) rooting: Rooting,
    pub(crate) kinds: [Option<Habit>; KINDS],
    pub(crate) seeds: (u32, u32),
}

/// How much of the ground crowns thrown down at random, no closer than they
/// allow, come to fill: the jamming limit of random sequential adsorption.
pub(crate) const PACKED: f64 = 0.55;

/// How broad the stands a wood's ages and kinds come in are, in metres.
const STANDS: f64 = 140.0;

/// How many times another grown tree's height a tree may be and still stand
/// near it: e^¼, a quarter apart in logarithm.
const NEAR: f64 = 1.284_025_416_687_741_4;

/// How much openness a wood's edge adds to the spacing of its trees.
pub(crate) const EDGE_OPENING: f64 = 0.4;

/// How broad the stretches a wood grows thick or open in are, as a share of
/// its patches' breadth.
const DENSITY_BREADTH: f64 = 0.45;

/// How far apart the places a gap may open in a canopy lie, and how broad
/// one is, least and most: a fallen tree's worth to a windthrown stand's.
const GAP_SPACING: f64 = 90.0;
const GAP_REACH: (f64, f64) = (8.0, 35.0);

/// The shares of a stand's trees overtopped by the canopy, and suppressed
/// beneath it: trees that came up late, or lost the race for the light, and
/// wait in the shade for a gap.
const OVERTOPPED: f64 = 0.25;
const SUPPRESSED: f64 = 0.15;

impl Reader {
    /// The kinds it reads among.
    pub(crate) fn kinds(&self) -> impl Iterator<Item = &Habit> + '_ {
        self.kinds.iter().flatten()
    }

    /// The tree the ground at `at`, on the land `grids` trace in `fields`,
    /// would grow from a place drawn as `draw` has it, if any: in the shade
    /// of `beneath` where what grows under a canopy is read. A place grows
    /// one as often as the ground there suits it.
    pub(crate) fn tree(
        &self,
        ground: (&Grids, &[Heightfield]),
        place: ((f64, f64), u32),
        beneath: Option<&Shade>,
    ) -> Option<Tree> {
        let chance = unit(place.1);
        let sprout = self.sprout(place, beneath)?;
        // Turned away by the wood's patches or the light before the ground
        // need be read.
        if (self.rooting.streams <= 0.0 && chance >= sprout.in_patches) || chance >= sprout.light {
            return None;
        }
        let (tree, suits) = self.grow(&sprout, ground)?;
        (chance < suits).then_some(tree)
    }

    /// What the place `(at, draw)` alone says of the tree it would grow,
    /// before the ground there is read: `None` where the wood's patches and
    /// gaps, or the light beneath `beneath`, leave no tree at all.
    pub(crate) fn sprout(
        &self,
        (at, draw): ((f64, f64), u32),
        beneath: Option<&Shade>,
    ) -> Option<Sprout> {
        let (patches, stands) = self.seeds;
        // The canopy's gaps are its own; what grows beneath takes to them.
        let open = match beneath {
            Some(_) => 0.0,
            None => gap(&self.woodland, patches, at),
        };
        let in_patches = wooded(&self.woodland, patches, at) * (1.0 - open);
        if self.rooting.streams <= 0.0 && in_patches <= 0.0 {
            return None;
        }
        let light = match beneath {
            Some(canopy) => thrives_beneath(canopy.at(at.0, at.1).1),
            None => 1.0,
        };
        if light <= 0.0 {
            return None;
        }
        let age = smoothstep(
            0.3,
            0.7,
            0.5 + 0.5 * fbm2(at.0 / STANDS, at.1 / STANDS, stands ^ 0x41, (3, 0.5, 2.0)),
        );
        Some(Sprout {
            at,
            draw,
            in_patches,
            light,
            age,
        })
    }

    /// The tallest the tree `sprout` stands for could grow on any ground.
    pub(crate) fn highest(&self, sprout: &Sprout) -> f64 {
        self.kinds()
            .map(|habit| habit.highest(self.wanted(habit, sprout, (1.0, 1.0))))
            .fold(0.0, f64::max)
    }

    /// How tall a tree of `habit` grown from `sprout` would be wanted, where
    /// its wood covers `wooded` of the ground and it grows `green`: a wood's
    /// edge is lower than its heart, and good ground grows taller. Rises with
    /// both.
    fn wanted(&self, habit: &Habit, sprout: &Sprout, (wooded, green): (f64, f64)) -> f64 {
        let tallest = habit.heights.iter().copied().fold(0.0, f64::max);
        let (least, most) = self.woodland.stature;
        let edge = 0.82 + 0.18 * smoothstep(0.35, 0.95, wooded);
        tallest
            * (least + (most - least) * sprout.age)
            * rank(sprout.draw)
            * edge
            * (0.92 + 0.12 * green)
    }

    /// The tree `sprout` grows on the land `grids` trace in `fields`, and
    /// how well the ground there suits one, `0.0..=1.0`; `None` where it
    /// suits none.
    pub(crate) fn grow(
        &self,
        sprout: &Sprout,
        (grids, fields): (&Grids, &[Heightfield]),
    ) -> Option<(Tree, f64)> {
        let Sprout {
            at,
            draw,
            in_patches,
            light,
            ..
        } = *sprout;
        let patches = self.seeds.0;
        let lie = grids.lie(fields, at.0, at.1);
        let wooded = in_patches.max(self.rooting.streams * lie.wet).min(1.0);
        let suits = wooded * light * self.rooting.suits(&lie, at);
        if suits <= 0.0 || grids.wet_over(fields, at, lie.height) {
            return None;
        }
        let (kind, habit) = self.kind(&lie, at, draw)?;
        let wanted = self.wanted(habit, sprout, (wooded, lie.green));
        let variant = habit.nearest(wanted, unit(mix32(draw ^ 0x6a09_e667)));
        let natural = *habit.heights.get(variant)?;
        let scale = habit.sized(wanted, natural);
        let height = natural * scale;
        let (thickest, openest) = self.woodland.closure;
        let breadth = DENSITY_BREADTH * self.woodland.patch.max(1.0);
        let openness = 0.5
            + 0.5
                * fbm2(
                    at.0 / breadth,
                    at.1 / breadth,
                    patches ^ 0x77,
                    (3, 0.5, 2.0),
                );
        let apart = (thickest + (openest - thickest) * smoothstep(0.3, 0.7, openness))
            * (1.0 + EDGE_OPENING * (1.0 - wooded));
        let tree = Tree {
            kind,
            variant: u8::try_from(variant).ok()?,
            scale,
            height,
            reach: habit.crown * height,
            apart,
            base: rooted(&lie, height),
        };
        Some((tree, suits))
    }

    /// Which of its kinds grows at `at`, on ground `lie` describes: the one
    /// that takes to it best, in the stands its kind grows in, and its index
    /// among them.
    fn kind(&self, lie: &Lie, at: (f64, f64), draw: u32) -> Option<(u8, &Habit)> {
        let mut best: Option<(f64, u8, &Habit)> = None;
        for (index, habit) in self.kinds().enumerate() {
            let index = u8::try_from(index).ok()?;
            let salt = u32::from(index).wrapping_mul(0x9e37_79b9);
            let stand = 0.5
                + 0.5
                    * fbm2(
                        at.0 / STANDS,
                        at.1 / STANDS,
                        self.seeds.1 ^ 0x5eed ^ salt,
                        (2, 0.5, 2.0),
                    );
            let own = 0.75 + 0.5 * unit(mix32(draw ^ salt));
            let score = habit.affinity.of(lie) * (0.3 + stand) * own;
            if best.is_none_or(|(most, _, _)| score > most) {
                best = Some((score, index, habit));
            }
        }
        best.map(|(_, index, habit)| (index, habit))
    }
}

/// How much of the ground `woodland` covers at `at`, `0.0..=1.0`: patches
/// `patch` across, covering the share of the land its `cover` asks for.
pub(crate) fn wooded(woodland: &Woodland, seed: u32, at: (f64, f64)) -> f64 {
    let patch = woodland.patch.max(1.0);
    let field = 0.5 + 0.5 * fbm2(at.0 / patch, at.1 / patch, seed, (4, 0.5, 2.0));
    // Four octaves of the plane's noise, halved and raised, fall about their
    // middle nearly as a logistic spread of 0.056 does: this threshold leaves
    // the share asked for above it.
    let cover = woodland.cover.clamp(0.02, 0.98);
    let threshold = 0.5 + 0.056 * mathf::ln((1.0 - cover) / cover);
    smoothstep(threshold - 0.02, threshold + 0.02, field)
}

/// How far into one of its canopy's gaps `at` lies, `0.0..=1.0`, for
/// `woodland` under `seed`: each lattice cell of the gaps holding one as its
/// `gaps` share has it, about the cell's jittered middle.
pub(crate) fn gap(woodland: &Woodland, seed: u32, at: (f64, f64)) -> f64 {
    if woodland.gaps <= 0.0 {
        return 0.0;
    }
    let found = cells2(at.0 / GAP_SPACING, at.1 / GAP_SPACING, seed ^ 0x6a95, 0.8);
    if unit(mix32(found.id)) >= woodland.gaps {
        return 0.0;
    }
    let reach = GAP_REACH.0 + (GAP_REACH.1 - GAP_REACH.0) * unit(mix32(found.id ^ 0x2545));
    1.0 - smoothstep(0.75 * reach, reach, found.nearest * GAP_SPACING)
}

/// How readily a shrub or a young tree grows under a canopy hiding `hidden`
/// of the sky, `0.0..=1.0`: least in the deepest shade, most in the gaps and
/// along the edges, and less again out in the open, where grass takes the
/// ground.
fn thrives_beneath(hidden: f64) -> f64 {
    let light = 1.0 - hidden;
    (0.03 + 0.97 * smoothstep(0.05, 0.3, light)) * (1.0 - 0.6 * smoothstep(0.7, 0.95, light))
}

/// The share of its stand's height a tree as `draw` has it grows to: most
/// the canopy's, the overtopped and the suppressed lower.
fn rank(draw: u32) -> f64 {
    let (place, within) = (
        unit(mix32(draw ^ 0x1f83_d9ab)),
        unit(mix32(draw ^ 0x5be0_cd19)),
    );
    if place < SUPPRESSED {
        0.35 + 0.25 * within
    } else if place < SUPPRESSED + OVERTOPPED {
        0.6 + 0.25 * within
    } else {
        0.88 + 0.24 * within
    }
}

/// The height a trunk `height` tall is based at on ground `lie` describes:
/// sunk far enough that its flare meets the ground on its downhill side.
pub(crate) fn rooted(lie: &Lie, height: f64) -> f64 {
    let slope = mathf::sqrt((1.0 - lie.upright * lie.upright).max(0.0)) / lie.upright.max(0.1);
    lie.height - 0.08 - 0.035 * height * slope
}

#[cfg(test)]
#[path = "wood_tests.rs"]
mod tests;
