//! The wireframe craft's renderer: convex hulls built from their corners,
//! seen from the flight's camera, and drawn as vector-display art — dark
//! faces with glowing edges, the hidden ones culled.
//!
//! A frame's craft are gathered as [`Part`]s of items, each item one thing
//! standing in the scene. The items are drawn far to near and each draws its
//! parts in the order its act chose, so a nearer thing hides what stands
//! behind it. A hull is convex, so its faces turned towards the camera never
//! overlap, and the edges beside one of them are exactly those in sight.
//!
//! Drawing produces a [`Display`]: the frame's polygons in the screen's own
//! sub-pixel units, worked out once and then replayed a band of rows at a
//! time on every core.

use alloc::vec::Vec;
use core::f64::consts::{LN_2, TAU};
use core::ops::Range;

use tairix_inline::ArrayVec;
use tairix_raster::{Canvas, Color, ScanScratch, SUBPIXEL};
use tairix_util::space::{Frame, Pose, Vec3};
use tairix_util::{fallible, mathf};
use tairix_wm::Rect;

use super::{band, sub, translucent, Moment, Rgb, View, CAMERA};

/// The most corners a hull is built from, the most faces it may have, the
/// most corners one face may have, and the most lines drawn on it.
const MAX_POINTS: usize = 48;
const MAX_FACES: usize = 64;
const MAX_CORNERS: usize = 24;
const MAX_LINES: usize = 32;

/// How close to the camera anything is drawn, in cells: nearer is clipped.
const NEAR: f64 = 0.12;

/// How far past the screen's edges a polygon is kept before it is clipped,
/// in screens, so no corner reaches a coordinate the scan converter cannot
/// hold.
const GUARD: f64 = 1.0;

/// An edge's core and its glow, in logical pixels wide; how much wider the
/// glow is a cell from the camera, and how much of that the core takes; and
/// how opaque the glow is.
const CORE: f64 = 1.4;
const GLOW: f64 = 5.5;
const THICKEN: f64 = 2.4;
const CORE_THICKEN: f64 = 0.4;
const GLOW_ALPHA: f64 = 0.2;

/// A face's darkness at its dimmest, and how much more of it a face turned
/// straight up shows.
const FACE_BASE: f64 = 0.7;
const FACE_UP: f64 = 0.45;

/// How far off a craft's light is half lost to the haze, in cells, and the
/// haze it fades into.
const FOG: f64 = 55.0;
const HAZE: Rgb = Rgb::new(44.0, 64.0, 118.0);

/// Sides of the polygon a round glow is drawn within, and of the arc about a
/// flame's base.
const GLOW_SIDES: u8 = 16;
const ARC_SIDES: u8 = 12;

/// The corners a solid is built from.
pub(super) type Corners = ArrayVec<Vec3, MAX_POINTS>;

/// The lines drawn on a solid.
pub(super) type Lines = ArrayVec<[Vec3; 2], MAX_LINES>;

/// The colours a craft is drawn in: its edges' light and its faces' dark.
#[derive(Copy, Clone, Debug)]
pub(super) struct Ink {
    pub(super) edge: Rgb,
    pub(super) face: Rgb,
}

/// A convex solid: its corners, its faces wound outwards, the edges between
/// them, and the lines drawn on its faces.
#[derive(Debug)]
pub(super) struct Hull {
    points: Vec<Vec3>,
    faces: Vec<Face>,
    /// Every face's corners, one run after another.
    rings: Vec<u8>,
    edges: Vec<Edge>,
    lines: Vec<Line>,
    /// The mean of its corners, in its own coordinates.
    centre: Vec3,
}

/// One face: where its corners' run lies, anticlockwise seen from outside,
/// and its outward unit normal.
#[derive(Copy, Clone, Debug)]
struct Face {
    ring: (u16, u16),
    normal: Vec3,
}

/// An edge: its two corners, and the faces either side of it.
#[derive(Copy, Clone, Debug)]
struct Edge {
    ends: [u8; 2],
    faces: [u8; 2],
}

/// A line on a hull: seen while the face it lies on is, or always when it
/// lies on none.
#[derive(Copy, Clone, Debug)]
struct Line {
    ends: [Vec3; 2],
    face: Option<u8>,
}

impl Hull {
    /// The convex hull of `points`, with `lines` drawn on it; `None` for
    /// corners enclosing no volume or more than a hull holds, or a heap that
    /// will not give it room.
    pub(super) fn of(points: &[Vec3], lines: &[[Vec3; 2]]) -> Option<Self> {
        if !(4..=MAX_POINTS).contains(&points.len()) || lines.len() > MAX_LINES {
            return None;
        }
        let extent = points
            .iter()
            .fold(0.0, |most: f64, point| most.max(point.length()));
        let tolerance = 1e-7 * extent.max(1e-3);
        let mut hull = Self {
            points: Vec::new(),
            faces: Vec::new(),
            rings: Vec::new(),
            edges: Vec::new(),
            lines: Vec::new(),
            centre: Vec3::ZERO,
        };
        if !(fallible::reserve(&mut hull.points, points.len())
            && fallible::reserve(&mut hull.faces, MAX_FACES)
            && fallible::reserve(&mut hull.rings, MAX_FACES * 4)
            && fallible::reserve(&mut hull.edges, MAX_FACES * 2)
            && fallible::reserve(&mut hull.lines, lines.len()))
        {
            return None;
        }
        hull.points.extend_from_slice(points);
        let count = f64::from(u8::try_from(points.len()).ok()?);
        hull.centre = points.iter().fold(Vec3::ZERO, |sum, point| sum + *point) * (1.0 / count);
        for (a, b, c) in triples(points.len()) {
            let normal = (points[b] - points[a]).cross(points[c] - points[a]);
            if normal.length() <= tolerance {
                continue;
            }
            let Some(outward) = supporting(points, normal.normalized(), points[a], tolerance)
            else {
                continue;
            };
            if !hull.has_plane(outward, points[a], tolerance) {
                hull.add_face(outward, points[a], tolerance)?;
            }
        }
        hull.link()?;
        for &ends in lines {
            let face = hull.faces.iter().position(|face| {
                let on = hull
                    .ring(face)
                    .first()
                    .and_then(|&at| hull.points.get(usize::from(at)));
                on.is_some_and(|on| {
                    ends.iter()
                        .all(|end| mathf::fabs(face.normal.dot(*end - *on)) <= tolerance * 10.0)
                })
            });
            hull.lines.push(Line {
                ends,
                face: face.and_then(|at| u8::try_from(at).ok()),
            });
        }
        // The fewest faces a solid has: a tetrahedron's.
        (hull.faces.len() >= 4).then_some(hull)
    }

    /// The mean of its corners, in its own coordinates.
    pub(super) const fn centre(&self) -> Vec3 {
        self.centre
    }

    /// How many faces it has.
    pub(super) fn face_count(&self) -> usize {
        self.faces.len()
    }

    /// The corners of face `face`, in its own coordinates, anticlockwise seen
    /// from outside.
    pub(super) fn face_corners(&self, face: usize) -> impl Iterator<Item = Vec3> + '_ {
        self.faces
            .get(face)
            .map_or(&[][..], |face| self.ring(face))
            .iter()
            .filter_map(|&at| self.points.get(usize::from(at)).copied())
    }

    /// Whether corner `corner`, placed at `pose`, is in sight of `camera`: a
    /// face it lies on is turned towards it.
    pub(super) fn shows_corner(&self, corner: usize, pose: &Pose, camera: &Camera) -> bool {
        let Some(point) = self.points.get(corner).copied() else {
            return false;
        };
        let seen = camera.relative(pose.point_to_world(point));
        self.faces.iter().any(|face| {
            self.ring(face).iter().any(|&at| usize::from(at) == corner)
                && pose.frame.to_world(face.normal).dot(seen) < 0.0
        })
    }

    /// Face `face`'s run of corners.
    fn ring(&self, face: &Face) -> &[u8] {
        self.rings
            .get(usize::from(face.ring.0)..usize::from(face.ring.1))
            .unwrap_or_default()
    }

    /// Whether a face already lies on the plane through `on` with `normal`.
    fn has_plane(&self, normal: Vec3, on: Vec3, tolerance: f64) -> bool {
        self.faces.iter().any(|face| {
            face.normal.dot(normal) > 1.0 - 1e-9
                && self
                    .ring(face)
                    .first()
                    .and_then(|&at| self.points.get(usize::from(at)))
                    .is_some_and(|corner| mathf::fabs(normal.dot(on - *corner)) <= tolerance)
        })
    }

    /// Add the face on the supporting plane through `on` with outward
    /// `normal`: the corners bounding those that lie on it, in order about
    /// it.
    fn add_face(&mut self, normal: Vec3, on: Vec3, tolerance: f64) -> Option<()> {
        let mut coplanar: ArrayVec<u8, MAX_POINTS> = ArrayVec::new();
        for (at, point) in self.points.iter().enumerate() {
            if mathf::fabs(normal.dot(*point - on)) <= tolerance {
                coplanar.try_push(u8::try_from(at).ok()?).ok()?;
            }
        }
        let across = Frame::around(normal);
        let points = &self.points;
        let flat = |at: u8| {
            let point = points.get(usize::from(at)).copied().unwrap_or_default();
            (point.dot(across.x), point.dot(across.y))
        };
        let ring = convex_ring(&mut coplanar, flat)?;
        if ring.len() < 3 || self.faces.len() >= MAX_FACES {
            return None;
        }
        let first = u16::try_from(self.rings.len()).ok()?;
        self.rings.extend_from_slice(&ring);
        let last = u16::try_from(self.rings.len()).ok()?;
        self.faces.push(Face {
            ring: (first, last),
            normal,
        });
        Some(())
    }

    /// Pair every edge with the faces either side of it; `None` unless each
    /// has exactly two, running it opposite ways, as a closed solid's must.
    fn link(&mut self) -> Option<()> {
        const OPEN: u8 = u8::MAX;
        for (face, &Face { ring, .. }) in self.faces.iter().enumerate() {
            let face = u8::try_from(face).ok()?;
            let ring = self.rings.get(usize::from(ring.0)..usize::from(ring.1))?;
            for (at, &from) in ring.iter().enumerate() {
                let to = *ring.get((at + 1) % ring.len())?;
                if let Some(edge) = self
                    .edges
                    .iter_mut()
                    .find(|edge| edge.ends == [to, from] && edge.faces[1] == OPEN)
                {
                    edge.faces[1] = face;
                } else if self.edges.iter().any(|edge| edge.ends == [from, to]) {
                    return None;
                } else {
                    if !fallible::reserve(&mut self.edges, 1) {
                        return None;
                    }
                    self.edges.push(Edge {
                        ends: [from, to],
                        faces: [face, OPEN],
                    });
                }
            }
        }
        self.edges
            .iter()
            .all(|edge| edge.faces[1] != OPEN)
            .then_some(())
    }
}

/// Every choice of three of `count` indices, each once.
fn triples(count: usize) -> impl Iterator<Item = (usize, usize, usize)> {
    (0..count)
        .flat_map(move |a| (a + 1..count).flat_map(move |b| (b + 1..count).map(move |c| (a, b, c))))
}

/// `normal` or its reverse, whichever points away from every one of `points`,
/// when all of them lie on one side of the plane through `on` and some lie
/// off it: points all in one plane bound no solid.
fn supporting(points: &[Vec3], normal: Vec3, on: Vec3, tolerance: f64) -> Option<Vec3> {
    let (mut above, mut below) = (false, false);
    for point in points {
        let apart = normal.dot(*point - on);
        above |= apart > tolerance;
        below |= apart < -tolerance;
    }
    match (above, below) {
        (true, false) => Some(-normal),
        (false, true) => Some(normal),
        _ => None,
    }
}

/// The corners of `points` bounding them in the plane `flat` lays them in,
/// anticlockwise: Andrew's monotone chain, which keeps no corner lying on a
/// line between two others.
fn convex_ring(
    points: &mut [u8],
    flat: impl Fn(u8) -> (f64, f64),
) -> Option<ArrayVec<u8, MAX_CORNERS>> {
    points.sort_unstable_by(|a, b| {
        let (a, b) = (flat(*a), flat(*b));
        a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1))
    });
    let turns_left = |o: u8, a: u8, b: u8| {
        let (o, a, b) = (flat(o), flat(a), flat(b));
        (a.0 - o.0) * (b.1 - o.1) - (a.1 - o.1) * (b.0 - o.0) > 1e-12
    };
    // Each point in turn, first dropping the chain's last while it and the
    // one before it do not turn left towards the point; never below `floor`.
    let extend = |chain: &mut ArrayVec<u8, { 2 * MAX_POINTS }>, floor: usize, point: u8| {
        while chain.len() >= floor {
            let (Some(&o), Some(&a)) = (chain.get(chain.len() - 2), chain.last()) else {
                break;
            };
            if turns_left(o, a, point) {
                break;
            }
            chain.pop();
        }
        chain.try_push(point).ok()
    };
    let mut chain = ArrayVec::new();
    for &point in points.iter() {
        extend(&mut chain, 2, point)?;
    }
    let lower = chain.len() + 1;
    for &point in points.iter().rev().skip(1) {
        extend(&mut chain, lower, point)?;
    }
    chain.pop();
    ArrayVec::try_from(chain.as_slice()).ok()
}

/// Where the camera stands this frame, and the screen it sees.
#[derive(Copy, Clone, Debug)]
pub(super) struct Camera {
    at: Vec3,
    view: View,
}

impl Camera {
    /// The flight's camera as it stands at `moment`.
    pub(super) fn at(view: &View, moment: Moment) -> Self {
        Self {
            at: Vec3::new(moment.sway, CAMERA, moment.flown),
            view: *view,
        }
    }

    /// Where it stands, in the world.
    pub(super) const fn position(&self) -> Vec3 {
        self.at
    }

    /// The screen it sees.
    pub(super) const fn view(&self) -> &View {
        &self.view
    }

    /// `world` relative to the camera: right of, above and ahead of it.
    fn relative(&self, world: Vec3) -> Vec3 {
        world - self.at
    }

    /// Where a point `seen` relative to the camera, at least [`NEAR`] ahead
    /// of it, lands on the screen, in pixels.
    fn project(&self, seen: Vec3) -> (f64, f64) {
        let scale = self.view.focal / seen.z;
        (
            self.view.centre + seen.x * scale,
            f64::from(self.view.horizon) - seen.y * scale,
        )
    }

    /// How far `world` stands from the camera, in cells.
    pub(super) fn distance(&self, world: Vec3) -> f64 {
        self.relative(world).length()
    }

    /// Whether `world` lands on the screen or within `slack` pixels of it.
    pub(super) fn sees(&self, world: Vec3, slack: f64) -> bool {
        let seen = self.relative(world);
        seen.z >= NEAR && {
            let (x, y) = self.project(seen);
            let (width, height) = (f64::from(self.view.width), f64::from(self.view.height));
            (-slack..=width + slack).contains(&x) && (-slack..=height + slack).contains(&y)
        }
    }
}

/// One of the solids the craft are built from.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) enum HullId {
    ShipFuselage,
    ShipPortWing,
    ShipStarboardWing,
    SaucerLens,
    SaucerDome,
    TankBody,
    TankTurret,
    TankBarrel,
    BikeBody,
    BikeRider,
}

impl HullId {
    /// Every solid, in the order [`Models`] holds them.
    const ALL: [Self; 10] = [
        Self::ShipFuselage,
        Self::ShipPortWing,
        Self::ShipStarboardWing,
        Self::SaucerLens,
        Self::SaucerDome,
        Self::TankBody,
        Self::TankTurret,
        Self::TankBarrel,
        Self::BikeBody,
        Self::BikeRider,
    ];

    /// The corners this solid is the hull of, and the lines drawn on it, as
    /// the module of the craft it belongs to gives them.
    fn shape(self) -> (Corners, Lines) {
        match self {
            Self::ShipFuselage => super::ship::fuselage(),
            Self::ShipPortWing => super::ship::port_wing(),
            Self::ShipStarboardWing => super::ship::starboard_wing(),
            Self::SaucerLens => super::saucer::lens(),
            Self::SaucerDome => super::saucer::dome(),
            Self::TankBody => super::tanks::body(),
            Self::TankTurret => super::tanks::turret(),
            Self::TankBarrel => super::tanks::barrel(),
            Self::BikeBody => super::riders::body(),
            Self::BikeRider => super::riders::rider(),
        }
    }
}

/// Every solid the craft are built from, built once as the scene is made.
pub(super) struct Models {
    hulls: Vec<Hull>,
}

impl Models {
    /// Every solid; `None` when the heap will not give them room.
    pub(super) fn new() -> Option<Self> {
        let mut hulls = Vec::new();
        if !fallible::reserve(&mut hulls, HullId::ALL.len()) {
            return None;
        }
        for id in HullId::ALL {
            let (points, lines) = id.shape();
            hulls.push(Hull::of(&points, &lines)?);
        }
        Some(Self { hulls })
    }

    /// The solid `id` names.
    pub(super) fn get(&self, id: HullId) -> Option<&Hull> {
        let at = HullId::ALL.iter().position(|held| *held == id)?;
        self.hulls.get(at)
    }
}

/// One piece of a thing standing in the scene, placed in the world.
#[derive(Copy, Clone, Debug)]
pub(super) enum Part {
    /// A solid at `pose`: its faces dark, its edges alight.
    Solid {
        hull: HullId,
        pose: Pose,
        ink: &'static Ink,
        alpha: f64,
    },
    /// One face of a solid at `pose`, outlined alone: a shard of a wreck.
    Shard {
        hull: HullId,
        face: u8,
        pose: Pose,
        ink: &'static Ink,
        alpha: f64,
    },
    /// A glowing line.
    Line {
        ends: [Vec3; 2],
        ink: &'static Ink,
        alpha: f64,
    },
    /// A glowing ring of `sides` straight lines about `centre`, in the plane
    /// of `frame`'s x and y axes.
    Ring {
        centre: Vec3,
        frame: Frame,
        radius: f64,
        sides: u8,
        ink: &'static Ink,
        alpha: f64,
    },
    /// A flame from `base` to `tip`, `radius` cells about its base; a round
    /// glow where the two are one.
    Glow {
        base: Vec3,
        tip: Vec3,
        radius: f64,
        light: Rgb,
        alpha: f64,
    },
    /// A round glow at corner `corner` of a solid at `pose`, lit only while a
    /// face it lies on is turned towards the camera.
    Beacon {
        hull: HullId,
        corner: u8,
        pose: Pose,
        radius: f64,
        light: Rgb,
        alpha: f64,
    },
}

/// One thing standing in the scene: the run of its parts, drawn in turn, and
/// how far off it stands.
#[derive(Copy, Clone, Debug)]
struct Item {
    parts: (u32, u32),
    depth: f64,
}

/// A frame's craft, as the acts set them out.
pub(super) struct Stage {
    camera: Camera,
    parts: Vec<Part>,
    items: Vec<Item>,
    /// The first part and the depth of the thing begun.
    open: Option<(u32, f64)>,
}

impl Stage {
    /// An empty stage with room for `parts` parts; `None` when the heap will
    /// not give it.
    pub(super) fn new(view: &View, parts: usize) -> Option<Self> {
        let mut stage = Self {
            camera: Camera::at(view, Moment::default()),
            parts: Vec::new(),
            items: Vec::new(),
            open: None,
        };
        (fallible::reserve(&mut stage.parts, parts) && fallible::reserve(&mut stage.items, parts))
            .then_some(stage)
    }

    /// Clear the stage for a frame `camera` sees.
    pub(super) fn reset(&mut self, camera: Camera) {
        self.camera = camera;
        self.parts.clear();
        self.items.clear();
        self.open = None;
    }

    /// The camera this frame is seen by.
    pub(super) const fn camera(&self) -> &Camera {
        &self.camera
    }

    /// Every part set out so far, in the order it was pushed.
    #[cfg(test)]
    pub(super) fn parts(&self) -> &[Part] {
        &self.parts
    }

    /// Begin a thing standing about `at`, which orders it among the rest:
    /// every part pushed until the next begins is its own.
    pub(super) fn begin(&mut self, at: Vec3) {
        self.close();
        let first = u32::try_from(self.parts.len()).unwrap_or(u32::MAX);
        self.open = Some((first, self.camera.distance(at)));
    }

    /// Add a part to the thing begun. One the heap has no room for is left
    /// out, and so is one pushed with nothing begun.
    pub(super) fn push(&mut self, part: Part) {
        if self.open.is_some() && fallible::reserve(&mut self.parts, 1) {
            self.parts.push(part);
        }
    }

    fn close(&mut self) {
        let Some((first, depth)) = self.open.take() else {
            return;
        };
        let last = u32::try_from(self.parts.len()).unwrap_or(u32::MAX);
        if last > first && fallible::reserve(&mut self.items, 1) {
            self.items.push(Item {
                parts: (first, last),
                depth,
            });
        }
    }

    /// Draw every thing onto `display` far to near, and note in `sky` the box
    /// each reaches above the horizon.
    pub(super) fn draw(&mut self, models: &Models, display: &mut Display, sky: &mut Vec<Rect>) {
        self.close();
        display.clear();
        sky.clear();
        self.items
            .sort_unstable_by(|a, b| b.depth.total_cmp(&a.depth));
        let view = &self.camera.view;
        let above = Rect::new(0, 0, view.width, view.horizon);
        for item in &self.items {
            let parts = usize::try_from(item.parts.0).unwrap_or(usize::MAX)
                ..usize::try_from(item.parts.1).unwrap_or(usize::MAX);
            display.reach = None;
            for part in self.parts.get(parts).unwrap_or_default() {
                display.part(part, models, &self.camera);
            }
            gather(sky, display.reach().intersection(&above));
        }
    }
}

/// How near two boxes a frame's craft reach above the horizon may come, in
/// pixels, before they are laid back as one: a blast's shards and sparks each
/// reach a box of their own, and laying the sky back under each apart costs
/// more than laying it back once under all of them.
const GATHER: i32 = 24;

/// Add `rect` to `sky`, as one box with every box it comes within [`GATHER`]
/// pixels of, and they with theirs.
fn gather(sky: &mut Vec<Rect>, mut rect: Rect) {
    if rect.is_empty() {
        return;
    }
    let near = |a: &Rect, b: &Rect| {
        a.left() - GATHER < b.right()
            && b.left() - GATHER < a.right()
            && a.top() - GATHER < b.bottom()
            && b.top() - GATHER < a.bottom()
    };
    let mut at = 0;
    while at < sky.len() {
        if sky.get(at).is_some_and(|held| near(held, &rect)) {
            rect = rect.union(&sky.swap_remove(at));
            at = 0;
        } else {
            at += 1;
        }
    }
    if fallible::reserve(sky, 1) {
        sky.push(rect);
    }
}

/// How one polygon of the display is filled.
#[derive(Copy, Clone, Debug)]
enum Fill {
    /// Flat, at its colour.
    Flat,
    /// A glow at its colour: round about `base`, `radius` pixels across, and
    /// drawn out as a flame `length` pixels along the unit `axis`.
    Glow {
        base: (f64, f64),
        axis: (f64, f64),
        length: f64,
        radius: f64,
    },
}

/// One polygon of the display: where its corners' run lies, its colour and
/// fill, and the rows it reaches.
#[derive(Copy, Clone, Debug)]
struct Shape {
    corners: (u32, u32),
    colour: Color,
    fill: Fill,
    rows: (u32, u32),
}

/// A frame's craft as polygons on the screen, far to near.
pub(super) struct Display {
    width: u32,
    height: u32,
    corners: Vec<(i32, i32)>,
    shapes: Vec<Shape>,
    /// The rows any shape reaches.
    rows: (u32, u32),
    /// The box, in pixels, the thing being drawn reaches: left, top, right,
    /// bottom.
    reach: Option<(i32, i32, i32, i32)>,
}

impl Display {
    /// An empty display of `view`'s screen with room for `shapes` polygons;
    /// `None` when the heap will not give it.
    pub(super) fn new(view: &View, shapes: usize) -> Option<Self> {
        let mut display = Self {
            width: view.width,
            height: view.height,
            corners: Vec::new(),
            shapes: Vec::new(),
            rows: (u32::MAX, 0),
            reach: None,
        };
        (fallible::reserve(&mut display.corners, shapes.saturating_mul(6))
            && fallible::reserve(&mut display.shapes, shapes))
        .then_some(display)
    }

    fn clear(&mut self) {
        self.corners.clear();
        self.shapes.clear();
        self.rows = (u32::MAX, 0);
        self.reach = None;
    }

    /// The rows any shape reaches, empty when there is none.
    pub(super) fn rows(&self) -> Range<u32> {
        self.rows.0.min(self.rows.1)..self.rows.1
    }

    /// The box on the screen the thing being drawn reaches.
    fn reach(&self) -> Rect {
        let Some((left, top, right, bottom)) = self.reach else {
            return Rect::EMPTY;
        };
        let clamp = |value: i32, most: u32| value.clamp(0, i32::try_from(most).unwrap_or(i32::MAX));
        let (left, top) = (clamp(left, self.width), clamp(top, self.height));
        let (right, bottom) = (clamp(right, self.width), clamp(bottom, self.height));
        Rect::new(
            left,
            top,
            u32::try_from(right - left).unwrap_or(0),
            u32::try_from(bottom - top).unwrap_or(0),
        )
    }

    /// Draw every shape reaching rows `rows` onto `canvas`, which holds them,
    /// in order.
    pub(super) fn replay(
        &self,
        canvas: &mut impl Canvas,
        rows: &Range<u32>,
        scratch: &mut ScanScratch,
    ) {
        for shape in &self.shapes {
            if shape.rows.1 <= rows.start || shape.rows.0 >= rows.end {
                continue;
            }
            let run = usize::try_from(shape.corners.0).unwrap_or(usize::MAX)
                ..usize::try_from(shape.corners.1).unwrap_or(usize::MAX);
            let Some(polygon) = self.corners.get(run) else {
                continue;
            };
            match shape.fill {
                Fill::Flat => canvas.fill_polygon_subpixel(polygon, shape.colour, scratch),
                Fill::Glow {
                    base,
                    axis,
                    length,
                    radius,
                } => canvas.wash_polygon_subpixel(
                    polygon,
                    shape.colour,
                    |x, y| {
                        let at = (f64::from(x) + 0.5, f64::from(y) + 0.5);
                        glow_strength(at, base, axis, length, radius)
                    },
                    scratch,
                ),
            }
        }
    }

    /// Draw `part` as `camera` sees it.
    fn part(&mut self, part: &Part, models: &Models, camera: &Camera) {
        match *part {
            Part::Solid {
                hull,
                pose,
                ink,
                alpha,
            } => {
                if let Some(hull) = models.get(hull) {
                    self.solid(hull, &pose, ink, alpha, camera);
                }
            }
            Part::Shard {
                hull,
                face,
                pose,
                ink,
                alpha,
            } => {
                if let Some(hull) = models.get(hull) {
                    let depth = camera.distance(pose.point_to_world(hull.centre));
                    let light = lit(ink.edge, depth);
                    let mut ring: ArrayVec<Vec3, MAX_CORNERS> = ArrayVec::new();
                    for corner in hull.face_corners(usize::from(face)) {
                        let _ = ring.try_push(camera.relative(pose.point_to_world(corner)));
                    }
                    self.outline(&ring, light, alpha, depth, camera);
                }
            }
            Part::Line { ends, ink, alpha } => {
                let seen = ends.map(|end| camera.relative(end));
                let depth = f64::midpoint(seen[0].length(), seen[1].length());
                self.edge(seen, lit(ink.edge, depth), alpha, depth, camera);
            }
            Part::Ring {
                centre,
                frame,
                radius,
                sides,
                ink,
                alpha,
            } => {
                let depth = camera.distance(centre);
                let sides = sides.clamp(3, u8::try_from(MAX_CORNERS).unwrap_or(u8::MAX));
                let mut ring: ArrayVec<Vec3, MAX_CORNERS> = ArrayVec::new();
                for side in 0..sides {
                    let turn = TAU * f64::from(side) / f64::from(sides);
                    let at = centre
                        + frame.x * (radius * mathf::cos(turn))
                        + frame.y * (radius * mathf::sin(turn));
                    let _ = ring.try_push(camera.relative(at));
                }
                self.outline(&ring, lit(ink.edge, depth), alpha, depth, camera);
            }
            Part::Glow {
                base,
                tip,
                radius,
                light,
                alpha,
            } => self.glow(
                [camera.relative(base), camera.relative(tip)],
                radius,
                light,
                alpha,
                camera,
            ),
            Part::Beacon {
                hull,
                corner,
                pose,
                radius,
                light,
                alpha,
            } => {
                let corner = usize::from(corner);
                let Some(hull) = models.get(hull) else {
                    return;
                };
                if let (true, Some(point)) = (
                    hull.shows_corner(corner, &pose, camera),
                    hull.points.get(corner),
                ) {
                    let seen = camera.relative(pose.point_to_world(*point));
                    self.glow([seen, seen], radius, light, alpha, camera);
                }
            }
        }
    }

    /// A solid: the faces turned towards the camera, dark, then every edge
    /// beside one of them and every line on one of them, alight.
    fn solid(&mut self, hull: &Hull, pose: &Pose, ink: &Ink, alpha: f64, camera: &Camera) {
        let mut seen: ArrayVec<Vec3, MAX_POINTS> = ArrayVec::new();
        for point in &hull.points {
            if seen
                .try_push(camera.relative(pose.point_to_world(*point)))
                .is_err()
            {
                return;
            }
        }
        let depth = camera.distance(pose.point_to_world(hull.centre));
        let fade = fog(depth);
        let mut front: u64 = 0;
        for (at, face) in hull.faces.iter().enumerate() {
            let ring = hull.ring(face);
            let normal = pose.frame.to_world(face.normal);
            let Some(on) = ring
                .first()
                .and_then(|&corner| seen.get(usize::from(corner)))
            else {
                continue;
            };
            if normal.dot(*on) >= 0.0 {
                continue;
            }
            front |= 1 << at;
            let dark = HAZE.mix(ink.face * (FACE_BASE + FACE_UP * normal.y), fade);
            let mut corners: ArrayVec<Vec3, MAX_CORNERS> = ArrayVec::new();
            for &corner in ring {
                if let Some(point) = seen.get(usize::from(corner)) {
                    let _ = corners.try_push(*point);
                }
            }
            self.polygon(&corners, translucent(dark, alpha), camera);
        }
        let light = lit(ink.edge, depth);
        for edge in &hull.edges {
            if front & (1 << edge.faces[0] | 1 << edge.faces[1]) == 0 {
                continue;
            }
            if let (Some(from), Some(to)) = (
                seen.get(usize::from(edge.ends[0])),
                seen.get(usize::from(edge.ends[1])),
            ) {
                self.edge([*from, *to], light, alpha, depth, camera);
            }
        }
        for line in &hull.lines {
            if line.face.is_none_or(|face| front & (1 << face) != 0) {
                let ends = line
                    .ends
                    .map(|end| camera.relative(pose.point_to_world(end)));
                self.edge(ends, light, alpha, depth, camera);
            }
        }
    }

    /// The closed loop through `ring`, seen relative to the camera, alight.
    fn outline(&mut self, ring: &[Vec3], light: Rgb, alpha: f64, depth: f64, camera: &Camera) {
        for (at, from) in ring.iter().enumerate() {
            if let Some(to) = ring.get((at + 1) % ring.len()) {
                self.edge([*from, *to], light, alpha, depth, camera);
            }
        }
    }

    /// A glowing line between `ends`, seen relative to the camera: its glow,
    /// then its core, each squared off half its width past either end.
    fn edge(&mut self, ends: [Vec3; 2], light: Rgb, alpha: f64, depth: f64, camera: &Camera) {
        let Some([from, to]) = clip_segment(ends) else {
            return;
        };
        let Some((from, to)) =
            guard_segment(camera.project(from), camera.project(to), &camera.view)
        else {
            return;
        };
        let near = THICKEN / depth.max(1.0);
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let length = mathf::hypot(dx, dy);
        let along = if length > f64::EPSILON {
            (dx / length, dy / length)
        } else {
            (1.0, 0.0)
        };
        for (width, strength) in [(GLOW + near, GLOW_ALPHA), (CORE + near * CORE_THICKEN, 1.0)] {
            let half = width * camera.view.pixel / 2.0;
            let start = (from.0 - along.0 * half, from.1 - along.1 * half);
            let end = (to.0 + along.0 * half, to.1 + along.1 * half);
            if let Some(quad) = band(start, end, half) {
                self.shape(&quad, translucent(light, alpha * strength), Fill::Flat);
            }
        }
    }

    /// A face seen relative to the camera, clipped to what stands ahead of it
    /// and to the guarded screen.
    fn polygon(&mut self, ring: &[Vec3], colour: Color, camera: &Camera) {
        let mut ahead: ArrayVec<(f64, f64), { MAX_CORNERS + 1 }> = ArrayVec::new();
        for (at, &point) in ring.iter().enumerate() {
            let Some(&next) = ring.get((at + 1) % ring.len()) else {
                continue;
            };
            if point.z >= NEAR {
                let _ = ahead.try_push(camera.project(point));
            }
            if (point.z >= NEAR) != (next.z >= NEAR) {
                let share = (NEAR - point.z) / (next.z - point.z);
                let _ = ahead.try_push(camera.project(point.lerp(next, share)));
            }
        }
        let guarded = guard_polygon(&ahead, &camera.view);
        let mut corners: ArrayVec<(i32, i32), GUARDED> = ArrayVec::new();
        for &point in &guarded {
            let _ = corners.try_push(sub(point));
        }
        self.shape(&corners, colour, Fill::Flat);
    }

    /// A glow from `base` towards `tip`, seen relative to the camera.
    fn glow(
        &mut self,
        [base, tip]: [Vec3; 2],
        radius: f64,
        light: Rgb,
        alpha: f64,
        camera: &Camera,
    ) {
        if base.z < NEAR {
            return;
        }
        let tip = if tip.z < NEAR {
            base.lerp(tip, (base.z - NEAR) / (base.z - tip.z))
        } else {
            tip
        };
        let (centre, end) = (camera.project(base), camera.project(tip));
        let reach = radius * camera.view.focal / base.z;
        let (dx, dy) = (end.0 - centre.0, end.1 - centre.1);
        let length = mathf::hypot(dx, dy);
        if reach < 0.35 || !touches_screen(centre, reach + length, &camera.view) {
            return;
        }
        let axis = if length > f64::EPSILON {
            (dx / length, dy / length)
        } else {
            (1.0, 0.0)
        };
        let mut outline: ArrayVec<(i32, i32), { GLOW_SIDES as usize + 1 }> = ArrayVec::new();
        // The corners of the polygon about the disc from angle `from` through
        // `span`, pushed out so its sides clear the circle.
        let mut around = |from: f64, span: f64, steps: u8, closed: bool| {
            let last = if closed { steps } else { steps + 1 };
            let widen = reach / mathf::cos(span / f64::from(steps) / 2.0);
            for step in 0..last {
                let turn = from + span * f64::from(step) / f64::from(steps);
                let _ = outline.try_push(sub((
                    centre.0 + widen * mathf::cos(turn),
                    centre.1 + widen * mathf::sin(turn),
                )));
            }
        };
        if length <= reach {
            around(0.0, TAU, GLOW_SIDES, true);
        } else {
            // The hull of the base's disc and the tip: the tip, then the arc
            // between the tangents from it, round the far side.
            let spread = mathf::acos(reach / length);
            let heading = mathf::atan2(axis.1, axis.0);
            around(heading + spread, TAU - 2.0 * spread, ARC_SIDES, false);
            let _ = outline.try_push(sub(end));
        }
        self.shape(
            &outline,
            translucent(light, alpha),
            Fill::Glow {
                base: centre,
                axis,
                length,
                radius: reach,
            },
        );
    }

    /// Add one polygon, noting the rows and the box it reaches; one wholly
    /// off the screen is left out.
    fn shape(&mut self, polygon: &[(i32, i32)], colour: Color, fill: Fill) {
        if polygon.len() < 3 || colour.a == 0 {
            return;
        }
        let (mut left, mut top, mut right, mut bottom) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
        for &(x, y) in polygon {
            left = left.min(x.div_euclid(SUBPIXEL));
            top = top.min(y.div_euclid(SUBPIXEL));
            right = right.max(x.div_euclid(SUBPIXEL) + 1);
            bottom = bottom.max(y.div_euclid(SUBPIXEL) + 1);
        }
        let (width, height) = (
            i32::try_from(self.width).unwrap_or(i32::MAX),
            i32::try_from(self.height).unwrap_or(i32::MAX),
        );
        if right <= 0 || bottom <= 0 || left >= width || top >= height {
            return;
        }
        if !(fallible::reserve(&mut self.corners, polygon.len())
            && fallible::reserve(&mut self.shapes, 1))
        {
            return;
        }
        let rows = (
            u32::try_from(top.max(0)).unwrap_or(0),
            u32::try_from(bottom.min(height)).unwrap_or(0),
        );
        let first = u32::try_from(self.corners.len()).unwrap_or(u32::MAX);
        self.corners.extend_from_slice(polygon);
        let last = u32::try_from(self.corners.len()).unwrap_or(u32::MAX);
        self.shapes.push(Shape {
            corners: (first, last),
            colour,
            fill,
            rows,
        });
        self.rows = (self.rows.0.min(rows.0), self.rows.1.max(rows.1));
        self.reach = Some(
            self.reach
                .map_or((left, top, right, bottom), |(l, t, r, b)| {
                    (l.min(left), t.min(top), r.max(right), b.max(bottom))
                }),
        );
    }
}

/// The most corners a polygon keeps once clipped to the guarded screen.
const GUARDED: usize = MAX_CORNERS + 8;

/// How strongly a glow lights the pixel centred at `at`, `0` to `255`.
///
/// Round about `base`, falling to nothing `radius` from it; and, where it is
/// drawn out as a flame, a cone narrowing from that disc to nothing `length`
/// along `axis`, fading as it goes. The stronger of the two shows, so a
/// flame the camera looks along shrinks smoothly into the disc alone.
fn glow_strength(
    at: (f64, f64),
    base: (f64, f64),
    axis: (f64, f64),
    length: f64,
    radius: f64,
) -> u8 {
    let (dx, dy) = (at.0 - base.0, at.1 - base.1);
    let round = {
        let open = (1.0 - (dx * dx + dy * dy) / (radius * radius)).max(0.0);
        open * open
    };
    let along = dx * axis.0 + dy * axis.1;
    let flame = if along > 0.0 && along < length {
        let left = 1.0 - along / length;
        let across = dx * axis.1 - dy * axis.0;
        let width = radius * left;
        let open = (1.0 - across * across / (width * width)).max(0.0);
        open * open * left * left
    } else {
        0.0
    };
    u8::try_from(mathf::round_i32(round.max(flame) * 255.0).clamp(0, 255)).unwrap_or(0)
}

/// The part of the segment `ends` at least [`NEAR`] ahead of the camera.
fn clip_segment([from, to]: [Vec3; 2]) -> Option<[Vec3; 2]> {
    let cut = |inside: Vec3, outside: Vec3| {
        inside.lerp(outside, (inside.z - NEAR) / (inside.z - outside.z))
    };
    match (from.z >= NEAR, to.z >= NEAR) {
        (true, true) => Some([from, to]),
        (true, false) => Some([from, cut(from, to)]),
        (false, true) => Some([cut(to, from), to]),
        (false, false) => None,
    }
}

/// The screen with [`GUARD`] screens of slack about it: left, top, right,
/// bottom.
fn guard(view: &View) -> [f64; 4] {
    let (width, height) = (f64::from(view.width), f64::from(view.height));
    let slack = GUARD * width.max(height);
    [-slack, -slack, width + slack, height + slack]
}

/// Whether a glow about `centre` reaching `reach` pixels touches the screen.
fn touches_screen(centre: (f64, f64), reach: f64, view: &View) -> bool {
    centre.0 + reach >= 0.0
        && centre.1 + reach >= 0.0
        && centre.0 - reach <= f64::from(view.width)
        && centre.1 - reach <= f64::from(view.height)
}

/// The part of the segment `from`–`to` inside the guarded screen, by Liang
/// and Barsky's clip; `None` when none of it is.
fn guard_segment(
    from: (f64, f64),
    to: (f64, f64),
    view: &View,
) -> Option<((f64, f64), (f64, f64))> {
    let [left, top, right, bottom] = guard(view);
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let (mut enter, mut leave) = (0.0_f64, 1.0_f64);
    for (towards, room) in [
        (-dx, from.0 - left),
        (dx, right - from.0),
        (-dy, from.1 - top),
        (dy, bottom - from.1),
    ] {
        if mathf::fabs(towards) < f64::EPSILON {
            if room < 0.0 {
                return None;
            }
        } else if towards < 0.0 {
            enter = enter.max(room / towards);
        } else {
            leave = leave.min(room / towards);
        }
    }
    (enter <= leave).then_some((
        (from.0 + dx * enter, from.1 + dy * enter),
        (from.0 + dx * leave, from.1 + dy * leave),
    ))
}

/// The part of the convex polygon `ring` inside the guarded screen, by
/// Sutherland and Hodgman's clip.
fn guard_polygon(ring: &[(f64, f64)], view: &View) -> ArrayVec<(f64, f64), GUARDED> {
    let [left, top, right, bottom] = guard(view);
    let Ok(mut kept) = ArrayVec::<(f64, f64), GUARDED>::try_from(ring) else {
        return ArrayVec::new();
    };
    for (horizontal, limit, keep_above) in [
        (true, left, true),
        (true, right, false),
        (false, top, true),
        (false, bottom, false),
    ] {
        let value = |point: (f64, f64)| if horizontal { point.0 } else { point.1 };
        let inside = |point: (f64, f64)| {
            if keep_above {
                value(point) >= limit
            } else {
                value(point) <= limit
            }
        };
        let mut next: ArrayVec<(f64, f64), GUARDED> = ArrayVec::new();
        for (at, &point) in kept.iter().enumerate() {
            let Some(&following) = kept.get((at + 1) % kept.len()) else {
                continue;
            };
            if inside(point) {
                let _ = next.try_push(point);
            }
            if inside(point) != inside(following) {
                let share = (limit - value(point)) / (value(following) - value(point));
                let _ = next.try_push((
                    point.0 + (following.0 - point.0) * share,
                    point.1 + (following.1 - point.1) * share,
                ));
            }
        }
        kept = next;
    }
    kept
}

/// How much of a craft's own light still reaches the camera from `depth`
/// cells off.
fn fog(depth: f64) -> f64 {
    mathf::exp(-depth * LN_2 / FOG)
}

/// An edge's light seen from `depth` cells off, faded into the haze.
fn lit(edge: Rgb, depth: f64) -> Rgb {
    HAZE.mix(edge, fog(depth))
}

#[cfg(test)]
#[path = "wire_tests.rs"]
mod tests;
