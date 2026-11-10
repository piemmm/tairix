//! A pass's front end: each draw's primitives cut, in order, into batches of
//! one instance; each batch's vertices fetched and shaded a tile of lanes at
//! a time, and its primitives assembled, clipped, culled and set up, every
//! piece noting the pixels it may reach.

use alloc::vec::Vec;
use core::sync::atomic::AtomicU32;

use tairix_inline::ArrayVec;
use tairix_raster::clip::{Mapped, MAX_CLIP_CORNERS};
use tairix_raster::{Primitive, MESH_SUB_UNITS};
use tairix_util::mathf;

use super::{Area, Drawn};
use crate::pass::{Count, IndexFormat};
use crate::render::{CullMode, FrontFace, Topology, VertexStep};
use crate::software::dispatch::Workspace;
use crate::software::shader::exec::{shade, Lanes, Placement};
use crate::software::shader::ir::{Carries, Fixed, LANES};
use crate::software::texel::{decode, raw_at, Plane};
use crate::software::Meter;

/// The most primitives one batch assembles.
pub(super) const BATCH_PRIMITIVES: u32 = 32;

/// The most positions one batch's primitives read: a list of triangles'.
pub(super) const BATCH_POSITIONS: usize = 3 * BATCH_PRIMITIVES as usize;

/// The batches one flush of the front end makes before the back end draws
/// what they set up.
pub(super) const FLUSH_BATCHES: usize = 16;

/// The indices a search for a restart reads between two looks at the clock.
const SCAN_STEP: u32 = 4096;

/// The slots of the table a batch finds its distinct vertices through: twice
/// the most it names, so never full.
const TABLE: usize = (2 * BATCH_POSITIONS).next_power_of_two();

/// A batch: primitives `first..first + count` of the run of `length`
/// positions from position `run` of one instance of one draw.
#[derive(Copy, Clone, Debug)]
pub(super) struct Planned {
    pub(super) draw: usize,
    pub(super) instance: u32,
    run: u32,
    length: u32,
    first: u32,
    count: u32,
}

/// Where cutting a pass's draws into batches has reached: the draw and
/// instance, the run of positions being cut and its length, once found, its
/// next primitive, and whether the draw has cut a batch.
pub(super) struct Planner {
    draw: usize,
    instance: u32,
    run: u32,
    length: Option<u32>,
    next: u32,
    cut: bool,
}

/// The positions and instances a draw counts.
fn extent(count: &Count) -> (u32, (u32, u32)) {
    let (positions, instances) = match count {
        Count::Vertices {
            vertices,
            instances,
        } => (vertices, instances),
        Count::Indices {
            indices, instances, ..
        } => (indices, instances),
    };
    (
        positions.end.saturating_sub(positions.start),
        (instances.start, instances.end),
    )
}

/// The primitives `length` positions of `topology` make.
const fn primitives(topology: Topology, length: u32) -> u32 {
    match topology {
        Topology::Points => length,
        Topology::Lines => length / 2,
        Topology::LineStrip => length.saturating_sub(1),
        Topology::Triangles => length / 3,
        Topology::TriangleStrip | Topology::TriangleFan => length.saturating_sub(2),
    }
}

/// The positions, within its run, primitive `i` of `topology` reads, in the
/// order its corners are taken — the first is its provoking vertex — and how
/// many it reads.
const fn corners(topology: Topology, i: u32) -> ([u32; 3], usize) {
    match topology {
        Topology::Points => ([i, 0, 0], 1),
        Topology::Lines => ([2 * i, 2 * i + 1, 0], 2),
        Topology::LineStrip => ([i, i + 1, 0], 2),
        Topology::Triangles => ([3 * i, 3 * i + 1, 3 * i + 2], 3),
        Topology::TriangleStrip if i % 2 == 1 => ([i, i + 2, i + 1], 3),
        Topology::TriangleStrip => ([i, i + 1, i + 2], 3),
        Topology::TriangleFan => ([i + 1, i + 2, 0], 3),
    }
}

impl Planner {
    pub(super) const fn new() -> Self {
        Self {
            draw: 0,
            instance: 0,
            run: 0,
            length: None,
            next: 0,
            cut: false,
        }
    }

    /// Whether every draw is cut.
    pub(super) const fn done(&self, draws: usize) -> bool {
        self.draw >= draws
    }

    /// Cut up to [`FLUSH_BATCHES`] more batches of `draws` into `out`, in
    /// order, the indices read finding where each run of a strip or fan
    /// restarts counted on `meter`: `false` where it stopped.
    pub(super) fn plan(
        &mut self,
        bound: &[Drawn<'_>],
        out: &mut Vec<Planned>,
        meter: &mut Meter<'_>,
    ) -> bool {
        out.clear();
        while out.len() < FLUSH_BATCHES {
            let Some(drawn) = bound.get(self.draw) else {
                return true;
            };
            let (positions, (first_instance, last_instance)) = extent(&drawn.work.count);
            if self.instance < first_instance {
                self.instance = first_instance;
            }
            if self.instance >= last_instance {
                self.advance_draw();
                continue;
            }
            let length = match self.length {
                Some(length) => length,
                None => match self.find_run(drawn, positions, meter) {
                    Some(length) => length,
                    None => return false,
                },
            };
            let made = primitives(drawn.render.topology, length);
            if self.next >= made {
                // The run's end, and past the restart that ended it.
                let after = self.run.saturating_add(length).saturating_add(1);
                if after < positions && drawn.restarts() {
                    self.run = after;
                } else if !self.cut {
                    // Every instance cuts what the first did: nothing.
                    self.advance_draw();
                    continue;
                } else {
                    if !meter.proceed(1) {
                        return false;
                    }
                    self.instance = self.instance.saturating_add(1);
                    self.run = 0;
                }
                self.length = None;
                self.next = 0;
                continue;
            }
            let count = (made - self.next).min(BATCH_PRIMITIVES);
            if out.try_reserve(1).is_err() {
                return false;
            }
            out.push(Planned {
                draw: self.draw,
                instance: self.instance,
                run: self.run,
                length,
                first: self.next,
                count,
            });
            self.next += count;
            self.cut = true;
        }
        true
    }

    fn advance_draw(&mut self) {
        self.draw += 1;
        self.instance = 0;
        self.run = 0;
        self.length = None;
        self.next = 0;
        self.cut = false;
    }

    /// The length of the run from its start: to the next restart index where
    /// the draw restarts, else to its end. `None` where it stopped.
    fn find_run(
        &mut self,
        drawn: &Drawn<'_>,
        positions: u32,
        meter: &mut Meter<'_>,
    ) -> Option<u32> {
        let start = self.run.min(positions);
        let mut end = positions;
        if drawn.restarts() {
            for position in start..positions {
                if (position - start).is_multiple_of(SCAN_STEP)
                    && !meter.proceed(u64::from(SCAN_STEP))
                {
                    return None;
                }
                if drawn.is_restart(position) {
                    end = position;
                    break;
                }
            }
        }
        let length = end - start;
        self.length = Some(length);
        Some(length)
    }
}

impl Drawn<'_> {
    /// Whether an index of all ones restarts its strips or fans.
    fn restarts(&self) -> bool {
        self.render.restart && self.index.is_some()
    }

    /// The raw index at `position` of the draw's indices: zero past its
    /// buffer.
    fn raw_index(&self, position: u32) -> u32 {
        let Some((words, start, size, format)) = self.index else {
            return 0;
        };
        let Count::Indices { indices, .. } = &self.work.count else {
            return 0;
        };
        let bytes = format.bytes();
        let at = u64::from(indices.start.saturating_add(position)) * bytes;
        if at.saturating_add(bytes) > size {
            return 0;
        }
        raw_at(words, start + at, bytes)[0]
    }

    /// Whether `position` holds the restart index.
    fn is_restart(&self, position: u32) -> bool {
        let all = match self.index.map(|(_, _, _, format)| format) {
            Some(IndexFormat::U16) => u32::from(u16::MAX),
            _ => u32::MAX,
        };
        self.raw_index(position) == all
    }

    /// The vertex `position` of the draw names.
    fn vertex(&self, position: u32) -> u32 {
        match &self.work.count {
            Count::Vertices { vertices, .. } => vertices.start.wrapping_add(position),
            Count::Indices { base_vertex, .. } => self
                .raw_index(position)
                .cast_signed()
                .wrapping_add(*base_vertex)
                .cast_unsigned(),
        }
    }

    /// The attribute `fetch` names of `element`, its channels decoded: zero
    /// past its buffer, with a missing alpha one.
    fn attribute(&self, fetch: &crate::render::Fetch, element: u32) -> [u32; 4] {
        let format = fetch.format;
        let raw = self
            .buffers
            .get(fetch.buffer as usize)
            .copied()
            .flatten()
            .and_then(|(words, start, size): (&[AtomicU32], u64, u64)| {
                let (stride, _) = self.render.buffers.get(fetch.buffer as usize)?;
                let bytes = u64::from(format.texel_bytes());
                let at = u64::from(element)
                    .checked_mul(u64::from(*stride))?
                    .checked_add(u64::from(fetch.offset))?;
                (at.checked_add(bytes)? <= size).then(|| raw_at(words, start + at, bytes))
            })
            .unwrap_or([0; 4]);
        decode(format, Plane::Colour, raw, self.srgb)
    }
}

/// Word `word` of the port `fetch` feeds, from its attribute's `decoded`
/// channels: narrowed to 16 bits where the port is.
fn port_word(fetch: &crate::render::Fetch, decoded: &[u32; 4], word: u32) -> u32 {
    let value = decoded
        .get((fetch.channel + word) as usize)
        .copied()
        .unwrap_or(0);
    if !fetch.narrow {
        return value;
    }
    match fetch.format.component() {
        tairix_spirv::Component::Float => u32::from(mathf::to_half(f32::from_bits(value))),
        tairix_spirv::Component::Sint | tairix_spirv::Component::Uint => value & 0xffff,
    }
}

/// A primitive set up: the convex polygon its clip left — a fan of pieces
/// from its first corner, a point's or line's square two of them — the
/// records of the corners of the primitive the draw named, which way it
/// faces, its depth's offset, the pixels it may reach, and, for a point, its
/// square.
#[derive(Clone, Debug)]
pub(super) struct Setup {
    pub(super) polygon: ArrayVec<Mapped, MAX_CLIP_CORNERS>,
    pub(super) corners: [u32; 3],
    pub(super) front: bool,
    pub(super) depth_offset: f64,
    pub(super) area: Area,
    pub(super) point: Option<(f64, f64, f64)>,
}

impl Setup {
    /// Piece `piece` of its fan, set up: `None` where it covers no area.
    pub(super) fn piece(&self, piece: usize) -> Option<Primitive> {
        let (Some(&first), Some(&second), Some(&third)) = (
            self.polygon.first(),
            self.polygon.get(piece + 1),
            self.polygon.get(piece + 2),
        ) else {
            return None;
        };
        Primitive::new([first, second, third])
    }

    /// How many pieces its fan holds.
    pub(super) fn pieces(&self) -> usize {
        self.polygon.len().saturating_sub(2)
    }
}

/// What a batch set up: its draw, the shaded vertices its primitives read,
/// their pieces in order, and the pixels they reach together.
pub(super) struct Batch {
    pub(super) draw: usize,
    pub(super) records: Vec<u32>,
    pub(super) setups: Vec<Setup>,
    pub(super) reach: Option<Area>,
}

impl Batch {
    /// An empty batch with room for the most a batch of a draw whose shaded
    /// vertices hold `record` words sets up.
    pub(super) fn with_room(record: usize) -> Option<Self> {
        let mut records = Vec::new();
        records
            .try_reserve_exact(BATCH_POSITIONS.saturating_mul(record))
            .ok()?;
        let mut setups = Vec::new();
        setups.try_reserve_exact(BATCH_PRIMITIVES as usize).ok()?;
        Some(Self {
            draw: 0,
            records,
            setups,
            reach: None,
        })
    }
}

/// A batch's vertices: the distinct vertices its positions name, in the
/// order first named, and each position's among them.
struct Gathered {
    vertices: ArrayVec<u32, BATCH_POSITIONS>,
    slots: ArrayVec<u8, BATCH_POSITIONS>,
}

/// Each position `planned` reads, its vertex found once, through a table of
/// those named so far.
fn gather(drawn: &Drawn<'_>, planned: &Planned) -> Option<Gathered> {
    let mut gathered = Gathered {
        vertices: ArrayVec::new(),
        slots: ArrayVec::new(),
    };
    let (start, end) = positions_read(drawn.render.topology, planned);
    let mut table = [u8::MAX; TABLE];
    let bits = TABLE.trailing_zeros();
    for position in (start..end).chain(fan_origin(drawn.render.topology)) {
        let vertex = drawn.vertex(planned.run + position);
        // A multiplicative hash's high bits, which every bit of the key moves.
        let mut at = (vertex.wrapping_mul(0x9e37_79b1) >> (32 - bits)) as usize;
        let slot = loop {
            let held = *table.get(at)?;
            if held == u8::MAX {
                let slot = u8::try_from(gathered.vertices.len()).ok()?;
                gathered.vertices.try_push(vertex).ok()?;
                *table.get_mut(at)? = slot;
                break slot;
            }
            if gathered.vertices.get(usize::from(held)) == Some(&vertex) {
                break held;
            }
            at = (at + 1) % TABLE;
        };
        gathered.slots.try_push(slot).ok()?;
    }
    Some(gathered)
}

/// The run's positions `planned`'s primitives read, as one span; a fan's
/// shared first corner apart.
fn positions_read(topology: Topology, planned: &Planned) -> (u32, u32) {
    let (first, last) = (planned.first, planned.first + planned.count);
    let (start, end) = match topology {
        Topology::Points => (first, last),
        Topology::Lines => (2 * first, 2 * last),
        Topology::LineStrip => (first, last + 1),
        Topology::Triangles => (3 * first, 3 * last),
        Topology::TriangleStrip => (first, last + 2),
        Topology::TriangleFan => (first + 1, last + 2),
    };
    (start, end.min(planned.length))
}

/// A fan's first corner, which every primitive of it reads.
fn fan_origin(topology: Topology) -> Option<u32> {
    (topology == Topology::TriangleFan).then_some(0)
}

/// The slot among `gathered`'s vertices of position `position` of the run.
fn slot_of(
    topology: Topology,
    planned: &Planned,
    gathered: &Gathered,
    position: u32,
) -> Option<u32> {
    let (start, end) = positions_read(topology, planned);
    let at = if fan_origin(topology) == Some(position) {
        (end - start) as usize
    } else {
        position.checked_sub(start)? as usize
    };
    gathered.slots.get(at).map(|&slot| u32::from(slot))
}

/// Shade, assemble and set up the batch `planned` of `drawn` into `batch`,
/// in `workspace`, counted on `meter`: `false` where the run stopped short or
/// found the program broken.
pub(super) fn run_batch(
    drawn: &Drawn<'_>,
    planned: &Planned,
    (batch, workspace): (&mut Batch, &mut Workspace),
    meter: &mut Meter<'_>,
) -> bool {
    batch.draw = planned.draw;
    batch.records.clear();
    batch.setups.clear();
    batch.reach = None;
    let Some(gathered) = gather(drawn, planned) else {
        return false;
    };
    if !shade_vertices(
        drawn,
        planned.instance,
        &gathered.vertices,
        (batch, workspace),
        meter,
    ) {
        return false;
    }
    let topology = drawn.render.topology;
    for i in planned.first..planned.first + planned.count {
        let mut records = [0u32; 3];
        let (taken, used) = corners(topology, i);
        for (record, &position) in records.iter_mut().zip(&taken).take(used) {
            let Some(slot) = slot_of(topology, planned, &gathered, position) else {
                return false;
            };
            *record = slot * drawn.render.record;
        }
        let made = match used {
            1 => set_up_point(drawn, batch, records[0]),
            2 => set_up_line(drawn, batch, [records[0], records[1]]),
            _ => set_up_triangle(drawn, batch, records),
        };
        if !made {
            return false;
        }
        meter.charge(u64::try_from(MAX_CLIP_CORNERS).unwrap_or(u64::MAX));
    }
    true
}

/// Shade `vertices` of `instance` a tile at a time, each vertex's outputs
/// laid out in the batch's records in turn.
fn shade_vertices(
    drawn: &Drawn<'_>,
    instance: u32,
    vertices: &[u32],
    (batch, workspace): (&mut Batch, &mut Workspace),
    meter: &mut Meter<'_>,
) -> bool {
    let render = drawn.render;
    let record = render.record as usize;
    let program = &render.vertex;
    if batch
        .records
        .try_reserve(vertices.len().saturating_mul(record))
        .is_err()
    {
        return false;
    }
    batch.records.resize(vertices.len() * record, 0);
    for (tile, chunk) in vertices.chunks(LANES).enumerate() {
        let Some(space) = workspace.tile(program) else {
            return false;
        };
        let place = Placement::batch(chunk.len(), drawn.vertex.subgroup_size);
        let first = tile * LANES;
        // Each lane's attribute, decoded as a port's first word is fed and
        // read by its later words.
        let mut decoded = [[0u32; 4]; LANES];
        let feed = |port: usize, word: u32, values: &mut Lanes| {
            let Some(carries) = program.inputs.get(port).map(|p| p.carries) else {
                return;
            };
            let lanes = values.iter_mut().zip(chunk);
            match carries {
                Carries::Fixed(Fixed::VertexIndex) => {
                    lanes.for_each(|(value, &vertex)| *value = u64::from(vertex));
                }
                Carries::Fixed(Fixed::InstanceIndex) => {
                    lanes.for_each(|(value, _)| *value = u64::from(instance));
                }
                Carries::Located { .. } => {
                    let Some(fetch) = render
                        .fetches
                        .binary_search_by_key(&port, |fetch| fetch.port as usize)
                        .ok()
                        .and_then(|at| render.fetches.get(at))
                    else {
                        return;
                    };
                    let per_instance = matches!(
                        render.buffers.get(fetch.buffer as usize),
                        Some((_, VertexStep::Instance))
                    );
                    for ((value, &vertex), texel) in lanes.zip(&mut decoded) {
                        if word == 0 {
                            *texel = drawn
                                .attribute(fetch, if per_instance { instance } else { vertex });
                        }
                        *value = u64::from(port_word(fetch, texel, word));
                    }
                }
                Carries::Fixed(_) => {}
            }
        };
        let starts = &render.starts;
        let records = &mut batch.records;
        let take = |port: usize, word: u32, values: &Lanes| {
            let Some(&start) = starts.get(port) else {
                return;
            };
            for (lane, &value) in values.iter().enumerate().take(chunk.len()) {
                let at = (first + lane) * record + start as usize + word as usize;
                if let Some(slot) = records.get_mut(at) {
                    *slot = crate::software::shader::ops::low32(value);
                }
            }
        };
        if shade((&drawn.vertex, &place), space, meter, feed, take).is_err() {
            return false;
        }
    }
    true
}

/// The clip-space position of the shaded vertex at `record`.
fn position(drawn: &Drawn<'_>, records: &[u32], record: u32) -> [f64; 4] {
    let Some(at) = drawn.render.position else {
        return [0.0; 4];
    };
    core::array::from_fn(|axis| {
        let word = records
            .get((record + at) as usize + axis)
            .copied()
            .unwrap_or(0);
        f64::from(f32::from_bits(word))
    })
}

/// The pixels a polygon with corners `polygon` may reach within the draw's
/// rectangle; `None` where it reaches none.
fn reach(drawn: &Drawn<'_>, polygon: &[Mapped]) -> Option<Area> {
    let unit = i64::from(MESH_SUB_UNITS);
    let pixel = |sub: i32| i64::from(sub).div_euclid(unit);
    let span = |pick: fn(&Mapped) -> i32| {
        polygon
            .iter()
            .fold(None, |held: Option<(i64, i64)>, corner| {
                let at = pixel(pick(corner));
                Some(held.map_or((at, at), |(low, high)| (low.min(at), high.max(at))))
            })
    };
    let fit = |(low, high): (i64, i64), (from, to): (u32, u32)| {
        let low = u32::try_from(low.max(i64::from(from))).ok()?;
        let high = u32::try_from((high + 1).min(i64::from(to))).ok()?;
        (low < high).then_some((low, high))
    };
    let rect = drawn.rect;
    let (x0, x1) = fit(span(|corner| corner.x)?, (rect.x0, rect.x1))?;
    let (y0, y1) = fit(span(|corner| corner.y)?, (rect.y0, rect.y1))?;
    Some(Area { x0, y0, x1, y1 })
}

/// File the primitive whose clip left `polygon`, its corners those of the
/// primitive at `records`, where it reaches any pixel the draw may touch.
fn file(
    drawn: &Drawn<'_>,
    batch: &mut Batch,
    (polygon, records): (ArrayVec<Mapped, MAX_CLIP_CORNERS>, [u32; 3]),
    (front, depth_offset, point): (bool, f64, Option<(f64, f64, f64)>),
) -> bool {
    let Some(area) = reach(drawn, &polygon) else {
        return true;
    };
    if batch.setups.try_reserve(1).is_err() {
        return false;
    }
    batch.reach = Some(batch.reach.map_or(area, |held| held.union(area)));
    batch.setups.push(Setup {
        polygon,
        corners: records,
        front,
        depth_offset,
        area,
        point,
    });
    true
}

/// Whether `primitive` faces the front under the draw's front face.
fn faces_front(drawn: &Drawn<'_>, primitive: &Primitive) -> bool {
    let clockwise = primitive.footprint().clockwise();
    match drawn.render.rasterization.front_face {
        FrontFace::Clockwise => clockwise,
        FrontFace::CounterClockwise => !clockwise,
    }
}

/// Whether a triangle facing as `front` says is culled.
const fn culled(cull: CullMode, front: bool) -> bool {
    match cull {
        CullMode::None => false,
        CullMode::Front => front,
        CullMode::Back => !front,
        CullMode::Both => true,
    }
}

fn set_up_triangle(drawn: &Drawn<'_>, batch: &mut Batch, records: [u32; 3]) -> bool {
    let at = records.map(|record| position(drawn, &batch.records, record));
    let mut polygon = ArrayVec::<Mapped, MAX_CLIP_CORNERS>::new();
    drawn.viewport.clip(at, &mut polygon);
    // Its facing and its depth's slope are one plane's, read from the first
    // piece with area.
    let first_piece = || {
        let first = *polygon.first()?;
        polygon
            .get(1..)?
            .windows(2)
            .find_map(|pair| Primitive::new([first, pair[0], pair[1]]))
    };
    let Some(piece) = first_piece() else {
        return true;
    };
    let raster = &drawn.render.rasterization;
    let front = faces_front(drawn, &piece);
    if culled(raster.cull, front) {
        return true;
    }
    let depth_offset = match (raster.depth_bias, drawn.depth_format) {
        (Some(bias), Some(format)) => {
            let deepest = polygon.iter().fold(0.0, |most, corner| {
                mathf::fmax(most, mathf::fabs(corner.depth))
            });
            bias.offset(format, piece.slope(), deepest)
        }
        _ => 0.0,
    };
    file(
        drawn,
        batch,
        (polygon, records),
        (front, depth_offset, None),
    )
}

/// A corner of an expanded point or line: `end` moved by `by` sub-units.
fn moved(end: &Mapped, by: (f64, f64)) -> Mapped {
    Mapped {
        x: mathf::round_i32(f64::from(end.x) + by.0),
        y: mathf::round_i32(f64::from(end.y) + by.1),
        ..*end
    }
}

/// The square or rectangle `corners`, in order round it, as a fan.
fn square(corners: &[Mapped; 4]) -> Option<ArrayVec<Mapped, MAX_CLIP_CORNERS>> {
    ArrayVec::try_from(&corners[..]).ok()
}

fn set_up_point(drawn: &Drawn<'_>, batch: &mut Batch, record: u32) -> bool {
    let at = position(drawn, &batch.records, record);
    let Some([centre, _]) = drawn.viewport.clip_line(at, at) else {
        return true;
    };
    let size = drawn.render.point_size.map_or(1.0, |offset| {
        let word = batch
            .records
            .get((record + offset) as usize)
            .copied()
            .unwrap_or(0);
        f64::from(f32::from_bits(word))
    });
    let largest = drawn.render.largest_point;
    let size = if size.is_nan() || size < 1.0 {
        1.0
    } else if size > largest {
        largest
    } else {
        size
    };
    let sub = f64::from(MESH_SUB_UNITS);
    let half = size * sub / 2.0;
    let centre = Mapped {
        weights: [1.0, 0.0, 0.0],
        linear: [1.0, 0.0, 0.0],
        ..centre
    };
    let corner = |dx: f64, dy: f64| moved(&centre, (dx * half, dy * half));
    let Some(polygon) = square(&[
        corner(-1.0, -1.0),
        corner(1.0, -1.0),
        corner(1.0, 1.0),
        corner(-1.0, 1.0),
    ]) else {
        return false;
    };
    let left_top = polygon.first().map_or((0.0, 0.0), |corner| {
        (f64::from(corner.x) / sub, f64::from(corner.y) / sub)
    });
    let square_at = (left_top.0, left_top.1, size);
    file(
        drawn,
        batch,
        (polygon, [record; 3]),
        (true, 0.0, Some(square_at)),
    )
}

fn set_up_line(drawn: &Drawn<'_>, batch: &mut Batch, records: [u32; 2]) -> bool {
    let ends = records.map(|record| position(drawn, &batch.records, record));
    let Some([a, b]) = drawn.viewport.clip_line(ends[0], ends[1]) else {
        return true;
    };
    let along = (
        f64::from(b.x) - f64::from(a.x),
        f64::from(b.y) - f64::from(a.y),
    );
    let length = mathf::hypot(along.0, along.1);
    if length.is_nan() || length <= 0.0 {
        return true;
    }
    let half = f64::from(drawn.render.rasterization.line_width) * f64::from(MESH_SUB_UNITS) / 2.0;
    let normal = (-along.1 / length * half, along.0 / length * half);
    let flip = (-normal.0, -normal.1);
    let Some(polygon) = square(&[
        moved(&a, normal),
        moved(&b, normal),
        moved(&b, flip),
        moved(&a, flip),
    ]) else {
        return false;
    };
    file(
        drawn,
        batch,
        (polygon, [records[0], records[1], records[1]]),
        (true, 0.0, None),
    )
}
