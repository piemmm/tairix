//! A render pass on the software device: its memories locked once in the
//! order every holder shares; its draws' primitives shaded, clipped and set
//! up a bounded batch at a time across the process's runner; and the bins
//! its attachments are cut into drawn in parallel, in place — cleared as the
//! pass says, its primitives in order, and resolved — a flush of batches at
//! a time, so a pass of any size draws whole in bounded memory.

mod back;
mod front;

use alloc::boxed::Box;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use tairix_inline::ArrayVec;
use tairix_raster::clip::Viewport;
use tairix_raster::{DepthFormat, Lattice, MAX_SAMPLES, MESH_SUB_UNITS};
use tairix_rt::sync::Mutex;
use tairix_util::mathf;

use self::back::{Room, Shaded};
use self::front::{Batch, Planned, Planner, FLUSH_BATCHES};
use super::dispatch::{counted_on, place_sets, resolve, rows_of, with_locked, Locked, Workspace};
use super::shader::exec::{Env, Resolved};
use super::texel::{encode, plane_bits, put_raw, raw_at, Plane, Srgb};
use super::{Meter, Outcome};
use crate::binding::BindSetState;
use crate::device::Shared;
use crate::format::Format;
use crate::image::ClearValue;
use crate::memory::Memory;
use crate::pass::{DrawWork, IndexFormat, Load, PassWork};
use crate::render::Render;
use crate::resource::ViewState;
use crate::MAX_PASS_ATTACHMENTS;

/// The most bytes a bin's samples take, every attachment's together, unless
/// a block's take more: what keeps a bin's working set in a core's cache.
const BIN_BYTES: u64 = 1 << 18;

/// The widest a bin is, in pixels; the narrowest is a block's.
const MAX_BIN_SIDE: u32 = 128;

/// A rectangle of pixels, `x0..x1` across and `y0..y1` down.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub(super) struct Area {
    pub(super) x0: u32,
    pub(super) y0: u32,
    pub(super) x1: u32,
    pub(super) y1: u32,
}

impl Area {
    /// Whether it and `other` share a pixel.
    pub(super) const fn meets(self, other: Self) -> bool {
        self.x0 < other.x1 && other.x0 < self.x1 && self.y0 < other.y1 && other.y0 < self.y1
    }

    /// The pixels it shares with `other`, where there are any.
    pub(super) fn within(self, other: Self) -> Option<Self> {
        let area = Self {
            x0: self.x0.max(other.x0),
            y0: self.y0.max(other.y0),
            x1: self.x1.min(other.x1),
            y1: self.y1.min(other.y1),
        };
        (area.x0 < area.x1 && area.y0 < area.y1).then_some(area)
    }

    /// The least rectangle holding it and `other`.
    pub(super) fn union(self, other: Self) -> Self {
        Self {
            x0: self.x0.min(other.x0),
            y0: self.y0.min(other.y0),
            x1: self.x1.max(other.x1),
            y1: self.y1.max(other.y1),
        }
    }
}

/// An attachment as a pass renders to it, in place: its level and layer's
/// memory, laid out by row and texel, a texel's samples together; its
/// format; and what the pass's first flush clears it to and its last
/// resolves it into.
pub(super) struct Held<'v> {
    words: &'v [AtomicU32],
    base: u64,
    row: u64,
    texel: u64,
    samples: u64,
    pub(super) format: Format,
    /// The bits of a texel its depth and its stencil lie in.
    pub(super) depth_bits: [u32; 4],
    pub(super) stencil_bits: [u32; 4],
    clear: Option<Clear>,
    resolve: Option<Into<'v>>,
}

/// What a pass's clear writes into a texel, and the bits it writes.
type Clear = ([u32; 4], [u32; 4]);

/// A view a target's samples resolve into: its level and layer's memory.
struct Into<'v> {
    words: &'v [AtomicU32],
    base: u64,
    row: u64,
    texel: u64,
}

impl Held<'_> {
    /// The byte sample `sample` of pixel `(x, y)` starts at: a texel's
    /// samples lie together.
    fn at(&self, (x, y): (u32, u32), sample: usize) -> u64 {
        self.base
            + u64::from(y) * self.row
            + (u64::from(x) * self.samples + sample as u64) * self.texel
    }

    /// Sample `sample` of pixel `pixel`, as its format holds it.
    pub(super) fn read(&self, pixel: (u32, u32), sample: usize) -> [u32; 4] {
        raw_at(self.words, self.at(pixel, sample), self.texel)
    }

    /// Hold `raw` as sample `sample` of pixel `pixel`.
    pub(super) fn write(&self, pixel: (u32, u32), sample: usize, raw: [u32; 4]) {
        put_raw(self.words, self.at(pixel, sample), self.texel, raw);
    }
}

/// A pass's attachments as its bins render to them. Bins meet at multiples
/// of their side, itself a multiple of a block's eight pixels, where every
/// attachment's texels start on a word as each row does, so bins drawn at
/// once never write one word.
pub(super) struct Frame<'v> {
    pub(super) targets: ArrayVec<Held<'v>, MAX_PASS_ATTACHMENTS>,
    pub(super) depth: Option<Held<'v>>,
    pub(super) samples: u32,
    area: Area,
    side: u32,
    pub(super) srgb: &'v Srgb,
}

impl<'v> Frame<'v> {
    /// The attachments of `pass` in `locked`'s views.
    fn of(pass: &PassWork, locked: &'v Locked<'_>, srgb: &'v Srgb) -> Option<Self> {
        let mut targets = ArrayVec::new();
        let samples = pass.samples;
        for target in &pass.colour {
            let clear = match target.load {
                Load::Clear(value) => Some((
                    clear_colour(target.view.desc.format, value, srgb),
                    [u32::MAX; 4],
                )),
                Load::Keep => None,
            };
            let resolve = match &target.resolve {
                Some(view) => Some(into(view, locked)?),
                None => None,
            };
            targets
                .try_push(held(&target.view, locked, (clear, resolve))?)
                .ok()?;
        }
        let depth = match &pass.depth_stencil {
            Some(depth) => {
                let format = depth.view.desc.format;
                let (depth_bits, stencil_bits) = (
                    plane_bits(format, Plane::Depth),
                    plane_bits(format, Plane::Stencil),
                );
                let mut raw = [0u32; 4];
                let mut written = [0u32; 4];
                if let Load::Clear(ClearValue::DepthStencil { depth: value, .. }) = depth.depth {
                    let stored = encode(format, Plane::Depth, [value.to_bits(), 0, 0, 0], srgb);
                    merge(&mut raw, stored, depth_bits);
                    written = or(written, depth_bits);
                }
                if let Load::Clear(ClearValue::DepthStencil { stencil, .. }) = depth.stencil {
                    let stored =
                        encode(format, Plane::Stencil, [u32::from(stencil), 0, 0, 0], srgb);
                    merge(&mut raw, stored, stencil_bits);
                    written = or(written, stencil_bits);
                }
                let clear = (written != [0; 4]).then_some((raw, written));
                Some(held(&depth.view, locked, (clear, None))?)
            }
            None => None,
        };
        let pixel_bytes = targets.iter().chain(&depth).fold(0u64, |total, held| {
            total.saturating_add(held.texel * held.samples)
        });
        let mut side = MAX_BIN_SIDE;
        while side > tairix_raster::BLOCK
            && u64::from(side * side).saturating_mul(pixel_bytes) > BIN_BYTES
        {
            side /= 2;
        }
        let area = pass.area;
        Some(Self {
            targets,
            depth,
            samples,
            area: Area {
                x0: area.x,
                y0: area.y,
                x1: area.x + area.width,
                y1: area.y + area.height,
            },
            side,
            srgb,
        })
    }

    /// The columns and rows of bins the pass's area is cut into, each bin
    /// a square of the grid its side rules from the framebuffer's origin.
    fn grid(&self) -> (u32, u32) {
        let side = self.side;
        (
            (self.area.x1 - 1) / side - self.area.x0 / side + 1,
            (self.area.y1 - 1) / side - self.area.y0 / side + 1,
        )
    }

    /// How many bins the pass's area is cut into.
    fn bin_count(&self) -> usize {
        let (across, down) = self.grid();
        across as usize * down as usize
    }

    /// Bin `unit`, counted row by row: its square's pixels within the area.
    fn bin(&self, unit: usize) -> Option<Area> {
        let (across, _) = self.grid();
        let (row, column) = (
            u32::try_from(unit / across as usize).ok()?,
            u32::try_from(unit % across as usize).ok()?,
        );
        let side = self.side;
        let (x, y) = (self.area.x0 / side + column, self.area.y0 / side + row);
        Area {
            x0: x * side,
            y0: y * side,
            x1: (x + 1) * side,
            y1: (y + 1) * side,
        }
        .within(self.area)
    }

    pub(super) const fn lattice(&self) -> Lattice {
        if self.samples == 4 {
            Lattice::Four
        } else {
            Lattice::Centre
        }
    }

    /// Where sample `sample` lies within its pixel.
    pub(super) fn sample_position(&self, sample: usize) -> (f64, f64) {
        let sub = f64::from(MESH_SUB_UNITS);
        self.lattice()
            .samples()
            .get(sample)
            .map_or((0.5, 0.5), |&(x, y)| {
                (f64::from(x) / sub, f64::from(y) / sub)
            })
    }

    fn attachments(&self) -> impl Iterator<Item = &Held<'v>> {
        self.targets.iter().chain(&self.depth)
    }

    /// Clear `bin`'s samples of every attachment the pass clears.
    pub(super) fn clear(&self, bin: Area, meter: &mut Meter<'_>) -> bool {
        let cleared = self
            .attachments()
            .filter(|held| held.clear.is_some())
            .count() as u64;
        if cleared == 0 {
            return true;
        }
        for y in bin.y0..bin.y1 {
            if !meter.proceed(0) {
                return false;
            }
            for held in self.attachments() {
                let Some((value, written)) = held.clear else {
                    continue;
                };
                for x in bin.x0..bin.x1 {
                    for sample in 0..self.samples as usize {
                        let raw = if written == [u32::MAX; 4] {
                            value
                        } else {
                            let mut raw = held.read((x, y), sample);
                            merge(&mut raw, value, written);
                            raw
                        };
                        held.write((x, y), sample, raw);
                    }
                }
            }
            meter.charge(u64::from(bin.x1 - bin.x0) * cleared * u64::from(self.samples));
        }
        true
    }

    /// Resolve `bin`'s samples of each target that resolves into its view.
    pub(super) fn resolve(&self, bin: Area, meter: &mut Meter<'_>) -> bool {
        let resolved = self
            .targets
            .iter()
            .filter(|held| held.resolve.is_some())
            .count() as u64;
        if resolved == 0 {
            return true;
        }
        for y in bin.y0..bin.y1 {
            if !meter.proceed(0) {
                return false;
            }
            for held in &self.targets {
                let Some(into) = &held.resolve else {
                    continue;
                };
                for x in bin.x0..bin.x1 {
                    let byte = into.base + u64::from(y) * into.row + u64::from(x) * into.texel;
                    put_raw(into.words, byte, into.texel, self.resolved(held, (x, y)));
                }
            }
            meter.charge(u64::from(bin.x1 - bin.x0) * resolved * u64::from(self.samples));
        }
        true
    }

    /// Pixel `pixel`'s samples of `held` averaged, in linear light for an
    /// sRGB format.
    fn resolved(&self, held: &Held<'_>, pixel: (u32, u32)) -> [u32; 4] {
        let mut sum = [0.0f64; 4];
        for sample in 0..self.samples as usize {
            let value = super::texel::decode(
                held.format,
                Plane::Colour,
                held.read(pixel, sample),
                self.srgb,
            );
            for (total, &channel) in sum.iter_mut().zip(&value) {
                *total += f64::from(f32::from_bits(channel));
            }
        }
        let count = f64::from(self.samples);
        let average = sum.map(|total| super::shader::ops::single_bits(total / count));
        encode(held.format, Plane::Colour, average, self.srgb)
    }
}

/// `stored`'s bits where `bits` names them, and `raw`'s elsewhere.
pub(super) fn merge(raw: &mut [u32; 4], stored: [u32; 4], bits: [u32; 4]) {
    for ((word, new), keep) in raw.iter_mut().zip(stored).zip(bits) {
        *word = (*word & !keep) | (new & keep);
    }
}

fn or(a: [u32; 4], b: [u32; 4]) -> [u32; 4] {
    core::array::from_fn(|word| a[word] | b[word])
}

/// The texel of `format` a colour clear `value` writes.
fn clear_colour(format: Format, value: ClearValue, srgb: &Srgb) -> [u32; 4] {
    let words = match value {
        ClearValue::Float(channels) => channels.map(f32::to_bits),
        ClearValue::Sint(channels) => channels.map(i32::cast_unsigned),
        ClearValue::Uint(channels) => channels,
        ClearValue::DepthStencil { .. } => [0; 4],
    };
    encode(format, Plane::Colour, words, srgb)
}

/// `view`'s one level and layer as a pass renders to it.
fn held<'v>(
    view: &ViewState,
    locked: &'v Locked<'_>,
    (clear, resolve): (Option<Clear>, Option<Into<'v>>),
) -> Option<Held<'v>> {
    let image = &view.image;
    let level = image
        .layout
        .levels
        .get(view.desc.range.base_level as usize)?;
    let format = view.desc.format;
    Some(Held {
        words: locked.view(&image.memory)?,
        base: image.offset + level.base + u64::from(view.desc.range.base_layer) * level.layer,
        row: level.row,
        texel: image.layout.texel,
        samples: image.layout.samples,
        format,
        depth_bits: plane_bits(format, Plane::Depth),
        stencil_bits: plane_bits(format, Plane::Stencil),
        clear,
        resolve,
    })
}

/// The one-sample view `view` as a resolve writes it.
fn into<'v>(view: &ViewState, locked: &'v Locked<'_>) -> Option<Into<'v>> {
    let image = &view.image;
    let level = image
        .layout
        .levels
        .get(view.desc.range.base_level as usize)?;
    Some(Into {
        words: locked.view(&image.memory)?,
        base: image.offset + level.base + u64::from(view.desc.range.base_layer) * level.layer,
        row: level.row,
        texel: image.layout.texel,
    })
}

/// A draw as its pass runs it: its checked work and pipeline, each stage's
/// bindings and push constants, the memory its vertex buffers and indices
/// lie in, its viewport, the pixels each of a pixel's samples may be covered
/// in — within the viewport, the scissor and the pass's area — and the
/// pixels they make up together, and its depth's format and range.
pub(super) struct Drawn<'d> {
    pub(super) work: &'d DrawWork,
    pub(super) render: &'d Render,
    pub(super) vertex: Env<'d>,
    pub(super) fragment: Option<Env<'d>>,
    pub(super) buffers: Vec<Option<(&'d [AtomicU32], u64, u64)>>,
    pub(super) index: Option<(&'d [AtomicU32], u64, u64, IndexFormat)>,
    pub(super) viewport: Viewport,
    pub(super) inside: [Option<Area>; MAX_SAMPLES],
    pub(super) rect: Area,
    pub(super) depth_format: Option<DepthFormat>,
    pub(super) depth_range: (f64, f64),
    pub(super) srgb: &'d Srgb,
}

/// The depth format the software device keeps a depth-stencil format's
/// depth in.
const fn depth_format_of(format: Format) -> DepthFormat {
    match format {
        Format::Depth16Unorm => DepthFormat::Unorm16,
        _ => DepthFormat::Float32,
    }
}

/// One participant's slot: its room for bins and batches, and, where it did
/// any work, what it left uncounted and whether it read the clock.
struct Slot<'w> {
    room: Room<'w>,
    left: Option<(u64, bool)>,
}

/// Run `pass` in `workspaces`, its work counted by the job's `meter`.
pub(crate) fn run(
    pass: &PassWork,
    meter: &mut Meter<'_>,
    device: &Shared,
    workspaces: &mut [Workspace],
) -> Outcome {
    let memories = match pass_memories(pass, meter) {
        Ok(memories) => memories,
        Err(outcome) => return outcome,
    };
    let mut lists = [memories.as_slice()];
    let ran = with_locked(&mut lists, meter, |locked, meter| {
        let frame = Frame::of(pass, locked, &device.srgb).ok_or(Outcome::Fault)?;
        // The descriptors of each run of draws binding alike, resolved through
        // their sets once, and the run each draw is of.
        let starts = (0..pass.draws.len())
            .filter(|&at| starts_run(&pass.draws, at))
            .count();
        let mut tables = Vec::new();
        tables
            .try_reserve_exact(starts)
            .map_err(|_| Outcome::OutOfMemory)?;
        let mut runs = Vec::new();
        runs.try_reserve_exact(pass.draws.len())
            .map_err(|_| Outcome::OutOfMemory)?;
        for (at, draw) in pass.draws.iter().enumerate() {
            if starts_run(&pass.draws, at) {
                let render = &draw.pipeline.stages;
                let mapping = place_sets(&draw.sets, locked.memories, meter)?;
                let mut resolved =
                    |program| resolve(program, &draw.sets, (locked.views, &mapping), meter);
                let vertex = resolved(&render.vertex)?;
                let fragment = match &render.fragment {
                    Some(shading) => Some(resolved(&shading.program)?),
                    None => None,
                };
                tables.push((at, vertex, fragment));
            }
            runs.push(tables.len().saturating_sub(1));
        }
        let mut rows = Vec::new();
        rows.try_reserve_exact(tables.len())
            .map_err(|_| Outcome::OutOfMemory)?;
        for (first, vertex, fragment) in &tables {
            let render = &pass
                .draws
                .get(*first)
                .ok_or(Outcome::Fault)?
                .pipeline
                .stages;
            let fragment = match (&render.fragment, fragment) {
                (Some(shading), Some(table)) => Some(rows_of(&shading.program, table)?),
                _ => None,
            };
            rows.push((rows_of(&render.vertex, vertex)?, fragment));
        }
        let mut draws = Vec::new();
        draws
            .try_reserve_exact(pass.draws.len())
            .map_err(|_| Outcome::OutOfMemory)?;
        for (work, &run) in pass.draws.iter().zip(&runs) {
            let (vertex, fragment) = rows.get(run).ok_or(Outcome::Fault)?;
            draws.push(drawn(
                device,
                &frame,
                (work, locked),
                (vertex, fragment.as_deref()),
            )?);
        }
        draw_pass(&frame, &draws, meter, device, workspaces)
    });
    match ran {
        Ok(outcome) | Err(outcome) => outcome,
    }
}

/// Whether draw `at` of `draws` binds otherwise than the one before it — its
/// pipeline or a set — and so starts a run of draws whose descriptors resolve
/// alike.
fn starts_run(draws: &[DrawWork], at: usize) -> bool {
    let (Some(draw), Some(before)) = (draws.get(at), at.checked_sub(1).and_then(|b| draws.get(b)))
    else {
        return true;
    };
    !Arc::ptr_eq(&draw.pipeline, &before.pipeline)
        || draw.sets.len() != before.sets.len()
        || !draw
            .sets
            .iter()
            .zip(&before.sets)
            .all(|(a, b)| Arc::ptr_eq(a, b))
}

/// Every memory the pass reaches, in address order, each once: its
/// attachments', its draws' vertex and index buffers', and every memory of
/// each distinct set its draws bind, gathered and sorted once, so a pass of
/// many draws costs its memories' logarithm rather than its sets'.
fn pass_memories(pass: &PassWork, meter: &mut Meter<'_>) -> Result<Vec<Arc<Memory>>, Outcome> {
    let exhausted = |_| Outcome::OutOfMemory;
    let mut sets: Vec<&Arc<BindSetState>> = Vec::new();
    let bound = pass.draws.iter().map(|draw| draw.sets.len()).sum::<usize>();
    sets.try_reserve_exact(bound).map_err(exhausted)?;
    sets.extend(pass.draws.iter().flat_map(|draw| &draw.sets));
    meter.charge(bound as u64);
    sets.sort_unstable_by_key(|set| Arc::as_ptr(set));
    sets.dedup_by(|a, b| Arc::ptr_eq(a, b));
    let others = pass.colour.len() * 2
        + 1
        + pass
            .draws
            .iter()
            .map(|draw| draw.vertex_buffers.len() + 1)
            .sum::<usize>();
    let count = sets.iter().fold(others, |total, set| {
        total.saturating_add(set.memories.len())
    });
    let mut memories: Vec<Arc<Memory>> = Vec::new();
    memories.try_reserve_exact(count).map_err(exhausted)?;
    for target in &pass.colour {
        memories.push(Arc::clone(&target.view.image.memory));
        if let Some(view) = &target.resolve {
            memories.push(Arc::clone(&view.image.memory));
        }
    }
    if let Some(depth) = &pass.depth_stencil {
        memories.push(Arc::clone(&depth.view.image.memory));
    }
    for draw in &pass.draws {
        memories.extend(
            draw.vertex_buffers
                .iter()
                .map(|source| Arc::clone(&source.memory)),
        );
        if let Some((source, _)) = &draw.index {
            memories.push(Arc::clone(&source.memory));
        }
    }
    for set in &sets {
        if !meter.proceed(set.memories.len() as u64) {
            return Err(Outcome::Overran);
        }
        memories.extend(set.memories.iter().cloned());
    }
    memories.sort_unstable_by_key(Arc::as_ptr);
    memories.dedup_by(|a, b| Arc::ptr_eq(a, b));
    Ok(memories)
}

/// `work` as its pass runs it, its stages' bindings `rows`.
fn drawn<'d>(
    device: &'d Shared,
    frame: &Frame<'_>,
    (work, locked): (&'d DrawWork, &'d Locked<'_>),
    (vertex, fragment): (&'d [&'d [Resolved<'d>]], Option<&'d [&'d [Resolved<'d>]]>),
) -> Result<Drawn<'d>, Outcome> {
    let render = &work.pipeline.stages;
    let limits = &device.info.limits;
    let push = work.push.as_deref().map_or(&[][..], Vec::as_slice);
    let env = |program, bindings| Env {
        program,
        bindings,
        push,
        srgb: &device.srgb,
        subgroup_size: limits.subgroup_size,
        groups: [1, 1, 1],
    };
    let mut buffers = Vec::new();
    buffers
        .try_reserve_exact(work.vertex_buffers.len())
        .map_err(|_| Outcome::OutOfMemory)?;
    for source in &work.vertex_buffers {
        buffers.push(
            locked
                .view(&source.memory)
                .map(|words| (words, source.start, source.size)),
        );
    }
    let index = work.index.as_ref().and_then(|(source, format)| {
        Some((
            locked.view(&source.memory)?,
            source.start,
            source.size,
            *format,
        ))
    });
    let port = work.viewport;
    let raster = &render.rasterization;
    let viewport = Viewport::new(
        (f64::from(port.x), f64::from(port.y)),
        (f64::from(port.width), f64::from(port.height)),
        (f64::from(port.min_depth), f64::from(port.max_depth)),
        raster.depth_clamp,
    )
    .ok_or(Outcome::Fault)?;
    let (inside, rect) = sample_areas(frame, &port, &work.scissor);
    let depth_format = render
        .depth_stencil
        .map(|state| depth_format_of(state.format));
    let (near, far) = (f64::from(port.min_depth), f64::from(port.max_depth));
    let depth_range = if raster.depth_clamp {
        (mathf::fmin(near, far), mathf::fmax(near, far))
    } else if depth_format == Some(DepthFormat::Unorm16) {
        (0.0, 1.0)
    } else {
        (f64::NEG_INFINITY, f64::INFINITY)
    };
    Ok(Drawn {
        work,
        render,
        vertex: env(&render.vertex, vertex),
        fragment: render
            .fragment
            .as_ref()
            .zip(fragment)
            .map(|(shading, rows)| env(&shading.program, rows)),
        buffers,
        index,
        viewport,
        inside,
        rect,
        depth_format,
        depth_range,
        srgb: &device.srgb,
    })
}

/// The pixels each sample of `frame`'s may be covered in by a draw through
/// `port` and `scissor` — within the viewport, held as a clipped edge would
/// hold it, the scissor and the pass's area — and the pixels they make up.
/// A primitive is clipped to a guard band about its viewport, so its samples
/// are held to the viewport here.
fn sample_areas(
    frame: &Frame<'_>,
    port: &crate::pass::Viewport,
    scissor: &crate::pass::Rect,
) -> ([Option<Area>; MAX_SAMPLES], Area) {
    let fenced = Area {
        x0: scissor.x,
        y0: scissor.y,
        x1: scissor.x.saturating_add(scissor.width),
        y1: scissor.y.saturating_add(scissor.height),
    }
    .within(frame.area);
    let (top, tall) = {
        let (y, height) = (f64::from(port.y), f64::from(port.height));
        if height < 0.0 {
            (y + height, -height)
        } else {
            (y, height)
        }
    };
    let inside: [Option<Area>; MAX_SAMPLES] = core::array::from_fn(|sample| {
        let (dx, dy) = frame.sample_position(sample);
        let (x0, x1) = sample_span(f64::from(port.x), f64::from(port.width), dx)?;
        let (y0, y1) = sample_span(top, tall, dy)?;
        Area { x0, y0, x1, y1 }.within(fenced?)
    });
    let rect = inside
        .iter()
        .take(frame.samples as usize)
        .flatten()
        .copied()
        .reduce(Area::union)
        .unwrap_or(Area {
            x0: 0,
            y0: 0,
            x1: 0,
            y1: 0,
        });
    (inside, rect)
}

/// The pixels whose sample `offset` into each lies on or past `start` and
/// short of `start + length`, a primitive's left and top edges kept and its
/// right and bottom ones not: `None` where there are none.
fn sample_span(start: f64, length: f64, offset: f64) -> Option<(u32, u32)> {
    let edge = |at: f64| u32::try_from(mathf::round_i32(mathf::ceil(at - offset)).max(0)).ok();
    let (low, high) = (edge(start)?, edge(start + length)?);
    (low < high).then_some((low, high))
}

/// Draw `draws` into `frame`: a flush of batches at a time set up across the
/// runner, then every bin they reach drawn across it; the first flush clears
/// every bin where the pass clears, and the last resolves every bin where it
/// resolves.
fn draw_pass(
    frame: &Frame<'_>,
    draws: &[Drawn<'_>],
    meter: &mut Meter<'_>,
    device: &Shared,
    workspaces: &mut [Workspace],
) -> Result<Outcome, Outcome> {
    let record = draws
        .iter()
        .map(|drawn| drawn.render.record as usize)
        .max()
        .unwrap_or(0);
    let mut batches: Vec<Batch> = Vec::new();
    batches
        .try_reserve_exact(FLUSH_BATCHES)
        .map_err(|_| Outcome::OutOfMemory)?;
    for _ in 0..FLUSH_BATCHES {
        batches.push(Batch::with_room(record).ok_or(Outcome::OutOfMemory)?);
    }
    // A pass of no draws shades nothing, so its job is lent no workspace:
    // its participants only clear and resolve.
    let width = device.runner.width().max(1);
    let width = if draws.is_empty() {
        width
    } else {
        width.min(workspaces.len())
    };
    if width == 0 {
        return Err(Outcome::Fault);
    }
    let mut slots: Vec<Mutex<Slot<'_>>> = Vec::new();
    slots
        .try_reserve_exact(width)
        .map_err(|_| Outcome::OutOfMemory)?;
    let mut lent = workspaces.iter_mut();
    for _ in 0..width {
        let mut workspace = lent.next();
        if let Some(workspace) = &mut workspace {
            if !workspace.clear(meter) {
                return Err(Outcome::Overran);
            }
        }
        let shaded = Box::try_new(Shaded::new()).map_err(|_| Outcome::OutOfMemory)?;
        slots.push(Mutex::new(Slot {
            room: Room { workspace, shaded },
            left: None,
        }));
    }
    let clears = frame.attachments().any(|held| held.clear.is_some());
    let resolves = frame.targets.iter().any(|held| held.resolve.is_some());
    let mut cutting = Planner::new();
    let mut planned: Vec<Planned> = Vec::new();
    planned
        .try_reserve_exact(FLUSH_BATCHES)
        .map_err(|_| Outcome::OutOfMemory)?;
    let mut first = true;
    loop {
        if !cutting.plan(draws, &mut planned, meter) {
            return Err(Outcome::Overran);
        }
        let last = cutting.done(draws.len());
        let made = batches.get_mut(..planned.len()).ok_or(Outcome::Fault)?;
        set_up(device, meter, &slots, (draws, &planned, made))?;
        let made = batches.get(..planned.len()).ok_or(Outcome::Fault)?;
        let everything = (first && clears) || (last && resolves);
        draw_bins(
            device,
            meter,
            &slots,
            (frame, draws, made),
            (first, last, everything),
        )?;
        first = false;
        if last {
            break;
        }
    }
    Ok(Outcome::Done)
}

/// Run `work` as up to one piece per slot on the device's runner, each piece
/// claiming units of `total` from a shared cursor, its meter counting on
/// from the job's and stopped with it.
fn spread<'w>(
    device: &Shared,
    meter: &mut Meter<'_>,
    slots: &[Mutex<Slot<'w>>],
    total: usize,
    work: &(dyn Fn(usize, &mut Room<'w>, &mut Meter<'_>) -> bool + Sync),
) -> Result<(), Outcome> {
    if total == 0 {
        return Ok(());
    }
    let fault = AtomicBool::new(false);
    let (stop, deadline) = (meter.stop, meter.deadline);
    let counted = meter.unchecked;
    let cursor = AtomicUsize::new(0);
    let pieces = slots.len().min(total);
    device.runner.run(pieces, &|piece| {
        let Some(slot) = slots.get(piece) else {
            fault.store(true, Ordering::Relaxed);
            stop.store(true, Ordering::Relaxed);
            return;
        };
        let mut held = slot.lock();
        let mut meter = Meter {
            stop,
            clock: &*device.clock,
            deadline,
            unchecked: counted,
            read: false,
        };
        let mut claimed = false;
        loop {
            let unit = cursor.fetch_add(1, Ordering::Relaxed);
            if unit >= total || stop.load(Ordering::Relaxed) {
                break;
            }
            claimed = true;
            if !work(unit, &mut held.room, &mut meter) {
                if !stop.load(Ordering::Relaxed) {
                    fault.store(true, Ordering::Relaxed);
                }
                stop.store(true, Ordering::Relaxed);
                break;
            }
        }
        held.left = claimed.then_some((meter.unchecked, meter.read));
    });
    let left = slots.iter().filter_map(|slot| slot.lock().left.take());
    let (unchecked, read) = counted_on(counted, left);
    meter.unchecked = unchecked;
    meter.read |= read;
    if fault.load(Ordering::Relaxed) {
        Err(Outcome::Fault)
    } else if stop.load(Ordering::Relaxed) {
        Err(Outcome::Overran)
    } else {
        Ok(())
    }
}

/// Set up each batch of `planned` into the batch of `made` at its place,
/// across the runner.
fn set_up(
    device: &Shared,
    meter: &mut Meter<'_>,
    slots: &[Mutex<Slot<'_>>],
    (bound, planned, made): (&[Drawn<'_>], &[Planned], &mut [Batch]),
) -> Result<(), Outcome> {
    let cells: Vec<Mutex<&mut Batch>> = {
        let mut cells = Vec::new();
        cells
            .try_reserve_exact(made.len())
            .map_err(|_| Outcome::OutOfMemory)?;
        cells.extend(made.iter_mut().map(Mutex::new));
        cells
    };
    spread(device, meter, slots, planned.len(), &|unit, room, meter| {
        let (Some(plan), Some(cell)) = (planned.get(unit), cells.get(unit)) else {
            return false;
        };
        let Some(drawn) = bound.get(plan.draw) else {
            return false;
        };
        let mut batch = cell.lock();
        let Some(workspace) = room.workspace.as_deref_mut() else {
            return false;
        };
        front::run_batch(drawn, plan, (&mut batch, workspace), meter)
    })
}

/// Draw each of `frame`'s bins a batch of `made` reaches — or every one,
/// where `everything` — across the runner.
fn draw_bins(
    device: &Shared,
    meter: &mut Meter<'_>,
    slots: &[Mutex<Slot<'_>>],
    (frame, draws, made): (&Frame<'_>, &[Drawn<'_>], &[Batch]),
    (first, last, everything): (bool, bool, bool),
) -> Result<(), Outcome> {
    spread(
        device,
        meter,
        slots,
        frame.bin_count(),
        &|unit, room, meter| {
            let Some(bin) = frame.bin(unit) else {
                return false;
            };
            meter.charge(made.len() as u64);
            let reached = everything
                || made
                    .iter()
                    .any(|batch| batch.reach.is_some_and(|reach| reach.meets(bin)));
            !reached || back::draw_bin((frame, draws, made), (bin, first, last), room, meter)
        },
    )
}

/// The host bytes a run of `pass` makes on a device of `participants`, made
/// as it starts and let go as it ends: its sets and memories gathered, its
/// lock and view tables, each run of draws binding alike's descriptor tables,
/// each draw's own, a flush's batches, and each participant's room.
pub(crate) fn run_bytes(pass: &PassWork, participants: usize) -> u64 {
    use core::mem::size_of;
    let wide = |n: usize| n as u64;
    let times = |count: u64, each: usize| count.saturating_mul(wide(each));
    let bound = pass.draws.iter().map(|draw| draw.sets.len()).sum::<usize>();
    // Every set's memories at most, however few are distinct.
    let memories = pass
        .draws
        .iter()
        .fold(wide(pass.colour.len() * 2 + 1), |total, draw| {
            let sets = draw.sets.iter().fold(0u64, |sum, set| {
                sum.saturating_add(wide(set.memories.len()))
            });
            total
                .saturating_add(sets)
                .saturating_add(wide(draw.vertex_buffers.len() + 1))
        });
    let gathered = times(wide(bound), size_of::<&Arc<BindSetState>>()).saturating_add(times(
        memories,
        size_of::<Arc<Memory>>()
            + size_of::<&Arc<Memory>>()
            + size_of::<crate::memory::Words<'_>>()
            + size_of::<&[AtomicU32]>(),
    ));
    let tables = (0..pass.draws.len())
        .filter(|&at| starts_run(&pass.draws, at))
        .filter_map(|at| pass.draws.get(at))
        .fold(0u64, |total, draw| {
            let render = &draw.pipeline.stages;
            let places = draw.sets.iter().fold(0u64, |sum, set| {
                sum.saturating_add(wide(set.memories.len()))
            });
            let programs = core::iter::once(&render.vertex)
                .chain(render.fragment.as_ref().map(|shading| &shading.program));
            programs
                .fold(
                    total.saturating_add(times(places, size_of::<u32>())),
                    |sum, program| {
                        sum.saturating_add(times(program.descriptors, size_of::<Resolved<'_>>()))
                            .saturating_add(times(
                                wide(program.bindings.len()),
                                size_of::<&[Resolved<'_>]>(),
                            ))
                    },
                )
                .saturating_add(wide(size_of::<(
                    usize,
                    Vec<Resolved<'_>>,
                    Option<Vec<Resolved<'_>>>,
                )>()))
                .saturating_add(wide(size_of::<(
                    Vec<&[Resolved<'_>]>,
                    Option<Vec<&[Resolved<'_>]>>,
                )>()))
        });
    let draws = pass.draws.iter().fold(0u64, |total, draw| {
        total
            .saturating_add(wide(size_of::<Drawn<'_>>() + size_of::<usize>()))
            .saturating_add(times(
                wide(draw.vertex_buffers.len()),
                size_of::<Option<(&[AtomicU32], u64, u64)>>(),
            ))
    });
    let record = pass
        .draws
        .iter()
        .map(|draw| draw.pipeline.stages.record as usize)
        .max()
        .unwrap_or(0);
    let batches = times(
        wide(FLUSH_BATCHES),
        size_of::<Batch>()
            + size_of::<Planned>()
            + size_of::<Mutex<&mut Batch>>()
            + front::BATCH_POSITIONS
                .saturating_mul(record)
                .saturating_mul(size_of::<u32>())
            + front::BATCH_PRIMITIVES as usize * size_of::<front::Setup>(),
    );
    let rooms = times(
        wide(participants),
        size_of::<Mutex<Slot<'_>>>() + size_of::<Shaded>(),
    );
    [gathered, tables, draws, batches, rooms]
        .into_iter()
        .fold(0, u64::saturating_add)
}

#[cfg(test)]
#[path = "render_tests.rs"]
mod tests;
