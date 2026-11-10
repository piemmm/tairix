//! Images as a tile reaches them: texels read and written by coordinate,
//! fetched, sampled through a sampler's filtering, addressing and level of
//! detail, gathered, and queried; and pointers to a storage texel for an
//! atomic.
//!
//! An access past the view — its extent, its levels, its layers or its
//! samples — reads what a texel of zeros decodes to, zero in every channel
//! the format holds and one for an alpha it lacks, and writes nothing.

use core::sync::atomic::AtomicU32;

use tairix_spirv::Component;
use tairix_util::mathf;

use super::memory::{ImageAt, Resolved, INVALID};
use super::{each_lane, low32, Machine};
use crate::format::{Aspects, Format};
use crate::image::ViewDimension;
use crate::sampler::{AddressMode, BorderColor, CompareOp, Filter, SamplerDesc, MAX_LOD_BIAS};
use crate::software::shader::ir::{Image, ImageOp, Num, Reg, LANES};
use crate::software::shader::ops;
use crate::software::texel::{self, missing, put_raw, raw_at, Plane, ONE};

/// A texel as a shader reads it: four words.
type Texel = [u32; 4];

/// An image and how it is sampled, as one lane names them.
#[derive(Copy, Clone)]
struct Bound<'d> {
    image: Option<ImageAt<'d>>,
    texels: Option<(&'d [AtomicU32], u64, u64, Format)>,
    sampler: Option<&'d SamplerDesc>,
}

/// Where a texel lies: its level, layer and place, all within the view.
#[derive(Copy, Clone)]
struct At {
    level: u32,
    layer: u32,
    x: u32,
    y: u32,
    z: u32,
    sample: u32,
}

/// Where a direction meets a cube view at one level: the level, the cube of
/// an array, the face and its side, and the direction's place on the face
/// in texels.
struct OnCube {
    absolute: u32,
    cube: u32,
    face: u32,
    side: u32,
    across: f64,
    down: f64,
}

impl Machine<'_, '_, '_> {
    /// What lane `lane`'s handle names: its image or texel buffer, and its
    /// sampler.
    fn bound(&self, image: &Image, lane: usize) -> Bound<'_> {
        let resolved = |binding: u32, word: u32| {
            let element =
                usize::try_from(self.word(image.handle, word, lane)).unwrap_or(usize::MAX);
            self.env
                .bindings
                .get(binding as usize)
                .and_then(|row| row.get(element))
                .copied()
        };
        let mut bound = Bound {
            image: None,
            texels: None,
            sampler: None,
        };
        match resolved(image.binding, 0) {
            Some(Resolved::Image(at)) => bound.image = Some(at),
            Some(Resolved::ImageSampler(at, sampler)) => {
                bound.image = Some(at);
                bound.sampler = Some(sampler);
            }
            Some(Resolved::Texels {
                words,
                offset,
                size,
                format,
            }) => bound.texels = Some((words, offset, size, format)),
            _ => {}
        }
        if let Some(Resolved::Sampler(sampler) | Resolved::ImageSampler(_, sampler)) =
            image.sampler.and_then(|binding| resolved(binding, 1))
        {
            bound.sampler = Some(sampler);
        }
        bound
    }

    pub(super) fn image(&mut self, image: &Image) {
        let mut out = [[0u64; LANES]; 4];
        // A uniform handle names one descriptor for every lane.
        let first = self.mask.trailing_zeros() as usize;
        let uniform = image.handle.is_uniform().then(|| self.bound(image, first));
        let lanes = match image.op {
            ImageOp::Write { .. } => self.mask & !self.place.helpers,
            _ => self.mask,
        };
        for lane in each_lane(lanes) {
            let bound = uniform.unwrap_or_else(|| self.bound(image, lane));
            let Some(result) = self.image_lane(image, &bound, lane) else {
                continue;
            };
            for (values, &value) in out.iter_mut().zip(&result) {
                if let Some(slot) = values.get_mut(lane) {
                    *slot = value;
                }
            }
        }
        let words = match image.op {
            ImageOp::Write { .. } => 0,
            ImageOp::Sample { dref: true, .. } | ImageOp::QueryLevels | ImageOp::QuerySamples => 1,
            ImageOp::QuerySize | ImageOp::QuerySizeLod { .. } => u32::from(image.coords).max(1),
            ImageOp::QueryLod => 2,
            _ => 4,
        };
        for (word, values) in (0..words).zip(&out) {
            self.store(image.dst, word, false, values);
        }
    }

    /// What lane `lane`'s run of `image` answers, word by word; `None` for a
    /// write, which answers nothing.
    fn image_lane(&self, image: &Image, bound: &Bound<'_>, lane: usize) -> Option<[u64; 4]> {
        let operands = &image.operands;
        let read = |reg: Option<Reg>| reg.map_or(0, |reg| self.word(reg, 0, lane));
        let mut coord = [0u64; 4];
        for (word, slot) in (0..u32::from(image.coords)).zip(&mut coord) {
            *slot = self.word(image.coord, word, lane);
        }
        let sample = read(operands.sample);
        Some(match image.op {
            ImageOp::Read => widen(self.fetch_texel(bound, image.component, &coord, (0, sample))),
            ImageOp::Write { texel, components } => {
                let mut value = [0u32; 4];
                for (word, slot) in (0..u32::from(components)).zip(&mut value) {
                    *slot = low32(self.word(texel, word, lane));
                }
                self.write_texel(bound, &coord, sample, value);
                return None;
            }
            ImageOp::Fetch => {
                // An offset moves the integer coordinate along each spatial
                // axis, wrapping as the coordinate's own 32 bits do.
                let mut coord = coord;
                if let Some(offset) = operands.offset {
                    let axes = bound
                        .image
                        .as_ref()
                        .map_or(1, |at| spatial_axes(at.view.dimension));
                    for (word, slot) in (0u32..).zip(&mut coord).take(axes) {
                        let moved = low32(*slot).wrapping_add(low32(self.word(offset, word, lane)));
                        *slot = u64::from(moved);
                    }
                }
                widen(self.fetch_texel(
                    bound,
                    image.component,
                    &coord,
                    (read(operands.lod), sample),
                ))
            }
            ImageOp::QuerySize => query_size(bound, 0),
            ImageOp::QuerySizeLod { lod } => query_size(bound, self.word(lod, 0, lane)),
            ImageOp::QueryLevels => [
                bound.image.map_or(0, |at| u64::from(at.view.range.levels)),
                0,
                0,
                0,
            ],
            ImageOp::QuerySamples => [
                bound.image.map_or(0, |at| u64::from(at.desc.samples)),
                0,
                0,
                0,
            ],
            ImageOp::Sample { .. } | ImageOp::QueryLod => match &bound.image {
                Some(at) => self.sample_lane(image, (at, bound.sampler), &coord, lane),
                None => widen(missing(image.component)),
            },
            ImageOp::Gather { component, dref } => match &bound.image {
                Some(at) => {
                    let point = coord.map(|bits| ops::float(image.coord_num, bits));
                    let channel = component.map_or(0, |reg| self.word(reg, 0, lane) & 3);
                    let reference = dref
                        .then(|| depth_reference(at, ops::float(Num::F32, read(operands.dref))));
                    let offset = operands
                        .offset
                        .map(|reg| [self.word(reg, 0, lane), self.word(reg, 1, lane), 0]);
                    let offsets = operands.offsets.map(|reg| {
                        let mut each = [[0u64; 2]; 4];
                        for (word, slot) in (0u32..).step_by(2).zip(&mut each) {
                            *slot = [self.word(reg, word, lane), self.word(reg, word + 1, lane)];
                        }
                        each
                    });
                    widen(self.gather(
                        at,
                        bound.sampler,
                        point,
                        (channel, reference),
                        (offset, offsets),
                    ))
                }
                None => widen(missing(image.component)),
            },
        })
    }

    /// A sample, or a level-of-detail query, for lane `lane` of a view it
    /// binds.
    fn sample_lane(
        &self,
        image: &Image,
        (at, sampler): (&ImageAt<'_>, Option<&SamplerDesc>),
        coord: &[u64; 4],
        lane: usize,
    ) -> [u64; 4] {
        let operands = &image.operands;
        let single = |reg: Reg| ops::float(Num::F32, self.word(reg, 0, lane));
        let (projective, compares) = match image.op {
            ImageOp::Sample { projective, dref } => (projective, dref),
            _ => (false, false),
        };
        let coordinate = |w: usize| ops::float(image.coord_num, coord.get(w).copied().unwrap_or(0));
        let axes = spatial_axes(at.view.dimension);
        // A projective coordinate's divisor follows its spatial axes, however
        // many components the coordinate holds past it.
        let q = if projective { coordinate(axes) } else { 1.0 };
        let point: [f64; 4] = core::array::from_fn(|w| {
            let value = coordinate(w);
            if projective && w < axes {
                value / q
            } else {
                value
            }
        });
        let grads = match operands.grad {
            Some((along_x, along_y)) => {
                let mut grads = ([0.0; 4], [0.0; 4]);
                for (word, (x, y)) in (0u32..)
                    .zip(grads.0.iter_mut().zip(&mut grads.1))
                    .take(axes)
                {
                    *x = ops::float(image.coord_num, self.word(along_x, word, lane));
                    *y = ops::float(image.coord_num, self.word(along_y, word, lane));
                }
                grads
            }
            None if operands.lod.is_none() => self.quad_grads(image, lane, (axes, projective)),
            None => ([0.0; 4], [0.0; 4]),
        };
        let grads = if is_cube(at.view.dimension) {
            let direction = [point[0], point[1], point[2]];
            let onto = |d: &[f64; 4]| {
                let [s, t] = face_derivatives(direction, [d[0], d[1], d[2]]);
                [s, t, 0.0, 0.0]
            };
            (onto(&grads.0), onto(&grads.1))
        } else {
            grads
        };
        let level = Lod {
            explicit: operands.lod.map(single),
            bias: operands.bias.map_or(0.0, single),
            grads,
        };
        if image.op == ImageOp::QueryLod {
            let (raw, clamped) = lambda(at, sampler, &level);
            return [
                u64::from(ops::single_bits(clamped)),
                u64::from(ops::single_bits(raw)),
                0,
                0,
            ];
        }
        let reference =
            compares.then(|| depth_reference(at, operands.dref.map_or(0.0, single) / q));
        let offset = operands.offset.map(|reg| {
            let mut offset = [0u64; 3];
            for (word, slot) in (0u32..).zip(&mut offset).take(axes) {
                *slot = self.word(reg, word, lane);
            }
            offset
        });
        widen(self.sample(
            at,
            sampler,
            (point, &level),
            reference,
            offset,
            image.component,
        ))
    }

    /// The derivatives across lane `lane`'s quad of the coordinate's first
    /// `axes` components, each divided by its own lane's divisor where
    /// `projective`, for an implicit level of detail; a neighbour not
    /// running lends the lane's own.
    fn quad_grads(
        &self,
        image: &Image,
        lane: usize,
        (axes, projective): (usize, bool),
    ) -> ([f64; 4], [f64; 4]) {
        let component =
            |at: usize, word: u32| ops::float(image.coord_num, self.word(image.coord, word, at));
        let divisor = u32::try_from(axes).unwrap_or(u32::MAX);
        let value = |at: usize, word: u32| {
            let at = if self.mask >> at & 1 != 0 { at } else { lane };
            if projective {
                component(at, word) / component(at, divisor)
            } else {
                component(at, word)
            }
        };
        let mut grads = ([0.0; 4], [0.0; 4]);
        for (word, (x, y)) in (0u32..)
            .zip(grads.0.iter_mut().zip(&mut grads.1))
            .take(axes)
        {
            *x = value(lane | 1, word) - value(lane & !1, word);
            *y = value(lane | 2, word) - value(lane & !2, word);
        }
        grads
    }

    /// The texel an integer coordinate names at level `level` of the view,
    /// in sample `sample`, or a texel buffer's element.
    fn fetch_texel(
        &self,
        bound: &Bound<'_>,
        component: Component,
        coord: &[u64; 4],
        (level, sample): (u64, u64),
    ) -> Texel {
        if let Some((words, offset, size, format)) = bound.texels {
            return buffer_texel(words, (offset, size), format, coord[0], self.env.srgb);
        }
        let Some(at) = bound.image else {
            return missing(component);
        };
        match locate(&at, coord, (level, sample)) {
            Some(place) => decode_at(&at, &place, plane_of(&at), self.env.srgb),
            None => zeros(&at, self.env.srgb),
        }
    }

    fn write_texel(&self, bound: &Bound<'_>, coord: &[u64; 4], sample: u64, value: Texel) {
        if let Some((words, offset, size, format)) = bound.texels {
            let bytes = u64::from(format.texel_bytes());
            let x = coord[0] & 0xffff_ffff;
            if bytes > 0
                && x.checked_mul(bytes)
                    .is_some_and(|start| start + bytes <= size)
            {
                let raw = texel::encode(format, Plane::Colour, value, self.env.srgb);
                put_raw(words, offset + x * bytes, bytes, raw);
            }
            return;
        }
        let Some(at) = bound.image else {
            return;
        };
        if let Some(place) = locate(&at, coord, (0, sample)) {
            let raw = texel::encode(at.view.format, plane_of(&at), value, self.env.srgb);
            let byte = at.layout.texel_offset(
                place.level,
                place.layer,
                [place.x, place.y, place.z],
                place.sample,
            );
            put_raw(at.words, at.base + byte, at.layout.texel, raw);
        }
    }

    pub(super) fn texel_pointer(
        &mut self,
        dst: Reg,
        image: u32,
        handle: Reg,
        (coord, coords): (Reg, u8),
        sample: Reg,
    ) {
        let (mut element, mut offset) = ([0; LANES], [u64::from(INVALID); LANES]);
        for lane in each_lane(self.mask) {
            let held = self.word(handle, 0, lane);
            if let Some(slot) = element.get_mut(lane) {
                *slot = held;
            }
            let mut point = [0u64; 4];
            for (word, slot) in (0..u32::from(coords)).zip(&mut point) {
                *slot = self.word(coord, word, lane);
            }
            let resolved = self
                .env
                .bindings
                .get(image as usize)
                .and_then(|row| row.get(usize::try_from(held).unwrap_or(usize::MAX)));
            let byte = match resolved {
                Some(Resolved::Image(at)) => locate(at, &point, (0, self.word(sample, 0, lane)))
                    .map(|place| {
                        at.layout.texel_offset(
                            place.level,
                            place.layer,
                            [place.x, place.y, place.z],
                            place.sample,
                        )
                    }),
                Some(Resolved::Texels { size, format, .. }) => {
                    let bytes = u64::from(format.texel_bytes());
                    let x = point[0] & 0xffff_ffff;
                    x.checked_mul(bytes).filter(|start| start + bytes <= *size)
                }
                _ => None,
            };
            if let (Some(slot), Some(byte)) = (offset.get_mut(lane), byte) {
                *slot = byte.min(u64::from(INVALID));
            }
        }
        self.store(dst, 0, false, &element);
        self.store(dst, 1, false, &offset);
    }

    /// A filtered sample of the view at `point`.
    fn sample(
        &self,
        at: &ImageAt<'_>,
        sampler: Option<&SamplerDesc>,
        (point, lod): ([f64; 4], &Lod),
        reference: Option<f64>,
        offset: Option<[u64; 3]>,
        component: Component,
    ) -> Texel {
        let desc = sampler.copied().unwrap_or(SamplerDesc::LINEAR);
        let (_, lambda) = lambda(at, sampler, lod);
        let levels = at.view.range.levels.max(1);
        let d = lambda.clamp(0.0, f64::from(levels - 1));
        let integer = component != Component::Float;
        let filter = if integer {
            Filter::Nearest
        } else if lambda <= 0.0 {
            desc.mag_filter
        } else {
            desc.min_filter
        };
        let compare = reference.map(|r| (r, desc.compare.unwrap_or(CompareOp::LessOrEqual)));
        let taps = |level: u32| self.filtered(at, &desc, level, point, (filter, compare, offset));
        if desc.mipmap_filter == Filter::Linear && !integer && !desc.unnormalized && levels > 1 {
            let low = mathf::floor(d);
            let low_level = to_u32(low);
            let high_level = (low_level + 1).min(levels - 1);
            // The last level is both taps, so it is filtered once.
            let lower = taps(low_level);
            let upper = if high_level == low_level {
                lower
            } else {
                taps(high_level)
            };
            mix_texels(&lower, &upper, d - low)
        } else if desc.unnormalized {
            taps(0)
        } else {
            taps(to_u32(mathf::ceil(d + 0.5) - 1.0))
        }
    }

    /// The view filtered at one level past its base.
    fn filtered(
        &self,
        at: &ImageAt<'_>,
        desc: &SamplerDesc,
        level: u32,
        point: [f64; 4],
        (filter, compare, offset): (Filter, Option<(f64, CompareOp)>, Option<[u64; 3]>),
    ) -> Texel {
        let view = at.view;
        if matches!(
            view.dimension,
            ViewDimension::Cube | ViewDimension::CubeArray
        ) {
            return self.cube(at, (level, point), (filter, compare));
        }
        let absolute = view.range.base_level + level;
        let extent = at.desc.level_extent(absolute);
        let size = [extent.width, extent.height, extent.depth];
        let axes = spatial_axes(view.dimension);
        let offset = offset.map_or([0; 3], |o| o.map(|v| ops::signed(v, 32)));
        let plane = plane_of(at);
        let layer = layer_of(view.dimension, &point, view.range.layers);
        let scaled = |axis: usize| {
            let coordinate = point.get(axis).copied().unwrap_or(0.0);
            if desc.unnormalized {
                coordinate
            } else {
                coordinate * f64::from(size.get(axis).copied().unwrap_or(1))
            }
        };
        let wrapped = |axis: usize, i: i64| {
            let mode = desc
                .address
                .get(axis)
                .copied()
                .unwrap_or(AddressMode::ClampToEdge);
            let mode = if axis >= axes {
                AddressMode::ClampToEdge
            } else if desc.unnormalized {
                clamped(mode)
            } else {
                mode
            };
            wrap(i, size.get(axis).copied().unwrap_or(1), mode)
        };
        let read = |coords: [Option<u32>; 3]| -> Texel {
            let [Some(x), Some(y), Some(z)] = coords else {
                return border(desc.border, at, compare);
            };
            let texel = decode_at(
                at,
                &At {
                    level: absolute,
                    layer: view.range.base_layer + layer,
                    x,
                    y,
                    z,
                    sample: 0,
                },
                plane,
                self.env.srgb,
            );
            compared(texel, compare)
        };
        let mut base = [0i64; 3];
        let mut fraction = [0.0f64; 3];
        for (axis, (low, part)) in base.iter_mut().zip(&mut fraction).enumerate().take(axes) {
            let shift = offset.get(axis).copied().unwrap_or(0);
            match filter {
                Filter::Nearest => *low = floor_i64(scaled(axis)) + shift,
                Filter::Linear => {
                    let centred = scaled(axis) - 0.5;
                    let floor = mathf::floor(centred);
                    *low = held_i64(floor) + shift;
                    *part = centred - floor;
                }
            }
        }
        if filter == Filter::Nearest {
            let [x, y, z] = base;
            return read([wrapped(0, x), wrapped(1, y), wrapped(2, z)]);
        }
        // Each axis has two places, wrapped once for every corner.
        let places: [[Option<u32>; 2]; 3] = core::array::from_fn(|axis| {
            let low = base.get(axis).copied().unwrap_or(0);
            [wrapped(axis, low), wrapped(axis, low + 1)]
        });
        let mut sum = [0.0f64; 4];
        for corner in 0..1usize << axes {
            let mut weight = 1.0;
            let mut coords = places.map(|[low, _]| low);
            for (axis, ((coordinate, &part), [_, high])) in coords
                .iter_mut()
                .zip(&fraction)
                .zip(&places)
                .enumerate()
                .take(axes)
            {
                if corner >> axis & 1 == 0 {
                    weight *= 1.0 - part;
                } else {
                    weight *= part;
                    *coordinate = *high;
                }
            }
            accumulate(&mut sum, &read(coords), weight);
        }
        rounded(sum)
    }

    /// Texel `(i, j)` of the face `on` names, carried onto the face beside
    /// where it lies past an edge, compared where a reference is.
    fn cube_texel_at(
        &self,
        at: &ImageAt<'_>,
        on: &OnCube,
        (i, j): (i64, i64),
        compare: Option<(f64, CompareOp)>,
    ) -> Texel {
        let (face, x, y) = cube_texel(on.face, i, j, on.side);
        let texel = decode_at(
            at,
            &At {
                level: on.absolute,
                layer: at.view.range.base_layer + on.cube * 6 + face,
                x,
                y,
                z: 0,
                sample: 0,
            },
            plane_of(at),
            self.env.srgb,
        );
        compared(texel, compare)
    }

    /// A cube's sample: its face chosen by the direction's major axis, its
    /// texels outside the face taken from the face beside it.
    fn cube(
        &self,
        at: &ImageAt<'_>,
        (level, point): (u32, [f64; 4]),
        (filter, compare): (Filter, Option<(f64, CompareOp)>),
    ) -> Texel {
        let on = on_cube(at, level, &point);
        let texel = |i: i64, j: i64| self.cube_texel_at(at, &on, (i, j), compare);
        match filter {
            // A nearest texel is clamped to its face, never carried past it.
            Filter::Nearest => {
                let last = i64::from(on.side.max(1)) - 1;
                let at = |coordinate: f64| floor_i64(coordinate).clamp(0, last);
                texel(at(on.across), at(on.down))
            }
            Filter::Linear => {
                let (across, down) = (on.across - 0.5, on.down - 0.5);
                let (left, top) = (mathf::floor(across), mathf::floor(down));
                let (right, bottom) = (across - left, down - top);
                let (i, j) = (held_i64(left), held_i64(top));
                let mut sum = [0.0f64; 4];
                accumulate(&mut sum, &texel(i, j), (1.0 - right) * (1.0 - bottom));
                accumulate(&mut sum, &texel(i + 1, j), right * (1.0 - bottom));
                accumulate(&mut sum, &texel(i, j + 1), (1.0 - right) * bottom);
                accumulate(&mut sum, &texel(i + 1, j + 1), right * bottom);
                rounded(sum)
            }
        }
    }

    /// One channel of each texel of the view's base level footprint at
    /// `point`: `(i0, j1)`, `(i1, j1)`, `(i1, j0)`, `(i0, j0)`, each from its
    /// own offset where four are given; a cube's from its face and the faces
    /// beside it.
    fn gather(
        &self,
        at: &ImageAt<'_>,
        sampler: Option<&SamplerDesc>,
        point: [f64; 4],
        (channel, reference): (u64, Option<f64>),
        (offset, four): (Option<[u64; 3]>, Option<[[u64; 2]; 4]>),
    ) -> Texel {
        let desc = sampler.copied().unwrap_or(SamplerDesc::LINEAR);
        let compare = reference.map(|r| (r, desc.compare.unwrap_or(CompareOp::LessOrEqual)));
        let channel = if compare.is_some() {
            0
        } else {
            usize::try_from(channel).unwrap_or(0).min(3)
        };
        let view = at.view;
        if matches!(
            view.dimension,
            ViewDimension::Cube | ViewDimension::CubeArray
        ) {
            let on = on_cube(at, 0, &point);
            let (i, j) = (floor_i64(on.across - 0.5), floor_i64(on.down - 0.5));
            let texel = |i: i64, j: i64| self.cube_texel_at(at, &on, (i, j), compare)[channel];
            return [
                texel(i, j + 1),
                texel(i + 1, j + 1),
                texel(i + 1, j),
                texel(i, j),
            ];
        }
        let extent = at.desc.level_extent(view.range.base_level);
        let size = [extent.width, extent.height];
        let plane = plane_of(at);
        let layer = layer_of(view.dimension, &point, view.range.layers);
        let fetch = |coords: [i64; 2]| -> u32 {
            let mut wrapped = [0u32; 2];
            for (axis, (slot, (&i, &n))) in
                wrapped.iter_mut().zip(coords.iter().zip(&size)).enumerate()
            {
                let mode = desc
                    .address
                    .get(axis)
                    .copied()
                    .unwrap_or(AddressMode::ClampToEdge);
                match wrap(i, n, mode) {
                    Some(w) => *slot = w,
                    None => return border(desc.border, at, compare)[channel],
                }
            }
            let [x, y] = wrapped;
            let texel = decode_at(
                at,
                &At {
                    level: view.range.base_level,
                    layer: view.range.base_layer + layer,
                    x,
                    y,
                    z: 0,
                    sample: 0,
                },
                plane,
                self.env.srgb,
            );
            compared(texel, compare)[channel]
        };
        let (i, j) = (
            floor_i64(point[0] * f64::from(size[0]) - 0.5),
            floor_i64(point[1] * f64::from(size[1]) - 0.5),
        );
        let footprint = |shift: [i64; 2]| [i + shift[0], j + shift[1]];
        if let Some(offsets) = four {
            return offsets.map(|shift| fetch(footprint(shift.map(|v| ops::signed(v, 32)))));
        }
        let shift = offset.map_or([0; 2], |o| [ops::signed(o[0], 32), ops::signed(o[1], 32)]);
        let [i, j] = footprint(shift);
        [
            fetch([i, j + 1]),
            fetch([i + 1, j + 1]),
            fetch([i + 1, j]),
            fetch([i, j]),
        ]
    }
}

/// `(λ', λ)`: the level of detail before and after the sampler's clamp.
fn lambda(at: &ImageAt<'_>, sampler: Option<&SamplerDesc>, lod: &Lod) -> (f64, f64) {
    let sampler = sampler.copied().unwrap_or(SamplerDesc::LINEAR);
    if sampler.unnormalized {
        return (0.0, 0.0);
    }
    let base = lod.explicit.unwrap_or_else(|| {
        let extent = at.desc.level_extent(at.view.range.base_level);
        let size = [
            f64::from(extent.width),
            f64::from(extent.height),
            f64::from(extent.depth),
        ];
        let axes = if is_cube(at.view.dimension) {
            2
        } else {
            spatial_axes(at.view.dimension)
        };
        let scale = |d: &[f64; 4]| {
            let sum = d
                .iter()
                .zip(&size)
                .take(axes)
                .fold(0.0, |total, (rate, side)| {
                    total + (rate * side) * (rate * side)
                });
            mathf::sqrt(sum)
        };
        let (rx, ry) = (scale(&lod.grads.0), scale(&lod.grads.1));
        let (most, least) = if rx < ry { (ry, rx) } else { (rx, ry) };
        let anisotropy = f64::from(sampler.max_anisotropy.max(1));
        let probes = if least > 0.0 {
            mathf::fmin(mathf::ceil(most / least), anisotropy)
        } else {
            1.0
        };
        if most > 0.0 {
            mathf::log2(most / probes)
        } else {
            f64::from(f32::MIN)
        }
    });
    let bias = (f64::from(sampler.lod_bias) + lod.bias)
        .clamp(-f64::from(MAX_LOD_BIAS), f64::from(MAX_LOD_BIAS));
    let raw = base + bias;
    let raw = if raw.is_nan() { 0.0 } else { raw };
    (
        raw,
        raw.clamp(f64::from(sampler.lod_min), f64::from(sampler.lod_max)),
    )
}

/// The view's size at level `level` past its base, then its layers.
fn query_size(bound: &Bound<'_>, level: u64) -> [u64; 4] {
    if let Some((_, _, size, format)) = bound.texels {
        let bytes = u64::from(format.texel_bytes()).max(1);
        return [size / bytes, 0, 0, 0];
    }
    let Some(at) = bound.image else {
        return [0; 4];
    };
    let view = at.view;
    if level >= u64::from(view.range.levels) {
        return [0; 4];
    }
    let level = view.range.base_level + u32::try_from(level).unwrap_or(0);
    let extent = at.desc.level_extent(level);
    let (width, height, depth) = (
        u64::from(extent.width),
        u64::from(extent.height),
        u64::from(extent.depth),
    );
    let layers = u64::from(view.range.layers);
    match view.dimension {
        ViewDimension::D1 => [width, 0, 0, 0],
        ViewDimension::D1Array => [width, layers, 0, 0],
        ViewDimension::D2 | ViewDimension::Cube => [width, height, 0, 0],
        ViewDimension::D2Array => [width, height, layers, 0],
        ViewDimension::CubeArray => [width, height, layers / 6, 0],
        ViewDimension::D3 => [width, height, depth, 0],
    }
}

/// Where `point`'s direction meets a cube view at `level` past its base.
fn on_cube(at: &ImageAt<'_>, level: u32, point: &[f64; 4]) -> OnCube {
    let view = at.view;
    let absolute = view.range.base_level + level;
    let side = at.desc.level_extent(absolute).width.max(1);
    let cube = if view.dimension == ViewDimension::CubeArray {
        to_u32(
            mathf::round_even(point[3]).clamp(0.0, f64::from((view.range.layers / 6).max(1) - 1)),
        )
    } else {
        0
    };
    let (face, s, t) = cube_face(point[0], point[1], point[2]);
    OnCube {
        absolute,
        cube,
        face,
        side,
        across: s * f64::from(side),
        down: t * f64::from(side),
    }
}

/// `texel`'s channels, as floats, weighted into `sum`.
fn accumulate(sum: &mut [f64; 4], texel: &Texel, weight: f64) {
    for (total, &bits) in sum.iter_mut().zip(texel) {
        *total += weight * f64::from(f32::from_bits(bits));
    }
}

/// A weighted sum of texels, each channel rounded once to a single.
fn rounded(sum: [f64; 4]) -> Texel {
    sum.map(ops::single_bits)
}

/// The level of detail a sample is given or takes.
struct Lod {
    explicit: Option<f64>,
    bias: f64,
    grads: ([f64; 4], [f64; 4]),
}

/// The plane a view reads: its depth or stencil, or its colour.
fn plane_of(at: &ImageAt<'_>) -> Plane {
    match at.view.range.aspects {
        a if a == Aspects::DEPTH => Plane::Depth,
        a if a == Aspects::STENCIL => Plane::Stencil,
        _ => Plane::Colour,
    }
}

/// The place an integer coordinate names at `level` past the view's base,
/// in `sample`; `None` past the view.
fn locate(at: &ImageAt<'_>, coord: &[u64; 4], (level, sample): (u64, u64)) -> Option<At> {
    let view = at.view;
    if level >= u64::from(view.range.levels) || sample >= u64::from(at.desc.samples) {
        return None;
    }
    let level = view.range.base_level + u32::try_from(level).ok()?;
    let extent = at.desc.level_extent(level);
    let c = |w: usize| u32::try_from(coord.get(w).copied().unwrap_or(0) & 0xffff_ffff).ok();
    let (x, y, z, layer) = match view.dimension {
        ViewDimension::D1 => (c(0)?, 0, 0, 0),
        ViewDimension::D1Array => (c(0)?, 0, 0, c(1)?),
        ViewDimension::D2 => (c(0)?, c(1)?, 0, 0),
        ViewDimension::D2Array | ViewDimension::Cube | ViewDimension::CubeArray => {
            (c(0)?, c(1)?, 0, c(2)?)
        }
        ViewDimension::D3 => (c(0)?, c(1)?, c(2)?, 0),
    };
    if x >= extent.width || y >= extent.height || z >= extent.depth || layer >= view.range.layers {
        return None;
    }
    Some(At {
        level,
        layer: view.range.base_layer + layer,
        x,
        y,
        z,
        sample: u32::try_from(sample).ok()?,
    })
}

/// The texel at `site`, decoded as the view reads it.
fn decode_at(at: &ImageAt<'_>, site: &At, plane: Plane, srgb: &texel::Srgb) -> Texel {
    let byte = at.layout.texel_offset(
        site.level,
        site.layer,
        [site.x, site.y, site.z],
        site.sample,
    );
    let raw = raw_at(at.words, at.base + byte, at.layout.texel);
    texel::decode(at.view.format, plane, raw, srgb)
}

/// What a texel of zeros decodes to through the view: what a read past it
/// answers.
fn zeros(at: &ImageAt<'_>, srgb: &texel::Srgb) -> Texel {
    texel::decode(at.view.format, plane_of(at), [0; 4], srgb)
}

/// Element `x` of a texel buffer of `format` over `size` bytes from byte
/// `offset`, zeros past it.
fn buffer_texel(
    words: &[AtomicU32],
    (offset, size): (u64, u64),
    format: Format,
    x: u64,
    srgb: &texel::Srgb,
) -> Texel {
    let bytes = u64::from(format.texel_bytes());
    let x = x & 0xffff_ffff;
    let raw = match x.checked_mul(bytes) {
        Some(start) if bytes > 0 && start + bytes <= size => raw_at(words, offset + start, bytes),
        _ => [0; 4],
    };
    texel::decode(format, Plane::Colour, raw, srgb)
}

/// How many of a view's coordinates address within a layer.
const fn spatial_axes(dimension: ViewDimension) -> usize {
    match dimension {
        ViewDimension::D1 | ViewDimension::D1Array => 1,
        ViewDimension::D2 | ViewDimension::D2Array => 2,
        ViewDimension::Cube | ViewDimension::CubeArray | ViewDimension::D3 => 3,
    }
}

/// An arrayed view's layer, its coordinate rounded to even and held to its
/// layers.
fn layer_of(dimension: ViewDimension, point: &[f64; 4], layers: u32) -> u32 {
    let coordinate = match dimension {
        ViewDimension::D1Array => point[1],
        ViewDimension::D2Array => point[2],
        _ => return 0,
    };
    to_u32(mathf::round_even(coordinate).clamp(0.0, f64::from(layers.max(1) - 1)))
}

/// `i` wrapped into `0..n` as `mode` addresses it; `None` for the border.
fn wrap(i: i64, n: u32, mode: AddressMode) -> Option<u32> {
    let n = i64::from(n.max(1));
    let mirror = |a: i64| if a >= 0 { a } else { -(1 + a) };
    let wrapped = match mode {
        AddressMode::Repeat => i.rem_euclid(n),
        AddressMode::MirroredRepeat => (n - 1) - mirror(i.rem_euclid(2 * n) - n),
        AddressMode::ClampToEdge => i.clamp(0, n - 1),
        AddressMode::MirrorClampToEdge => mirror(i).clamp(0, n - 1),
        AddressMode::ClampToBorder => {
            if (0..n).contains(&i) {
                i
            } else {
                return None;
            }
        }
    };
    u32::try_from(wrapped).ok()
}

/// What unnormalised coordinates address with: clamped, to the edge or the
/// border.
const fn clamped(mode: AddressMode) -> AddressMode {
    match mode {
        AddressMode::ClampToBorder => AddressMode::ClampToBorder,
        _ => AddressMode::ClampToEdge,
    }
}

/// The border colour as a texel of the view's channels: as many of the
/// colour's as the view's format holds, in what they read as, and the rest
/// what a texel lacks; compared where a depth reference is.
fn border(color: BorderColor, at: &ImageAt<'_>, compare: Option<(f64, CompareOp)>) -> Texel {
    let (component, held) = match plane_of(at) {
        Plane::Depth => (Component::Float, 1),
        Plane::Stencil => (Component::Uint, 1),
        Plane::Colour => (at.view.format.component(), at.view.format.channels()),
    };
    let one = if component == Component::Float {
        ONE
    } else {
        1
    };
    let painted = match color {
        BorderColor::TransparentBlack => [0; 4],
        BorderColor::OpaqueBlack => [0, 0, 0, one],
        BorderColor::OpaqueWhite => [one; 4],
    };
    let mut texel = missing(component);
    for (slot, &value) in texel.iter_mut().zip(&painted).take(held as usize) {
        *slot = value;
    }
    compared(texel, compare)
}

/// A depth reference as a comparison against the view's format takes it:
/// held to `[0, 1]` for a normalized format, as is for a float one.
fn depth_reference(at: &ImageAt<'_>, reference: f64) -> f64 {
    if at.view.format == Format::Depth16Unorm {
        reference.clamp(0.0, 1.0)
    } else {
        reference
    }
}

/// A texel's depth compared against a reference: one where it passes and
/// zero where not, in its first channel; the texel itself where there is no
/// reference.
fn compared(texel: Texel, compare: Option<(f64, CompareOp)>) -> Texel {
    match compare {
        Some((reference, op)) => {
            let held = f64::from(f32::from_bits(texel[0]));
            let pass = op.passes(reference, held);
            [if pass { ONE } else { 0 }, 0, 0, ONE]
        }
        None => texel,
    }
}

/// `low` moved `toward` the way to `high`, channel by channel.
fn mix_texels(low: &Texel, high: &Texel, toward: f64) -> Texel {
    core::array::from_fn(|channel| {
        let from = f64::from(f32::from_bits(low[channel]));
        let to = f64::from(f32::from_bits(high[channel]));
        ops::single_bits(from + (to - from) * toward)
    })
}

/// The face of a cube the direction `(x, y, z)` meets: its major axis and
/// that axis's sign.
fn major_face(x: f64, y: f64, z: f64) -> u32 {
    let (ax, ay, az) = (x.abs(), y.abs(), z.abs());
    let (axis, value) = if ax >= ay && ax >= az {
        (0, x)
    } else if ay >= az {
        (2, y)
    } else {
        (4, z)
    };
    if value >= 0.0 {
        axis
    } else {
        axis + 1
    }
}

/// A direction's components on cube face `face`: along the face's `s` and
/// `t`, and along its major axis, signed so a direction meeting the face is
/// positive there.
fn on_face(face: u32, [x, y, z]: [f64; 3]) -> [f64; 3] {
    match face {
        0 => [-z, -y, x],
        1 => [z, -y, -x],
        2 => [x, z, y],
        3 => [x, -z, -y],
        4 => [x, -y, z],
        _ => [-x, -y, -z],
    }
}

/// A cube's face for the direction `(x, y, z)`, and the direction's place
/// on it in `0..=1` along each of its axes.
fn cube_face(x: f64, y: f64, z: f64) -> (u32, f64, f64) {
    let face = major_face(x, y, z);
    let [sc, tc, ma] = on_face(face, [x, y, z]);
    let ma = if ma > 0.0 { ma } else { 1.0 };
    (
        face,
        f64::midpoint(sc / ma, 1.0),
        f64::midpoint(tc / ma, 1.0),
    )
}

/// The derivatives `d` of the cube direction `r` carried onto the face `r`
/// meets, along its `s` and `t`: the place on a face is a quotient of the
/// direction's components, and changes as both of them do.
fn face_derivatives(r: [f64; 3], d: [f64; 3]) -> [f64; 2] {
    let face = major_face(r[0], r[1], r[2]);
    let [sc, tc, ma] = on_face(face, r);
    let [dsc, dtc, dma] = on_face(face, d);
    if ma <= 0.0 || ma.is_nan() {
        return [0.0; 2];
    }
    let along = |c: f64, dc: f64| 0.5 * (dc * ma - c * dma) / (ma * ma);
    [along(sc, dsc), along(tc, dtc)]
}

/// Whether a view of `dimension` is a cube's, or an array of them.
const fn is_cube(dimension: ViewDimension) -> bool {
    matches!(dimension, ViewDimension::Cube | ViewDimension::CubeArray)
}

/// The texel `(i, j)` of `face`, a face `side` square, carried onto the
/// face its centre's direction meets where it lies past an edge.
fn cube_texel(face: u32, i: i64, j: i64, side: u32) -> (u32, u32, u32) {
    let last = i64::from(side.max(1)) - 1;
    let within = |index: i64| u32::try_from(index.clamp(0, last)).unwrap_or(0);
    if (0..=last).contains(&i) && (0..=last).contains(&j) {
        return (face, within(i), within(j));
    }
    let width = f64::from(side.max(1));
    // A footprint reaches one texel past the face at most.
    let axis = |index: i64| {
        let index = i32::try_from(index.clamp(-1, last + 1)).unwrap_or(0);
        (2.0 * (f64::from(index) + 0.5)) / width - 1.0
    };
    let (sc, tc) = (axis(i), axis(j));
    let direction = match face {
        0 => [1.0, -tc, -sc],
        1 => [-1.0, -tc, sc],
        2 => [sc, 1.0, tc],
        3 => [sc, -1.0, -tc],
        4 => [sc, -tc, 1.0],
        _ => [-sc, -tc, -1.0],
    };
    let (beside, across, down) = cube_face(direction[0], direction[1], direction[2]);
    (
        beside,
        within(floor_i64(across * width)),
        within(floor_i64(down * width)),
    )
}

fn widen(texel: Texel) -> [u64; 4] {
    texel.map(u64::from)
}

fn floor_i64(x: f64) -> i64 {
    held_i64(mathf::floor(x))
}

/// `floored`, already a whole number, held to an `i64` well within it.
#[allow(
    clippy::cast_possible_truncation,
    reason = "the value is a whole number, held to an i64"
)]
fn held_i64(floored: f64) -> i64 {
    if floored >= 9.0e18 {
        i64::MAX / 4
    } else if floored <= -9.0e18 {
        i64::MIN / 4
    } else {
        floored as i64
    }
}

#[allow(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "the value was held to a level or layer count"
)]
fn to_u32(x: f64) -> u32 {
    if x.is_nan() || x <= 0.0 {
        0
    } else if x >= f64::from(u32::MAX) {
        u32::MAX
    } else {
        x as u32
    }
}

#[cfg(test)]
#[path = "image_tests.rs"]
mod tests;
