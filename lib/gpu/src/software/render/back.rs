//! A pass's back end: each bin of its attachments drawn in place, its
//! batches' primitives rasterised in order — each piece of a primitive's
//! clipped polygon set up as it is reached, its coverage an eight-pixel block
//! at a time, the covered quads shaded sixteen to a tile, each sample tested
//! and blended in turn.

use alloc::boxed::Box;

use tairix_inline::ArrayVec;
use tairix_raster::{
    BlendInputs, BlendRange, CompareOp, Interpolation, Outcome, Primitive, StencilFace, StencilOp,
    BLOCK, MESH_SUB_UNITS,
};
use tairix_spirv::Component;
use tairix_util::mathf;

use super::front::{Batch, Setup};
use super::{merge, Area, Drawn, Frame};
use crate::format::Format;
use crate::render::{ColourMask, Effects, Shading};
use crate::software::dispatch::Workspace;
use crate::software::shader::exec::{shade, Lanes, Placement};
use crate::software::shader::ir::{Carries, Fixed, LANES};
use crate::software::shader::ops::{low32, single_bits};
use crate::software::texel::{decode, encode, Plane, Srgb};
use crate::software::Meter;
use crate::MAX_PASS_ATTACHMENTS;

/// The quads one fragment tile holds.
const QUADS: usize = LANES / 4;

/// A piece of a primitive a tile's quads came from: where it is, by batch,
/// primitive and piece, and the piece set up.
struct Piece {
    at: (u32, u32, u32),
    primitive: Primitive,
}

/// A quad waiting in a tile: its piece's place among the tile's, the pixel
/// at its top-left, and each of its pixels' samples its piece covers within
/// the draw's sample mask, and those of them its tests before shading kept.
#[derive(Copy, Clone, Debug)]
struct Quad {
    piece: u8,
    x: u32,
    y: u32,
    raster: [u8; 4],
    coverage: [u8; 4],
}

/// Which samples a depth and stencil test writes the outcome of.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Writes {
    /// Every one it tests.
    Every,
    /// Those that fail it: where tests before shading cannot differ from
    /// those after it, a failing sample's outcome is final then.
    Failing,
}

/// What a fragment tile's lanes wrote: each target's colour, the second
/// colour a dual-source blend reads, the depth, and the coverage.
pub(super) struct Shaded {
    colour: [[[u32; 4]; LANES]; MAX_PASS_ATTACHMENTS],
    second: [[u32; 4]; LANES],
    depth: [u32; LANES],
    mask: [u32; LANES],
}

impl Shaded {
    pub(super) const fn new() -> Self {
        Self {
            colour: [[[0; 4]; LANES]; MAX_PASS_ATTACHMENTS],
            second: [[0; 4]; LANES],
            depth: [0; LANES],
            mask: [0; LANES],
        }
    }
}

/// One participant's room for the bins it draws: its workspace, where its
/// pass shades, and what a tile it shades writes.
pub(super) struct Room<'w> {
    pub(super) workspace: Option<&'w mut Workspace>,
    pub(super) shaded: Box<Shaded>,
}

/// How a draw's depth and stencil tests fall about its shading.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
enum Tests {
    /// Tested and written before shading, as the fragment stage asks.
    Early,
    /// Tested before shading, where the outcome cannot differ, and tested
    /// again and written after it.
    Culled,
    /// Tested and written after shading.
    Late,
}

impl Drawn<'_> {
    /// The samples of a pixel of `samples` its pipeline's sample mask lets it
    /// cover.
    fn sample_mask(&self, samples: u32) -> u8 {
        let all = (1u32 << samples.min(8)) - 1;
        u8::try_from(self.render.multisample.mask & all).unwrap_or(u8::MAX)
    }

    fn tests(&self) -> Tests {
        let Some(shading) = &self.render.fragment else {
            return Tests::Early;
        };
        let effects = shading.effects;
        if effects.contains(Effects::EARLY) {
            return Tests::Early;
        }
        if effects.contains(Effects::WRITES_MEMORY) || effects.contains(Effects::WRITES_DEPTH) {
            return Tests::Late;
        }
        let changes_coverage = effects.contains(Effects::DISCARDS)
            || effects.contains(Effects::WRITES_MASK)
            || self.render.multisample.alpha_to_coverage;
        let keeps_on_failure = self
            .render
            .depth_stencil
            .and_then(|state| state.stencil)
            .is_none_or(|faces| {
                faces
                    .iter()
                    .all(|face| face.fail == StencilOp::Keep && face.depth_fail == StencilOp::Keep)
            });
        if changes_coverage && !keeps_on_failure {
            Tests::Late
        } else {
            Tests::Culled
        }
    }
}

/// A bin being drawn: its pixels, the room it is drawn in, and the quads
/// waiting to be shaded with the pieces they came from.
struct Drawing<'a, 'd, 'w> {
    frame: &'a Frame<'d>,
    draws: &'a [Drawn<'d>],
    batches: &'a [Batch],
    bin: Area,
    room: &'a mut Room<'w>,
    tile: ArrayVec<Quad, QUADS>,
    pieces: ArrayVec<Piece, QUADS>,
    /// The draw the waiting quads are of.
    draw: usize,
}

/// Draw `bin` of `frame`: cleared where `first` and the pass clears, every
/// primitive of `batches` reaching it rasterised in order, and resolved where
/// `last` and the pass resolves, in `room`, counted on `meter`: `false` where
/// it stopped short or found the work broken.
pub(super) fn draw_bin(
    (frame, bound, batches): (&Frame<'_>, &[Drawn<'_>], &[Batch]),
    (bin, first, last): (Area, bool, bool),
    room: &mut Room<'_>,
    meter: &mut Meter<'_>,
) -> bool {
    if first && !frame.clear(bin, meter) {
        return false;
    }
    let mut drawing = Drawing {
        frame,
        draws: bound,
        batches,
        bin,
        room,
        tile: ArrayVec::new(),
        pieces: ArrayVec::new(),
        draw: 0,
    };
    for (at, batch) in (0u32..).zip(batches) {
        if !batch.reach.is_some_and(|reach| reach.meets(bin)) {
            continue;
        }
        let Some(drawn) = bound.get(batch.draw) else {
            return false;
        };
        for (index, setup) in (0u32..).zip(&batch.setups) {
            if !setup.area.meets(bin) {
                continue;
            }
            if !meter.proceed(0) || !drawing.rasterise(drawn, (at, index), setup, meter) {
                return false;
            }
        }
    }
    drawing.shade_tile(meter) && (!last || frame.resolve(bin, meter))
}

/// The bits of a block's pixels within `area`, row by row from bit 0, of the
/// block whose top-left is `(x, y)`.
fn window(area: Area, (x, y): (u32, u32)) -> u64 {
    let mut bits = 0u64;
    for row in 0..BLOCK {
        let py = y + row;
        if py < area.y0 || py >= area.y1 {
            continue;
        }
        let from = area.x0.saturating_sub(x).min(BLOCK);
        let to = area.x1.saturating_sub(x).min(BLOCK);
        if from < to {
            let run = ((1u64 << (to - from)) - 1) << from;
            bits |= run << (row * BLOCK);
        }
    }
    bits
}

/// The top-left pixel of quad `quad` of a block, from the block's.
const fn quad_corner((x, y): (u32, u32), quad: u32) -> (u32, u32) {
    (x + quad % 4 * 2, y + quad / 4 * 2)
}

/// The pixel of lane `lane` of a quad whose top-left is `(x, y)`.
const fn lane_pixel((x, y): (u32, u32), lane: u32) -> (u32, u32) {
    (x + (lane & 1), y + ((lane >> 1) & 1))
}

/// Each quad's pixels' covered samples, of the block `masks` covers: the
/// sixteen quads in row order.
fn quads(masks: &[u64; 4], samples: usize) -> [[u8; 4]; QUADS] {
    let mut covered = [[0u8; 4]; QUADS];
    for (quad, cover) in (0u32..).zip(&mut covered) {
        for (lane, held) in (0u32..).zip(cover.iter_mut()) {
            let (column, row) = lane_pixel(quad_corner((0, 0), quad), lane);
            let bit = row * BLOCK + column;
            for (sample, mask) in masks.iter().enumerate().take(samples) {
                if mask >> bit & 1 != 0 {
                    *held |= 1 << sample;
                }
            }
        }
    }
    covered
}

impl Drawing<'_, '_, '_> {
    /// Rasterise the primitive `setup`, at `at`, into the bin: each piece of
    /// its polygon a block at a time, its covered quads tested where its draw
    /// tests early and queued to be shaded.
    fn rasterise(
        &mut self,
        drawn: &Drawn<'_>,
        at: (u32, u32),
        setup: &Setup,
        meter: &mut Meter<'_>,
    ) -> bool {
        let Some(rect) = setup.area.within(self.bin) else {
            return true;
        };
        let lattice = self.frame.lattice();
        let samples = self.frame.samples as usize;
        let sample_mask = drawn.sample_mask(self.frame.samples);
        let inside = drawn
            .inside
            .map(|area| area.and_then(|area| area.within(rect)));
        // Each sample's pixels are its own only where a viewport edge falls
        // between them; otherwise one window serves every sample.
        let shared = inside.iter().take(samples).all(|area| *area == inside[0]);
        for piece in 0..setup.pieces() {
            let Some(primitive) = setup.piece(piece) else {
                continue;
            };
            let Ok(index) = u32::try_from(piece) else {
                return false;
            };
            let key = (at.0, at.1, index);
            let mut y = rect.y0 / BLOCK * BLOCK;
            while y < rect.y1 {
                let mut x = rect.x0 / BLOCK * BLOCK;
                while x < rect.x1 {
                    let first = inside[0].map_or(0, |area| window(area, (x, y)));
                    let mut masks = primitive.footprint().block((x, y), lattice);
                    for (sample, mask) in masks.iter_mut().enumerate().take(samples) {
                        *mask &= if shared {
                            first
                        } else {
                            inside[sample].map_or(0, |area| window(area, (x, y)))
                        };
                    }
                    meter.charge(u64::from(BLOCK * BLOCK));
                    if masks.iter().any(|&mask| mask != 0) {
                        for (quad, coverage) in (0u32..).zip(quads(&masks, samples)) {
                            let coverage = coverage.map(|cover| cover & sample_mask);
                            if coverage == [0; 4] {
                                continue;
                            }
                            let corner = quad_corner((x, y), quad);
                            if !self.queue(
                                drawn,
                                setup,
                                (key, &primitive),
                                (corner, coverage),
                                meter,
                            ) {
                                return false;
                            }
                        }
                    }
                    x += BLOCK;
                }
                y += BLOCK;
            }
        }
        true
    }

    /// Queue the quad at `corner` of `drawn`'s piece `key`, shading what waits
    /// first where it must go before: another draw's quads, a full tile, or,
    /// where its tests cull early, a quad of the same pixels whose writes it
    /// must see.
    fn queue(
        &mut self,
        drawn: &Drawn<'_>,
        setup: &Setup,
        (key, primitive): ((u32, u32, u32), &Primitive),
        (corner, raster): ((u32, u32), [u8; 4]),
        meter: &mut Meter<'_>,
    ) -> bool {
        let tests = drawn.tests();
        let mut coverage = raster;
        let draw = self
            .batches
            .get(key.0 as usize)
            .map_or(usize::MAX, |batch| batch.draw);
        let conflicting =
            tests == Tests::Culled && self.tile.iter().any(|held| (held.x, held.y) == corner);
        let crowded = self.tile.is_full()
            || (self.pieces.is_full() && !self.pieces.iter().any(|piece| piece.at == key));
        if !self.tile.is_empty()
            && (self.draw != draw || crowded || conflicting)
            && !self.shade_tile(meter)
        {
            return false;
        }
        if tests != Tests::Late && drawn.render.depth_stencil.is_some() {
            let writes = if tests == Tests::Early {
                Writes::Every
            } else {
                Writes::Failing
            };
            for (lane, cover) in (0u32..).zip(coverage.iter_mut()) {
                let pixel = lane_pixel(corner, lane);
                *cover = self.test(drawn, (setup, primitive), pixel, (*cover, None), writes);
            }
            meter.charge(4 * u64::from(self.frame.samples));
            if coverage == [0; 4] {
                return true;
            }
        }
        if drawn.render.fragment.is_none() {
            // Nothing to shade: its tests were its whole work.
            return true;
        }
        let slot = if let Some(slot) = self.pieces.iter().position(|piece| piece.at == key) {
            slot
        } else {
            let piece = Piece {
                at: key,
                primitive: primitive.clone(),
            };
            if self.pieces.try_push(piece).is_err() {
                return false;
            }
            self.pieces.len() - 1
        };
        let Ok(piece) = u8::try_from(slot) else {
            return false;
        };
        self.draw = draw;
        self.tile
            .try_push(Quad {
                piece,
                x: corner.0,
                y: corner.1,
                raster,
                coverage,
            })
            .is_ok()
    }

    /// The depth of sample `sample` of pixel `(x, y)` of a piece, as its draw
    /// tests it: the fragment's own written depth where it writes one, held
    /// to the range a clamp or the format allows.
    fn depth_at(
        &self,
        drawn: &Drawn<'_>,
        (setup, primitive): (&Setup, &Primitive),
        (x, y, sample): (u32, u32, usize),
        written: Option<f32>,
    ) -> f64 {
        let depth = written.map_or_else(
            || {
                let (sx, sy) = self.frame.sample_position(sample);
                primitive.depth(f64::from(x) + sx, f64::from(y) + sy) + setup.depth_offset
            },
            f64::from,
        );
        let (low, high) = drawn.depth_range;
        mathf::clamp(depth, low, high)
    }

    /// The samples of `cover` of pixel `(x, y)` that pass the draw's stencil
    /// and depth tests, the outcome of those `writes` names written: where
    /// the fragment wrote its own depth, `written`.
    fn test(
        &mut self,
        drawn: &Drawn<'_>,
        piece: (&Setup, &Primitive),
        (x, y): (u32, u32),
        (cover, written): (u8, Option<f32>),
        writes: Writes,
    ) -> u8 {
        let (Some(state), Some(held)) = (drawn.render.depth_stencil, self.frame.depth.as_ref())
        else {
            return cover;
        };
        let faces = state.stencil.map(|faces| {
            let face = if piece.0.front { faces[0] } else { faces[1] };
            StencilFace {
                fail: face.fail,
                pass: face.pass,
                depth_fail: face.depth_fail,
                compare: face.compare,
                compare_mask: face.compare_mask,
                write_mask: face.write_mask,
                reference: drawn.work.stencil_reference,
            }
        });
        let depth_format = drawn.depth_format;
        let srgb = self.frame.srgb;
        let mut passed = 0u8;
        for sample in 0..self.frame.samples as usize {
            if cover >> sample & 1 == 0 {
                continue;
            }
            // The fragment's depth as the format holds it, where it is tested
            // or written.
            let mine = depth_format
                .filter(|_| state.depth_write || state.depth_compare != CompareOp::Always)
                .map(|format| {
                    format.load(format.store(self.depth_at(drawn, piece, (x, y, sample), written)))
                });
            let mut raw = held.read((x, y), sample);
            let before = raw;
            let stencil = faces.map(|_| decode(held.format, Plane::Stencil, raw, srgb)[0]);
            let stencil_passes = match (faces, stencil) {
                (Some(face), Some(value)) => face.passes(u8::try_from(value & 0xff).unwrap_or(0)),
                _ => true,
            };
            let depth_passes = stencil_passes
                && match mine {
                    Some(mine) if state.depth_compare != CompareOp::Always => {
                        let theirs = f64::from(f32::from_bits(
                            decode(held.format, Plane::Depth, raw, srgb)[0],
                        ));
                        state.depth_compare.passes(mine, theirs)
                    }
                    _ => true,
                };
            if writes == Writes::Every || !depth_passes {
                if let (Some(face), Some(value)) = (faces, stencil) {
                    let outcome = if !stencil_passes {
                        Outcome::StencilFailed
                    } else if depth_passes {
                        Outcome::Passed
                    } else {
                        Outcome::DepthFailed
                    };
                    let next = face.write(outcome, u8::try_from(value & 0xff).unwrap_or(0));
                    let stored = encode(
                        held.format,
                        Plane::Stencil,
                        [u32::from(next), 0, 0, 0],
                        srgb,
                    );
                    merge(&mut raw, stored, held.stencil_bits);
                }
                if let (true, true, Some(mine)) = (depth_passes, state.depth_write, mine) {
                    let stored = encode(
                        held.format,
                        Plane::Depth,
                        [single_bits(mine), 0, 0, 0],
                        srgb,
                    );
                    merge(&mut raw, stored, held.depth_bits);
                }
                if raw != before {
                    held.write((x, y), sample, raw);
                }
            }
            if depth_passes {
                passed |= 1 << sample;
            }
        }
        passed
    }

    /// Shade the quads waiting, then test and write each of their samples in
    /// the order they were queued.
    fn shade_tile(&mut self, meter: &mut Meter<'_>) -> bool {
        if self.tile.is_empty() {
            return true;
        }
        let tile = core::mem::take(&mut self.tile);
        let pieces = core::mem::take(&mut self.pieces);
        let Some(drawn) = self.draws.get(self.draw) else {
            return false;
        };
        let (Some(shading), Some(env)) = (&drawn.render.fragment, &drawn.fragment) else {
            return false;
        };
        let (mut live, mut helpers) = (0u64, 0u64);
        for (slot, quad) in tile.iter().enumerate() {
            for (lane, &cover) in quad.coverage.iter().enumerate() {
                let bit = 1u64 << (4 * slot + lane);
                if cover == 0 {
                    helpers |= bit;
                } else {
                    live |= bit;
                }
            }
        }
        let place = Placement::quads(live, helpers, env.subgroup_size);
        let mut feeder = Feeder {
            frame: self.frame,
            batches: self.batches,
            drawn,
            shading,
            tile: &tile,
            pieces: &pieces,
            weights: None,
        };
        let killed = {
            let Some(space) = self
                .room
                .workspace
                .as_deref_mut()
                .and_then(|workspace| workspace.tile(&shading.program))
            else {
                return false;
            };
            let shaded = &mut self.room.shaded;
            match shade(
                (env, &place),
                space,
                meter,
                |port, word, values| feeder.feed(port, word, values),
                |port, word, values| take(shading, (port, word), values, shaded),
            ) {
                Ok(killed) => killed,
                Err(_) => return false,
            }
        };
        for (slot, quad) in tile.iter().enumerate() {
            let Some(piece) = pieces.get(usize::from(quad.piece)) else {
                return false;
            };
            let Some(setup) = self
                .batches
                .get(piece.at.0 as usize)
                .and_then(|batch| batch.setups.get(piece.at.1 as usize))
            else {
                return false;
            };
            if !self.resolve_quad(
                drawn,
                shading,
                (setup, &piece.primitive),
                (slot, quad),
                killed,
                meter,
            ) {
                return false;
            }
        }
        true
    }

    /// Test and write the samples of `quad`, in tile slot `slot`, its lanes'
    /// outputs those the tile left.
    fn resolve_quad(
        &mut self,
        drawn: &Drawn<'_>,
        shading: &Shading,
        piece: (&Setup, &Primitive),
        (slot, quad): (usize, &Quad),
        killed: u64,
        meter: &mut Meter<'_>,
    ) -> bool {
        let tests = drawn.tests();
        let samples = self.frame.samples;
        for (lane, &cover) in (0u32..).zip(&quad.coverage) {
            let at = 4 * slot + lane as usize;
            if cover == 0 || killed >> at & 1 != 0 {
                continue;
            }
            let mut cover = cover;
            if shading.effects.contains(Effects::WRITES_MASK) {
                let written = self.room.shaded.mask.get(at).copied().unwrap_or(0);
                cover &= u8::try_from(written & 0xff).unwrap_or(0);
            }
            if drawn.render.multisample.alpha_to_coverage {
                let first = shading.channels.first().copied().unwrap_or(0);
                let alpha = self.room.shaded.colour[0].get(at).map_or(0.0, |&colour| {
                    f32::from_bits(written_only(colour, first)[3])
                });
                cover &= alpha_coverage(alpha, samples);
            }
            let pixel = lane_pixel((quad.x, quad.y), lane);
            if tests != Tests::Early && drawn.render.depth_stencil.is_some() {
                let written = shading
                    .effects
                    .contains(Effects::WRITES_DEPTH)
                    .then(|| f32::from_bits(self.room.shaded.depth.get(at).copied().unwrap_or(0)));
                cover = self.test(drawn, piece, pixel, (cover, written), Writes::Every);
            }
            if cover != 0 && !self.write_colour((drawn, shading), pixel, at, cover) {
                return false;
            }
            meter.charge(u64::from(samples) * (self.frame.targets.len() as u64 + 1));
        }
        true
    }

    /// Write lane `at`'s colours into each target's samples of `cover` at
    /// pixel `pixel`, blended or combined as the draw says: a target the
    /// fragment stage writes no channel of is left as it was.
    fn write_colour(
        &mut self,
        (drawn, shading): (&Drawn<'_>, &Shading),
        pixel: (u32, u32),
        at: usize,
        cover: u8,
    ) -> bool {
        let srgb = self.frame.srgb;
        let second = written_only(
            self.room.shaded.second.get(at).copied().unwrap_or([0; 4]),
            shading.second_channels,
        );
        for (index, (target, held)) in drawn
            .render
            .targets
            .iter()
            .zip(&self.frame.targets)
            .enumerate()
        {
            let channels = shading.channels.get(index).copied().unwrap_or(0);
            if channels == 0 {
                continue;
            }
            let Some(source) = self
                .room
                .shaded
                .colour
                .get(index)
                .and_then(|lanes| lanes.get(at))
                .map(|&colour| written_only(colour, channels))
            else {
                return false;
            };
            let format = held.format;
            let logic = drawn.render.logic_op.filter(|_| combines_logically(format));
            let blend = target.blend.filter(|_| {
                drawn.render.logic_op.is_none() && format.component() == Component::Float
            });
            // Neither blended nor combined, every sample takes one value.
            let plain = (logic.is_none() && blend.is_none())
                .then(|| encode(format, Plane::Colour, source, srgb));
            for sample in 0..self.frame.samples as usize {
                if cover >> sample & 1 == 0 {
                    continue;
                }
                let stored = held.read(pixel, sample);
                let result = match (plain, logic, blend) {
                    (Some(raw), ..) => raw,
                    (None, Some(op), _) => {
                        let source = encode(format, Plane::Colour, source, srgb);
                        core::array::from_fn(|word| op.apply(source[word], stored[word]))
                    }
                    (None, None, Some(blend)) => {
                        let destination =
                            decode(format, Plane::Colour, stored, srgb).map(f32::from_bits);
                        let blended = blend.apply(
                            range_of(format),
                            BlendInputs {
                                source: source.map(f32::from_bits),
                                source1: second.map(f32::from_bits),
                                destination,
                                constant: drawn.work.blend_constant,
                            },
                        );
                        encode(format, Plane::Colour, blended.map(f32::to_bits), srgb)
                    }
                    (None, None, None) => encode(format, Plane::Colour, source, srgb),
                };
                let result = if target.write_mask == ColourMask::ALL {
                    result
                } else {
                    masked(format, (result, stored), target.write_mask, srgb)
                };
                held.write(pixel, sample, result);
            }
        }
        true
    }
}

/// `colour` in the channels `channels` names, red at bit 0, and zero in the
/// rest: what a fragment output of fewer components writes.
fn written_only(colour: [u32; 4], channels: u8) -> [u32; 4] {
    core::array::from_fn(|channel| {
        if channels >> channel & 1 == 0 {
            0
        } else {
            colour[channel]
        }
    })
}

/// The samples alpha `alpha` covers of `samples`: from the first, a share
/// as large as the alpha, rounded to the nearest sample.
fn alpha_coverage(alpha: f32, samples: u32) -> u8 {
    let held = mathf::clamp(f64::from(alpha), 0.0, 1.0);
    let count = u32::try_from(mathf::round_i32(held * f64::from(samples))).unwrap_or(0);
    u8::try_from((1u32 << count.min(samples)) - 1).unwrap_or(u8::MAX)
}

/// Whether a logic operation combines a target of `format`: an integer or
/// normalised one, and not sRGB.
const fn combines_logically(format: Format) -> bool {
    !format.is_srgb() && !format.is_float()
}

/// The range a blend into `format` holds its result to.
const fn range_of(format: Format) -> BlendRange {
    if format.is_float() {
        BlendRange::Unbounded
    } else if format.is_signed_normalised() {
        BlendRange::Signed
    } else {
        BlendRange::Unsigned
    }
}

/// `result` in the channels `mask` names and `held` in the rest, of a texel
/// of `format`.
fn masked(
    format: Format,
    (result, held): ([u32; 4], [u32; 4]),
    mask: ColourMask,
    srgb: &Srgb,
) -> [u32; 4] {
    let new = decode(format, Plane::Colour, result, srgb);
    let old = decode(format, Plane::Colour, held, srgb);
    let channels = [
        ColourMask::RED,
        ColourMask::GREEN,
        ColourMask::BLUE,
        ColourMask::ALPHA,
    ];
    let chosen = core::array::from_fn(|channel| {
        if mask.contains(channels[channel]) {
            new[channel]
        } else {
            old[channel]
        }
    });
    encode(format, Plane::Colour, chosen, srgb)
}

/// What feeds a fragment tile's inputs: the pass's frame, the batches its
/// primitives were set up in, the draw and its fragment stage, the quads
/// with the pieces they came from, and the weights its lanes last
/// interpolated with, found once for an input's words.
struct Feeder<'a, 'd> {
    frame: &'a Frame<'d>,
    batches: &'a [Batch],
    drawn: &'a Drawn<'d>,
    shading: &'a Shading,
    tile: &'a [Quad],
    pieces: &'a [Piece],
    weights: Option<(Weighed, [[f64; 3]; LANES])>,
}

/// What a tile's weights were found for: an input port, how it varies, and
/// whether at its centroid.
type Weighed = (usize, Interpolation, bool);

impl Feeder<'_, '_> {
    /// The piece `quad` came from, with its batch and setup.
    fn piece_of(&self, quad: &Quad) -> Option<(&Batch, &Setup, &Primitive)> {
        let piece = self.pieces.get(usize::from(quad.piece))?;
        let batch = self.batches.get(piece.at.0 as usize)?;
        Some((
            batch,
            batch.setups.get(piece.at.1 as usize)?,
            &piece.primitive,
        ))
    }

    /// Word `word` of input port `port` for every lane of the tile.
    fn feed(&mut self, port: usize, word: u32, values: &mut Lanes) {
        let Some(carries) = self.shading.program.inputs.get(port).map(|p| p.carries) else {
            return;
        };
        if let Carries::Located { .. } = carries {
            self.located((port, word), values);
            return;
        }
        for (slot, quad) in self.tile.iter().enumerate() {
            let Some((_, setup, primitive)) = self.piece_of(quad) else {
                continue;
            };
            for lane in 0..4u32 {
                let (px, py) = lane_pixel((quad.x, quad.y), lane);
                let (cx, cy) = (f64::from(px) + 0.5, f64::from(py) + 0.5);
                let value = match carries {
                    Carries::Fixed(Fixed::FragCoord) => match word {
                        0 => single_bits(cx),
                        1 => single_bits(cy),
                        2 => {
                            let depth = primitive.depth(cx, cy) + setup.depth_offset;
                            let (low, high) = self.drawn.depth_range;
                            single_bits(mathf::clamp(depth, low, high))
                        }
                        _ => single_bits(primitive.inverse_w(cx, cy)),
                    },
                    Carries::Fixed(Fixed::FrontFacing) => u32::from(setup.front),
                    Carries::Fixed(Fixed::PointCoord) => {
                        setup.point.map_or(0, |(left, top, size)| {
                            let along = if word == 0 {
                                (cx - left) / size
                            } else {
                                (cy - top) / size
                            };
                            single_bits(along)
                        })
                    }
                    Carries::Fixed(Fixed::SampleMask) if word == 0 => {
                        u32::from(quad.raster.get(lane as usize).copied().unwrap_or(0))
                    }
                    Carries::Fixed(_) | Carries::Located { .. } => 0,
                };
                if let Some(slot) = values.get_mut(4 * slot + lane as usize) {
                    *slot = u64::from(value);
                }
            }
        }
    }

    /// Where a centroid input is interpolated in pixel `(x, y)`, whose
    /// samples `cover` a piece covers: its centre where the piece covers that,
    /// else its first covered sample.
    fn centroid(&self, primitive: &Primitive, (x, y): (u32, u32), cover: u8) -> (f64, f64) {
        let all = u8::try_from((1u32 << self.frame.samples) - 1).unwrap_or(u8::MAX);
        let unit = MESH_SUB_UNITS;
        let centre = |at: u32| {
            i32::try_from(at).map_or(i32::MAX, |at| {
                at.saturating_mul(unit).saturating_add(unit / 2)
            })
        };
        if cover == all || primitive.footprint().covers(centre(x), centre(y)) {
            return (0.5, 0.5);
        }
        self.frame.sample_position(cover.trailing_zeros() as usize)
    }

    /// Each lane's weights for `how`, at its centroid where `centroid`.
    fn weights_for(&self, how: Interpolation, centroid: bool) -> [[f64; 3]; LANES] {
        let mut weights = [[0.0; 3]; LANES];
        for (slot, quad) in self.tile.iter().enumerate() {
            let Some((_, _, primitive)) = self.piece_of(quad) else {
                continue;
            };
            for lane in 0..4u32 {
                let (x, y) = lane_pixel((quad.x, quad.y), lane);
                let cover = quad.raster.get(lane as usize).copied().unwrap_or(0);
                let (dx, dy) = if centroid {
                    self.centroid(primitive, (x, y), cover)
                } else {
                    (0.5, 0.5)
                };
                if let Some(held) = weights.get_mut(4 * slot + lane as usize) {
                    *held = primitive.weights(how, f64::from(x) + dx, f64::from(y) + dy);
                }
            }
        }
        weights
    }

    /// Word `word` of the located input port `port` for every lane: the
    /// vertex outputs it reads interpolated as it asks.
    fn located(&mut self, (port, word): (usize, u32), values: &mut Lanes) {
        let Some(link) = self
            .shading
            .starts
            .get(port)
            .and_then(|&first| self.shading.links.get(first as usize + word as usize))
            .copied()
        else {
            return;
        };
        let Some(source) = link.word else {
            return;
        };
        let how = match link.interpolation {
            tairix_spirv::Interpolation::Flat => None,
            tairix_spirv::Interpolation::Smooth => Some(Interpolation::Perspective),
            tairix_spirv::Interpolation::NoPerspective => Some(Interpolation::Linear),
        };
        if let Some(how) = how {
            let key = (port, how, link.centroid);
            if self.weights.as_ref().is_none_or(|(held, _)| *held != key) {
                self.weights = Some((key, self.weights_for(how, link.centroid)));
            }
        }
        let value = |bits: u32| {
            if link.narrow {
                f64::from(mathf::from_half(u16::try_from(bits & 0xffff).unwrap_or(0)))
            } else {
                f64::from(f32::from_bits(bits))
            }
        };
        for (slot, quad) in self.tile.iter().enumerate() {
            let Some((batch, setup, _)) = self.piece_of(quad) else {
                continue;
            };
            let corner = |index: usize| {
                setup
                    .corners
                    .get(index)
                    .and_then(|&record| batch.records.get((record + source) as usize))
                    .copied()
                    .unwrap_or(0)
            };
            for lane in 0..4usize {
                let at = 4 * slot + lane;
                let mixed = match (how, &self.weights) {
                    (Some(_), Some((_, weights))) => {
                        let weights = weights.get(at).copied().unwrap_or([0.0; 3]);
                        let mixed = weights[0] * value(corner(0))
                            + weights[1] * value(corner(1))
                            + weights[2] * value(corner(2));
                        if link.narrow {
                            u32::from(mathf::to_half(mathf::single(mixed)))
                        } else {
                            single_bits(mixed)
                        }
                    }
                    _ => corner(0),
                };
                if let Some(held) = values.get_mut(at) {
                    *held = u64::from(mixed);
                }
            }
        }
    }
}

/// Keep word `word` of output port `port` of a tile's lanes where its
/// meaning says.
fn take(shading: &Shading, (port, word): (usize, u32), values: &Lanes, shaded: &mut Shaded) {
    let Some(carries) = shading.program.outputs.get(port).map(|p| p.carries) else {
        return;
    };
    let narrow = shading.narrow_outputs >> port & 1 != 0;
    let widen = |value: u64| {
        let bits = low32(value);
        if narrow {
            mathf::from_half(u16::try_from(bits & 0xffff).unwrap_or(0)).to_bits()
        } else {
            bits
        }
    };
    match carries {
        Carries::Located {
            location,
            component,
            index,
        } => {
            let channel = (component + word) as usize;
            let lanes = if index == 0 {
                shaded.colour.get_mut(location as usize)
            } else {
                Some(&mut shaded.second)
            };
            if let Some(lanes) = lanes {
                for (held, &value) in lanes.iter_mut().zip(values) {
                    if let Some(slot) = held.get_mut(channel) {
                        *slot = widen(value);
                    }
                }
            }
        }
        Carries::Fixed(Fixed::FragDepth) => {
            for (held, &value) in shaded.depth.iter_mut().zip(values) {
                *held = low32(value);
            }
        }
        Carries::Fixed(Fixed::SampleMask) if word == 0 => {
            for (held, &value) in shaded.mask.iter_mut().zip(values) {
                *held = low32(value);
            }
        }
        Carries::Fixed(_) => {}
    }
}
