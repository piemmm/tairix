//! Prototypes: a tree, a rock or a bush built once in its own frame and
//! placed as many times as a scene wants it.
//!
//! A prototype is a list of parts — tapering limbs with rounded or open ends,
//! flat leaves cut to an outline, and the triangles of a mesh, which may be a
//! pad or a petal shaped in the round and cut to its own outline — and a
//! hierarchy over them, so a ray entering an instance tests the few parts
//! along its path. Its parts are stored in single precision, since a forest's worth of
//! leaves is what a scene holds most of; they are met in double.

use alloc::vec::Vec;

use tairix_util::mathf;

use crate::bvh::{Builder, Bvh, Walk};
use crate::cut::{meet_relieved, Cutting, Seeking};
use crate::flare::Flare;
use crate::leaf::Outline;
use crate::lily::Trim;
use crate::shape::{Aabb, Hit};
use crate::vector::{single, singles, Ray, Vec3};

/// A limb: a tube tapering from radius `radii[0]` at `a` to `radii[1]` at
/// `b`, each end rounded by a sphere of its radius, so a chain of them bends
/// without a seam — or left open, for something else to close, where it
/// broke.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Tube {
    pub(crate) a: [f32; 3],
    pub(crate) b: [f32; 3],
    pub(crate) radii: [f32; 2],
    /// How far along its stem each end lies, for the bark's pattern.
    pub(crate) stem: [f32; 2],
    pub(crate) material: u16,
    pub(crate) key: u32,
    /// How far round it, in radians, its bark's angle starts from where the
    /// world alone would start it: toward the side its stem carries along
    /// it, so a bending stem's segments agree on it.
    pub(crate) turn: f32,
    /// Which of its ends are open rather than rounded.
    pub(crate) open: [bool; 2],
    /// The flare its foot swells in, among its prototype's, if it does.
    pub(crate) flare: Option<u16>,
}

impl Tube {
    /// A limb from `a` to `b`, `radii` thick at either end and `stem`
    /// metres along its stem there, in `material` and keyed `key`, its bark
    /// begun round it toward `side`.
    pub(crate) fn new(
        (a, b): (Vec3, Vec3),
        (radii, stem): ((f64, f64), (f64, f64)),
        (material, key): (u16, u32),
        side: Vec3,
    ) -> Self {
        let (a, b) = (singles(a), singles(b));
        let (first, second) = round((point(b) - point(a)).normalized());
        let (x, y) = (side.dot(first), side.dot(second));
        let turn = if x * x + y * y > 1e-12 {
            mathf::atan2(y, x)
        } else {
            0.0
        };
        Self {
            a,
            b,
            radii: [single(radii.0), single(radii.1)],
            stem: [single(stem.0), single(stem.1)],
            material,
            key,
            turn: single(turn),
            open: [false; 2],
            flare: None,
        }
    }

    /// The same limb with each end `open` where it is, rather than rounded.
    pub(crate) const fn opened(self, open: [bool; 2]) -> Self {
        Self { open, ..self }
    }

    /// Where on the sphere rounding its end `end` — `0` its first, `1` its
    /// second — a point facing `normal` from that sphere's middle lies: how
    /// far along its stem, carried on over the sphere down its meridian from
    /// the round where the limb meets it, and the sphere's girth there about
    /// the limb's axis. Over a free end the bark crowds to a point, as a
    /// cactus's ribs crowd over its apex.
    pub(crate) fn over_end(&self, end: usize, normal: Vec3) -> (f64, f64) {
        let axis = (point(self.b) - point(self.a)).normalized();
        let (from, to) = (f64::from(self.stem[0]), f64::from(self.stem[1]));
        let onward = if to >= from { 1.0 } else { -1.0 };
        let (outward, stem, radius, onward) = if end == 0 {
            (-axis, from, self.radii[0], -onward)
        } else {
            (axis, to, self.radii[1], onward)
        };
        let polar = mathf::acos(normal.dot(outward).clamp(-1.0, 1.0));
        let radius = f64::from(radius);
        (
            stem + onward * (core::f64::consts::FRAC_PI_2 - polar) * radius,
            radius * mathf::sin(polar),
        )
    }

    /// The same limb, its foot swelling in its prototype's flare `flare`.
    pub(crate) const fn flared(self, flare: u16) -> Self {
        Self {
            flare: Some(flare),
            ..self
        }
    }

    /// The unit way out from the limb's axis at `angle` round it, as its bark
    /// reckons angles.
    pub(crate) fn way(&self, angle: f64) -> Vec3 {
        let (first, second) = round((point(self.b) - point(self.a)).normalized());
        let turned = angle + f64::from(self.turn);
        first * mathf::cos(turned) + second * mathf::sin(turned)
    }

    /// The limb's radius `up` along its axis from its first end, before any
    /// flare swells it.
    pub(crate) fn round_radius(&self, up: f64) -> f64 {
        let length = (point(self.b) - point(self.a)).length().max(1e-12);
        let (ra, rb) = (f64::from(self.radii[0]), f64::from(self.radii[1]));
        ra + (rb - ra) * (up / length).clamp(0.0, 1.0)
    }

    /// The angle round the limb, as its bark reckons angles, that `way`
    /// points out from its axis toward.
    pub(crate) fn angle_of(&self, way: Vec3) -> f64 {
        let (first, second) = round((point(self.b) - point(self.a)).normalized());
        mathf::atan2(way.dot(second), way.dot(first)) - f64::from(self.turn)
    }
}

/// The two directions about a limb running along the unit `axis` its angle
/// is measured between, fixed by the world alone: what a limb's own `turn` is
/// reckoned from.
pub(crate) fn round(axis: Vec3) -> (Vec3, Vec3) {
    let reference = if axis.x.abs() < 0.9 {
        Vec3::new(1.0, 0.0, 0.0)
    } else {
        Vec3::new(0.0, 0.0, 1.0)
    };
    let first = (reference - axis * reference.dot(axis)).normalized();
    (first, axis.cross(first))
}

/// A leaf: the part of the plane through `base` facing `normal` that its
/// outline covers, its midrib running `length` along the unit `axis` and its
/// greatest half-width `width`; `fold` tilts its halves up from the midrib.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Blade {
    pub(crate) base: [f32; 3],
    pub(crate) normal: [f32; 3],
    pub(crate) axis: [f32; 3],
    pub(crate) length: f32,
    pub(crate) width: f32,
    pub(crate) outline: Outline,
    pub(crate) fold: f32,
    pub(crate) material: u16,
    pub(crate) key: u32,
}

/// A triangle of the prototype's vertices, shaded by their normals blended,
/// in its own material, or in whatever its placing is made in where it has
/// none; keyed as its mesh is, and, where its mesh is a pad or a petal
/// shaped in the round, `size` across and cut to that part's outline where
/// its surface coordinates fall.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Facet {
    pub(crate) corners: [u32; 3],
    pub(crate) material: Option<u16>,
    pub(crate) key: u32,
    pub(crate) trim: Option<Trim>,
    pub(crate) size: f32,
}

impl Facet {
    /// A triangle of a mesh that is not mapped, in `material` or its
    /// placing's.
    pub(crate) const fn plain(corners: [u32; 3], material: Option<u16>) -> Self {
        Self {
            corners,
            material,
            key: 0,
            trim: None,
            size: 0.0,
        }
    }
}

/// One part of a prototype.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Part {
    Tube(Tube),
    Leaf(Blade),
    Facet(Facet),
}

/// A shape built once, in its own frame.
#[derive(Clone, Debug)]
pub(crate) struct Prototype {
    parts: Vec<Part>,
    vertices: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    /// Where on its mesh's own surface each vertex lies, in metres; empty
    /// where no mesh of the prototype is mapped.
    coords: Vec<[f32; 2]>,
    flares: Vec<Flare>,
    bvh: Bvh,
    bounds: Aabb,
}

/// A mesh's own surface, a pad's or a petal's: where on it each of its
/// points lies, in metres; the key it is drawn from; the size its outline is
/// reckoned in; and the outline it is cut to, if any.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Mapping<'a> {
    pub(crate) coords: &'a [[f32; 2]],
    pub(crate) key: u32,
    pub(crate) size: f64,
    pub(crate) trim: Option<Trim>,
}

/// A mesh to add to an assembly: its points, and its faces, each three of
/// its points and a material.
#[derive(Debug, Default)]
pub(crate) struct Mesh {
    pub(crate) points: Vec<Vec3>,
    pub(crate) faces: Vec<([u32; 3], u16)>,
}

/// A prototype as it is put together: its parts, the vertices, normals and
/// surface coordinates its facets are cut from, and the flares its limbs'
/// feet swell in.
#[derive(Debug, Default)]
pub(crate) struct Assembly {
    parts: Vec<Part>,
    vertices: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    coords: Vec<[f32; 2]>,
    flares: Vec<Flare>,
}

impl Assembly {
    /// An assembly with room made for `parts` parts and `vertices`
    /// vertices; `None` when the heap will not hold them.
    pub(crate) fn with_room(parts: usize, vertices: usize) -> Option<Self> {
        let mut assembly = Self::default();
        assembly.parts.try_reserve(parts).ok()?;
        assembly.vertices.try_reserve(vertices).ok()?;
        assembly.normals.try_reserve(vertices).ok()?;
        Some(assembly)
    }

    /// How many parts it holds.
    pub(crate) fn parts(&self) -> usize {
        self.parts.len()
    }

    pub(crate) fn push(&mut self, part: Part) -> Option<()> {
        self.parts.try_reserve(1).ok()?;
        self.parts.push(part);
        Some(())
    }

    /// `flare` among the assembly's flares, and its index there.
    pub(crate) fn flare(&mut self, flare: Flare) -> Option<u16> {
        let index = u16::try_from(self.flares.len()).ok()?;
        self.flares.try_reserve(1).ok()?;
        self.flares.push(flare);
        Some(index)
    }

    /// The mesh of `points` whose `faces` each name three of them and a
    /// material, shaded smooth where they share a point.
    pub(crate) fn mesh(&mut self, points: &[Vec3], faces: &[([u32; 3], u16)]) -> Option<()> {
        self.add_mesh(points, faces, None)
    }

    /// [`Self::mesh`], its points lying on its own surface where `mapping`
    /// has them and its facets cut to the outline it names; `None` too when
    /// the mapping does not place every point.
    pub(crate) fn mesh_mapped(
        &mut self,
        points: &[Vec3],
        faces: &[([u32; 3], u16)],
        mapping: Mapping<'_>,
    ) -> Option<()> {
        if mapping.coords.len() != points.len() || mapping.size.is_nan() || mapping.size <= 0.0 {
            return None;
        }
        self.add_mesh(points, faces, Some(mapping))
    }

    fn add_mesh(
        &mut self,
        points: &[Vec3],
        faces: &[([u32; 3], u16)],
        mapping: Option<Mapping<'_>>,
    ) -> Option<()> {
        let mut corners = Vec::new();
        corners.try_reserve_exact(faces.len()).ok()?;
        corners.extend(faces.iter().map(|&(corners, _)| corners));
        let normals = normals_of(points, &corners)?;
        let first = u32::try_from(self.vertices.len()).ok()?;
        self.vertices.try_reserve(points.len()).ok()?;
        self.normals.try_reserve(points.len()).ok()?;
        self.vertices
            .extend(points.iter().map(|&point| singles(point)));
        self.normals.extend_from_slice(&normals);
        // Once any mesh is mapped, every vertex carries coordinates, those of
        // an unmapped mesh nought.
        if mapping.is_some() || !self.coords.is_empty() {
            self.coords
                .try_reserve(self.vertices.len() - self.coords.len())
                .ok()?;
            self.coords.resize(usize::try_from(first).ok()?, [0.0; 2]);
            match mapping {
                Some(mapping) => self.coords.extend_from_slice(mapping.coords),
                None => self.coords.resize(self.vertices.len(), [0.0; 2]),
            }
        }
        self.parts.try_reserve(faces.len()).ok()?;
        for &([a, b, c], material) in faces {
            let plain = Facet::plain([first + a, first + b, first + c], Some(material));
            self.parts.push(Part::Facet(match mapping {
                Some(mapping) => Facet {
                    key: mapping.key,
                    trim: mapping.trim,
                    size: single(mapping.size),
                    ..plain
                },
                None => plain,
            }));
        }
        Some(())
    }

    /// The prototype it makes, its hierarchy still to build; `None` when the
    /// heap will not hold it, or a part names what the assembly lacks.
    pub(crate) fn finish(self) -> Option<Building> {
        let mut building =
            Prototype::flared(self.parts, (self.vertices, self.normals), self.flares)?;
        building.coords = self.coords;
        Some(building)
    }
}

/// Each vertex's normal: the sum of its faces' area-weighted normals, made
/// unit.
pub(crate) fn normals_of(vertices: &[Vec3], faces: &[[u32; 3]]) -> Option<Vec<[f32; 3]>> {
    let mut sums = Vec::new();
    sums.try_reserve_exact(vertices.len()).ok()?;
    sums.resize(vertices.len(), Vec3::ZERO);
    for &[a, b, c] in faces {
        let (pa, pb, pc) = (
            *vertices.get(a as usize)?,
            *vertices.get(b as usize)?,
            *vertices.get(c as usize)?,
        );
        let weighted = (pb - pa).cross(pc - pa);
        for index in [a, b, c] {
            let sum = sums.get_mut(index as usize)?;
            *sum += weighted;
        }
    }
    let mut normals = Vec::new();
    normals.try_reserve_exact(sums.len()).ok()?;
    normals.extend(sums.iter().map(|&sum| singles(sum.normalized())));
    Some(normals)
}

/// A point held in single precision, back in double.
pub(crate) fn point(at: [f32; 3]) -> Vec3 {
    Vec3::new(f64::from(at[0]), f64::from(at[1]), f64::from(at[2]))
}

/// A prototype whose hierarchy is still being built.
#[derive(Debug)]
pub(crate) struct Building {
    parts: Vec<Part>,
    vertices: Vec<[f32; 3]>,
    normals: Vec<[f32; 3]>,
    coords: Vec<[f32; 2]>,
    flares: Vec<Flare>,
    builder: Builder,
    bounds: Aabb,
}

/// Parts' worth of a prototype's hierarchy built in one step.
pub(crate) const BUILD_UNIT: usize = 24_000;

impl Building {
    /// Build about `budget` parts' worth more of the hierarchy; whether it
    /// is whole.
    pub(crate) fn step(&mut self, budget: usize) -> bool {
        self.builder.step(budget)
    }

    /// The prototype, its hierarchy built whole at once.
    #[cfg(test)]
    pub(crate) fn whole(mut self) -> Prototype {
        self.step(usize::MAX);
        self.finish()
    }

    /// The prototype, once its hierarchy is whole.
    pub(crate) fn finish(self) -> Prototype {
        Prototype {
            parts: self.parts,
            vertices: self.vertices,
            normals: self.normals,
            coords: self.coords,
            flares: self.flares,
            bvh: self.builder.finish(),
            bounds: self.bounds,
        }
    }
}

impl Prototype {
    /// The prototype of `parts`, whose facets index `vertices` and their
    /// `normals`, built at once; `None` when the heap will not hold its
    /// hierarchy, or a facet names a vertex it lacks.
    #[cfg(test)]
    pub(crate) fn new(
        parts: Vec<Part>,
        vertices: Vec<[f32; 3]>,
        normals: Vec<[f32; 3]>,
    ) -> Option<Self> {
        Some(Self::building(parts, vertices, normals)?.whole())
    }

    /// The prototype of `parts`, whose facets index `vertices` and their
    /// `normals`, its hierarchy to build step by step; `None` when the heap
    /// will not hold it, or a facet names a vertex it lacks.
    pub(crate) fn building(
        parts: Vec<Part>,
        vertices: Vec<[f32; 3]>,
        normals: Vec<[f32; 3]>,
    ) -> Option<Building> {
        Self::flared(parts, (vertices, normals), Vec::new())
    }

    /// [`Self::building`], its limbs' feet swelling in `flares`; `None` too
    /// when a limb names a flare it lacks.
    pub(crate) fn flared(
        parts: Vec<Part>,
        (vertices, normals): (Vec<[f32; 3]>, Vec<[f32; 3]>),
        flares: Vec<Flare>,
    ) -> Option<Building> {
        if normals.len() != vertices.len() {
            return None;
        }
        let mut boxes = Vec::new();
        if boxes.try_reserve_exact(parts.len()).is_err() {
            return None;
        }
        let mut bounds = Aabb::EMPTY;
        for (index, part) in parts.iter().enumerate() {
            let extent = part_bounds(part, &vertices, &flares)?;
            bounds = bounds.union(extent);
            boxes.push((u32::try_from(index).ok()?, extent));
        }
        let builder = Builder::new(&boxes)?;
        // A prototype of no parts is a point, so every box built on it is one.
        if bounds.min.x > bounds.max.x {
            bounds = Aabb::around(Vec3::ZERO, 1e-3);
        }
        Some(Building {
            parts,
            vertices,
            normals,
            coords: Vec::new(),
            flares,
            builder,
            bounds,
        })
    }

    /// The box the prototype lies in, in its own frame.
    pub(crate) const fn bounds(&self) -> Aabb {
        self.bounds
    }

    /// The parts it is made of.
    #[cfg(test)]
    pub(crate) fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// Where its vertex `index` lies, if it has one.
    #[cfg(test)]
    pub(crate) fn vertex(&self, index: u32) -> Option<Vec3> {
        self.vertices.get(index as usize).copied().map(point)
    }

    /// The flare `tube`'s foot swells in, if it does.
    pub(crate) fn flare_of(&self, tube: &Tube) -> Option<&Flare> {
        tube.flare
            .and_then(|flare| self.flares.get(usize::from(flare)))
    }

    /// The nearest part `ray`, given in the prototype's frame, meets within
    /// `(near, far)`, its limbs cut as `cutting` has them, if it does.
    pub(crate) fn intersect(
        &self,
        ray: &Ray,
        (near, far): (f64, f64),
        cutting: Option<&Cutting<'_>>,
    ) -> Option<Hit> {
        let mut best: Option<Hit> = None;
        self.bvh.walk(ray, far, |index, reach| {
            match self
                .parts
                .get(index as usize)
                .and_then(|part| self.meet(part, ray, ((near, reach), Seeking::Nearest), cutting))
            {
                Some(hit) => {
                    let t = hit.t;
                    best = Some(hit);
                    Walk::Within(t)
                }
                None => Walk::Within(reach),
            }
        });
        best
    }

    /// Whether `ray` meets any part within `(near, far)`, its limbs cut as
    /// `cutting` has them, if it does.
    pub(crate) fn occludes(
        &self,
        ray: &Ray,
        (near, far): (f64, f64),
        cutting: Option<&Cutting<'_>>,
    ) -> bool {
        let mut blocked = false;
        self.bvh.walk(ray, far, |index, reach| {
            let met = self
                .parts
                .get(index as usize)
                .and_then(|part| self.meet(part, ray, ((near, reach), Seeking::Any), cutting))
                .is_some();
            if met {
                blocked = true;
                Walk::Stop
            } else {
                Walk::Within(reach)
            }
        });
        blocked
    }

    fn meet(
        &self,
        part: &Part,
        ray: &Ray,
        (span, seeking): ((f64, f64), Seeking),
        cutting: Option<&Cutting<'_>>,
    ) -> Option<Hit> {
        match part {
            Part::Tube(tube) => {
                let flare = self.flare_of(tube);
                let cut = cutting.and_then(|cutting| Some((cutting, cutting.of(tube)?)));
                if flare.is_none() && cut.is_none() {
                    meet_tube(tube, ray, span)
                } else {
                    meet_relieved(
                        tube,
                        ray,
                        (span, seeking),
                        (cutting, cut.map(|(_, cut)| cut)),
                        flare,
                    )
                }
            }
            Part::Leaf(blade) => meet_blade(blade, ray, span),
            Part::Facet(facet) => self.meet_facet(facet, ray, span),
        }
    }

    /// Where `ray` meets `facet` within `(near, far)` (Möller and Trumbore,
    /// "Fast, minimum storage ray/triangle intersection", 1997).
    #[allow(
        clippy::many_single_char_names,
        reason = "the paper's symbols, as it writes them"
    )]
    fn meet_facet(&self, facet: &Facet, ray: &Ray, (near, far): (f64, f64)) -> Option<Hit> {
        let corner = |at: usize| {
            let index = *facet.corners.get(at)? as usize;
            Some((
                point(*self.vertices.get(index)?),
                point(*self.normals.get(index)?),
            ))
        };
        let ((v0, n0), (v1, n1), (v2, n2)) = (corner(0)?, corner(1)?, corner(2)?);
        let (e1, e2) = (v1 - v0, v2 - v0);
        let p = ray.dir.cross(e2);
        let det = e1.dot(p);
        if det.abs() < 1e-18 {
            return None;
        }
        let inverse = 1.0 / det;
        let s = ray.origin - v0;
        let u = s.dot(p) * inverse;
        if !(0.0..=1.0).contains(&u) {
            return None;
        }
        let q = s.cross(e1);
        let v = ray.dir.dot(q) * inverse;
        if v < 0.0 || u + v > 1.0 {
            return None;
        }
        let t = e2.dot(q) * inverse;
        if !(t > near && t < far) {
            return None;
        }
        let at = self.mapped(facet, (u, v))?;
        let size = f64::from(facet.size);
        if let Some(trim) = facet.trim {
            if !trim.keeps((at.0 / size, at.1 / size), facet.key) {
                return None;
            }
        }
        // Out is the way the corners' normals face, however the corners wind.
        let blended = (n0 * (1.0 - u - v) + n1 * u + n2 * v).normalized();
        let face = e1.cross(e2).normalized();
        let normal = if face.dot(blended) < 0.0 { -face } else { face };
        Some(Hit {
            t,
            normal,
            shading: blended,
            mark: facet.key,
            along: 0.0,
            uv: at,
            girth: size,
            material: facet.material.map(u32::from),
            tangent: Vec3::ZERO,
            relieved: false,
            member: None,
        })
    }

    /// Where on its mesh's own surface the point `(u, v)` of `facet`'s
    /// corners lies, in metres: nought on a mesh that is not mapped. `None`
    /// when a corner names a vertex the prototype lacks.
    fn mapped(&self, facet: &Facet, (u, v): (f64, f64)) -> Option<(f64, f64)> {
        if self.coords.is_empty() {
            return Some((0.0, 0.0));
        }
        let corner = |at: usize| {
            let [x, y] = *self.coords.get(*facet.corners.get(at)? as usize)?;
            Some((f64::from(x), f64::from(y)))
        };
        let ((x0, y0), (x1, y1), (x2, y2)) = (corner(0)?, corner(1)?, corner(2)?);
        let w = 1.0 - u - v;
        Some((x0 * w + x1 * u + x2 * v, y0 * w + y1 * u + y2 * v))
    }
}

/// The box a part lies in.
fn part_bounds(part: &Part, vertices: &[[f32; 3]], flares: &[Flare]) -> Option<Aabb> {
    Some(match part {
        Part::Tube(tube) => {
            let (a, b) = (point(tube.a), point(tube.b));
            let most = match tube.flare {
                Some(flare) => flares.get(usize::from(flare))?.most(),
                None => 1.0,
            };
            let (ra, rb) = (f64::from(tube.radii[0]), f64::from(tube.radii[1]));
            Aabb::around(a, ra * most).union(Aabb::around(b, rb * most))
        }
        Part::Leaf(blade) => {
            let (base, axis, normal) = (point(blade.base), point(blade.axis), point(blade.normal));
            let across = normal.cross(axis) * f64::from(blade.width);
            let tip = base + axis * f64::from(blade.length);
            let lift = normal * (0.25 * f64::from(blade.fold) * f64::from(blade.width));
            [base + across, base - across, tip + across, tip - across]
                .into_iter()
                .fold(Aabb::EMPTY, Aabb::including)
                .union(Aabb::around(base + lift, 1e-4))
                .padded()
        }
        Part::Facet(facet) => {
            let mut bounds = Aabb::EMPTY;
            for &corner in &facet.corners {
                bounds = bounds.including(point(*vertices.get(corner as usize)?));
            }
            bounds.padded()
        }
    })
}

/// Where `ray` meets a limb within `(near, far)`: the tapering body between
/// its two spheres, or the sphere capping either end it rounds (Quílez,
/// "Rounded cone – intersection", 2019).
#[allow(
    clippy::many_single_char_names,
    reason = "the derivation's symbols, as it writes them"
)]
fn meet_tube(tube: &Tube, ray: &Ray, (near, far): (f64, f64)) -> Option<Hit> {
    let (a, b) = (point(tube.a), point(tube.b));
    let (ra, rb) = (f64::from(tube.radii[0]), f64::from(tube.radii[1]));
    let ba = b - a;
    let (oa, ob) = (ray.origin - a, ray.origin - b);
    let rr = ra - rb;
    let m0 = ba.dot(ba);
    let (m1, m2, m3) = (ba.dot(oa), ba.dot(ray.dir), ray.dir.dot(oa));
    let m5 = oa.dot(oa);
    let d2 = m0 - rr * rr;
    let k2 = d2 - m2 * m2;
    let k1 = d2 * m3 - m1 * m2 + m2 * rr * ra;
    let k0 = d2 * m5 - m1 * m1 + m1 * rr * ra * 2.0 - m0 * ra * ra;
    let h = k1 * k1 - k0 * k2;
    if h < 0.0 {
        return None;
    }
    let stem = |along: f64| {
        f64::from(tube.stem[0]) + (f64::from(tube.stem[1]) - f64::from(tube.stem[0])) * along
    };
    if k2.abs() > 1e-18 {
        let t = (-mathf::sqrt(h) - k1) / k2;
        let y = m1 - ra * rr + t * m2;
        if y > 0.0 && y < d2 {
            if !(t > near && t < far) {
                return None;
            }
            let normal = ((oa + ray.dir * t) * d2 - ba * y).normalized();
            let along = y / d2;
            let girth = ra + (rb - ra) * along;
            return Some(limb_hit(t, normal, (stem(along), along, girth), tube));
        }
    }
    let mut best: Option<Hit> = None;
    let ends = [(a, ra, oa, 0.0), (b, rb, ob, 1.0)];
    for (end, ((centre, radius, to, at), open)) in ends.into_iter().zip(tube.open).enumerate() {
        let dot = ray.dir.dot(to);
        let reach = dot * dot - to.dot(to) + radius * radius;
        if open || reach <= 0.0 || radius <= 0.0 {
            continue;
        }
        let t = -dot - mathf::sqrt(reach);
        if t > near && t < far && best.is_none_or(|held| t < held.t) {
            let normal = (ray.at(t) - centre) / radius;
            let (stem, girth) = tube.over_end(end, normal);
            best = Some(limb_hit(t, normal, (stem, at, girth), tube));
        }
    }
    best
}

/// A hit on `tube` at `t` with outward `normal`, `stem` along its stem,
/// `along` of the way from its first end to its second and `girth` in
/// radius there: its surface coordinates are the distance along the stem
/// and the distance round it.
pub(crate) fn limb_hit(
    t: f64,
    normal: Vec3,
    (stem, along, girth): (f64, f64, f64),
    tube: &Tube,
) -> Hit {
    let axis = (point(tube.b) - point(tube.a)).normalized();
    let (first, second) = round(axis);
    let angle = mathf::atan2(normal.dot(second), normal.dot(first)) - f64::from(tube.turn);
    Hit {
        t,
        normal,
        shading: normal,
        mark: tube.key,
        along,
        uv: (stem, angle),
        girth,
        material: Some(u32::from(tube.material)),
        tangent: axis,
        relieved: false,
        member: None,
    }
}

/// Where `ray` meets a leaf within `(near, far)`.
fn meet_blade(blade: &Blade, ray: &Ray, (near, far): (f64, f64)) -> Option<Hit> {
    let (base, normal, axis) = (point(blade.base), point(blade.normal), point(blade.axis));
    let facing = normal.dot(ray.dir);
    if facing.abs() < 1e-12 {
        return None;
    }
    let t = normal.dot(base - ray.origin) / facing;
    if !(t > near && t < far) {
        return None;
    }
    let offset = ray.at(t) - base;
    let across = normal.cross(axis);
    let (length, width) = (f64::from(blade.length), f64::from(blade.width));
    let u = offset.dot(axis) / length;
    let v = offset.dot(across) / width;
    if !blade.outline.covers(u, v) {
        return None;
    }
    // Each half tilts up from the midrib, so the blade catches the light as
    // a shallow trough does.
    let from = blade.outline.off_midrib(u, v);
    let tilt = f64::from(blade.fold) * from * v.signum();
    let shading =
        (normal - across * tilt + axis * (0.15 * (u - 0.5) * f64::from(blade.fold))).normalized();
    Some(Hit {
        t,
        normal,
        shading,
        mark: blade.key,
        along: u,
        uv: (u, v),
        girth: 0.0,
        material: Some(u32::from(blade.material)),
        tangent: axis,
        relieved: false,
        member: None,
    })
}

#[cfg(test)]
#[path = "prototype_tests.rs"]
mod tests;
