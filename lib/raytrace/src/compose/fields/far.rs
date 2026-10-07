//! A farmed land's hedges and walls beyond where they stand built, out to
//! where they shrink below a pixel: one mesh along every stretch of them
//! across the view, its stations as far apart as a few pixels span where they
//! stand, its section a hedge's crown or a wall's battered faces and cap.
//! Leaf and stone are too small to tell apart there, so each is coloured as
//! they are on the mean: a hedge in clumps of its shrubs' leaves, a wall as
//! its own stones massed.

use alloc::vec::Vec;

use tairix_countryside::boundary::{standing, Boundary, Kind as Bound};
use tairix_countryside::plane::{self, Walk};
use tairix_countryside::Point;
use tairix_util::mathf;

use super::drystone::{self, Walling};
use super::{across_view, Hedging, Laid, HAWTHORN, ROWED, SWELL};
use crate::bark::Bark;
use crate::compose::plants::{bark, palette, snowed, translucency, Kind, Stand};
use crate::compose::{rgb, Dice, Stage};
use crate::heightfield::Heightfield;
use crate::land::Land;
use crate::material::{Finish, Material};
use crate::noise::{noise2, smoothstep};
use crate::pigment::{Clumped, Pigment};
use crate::prototype::{normals_of, Facet, Part, Prototype};
use crate::shape::Shape;
use crate::tree::Season;
use crate::vector::{share, singles, Frame, Pose, Vec3};

/// How many pixels apart a far boundary's stations stand, and how few pixels
/// tall it still spans where its mesh ends.
const SPACED: f64 = 4.0;
const SPANNED: f64 = 1.0;

/// How many stations a unit of the setting out takes, about.
const STATIONS_A_UNIT: usize = 2000;

/// How many times a change between near and far along a line is halved to
/// find where it lies.
const NARROWED: u32 = 12;

/// How much of its leaves' light a hedge gives back seen from outside, the
/// rest lost in the shade among them, measured against hedges planted shrub
/// by shrub; how far across its clumps are; and how far along it its crown
/// lumps run, how deep, as shares of its height and breadth.
const CANOPY: f64 = 0.45;
const CLUMP: f64 = 0.7;
const LUMP: (f64, f64) = (0.9, 0.12);

/// Where a hedge's crown is widest, as a share of its height, and how far
/// above and below that it reaches.
const CROWN: (f64, f64) = (0.55, 0.47);

/// How far a far wall's capstones rise and lean out over its top, and how
/// their tops wander along it, as shares of how tall they stand.
const CAP_WANDER: (f64, f64) = (0.3, 0.2);

/// Where a far boundary is set out from: where the eye stands and the way it
/// looks, how wide a pixel spans a metre off, how far off its hedges and its
/// walls stand built, the materials its hedges' and its walls' meshes are in,
/// and how far its hedges' shrubs spread their crowns, as a share of their
/// height.
#[derive(Copy, Clone, Debug)]
pub(super) struct Sight {
    pub(super) eye: Point,
    pub(super) heading: f64,
    pub(super) pixel: f64,
    pub(super) built: (f64, f64),
    pub(super) materials: (u16, u16),
    pub(super) spread: f64,
}

impl Sight {
    /// Whether `at` lies across the view, or near enough either side of it
    /// to shadow what is seen.
    fn seen(&self, at: Point) -> bool {
        across_view((self.eye, self.heading), at)
    }

    /// How far apart a far boundary's stations stand `apart` from the eye.
    fn step(&self, apart: f64) -> f64 {
        (SPACED * self.pixel * apart).max(0.05)
    }

    /// How far off a boundary `tall` still spans a pixel.
    fn farthest(&self, tall: f64) -> f64 {
        tall / (SPANNED * self.pixel)
    }
}

/// The hedges and walls of a farmed land beyond where they stand built,
/// being set out a unit at a time as one mesh: the next of its layout's
/// boundaries to look at, of how many, and the mesh so far.
#[derive(Debug)]
pub(super) struct Distant {
    sight: Sight,
    next: usize,
    count: usize,
    vertices: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    parts: Vec<Part>,
}

impl Distant {
    /// The far boundaries of a layout of `count` to be set out from `sight`.
    pub(super) const fn new(sight: Sight, count: usize) -> Self {
        Self {
            sight,
            next: 0,
            count,
            vertices: Vec::new(),
            normals: Vec::new(),
            parts: Vec::new(),
        }
    }

    /// How far it has come, as a share.
    pub(super) fn done(&self) -> f64 {
        share(self.next, self.count)
    }

    /// Set out the next of `land`'s boundaries on `stage`, as far as a
    /// unit's stations, and once every one is, raise their mesh; whether
    /// they are raised, or `None` when the heap will not hold them.
    pub(super) fn step(&mut self, stage: &mut Stage, land: &Land) -> Option<bool> {
        let boundaries = land.layout.as_ref()?.boundaries();
        let mut stations = 0;
        while stations < STATIONS_A_UNIT {
            let Some(boundary) = boundaries.get(self.next) else {
                self.raise(stage)?;
                return Some(true);
            };
            self.next += 1;
            let edge = Laid {
                land,
                line: &boundary.line,
                eye: self.sight.eye,
            };
            stations += match boundary.kind {
                Bound::Hedge => self.hedge(&stage.fields, (&edge, boundary))?,
                Bound::Wall => self.wall(&stage.fields, (&edge, boundary))?,
                Bound::Fence | Bound::Ditch | Bound::Open => 0,
            };
        }
        Some(false)
    }

    /// Raise the mesh set out on `stage`, if any of it was, as one structure
    /// about the eye.
    fn raise(&mut self, stage: &mut Stage) -> Option<()> {
        if self.parts.is_empty() {
            return Some(());
        }
        let parts = core::mem::take(&mut self.parts);
        let vertices = core::mem::take(&mut self.vertices);
        let normals = core::mem::take(&mut self.normals);
        let prototype = stage.assemble(Prototype::building(parts, vertices, normals)?)?;
        let pose = Pose::new(self.origin(), Frame::WORLD);
        let shape = Shape::Instance {
            prototype,
            pose,
            scale: 1.0,
            key: 0,
        };
        stage.add(shape, usize::from(self.sight.materials.0), pose, false)?;
        Some(())
    }

    /// Where the mesh's own frame stands: under the eye, so its vertices,
    /// held in single precision, are finest where they are nearest.
    fn origin(&self) -> Vec3 {
        Vec3::new(self.sight.eye.x, 0.0, self.sight.eye.y)
    }

    /// Set out what shows far off of `laid`'s hedge, `boundary`, over
    /// `fields`; how many stations it took.
    fn hedge(
        &mut self,
        fields: &[Heightfield],
        (laid, boundary): (&Laid<'_>, &Boundary),
    ) -> Option<usize> {
        let hedging = Hedging::new(boundary);
        let sight = self.sight;
        let farthest = sight.farthest(hedging.height * (SWELL.0 + SWELL.1));
        if beyond(boundary, sight.eye, farthest) {
            return Some(0);
        }
        let length = plane::length(&boundary.line);
        let mut stations = 0;
        for span in standing(&boundary.gaps, (0.0, length), 0.5).ok()? {
            let mut walk = Walk::new(laid.line);
            let runs = runs(span, |along| {
                let Some((at, _)) = walk.at(along) else {
                    return (false, 1.0);
                };
                let apart = (at - sight.eye).length();
                let mended = hedging
                    .mended
                    .is_some_and(|(from, to)| (from..to).contains(&along));
                let far = !mended
                    && apart > sight.built.0
                    && apart <= sight.farthest(hedging.tall(along))
                    && sight.seen(at);
                (far, sight.step(apart))
            })?;
            for run in runs {
                stations += self.lay(
                    fields,
                    laid,
                    run,
                    &Hedged {
                        hedging: &hedging,
                        sight,
                    },
                )?;
            }
        }
        Some(stations)
    }

    /// Set out what shows far off of `laid`'s wall, `boundary`, over
    /// `fields`: what of it is not laid stone by stone; how many stations it
    /// took.
    fn wall(
        &mut self,
        fields: &[Heightfield],
        (laid, boundary): (&Laid<'_>, &Boundary),
    ) -> Option<usize> {
        let walling = Walling::new(boundary, &mut Dice::keyed(boundary.key, 0))?;
        let sight = self.sight;
        let (height, cap) = walling.section();
        let farthest = sight.farthest(height + cap);
        if beyond(boundary, sight.eye, farthest) {
            return Some(0);
        }
        let mut stations = 0;
        for (index, &span) in walling.spans().iter().enumerate() {
            let plan = walling.plan(index, span)?;
            let (mut walk, mut bays) = (Walk::new(laid.line), Walk::new(laid.line));
            let runs = runs(span, |along| {
                let Some((at, _)) = walk.at(along) else {
                    return (false, 1.0);
                };
                let apart = (at - sight.eye).length();
                let far = apart <= farthest
                    && sight.seen(at)
                    && !plan.laid((&mut bays, sight.eye), along, sight.built.1);
                (far, sight.step(apart))
            })?;
            let walled = Walled {
                walling: &walling,
                plan: &plan,
                seed: Dice::keyed(boundary.key, usize::MAX).seed(),
            };
            for run in runs {
                stations += self.lay(fields, laid, run, &walled)?;
            }
        }
        Some(stations)
    }

    /// Lay the run `(from, to)` along `laid`'s line over `fields` in
    /// `section`, a station each step of the way, the run closed at both
    /// ends; how many stations it took.
    fn lay(
        &mut self,
        fields: &[Heightfield],
        laid: &Laid<'_>,
        (from, to): (f64, f64),
        section: &dyn Section,
    ) -> Option<usize> {
        let strips = section.strips();
        let wide = strips.iter().sum::<usize>();
        let mut points: Vec<(f64, f64)> = Vec::new();
        points.try_reserve_exact(wide).ok()?;
        let mut vertices: Vec<Vec3> = Vec::new();
        let mut walk = Walk::new(laid.line);
        let (origin, eye) = (self.origin(), self.sight.eye);
        let mut along = from;
        let mut stations = 0;
        loop {
            let Some((at, way)) = walk.at(along) else {
                return Some(0);
            };
            let step = self.sight.step((at - eye).length());
            points.clear();
            section.at(along, step, &mut points);
            if points.len() != wide {
                return None;
            }
            let ground = laid.ground(fields, at);
            let left = way.left();
            let place = |(across, up): (f64, f64)| {
                Vec3::new(
                    at.x + left.x * across - origin.x,
                    ground + up,
                    at.y + left.y * across - origin.z,
                )
            };
            vertices.try_reserve(wide).ok()?;
            vertices.extend(points.iter().map(|&point| place(point)));
            stations += 1;
            if along >= to {
                break;
            }
            along = (along + step).min(to);
        }
        if stations < 2 {
            return Some(stations);
        }
        let first = outline(vertices.get(..wide)?, strips)?;
        let last = outline(vertices.get(vertices.len() - wide..)?, strips)?;
        let mut faces = strip_faces(stations, strips)?;
        for (cap, forward) in [(&first, false), (&last, true)] {
            let base = u32::try_from(vertices.len()).ok()?;
            vertices.try_reserve(cap.len()).ok()?;
            vertices.extend_from_slice(cap);
            fan(&mut faces, (base, cap.len()), forward)?;
        }
        // A wall's cap closes up where it has tumbled, leaving triangles
        // with nothing to them.
        faces.retain(|&[a, b, c]| {
            let corner = |index: u32| vertices.get(index as usize).copied().unwrap_or(Vec3::ZERO);
            (corner(b) - corner(a))
                .cross(corner(c) - corner(a))
                .length()
                > 1e-9
        });
        self.append(&vertices, &faces, section.material(&self.sight))?;
        Some(stations)
    }

    /// Take `vertices` and the triangles `faces` among them into the mesh, in
    /// `material`, each vertex's normal the mean of its triangles'.
    fn append(&mut self, vertices: &[Vec3], faces: &[[u32; 3]], material: u16) -> Option<()> {
        let normals = normals_of(vertices, faces)?;
        let base = u32::try_from(self.vertices.len()).ok()?;
        self.vertices.try_reserve(vertices.len()).ok()?;
        self.normals.try_reserve(normals.len()).ok()?;
        self.parts.try_reserve(faces.len()).ok()?;
        self.vertices
            .extend(vertices.iter().map(|&vertex| singles(vertex)));
        self.normals.extend_from_slice(&normals);
        for face in faces {
            let corners = [
                base.checked_add(face[0])?,
                base.checked_add(face[1])?,
                base.checked_add(face[2])?,
            ];
            self.parts
                .push(Part::Facet(Facet::plain(corners, Some(material))));
        }
        Some(())
    }
}

/// Whether all of `boundary` lies further from `eye` than `farthest`.
fn beyond(boundary: &Boundary, eye: Point, farthest: f64) -> bool {
    plane::nearest(&boundary.line, eye).is_none_or(|near| near.distance > farthest)
}

/// The runs of `span` along a line where `far` holds, read a step at a time,
/// `far` answering whether it holds at a place and the step to take from it;
/// each change between steps narrowed to where it lies.
fn runs(
    (from, to): (f64, f64),
    mut far: impl FnMut(f64) -> (bool, f64),
) -> Option<Vec<(f64, f64)>> {
    let mut runs = Vec::new();
    let (mut along, (mut holds, mut step)) = (from, far(from));
    let mut start = holds.then_some(from);
    while along < to {
        let next = (along + step).min(to);
        let (then, then_step) = far(next);
        if then != holds {
            let (mut low, mut high) = (along, next);
            for _ in 0..NARROWED {
                let middle = f64::midpoint(low, high);
                if far(middle).0 == holds {
                    low = middle;
                } else {
                    high = middle;
                }
            }
            match start.take() {
                Some(begun) => {
                    runs.try_reserve(1).ok()?;
                    runs.push((begun, low));
                }
                None => start = Some(high),
            }
        }
        (along, holds, step) = (next, then, then_step);
    }
    if let Some(begun) = start.filter(|&begun| to > begun) {
        runs.try_reserve(1).ok()?;
        runs.push((begun, to));
    }
    Some(runs)
}

/// The triangles of `stations` sections of `strips` laid one after another,
/// each strip's points joined to the next station's, wound to face out.
fn strip_faces(stations: usize, strips: &[usize]) -> Option<Vec<[u32; 3]>> {
    let wide = u32::try_from(strips.iter().sum::<usize>()).ok()?;
    // Every index lies below the last station's end.
    u32::try_from(stations).ok()?.checked_mul(wide)?;
    let quads = strips
        .iter()
        .map(|points| points.saturating_sub(1))
        .sum::<usize>();
    let mut faces = Vec::new();
    faces
        .try_reserve_exact(2 * quads * stations.saturating_sub(1))
        .ok()?;
    for station in 0..u32::try_from(stations.saturating_sub(1)).ok()? {
        let mut offset = station * wide;
        for &points in strips {
            for k in 0..u32::try_from(points.saturating_sub(1)).ok()? {
                let (a, d) = (offset + k, offset + wide + k);
                faces.push([a, d, d + 1]);
                faces.push([a, d + 1, a + 1]);
            }
            offset += u32::try_from(points).ok()?;
        }
    }
    Some(faces)
}

/// The outline of the section `vertices` holds, its `strips` laid one after
/// another, without the points two strips meeting at a crease share.
fn outline(vertices: &[Vec3], strips: &[usize]) -> Option<Vec<Vec3>> {
    let mut outline: Vec<Vec3> = Vec::new();
    outline.try_reserve_exact(strips.iter().sum()).ok()?;
    let mut offset = 0;
    for &points in strips {
        for &vertex in vertices.get(offset..offset + points)? {
            if outline
                .last()
                .is_none_or(|&before| (before - vertex).length() > 1e-6)
            {
                outline.push(vertex);
            }
        }
        offset += points;
    }
    Some(outline)
}

/// Close the outline of `count` points from `base`, left to right over the
/// section's top, with a fan of triangles into `faces`, facing on along the
/// line where `forward`, else back along it.
fn fan(faces: &mut Vec<[u32; 3]>, (base, count): (u32, usize), forward: bool) -> Option<()> {
    let count = u32::try_from(count).ok()?;
    base.checked_add(count)?;
    faces
        .try_reserve(usize::try_from(count.saturating_sub(2)).ok()?)
        .ok()?;
    for k in 1..count.saturating_sub(1) {
        let (b, c) = (base + k, base + k + 1);
        faces.push(if forward { [base, c, b] } else { [base, b, c] });
    }
    Some(())
}

/// The section a far boundary's mesh is laid in.
trait Section {
    /// How many points each strip of the section holds, laid one after
    /// another, left to right over its top: a strip is shaded smooth, and
    /// where two meet the section's surface is creased.
    fn strips(&self) -> &'static [usize];

    /// The section `along` the line, its stations `step` apart there, into
    /// `points`: across the line to its left and up from the ground there.
    fn at(&self, along: f64, step: f64, points: &mut Vec<(f64, f64)>);

    /// The material the section is laid in.
    fn material(&self, sight: &Sight) -> u16;
}

/// A hedge far off, as `hedging` has it planted.
struct Hedged<'a> {
    hedging: &'a Hedging,
    sight: Sight,
}

impl Section for Hedged<'_> {
    fn strips(&self) -> &'static [usize] {
        &[9]
    }

    /// Its crown as its shrubs' stand massed: widest a little above half its
    /// height, as broad as their crowns spread and their rows are set apart,
    /// and narrowing to the stems at its foot; lumpy where its stations stand
    /// close enough to show it, its foot sunk as far as its stations stand
    /// apart.
    fn at(&self, along: f64, step: f64, points: &mut Vec<(f64, f64)>) {
        let hedging = self.hedging;
        let tall = hedging.tall(along);
        let half = self.sight.spread * tall + f64::midpoint(ROWED.0, ROWED.1) * hedging.breadth;
        let (middle, reach) = (CROWN.0 * tall, CROWN.1 * tall);
        let lumpy = LUMP.1 * (1.0 - smoothstep(0.5 * LUMP.0, LUMP.0, step));
        let lump =
            |row: u32| 1.0 + lumpy * noise2(along / LUMP.0, 1.7 * f64::from(row), hedging.seed);
        let sunk = 0.25 + 0.1 * step;
        // Round from its left foot over its crown, at these angles above its
        // widest.
        let rim = [-55.0_f64, 0.0, 45.0, 90.0, 135.0, 180.0, 235.0];
        points.push((0.5 * half, -sunk));
        for (row, degrees) in (1..).zip(rim) {
            let (sin, cos) = (
                mathf::sin(degrees.to_radians()),
                mathf::cos(degrees.to_radians()),
            );
            let swell = lump(row);
            points.push((half * cos * swell, middle + reach * sin * swell));
        }
        points.push((-0.5 * half, -sunk));
    }

    fn material(&self, sight: &Sight) -> u16 {
        sight.materials.0
    }
}

/// A wall far off, as `walling` lays it, its stretch to `plan`, its capstones'
/// tops wandering under `seed`.
struct Walled<'a> {
    walling: &'a Walling,
    plan: &'a drystone::Plan,
    seed: u32,
}

impl Section for Walled<'_> {
    fn strips(&self) -> &'static [usize] {
        &[2, 2, 2, 2, 2]
    }

    /// Its battered faces to as high as it still stands, and where it stands
    /// whole its capstones across its top, a little broader; each a strip of
    /// its own, so the faces and the cap meet in an arris; its foot sunk as
    /// far as its stations stand apart.
    fn at(&self, along: f64, step: f64, points: &mut Vec<(f64, f64)>) {
        let (height, cap) = self.walling.section();
        let half = |y: f64| self.walling.half_at(y);
        let stands = self.plan.standing(height, along);
        let sunk = drystone::FOUNDED + 0.1 * step;
        let (wide, crown) = if stands >= height {
            let wander = CAP_WANDER.1 * (1.0 - smoothstep(0.5 * CAP_WANDER.0, CAP_WANDER.0, step));
            let tall = cap * f64::midpoint(drystone::CAPPED.0, drystone::CAPPED.1);
            let over = f64::midpoint(drystone::OVERHANG.0, drystone::OVERHANG.1);
            (
                half(height) + over,
                stands + tall * (1.0 + wander * noise2(along / CAP_WANDER.0, 0.0, self.seed)),
            )
        } else {
            (half(stands), stands)
        };
        points.extend_from_slice(&[
            (half(0.0), -sunk),
            (half(stands), stands),
            (wide, stands),
            (wide, crown),
            (wide, crown),
            (-wide, crown),
            (-wide, crown),
            (-wide, stands),
            (-half(stands), stands),
            (-half(0.0), -sunk),
        ]);
    }

    fn material(&self, sight: &Sight) -> u16 {
        sight.materials.1
    }
}

/// The material hedges far off are in for `season`, drawn from `seed`:
/// clumps of their thorn's and their hazel's leaves, thorn the most, letting
/// through as much of the light on them as those leaves do, which the hedge
/// itself then shades; bare in winter, a tangle of their twigs' bark under
/// what snow lies on it. `None` when the stage holds no more materials.
pub(super) fn canopy(stage: &mut Stage, season: Season, seed: u32) -> Option<u16> {
    let bare = season == Season::Winter;
    let mut tones = [Vec3::ZERO; 8];
    let (fours, _) = tones.as_chunks_mut::<4>();
    for (four, kind) in fours.iter_mut().zip([Kind::Hawthorn, Kind::Hazel]) {
        let shades = if bare {
            let Bark {
                light,
                dark,
                accent,
                ..
            } = bark(kind, seed);
            [light, dark, accent, light.lerp(dark, 0.5)]
        } else {
            palette(kind, season).map(rgb)
        };
        for (tone, shade) in four.iter_mut().zip(shades) {
            *tone = shade * CANOPY;
        }
    }
    let pigment = Pigment::Clumped(Clumped::new(
        (tones, HAWTHORN),
        (1.0 / CLUMP, snowed(season)),
        seed,
    ));
    let (thorn, hazel) = (translucency(Kind::Hawthorn), translucency(Kind::Hazel));
    let finish = if bare {
        Finish::Matte
    } else {
        Finish::Leaf {
            translucency: hazel + (thorn - hazel) * HAWTHORN,
        }
    };
    u16::try_from(stage.material(Material::new(pigment, finish))?).ok()
}

/// How far a hedge's shrubs spread their crowns, as a share of their height:
/// thorn the most of them.
pub(super) fn spread() -> f64 {
    let (thorn, hazel) = (
        Kind::Hawthorn.crown(Stand::Open),
        Kind::Hazel.crown(Stand::Open),
    );
    hazel + (thorn - hazel) * HAWTHORN
}

/// The material walls far off are in: `stone`, the material their stones
/// are, massed in units a wall's typical stone across; `None` when `stone`
/// is no masonry or the stage holds no more materials.
pub(super) fn massed(stage: &mut Stage, stone: u16) -> Option<u16> {
    let mut material = stage.materials.get(usize::from(stone))?.clone();
    let Pigment::Masonry(masonry) = &material.pigment else {
        return None;
    };
    material.pigment = Pigment::Masonry(masonry.massed(drystone::TYPICAL));
    u16::try_from(stage.material(material)?).ok()
}

#[cfg(test)]
#[path = "far_tests.rs"]
mod tests;
