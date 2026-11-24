//! What a pipeline's shaders reach and how: bind set layouts, the bind sets
//! made whole against them, pipeline layouts, and shader modules.

use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt;
use core::ops::Range;

use tairix_spirv::enumerant::{Dim, ImageFormat};
use tairix_spirv::{
    Access, Admitted, Binding, BindingKind, Component, EntryPoint, ImageShape, Layout, SampleForms,
    Stages,
};

use crate::backend::Backend;
use crate::device::{Buffer, Context, Shared};
use crate::format::{Aspects, Format, FormatFeatures};
use crate::host::{owned, Host, Object};
use crate::image::{ImageUsage, ViewDimension};
use crate::info::{BufferUsage, DeviceInfo, Features};
use tairix_util::fallible;

use crate::memory::{overlaps, Charge};
use crate::model::{
    exhausted, shared_bytes, BindSetKey, BindSetLayoutKey, BufferKey, BufferState, BufferViewKey,
    ContextKey, ContextState, Model, PipelineLayoutKey, SamplerKey, ShaderModuleKey, ViewKey,
};
use crate::objects::{BufferView, ImageView, Sampler};
use crate::resource::{BufferViewState, SamplerState, ViewState};
use crate::sampler::{Filter, SamplerDesc};
use crate::Error;

/// What a sampled image binding's texels are, and whether its samplers may
/// filter them.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum SampleType {
    /// Float colour of a format the device filters.
    Float,
    /// Float colour, read through samplers that do not filter.
    UnfilterableFloat,
    /// Depth of a format the device filters, compared or read.
    Depth,
    /// Depth, compared or read through samplers that do not filter.
    UnfilterableDepth,
    /// Signed integer colour.
    Sint,
    /// Unsigned integer colour, or a stencil plane.
    Uint,
}

impl SampleType {
    /// What a module reads its texels as.
    const fn component(self) -> Component {
        match self {
            Self::Float | Self::UnfilterableFloat | Self::Depth | Self::UnfilterableDepth => {
                Component::Float
            }
            Self::Sint => Component::Sint,
            Self::Uint => Component::Uint,
        }
    }

    /// Whether a view bound to it is of a format the device filters.
    const fn filtered(self) -> bool {
        matches!(self, Self::Float | Self::Depth)
    }

    /// Whether a view bound to it sees depth.
    const fn depth(self) -> bool {
        matches!(self, Self::Depth | Self::UnfilterableDepth)
    }
}

/// What a sampler binding's samplers do.
#[derive(Copy, Clone, Debug, Eq, PartialEq, Hash)]
pub enum SamplerType {
    /// Read float colour or depth, filtered or not.
    Filtering,
    /// Read float colour or depth, every filter nearest.
    NonFiltering,
    /// Read integer texels, every filter nearest, the border read as integers.
    Integer,
    /// Compare depth, filtered or not.
    Comparison,
    /// Compare depth, every filter nearest.
    NonFilteringComparison,
    /// Read texels at unnormalised coordinates: a combined image sampler's
    /// alone, sampled at an explicit level of detail with no offset, bias,
    /// projection or gather.
    Unnormalized,
}

impl SamplerType {
    /// Whether it samples texels of `sample`.
    const fn samples(self, sample: SampleType) -> bool {
        use SampleType as S;
        match self {
            Self::Filtering => matches!(sample, S::Float | S::Depth),
            Self::NonFiltering => matches!(
                sample,
                S::Float | S::UnfilterableFloat | S::Depth | S::UnfilterableDepth
            ),
            Self::Integer => matches!(sample, S::Sint | S::Uint),
            Self::Comparison => matches!(sample, S::Depth),
            Self::NonFilteringComparison => matches!(sample, S::Depth | S::UnfilterableDepth),
            Self::Unnormalized => true,
        }
    }

    /// Whether a module may sample through it in `forms`: a comparing
    /// sampler compares and every other reads, and an unnormalised one
    /// reads only at an explicit level with no offset, bias, projection or
    /// gather.
    fn takes(self, forms: SampleForms) -> bool {
        let compares = matches!(self, Self::Comparison | Self::NonFilteringComparison);
        let other = if compares {
            SampleForms::READ
        } else {
            SampleForms::COMPARE
        };
        let unnormalized = SampleForms::IMPLICIT
            .with(SampleForms::PROJECTIVE)
            .with(SampleForms::BIAS)
            .with(SampleForms::OFFSET)
            .with(SampleForms::GATHER);
        !forms.intersects(other) && (self != Self::Unnormalized || !forms.intersects(unnormalized))
    }

    /// Whether `desc` is a sampler of it.
    fn admits(self, desc: &SamplerDesc) -> bool {
        let nearest = [desc.mag_filter, desc.min_filter, desc.mipmap_filter]
            .iter()
            .all(|&filter| filter == Filter::Nearest);
        let normalized = !desc.unnormalized;
        match self {
            Self::Filtering => desc.compare.is_none() && normalized,
            Self::NonFiltering | Self::Integer => desc.compare.is_none() && normalized && nearest,
            Self::Comparison => desc.compare.is_some() && normalized,
            Self::NonFilteringComparison => desc.compare.is_some() && normalized && nearest,
            Self::Unnormalized => desc.unnormalized,
        }
    }
}

/// What a binding holds, and for an image sampled through a sampler, how.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub enum DescriptorType {
    /// A sampler.
    Sampler(SamplerType),
    /// An image of the shape a module declares, for sampling.
    SampledImage(ImageShape, SampleType),
    /// An image of the shape a module declares, read and written by texel.
    StorageImage(ImageShape),
    /// An image of the shape a module declares and the sampler it is
    /// sampled with.
    CombinedImageSampler(ImageShape, SampleType, SamplerType),
    /// A buffer of texels read through a format.
    UniformTexelBuffer(ImageShape),
    /// A buffer of texels read and written through a format.
    StorageTexelBuffer(ImageShape),
    /// A uniform buffer.
    UniformBuffer,
    /// A storage buffer.
    StorageBuffer,
}

impl DescriptorType {
    /// What a module declares for a binding of it.
    #[must_use]
    pub const fn kind(self) -> BindingKind {
        match self {
            Self::Sampler(_) => BindingKind::Sampler,
            Self::SampledImage(shape, _) => BindingKind::SampledImage(shape),
            Self::StorageImage(shape) => BindingKind::StorageImage(shape),
            Self::CombinedImageSampler(shape, ..) => BindingKind::CombinedImageSampler(shape),
            Self::UniformTexelBuffer(shape) => BindingKind::UniformTexelBuffer(shape),
            Self::StorageTexelBuffer(shape) => BindingKind::StorageTexelBuffer(shape),
            Self::UniformBuffer => BindingKind::UniformBuffer,
            Self::StorageBuffer => BindingKind::StorageBuffer,
        }
    }

    /// The texels an image sampled through a sampler is of.
    const fn sample(self) -> Option<SampleType> {
        match self {
            Self::SampledImage(_, sample) | Self::CombinedImageSampler(_, sample, _) => {
                Some(sample)
            }
            _ => None,
        }
    }

    /// What its samplers do.
    const fn sampler(self) -> Option<SamplerType> {
        match self {
            Self::Sampler(sampler) | Self::CombinedImageSampler(_, _, sampler) => Some(sampler),
            _ => None,
        }
    }

    /// Whether it holds what its sampling says it does: an image's texels
    /// what its shape reads, unfiltered where it is multisampled; a combined
    /// sampler's of a type its image's texels take, its unnormalised
    /// coordinates over one row or rectangle; and no separate sampler's
    /// unnormalised.
    fn sampled_as_shaped(self) -> bool {
        let image = |shape: ImageShape, sample: SampleType| {
            sample.component() == shape.component && !(shape.multisampled && sample.filtered())
        };
        match self {
            Self::Sampler(sampler) => sampler != SamplerType::Unnormalized,
            Self::SampledImage(shape, sample) => image(shape, sample),
            Self::CombinedImageSampler(shape, sample, sampler) => {
                let plain = matches!(shape.dim, Dim::DIM_1D | Dim::DIM_2D)
                    && !shape.arrayed
                    && !shape.multisampled;
                image(shape, sample)
                    && sampler.samples(sample)
                    && (sampler != SamplerType::Unnormalized || plain)
            }
            _ => true,
        }
    }
}

/// One binding of a bind set layout.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct LayoutBinding {
    /// Its number within the set.
    pub binding: u32,
    /// What it holds; an image binding states the shape a module declares.
    pub kind: DescriptorType,
    /// How many descriptors it holds.
    pub count: u32,
    /// How the shaders it is bound for may access it.
    pub access: Access,
    /// The stages that see it.
    pub stages: Stages,
}

impl LayoutBinding {
    /// The binding providing what `binding` of a module's entry point needs,
    /// seen by `stages`; `None` for a sampler or an image sampled through
    /// one, whose sample and sampler types no module states.
    #[must_use]
    pub fn unsampled(binding: &Binding, stages: Stages) -> Option<Self> {
        let kind = match binding.kind {
            BindingKind::StorageImage(shape) => DescriptorType::StorageImage(shape),
            BindingKind::UniformTexelBuffer(shape) => DescriptorType::UniformTexelBuffer(shape),
            BindingKind::StorageTexelBuffer(shape) => DescriptorType::StorageTexelBuffer(shape),
            BindingKind::UniformBuffer => DescriptorType::UniformBuffer,
            BindingKind::StorageBuffer => DescriptorType::StorageBuffer,
            BindingKind::Sampler
            | BindingKind::SampledImage(_)
            | BindingKind::CombinedImageSampler(_) => return None,
        };
        Some(Self {
            binding: binding.binding,
            kind,
            count: binding.count,
            access: binding.access,
            stages,
        })
    }

    /// Refuse a binding of no descriptors or stages, past the device's
    /// binding numbers, of more than one descriptor where the device takes no
    /// arrays of them, or of an access, shape or sampling its kind does not
    /// take.
    fn check(&self, device: &DeviceInfo) -> Result<(), Error> {
        if self.count == 0 || self.stages == Stages::NONE {
            return Err(Error::Empty);
        }
        if self.binding >= device.limits.max_binding_number {
            return Err(Error::OutOfRange);
        }
        if self.count > 1 && !device.features.contains(Features::BINDING_ARRAYS) {
            return Err(Error::Unsupported);
        }
        let kind = self.kind.kind();
        let allowed = if writable(kind) {
            Access::READ.with(Access::WRITE)
        } else {
            Access::READ
        };
        let shaped = match kind {
            BindingKind::Sampler | BindingKind::UniformBuffer | BindingKind::StorageBuffer => true,
            BindingKind::SampledImage(shape) | BindingKind::CombinedImageSampler(shape) => {
                image_shape(shape, true)
            }
            BindingKind::StorageImage(shape) => image_shape(shape, false) && storage_format(shape),
            BindingKind::UniformTexelBuffer(shape) => texel_shape(shape),
            BindingKind::StorageTexelBuffer(shape) => texel_shape(shape) && storage_format(shape),
        };
        if shaped
            && self.kind.sampled_as_shaped()
            && Stages::ALL.contains(self.stages)
            && allowed.contains(self.access)
        {
            Ok(())
        } else {
            Err(Error::Shape)
        }
    }
}

const fn writable(kind: BindingKind) -> bool {
    matches!(
        kind,
        BindingKind::StorageImage(_)
            | BindingKind::StorageTexelBuffer(_)
            | BindingKind::StorageBuffer
    )
}

/// Whether an image binding's shape is one an admitted module declares: no
/// three-dimensional image has layers, and only a sampled rectangle may be
/// multisampled.
fn image_shape(shape: ImageShape, sampled: bool) -> bool {
    let dim = matches!(
        shape.dim,
        Dim::DIM_1D | Dim::DIM_2D | Dim::DIM_3D | Dim::CUBE
    );
    let layered = !shape.arrayed || shape.dim != Dim::DIM_3D;
    let multisampling = !shape.multisampled || (sampled && shape.dim == Dim::DIM_2D);
    dim && layered && multisampling
}

fn texel_shape(shape: ImageShape) -> bool {
    shape.dim == Dim::BUFFER && !shape.arrayed && !shape.multisampled
}

/// Whether a storage binding's format is unknown, or one a format's channels
/// read as the binding says.
fn storage_format(shape: ImageShape) -> bool {
    shape.format == ImageFormat::UNKNOWN
        || Format::ALL.iter().any(|format| {
            format.storage_format() == Some(shape.format) && format.component() == shape.component
        })
}

/// Whether a view of shape `have` serves a binding of shape `need`: the same
/// dimension, layers, samples and channels, and for storage the binding's
/// format unless it names none.
fn serves(have: ImageShape, need: ImageShape, storage: bool) -> bool {
    let format = !storage || need.format == ImageFormat::UNKNOWN || need.format == have.format;
    have.dim == need.dim
        && have.arrayed == need.arrayed
        && have.multisampled == need.multisampled
        && have.component == need.component
        && format
}

/// Whether a format of `features` is accessed as `access` through a storage
/// binding of `shape`: one naming no format reads it only where it offers
/// `read`, and writes it only where it offers `write`.
fn accessed(
    features: FormatFeatures,
    shape: ImageShape,
    access: Access,
    [read, write]: [FormatFeatures; 2],
) -> bool {
    let offers = |accessed, feature| !access.contains(accessed) || features.contains(feature);
    shape.format != ImageFormat::UNKNOWN
        || (offers(Access::READ, read) && offers(Access::WRITE, write))
}

/// A bind set layout: its bindings, by ascending number.
#[derive(Debug)]
pub(crate) struct BindSetLayoutState {
    pub(crate) bindings: Vec<LayoutBinding>,
    /// Where each binding's first descriptor sits among a bind set's.
    pub(crate) firsts: Vec<u64>,
    /// How many descriptors a bind set of it holds.
    descriptors: u64,
    _charge: Charge,
}

impl BindSetLayoutState {
    /// The place of binding `binding` among the layout's, and where its
    /// descriptors start.
    pub(crate) fn find(&self, binding: u32) -> Option<(usize, u64)> {
        let at = self
            .bindings
            .binary_search_by_key(&binding, |b| b.binding)
            .ok()?;
        Some((at, *self.firsts.get(at)?))
    }

    /// Whether a bind set of this layout serves one of `other`'s: the same
    /// bindings, whichever object declared them.
    pub(crate) fn serves(self: &Arc<Self>, other: &Arc<Self>) -> bool {
        Arc::ptr_eq(self, other) || self.bindings == other.bindings
    }
}

/// One descriptor of a bind set: what it binds, held for as long as the set
/// is.
#[derive(Debug)]
pub(crate) enum Bound<B: Backend> {
    Sampler(Arc<SamplerState<B>>),
    Image(Arc<ViewState<B>>),
    ImageSampler(Arc<ViewState<B>>, Arc<SamplerState<B>>),
    TexelBuffer(Arc<BufferViewState<B>>),
    /// `size` bytes from `offset` into the buffer.
    Buffer {
        buffer: Arc<BufferState<B>>,
        offset: u64,
        size: u64,
    },
}

impl<B: Backend> Bound<B> {
    /// The memory it reaches: none for a sampler.
    pub(crate) fn memory(&self) -> Option<&Arc<B::Memory>> {
        match self {
            Self::Buffer { buffer, .. } => Some(&buffer.memory),
            Self::TexelBuffer(texels) => Some(&texels.buffer.memory),
            Self::Image(view) | Self::ImageSampler(view, _) => Some(&view.image.memory),
            Self::Sampler(_) => None,
        }
    }

    /// Where it lies, by memory: the bytes of a buffer, the views of an
    /// image; nothing for a sampler.
    fn lies(&self) -> Option<Lies<'_, B>> {
        match self {
            Self::Buffer {
                buffer,
                offset,
                size,
            } => {
                let start = buffer.offset + offset;
                Some(Lies::Bytes(identity(&buffer.memory), start..start + size))
            }
            Self::TexelBuffer(texels) => {
                let start = texels.buffer.offset + texels.desc.offset;
                Some(Lies::Bytes(
                    identity(&texels.buffer.memory),
                    start..start + texels.desc.size,
                ))
            }
            Self::Image(view) | Self::ImageSampler(view, _) => Some(Lies::View(view)),
            Self::Sampler(_) => None,
        }
    }
}

/// Where a descriptor lies.
enum Lies<'d, B: Backend> {
    /// A span of a memory, by the memory's identity.
    Bytes(usize, Range<u64>),
    /// A view of an image.
    View(&'d ViewState<B>),
}

/// The identity a bind set's reach and a pass's attachments order a memory
/// or an image by: where it lies, which nothing else does while it is held.
pub(crate) fn identity<T>(held: &Arc<T>) -> usize {
    Arc::as_ptr(held).addr()
}

/// A bind set: every descriptor of its layout, binding by binding in the
/// layout's order, the descriptors as its backend reads them, and what its
/// descriptors a draw's stages see reach.
#[derive(Debug)]
pub(crate) struct BindSetState<B: Backend> {
    pub(crate) layout: Arc<BindSetLayoutState>,
    pub(crate) resources: Vec<Bound<B>>,
    pub(crate) form: B::Set,
    pub(crate) reach: Reach,
    _charge: Charge,
}

/// What a pass's attachment holds of its image: its image's identity and
/// placement in its memory, and the planes, level and layer it is.
#[derive(Clone, Debug)]
pub(crate) struct Touched {
    pub(crate) memory: usize,
    pub(crate) placement: Range<u64>,
    pub(crate) image: usize,
    pub(crate) aspects: Aspects,
    pub(crate) level: u32,
    pub(crate) layer: u32,
}

impl Touched {
    /// Whether it and `other` share a texel: of one image, a plane, level
    /// and layer; of two, where their placements meet, since no backend
    /// says where in its placement an image's texels lie.
    pub(crate) fn meets(&self, other: &Self) -> bool {
        if self.image == other.image {
            self.aspects.intersects(other.aspects)
                && self.level == other.level
                && self.layer == other.layer
        } else {
            self.memory == other.memory && overlaps(&self.placement, &other.placement)
        }
    }

    /// Whether `bytes` of the memory `memory` meet its image's placement.
    pub(crate) fn meets_bytes(&self, memory: usize, bytes: &Range<u64>) -> bool {
        self.memory == memory && overlaps(&self.placement, bytes)
    }
}

/// What a bind set's descriptors that a vertex or fragment stage sees reach:
/// those all such descriptors reach, and those reached by descriptors those
/// stages may write. A pass refuses a draw whose sets reach what it renders
/// to, at a cost of the reach's logarithm.
#[derive(Debug, Default)]
pub(crate) struct Reach {
    any: Reached,
    written: Reached,
}

/// What some descriptors reach, independent of any backend's texel layout:
/// the bytes of buffers, merged into disjoint spans by memory and address;
/// each memory's image placements, merged into disjoint runs, a run's one
/// image named where it holds only one; and the subresources each image's
/// views see, merged into disjoint spans of layers by image, plane and level.
#[derive(Debug, Default)]
struct Reached {
    bytes: Vec<(usize, Range<u64>)>,
    runs: Vec<Run>,
    seen: Vec<Seen>,
}

/// A run of image placements in one memory.
#[derive(Clone, Debug)]
struct Run {
    memory: usize,
    span: Range<u64>,
    lone: Option<usize>,
}

/// The layers of one plane and level of an image that views see.
#[derive(Clone, Debug)]
struct Seen {
    image: usize,
    aspect: u32,
    level: u32,
    layers: Range<u32>,
}

impl Reach {
    /// What `resources`, laid out as `layout` lays them out, reach.
    fn of<'r, B: Backend>(
        layout: &BindSetLayoutState,
        resources: &'r [Bound<B>],
    ) -> Result<Self, Error> {
        let seen = Stages::VERTEX.with(Stages::FRAGMENT);
        let walk = |each: &mut dyn FnMut(bool, Lies<'r, B>)| -> Result<(), Error> {
            for (binding, &first) in layout.bindings.iter().zip(&layout.firsts) {
                if !binding.stages.intersects(seen) {
                    continue;
                }
                let first = usize::try_from(first).map_err(exhausted)?;
                let held = first..first.saturating_add(binding.count as usize);
                let bound = resources.get(held).ok_or(Error::Malformed)?;
                let writes = binding.access.contains(Access::WRITE);
                for lies in bound.iter().filter_map(Bound::lies) {
                    each(writes, lies);
                }
            }
            Ok(())
        };
        let mut count = 0usize;
        walk(&mut |_, _| count += 1)?;
        let mut lies: Vec<(bool, Lies<'r, B>)> = Vec::new();
        lies.try_reserve_exact(count).map_err(exhausted)?;
        walk(&mut |writes, lie| {
            // Counted above, so within what is reserved.
            if lies.len() < lies.capacity() {
                lies.push((writes, lie));
            }
        })?;
        Ok(Self {
            any: Reached::of(lies.iter().map(|(_, lie)| lie))?,
            written: Reached::of(
                lies.iter()
                    .filter(|(writes, _)| *writes)
                    .map(|(_, lie)| lie),
            )?,
        })
    }

    /// Whether a descriptor a draw's stages see reaches `touched` — where
    /// `written`, one they may write.
    pub(crate) fn meets(&self, touched: &Touched, written: bool) -> bool {
        if written {
            self.written.meets(touched)
        } else {
            self.any.meets(touched)
        }
    }

    /// The host bytes it keeps.
    fn held_bytes(&self) -> u64 {
        self.any
            .held_bytes()
            .saturating_add(self.written.held_bytes())
    }
}

impl Reached {
    fn of<'l, 'd: 'l, B: Backend>(
        lies: impl Iterator<Item = &'l Lies<'d, B>> + Clone,
    ) -> Result<Self, Error> {
        let (mut spans, mut views, mut levels) = (0usize, 0usize, 0usize);
        for lie in lies.clone() {
            match lie {
                Lies::Bytes(..) => spans += 1,
                Lies::View(view) => {
                    views += 1;
                    let range = &view.desc.range;
                    let planes = range.aspects.bits().count_ones() as usize;
                    levels = levels.saturating_add(planes.saturating_mul(range.levels as usize));
                }
            }
        }
        let mut bytes: Vec<(usize, Range<u64>)> = Vec::new();
        bytes.try_reserve_exact(spans).map_err(exhausted)?;
        let mut runs: Vec<Run> = Vec::new();
        runs.try_reserve_exact(views).map_err(exhausted)?;
        let mut seen: Vec<Seen> = Vec::new();
        seen.try_reserve_exact(levels).map_err(exhausted)?;
        for lie in lies {
            match lie {
                Lies::Bytes(memory, span) => bytes.push((*memory, span.clone())),
                Lies::View(view) => {
                    let image = &view.image;
                    runs.push(Run {
                        memory: identity(&image.memory),
                        span: image.offset..image.offset.saturating_add(image.size),
                        lone: Some(identity(image)),
                    });
                    let range = &view.desc.range;
                    let layers = range.base_layer..range.base_layer.saturating_add(range.layers);
                    let levels = range.base_level..range.base_level.saturating_add(range.levels);
                    for plane in range.aspects.planes() {
                        for level in levels.clone() {
                            seen.push(Seen {
                                image: identity(image),
                                aspect: plane.bits(),
                                level,
                                layers: layers.clone(),
                            });
                        }
                    }
                }
            }
        }
        Self::folded(bytes, runs, seen)
    }

    /// What `bytes`, `runs` and `seen` hold, each list merged where its parts
    /// meet: spans of one memory that overlap or touch, placements of one
    /// memory that overlap, keeping one image's name only while its run holds
    /// no other, and layers of one image's plane and level that overlap or
    /// touch.
    fn folded(
        bytes: Vec<(usize, Range<u64>)>,
        runs: Vec<Run>,
        seen: Vec<Seen>,
    ) -> Result<Self, Error> {
        Ok(Self {
            bytes: merged(
                bytes,
                |(memory, span)| (*memory, span.start),
                |held, next| held.0 == next.0 && next.1.start <= held.1.end,
                |held, next| held.1.end = held.1.end.max(next.1.end),
            )?,
            runs: merged(
                runs,
                |run| (run.memory, run.span.start),
                |held, next| held.memory == next.memory && next.span.start < held.span.end,
                |held, next| {
                    held.span.end = held.span.end.max(next.span.end);
                    if held.lone != next.lone {
                        held.lone = None;
                    }
                },
            )?,
            seen: merged(
                seen,
                |seen| (seen.image, seen.aspect, seen.level, seen.layers.start),
                |held, next| {
                    (held.image, held.aspect, held.level) == (next.image, next.aspect, next.level)
                        && next.layers.start <= held.layers.end
                },
                |held, next| held.layers.end = held.layers.end.max(next.layers.end),
            )?,
        })
    }

    fn meets(&self, touched: &Touched) -> bool {
        self.meets_bytes(touched) || self.meets_other_image(touched) || self.sees(touched)
    }

    /// Whether a buffer's bytes meet the attachment's image's placement.
    fn meets_bytes(&self, touched: &Touched) -> bool {
        let key = (touched.memory, touched.placement.start);
        let after = self
            .bytes
            .partition_point(|(memory, span)| (*memory, span.end) <= key);
        self.bytes
            .get(after)
            .is_some_and(|(memory, span)| touched.meets_bytes(*memory, span))
    }

    /// Whether an image other than the attachment's has a placement meeting
    /// its image's: a run meeting it of another image, or of more than one,
    /// which then holds one that meets it.
    fn meets_other_image(&self, touched: &Touched) -> bool {
        let key = (touched.memory, touched.placement.start);
        let after = self
            .runs
            .partition_point(|run| (run.memory, run.span.end) <= key);
        self.runs
            .get(after..)
            .unwrap_or(&[])
            .iter()
            .take_while(|run| {
                run.memory == touched.memory && run.span.start < touched.placement.end
            })
            .any(|run| run.lone != Some(touched.image))
    }

    /// Whether a view of the attachment's own image sees its plane, level
    /// and layer.
    fn sees(&self, touched: &Touched) -> bool {
        touched.aspects.planes().any(|plane| {
            let key = (touched.image, plane.bits(), touched.level);
            let after = self.seen.partition_point(|seen| {
                let held = (seen.image, seen.aspect, seen.level);
                held < key || (held == key && seen.layers.end <= touched.layer)
            });
            self.seen.get(after).is_some_and(|seen| {
                (seen.image, seen.aspect, seen.level) == key && seen.layers.start <= touched.layer
            })
        })
    }

    fn held_bytes(&self) -> u64 {
        fallible::held(&self.bytes)
            .saturating_add(fallible::held(&self.runs))
            .saturating_add(fallible::held(&self.seen))
    }
}

/// `items` sorted by `key`, each folded by `join` into the last kept where
/// `joins` says it meets it: two passes, so the list is held in exactly the
/// room it takes.
fn merged<T: Clone, K: Ord>(
    mut items: Vec<T>,
    key: impl Fn(&T) -> K,
    joins: impl Fn(&T, &T) -> bool,
    join: impl Fn(&mut T, &T),
) -> Result<Vec<T>, Error> {
    items.sort_unstable_by_key(|item| key(item));
    let mut count = 0usize;
    let mut run: Option<T> = None;
    for item in &items {
        match &mut run {
            Some(held) if joins(held, item) => join(held, item),
            _ => {
                count += 1;
                run = Some(item.clone());
            }
        }
    }
    let mut out: Vec<T> = Vec::new();
    out.try_reserve_exact(count).map_err(exhausted)?;
    for item in items {
        match out.last_mut() {
            Some(held) if joins(held, &item) => join(held, &item),
            _ => out.push(item),
        }
    }
    Ok(out)
}

/// The push constants a pipeline layout holds.
#[derive(Copy, Clone, Debug, Eq, PartialEq)]
pub struct PushConstants {
    /// How many bytes: whole words, at most the device's
    /// [`max_push_constant_bytes`](crate::Limits::max_push_constant_bytes).
    pub bytes: u32,
    /// The stages that see them: some where there are bytes, none where
    /// there are not.
    pub stages: Stages,
}

impl PushConstants {
    /// No push constants.
    pub const NONE: Self = Self {
        bytes: 0,
        stages: Stages::NONE,
    };
}

/// A pipeline layout: its bind set layouts, set by set, and its push
/// constants.
#[derive(Debug)]
pub(crate) struct PipelineLayoutState {
    pub(crate) sets: Vec<Arc<BindSetLayoutState>>,
    pub(crate) push_constants: PushConstants,
    /// Every set's bindings, by ascending set and binding.
    bindings: Vec<Binding>,
    _charge: Charge,
}

/// An admitted module, and the charge for the host memory it keeps.
pub(crate) struct ShaderModuleState {
    pub(crate) admitted: Arc<Admitted>,
    _charge: Charge,
}

impl PipelineLayoutState {
    /// The layout an entry point is agreed against.
    pub(crate) fn agreement(&self) -> Layout<'_> {
        Layout {
            bindings: &self.bindings,
            push_constants: self.push_constants.bytes,
            push_constant_stages: self.push_constants.stages,
        }
    }

    /// Refuse an entry point, agreed with the layout, that samples an image
    /// through a sampler in a way their types do not take.
    pub(crate) fn sampling(&self, entry: &EntryPoint) -> Result<(), Error> {
        for sampling in entry.samplings() {
            let refused = Error::Sampling(sampling.image, sampling.sampler);
            let sample = self
                .descriptor(sampling.image)
                .and_then(DescriptorType::sample)
                .ok_or(refused)?;
            let sampler = self
                .descriptor(sampling.sampler)
                .and_then(DescriptorType::sampler)
                .ok_or(refused)?;
            if !sampler.samples(sample) || !sampler.takes(sampling.forms) {
                return Err(refused);
            }
        }
        Ok(())
    }

    /// What the binding at `(set, binding)` holds.
    fn descriptor(&self, (set, binding): (u32, u32)) -> Option<DescriptorType> {
        let layout = self.sets.get(set as usize)?;
        let (at, _) = layout.find(binding)?;
        Some(layout.bindings.get(at)?.kind)
    }
}

/// What one descriptor of a bind set binds.
#[derive(Copy, Clone, Debug)]
pub enum Resource<'a> {
    /// A sampler, for a sampler binding.
    Sampler(&'a Sampler),
    /// A view, for a sampled or storage image binding.
    Image(&'a ImageView),
    /// A view and the sampler it is sampled with, for a combined binding.
    ImageSampler(&'a ImageView, &'a Sampler),
    /// A texel buffer, for a uniform or storage texel buffer binding.
    TexelBuffer(&'a BufferView),
    /// `size` bytes from `offset` into a buffer, for a uniform or storage
    /// buffer binding.
    Buffer {
        /// The buffer.
        buffer: &'a Buffer,
        /// Where the range starts in it.
        offset: u64,
        /// How many bytes it spans.
        size: u64,
    },
}

/// The resources a bind set binds at one binding of its layout: as many as
/// the binding's count.
#[derive(Copy, Clone, Debug)]
pub struct BindSetEntry<'a> {
    /// The binding.
    pub binding: u32,
    /// Its descriptors, in order.
    pub resources: &'a [Resource<'a>],
}

/// A resource as a request names it.
#[derive(Copy, Clone, Debug)]
pub(crate) enum Named {
    Sampler(SamplerKey),
    Image(ViewKey),
    ImageSampler(ViewKey, SamplerKey),
    TexelBuffer(BufferViewKey),
    Buffer {
        buffer: BufferKey,
        offset: u64,
        size: u64,
    },
}

impl Named {
    fn of(resource: &Resource<'_>) -> Self {
        match *resource {
            Resource::Sampler(sampler) => Self::Sampler(sampler.key),
            Resource::Image(view) => Self::Image(view.key),
            Resource::ImageSampler(view, sampler) => Self::ImageSampler(view.key, sampler.key),
            Resource::TexelBuffer(view) => Self::TexelBuffer(view.key),
            Resource::Buffer {
                buffer,
                offset,
                size,
            } => Self::Buffer {
                buffer: buffer.key,
                offset,
                size,
            },
        }
    }
}

/// A bind set as a request names it: its layout, and for each of the
/// layout's bindings in order its number and how many of `resources` it
/// takes.
#[derive(Copy, Clone, Debug)]
pub(crate) struct SetRequest<'a> {
    pub(crate) layout: BindSetLayoutKey,
    pub(crate) entries: &'a [(u32, usize)],
    pub(crate) resources: &'a [Named],
}

/// What a bind set is made of, looked up under the device's lock: the context
/// as the device holds it, the set's layout, and its resources.
type SetParts<B> = (
    Arc<<B as Backend>::Context>,
    Arc<BindSetLayoutState>,
    Vec<Bound<B>>,
);

/// A bind set layout's tables, checked against the device's description and
/// built before its lock is taken: what is left under it is the filing.
pub(crate) struct LayoutTables {
    bindings: Vec<LayoutBinding>,
    firsts: Vec<u64>,
    descriptors: u64,
}

impl LayoutTables {
    pub(crate) fn of(info: &DeviceInfo, bindings: &[LayoutBinding]) -> Result<Self, Error> {
        if bindings.windows(2).any(|w| w[0].binding >= w[1].binding) {
            return Err(Error::Bindings);
        }
        let mut descriptors = 0u64;
        for binding in bindings {
            binding.check(info)?;
            descriptors = descriptors.saturating_add(u64::from(binding.count));
        }
        if descriptors > u64::from(info.limits.max_bind_set_descriptors) {
            return Err(Error::TooMany);
        }
        let mut next = 0u64;
        let firsts = fallible::collected(
            bindings.len(),
            bindings.iter().map(|binding| {
                let first = next;
                next = next.saturating_add(u64::from(binding.count));
                first
            }),
        )
        .ok_or(Error::OutOfMemory)?;
        let bindings = fallible::copied(bindings).ok_or(Error::OutOfMemory)?;
        Ok(Self {
            bindings,
            firsts,
            descriptors,
        })
    }
}

impl<B: Backend> Model<B> {
    pub(crate) fn create_bind_set_layout(
        &mut self,
        key: ContextKey,
        tables: LayoutTables,
    ) -> Result<BindSetLayoutKey, Error> {
        let LayoutTables {
            bindings,
            firsts,
            descriptors,
        } = tables;
        let bytes = shared_bytes::<BindSetLayoutState>()
            .saturating_add(fallible::held(&bindings))
            .saturating_add(fallible::held(&firsts));
        self.file_charged(
            key,
            |context| &mut context.bind_set_layouts,
            bytes,
            |charge| {
                crate::shared(BindSetLayoutState {
                    bindings,
                    firsts,
                    descriptors,
                    _charge: charge,
                })
            },
        )
    }

    /// The layout and every resource of a bind set `request` names, each
    /// checked for the binding it fills: the first step of making one, taken
    /// under the device's lock, as each resource is looked up there.
    fn bind_set_resources(
        &self,
        key: ContextKey,
        request: &SetRequest<'_>,
    ) -> Result<SetParts<B>, Error> {
        let info = self.info();
        let context = self.live(key)?;
        let layout = Arc::clone(
            context
                .bind_set_layouts
                .get(request.layout)
                .ok_or(Error::Stale)?,
        );
        let filled =
            request.entries.len() == layout.bindings.len()
                && request.entries.iter().zip(&layout.bindings).all(
                    |(&(number, count), binding)| {
                        number == binding.binding
                            && usize::try_from(binding.count).is_ok_and(|n| n == count)
                    },
                )
                && usize::try_from(layout.descriptors).is_ok_and(|n| n == request.resources.len());
        if !filled {
            return Err(Error::Bindings);
        }
        // The set's descriptors alone are charged this much, so a set past the
        // limit is refused before any is looked up.
        let least = layout
            .descriptors
            .saturating_mul(size_of::<Bound<B>>() as u64)
            .saturating_add(shared_bytes::<BindSetState<B>>());
        context.budget.admit(least)?;
        let mut resources = Vec::new();
        resources
            .try_reserve_exact(request.resources.len())
            .map_err(exhausted)?;
        let mut named = request.resources.iter();
        for (binding, &(_, count)) in layout.bindings.iter().zip(request.entries) {
            for &resource in named.by_ref().take(count) {
                resources.push(bound(info, context, binding, resource)?);
            }
        }
        Ok((Arc::clone(&context.device), layout, resources))
    }

    pub(crate) fn create_pipeline_layout(
        &mut self,
        key: ContextKey,
        sets: &[BindSetLayoutKey],
        push_constants: PushConstants,
    ) -> Result<PipelineLayoutKey, Error> {
        let limits = &self.info().limits;
        if sets.len() > limits.max_bind_sets as usize {
            return Err(Error::TooMany);
        }
        if push_constants.bytes > limits.max_push_constant_bytes {
            return Err(Error::TooLarge);
        }
        if !push_constants.bytes.is_multiple_of(4) {
            return Err(Error::Misaligned);
        }
        if (push_constants.bytes == 0) != (push_constants.stages == Stages::NONE) {
            return Err(Error::Empty);
        }
        if !Stages::ALL.contains(push_constants.stages) {
            return Err(Error::Shape);
        }
        let context = self.live(key)?;
        let mut held = Vec::new();
        held.try_reserve_exact(sets.len()).map_err(exhausted)?;
        for &set in sets {
            held.push(Arc::clone(
                context.bind_set_layouts.get(set).ok_or(Error::Stale)?,
            ));
        }
        let mut bindings = Vec::new();
        let count = held.iter().map(|set| set.bindings.len()).sum();
        bindings.try_reserve_exact(count).map_err(exhausted)?;
        for (set, layout) in (0u32..).zip(&held) {
            bindings.extend(layout.bindings.iter().map(|b| Binding {
                set,
                binding: b.binding,
                kind: b.kind.kind(),
                count: b.count,
                access: b.access,
                stages: b.stages,
            }));
        }
        let bytes = shared_bytes::<PipelineLayoutState>()
            .saturating_add(fallible::held(&held))
            .saturating_add(fallible::held(&bindings));
        self.file_charged(
            key,
            |context| &mut context.pipeline_layouts,
            bytes,
            |charge| {
                crate::shared(PipelineLayoutState {
                    sets: held,
                    push_constants,
                    bindings,
                    _charge: charge,
                })
            },
        )
    }

    pub(crate) fn create_shader_module(
        &mut self,
        key: ContextKey,
        module: &Arc<Admitted>,
    ) -> Result<ShaderModuleKey, Error> {
        if !self.info().runs_module(module) {
            return Err(Error::Unsupported);
        }
        let bytes = shared_bytes::<ShaderModuleState>()
            .saturating_add(shared_bytes::<Admitted>())
            .saturating_add(module.held_bytes());
        self.file_charged(
            key,
            |context| &mut context.shader_modules,
            bytes,
            |charge| {
                crate::shared(ShaderModuleState {
                    admitted: Arc::clone(module),
                    _charge: charge,
                })
            },
        )
    }
}

/// `resource` held for `binding`, or why it cannot be.
fn bound<B: Backend>(
    info: &DeviceInfo,
    context: &ContextState<B>,
    binding: &LayoutBinding,
    resource: Named,
) -> Result<Bound<B>, Error> {
    let limits = &info.limits;
    let kind = binding.kind;
    match (kind, resource) {
        (DescriptorType::Sampler(sampler), Named::Sampler(key)) => {
            let state = sampler_state(context, key)?;
            admitted(sampler, &state)?;
            Ok(Bound::Sampler(state))
        }
        (DescriptorType::SampledImage(shape, sample), Named::Image(view)) => {
            let view = view_state(context, view)?;
            sampled(info, &view, shape, sample)?;
            Ok(Bound::Image(view))
        }
        (DescriptorType::StorageImage(shape), Named::Image(view)) => {
            let view = view_state(context, view)?;
            storage(info, &view, shape, binding.access)?;
            Ok(Bound::Image(view))
        }
        (
            DescriptorType::CombinedImageSampler(shape, sample, sampler),
            Named::ImageSampler(view, key),
        ) => {
            let view = view_state(context, view)?;
            let state = sampler_state(context, key)?;
            sampled(info, &view, shape, sample)?;
            admitted(sampler, &state)?;
            unnormalized(&view, sample, &state)?;
            Ok(Bound::ImageSampler(view, state))
        }
        (DescriptorType::UniformTexelBuffer(shape), Named::TexelBuffer(view)) => {
            let view = texel_state(context, view)?;
            texel(info, &view, shape, None)?;
            Ok(Bound::TexelBuffer(view))
        }
        (DescriptorType::StorageTexelBuffer(shape), Named::TexelBuffer(view)) => {
            let view = texel_state(context, view)?;
            texel(info, &view, shape, Some(binding.access))?;
            Ok(Bound::TexelBuffer(view))
        }
        (
            DescriptorType::UniformBuffer | DescriptorType::StorageBuffer,
            Named::Buffer {
                buffer,
                offset,
                size,
            },
        ) => {
            let state = context.buffers.get(buffer).ok_or(Error::Stale)?;
            let need = if kind == DescriptorType::UniformBuffer {
                Span {
                    usage: BufferUsage::UNIFORM,
                    alignment: limits.uniform_buffer_alignment,
                    size_alignment: limits.uniform_buffer_size_alignment,
                    longest: limits.max_uniform_buffer_range,
                }
            } else {
                Span {
                    usage: BufferUsage::STORAGE,
                    alignment: limits.storage_buffer_alignment,
                    size_alignment: limits.storage_buffer_size_alignment,
                    longest: limits.max_storage_buffer_range,
                }
            };
            need.check(state, offset, size)?;
            Ok(Bound::Buffer {
                buffer: Arc::clone(state),
                offset,
                size,
            })
        }
        _ => Err(Error::Shape),
    }
}

fn sampler_state<B: Backend>(
    context: &ContextState<B>,
    sampler: SamplerKey,
) -> Result<Arc<SamplerState<B>>, Error> {
    context.samplers.get(sampler).cloned().ok_or(Error::Stale)
}

fn view_state<B: Backend>(
    context: &ContextState<B>,
    view: ViewKey,
) -> Result<Arc<ViewState<B>>, Error> {
    context.views.get(view).cloned().ok_or(Error::Stale)
}

fn texel_state<B: Backend>(
    context: &ContextState<B>,
    view: BufferViewKey,
) -> Result<Arc<BufferViewState<B>>, Error> {
    context.buffer_views.get(view).cloned().ok_or(Error::Stale)
}

/// Refuse a view a sampled binding of `shape` and `sample` cannot read: of an
/// image not made for sampling, of a plane other than its texels', of a
/// format the device does not sample or, where its samplers may filter, does
/// not filter, or of another shape.
fn sampled<B: Backend>(
    info: &DeviceInfo,
    view: &ViewState<B>,
    shape: ImageShape,
    sample: SampleType,
) -> Result<(), Error> {
    if !view.image.desc.usage.contains(ImageUsage::SAMPLED) {
        return Err(Error::Usage);
    }
    let aspects = view.desc.range.aspects;
    let plane = if sample.depth() {
        aspects == Aspects::DEPTH
    } else {
        aspects == Aspects::COLOR || (sample == SampleType::Uint && aspects == Aspects::STENCIL)
    };
    if !plane {
        return Err(Error::Aspect);
    }
    let features = info.formats.get(view.desc.format);
    if !features.contains(FormatFeatures::SAMPLED)
        || (sample.filtered() && !features.contains(FormatFeatures::FILTERED))
    {
        return Err(Error::Unsupported);
    }
    if serves(view.desc.shape(&view.image.desc), shape, false) {
        Ok(())
    } else {
        Err(Error::Shape)
    }
}

/// Refuse a sampler a binding of `sampler` does not take.
fn admitted<B: Backend>(sampler: SamplerType, state: &SamplerState<B>) -> Result<(), Error> {
    if sampler.admits(&state.desc) {
        Ok(())
    } else {
        Err(Error::Shape)
    }
}

/// Refuse a view a storage binding of `shape` cannot access as `access`: of
/// an image not made for storage, of more than one level, of a format the
/// device does not store or, for a binding naming no format, does not read or
/// write so, or of another shape or format.
fn storage<B: Backend>(
    info: &DeviceInfo,
    view: &ViewState<B>,
    shape: ImageShape,
    access: Access,
) -> Result<(), Error> {
    if !view.image.desc.usage.contains(ImageUsage::STORAGE) {
        return Err(Error::Usage);
    }
    if view.desc.range.levels != 1 {
        return Err(Error::Shape);
    }
    let unformatted = [
        FormatFeatures::STORAGE_READ_WITHOUT_FORMAT,
        FormatFeatures::STORAGE_WRITE_WITHOUT_FORMAT,
    ];
    let features = info.formats.get(view.desc.format);
    if !features.contains(FormatFeatures::STORAGE)
        || !accessed(features, shape, access, unformatted)
    {
        return Err(Error::Unsupported);
    }
    if serves(view.desc.shape(&view.image.desc), shape, true) {
        Ok(())
    } else {
        Err(Error::Shape)
    }
}

/// Refuse a view an unnormalised sampler cannot sample: anything but one
/// level and layer of a row or rectangle, and texels read as `sample` that
/// are not filtered where the sampler filters.
fn unnormalized<B: Backend>(
    view: &ViewState<B>,
    sample: SampleType,
    sampler: &SamplerState<B>,
) -> Result<(), Error> {
    let desc = &sampler.desc;
    if !desc.unnormalized {
        return Ok(());
    }
    let plain = matches!(view.desc.dimension, ViewDimension::D1 | ViewDimension::D2)
        && view.desc.range.levels == 1
        && view.desc.range.layers == 1;
    if !plain {
        return Err(Error::Shape);
    }
    // Unnormalised coordinates take one filter for both and no mip filter. A
    // filtered sample type is of a format the device filters, as `sampled`
    // holds; a plane's format alone says nothing of the other plane.
    if desc.mag_filter == Filter::Linear && !sample.filtered() {
        return Err(Error::Shape);
    }
    Ok(())
}

/// Refuse a texel buffer a binding of `shape` cannot read, or as a storage
/// binding accessed as `storage` cannot access so: over a buffer not made for
/// that use, of a format the device does not offer for it or, for a storage
/// binding naming no format, does not read or write so, or of another shape
/// or format.
fn texel<B: Backend>(
    info: &DeviceInfo,
    view: &BufferViewState<B>,
    shape: ImageShape,
    storage: Option<Access>,
) -> Result<(), Error> {
    let features = info.formats.get(view.desc.format);
    let (usage, offered) = match storage {
        Some(access) => (
            BufferUsage::STORAGE_TEXEL,
            features.contains(FormatFeatures::STORAGE_TEXEL_BUFFER)
                && accessed(
                    features,
                    shape,
                    access,
                    [
                        FormatFeatures::TEXEL_BUFFER_READ_WITHOUT_FORMAT,
                        FormatFeatures::TEXEL_BUFFER_WRITE_WITHOUT_FORMAT,
                    ],
                ),
        ),
        None => (
            BufferUsage::UNIFORM_TEXEL,
            features.contains(FormatFeatures::UNIFORM_TEXEL_BUFFER),
        ),
    };
    if !view.buffer.usage.contains(usage) {
        return Err(Error::Usage);
    }
    if !offered {
        return Err(Error::Unsupported);
    }
    if serves(view.desc.shape(), shape, storage.is_some()) {
        Ok(())
    } else {
        Err(Error::Shape)
    }
}

/// What a buffer binding's range needs: a buffer made for `usage`, an
/// offset a multiple of `alignment`, a size a multiple of `size_alignment`,
/// and a span of at most `longest`.
struct Span {
    usage: BufferUsage,
    alignment: u64,
    size_alignment: u64,
    longest: u64,
}

impl Span {
    fn check<B: Backend>(
        &self,
        buffer: &BufferState<B>,
        offset: u64,
        size: u64,
    ) -> Result<(), Error> {
        if !buffer.usage.contains(self.usage) {
            return Err(Error::Usage);
        }
        if size == 0 {
            return Err(Error::Empty);
        }
        if !offset.is_multiple_of(self.alignment) || !size.is_multiple_of(self.size_alignment) {
            return Err(Error::Misaligned);
        }
        if offset.checked_add(size).is_none_or(|end| end > buffer.size) {
            return Err(Error::OutOfRange);
        }
        if size > self.longest {
            return Err(Error::TooLarge);
        }
        Ok(())
    }
}

impl Context {
    /// A bind set layout of `bindings`, by ascending binding number.
    ///
    /// # Errors
    ///
    /// [`Error::Bindings`] for bindings out of order or repeated,
    /// [`Error::Empty`] for one of no descriptors or stages,
    /// [`Error::OutOfRange`] for one past the device's binding numbers,
    /// [`Error::Unsupported`] for one of more than one descriptor on a device
    /// not stating [`Features::BINDING_ARRAYS`], [`Error::Shape`] for an
    /// access or shape its kind does not take, and
    /// [`Error::TooMany`] for more descriptors than the device's
    /// [`max_bind_set_descriptors`](crate::Limits::max_bind_set_descriptors).
    ///
    /// And those of every request that makes memory ([`Context#memory`]).
    pub fn create_bind_set_layout(
        &self,
        bindings: &[LayoutBinding],
    ) -> Result<BindSetLayout, Error> {
        let key = self.host.create_bind_set_layout(self.key, bindings)?;
        Ok(BindSetLayout {
            host: Arc::clone(&self.host),
            context: self.key,
            key,
        })
    }

    /// A bind set of `layout`, made whole: an entry for each of its bindings
    /// in order, each with as many resources as the binding's count. It holds
    /// what it binds, and through it its memory, for as long as it lives.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for an object of another context,
    /// [`Error::Bindings`] for entries that do not fill the layout so, and for
    /// a resource the binding cannot take: [`Error::Shape`] for one of another
    /// kind or shape, [`Error::Usage`] for one not made for the use,
    /// [`Error::Aspect`] for a view of more than one plane or a comparison of
    /// what is not depth, [`Error::Unsupported`] for a format the device does
    /// not offer for it, and [`Error::Empty`], [`Error::Misaligned`] — an
    /// offset or a size off the device's alignments — [`Error::OutOfRange`]
    /// or [`Error::TooLarge`] for a buffer range the binding cannot hold.
    ///
    /// And those of every request that makes memory ([`Context#memory`]).
    pub fn create_bind_set(
        &self,
        layout: &BindSetLayout,
        entries: &[BindSetEntry<'_>],
    ) -> Result<BindSet, Error> {
        owned(&layout.host, layout.context, &self.host, self.key)?;
        let count = entries.iter().map(|entry| entry.resources.len()).sum();
        let mut resources = Vec::new();
        resources.try_reserve_exact(count).map_err(exhausted)?;
        for resource in entries.iter().flat_map(|entry| entry.resources) {
            self.owns(resource)?;
            resources.push(Named::of(resource));
        }
        let shape = fallible::collected(
            entries.len(),
            entries
                .iter()
                .map(|entry| (entry.binding, entry.resources.len())),
        )
        .ok_or(Error::OutOfMemory)?;
        let key = self.host.create_bind_set(
            self.key,
            &SetRequest {
                layout: layout.key,
                entries: &shape,
                resources: &resources,
            },
        )?;
        Ok(BindSet {
            host: Arc::clone(&self.host),
            context: self.key,
            key,
        })
    }

    fn owns(&self, resource: &Resource<'_>) -> Result<(), Error> {
        let mine =
            |host: &Arc<dyn Host>, context: ContextKey| owned(host, context, &self.host, self.key);
        match *resource {
            Resource::Sampler(sampler) => mine(&sampler.host, sampler.context),
            Resource::Image(view) => mine(&view.host, view.context),
            Resource::ImageSampler(view, sampler) => {
                mine(&view.host, view.context)?;
                mine(&sampler.host, sampler.context)
            }
            Resource::TexelBuffer(view) => mine(&view.host, view.context),
            Resource::Buffer { buffer, .. } => mine(&buffer.host, buffer.context),
        }
    }

    /// A pipeline layout of `sets`, set `i` laid out by `sets[i]`, and
    /// `push_constants`.
    ///
    /// # Errors
    ///
    /// [`Error::Foreign`] for a bind set layout of another context,
    /// [`Error::TooMany`] for more sets than the device's
    /// [`max_bind_sets`](crate::Limits::max_bind_sets), and for push constants
    /// past the device's [`Error::TooLarge`], of part of a word
    /// [`Error::Misaligned`], of bytes but no stages or stages but no bytes
    /// [`Error::Empty`], and of a stage there is not [`Error::Shape`].
    ///
    /// And those of every request that makes memory ([`Context#memory`]).
    pub fn create_pipeline_layout(
        &self,
        sets: &[&BindSetLayout],
        push_constants: PushConstants,
    ) -> Result<PipelineLayout, Error> {
        let mut keys = Vec::new();
        keys.try_reserve_exact(sets.len()).map_err(exhausted)?;
        for set in sets {
            owned(&set.host, set.context, &self.host, self.key)?;
            keys.push(set.key);
        }
        let key = self
            .host
            .create_pipeline_layout(self.key, &keys, push_constants)?;
        Ok(PipelineLayout {
            host: Arc::clone(&self.host),
            context: self.key,
            key,
        })
    }

    /// A shader module of an admitted module.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`] for a module needing a capability the device
    /// does not run, or of a newer SPIR-V version than it takes.
    ///
    /// And those of every request that makes memory ([`Context#memory`]).
    pub fn create_shader_module(&self, module: Admitted) -> Result<ShaderModule, Error> {
        let module = crate::shared(module)?;
        let key = self.host.create_shader_module(self.key, &module)?;
        Ok(ShaderModule {
            host: Arc::clone(&self.host),
            context: self.key,
            key,
            module,
        })
    }
}

impl<B: Backend> Shared<B> {
    /// A bind set of the context as `request` names it: its resources looked
    /// up under the device's lock, what they reach and their descriptors as
    /// the backend reads them made with it let go, and the set filed under
    /// it.
    pub(crate) fn create_bind_set(
        &self,
        context: ContextKey,
        request: &SetRequest<'_>,
    ) -> Result<BindSetKey, Error> {
        let (device, layout, resources) = self.model.lock().bind_set_resources(context, request)?;
        #[cfg(test)]
        crate::fair::held::note_making();
        let reach = Reach::of(&layout, &resources)?;
        let (form, form_bytes) = self.backend.set(&device, &layout, &resources)?;
        let bytes = [
            shared_bytes::<BindSetState<B>>(),
            fallible::held(&resources),
            form_bytes,
            reach.held_bytes(),
        ]
        .into_iter()
        .fold(0, u64::saturating_add);
        let mut parts = Some((layout, resources, form, reach));
        let filed = self.model.lock().file_charged(
            context,
            |context| &mut context.bind_sets,
            bytes,
            |charge| {
                let (layout, resources, form, reach) = parts.take().ok_or(Error::Malformed)?;
                crate::shared(BindSetState {
                    layout,
                    resources,
                    form,
                    reach,
                    _charge: charge,
                })
            },
        );
        // A refusal before the set was made leaves its parts here, to go with
        // the lock let go: they may hold the last of a memory.
        drop(parts);
        filed
    }
}

handle!(
    /// The bindings a bind set holds.
    BindSetLayout,
    BindSetLayoutKey,
    BindSetLayout
);

handle!(
    /// The resources one set of a pipeline's bindings reaches, made whole and
    /// held for as long as the set lives.
    BindSet,
    BindSetKey,
    BindSet
);

handle!(
    /// The bind set layouts and push constants a pipeline's shaders reach.
    PipelineLayout,
    PipelineLayoutKey,
    PipelineLayout
);

/// An admitted shader module, made a device's.
pub struct ShaderModule {
    pub(crate) host: Arc<dyn Host>,
    pub(crate) context: ContextKey,
    pub(crate) key: ShaderModuleKey,
    module: Arc<Admitted>,
}

impl ShaderModule {
    /// The module.
    #[must_use]
    pub fn module(&self) -> &Admitted {
        &self.module
    }
}

impl Drop for ShaderModule {
    fn drop(&mut self) {
        self.host
            .release(self.context, Object::ShaderModule(self.key));
    }
}

impl fmt::Debug for ShaderModule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ShaderModule").finish_non_exhaustive()
    }
}

#[cfg(test)]
#[path = "binding_tests.rs"]
pub(crate) mod tests;

#[cfg(test)]
#[path = "sampling_tests.rs"]
mod sampling_tests;
