//! Plants of the water's edge: reeds and reedmace standing in the shallows
//! and along wet banks, water lilies and pondweed floating on still water,
//! and water-crowfoot streaming in running water.
//!
//! Each is grown as a square patch of its plants, so one prototype stands for
//! a clump near the eye and another for a whole bed far off, the squares
//! laid edge to edge without a seam, and at a stature of its kind's so a bed
//! can shorten toward its edges. Every plant's parts share the two low bits
//! of their key, so a plant takes one of its material's four colours whole.

use alloc::vec::Vec;
use core::f64::consts::{PI, TAU};

use tairix_rng::{NonCryptoRng, RandU64};
use tairix_util::mathf;

use crate::leaf::Outline;
use crate::prototype::{Blade, Building, Part, Prototype, Tube};
use crate::sample::mix32;
use crate::tree::Season;
use crate::vector::{single, singles, Vec3};

/// A plant of the water's edge.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) enum Margin {
    /// Common reed: tall stems leafy up their length, a plume nodding from
    /// each top.
    Reed,
    /// Reedmace, the bulrush: fans of strap leaves rising from the root, and
    /// flowering stems each topped by a brown velvet spike.
    Reedmace,
    /// Water lily: round pads floating flat, and in summer a white flower.
    Lily,
    /// Floating pondweed: small oval leaves lying on the water in rosettes.
    Pondweed,
    /// Water-crowfoot: long stems streaming down the current just beneath
    /// the surface, tufted with thread-fine leaves, and in spring and
    /// summer white flowers held on the water.
    Crowfoot,
}

/// The materials a patch is made in: its stems; its leaves or pads; its
/// heads — a reed's plume, a reedmace's spike, a lily's petals; and a lily
/// flower's heart.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(crate) struct Marsh {
    pub(crate) stems: u16,
    pub(crate) leaves: u16,
    pub(crate) heads: u16,
    pub(crate) hearts: u16,
}

/// A patch of `count` of `margin`'s plants filling a square `side` across
/// about its middle, as they are in `season`, standing up from or lying on
/// its plane, as tall or as broad as `stature` of their kind's; its
/// hierarchy still to build. `None` when the heap will not hold it.
pub(crate) fn patch(
    margin: Margin,
    (side, count, stature): (f64, u16, f64),
    marsh: Marsh,
    season: Season,
    seed: u64,
) -> Option<Building> {
    let mut grower = Grower {
        parts: Vec::new(),
        dice: NonCryptoRng::seed_from_u64(seed),
        marsh,
        season,
        stature,
    };
    grower.parts.try_reserve(most_parts(margin, count)).ok()?;
    // The wind the whole patch's plumes and leaves lean with, and the first
    // of the one lily in eight that flowers.
    let wind = TAU * grower.unit();
    let flowering = u32::from(FLOWERING_EVERY);
    let first = u32::try_from(grower.dice.next_below(u64::from(flowering))).unwrap_or(0);
    let mut key = mix32(u32::try_from(seed >> 32).unwrap_or(0) ^ 0x57a7);
    for plant in 0..count {
        let at = Vec3::new(
            side * (grower.unit() - 0.5),
            0.0,
            side * (grower.unit() - 0.5),
        );
        key = mix32(key ^ u32::from(plant));
        match margin {
            Margin::Reed => grower.reed(at, wind, key)?,
            Margin::Reedmace => grower.reedmace(at, wind, key)?,
            Margin::Lily => {
                let flowers =
                    season == Season::Summer && (u32::from(plant) + first) % flowering == 0;
                grower.lily(at, key, flowers)?;
            }
            Margin::Pondweed => grower.pondweed(at, key)?,
            Margin::Crowfoot => {
                let flowers = matches!(season, Season::Spring | Season::Summer)
                    && (u32::from(plant) + first) % flowering == 0;
                grower.crowfoot(at, key, flowers)?;
            }
        }
    }
    Prototype::building(grower.parts, Vec::new(), Vec::new())
}

/// The most parts a patch of `count` of `margin`'s plants takes.
fn most_parts(margin: Margin, count: u16) -> usize {
    let count = usize::from(count);
    match margin {
        Margin::Reed => count * usize::from(2 + u16::from(REED_PIECES) * REED_LEAVES + PLUME),
        Margin::Reedmace => count * usize::from(3 + u16::from(MACE_PIECES) * MACE_LEAVES),
        Margin::Lily => count + count.div_ceil(usize::from(FLOWERING_EVERY)) * usize::from(FLOWER),
        Margin::Pondweed => count * usize::from(ROSETTE),
        Margin::Crowfoot => {
            count * usize::from(STREAMERS * STREAMER_PIECES * (1 + TUFT))
                + count.div_ceil(usize::from(FLOWERING_EVERY)) * usize::from(BLOSSOM)
        }
    }
}

/// Leaves a reed's stem carries and the flat pieces each arches through,
/// the blades its plume spreads, the leaves of one reedmace plant's fan and
/// their pieces, and the most leaves a pondweed's rosette spreads.
const REED_LEAVES: u16 = 8;
const REED_PIECES: u8 = 3;
const PLUME: u16 = 10;
const MACE_LEAVES: u16 = 8;
const MACE_PIECES: u8 = 4;
const ROSETTE: u16 = 5;
/// The stems a crowfoot plant streams, the pieces each runs in, the leaves
/// each piece's tuft spreads, and the parts of its flower: five petals and
/// a heart.
const STREAMERS: u16 = 3;
const STREAMER_PIECES: u16 = 4;
const TUFT: u16 = 4;
const BLOSSOM: u16 = 6;
/// A lily flower's whorls, outermost first — its green sepals, then its
/// petals — each as its count, how far it rises from the water in radians,
/// and its length against the flower's size.
const WHORLS: [(u16, f64, f64); 4] = [
    (4, 0.1, 0.9),
    (8, 0.35, 1.0),
    (8, 0.75, 0.85),
    (6, 1.1, 0.65),
];
/// One lily, or crowfoot, in so many flowers in its season.
const FLOWERING_EVERY: u16 = 8;
/// The parts a lily's flower takes: its whorls and its heart.
const FLOWER: u16 = {
    let (mut parts, mut whorl) = (1, 0);
    while whorl < WHORLS.len() {
        parts += WHORLS[whorl].0;
        whorl += 1;
    }
    parts
};

/// A patch being grown: its parts so far, its draws, its materials, the
/// season it stands in, and its plants' stature against their kind's.
struct Grower {
    parts: Vec<Part>,
    dice: NonCryptoRng,
    marsh: Marsh,
    season: Season,
    stature: f64,
}

impl Grower {
    fn unit(&mut self) -> f64 {
        self.dice.next_f64()
    }

    fn range(&mut self, low: f64, high: f64) -> f64 {
        low + (high - low) * self.unit()
    }

    fn push(&mut self, part: Part) -> Option<()> {
        self.parts.try_reserve(1).ok()?;
        self.parts.push(part);
        Some(())
    }

    /// A tube from `a` to `b`, `radii` thick at its ends, in `material`.
    fn tube(
        &mut self,
        (a, b): (Vec3, Vec3),
        radii: (f64, f64),
        material: u16,
        key: u32,
    ) -> Option<()> {
        let along = (b - a).normalized();
        let side = if along.y.abs() < 0.99 {
            Vec3::UP
        } else {
            Vec3::new(1.0, 0.0, 0.0)
        };
        self.push(Part::Tube(Tube::new(
            (a, b),
            (radii, (0.0, 0.0)),
            (material, key),
            along.cross(side),
        )))
    }

    /// A long strap leaf from `base`, setting out along the unit `heading`
    /// and bowing down by `droop` radians over its `length`, `width` either
    /// side of its midrib, in `segments` flat pieces that keep one outline.
    fn strap(
        &mut self,
        (base, heading): (Vec3, Vec3),
        (length, width, droop): (f64, f64, f64),
        (segments, key): (u8, u32),
    ) -> Option<()> {
        let level = Vec3::new(heading.x, 0.0, heading.z);
        let across = if level.length() > 1e-6 {
            Vec3::UP.cross(level).normalized()
        } else {
            Vec3::new(1.0, 0.0, 0.0)
        };
        let piece = length / f64::from(segments.max(1));
        let (mut point, mut axis) = (base, heading);
        for segment in 0..segments {
            let normal = across.cross(axis).normalized();
            let normal = if normal.y < 0.0 { -normal } else { normal };
            let from = segment_mark(segment, segments);
            let to = segment_mark(segment + 1, segments);
            self.push(Part::Leaf(Blade {
                base: singles(point),
                normal: singles(normal),
                axis: singles(axis),
                length: single(piece),
                width: single(width),
                outline: Outline::Strap { from, to },
                fold: 0.35,
                material: self.marsh.leaves,
                key,
            }))?;
            point += axis * piece;
            axis = (axis - Vec3::UP * (droop / f64::from(segments.max(1)))).normalized();
        }
        Some(())
    }

    /// A common reed rooted at `root`: a stem leaning a little with the
    /// `wind`, strap leaves set alternately up its upper length, and a plume
    /// nodding from its top — on only some, last year's, in spring, and on
    /// most as this year's open over the summer.
    fn reed(&mut self, root: Vec3, wind: f64, key: u32) -> Option<()> {
        let height = self.range(1.6, 2.8) * self.stature;
        let lean = 0.05 + 0.1 * self.unit();
        let toward = Vec3::new(mathf::cos(wind), 0.0, mathf::sin(wind));
        let stem = (Vec3::UP + toward * lean).normalized();
        let top = root + stem * height;
        let middle = root + stem * (0.55 * height);
        let thick = self.range(0.0035, 0.006);
        let stems = self.marsh.stems;
        self.tube((root, middle), (thick, 0.8 * thick), stems, part(key, 0))?;
        self.tube(
            (middle, top),
            (0.8 * thick, 0.35 * thick),
            stems,
            part(key, 1),
        )?;
        let turn = TAU * self.unit();
        for leaf in 0..REED_LEAVES {
            let up = 0.22 + 0.62 * f64::from(leaf) / f64::from(REED_LEAVES);
            let around = turn + PI * f64::from(leaf) + self.range(-0.4, 0.4);
            let out = Vec3::new(mathf::cos(around), 0.0, mathf::sin(around));
            let rise = self.range(0.5, 0.9);
            let heading = (stem * mathf::cos(rise) + out * mathf::sin(rise)).normalized();
            let length = self.range(0.28, 0.45) * (1.1 - 0.35 * up);
            let width = self.range(0.009, 0.016);
            let droop = self.range(0.4, 1.0);
            let base = root + stem * (up * height);
            self.strap(
                (base, heading),
                (length, width, droop),
                (REED_PIECES, part(key, 2 + u32::from(leaf))),
            )?;
        }
        let plumed = match self.season {
            Season::Spring => self.unit() < 0.4,
            Season::Summer => self.unit() < 0.6,
            Season::Autumn { .. } | Season::Winter => true,
        };
        if plumed {
            self.plume(top, toward, part(key, 64))?;
        }
        Some(())
    }

    /// A reed's plume at `top`: fine branches spreading from it and nodding
    /// over toward `toward`.
    fn plume(&mut self, top: Vec3, toward: Vec3, key: u32) -> Option<()> {
        let nod = self.range(0.5, 1.1);
        let middle = (Vec3::UP * mathf::cos(nod) + toward * mathf::sin(nod)).normalized();
        let across = Vec3::UP.cross(toward).normalized();
        let length = self.range(0.16, 0.3);
        for branch in 0..PLUME {
            let spread = self.range(-0.35, 0.35);
            let tilt = self.range(-0.25, 0.25);
            let axis = (middle + across * spread + Vec3::UP * tilt).normalized();
            let normal = across.cross(axis).normalized();
            let normal = if normal.y < 0.0 { -normal } else { normal };
            let base = top - Vec3::UP * (0.06 * length * f64::from(branch % 3));
            let reach = length * self.range(0.6, 1.0);
            self.push(Part::Leaf(Blade {
                base: singles(base),
                normal: singles(normal),
                axis: singles(axis),
                length: single(reach),
                width: single(0.22 * length),
                outline: Outline::Fascicle { count: 7 },
                fold: 0.0,
                material: self.marsh.heads,
                key,
            }))?;
        }
        Some(())
    }

    /// A reedmace plant rooted at `root`: a flattened fan of strap leaves
    /// rising nearly straight and bowing over at their tips, and, on many, a
    /// flowering stem topped by its brown spike and the thin spike above it.
    fn reedmace(&mut self, root: Vec3, wind: f64, key: u32) -> Option<()> {
        let height = self.range(1.3, 2.3) * self.stature;
        let fan = wind + self.range(-0.6, 0.6);
        for leaf in 0..MACE_LEAVES {
            let side = if leaf % 2 == 0 { 1.0 } else { -1.0 };
            let around = fan + side * self.range(0.0, 0.25);
            let out = Vec3::new(mathf::cos(around), 0.0, mathf::sin(around)) * side;
            let rise = self.range(0.05, 0.25) * (0.5 + f64::from(leaf) / f64::from(MACE_LEAVES));
            let heading = (Vec3::UP * mathf::cos(rise) + out * mathf::sin(rise)).normalized();
            let length = height * self.range(0.75, 1.05);
            let width = self.range(0.007, 0.012);
            let droop = self.range(0.15, 0.6);
            self.strap(
                (root, heading),
                (length, width, droop),
                (MACE_PIECES, part(key, u32::from(leaf))),
            )?;
        }
        if self.unit() < 0.6 {
            let top = root + Vec3::UP * (height * self.range(0.85, 1.05));
            let spike = self.range(0.14, 0.22);
            let thin = self.range(0.08, 0.14);
            let head = top - Vec3::UP * thin;
            let stem = self.range(0.004, 0.006);
            let (stems, heads) = (self.marsh.stems, self.marsh.heads);
            self.tube(
                (root, head - Vec3::UP * spike),
                (stem, 0.8 * stem),
                stems,
                part(key, 64),
            )?;
            let girth = self.range(0.012, 0.017);
            self.tube(
                (head - Vec3::UP * spike, head),
                (girth, girth),
                heads,
                part(key, 65),
            )?;
            self.tube((head, top), (0.45 * stem, 0.2 * stem), stems, part(key, 66))?;
        }
        Some(())
    }

    /// A water lily's pad floating flat at `at`, a few millimetres apart in
    /// height from its neighbours so none lies in another's plane, and its
    /// flower when `flowering`.
    fn lily(&mut self, at: Vec3, key: u32, flowering: bool) -> Option<()> {
        let radius = self.range(0.08, 0.15) * self.stature;
        let lift = 0.002 + 0.004 * self.unit();
        let tilt = self.range(-0.03, 0.03);
        let around = TAU * self.unit();
        let axis = Vec3::new(mathf::cos(around), tilt, mathf::sin(around)).normalized();
        let normal =
            Vec3::new(-axis.x * axis.y, 1.0 - axis.y * axis.y, -axis.z * axis.y).normalized();
        let centre = at + Vec3::UP * lift;
        self.push(Part::Leaf(Blade {
            base: singles(centre - axis * radius),
            normal: singles(normal),
            axis: singles(axis),
            length: single(2.0 * radius),
            width: single(radius),
            outline: Outline::Pad,
            fold: 0.05,
            material: self.marsh.leaves,
            key,
        }))?;
        if flowering {
            self.flower(centre + Vec3::UP * 0.004, key)?;
        }
        Some(())
    }

    /// A water lily's flower at `at`: whorls of white petals cupping upward
    /// about a yellow heart, over a ring of green sepals.
    fn flower(&mut self, at: Vec3, key: u32) -> Option<()> {
        let size = self.range(0.045, 0.07);
        let turn = TAU * self.unit();
        let mut index = 0;
        for (whorl, (count, rise, scale)) in WHORLS.into_iter().enumerate() {
            let material = if whorl == 0 {
                self.marsh.leaves
            } else {
                self.marsh.heads
            };
            let offset = turn + PI * f64::from(u16::try_from(whorl).unwrap_or(0)) / 8.0;
            for petal in 0..count {
                index += 1;
                let around = offset + TAU * f64::from(petal) / f64::from(count);
                let out = Vec3::new(mathf::cos(around), 0.0, mathf::sin(around));
                let axis = (out * mathf::cos(rise) + Vec3::UP * mathf::sin(rise)).normalized();
                let across = Vec3::UP.cross(out).normalized();
                let normal = across.cross(axis).normalized();
                let normal = if normal.y < 0.0 { -normal } else { normal };
                self.push(Part::Leaf(Blade {
                    base: singles(at),
                    normal: singles(normal),
                    axis: singles(axis),
                    length: single(size * scale),
                    width: single(0.42 * size * scale),
                    outline: Outline::Ovate { teeth: 0 },
                    fold: 0.4,
                    material,
                    key: part(key, index),
                }))?;
            }
        }
        // Raised by its radius, so the round of its foot sits on the
        // flower's and not in the water.
        let heart = 0.22 * size;
        let hearts = self.marsh.hearts;
        self.tube(
            (at + Vec3::UP * (0.9 * heart), at + Vec3::UP * (1.3 * heart)),
            (heart, 0.8 * heart),
            hearts,
            part(key, 0),
        )
    }

    /// A water-crowfoot plant rising at `at`: its stems streaming down the
    /// current, the patch's `z`, just beneath the surface and swaying from
    /// side to side, a tuft of thread-fine leaves at each joint, and, if
    /// `flowering`, a white flower held on the water at a stem's end.
    fn crowfoot(&mut self, at: Vec3, key: u32, flowering: bool) -> Option<()> {
        let stems = self.marsh.stems;
        let mut index = 0;
        for streamer in 0..STREAMERS {
            let length = self.range(0.5, 1.1) * self.stature;
            let piece = length / f64::from(STREAMER_PIECES);
            let mut point = at
                + Vec3::new(
                    self.range(-0.06, 0.06),
                    -self.range(0.015, 0.045),
                    self.range(-0.06, 0.06),
                );
            let mut sway = self.range(-0.25, 0.25);
            for _ in 0..STREAMER_PIECES {
                sway = (sway + self.range(-0.3, 0.3)).clamp(-0.6, 0.6);
                let axis = Vec3::new(sway, 0.0, 1.0).normalized();
                let next = point + axis * piece;
                index += 1;
                self.tube((point, next), (0.0018, 0.0014), stems, part(key, index))?;
                for _ in 0..TUFT {
                    let spread = Vec3::new(self.range(-0.7, 0.7), self.range(-0.08, 0.02), 0.0);
                    let out = (axis + spread).normalized();
                    let across = Vec3::UP.cross(out).normalized();
                    let normal = across.cross(out).normalized();
                    let thread = self.range(0.04, 0.08);
                    index += 1;
                    self.push(Part::Leaf(Blade {
                        base: singles(next),
                        normal: singles(if normal.y < 0.0 { -normal } else { normal }),
                        axis: singles(out),
                        length: single(thread),
                        width: single(0.0025),
                        outline: Outline::Strap { from: 0, to: 255 },
                        fold: 0.0,
                        material: self.marsh.leaves,
                        key: part(key, index),
                    }))?;
                }
                point = next;
            }
            if flowering && streamer == 0 {
                self.blossom(
                    Vec3::new(point.x, at.y + 0.004, point.z),
                    part(key, 1 + index),
                )?;
            }
        }
        Some(())
    }

    /// A crowfoot's flower held on the water at `at`: five white petals
    /// spread almost flat about a small yellow heart.
    fn blossom(&mut self, at: Vec3, key: u32) -> Option<()> {
        let size = self.range(0.009, 0.013);
        let turn = TAU * self.unit();
        for petal in 0..BLOSSOM - 1 {
            let around = turn + TAU * f64::from(petal) / f64::from(BLOSSOM - 1);
            let out = Vec3::new(mathf::cos(around), 0.0, mathf::sin(around));
            let axis = (out + Vec3::UP * 0.15).normalized();
            let across = Vec3::UP.cross(out).normalized();
            let normal = across.cross(axis).normalized();
            self.push(Part::Leaf(Blade {
                base: singles(at),
                normal: singles(if normal.y < 0.0 { -normal } else { normal }),
                axis: singles(axis),
                length: single(size),
                width: single(0.45 * size),
                outline: Outline::Ovate { teeth: 0 },
                fold: 0.2,
                material: self.marsh.heads,
                key: part(key, u32::from(petal) + 1),
            }))?;
        }
        let heart = 0.18 * size;
        let hearts = self.marsh.hearts;
        self.tube(
            (at + Vec3::UP * heart, at + Vec3::UP * (1.6 * heart)),
            (heart, 0.7 * heart),
            hearts,
            part(key, 0),
        )
    }

    /// A rosette of floating pondweed leaves spreading from `at`, where their
    /// stem rises, each a few millimetres apart in height.
    fn pondweed(&mut self, at: Vec3, key: u32) -> Option<()> {
        let leaves =
            u32::from(ROSETTE) - u32::from(self.unit() < 0.5) - u32::from(self.unit() < 0.7);
        let turn = TAU * self.unit();
        for leaf in 0..leaves {
            let length = self.range(0.06, 0.11) * self.stature;
            let around = turn + TAU * (f64::from(leaf) + self.range(-0.2, 0.2)) / f64::from(leaves);
            let lift = 0.002 + 0.004 * self.unit();
            let axis = Vec3::new(mathf::cos(around), 0.0, mathf::sin(around));
            self.push(Part::Leaf(Blade {
                base: singles(at + axis * (0.1 * length) + Vec3::UP * lift),
                normal: singles(Vec3::UP),
                axis: singles(axis),
                length: single(length),
                width: single(0.42 * length),
                outline: Outline::Ovate { teeth: 0 },
                fold: 0.1,
                material: self.marsh.leaves,
                key: part(key, leaf),
            }))?;
        }
        Some(())
    }
}

/// The key of part `index` of the plant keyed `key`: its own, but in the
/// plant's colour.
fn part(key: u32, index: u32) -> u32 {
    (mix32(key ^ index.wrapping_mul(0x9e37_79b9)) & !3) | (key & 3)
}

/// Where piece `segment` of a leaf in `segments` pieces begins, in 255ths of
/// the way along it.
fn segment_mark(segment: u8, segments: u8) -> u8 {
    u8::try_from(u16::from(segment) * 255 / u16::from(segments.max(1))).unwrap_or(u8::MAX)
}

#[cfg(test)]
#[path = "waterside_tests.rs"]
mod tests;
